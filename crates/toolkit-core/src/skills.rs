//! Skill-directory discovery for `toolkit skills list`.
//!
//! A *skill* is a directory that directly contains a `SKILL.md` file. Discovery
//! walks the supplied root recursively, skips hidden directories and
//! `node_modules`, never follows symlinks, and keeps every result path inside
//! the supplied root.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::frontmatter::parse_frontmatter;

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

/// Errors returned by discovery.
#[derive(Debug)]
pub enum SkillsError {
    /// The supplied root does not exist.
    RootNotFound(PathBuf),
    /// The supplied root exists but is not a directory.
    RootNotDirectory(PathBuf),
    /// Reading the root or a skill's `SKILL.md` failed.
    Io(std::io::Error, PathBuf),
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
        }
    }
}

impl std::error::Error for SkillsError {}

/// Directories that are never treated as skill containers or descended into.
fn is_skipped(name: &str) -> bool {
    name.starts_with('.') || name == "node_modules" || name == "target"
}

/// Enumerates skills under `root`, sorted by frontmatter name (directory name
/// when the frontmatter has none), then by relative path. Every result stays
/// inside `root`; symlinked directories are never followed.
///
/// # Errors
///
/// Returns [`SkillsError::RootNotFound`] when `root` does not exist,
/// [`SkillsError::RootNotDirectory`] when it is not a directory, and
/// [`SkillsError::Io`] when the root, a skill directory or a `SKILL.md` file
/// cannot be read.
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
            match fs::metadata(&skill_file) {
                Ok(metadata) if metadata.is_file() => {
                    entries.push(read_entry(&child_dir, &skill_file, &child_relative)?);
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

fn read_entry(dir: &Path, skill_file: &Path, relative: &str) -> Result<SkillEntry, SkillsError> {
    let content = fs::read_to_string(skill_file)
        .map_err(|error| SkillsError::Io(error, skill_file.to_path_buf()))?;
    let frontmatter = parse_frontmatter(&content);
    let dir_name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    Ok(SkillEntry {
        name: frontmatter.name.unwrap_or_else(|| dir_name.clone()),
        description: frontmatter.description.unwrap_or_default(),
        path: relative.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fmt::Write as _;

    /// Creates a unique temporary directory; the caller is responsible for
    /// removal. No external test dependency is introduced for this.
    fn temp_dir(label: &str) -> PathBuf {
        let unique = format!(
            "toolkit-core-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
        );
        let path = env::temp_dir().join(unique);
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

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

    fn names(entries: &[SkillEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    #[test]
    fn enumerates_nested_skills_sorted_by_name() {
        let root = temp_dir("nested");
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

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn name_tiebreak_falls_back_to_relative_path() {
        let root = temp_dir("tiebreak");
        skill(&root, "b/same", None, None);
        skill(&root, "a/same", None, None);

        let entries = discover_skills(&root).expect("discover");
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(paths, vec!["a/same", "b/same"]);

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn hidden_and_vendored_directories_are_skipped() {
        let root = temp_dir("hidden");
        skill(&root, "visible", None, None);
        skill(&root, ".hidden", None, None);
        skill(&root, "node_modules/vendor-skill", None, None);
        skill(&root, "target/debug-skill", None, None);

        let entries = discover_skills(&root).expect("discover");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "visible");

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn directory_without_skill_md_is_not_a_skill_but_is_descended() {
        let root = temp_dir("descend");
        skill(&root, "group/nested", None, None);

        let entries = discover_skills(&root).expect("discover");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "group/nested");
        assert_eq!(entries[0].name, "nested");

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn missing_root_is_reported() {
        let root = temp_dir("missing").join("does-not-exist");
        let error = discover_skills(&root).expect_err("must fail");
        assert!(matches!(error, SkillsError::RootNotFound(_)));
        assert!(error.to_string().contains("root does not exist"));
        fs::remove_dir_all(root.parent().expect("temp parent")).expect("cleanup");
    }

    #[test]
    fn file_root_is_reported() {
        let root = temp_dir("file");
        let file = root.join("not-a-dir");
        fs::write(&file, "x").expect("write file");
        let error = discover_skills(&file).expect_err("must fail");
        assert!(matches!(error, SkillsError::RootNotDirectory(_)));
        assert!(error.to_string().contains("root is not a directory"));
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn symlinked_directories_are_not_followed() {
        let root = temp_dir("symlink");
        let outside = temp_dir("symlink-outside");
        skill(&outside, "outside-skill", None, None);
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.join("outside-skill"), root.join("linked"))
            .expect("create symlink");
        skill(&root, "real", None, None);

        let entries = discover_skills(&root).expect("discover");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "real");

        fs::remove_dir_all(&root).expect("cleanup");
        fs::remove_dir_all(&outside).expect("cleanup");
    }

    #[test]
    fn empty_root_yields_no_entries() {
        let root = temp_dir("empty");
        let entries = discover_skills(&root).expect("discover");
        assert!(entries.is_empty());
        fs::remove_dir_all(&root).expect("cleanup");
    }
}
