//! The artifacts reader's one path policy (#4584, #4596).
//!
//! Every file `startup artifacts` opens is either named by the caller or
//! discovered by the validator:
//!
//! - [`Origin::Explicit`]: `--manifest` and `--baseline`. The caller chose the
//!   path, so linked parent directories are allowed, as in Python; only the
//!   final component must not be a link.
//! - [`Origin::Discovered`]: the default manifest and baseline, the
//!   `fixtures` directory and every contract document or schema the manifest
//!   declares. No component may be a link. A relative declaration is walked
//!   from the selected root; one that is not spelled beneath the root (an
//!   absolute declaration, which Python's `root / path` keeps as is) is walked
//!   component by component from its own start. The decision rests on what
//!   `lstat` reports for each component as it is reached, never on comparing
//!   the spelling with the root, so `/a/other/../plugin/linked/x` cannot slip
//!   past the check by not starting with the root's text.
//!
//! The run directory and `--plugin-root` are explicit roots and are not
//! examined; the run-directory walk itself refuses every link it meets.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::startup::{is_regular_file, read_text, refuse_links_below};

/// How the reader came by a path.
#[derive(Clone, Copy)]
pub(crate) enum Origin<'a> {
    /// Named on the command line: only the final component is checked.
    Explicit,
    /// Chosen by the validator beneath this root: every component is checked.
    Discovered(&'a Path),
}

impl Origin<'_> {
    /// Refuses a symbolic link this origin does not allow.
    pub(crate) fn refuse_links(self, path: &Path) -> Result<(), String> {
        match self {
            Self::Explicit => Ok(()),
            Self::Discovered(root) if path.starts_with(root) => refuse_links_below(root, path),
            Self::Discovered(_) => refuse_links_along(path),
        }
    }

    /// `Path.is_file()` under this origin's link policy.
    pub(crate) fn is_file(self, path: &Path) -> Result<bool, String> {
        self.refuse_links(path)?;
        is_regular_file(path)
    }

    /// A bounded text read under this origin's link policy.
    pub(crate) fn read(self, path: &Path, shown: &str) -> Result<String, String> {
        self.refuse_links(path)?;
        read_text(path, shown)
    }
}

/// Refuses a symbolic link at any component of `path`, walking from its first
/// component. A component that does not exist, or lies below a regular file,
/// ends the walk; the caller's presence check then reports the path absent.
fn refuse_links_along(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "cannot read {}: symbolic links are not followed",
                    current.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => return Ok(()),
            Ok(_) => {}
            Err(error)
                if error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(20) =>
            {
                return Ok(());
            }
            Err(error) => return Err(format!("cannot read {}: {error}", current.display())),
        }
    }
    Ok(())
}
