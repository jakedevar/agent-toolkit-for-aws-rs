//! The target repository's actual Git metadata (R-AR-META, #5061).
//!
//! A repository's administrative directory need not be named `.git`
//! (`--separate-git-dir`, linked worktrees, `core.worktree`), so names cannot
//! protect it. Before the snapshot is read, `apply-snapshot` refuses a
//! caller environment that redirects Git, asks Git itself where the
//! metadata is with one hardened `git rev-parse`, pins the reported Git and
//! common directories by `(st_dev, st_ino)` (the protected set `M`) and the
//! top level (`K`), and walks up from the held `--into` descriptor to `K`
//! with `..` steps, refusing a root inside metadata. R-AR-APPLY then refuses
//! any existing target directory in `M` or shaped like a Git directory, and
//! any Git-shaped snapshot directory.

use std::ffi::OsStr;
use std::path::Path;

use super::fsops::{self, DirHandle};
use super::git::{call, GitCall, GitRunner};
use super::nav::Ident;
use crate::launch::text::shown;
use crate::launch::LaunchError;
use toolkit_aws::Env;

/// Caller variables that change which metadata the user's later `git`
/// commands use; any of them set (non-empty) refuses the apply.
pub const REDIRECTING_ENV: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
];

/// Largest `rev-parse` output accepted.
pub const DISCOVERY_STDOUT_CAP: usize = 64 * 1024;
/// Most `..` steps of the root containment walk.
pub const MAX_UP_STEPS: usize = 2048;

/// The fixed reason codes of a failed discovery.
pub mod reason {
    /// Git could not be started.
    pub const GIT_MISSING: &str = "git_missing";
    /// Git exited non-zero (not a repository, `safe.directory`).
    pub const GIT_FAILED: &str = "git_failed";
    /// Git passed the time limit.
    pub const TIMEOUT: &str = "timeout";
    /// Git's output passed the cap.
    pub const TOO_LARGE: &str = "output_too_large";
    /// Not exactly six LF-terminated lines.
    pub const LINES: &str = "unexpected_output";
    /// `--path-format=absolute` echoed back (Git older than 2.31).
    pub const OLD_GIT: &str = "git_too_old";
    /// Not inside a work tree (or inside the Git directory, or bare).
    pub const NOT_WORK_TREE: &str = "not_a_work_tree";
    /// A reported path is not absolute.
    pub const RELATIVE: &str = "relative_path";
    /// A reported directory could not be opened.
    pub const OPEN: &str = "metadata_unreadable";
    /// The containment walk never met the top level.
    pub const OUTSIDE: &str = "outside_work_tree";
}

/// The discovery refusal.
#[must_use]
pub fn discovery_message(code: &str) -> String {
    format!(
        "apply-snapshot could not locate the target repository's Git metadata ({code}); \
         nothing was written."
    )
}

/// The target-side refusal.
#[must_use]
pub fn metadata_message(shown_path: &str) -> String {
    format!("The snapshot would write into Git metadata at {shown_path} and was not applied.")
}

/// The snapshot-side refusal.
#[must_use]
pub fn shaped_snapshot_message(shown_path: &str) -> String {
    format!("The snapshot contains a directory shaped like Git metadata: {shown_path}.")
}

/// The protected identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protected {
    /// The Git directory and the common directory (`M`).
    pub metadata: Vec<Ident>,
    /// The top-level directory (`K`).
    pub top: Ident,
}

/// Refuse a caller environment that redirects Git.
///
/// # Errors
/// "apply-snapshot does not run with <NAME> set; unset it and re-run."
pub fn check_environment(env: &dyn Env) -> Result<(), LaunchError> {
    for name in REDIRECTING_ENV {
        if env.var(name).is_some_and(|value| !value.is_empty()) {
            return Err(LaunchError::Archive(format!(
                "apply-snapshot does not run with {name} set; unset it and re-run."
            )));
        }
    }
    Ok(())
}

fn refuse(code: &str) -> LaunchError {
    LaunchError::Archive(discovery_message(code))
}

/// Ask Git where the metadata of the work tree at `into` is and pin it.
///
/// # Errors
/// [`discovery_message`] with the fixed reason.
pub fn discover(git: &GitRunner, into: &Path) -> Result<Protected, LaunchError> {
    let runner = GitRunner {
        stdout_cap: DISCOVERY_STDOUT_CAP,
        ..git.clone()
    };
    let args = [
        "rev-parse",
        "--path-format=absolute",
        "--is-inside-work-tree",
        "--is-inside-git-dir",
        "--is-bare-repository",
        "--show-toplevel",
        "--git-dir",
        "--git-common-dir",
    ]
    .map(OsStr::new);
    let stdout = match call(&runner, into, &args) {
        GitCall::Done {
            success: true,
            stdout,
        } => stdout,
        GitCall::Done { success: false, .. } => return Err(refuse(reason::GIT_FAILED)),
        GitCall::Spawn => return Err(refuse(reason::GIT_MISSING)),
        GitCall::Timeout => return Err(refuse(reason::TIMEOUT)),
        GitCall::TooLarge => return Err(refuse(reason::TOO_LARGE)),
    };
    let text = std::str::from_utf8(&stdout).map_err(|_| refuse(reason::LINES))?;
    let Some(body) = text.strip_suffix('\n') else {
        return Err(refuse(reason::LINES));
    };
    let lines: Vec<&str> = body.split('\n').collect();
    if lines.first() == Some(&"--path-format=absolute") {
        return Err(refuse(reason::OLD_GIT));
    }
    if lines.len() != 6 {
        return Err(refuse(reason::LINES));
    }
    if lines[..3] != ["true", "false", "false"] {
        return Err(refuse(reason::NOT_WORK_TREE));
    }
    if !lines[3..].iter().all(|path| path.starts_with('/')) {
        return Err(refuse(reason::RELATIVE));
    }
    let open = |path: &str| {
        fsops::open_root(Path::new(path))
            .map(|dir| dir.identity())
            .map_err(|_| refuse(reason::OPEN))
    };
    let top = open(lines[3])?;
    let metadata = vec![open(lines[4])?, open(lines[5])?];
    Ok(Protected { metadata, top })
}

/// A directory Git could treat as a repository directory: an entry named
/// `commondir`, or `HEAD` with `objects` or `refs` (any types).
#[must_use]
pub fn is_git_shaped(dir: &DirHandle) -> bool {
    let has = |name: &std::ffi::CStr| fsops::stat_at(dir, name).is_ok();
    has(c"commondir") || (has(c"HEAD") && (has(c"objects") || has(c"refs")))
}

/// The same rule over a listed directory's names.
#[must_use]
pub fn names_are_git_shaped<'a>(names: impl IntoIterator<Item = &'a [u8]>) -> bool {
    let (mut commondir, mut head, mut store) = (false, false, false);
    for name in names {
        match name {
            b"commondir" => commondir = true,
            b"HEAD" => head = true,
            b"objects" | b"refs" => store = true,
            _ => {}
        }
    }
    commondir || (head && store)
}

/// Which target-side rules run (both in production; T-APPLY-11 disables one
/// at a time to show the other refuses on its own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaRules {
    /// The identity rule.
    pub identity: bool,
    /// The Git-shaped rule.
    pub shaped: bool,
}

impl Protected {
    /// Whether an existing target directory may be written into.
    #[must_use]
    pub fn allows(&self, dir: &DirHandle, rules: MetaRules) -> bool {
        !(rules.identity && self.metadata.contains(&dir.identity())
            || rules.shaped && is_git_shaped(dir))
    }
}

/// Walk up from the held `--into` directory to the top level; every
/// directory on the way must be allowed.
///
/// # Errors
/// [`metadata_message`] for a root inside metadata, [`reason::OUTSIDE`]
/// when the walk never meets the top level.
pub fn check_root(
    into: &DirHandle,
    into_path: &Path,
    protected: &Protected,
    rules: MetaRules,
) -> Result<(), LaunchError> {
    let inside = || LaunchError::Archive(metadata_message(&shown(&into_path.to_string_lossy())));
    let mut current = fsops::enter_dir(into, c".").map_err(|_| refuse(reason::OPEN))?;
    for _ in 0..=MAX_UP_STEPS {
        if !protected.allows(&current, rules) {
            return Err(inside());
        }
        if current.identity() == protected.top {
            return Ok(());
        }
        let parent = fsops::enter_dir(&current, c"..").map_err(|_| refuse(reason::OPEN))?;
        if parent.identity() == current.identity() {
            break;
        }
        current = parent;
    }
    Err(refuse(reason::OUTSIDE))
}

impl MetaRules {
    /// Both rules.
    pub const PRODUCTION: Self = Self {
        identity: true,
        shaped: true,
    };
}
