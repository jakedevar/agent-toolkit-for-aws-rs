//! Shared harness for the startup cross-file contracts.
//!
//! A contract is a function over a [`Plugin`] view that returns `Err(reason)`
//! when the committed plugin text breaks it. [`Plugin::real`] views the
//! committed `plugins/aws-startup-advisor` tree. A mutation view layers one
//! edited copy of a file, written into a private temporary directory, over the
//! real tree, so the same contract can be shown to fail on a broken copy
//! without touching the committed files (see [`check`]).

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};

/// Result of one contract.
pub type Verdict = Result<(), String>;

/// Fail a contract with a formatted reason unless `cond` holds.
macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !($cond) {
            return Err(format!($($arg)+));
        }
    };
}
pub(crate) use ensure;

thread_local! {
    static NEXT: Cell<u32> = const { Cell::new(0) };
}

/// A private scratch directory removed on drop.
pub struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let n = NEXT.with(|c| {
            let v = c.get();
            c.set(v + 1);
            v
        });
        let dir = std::env::temp_dir().join(format!(
            "startup-contracts-{}-{:?}-{n}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A view of the plugin tree: the real files, optionally shadowed by edited
/// copies kept in a scratch directory.
pub struct Plugin {
    root: PathBuf,
    overlay: Option<Scratch>,
}

impl Plugin {
    /// The committed plugin tree.
    pub fn real() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("repository root")
            .join("plugins")
            .join("aws-startup-advisor");
        assert!(root.is_dir(), "{} is not a directory", root.display());
        Self {
            root,
            overlay: None,
        }
    }

    /// A view rooted at `root`, which need not exist (negative cases).
    pub fn at(root: PathBuf) -> Self {
        Self {
            root,
            overlay: None,
        }
    }

    fn resolve(&self, rel: &str) -> PathBuf {
        if let Some(scratch) = &self.overlay {
            let shadow = scratch.0.join(rel);
            if shadow.exists() {
                return shadow;
            }
        }
        self.root.join(rel)
    }

    /// Read a UTF-8 file, as Python's `read_text(encoding="utf-8")` does.
    pub fn read(&self, rel: &str) -> Result<String, String> {
        let path = self.resolve(rel);
        fs::read_to_string(&path).map_err(|e| format!("{rel}: {e}"))
    }

    pub fn is_file(&self, rel: &str) -> bool {
        self.resolve(rel).is_file()
    }

    /// This view with `rel` replaced by `edit` applied to its text, the copy
    /// written under a scratch directory.
    ///
    /// # Panics
    /// When the edit leaves the text unchanged: a mutation that changes
    /// nothing would make a "fails on a broken copy" test vacuous.
    pub fn with_edit(&self, rel: &str, edit: impl FnOnce(&str) -> String) -> Plugin {
        let before = self.read(rel).expect("mutation target must exist");
        let after = edit(&before);
        assert_ne!(before, after, "the edit of {rel} changed nothing");
        let scratch = match &self.overlay {
            Some(_) => panic!("stack edits with `with_edits`"),
            None => Scratch::new(),
        };
        let dest = scratch.0.join(rel);
        fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir");
        fs::write(&dest, after).expect("write edited copy");
        Plugin {
            root: self.root.clone(),
            overlay: Some(scratch),
        }
    }
}

/// One edit: the file (relative to the plugin root) and the text transform.
pub struct Edit {
    pub file: &'static str,
    pub apply: fn(&str) -> String,
}

/// `file` with every occurrence of `from` replaced by `to`.
#[macro_export]
macro_rules! edit_replace {
    ($file:expr, $from:expr, $to:expr) => {
        $crate::common::Edit {
            file: $file,
            apply: |t| t.replace($from, $to),
        }
    };
}

/// `file` with its text emptied.
#[macro_export]
macro_rules! edit_empty {
    ($file:expr) => {
        $crate::common::Edit {
            file: $file,
            apply: |_| String::from("\n"),
        }
    };
}

/// Run `contract` on the committed tree (it must pass) and once per `breaks`
/// entry on an edited copy (it must fail, and the reason must contain the
/// expected fragment).
///
/// # Panics
/// When the contract rejects the committed tree, accepts an edited copy, or
/// rejects it for a different reason.
pub fn check(contract: fn(&Plugin) -> Verdict, breaks: &[(Edit, &str)]) {
    let real = Plugin::real();
    if let Err(reason) = contract(&real) {
        panic!("the committed plugin breaks the contract: {reason}");
    }
    assert!(!breaks.is_empty(), "a contract needs a failing mutation");
    for (edit, expected) in breaks {
        let broken = real.with_edit(edit.file, edit.apply);
        match contract(&broken) {
            Ok(()) => panic!("an edited copy of {} still passes the contract", edit.file),
            Err(reason) => assert!(
                reason.contains(expected),
                "edited {} failed for another reason: {reason} (wanted {expected:?})",
                edit.file
            ),
        }
    }
}

/// Byte index of the first `needle` in `text`, as Python's `str.index` (an
/// absent needle is an error instead of `-1`).
pub fn idx(text: &str, needle: &str) -> Result<usize, String> {
    text.find(needle)
        .ok_or_else(|| format!("{needle:?} not found"))
}

/// `text.index(needle, from)`.
pub fn idx_from(text: &str, needle: &str, from: usize) -> Result<usize, String> {
    text.get(from..)
        .and_then(|rest| rest.find(needle))
        .map(|i| i + from)
        .ok_or_else(|| format!("{needle:?} not found after byte {from}"))
}

/// `text[text.index(start):text.index(end)]`: both indexes come from the
/// start of the text and a reversed pair is the empty string.
pub fn between<'a>(text: &'a str, start: &str, end: &str) -> Result<&'a str, String> {
    let s = idx(text, start)?;
    let e = idx(text, end)?;
    Ok(if e < s { "" } else { &text[s..e] })
}

/// Whether `pattern` (Python `re` syntax, no flags) matches anywhere in `text`.
pub fn has(pattern: &str, text: &str) -> bool {
    fancy_regex::Regex::new(pattern)
        .expect("valid pattern")
        .is_match(text)
        .expect("match")
}

/// `text[i:i + n]` with `i = text.index(anchor)`; indexes count characters as
/// in Python.
pub fn window(text: &str, anchor: &str, n: usize) -> Result<String, String> {
    let i = idx(text, anchor)?;
    Ok(text[i..].chars().take(n).collect())
}

/// `text[i:]` with `i = text.index(anchor)`.
pub fn from(text: &str, anchor: &str) -> Result<String, String> {
    Ok(text[idx(text, anchor)?..].to_owned())
}

/// Last path component, for failure messages.
pub fn name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `text` with `from` replaced by `to` only inside the `len`-character window
/// that starts at `anchor` (mutation helper for window-scoped contracts).
pub fn replace_in_window(text: &str, anchor: &str, len: usize, from: &str, to: &str) -> String {
    let start = text.find(anchor).expect("anchor present");
    let end = text[start..]
        .char_indices()
        .nth(len)
        .map_or(text.len(), |(i, _)| start + i);
    format!(
        "{}{}{}",
        &text[..start],
        text[start..end].replace(from, to),
        &text[end..]
    )
}

/// `file` with `from` replaced by `to` only inside the window at `anchor`.
#[macro_export]
macro_rules! edit_window {
    ($file:expr, $anchor:expr, $len:expr, $from:expr, $to:expr) => {
        $crate::common::Edit {
            file: $file,
            apply: |t| $crate::common::replace_in_window(t, $anchor, $len, $from, $to),
        }
    };
}

/// Whether the `n` characters starting at `marker` are identical in `a` and
/// `b` (the "mirrored copies" contract).
pub fn mirrored(p: &Plugin, a: &str, b: &str, marker: &str, n: usize) -> Verdict {
    let (ta, tb) = (p.read(a)?, p.read(b)?);
    ensure!(
        ta.contains(marker) && tb.contains(marker),
        "marker {marker:?} missing from {} or {}",
        name(a),
        name(b)
    );
    ensure!(
        window(&ta, marker, n)? == window(&tb, marker, n)?,
        "the block at {marker:?} has drifted between {a} and {b}"
    );
    Ok(())
}
