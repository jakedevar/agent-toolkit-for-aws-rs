//! Git ignore rules for the upload selection (R-AR-GIT; Python
//! `_git_included_files`, `_gitignore_files_without_repo`,
//! `_has_git_metadata`, `_has_gitignore`).
//!
//! Git runs from the configured program path with `current_dir = root`,
//! `stdin` null, `stderr` discarded, `stdout` collected into a [`CappedBuf`]
//! (256 MiB, #5082), a wall-clock limit per call (30 s; the whole process
//! group is killed at the limit), `-c core.fsmonitor=false` prepended, and
//! the caller's `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`,
//! `GIT_COMMON_DIR`, `GIT_OBJECT_DIRECTORY`,
//! `GIT_ALTERNATE_OBJECT_DIRECTORIES` and every `GIT_CONFIG*` variable
//! removed, so the selection always describes `root`. Git's output only
//! selects names; it never supplies archive bytes.
//!
//! [`CappedBuf`]: crate::launch::buffer::CappedBuf

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::fsops::{kill_process_group, list_names, open_dir_at, stat_at, DirHandle, EntryType};
use super::select::{is_skip_dir, MAX_DEPTH};
use crate::launch::buffer::CappedBuf;
use crate::launch::clock::{random_hex, Random};
use crate::launch::text::shown;
use crate::launch::LaunchError;

/// Wall-clock limit per Git call.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest captured Git output.
pub const GIT_STDOUT_CAP: usize = 256 * 1024 * 1024;

/// The fixed messages (Python parity).
pub mod message {
    /// `ls-files --ignored` failed after the selection succeeded.
    pub const TRACKED_IGNORED: &str =
        "Git could not determine which tracked files match ignore rules.";
    /// The selection failed in a Git worktree (or the output was refused).
    pub const SELECTION: &str = "Git could not determine which files are safe to upload. \
                                 Resolve the Git error and try again.";
    /// A call passed the time limit.
    pub const TIMEOUT: &str = "Git timed out while determining which files are safe to upload.";
    /// Git could not run while rules apply.
    pub const REQUIRED: &str =
        "Git is required to apply this source directory's ignore rules before upload.";
    /// `git init --bare` failed for the temporary metadata.
    pub const INIT: &str = "Git could not initialize temporary metadata to apply ignore rules.";
}

/// How Git is run.
#[derive(Debug, Clone)]
pub struct GitRunner {
    /// The program (`git` from `PATH` in production).
    pub program: PathBuf,
    /// Per-call wall-clock limit.
    pub timeout: Duration,
    /// Largest captured output.
    pub stdout_cap: usize,
    /// Test seam, `None` in production: stop Git's repository discovery
    /// (`GIT_CEILING_DIRECTORIES`) and the `.git` search of
    /// [`has_git_metadata`] at this directory, so test trees under a
    /// temporary directory that itself lies inside a repository are judged
    /// on their own.
    pub ceiling: Option<PathBuf>,
}

impl GitRunner {
    /// `git` from `PATH` with the production limits.
    #[must_use]
    pub fn system() -> Self {
        Self {
            program: PathBuf::from("git"),
            timeout: GIT_TIMEOUT,
            stdout_cap: GIT_STDOUT_CAP,
            ceiling: None,
        }
    }
}

/// Caller variables that would point Git at another repository or inject
/// configuration (every `GIT_CONFIG*` name is removed as well).
const REMOVED_ENV: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// One finished Git call.
struct GitOutput {
    success: bool,
    stdout: Vec<u8>,
}

/// Why a Git call produced no output.
enum GitFailure {
    /// Git could not be started (Python `OSError`).
    Spawn,
    /// The call passed the time limit (Python `TimeoutExpired`).
    Timeout,
    /// The output passed the cap.
    TooLarge,
}

/// Read `stdout` into a [`CappedBuf`] of `cap` bytes; `None` past the cap.
fn read_capped(stdout: &mut impl Read, cap: usize) -> Option<Vec<u8>> {
    let mut buf = CappedBuf::new(cap);
    let mut chunk = [0_u8; 8192];
    loop {
        match stdout.read(&mut chunk) {
            Ok(0) => return Some(buf.into_vec()),
            Ok(read) => buf.extend(&chunk[..read]).ok()?,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Some(buf.into_vec()),
        }
    }
}

fn stop(child: &mut Child) {
    kill_process_group(child);
    let _ = child.kill();
    let _ = child.wait();
}

/// Run `git -c core.fsmonitor=false <args>` in `cwd` under the runner's
/// limits.
fn run(runner: &GitRunner, cwd: &Path, args: &[&OsStr]) -> Result<GitOutput, GitFailure> {
    let mut command = Command::new(&runner.program);
    command
        .arg("-c")
        .arg("core.fsmonitor=false")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    for name in REMOVED_ENV {
        command.env_remove(name);
    }
    if let Some(ceiling) = &runner.ceiling {
        command.env("GIT_CEILING_DIRECTORIES", ceiling);
    }
    for (name, _) in std::env::vars_os() {
        if name
            .to_str()
            .is_some_and(|name| name.starts_with("GIT_CONFIG"))
        {
            command.env_remove(name);
        }
    }
    let mut child = command.spawn().map_err(|_| GitFailure::Spawn)?;
    let Some(mut stdout) = child.stdout.take() else {
        stop(&mut child);
        return Err(GitFailure::Spawn);
    };
    let cap = runner.stdout_cap;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(read_capped(&mut stdout, cap));
    });
    let deadline = Instant::now() + runner.timeout;
    let mut output: Option<Option<Vec<u8>>> = None;
    loop {
        if output.is_none() {
            if let Ok(read) = receiver.try_recv() {
                output = Some(read);
            }
        }
        if matches!(output, Some(None)) {
            stop(&mut child);
            return Err(GitFailure::TooLarge);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let read = if let Some(read) = output.take() {
                    read
                } else {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let Ok(read) = receiver.recv_timeout(remaining) else {
                        // A grandchild still holds the pipe.
                        kill_process_group(&child);
                        return Err(GitFailure::Timeout);
                    };
                    read
                };
                return read
                    .map(|stdout| GitOutput {
                        success: status.success(),
                        stdout,
                    })
                    .ok_or(GitFailure::TooLarge);
            }
            Ok(None) => {}
            Err(_) => {
                stop(&mut child);
                return Err(GitFailure::Spawn);
            }
        }
        if Instant::now() >= deadline {
            stop(&mut child);
            return Err(GitFailure::Timeout);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// The outcome of one hardened Git call for other archive modules
/// (R-AR-META's discovery).
pub(crate) enum GitCall {
    /// Git ran to the end.
    Done {
        /// Exit status 0.
        success: bool,
        /// Captured stdout (at most the runner's cap).
        stdout: Vec<u8>,
    },
    /// Git could not be started.
    Spawn,
    /// The call passed the time limit.
    Timeout,
    /// The output passed the cap.
    TooLarge,
}

/// Run `git -c core.fsmonitor=false <args>` in `cwd` with the runner's
/// hardening (null stdin, discarded stderr, capped stdout, time limit,
/// repository-redirecting variables removed).
pub(crate) fn call(runner: &GitRunner, cwd: &Path, args: &[&OsStr]) -> GitCall {
    match run(runner, cwd, args) {
        Ok(output) => GitCall::Done {
            success: output.success,
            stdout: output.stdout,
        },
        Err(GitFailure::Spawn) => GitCall::Spawn,
        Err(GitFailure::Timeout) => GitCall::Timeout,
        Err(GitFailure::TooLarge) => GitCall::TooLarge,
    }
}

/// NUL-separated paths as UTF-8 strings.
fn decode_paths(stdout: &[u8]) -> Result<HashSet<String>, LaunchError> {
    stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            String::from_utf8(path.to_vec()).map_err(|_| {
                LaunchError::Archive(format!(
                    "Archive cannot include a file whose name is not valid UTF-8: {}",
                    shown(&String::from_utf8_lossy(path))
                ))
            })
        })
        .collect()
}

fn archive_error(message: &str) -> LaunchError {
    LaunchError::Archive(message.to_string())
}

/// The paths (relative to `root`, `/`-separated) Git selects, or `None` when
/// no ignore rules apply.
///
/// `root_path` is the absolute root Git runs in; `root` is the held
/// descriptor the `.gitignore` search walks; `temp_parent` holds the
/// private temporary metadata of a tree outside Git.
///
/// # Errors
/// The Python messages of [`message`], or the non-UTF-8 name refusal.
pub fn git_included_files(
    runner: &GitRunner,
    root_path: &Path,
    root: &DirHandle,
    temp_parent: &Path,
    random: &dyn Random,
) -> Result<Option<HashSet<String>>, LaunchError> {
    let unavailable = || {
        if has_git_metadata(root_path, runner.ceiling.as_deref()) || has_gitignore(root) {
            Err(archive_error(message::REQUIRED))
        } else {
            Ok(None)
        }
    };
    let failure = |failure: GitFailure| match failure {
        GitFailure::Spawn => unavailable(),
        GitFailure::Timeout => Err(archive_error(message::TIMEOUT)),
        GitFailure::TooLarge => Err(archive_error(message::SELECTION)),
    };
    let listed = [
        OsStr::new("ls-files"),
        OsStr::new("--cached"),
        OsStr::new("--others"),
        OsStr::new("--exclude-standard"),
        OsStr::new("-z"),
    ];
    let selected = match run(runner, root_path, &listed) {
        Ok(output) => output,
        Err(error) => return failure(error),
    };
    if selected.success {
        let ignored_args = [
            OsStr::new("ls-files"),
            OsStr::new("--cached"),
            OsStr::new("--ignored"),
            OsStr::new("--exclude-standard"),
            OsStr::new("-z"),
        ];
        let ignored = match run(runner, root_path, &ignored_args) {
            Ok(output) if output.success => output,
            Ok(_) => return Err(archive_error(message::TRACKED_IGNORED)),
            Err(error) => return failure(error),
        };
        let mut paths = decode_paths(&selected.stdout)?;
        for path in decode_paths(&ignored.stdout)? {
            paths.remove(&path);
        }
        return Ok(Some(paths));
    }
    if has_git_metadata(root_path, runner.ceiling.as_deref()) {
        return Err(archive_error(message::SELECTION));
    }
    if !has_gitignore(root) {
        return Ok(None);
    }
    match without_repository(runner, root_path, temp_parent, random) {
        Ok(paths) => Ok(Some(paths)),
        Err(Ok(error)) => Err(error),
        Err(Err(error)) => failure(error),
    }
}

/// Python `_gitignore_files_without_repo`: a private temporary bare
/// repository applies the rules to a tree outside Git.
fn without_repository(
    runner: &GitRunner,
    root_path: &Path,
    temp_parent: &Path,
    random: &dyn Random,
) -> Result<HashSet<String>, Result<LaunchError, GitFailure>> {
    let nonce = random_hex(random, 8).map_err(Ok)?;
    let git_dir = temp_parent.join(format!("launch-with-aws-git-{nonce}"));
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(&git_dir)
        .map_err(|_| Err(GitFailure::Spawn))?;
    let result = (|| {
        let init = run(
            runner,
            root_path,
            &[
                OsStr::new("init"),
                OsStr::new("--bare"),
                OsStr::new("--quiet"),
                git_dir.as_os_str(),
            ],
        )
        .map_err(Err)?;
        if !init.success {
            return Err(Ok(archive_error(message::INIT)));
        }
        let mut git_dir_arg = OsString::from("--git-dir=");
        git_dir_arg.push(&git_dir);
        let mut work_tree_arg = OsString::from("--work-tree=");
        work_tree_arg.push(root_path);
        let listed = run(
            runner,
            root_path,
            &[
                &git_dir_arg,
                &work_tree_arg,
                OsStr::new("ls-files"),
                OsStr::new("--others"),
                OsStr::new("--exclude-standard"),
                OsStr::new("-z"),
            ],
        )
        .map_err(Err)?;
        if !listed.success {
            return Err(Ok(archive_error(message::SELECTION)));
        }
        decode_paths(&listed.stdout).map_err(Ok)
    })();
    let _ = fs::remove_dir_all(&git_dir);
    result
}

/// Whether `root` (absolute) is at or below a directory containing `.git`,
/// searching no higher than below `ceiling` when one is set (test seam).
#[must_use]
pub fn has_git_metadata(root: &Path, ceiling: Option<&Path>) -> bool {
    root.ancestors()
        .take_while(|ancestor| ceiling != Some(*ancestor))
        .any(|ancestor| ancestor.join(".git").exists())
}

/// Whether the tree below `root` holds a `.gitignore`, skipping the
/// `_SKIP_DIRS` directories and never following a symlink.
#[must_use]
pub fn has_gitignore(root: &DirHandle) -> bool {
    gitignore_below(root, 0)
}

fn gitignore_below(dir: &DirHandle, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let Ok(names) = list_names(dir) else {
        return false;
    };
    for name in names {
        let Ok(stat) = stat_at(dir, &name) else {
            continue;
        };
        let text = name.to_str().ok();
        match stat.kind {
            EntryType::Directory => {
                if text.is_some_and(is_skip_dir) {
                    continue;
                }
                if let Ok(sub) = open_dir_at(dir, &name, &stat) {
                    if gitignore_below(&sub, depth + 1) {
                        return true;
                    }
                }
            }
            _ if text == Some(".gitignore") => return true,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests_git;
