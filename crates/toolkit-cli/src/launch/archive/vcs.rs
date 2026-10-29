//! Version-control administrative names in snapshots (R-AR-VCS, #5040).
//!
//! A snapshot containing any component that, after normalisation, names a
//! version-control administrative directory or file is refused as a whole
//! before anything is created. Normalisation covers the aliases Git itself
//! guards against (`is_hfs_dotgit`, `is_ntfs_dotgit`): NFC, simple case
//! folding, the HFS+ ignorable code points, NTFS stream syntax (`:` and what
//! follows), trailing dots and spaces, and 8.3 short names `git~N`, `hg~N`,
//! `svn~N`, `bzr~N`. The denylist is necessary, not sufficient: the
//! repository's actual Git directory is protected by identity at apply time
//! (R-AR-META, L6).

use unicode_normalization::UnicodeNormalization;

use crate::launch::text::shown;
use crate::launch::LaunchError;

/// The denylisted component names (compared after [`normalise_component`]).
pub const VCS_NAMES: [&str; 11] = [
    ".git",
    ".hg",
    ".svn",
    ".bzr",
    "_darcs",
    "CVS",
    ".jj",
    ".pijul",
    ".sl",
    ".fslckout",
    "_FOSSIL_",
];

/// The 8.3 short-name stems (`<stem>~<digits>`).
pub const SHORT_NAME_STEMS: [&str; 4] = ["git", "hg", "svn", "bzr"];

/// A component normalised for comparison with [`VCS_NAMES`].
#[must_use]
pub fn normalise_component(component: &str) -> String {
    let visible: String = component
        .chars()
        .filter(|c| !is_hfs_ignorable(*c))
        .collect();
    let nfc: String = visible.nfc().collect();
    let folded: String = nfc.to_uppercase().to_lowercase().nfc().collect();
    let stream_free = folded.split(':').next().unwrap_or("");
    stream_free.trim_end_matches(['.', ' ']).to_string()
}

/// The code points HFS+ ignores in names (Git `is_hfs_dotgit`).
fn is_hfs_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}'
    )
}

/// Whether `component` names version-control metadata.
#[must_use]
pub fn is_vcs_component(component: &str) -> bool {
    let normalised = normalise_component(component);
    if VCS_NAMES
        .iter()
        .any(|name| normalised == name.to_lowercase())
    {
        return true;
    }
    normalised.split_once('~').is_some_and(|(stem, digits)| {
        SHORT_NAME_STEMS.contains(&stem)
            && !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

/// Refuse a snapshot entry name with a version-control component.
///
/// # Errors
/// [`LaunchError::Archive`]: `The snapshot contains version-control metadata
/// and was not extracted: <shown name>.`
pub fn check_snapshot_name(name: &str) -> Result<(), LaunchError> {
    let body = name.strip_suffix('/').unwrap_or(name);
    if body.split('/').any(is_vcs_component) {
        return Err(LaunchError::Archive(format!(
            "The snapshot contains version-control metadata and was not extracted: {}.",
            shown(name)
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests_vcs;
