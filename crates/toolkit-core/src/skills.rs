//! Skill-directory discovery for `toolkit skills list`.
//!
//! A *skill* is a directory that directly contains a `SKILL.md` file. Discovery
//! walks the supplied root recursively, skips hidden directories, `node_modules`
//! and `target`, never follows static symlinks (directories or `SKILL.md`,
//! to anywhere including outside ROOT: they are skipped, not read) and
//! reports every result path relative to the supplied root. The read
//! re-checks the final file's identity after opening (see
//! [`crate::paths::read_regular_file`]).
//!
//! The walk is pathname-based and does not defend against concurrent mutation
//! of the tree during a run: an ancestor directory swapped for a symlink
//! between being listed and being opened is not detected (see the threat
//! model in [`crate::paths`]). Discovery is a single entry point
//! ([`discover_skills`]) so a handle-anchored walker can replace this one
//! later without an API change.
//!
//! Decisions recorded for the carry-forward review notes of #4426:
//!
//! - ROOT itself is **not** eligible: only directories *under* ROOT are
//!   candidates, so `ROOT/SKILL.md` is ignored. Point ROOT at the parent to
//!   list a single skill.
//! - A frontmatter `name` that is empty or only whitespace is treated as
//!   absent and falls back to the directory name; an empty `description`
//!   stays empty.
//! - Plain-text lines replace every control character (and U+2028/U+2029) in
//!   `name` and `description` with one space so one skill is always one line
//!   with exactly one tab; JSON output keeps the exact text, escaped.
//! - `SKILL.md` larger than [`MAX_SKILL_FILE_BYTES`] or not valid UTF-8 is a
//!   runtime error, as an unreadable file was before.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::diag::{is_unsafe_char, neutralize_line};
use crate::frontmatter::parse_frontmatter;
use crate::json::Json;
use crate::paths::{read_regular_file, PathError};
use crate::schema::{ObjectSchema, Schema};

/// Largest `SKILL.md` that discovery will read (1 MiB).
pub const MAX_SKILL_FILE_BYTES: u64 = 1024 * 1024;

/// One discovered skill, ready for human or JSON output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEntry {
    /// Frontmatter `name`, falling back to the directory name.
    pub name: String,
    /// Frontmatter `description`, empty when absent.
    pub description: String,
    /// Path of the skill directory relative to the enumeration root, using
    /// `/` separators.
    pub path: String,
}

impl SkillEntry {
    /// JSON object with exactly the string members `name`, `description` and
    /// `path`, in that order.
    #[must_use]
    pub fn to_json(&self) -> Json {
        Json::object(vec![
            ("name", Json::string(self.name.as_str())),
            ("description", Json::string(self.description.as_str())),
            ("path", Json::string(self.path.as_str())),
        ])
    }

    /// The plain-text listing line: `<name><TAB><description>` with control
    /// characters flattened to spaces and bidirectional or invisible formatting
    /// characters written as `\uXXXX` escapes (#4639).
    #[must_use]
    pub fn plain_line(&self) -> String {
        format!(
            "{}\t{}",
            flatten_controls(&self.name),
            flatten_controls(&self.description)
        )
    }
}

/// Control characters and line/paragraph separators become spaces; the other
/// characters that can reorder or hide text (bidirectional marks and overrides
/// such as U+202E, zero-width characters, the byte-order mark; see
/// [`is_unsafe_char`]) are written as `\uXXXX` so they stay visible and cannot
/// reorder the listing (#4639).
fn flatten_controls(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if c.is_control() || c == '\u{2028}' || c == '\u{2029}' {
            out.push(' ');
        } else if is_unsafe_char(c) {
            let mut buf = [0u8; 4];
            out.push_str(&neutralize_line(c.encode_utf8(&mut buf)));
        } else {
            out.push(c);
        }
    }
    out
}

/// Schema of the `--json` output of `toolkit skills list`.
#[must_use]
pub fn skills_output_schema() -> Schema {
    Schema::Array(Box::new(
        ObjectSchema::new()
            .required("name", Schema::String)
            .required("description", Schema::String)
            .required("path", Schema::String)
            .into(),
    ))
}

/// Errors returned by discovery.
#[derive(Debug)]
pub enum SkillsError {
    /// The supplied root does not exist.
    RootNotFound(PathBuf),
    /// The supplied root exists but is not a directory.
    RootNotDirectory(PathBuf),
    /// Reading the root or a skill directory failed.
    Io(std::io::Error, PathBuf),
    /// A `SKILL.md` could not be read (too large, not UTF-8, I/O failure or
    /// replaced while being opened).
    SkillFile(PathError),
}

impl fmt::Display for SkillsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootNotFound(path) => {
                write!(f, "root does not exist: {}", path.display())
            }
            Self::RootNotDirectory(path) => {
                write!(f, "root is not a directory: {}", path.display())
            }
            Self::Io(error, path) => write!(f, "cannot read {}: {error}", path.display()),
            Self::SkillFile(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SkillsError {}

/// Directories that are never treated as skill containers or descended into.
fn is_skipped(name: &str) -> bool {
    name.starts_with('.') || name == "node_modules" || name == "target"
}

/// Enumerates skills under `root`, sorted by frontmatter name (directory name
/// when the frontmatter has none), then by relative path. Reported paths are
/// relative to `root`. Symlinks present in the tree are refused at any
/// component: a symlinked directory is not followed and a symlinked `SKILL.md`
/// is skipped instead of read.
///
/// This holds for a tree that is not being modified during the call. The walk
/// does not defend against concurrent mutation (for example a listed directory
/// swapped for a symlink to an outside directory mid-run); see the threat
/// model in [`crate::paths`].
///
/// # Errors
///
/// Returns [`SkillsError::RootNotFound`] when `root` does not exist,
/// [`SkillsError::RootNotDirectory`] when it is not a directory,
/// [`SkillsError::Io`] when the root or a skill directory cannot be read and
/// [`SkillsError::SkillFile`] when a `SKILL.md` cannot be read.
pub fn discover_skills(root: &Path) -> Result<Vec<SkillEntry>, SkillsError> {
    let root = root.to_path_buf();
    let metadata = match fs::metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SkillsError::RootNotFound(root))
        }
        Err(error) => return Err(SkillsError::Io(error, root)),
    };
    if !metadata.is_dir() {
        return Err(SkillsError::RootNotDirectory(root));
    }

    let mut entries = Vec::new();
    // Explicit stack instead of recursion; segments hold `/`-joined relative
    // paths so output is independent of the host path separator.
    let mut pending: Vec<(PathBuf, String)> = vec![(root.clone(), String::new())];

    while let Some((dir, relative)) = pending.pop() {
        let read = fs::read_dir(&dir).map_err(|error| SkillsError::Io(error, dir.clone()))?;
        let mut children: Vec<(PathBuf, String)> = Vec::new();
        for child in read {
            let child = child.map_err(|error| SkillsError::Io(error, dir.clone()))?;
            // file_type does not follow symlinks, so linked directories are
            // skipped rather than traversed.
            let Ok(file_type) = child.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let name = child.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if is_skipped(name) {
                continue;
            }
            let child_relative = if relative.is_empty() {
                name.to_string()
            } else {
                format!("{relative}/{name}")
            };
            children.push((dir.join(name), child_relative));
        }

        // Deterministic traversal order keeps directory-read nondeterminism
        // out of the stack; the final sort below is the contract.
        children.sort();
        for (child_dir, child_relative) in children {
            let skill_file = child_dir.join("SKILL.md");
            // symlink_metadata does not follow symlinks, so a SKILL.md that is
            // itself a symlink (to anywhere, including outside ROOT) is skipped
            // rather than read; only real regular files pass.
            match fs::symlink_metadata(&skill_file) {
                Ok(metadata) if metadata.is_file() => {
                    if let Some(entry) = read_entry(&child_dir, &skill_file, &child_relative)? {
                        entries.push(entry);
                    }
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(SkillsError::Io(error, skill_file)),
            }
            pending.push((child_dir, child_relative));
        }
    }

    entries.sort_by(|a, b| (&a.name, &a.path).cmp(&(&b.name, &b.path)));
    Ok(entries)
}

/// Reads one skill. `Ok(None)` means the file stopped being a regular file
/// between the directory scan and the read (replaced by a link, a directory or
/// removed): it is skipped exactly like a symlinked `SKILL.md`.
fn read_entry(
    dir: &Path,
    skill_file: &Path,
    relative: &str,
) -> Result<Option<SkillEntry>, SkillsError> {
    let content = match read_regular_file(skill_file, MAX_SKILL_FILE_BYTES) {
        Ok(content) => content,
        Err(PathError::Symlink(_) | PathError::NotRegularFile(_) | PathError::NotFound(_)) => {
            return Ok(None)
        }
        Err(error) => return Err(SkillsError::SkillFile(error)),
    };
    let frontmatter = parse_frontmatter(&content);
    let dir_name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    let name = frontmatter
        .name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(dir_name);
    Ok(Some(SkillEntry {
        name,
        description: frontmatter.description.unwrap_or_default(),
        path: relative.to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse_json;
    use crate::schema::validate;
    use crate::testutil::TempDir;
    use std::fmt::Write as _;

    fn skill(dir: &Path, relative: &str, name: Option<&str>, description: Option<&str>) {
        let full = dir.join(relative);
        fs::create_dir_all(&full).expect("create skill dir");
        let mut content = String::from("---\n");
        if let Some(name) = name {
            let _ = writeln!(content, "name: {name}");
        }
        if let Some(description) = description {
            let _ = writeln!(content, "description: >\n  {description}");
        }
        content.push_str("---\n");
        fs::write(full.join("SKILL.md"), content).expect("write SKILL.md");
    }

    fn raw_skill(dir: &Path, relative: &str, content: &[u8]) {
        let full = dir.join(relative);
        fs::create_dir_all(&full).expect("create skill dir");
        fs::write(full.join("SKILL.md"), content).expect("write SKILL.md");
    }

    fn names(entries: &[SkillEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    #[test]
    fn enumerates_nested_skills_sorted_by_name() {
        let root = TempDir::new("nested");
        skill(&root, "core-skills/zulu", None, Some("z"));
        skill(
            &root,
            "specialized-skills/storage-skills/alpha",
            Some("alpha"),
            None,
        );
        skill(&root, "core-skills/mid", None, None);

        let entries = discover_skills(&root).expect("discover");
        assert_eq!(names(&entries), vec!["alpha", "mid", "zulu"]);
        assert_eq!(entries[0].path, "specialized-skills/storage-skills/alpha");
        assert_eq!(entries[1].path, "core-skills/mid");
        assert_eq!(entries[2].description, "z");
    }

    #[test]
    fn name_tiebreak_falls_back_to_relative_path() {
        let root = TempDir::new("tiebreak");
        skill(&root, "b/same", None, None);
        skill(&root, "a/same", None, None);

        let entries = discover_skills(&root).expect("discover");
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(paths, vec!["a/same", "b/same"]);
    }

    #[test]
    fn output_is_deterministic_across_runs() {
        let root = TempDir::new("deterministic");
        for index in 0..20 {
            skill(
                &root,
                &format!("group-{}/s{index:02}", index % 3),
                None,
                None,
            );
        }
        let first = discover_skills(&root).expect("first");
        let second = discover_skills(&root).expect("second");
        assert_eq!(first, second);
        let mut sorted = first.clone();
        sorted.sort_by(|a, b| (&a.name, &a.path).cmp(&(&b.name, &b.path)));
        assert_eq!(first, sorted);
    }

    #[test]
    fn hidden_and_vendored_directories_are_skipped() {
        let root = TempDir::new("hidden");
        skill(&root, "visible", None, None);
        skill(&root, ".hidden", None, None);
        skill(&root, "node_modules/vendor-skill", None, None);
        skill(&root, "target/debug-skill", None, None);

        let entries = discover_skills(&root).expect("discover");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "visible");
    }

    #[test]
    fn directory_without_skill_md_is_not_a_skill_but_is_descended() {
        let root = TempDir::new("descend");
        skill(&root, "group/nested", None, None);

        let entries = discover_skills(&root).expect("discover");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "group/nested");
        assert_eq!(entries[0].name, "nested");
    }

    #[test]
    fn root_itself_is_not_eligible_but_nested_skills_are() {
        // Decision: only directories under ROOT are candidates.
        let root = TempDir::new("root-self");
        raw_skill(&root, ".", b"---\nname: root-skill\n---\n");
        assert!(root.join("SKILL.md").is_file());
        skill(&root, "child", Some("child"), None);

        let entries = discover_skills(&root).expect("discover");
        assert_eq!(names(&entries), vec!["child"]);

        let only_root = TempDir::new("root-only");
        raw_skill(&only_root, ".", b"---\nname: root-skill\n---\n");
        assert!(discover_skills(&only_root).expect("discover").is_empty());
    }

    #[test]
    fn empty_or_blank_frontmatter_name_falls_back_to_the_directory_name() {
        let root = TempDir::new("empty-name");
        raw_skill(&root, "bare", b"---\nname:\ndescription: d\n---\n");
        raw_skill(&root, "quoted", b"---\nname: \"\"\n---\n");
        raw_skill(&root, "blank", b"---\nname: \"   \"\n---\n");
        raw_skill(&root, "absent", b"---\ndescription: only\n---\n");
        raw_skill(&root, "present", b"---\nname: custom\n---\n");

        let entries = discover_skills(&root).expect("discover");
        assert_eq!(
            names(&entries),
            vec!["absent", "bare", "blank", "custom", "quoted"]
        );
        let bare = entries
            .iter()
            .find(|entry| entry.path == "bare")
            .expect("bare");
        assert_eq!(bare.description, "d");
    }

    #[test]
    fn bom_prefixed_skill_md_is_read() {
        let root = TempDir::new("bom");
        raw_skill(
            &root,
            "bom-dir",
            b"\xef\xbb\xbf---\nname: from-bom\ndescription: read\n---\n",
        );
        let entries = discover_skills(&root).expect("discover");
        assert_eq!(names(&entries), vec!["from-bom"]);
        assert_eq!(entries[0].description, "read");
    }

    #[test]
    fn plain_line_flattens_controls_and_json_keeps_them() {
        let root = TempDir::new("multiline");
        raw_skill(
            &root,
            "multi",
            b"---\nname: |\n  first\n  second\ndescription: |\n  one\n  two\n---\n",
        );
        let entries = discover_skills(&root).expect("discover");
        assert_eq!(entries[0].name, "first\nsecond");
        assert_eq!(entries[0].plain_line(), "first second\tone two");
        assert_eq!(entries[0].plain_line().matches('\t').count(), 1);
        assert!(!entries[0].plain_line().contains('\n'));

        let tabbed = SkillEntry {
            name: "a\tb\r\u{2028}c".to_string(),
            description: "x\u{7f}y".to_string(),
            path: "p".to_string(),
        };
        assert_eq!(tabbed.plain_line(), "a b  c\tx y");
        assert!(tabbed.to_json().to_compact_string().contains("a\\tb\\r"));
    }

    #[test]
    fn plain_line_escapes_bidi_and_invisible_characters() {
        let entry = SkillEntry {
            name: "safe\u{202e}gpj.exe".to_string(),
            description: "a\u{200b}b\u{feff}c\u{2066}d".to_string(),
            path: "p".to_string(),
        };
        let line = entry.plain_line();
        assert_eq!(line, "safe\\u202Egpj.exe\ta\\u200Bb\\uFEFFc\\u2066d");
        assert!(!line.chars().any(|c| c != '\t' && is_unsafe_char(c)));
        // JSON output keeps the metadata unchanged.
        assert!(entry.to_json().to_compact_string().contains('\u{202e}'));
    }

    #[test]
    fn json_output_validates_against_the_published_schema() {
        let root = TempDir::new("schema");
        skill(&root, "one", Some("one"), Some("first"));
        skill(&root, "two", None, None);
        let entries = discover_skills(&root).expect("discover");
        let text = crate::json::skills_to_json(&entries);
        let parsed = parse_json(&text).expect("output is valid json");
        validate(&skills_output_schema(), &parsed).expect("output matches schema");

        // Negative: a document with a wrong type, a missing and an extra member.
        let broken = parse_json("[{\"name\":1,\"path\":\"p\",\"extra\":true}]").expect("json");
        let violations = validate(&skills_output_schema(), &broken).expect_err("must fail");
        let paths: Vec<&str> = violations.iter().map(|v| v.path.as_str()).collect();
        assert_eq!(paths, vec!["$[0].name", "$[0].description", "$[0].extra"]);
    }

    #[test]
    fn missing_root_is_reported() {
        let parent = TempDir::new("missing");
        let root = parent.join("does-not-exist");
        let error = discover_skills(&root).expect_err("must fail");
        assert!(matches!(error, SkillsError::RootNotFound(_)));
        assert!(error.to_string().contains("root does not exist"));
    }

    #[test]
    fn file_root_is_reported() {
        let root = TempDir::new("file");
        let file = root.join("not-a-dir");
        fs::write(&file, "x").expect("write file");
        let error = discover_skills(&file).expect_err("must fail");
        assert!(matches!(error, SkillsError::RootNotDirectory(_)));
        assert!(error.to_string().contains("root is not a directory"));
    }

    #[test]
    fn unreadable_skill_files_fail_discovery() {
        let root = TempDir::new("unreadable");
        raw_skill(&root, "binary", b"\xff\xfe\x00 not utf-8");
        let error = discover_skills(&root).expect_err("must fail");
        assert!(matches!(
            error,
            SkillsError::SkillFile(PathError::NotUtf8(_))
        ));
        assert!(error.to_string().contains("not valid UTF-8"));

        let big = TempDir::new("oversized");
        let mut content = b"---\nname: big\n---\n".to_vec();
        content.resize(
            usize::try_from(MAX_SKILL_FILE_BYTES).expect("fits") + 1,
            b'x',
        );
        raw_skill(&big, "big", &content);
        let error = discover_skills(&big).expect_err("must fail");
        assert!(matches!(
            error,
            SkillsError::SkillFile(PathError::TooLarge { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directories_are_not_followed() {
        let root = TempDir::new("symlink");
        let outside = TempDir::new("symlink-outside");
        skill(&outside, "outside-skill", None, None);
        std::os::unix::fs::symlink(outside.join("outside-skill"), root.join("linked"))
            .expect("create symlink");
        skill(&root, "real", None, None);

        let entries = discover_skills(&root).expect("discover");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "real");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_skill_md_file_is_skipped_not_read() {
        // A SKILL.md that is itself a symlink to a file outside ROOT must not
        // be read; containment covers the metadata file, not only directories.
        let root = TempDir::new("symlink-skill-md");
        let outside = TempDir::new("symlink-skill-md-outside");
        fs::create_dir_all(outside.join("victim")).expect("create outside dir");
        fs::write(
            outside.join("victim").join("secret.md"),
            "---\nname: leaked\ndescription: from outside\n---\n",
        )
        .expect("write outside file");
        skill(&root, "real", Some("real"), None);
        fs::create_dir_all(root.join("leaky")).expect("create skill dir");
        std::os::unix::fs::symlink(
            outside.join("victim").join("secret.md"),
            root.join("leaky").join("SKILL.md"),
        )
        .expect("create SKILL.md symlink");
        // A dangling SKILL.md link is also skipped quietly.
        fs::create_dir_all(root.join("dangling")).expect("create skill dir");
        std::os::unix::fs::symlink(root.join("gone.md"), root.join("dangling").join("SKILL.md"))
            .expect("create dangling symlink");

        let entries = discover_skills(&root).expect("discover");
        assert_eq!(entries.len(), 1, "only the real skill is listed");
        assert_eq!(entries[0].path, "real");
        assert_eq!(entries[0].name, "real");
        assert!(
            !entries.iter().any(|entry| entry.name == "leaked"),
            "symlinked SKILL.md content must not leak"
        );
    }

    #[test]
    fn empty_root_yields_no_entries() {
        let root = TempDir::new("empty");
        let entries = discover_skills(&root).expect("discover");
        assert!(entries.is_empty());
    }
}
