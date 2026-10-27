//! Path and filesystem contracts shared by every command.
//!
//! - [`normalize_relative`] turns an untrusted relative path into a canonical
//!   `/`-joined form and rejects anything that could escape its root.
//! - [`resolve_within`] joins a normalized path under a root and refuses to
//!   pass through a symbolic link at any component (the root itself may be a
//!   link: it is the caller's explicit choice).
//! - [`read_regular_file`] reads a bounded UTF-8 text file only when the path
//!   is a regular file and not a symbolic link. On Unix the opened handle's
//!   device and inode must match the pre-open `lstat`, which closes the
//!   check-then-open window **for the final path component**: a file swapped
//!   for a link in between is refused with [`PathError::Changed`] instead of
//!   being followed. Other platforms keep the `lstat` check only.
//!
//! # Threat model
//!
//! Static symbolic links are always refused, at any component below the
//! root. These functions do **not** defend against concurrent mutation of
//! the tree during a run. In particular the identity check covers only the
//! final component: if an *ancestor* directory is renamed away and replaced
//! with a symlink to an outside directory after a walk has listed it but
//! before it is opened, the pathname-based `lstat`, open and `read_dir` all
//! resolve through the swapped ancestor, the opened handle matches the
//! `lstat` taken through the same swapped path, and outside content can be
//! read. Defending against that requires a traversal anchored to verified
//! directory handles (`openat`-style, no-follow per component), which needs a
//! non-`std` dependency or `unsafe`; the toolkit does not take that on
//! because renaming directories inside the user's own checkout while the tool
//! runs is outside its model. Callers use only the public signatures here
//! (`resolve_within`, `read_regular_file`) and the discovery entry point, so a
//! later slice that needs adversarial containment can replace the
//! implementation with a handle-anchored walker without an API change.

use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Default upper bound for text files read by the toolkit (1 MiB).
pub const MAX_TEXT_FILE_BYTES: u64 = 1024 * 1024;

/// Failure of a path or file-reading contract.
#[derive(Debug)]
pub enum PathError {
    /// A relative path was rejected before touching the filesystem.
    Invalid(String),
    /// The path does not exist.
    NotFound(PathBuf),
    /// The path (or a component of it) is a symbolic link.
    Symlink(PathBuf),
    /// The path exists but is not a regular file.
    NotRegularFile(PathBuf),
    /// The file is larger than the allowed number of bytes.
    TooLarge {
        /// The offending file.
        path: PathBuf,
        /// The byte limit that was exceeded.
        limit: u64,
    },
    /// The file is not valid UTF-8.
    NotUtf8(PathBuf),
    /// The file was replaced between the symlink check and the open.
    Changed(PathBuf),
    /// Any other I/O failure.
    Io(io::Error, PathBuf),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid path: {reason}"),
            Self::NotFound(path) => write!(f, "path does not exist: {}", path.display()),
            Self::Symlink(path) => {
                write!(f, "refusing to follow symbolic link: {}", path.display())
            }
            Self::NotRegularFile(path) => write!(f, "not a regular file: {}", path.display()),
            Self::TooLarge { path, limit } => {
                write!(f, "{} is larger than {limit} bytes", path.display())
            }
            Self::NotUtf8(path) => write!(f, "{} is not valid UTF-8", path.display()),
            Self::Changed(path) => {
                write!(f, "{} changed while it was being opened", path.display())
            }
            Self::Io(error, path) => write!(f, "cannot read {}: {error}", path.display()),
        }
    }
}

impl std::error::Error for PathError {}

/// Normalizes an untrusted relative path to `/`-joined segments.
///
/// Empty and `.` segments and repeated separators are dropped. The path is
/// rejected when it is empty, has no real component, contains a NUL byte or a
/// backslash, is absolute (leading `/` or a drive prefix such as `C:`), or has
/// a `..` segment.
///
/// # Errors
///
/// Returns [`PathError::Invalid`] naming the first violated rule.
pub fn normalize_relative(input: &str) -> Result<String, PathError> {
    let invalid = |reason: &str| Err(PathError::Invalid(format!("{reason}: {input:?}")));
    if input.is_empty() {
        return invalid("path is empty");
    }
    if input.contains('\0') {
        return invalid("path contains a NUL byte");
    }
    if input.contains('\\') {
        return invalid("backslash separators are not allowed");
    }
    if input.starts_with('/') {
        return invalid("absolute paths are not allowed");
    }
    let mut chars = input.chars();
    if matches!((chars.next(), chars.next()), (Some(drive), Some(':')) if drive.is_ascii_alphabetic())
    {
        return invalid("drive-prefixed paths are not allowed");
    }
    let mut segments: Vec<&str> = Vec::new();
    for segment in input.split('/') {
        match segment {
            "" | "." => {}
            ".." => return invalid("parent traversal ('..') is not allowed"),
            other => segments.push(other),
        }
    }
    if segments.is_empty() {
        return invalid("path has no components");
    }
    Ok(segments.join("/"))
}

/// Joins `relative` under `root` after [`normalize_relative`], requiring that
/// every component exists and none is a symbolic link.
///
/// # Errors
///
/// Returns [`PathError::Invalid`] for a rejected relative path,
/// [`PathError::NotFound`] when a component is missing,
/// [`PathError::Symlink`] when a component is a symbolic link and
/// [`PathError::Io`] for other filesystem failures.
pub fn resolve_within(root: &Path, relative: &str) -> Result<PathBuf, PathError> {
    let normalized = normalize_relative(relative)?;
    let mut current = root.to_path_buf();
    for segment in normalized.split('/') {
        current.push(segment);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(PathError::Symlink(current));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(PathError::NotFound(current));
            }
            Err(error) => return Err(PathError::Io(error, current)),
        }
    }
    Ok(current)
}

/// Opens a regular file for reading, refusing symbolic links and non-regular
/// files, and returns the handle with its metadata.
///
/// This is the shared open step behind [`read_regular_file`] and the byte-exact
/// synchronization commands (#4377), which must not impose a text size limit.
/// On Unix the opened handle's device and inode must match the pre-open
/// `lstat` (final component only; see the module threat model).
///
/// # Errors
///
/// Returns [`PathError::NotFound`], [`PathError::Symlink`],
/// [`PathError::NotRegularFile`], [`PathError::Changed`] or
/// [`PathError::Io`].
pub fn open_regular_file(path: &Path) -> Result<(fs::File, fs::Metadata), PathError> {
    let to_error = |error: io::Error| io_error(path, error);
    let before = fs::symlink_metadata(path).map_err(to_error)?;
    if before.file_type().is_symlink() {
        return Err(PathError::Symlink(path.to_path_buf()));
    }
    if !before.is_file() {
        return Err(PathError::NotRegularFile(path.to_path_buf()));
    }
    let file = fs::File::open(path).map_err(to_error)?;
    let opened = file.metadata().map_err(to_error)?;
    if !opened.is_file() || !same_file(&before, &opened) {
        return Err(PathError::Changed(path.to_path_buf()));
    }
    Ok((file, opened))
}

/// Reads a UTF-8 text file of at most `max_bytes` bytes, refusing symbolic
/// links and non-regular files.
///
/// # Errors
///
/// Returns [`PathError::NotFound`], [`PathError::Symlink`],
/// [`PathError::NotRegularFile`], [`PathError::Changed`] (identity mismatch
/// after open, Unix), [`PathError::TooLarge`], [`PathError::NotUtf8`] or
/// [`PathError::Io`].
pub fn read_regular_file(path: &Path, max_bytes: u64) -> Result<String, PathError> {
    let (file, opened) = open_regular_file(path)?;
    let to_error = |error: io::Error| io_error(path, error);
    if opened.len() > max_bytes {
        return Err(PathError::TooLarge {
            path: path.to_path_buf(),
            limit: max_bytes,
        });
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(to_error)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(PathError::TooLarge {
            path: path.to_path_buf(),
            limit: max_bytes,
        });
    }
    String::from_utf8(bytes).map_err(|_| PathError::NotUtf8(path.to_path_buf()))
}

fn io_error(path: &Path, error: io::Error) -> PathError {
    if error.kind() == io::ErrorKind::NotFound {
        PathError::NotFound(path.to_path_buf())
    } else {
        PathError::Io(error, path.to_path_buf())
    }
}

#[cfg(unix)]
fn same_file(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == after.dev() && before.ino() == after.ino()
}

#[cfg(not(unix))]
fn same_file(_before: &fs::Metadata, _after: &fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn invalid_reason(input: &str) -> String {
        let Err(PathError::Invalid(reason)) = normalize_relative(input) else {
            panic!("expected an Invalid error for {input:?}");
        };
        reason
    }

    #[test]
    fn normalization_collapses_redundant_segments() {
        assert_eq!(normalize_relative("a/b").expect("ok"), "a/b");
        assert_eq!(normalize_relative("./a//b/./c/").expect("ok"), "a/b/c");
        assert_eq!(normalize_relative("single").expect("ok"), "single");
    }

    #[test]
    fn escaping_and_ambiguous_paths_are_rejected() {
        assert!(invalid_reason("").contains("empty"));
        assert!(invalid_reason("/etc/passwd").contains("absolute"));
        assert!(invalid_reason("../x").contains("parent traversal"));
        assert!(invalid_reason("a/../b").contains("parent traversal"));
        assert!(invalid_reason("a/b/..").contains("parent traversal"));
        assert!(invalid_reason("a\\b").contains("backslash"));
        assert!(invalid_reason("a\0b").contains("NUL"));
        assert!(invalid_reason("C:/x").contains("drive"));
        assert!(invalid_reason(".").contains("no components"));
        assert!(invalid_reason("./").contains("no components"));
        // A name that merely contains dots is fine.
        assert_eq!(normalize_relative("..a/b..").expect("ok"), "..a/b..");
    }

    #[test]
    fn resolve_within_returns_existing_real_paths() {
        let root = TempDir::new("resolve-ok");
        std::fs::create_dir_all(root.join("a/b")).expect("mkdir");
        let resolved = resolve_within(&root, "a/./b").expect("resolves");
        assert_eq!(resolved, root.join("a").join("b"));
    }

    #[test]
    fn resolve_within_reports_missing_components() {
        let root = TempDir::new("resolve-missing");
        let error = resolve_within(&root, "nope/x").expect_err("missing");
        assert!(matches!(error, PathError::NotFound(_)));
        let error = resolve_within(&root, "../escape").expect_err("escape");
        assert!(matches!(error, PathError::Invalid(_)));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_within_refuses_symlink_components() {
        let root = TempDir::new("resolve-link");
        let outside = TempDir::new("resolve-link-outside");
        std::fs::create_dir_all(outside.join("secret")).expect("mkdir");
        std::os::unix::fs::symlink(outside.join("secret"), root.join("link")).expect("symlink");
        let error = resolve_within(&root, "link/inner").expect_err("link refused");
        assert!(matches!(error, PathError::Symlink(_)), "{error}");
        let error = resolve_within(&root, "link").expect_err("final link refused");
        assert!(matches!(error, PathError::Symlink(_)));
    }

    #[test]
    fn reads_regular_utf8_files() {
        let root = TempDir::new("read-ok");
        std::fs::write(root.join("f.txt"), "héllo\n").expect("write");
        assert_eq!(
            read_regular_file(&root.join("f.txt"), MAX_TEXT_FILE_BYTES).expect("read"),
            "héllo\n"
        );
    }

    #[test]
    fn missing_directory_and_oversized_and_binary_files_are_refused() {
        let root = TempDir::new("read-bad");
        let missing = read_regular_file(&root.join("absent"), 10).expect_err("missing");
        assert!(matches!(missing, PathError::NotFound(_)));

        std::fs::create_dir(root.join("dir")).expect("mkdir");
        let directory = read_regular_file(&root.join("dir"), 10).expect_err("dir");
        assert!(matches!(directory, PathError::NotRegularFile(_)));

        std::fs::write(root.join("big"), "0123456789ab").expect("write");
        let big = read_regular_file(&root.join("big"), 11).expect_err("too large");
        assert!(matches!(big, PathError::TooLarge { limit: 11, .. }));
        assert_eq!(
            read_regular_file(&root.join("big"), 12).expect("exact limit"),
            "0123456789ab"
        );

        std::fs::write(root.join("bin"), [0xff, 0xfe, 0x00]).expect("write");
        let binary = read_regular_file(&root.join("bin"), 10).expect_err("not utf-8");
        assert!(matches!(binary, PathError::NotUtf8(_)));
        assert!(binary.to_string().contains("not valid UTF-8"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_files_are_never_read() {
        let root = TempDir::new("read-link");
        let outside = TempDir::new("read-link-outside");
        std::fs::write(outside.join("secret.txt"), "top secret").expect("write");
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link.txt"))
            .expect("symlink");
        let error = read_regular_file(&root.join("link.txt"), 1024).expect_err("refused");
        assert!(matches!(error, PathError::Symlink(_)), "{error}");
        assert!(!error.to_string().contains("top secret"));
        // A dangling link is also a symlink refusal, not "not found".
        std::os::unix::fs::symlink(root.join("gone"), root.join("dangling")).expect("symlink");
        let dangling = read_regular_file(&root.join("dangling"), 1024).expect_err("refused");
        assert!(matches!(dangling, PathError::Symlink(_)));
    }
}
