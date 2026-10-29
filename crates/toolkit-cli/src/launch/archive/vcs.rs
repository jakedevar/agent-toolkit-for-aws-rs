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
    let _ = component;
    todo!("L2: vcs normalise")
}

/// Whether `component` names version-control metadata.
#[must_use]
pub fn is_vcs_component(component: &str) -> bool {
    let _ = component;
    todo!("L2: vcs match")
}

/// Refuse a snapshot entry name with a version-control component.
///
/// # Errors
/// [`LaunchError::Archive`]: `The snapshot contains version-control metadata
/// and was not extracted: <shown name>.`
pub fn check_snapshot_name(name: &str) -> Result<(), LaunchError> {
    let _ = name;
    todo!("L2: vcs check")
}

#[cfg(test)]
mod tests_vcs;
