//! The generated staging manifest, `plugins/<plugin>/bin/manifest.json`,
//! and the offline verification of a staged `bin/` directory against it.
//!
//! The manifest records provenance (source commit, whether the tree was
//! dirty, the compiler) and, per staged file, its platform, Rust target,
//! package, bin, features, size and sha256. Every file under `bin/` must be
//! listed: an unlisted file is a hand-placed artifact and fails verification.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use toolkit_core::{parse_json, sha256_hex, Json};

use crate::declaration::{object, reject_unknown, required_str, Declaration};
use crate::launcher::{dispatcher_script, display};
use crate::platform::Platform;
use crate::{BIN_DIR, MANIFEST_FILE};

/// One staged binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedBinary {
    /// Declared binary name.
    pub name: String,
    /// Platform it was built for.
    pub platform: Platform,
    /// Rust target triple used.
    pub triple: String,
    /// Workspace package.
    pub package: String,
    /// Cargo bin target.
    pub bin: String,
    /// Cargo features enabled.
    pub features: Vec<String>,
    /// `cargo` or `cargo-zigbuild`.
    pub builder: String,
    /// Path relative to `bin/`.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Lower-case hex sha256.
    pub sha256: String,
}

/// One generated dispatcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedDispatcher {
    /// Binary name (the dispatcher path is `bin/<name>`).
    pub name: String,
    /// Lower-case hex sha256 of the script.
    pub sha256: String,
}

/// The staging manifest of one plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagingManifest {
    /// Plugin directory name.
    pub plugin: String,
    /// 40-hex commit the binaries were built from.
    pub source_commit: String,
    /// Whether the checkout differed from that commit at build time (a
    /// tracked change or an untracked, non-ignored file).
    pub source_dirty: bool,
    /// `rustc -V` of the toolchain used.
    pub rustc: String,
    /// Staged binaries, sorted by name then platform.
    pub binaries: Vec<StagedBinary>,
    /// Dispatchers, one per staged name, sorted.
    pub dispatchers: Vec<StagedDispatcher>,
}

impl StagingManifest {
    /// Serializes the manifest as pretty JSON with a trailing newline.
    #[must_use]
    pub fn render(&self) -> String {
        let binaries = self
            .binaries
            .iter()
            .map(|binary| {
                Json::object(vec![
                    ("name", Json::string(&binary.name)),
                    ("platform", Json::string(binary.platform.key())),
                    ("triple", Json::string(&binary.triple)),
                    ("package", Json::string(&binary.package)),
                    ("bin", Json::string(&binary.bin)),
                    (
                        "features",
                        Json::Array(binary.features.iter().map(Json::string).collect()),
                    ),
                    ("builder", Json::string(&binary.builder)),
                    ("path", Json::string(&binary.path)),
                    (
                        "size",
                        Json::Int(i64::try_from(binary.size).unwrap_or(i64::MAX)),
                    ),
                    ("sha256", Json::string(&binary.sha256)),
                ])
            })
            .collect();
        let dispatchers = self
            .dispatchers
            .iter()
            .map(|dispatcher| {
                Json::object(vec![
                    ("name", Json::string(&dispatcher.name)),
                    ("path", Json::string(&dispatcher.name)),
                    ("sha256", Json::string(&dispatcher.sha256)),
                ])
            })
            .collect();
        let root = Json::object(vec![
            ("schema", Json::Int(1)),
            (
                "generated_by",
                Json::string("plugin-stage (#4487); generated, do not edit"),
            ),
            ("plugin", Json::string(&self.plugin)),
            ("source_commit", Json::string(&self.source_commit)),
            ("source_dirty", Json::Bool(self.source_dirty)),
            ("rustc", Json::string(&self.rustc)),
            ("binaries", Json::Array(binaries)),
            ("dispatchers", Json::Array(dispatchers)),
        ]);
        let mut text = root.to_pretty_string();
        text.push('\n');
        text
    }

    /// Parses a staging manifest.
    ///
    /// # Errors
    ///
    /// Returns the first structural problem.
    pub fn parse(text: &str) -> Result<Self, String> {
        let root = parse_json(text).map_err(|err| format!("invalid JSON: {err}"))?;
        let pairs = object(&root, "manifest")?;
        reject_unknown(
            pairs,
            &[
                "schema",
                "generated_by",
                "plugin",
                "source_commit",
                "source_dirty",
                "rustc",
                "binaries",
                "dispatchers",
            ],
            "manifest",
        )?;
        if root.get("schema") != Some(&Json::Int(1)) {
            return Err("manifest: `schema` must be 1".to_string());
        }
        let source_commit = required_str(&root, "source_commit", "manifest")?;
        if source_commit.len() != 40
            || !source_commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("manifest: `source_commit` must be a 40-hex commit id".to_string());
        }
        let Some(Json::Bool(source_dirty)) = root.get("source_dirty") else {
            return Err("manifest: `source_dirty` must be a boolean".to_string());
        };
        let binaries = match root.get("binaries") {
            Some(Json::Array(items)) => items
                .iter()
                .map(parse_binary)
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err("manifest: `binaries` must be an array".to_string()),
        };
        let dispatchers = match root.get("dispatchers") {
            Some(Json::Array(items)) => items
                .iter()
                .map(parse_dispatcher)
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err("manifest: `dispatchers` must be an array".to_string()),
        };
        Ok(Self {
            plugin: required_str(&root, "plugin", "manifest")?.to_string(),
            source_commit: source_commit.to_string(),
            source_dirty: *source_dirty,
            rustc: required_str(&root, "rustc", "manifest")?.to_string(),
            binaries,
            dispatchers,
        })
    }
}

fn parse_binary(value: &Json) -> Result<StagedBinary, String> {
    let pairs = object(value, "manifest binary")?;
    reject_unknown(
        pairs,
        &[
            "name", "platform", "triple", "package", "bin", "features", "builder", "path", "size",
            "sha256",
        ],
        "manifest binary",
    )?;
    let text = |key| required_str(value, key, "manifest binary").map(str::to_string);
    let key = text("platform")?;
    let platform = Platform::from_key(&key)
        .ok_or_else(|| format!("manifest binary: unknown platform {key:?}"))?;
    let features = match value.get("features") {
        Some(Json::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "manifest binary: features must be strings".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("manifest binary: `features` must be an array".to_string()),
    };
    let size = match value.get("size") {
        Some(Json::Int(size)) => {
            u64::try_from(*size).map_err(|_| "manifest binary: negative size".to_string())?
        }
        _ => return Err("manifest binary: `size` must be an integer".to_string()),
    };
    Ok(StagedBinary {
        name: text("name")?,
        platform,
        triple: text("triple")?,
        package: text("package")?,
        bin: text("bin")?,
        features,
        builder: text("builder")?,
        path: text("path")?,
        size,
        sha256: text("sha256")?,
    })
}

fn parse_dispatcher(value: &Json) -> Result<StagedDispatcher, String> {
    let pairs = object(value, "manifest dispatcher")?;
    reject_unknown(pairs, &["name", "path", "sha256"], "manifest dispatcher")?;
    let name = required_str(value, "name", "manifest dispatcher")?;
    if required_str(value, "path", "manifest dispatcher")? != name {
        return Err(format!(
            "manifest dispatcher {name}: path must equal the name"
        ));
    }
    Ok(StagedDispatcher {
        name: name.to_string(),
        sha256: required_str(value, "sha256", "manifest dispatcher")?.to_string(),
    })
}

/// Verifies every staged plugin `bin/` directory under `plugins_root`.
///
/// Always: each `bin/` directory belongs to a plugin that declares binaries,
/// has a manifest, lists only declared binaries and platforms at their
/// layout paths, and every listed file exists, is a regular executable file
/// and matches its size and sha256; every dispatcher is the generated script;
/// no unlisted file exists. With `complete` (the release gate), every
/// declared binary is staged for every declared platform from a clean tree.
/// Returns the problems found; empty means the staged tree is consistent.
#[must_use]
pub fn verify_staged(
    repo_root: &Path,
    plugins_root: &Path,
    declaration: &Declaration,
    complete: bool,
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut plugins: BTreeSet<String> = declaration
        .plugins()
        .into_iter()
        .map(str::to_string)
        .collect();
    if let Ok(entries) = fs::read_dir(plugins_root) {
        for entry in entries.filter_map(Result::ok) {
            // symlink_metadata: a dangling or symlinked bin/ is still found.
            if fs::symlink_metadata(entry.path().join(BIN_DIR)).is_ok() {
                plugins.insert(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    for plugin in plugins {
        let plugin_dir = plugins_root.join(&plugin);
        let bin_dir = plugin_dir.join(BIN_DIR);
        let shown = display(repo_root, &bin_dir);
        match staged_bin_dir(repo_root, &plugin_dir) {
            Ok(true) => {}
            Ok(false) => {
                if complete {
                    problems.push(format!("{shown}: not staged"));
                }
                continue;
            }
            Err(problem) => {
                problems.push(problem);
                continue;
            }
        }
        if declaration.plugin_binaries(&plugin).next().is_none() {
            problems.push(format!(
                "{shown}: plugin {plugin} declares no binaries in {}, so it may not have a bin/ directory",
                crate::DECLARATION_PATH
            ));
            continue;
        }
        let manifest_path = bin_dir.join(MANIFEST_FILE);
        let parsed = read_regular(&manifest_path)
            .map_err(|err| {
                if err == MISSING {
                    format!(
                        "{shown}: has no {MANIFEST_FILE}; staged files must come from plugin-stage"
                    )
                } else {
                    format!("{shown}/{MANIFEST_FILE}: {err}")
                }
            })
            .and_then(|bytes| {
                let text = String::from_utf8(bytes)
                    .map_err(|_| format!("{shown}/{MANIFEST_FILE}: is not UTF-8"))?;
                StagingManifest::parse(&text)
                    .map_err(|err| format!("{shown}/{MANIFEST_FILE}: {err}"))
            });
        let manifest = match parsed {
            Ok(manifest) => manifest,
            Err(problem) => {
                problems.push(problem);
                continue;
            }
        };
        let checker = PluginCheck {
            plugin: &plugin,
            bin_dir: &bin_dir,
            shown: &shown,
            declaration,
        };
        checker.run(&manifest, complete, &mut problems);
    }
    problems
}

struct PluginCheck<'a> {
    plugin: &'a str,
    bin_dir: &'a Path,
    shown: &'a str,
    declaration: &'a Declaration,
}

impl PluginCheck<'_> {
    fn run(&self, manifest: &StagingManifest, complete: bool, problems: &mut Vec<String>) {
        let shown = self.shown;
        if manifest.plugin != self.plugin {
            problems.push(format!(
                "{shown}/{MANIFEST_FILE}: names plugin {:?}, expected {:?}",
                manifest.plugin, self.plugin
            ));
        }
        let mut listed: BTreeSet<String> = BTreeSet::from([MANIFEST_FILE.to_string()]);
        let mut staged = BTreeSet::new();
        for binary in &manifest.binaries {
            let label = format!("{shown}/{}", binary.path);
            if !staged.insert((binary.name.clone(), binary.platform)) {
                problems.push(format!("{label}: listed twice"));
            }
            listed.insert(binary.path.clone());
            self.check_binary(binary, &label, problems);
        }
        let names: BTreeSet<&str> = manifest.binaries.iter().map(|b| b.name.as_str()).collect();
        let dispatched: BTreeSet<&str> = manifest
            .dispatchers
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        if names != dispatched {
            problems.push(format!(
                "{shown}/{MANIFEST_FILE}: dispatchers {dispatched:?} do not match staged binaries {names:?}"
            ));
        }
        for dispatcher in &manifest.dispatchers {
            listed.insert(dispatcher.name.clone());
            let expected = dispatcher_script(&dispatcher.name);
            let label = format!("{shown}/{}", dispatcher.name);
            if dispatcher.sha256 != sha256_hex(expected.as_bytes()) {
                problems.push(format!(
                    "{label}: manifest sha256 is not the generated dispatcher's"
                ));
            }
            match read_regular(&self.bin_dir.join(&dispatcher.name)) {
                Ok(bytes) if bytes == expected.as_bytes() => {
                    executable(&self.bin_dir.join(&dispatcher.name), &label, problems);
                }
                Ok(_) => problems.push(format!("{label}: differs from the generated dispatcher")),
                Err(err) => problems.push(format!("{label}: {err}")),
            }
        }
        let mut found = Vec::new();
        walk_files(self.bin_dir, "", &mut found, problems, shown);
        for file in found {
            if !listed.contains(&file) {
                problems.push(format!(
                    "{shown}/{file}: not listed in {MANIFEST_FILE} (staged files are generated, never hand-placed)"
                ));
            }
        }
        if complete {
            if manifest.source_dirty {
                problems.push(format!(
                    "{shown}/{MANIFEST_FILE}: staged from a dirty tree; a release needs a clean commit"
                ));
            }
            for binary in self.declaration.plugin_binaries(self.plugin) {
                for spec in &self.declaration.platforms {
                    if !staged.contains(&(binary.name.clone(), spec.platform)) {
                        problems.push(format!(
                            "{shown}: {} is not staged for {}",
                            binary.name, spec.platform
                        ));
                    }
                }
            }
        }
    }

    fn check_binary(
        &self,
        binary: &crate::manifest::StagedBinary,
        label: &str,
        problems: &mut Vec<String>,
    ) {
        let Some(spec) = self.declaration.binary(self.plugin, &binary.name) else {
            problems.push(format!(
                "{label}: binary {} is not declared for plugin {}",
                binary.name, self.plugin
            ));
            return;
        };
        let Some(platform_spec) = self.declaration.platform(binary.platform) else {
            problems.push(format!(
                "{label}: platform {} is not declared",
                binary.platform
            ));
            return;
        };
        if binary.path != binary.platform.staged_path(&binary.name) {
            problems.push(format!(
                "{label}: expected layout path {}",
                binary.platform.staged_path(&binary.name)
            ));
            return;
        }
        if binary.triple != platform_spec.triple
            || binary.package != spec.package
            || binary.bin != spec.bin
            || binary.features != spec.features
        {
            problems.push(format!(
                "{label}: built from {}/{} {:?} for {}, but the declaration says {}/{} {:?} for {}",
                binary.package,
                binary.bin,
                binary.features,
                binary.triple,
                spec.package,
                spec.bin,
                spec.features,
                platform_spec.triple
            ));
        }
        let path = self.bin_dir.join(&binary.path);
        match read_staged(self.bin_dir, &binary.path) {
            Ok(bytes) => {
                if u64::try_from(bytes.len()).ok() != Some(binary.size) {
                    problems.push(format!("{label}: size differs from the manifest"));
                }
                if sha256_hex(&bytes) != binary.sha256 {
                    problems.push(format!("{label}: sha256 differs from the manifest"));
                }
                executable(&path, label, problems);
            }
            Err(err) => problems.push(format!("{label}: {err}")),
        }
    }
}

const MISSING: &str = "is missing";

/// Whether `plugin_dir/bin` is staged. A symlinked (or non-directory)
/// plugin directory or `bin/` is refused before anything is read through it
/// (#5023): the release gate certifies a self-contained package, and a
/// symlink force-added to the release tree would not ship the verified bytes.
///
/// # Errors
///
/// Names the symlinked or non-directory path.
pub fn staged_bin_dir(repo_root: &Path, plugin_dir: &Path) -> Result<bool, String> {
    let real_dir = |path: &Path, what: &str| {
        match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "{}: is a symlink; {what} must be a real directory so the staged package is self-contained",
            display(repo_root, path)
        )),
        Ok(meta) if meta.is_dir() => Ok(true),
        Ok(_) => Err(format!("{}: is not a directory", display(repo_root, path))),
        Err(_) => Ok(false),
    }
    };
    if !real_dir(plugin_dir, "a plugin with staged binaries")? {
        return Ok(false);
    }
    real_dir(&plugin_dir.join(BIN_DIR), "bin/")
}

/// Reads a regular file without following a symlink at `path`.
pub(crate) fn read_regular(path: &Path) -> Result<Vec<u8>, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err("is a symlink; staged files must be regular files inside bin/".to_string())
        }
        Ok(meta) if meta.file_type().is_file() => {
            fs::read(path).map_err(|err| format!("cannot read: {err}"))
        }
        Ok(_) => Err("is not a regular file".to_string()),
        Err(_) => Err(MISSING.to_string()),
    }
}

/// Reads `bin_dir/rel` after checking that its platform directory is a real
/// directory, not a symlink out of the package.
fn read_staged(bin_dir: &Path, rel: &str) -> Result<Vec<u8>, String> {
    if let Some(parent) = Path::new(rel)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        match fs::symlink_metadata(bin_dir.join(parent)) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "its directory {} is a symlink; platform directories must be real directories inside bin/",
                    parent.display()
                ))
            }
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(format!("{} is not a directory", parent.display())),
            Err(_) => return Err(MISSING.to_string()),
        }
    }
    read_regular(&bin_dir.join(rel))
}

#[cfg(unix)]
fn executable(path: &Path, label: &str, problems: &mut Vec<String>) {
    use std::os::unix::fs::PermissionsExt;
    match fs::metadata(path) {
        Ok(meta) if meta.permissions().mode() & 0o111 == 0o111 => {}
        Ok(_) => problems.push(format!("{label}: is not executable (expected mode 0755)")),
        Err(err) => problems.push(format!("{label}: {err}")),
    }
}

#[cfg(not(unix))]
fn executable(_path: &Path, _label: &str, _problems: &mut Vec<String>) {}

fn walk_files(
    dir: &Path,
    prefix: &str,
    out: &mut Vec<String>,
    problems: &mut Vec<String>,
    shown: &str,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        problems.push(format!("{shown}/{prefix}: cannot be listed"));
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        match entry.file_type() {
            Ok(kind) if kind.is_symlink() => problems.push(format!(
                "{shown}/{rel}: is a symlink; staged files and platform directories must be real entries inside bin/"
            )),
            Ok(kind) if kind.is_dir() => {
                if prefix.is_empty() && Platform::from_key(&name).is_some() {
                    walk_files(&entry.path(), &rel, out, problems, shown);
                } else {
                    problems.push(format!("{shown}/{rel}: unexpected directory"));
                }
            }
            _ => out.push(rel),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StagingManifest {
        StagingManifest {
            plugin: "aws-core".to_string(),
            source_commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
            source_dirty: false,
            rustc: "rustc 1.94.1".to_string(),
            binaries: vec![StagedBinary {
                name: "tool".to_string(),
                platform: Platform::LinuxX86_64,
                triple: "x86_64-unknown-linux-gnu".to_string(),
                package: "tool-pkg".to_string(),
                bin: "tool".to_string(),
                features: vec!["a/b".to_string()],
                builder: "cargo".to_string(),
                path: "linux-x86_64/tool".to_string(),
                size: 3,
                sha256: sha256_hex(b"abc"),
            }],
            dispatchers: vec![StagedDispatcher {
                name: "tool".to_string(),
                sha256: sha256_hex(dispatcher_script("tool").as_bytes()),
            }],
        }
    }

    #[test]
    fn manifests_round_trip() {
        let manifest = sample();
        let text = manifest.render();
        assert!(text.ends_with("}\n"));
        assert_eq!(StagingManifest::parse(&text).expect("parse"), manifest);
    }

    #[test]
    fn malformed_manifests_are_refused() {
        let text = sample().render();
        for (from, to, needle) in [
            ("\"schema\": 1", "\"schema\": 9", "schema"),
            ("0123456789abcdef0123456789abcdef01234567", "HEAD", "40-hex"),
            (
                "\"source_dirty\": false",
                "\"source_dirty\": \"no\"",
                "boolean",
            ),
            (
                "\"platform\": \"linux-x86_64\"",
                "\"platform\": \"plan9-x86_64\"",
                "unknown platform",
            ),
            ("\"size\": 3", "\"size\": -3", "negative"),
            ("\"rustc\"", "\"compiler\"", "unknown field"),
        ] {
            assert!(text.contains(from), "{from}");
            let error = StagingManifest::parse(&text.replacen(from, to, 1)).expect_err(to);
            assert!(error.contains(needle), "{to}: {error}");
        }
    }
}
