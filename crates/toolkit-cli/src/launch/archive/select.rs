//! The upload source walk (R-AR-SEL, R-AR-OPEN; Python `zip_local_repo`).
//!
//! The caller's path is opened once ([`fsops::open_root`]); every listing,
//! classification and open below it is relative to held descriptors. The
//! walk never follows a symlink below the root, skips every non-regular file
//! (a FIFO is never opened), applies the Python skip tables exactly, visits
//! names in byte order per directory, and reads each selected file through a
//! handle whose type and identity were checked first, at most its walked
//! size plus one byte (capped at the per-entry limit). Any change seen
//! during the walk fails the build before an archive exists, so no upload
//! request can follow.
//!
//! [`WalkHook`] is the race seam of the tests: it is called between the
//! classification and the open of every file and between the listing and
//! the entering of every directory. Production uses [`NoHook`].

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use super::fsops::{self, DirHandle, EntryType, OpenError};
use super::git::{git_included_files, GitRunner};
use super::zip_read::{read_archive, Profile, ReadStats};
use super::zip_write::{sanitize_root_name, ZipWriter};
use super::ArchiveLimits;
use crate::launch::clock::Random;
use crate::launch::config::expand_user;
use crate::launch::text::shown;
use crate::launch::LaunchError;

/// Directory names never entered (`_SKIP_DIRS`, exact match).
pub const SKIP_DIRS: [&str; 28] = [
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    ".next",
    ".nuxt",
    ".turbo",
    ".cache",
    ".parcel-cache",
    "dist",
    "build",
    "out",
    "coverage",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".idea",
    ".vscode",
    ".aws",
    ".ssh",
    ".gnupg",
    ".gcp",
    ".azure",
    ".docker",
    ".kube",
];

/// File names skipped as secrets (`_SKIP_FILES`, case-insensitive).
pub const SKIP_FILES: [&str; 13] = [
    ".env",
    ".env.local",
    ".env.production",
    ".env.development",
    ".env.test",
    ".env.staging",
    "credentials",
    "credentials.json",
    ".git-credentials",
    ".netrc",
    ".pypirc",
    ".npmrc",
    ".htpasswd",
];

/// Secret suffixes (`_SKIP_SUFFIXES`, case-insensitive).
pub const SKIP_SUFFIXES: [&str; 6] = [".pem", ".key", ".p12", ".pfx", ".jks", ".keystore"];

/// Secret name prefixes (`_SKIP_NAME_PREFIXES`, case-insensitive).
pub const SKIP_NAME_PREFIXES: [&str; 4] = ["id_rsa", "id_ed25519", "id_ecdsa", "id_dsa"];

/// Deepest directory nesting walked (R-AR-OPEN: 4096 / 2).
pub const MAX_DEPTH: usize = 2048;

/// Python `_is_secret_file`.
#[must_use]
pub fn is_secret_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    SKIP_FILES.contains(&lower.as_str())
        || lower.starts_with(".env.")
        || SKIP_SUFFIXES.iter().any(|suffix| lower.ends_with(suffix))
        || SKIP_NAME_PREFIXES
            .iter()
            .any(|prefix| lower.starts_with(prefix))
}

/// Whether a directory name is in [`SKIP_DIRS`].
#[must_use]
pub fn is_skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

/// The race seam.
pub trait WalkHook {
    /// Between classifying the regular file `rel` and opening it.
    fn before_open(&self, rel: &str) {
        let _ = rel;
    }
    /// Between listing the directory `rel` in its parent and entering it.
    fn before_enter(&self, rel: &str) {
        let _ = rel;
    }
}

/// The production hook: does nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHook;

impl WalkHook for NoHook {}

/// Everything [`build_archive`] needs besides the caller's arguments.
pub struct BuildOptions<'a> {
    /// How Git runs.
    pub git: &'a GitRunner,
    /// The race seam.
    pub hook: &'a dyn WalkHook,
    /// Archive limits.
    pub limits: ArchiveLimits,
    /// `HOME`, for `~` expansion.
    pub home: Option<&'a Path>,
    /// The current directory, for a relative path.
    pub cwd: &'a Path,
    /// Parent of the private temporary Git metadata.
    pub temp_parent: &'a Path,
    /// Randomness for temporary names.
    pub random: &'a dyn Random,
}

/// A finished upload archive.
#[derive(Debug)]
pub struct BuiltArchive {
    /// The archive bytes (at most the archive limit).
    pub bytes: Vec<u8>,
    /// Files archived.
    pub files: usize,
    /// Relative paths skipped as secrets (Python logged them at INFO).
    pub skipped_secrets: Vec<String>,
}

/// The absolute, lexically normalised form of `path` (Python
/// `os.path.abspath(os.path.expanduser(path))`).
#[must_use]
pub fn absolute_root(path: &str, home: Option<&Path>, cwd: &Path) -> PathBuf {
    let expanded = expand_user(path, home);
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };
    let mut out = PathBuf::from("/");
    for component in joined.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// Build the upload archive of the directory `path` under the top-level
/// name `sanitize_root_name(root_name)`.
///
/// # Errors
/// The Python `ArchiveError` messages ("Not a directory", "No files found",
/// the limits, Git), the changed-file refusal and the non-UTF-8 refusal.
pub fn build_archive(
    path: &str,
    root_name: &str,
    options: &BuildOptions<'_>,
) -> Result<BuiltArchive, LaunchError> {
    let root_path = absolute_root(path, options.home, options.cwd);
    let root = fsops::open_root(&root_path)
        .map_err(|_| LaunchError::Archive(format!("Not a directory: {}", shown(path))))?;
    let prefix = sanitize_root_name(root_name);
    let selection = git_included_files(
        options.git,
        &root_path,
        &root,
        options.temp_parent,
        options.random,
    )?;
    let mut walk = Walk {
        writer: ZipWriter::new(options.limits),
        selection,
        hook: options.hook,
        prefix,
        limits: options.limits,
        files: 0,
        skipped_secrets: Vec::new(),
    };
    walk.directory(&root, "", 0)?;
    if walk.files == 0 {
        return Err(LaunchError::Archive(format!(
            "No files found under {}",
            shown(path)
        )));
    }
    let Walk {
        writer,
        files,
        skipped_secrets,
        ..
    } = walk;
    let bytes = writer.finish()?.into_vec();
    read_archive(
        &bytes,
        Profile::Upload,
        &options.limits,
        &mut ReadStats::default(),
    )?;
    Ok(BuiltArchive {
        bytes,
        files,
        skipped_secrets,
    })
}

/// The walk state.
struct Walk<'a> {
    writer: ZipWriter,
    selection: Option<HashSet<String>>,
    hook: &'a dyn WalkHook,
    prefix: String,
    limits: ArchiveLimits,
    files: usize,
    skipped_secrets: Vec<String>,
}

fn changed(rel: &str) -> LaunchError {
    LaunchError::Archive(format!(
        "A file changed while the archive was being built: {}. Re-run create-launch.",
        shown(rel)
    ))
}

fn read_error(rel: &str, error: &io::Error) -> LaunchError {
    if error.kind() == io::ErrorKind::NotFound {
        return changed(rel);
    }
    LaunchError::Archive(format!("Could not read {}: {}.", shown(rel), error.kind()))
}

fn open_error(rel: &str, error: &OpenError) -> LaunchError {
    match error {
        OpenError::Changed => changed(rel),
        OpenError::Io(error) => read_error(rel, error),
    }
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

impl Walk<'_> {
    fn directory(&mut self, dir: &DirHandle, rel: &str, depth: usize) -> Result<(), LaunchError> {
        if depth > MAX_DEPTH {
            return Err(LaunchError::Archive(format!(
                "The source directory is nested more than {MAX_DEPTH} levels deep."
            )));
        }
        let names = fsops::list_names(dir).map_err(|error| read_error(rel, &error))?;
        for name in names {
            let stat = match fsops::stat_at(dir, &name) {
                Ok(stat) => stat,
                Err(error) => {
                    let shown_rel = join(rel, &name.to_string_lossy());
                    return Err(read_error(&shown_rel, &error));
                }
            };
            if matches!(stat.kind, EntryType::Symlink | EntryType::Special) {
                continue;
            }
            let Ok(text) = name.to_str() else {
                return Err(LaunchError::Archive(format!(
                    "Archive cannot include a file whose name is not valid UTF-8: {}",
                    shown(&join(rel, &name.to_string_lossy()))
                )));
            };
            let child = join(rel, text);
            if stat.kind == EntryType::Directory {
                if is_skip_dir(text) {
                    continue;
                }
                self.hook.before_enter(&child);
                let sub = fsops::open_dir_at(dir, &name, &stat)
                    .map_err(|error| open_error(&child, &error))?;
                self.directory(&sub, &child, depth + 1)?;
                continue;
            }
            if self
                .selection
                .as_ref()
                .is_some_and(|selection| !selection.contains(&child))
            {
                continue;
            }
            if is_secret_file(text) {
                self.skipped_secrets.push(child);
                continue;
            }
            let entry_name = format!("{}/{child}", self.prefix);
            self.writer.check_next(&entry_name, stat.size)?;
            self.hook.before_open(&child);
            let file = fsops::open_file_at(dir, &name, &stat)
                .map_err(|error| open_error(&child, &error))?;
            let data = read_walked(file, stat.size, self.limits.max_entry_uncompressed)
                .map_err(|error| read_error(&child, &error))?
                .ok_or_else(|| changed(&child))?;
            self.writer
                .add_file(&entry_name, &data, stat.mode, stat.mtime)?;
            self.files += 1;
        }
        Ok(())
    }
}

/// Read at most `min(size, cap) + 1` bytes; `None` when the file no longer
/// has exactly `size` bytes.
fn read_walked(mut file: File, size: u64, cap: u64) -> io::Result<Option<Vec<u8>>> {
    let limit = usize::try_from(size.min(cap) + 1)
        .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
    let mut buf = vec![0_u8; limit];
    let mut filled = 0;
    while filled < limit {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    if filled as u64 != size {
        return Ok(None);
    }
    buf.truncate(filled);
    Ok(Some(buf))
}

#[cfg(test)]
mod tests_select;
