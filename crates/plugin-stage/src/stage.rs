//! Building the declared bins with locally installed toolchains and
//! installing them into each plugin's `bin/` layout.
//!
//! Planning is pure ([`plan`] over a [`Probe`]) so every refusal (unknown
//! binary, package not in the workspace, Rust target not installed, no cross
//! linker) is decided, and reported together, before anything is built.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use toolkit_core::{parse_json, sha256_hex, Json};

use crate::declaration::{BinarySpec, Declaration, Destination, PlatformSpec};
use crate::feature_graph::Resolver;
use crate::launcher::dispatcher_script;
use crate::manifest::{
    read_regular, staged_bin_dir, StagedBinary, StagedDispatcher, StagingManifest,
};
use crate::platform::Platform;
use crate::{BIN_DIR, MANIFEST_FILE};

/// What the local machine can build, discovered once per run.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    /// `host:` line of `rustc -vV`.
    pub host_triple: String,
    /// `rustc -V`.
    pub rustc_version: String,
    /// Rust targets whose standard library is installed in the sysroot.
    pub installed_targets: BTreeSet<String>,
    /// `zig version`, when `zig` runs.
    pub zig: Option<String>,
    /// Whether `cargo zigbuild` runs.
    pub cargo_zigbuild: bool,
    /// Workspace packages and their bin targets (`cargo metadata --no-deps`).
    pub workspace_bins: BTreeMap<String, BTreeSet<String>>,
    /// Cargo target directory.
    pub target_dir: PathBuf,
}

/// How one step is compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builder {
    /// Plain `cargo build` for the host target.
    Cargo,
    /// `cargo zigbuild`: zig links the cross target (or pins a glibc floor).
    Zigbuild,
}

impl Builder {
    /// Name recorded in the manifest.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Zigbuild => "cargo-zigbuild",
        }
    }
}

/// One binary built for one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildStep {
    /// The declared binary.
    pub binary: BinarySpec,
    /// The declared platform.
    pub platform: PlatformSpec,
    /// How it is compiled.
    pub builder: Builder,
}

impl BuildStep {
    /// The `--target` value: the triple, plus `.<glibc>` for a zig glibc floor.
    #[must_use]
    pub fn cargo_target(&self) -> String {
        match (&self.platform.glibc, self.builder) {
            (Some(glibc), Builder::Zigbuild) => format!("{}.{glibc}", self.platform.triple),
            _ => self.platform.triple.clone(),
        }
    }

    /// Cargo arguments (after `cargo`).
    #[must_use]
    pub fn cargo_args(&self) -> Vec<String> {
        let mut args = vec![
            match self.builder {
                Builder::Cargo => "build",
                Builder::Zigbuild => "zigbuild",
            }
            .to_string(),
            "--release".to_string(),
            "--locked".to_string(),
            "--package".to_string(),
            self.binary.package.clone(),
            "--bin".to_string(),
            self.binary.bin.clone(),
            "--target".to_string(),
            self.cargo_target(),
        ];
        if !self.binary.features.is_empty() {
            args.push("--features".to_string());
            args.push(self.binary.features.join(","));
        }
        args
    }

    /// Where cargo writes the binary.
    #[must_use]
    pub fn output_path(&self, target_dir: &Path) -> PathBuf {
        let suffix = if self.platform.platform.is_windows() {
            ".exe"
        } else {
            ""
        };
        target_dir
            .join(&self.platform.triple)
            .join("release")
            .join(format!("{}{suffix}", self.binary.bin))
    }

    /// The command line, for plans and logs.
    #[must_use]
    pub fn display(&self) -> String {
        format!(
            "{}/{} [{}]: cargo {}",
            self.binary.destination.key(),
            self.binary.name,
            self.platform.platform,
            self.cargo_args().join(" ")
        )
    }
}

/// Resolves `--platform` values: a key, `host` or `all` (default `host`).
///
/// # Errors
///
/// Names an unknown or undeclared platform, or a host the declaration does
/// not build.
pub fn select_platforms(
    requested: &[String],
    declaration: &Declaration,
    host_triple: &str,
) -> Result<Vec<Platform>, String> {
    let host = || {
        declaration
            .platforms
            .iter()
            .find(|spec| spec.triple == host_triple)
            .or_else(|| {
                declaration
                    .platforms
                    .iter()
                    .find(|spec| spec.platform.matches_triple(host_triple))
            })
            .map(|spec| spec.platform)
            .ok_or_else(|| format!("host {host_triple} is not one of the declared platforms"))
    };
    let mut selected = BTreeSet::new();
    let defaults = ["host".to_string()];
    let requested = if requested.is_empty() {
        &defaults[..]
    } else {
        requested
    };
    for value in requested {
        match value.as_str() {
            "all" => selected.extend(declaration.platforms.iter().map(|spec| spec.platform)),
            "host" => {
                selected.insert(host()?);
            }
            key => {
                let platform = Platform::from_key(key).ok_or_else(|| {
                    format!(
                        "unknown platform {key:?} (expected host, all or one of {})",
                        Platform::ALL.map(Platform::key).join(", ")
                    )
                })?;
                if declaration.platform(platform).is_none() {
                    return Err(format!(
                        "platform {key} is not declared in {}",
                        crate::DECLARATION_PATH
                    ));
                }
                selected.insert(platform);
            }
        }
    }
    Ok(selected.into_iter().collect())
}

/// Plans the build steps for `only` (`plugin/name`; empty means every
/// declared binary) on `platforms`.
///
/// # Errors
///
/// Every reason the plan cannot be built on this machine, each with the
/// remedy.
pub fn plan(
    declaration: &Declaration,
    platforms: &[Platform],
    only: &[String],
    probe: &Probe,
) -> Result<Vec<BuildStep>, Vec<String>> {
    let mut problems = Vec::new();
    let mut binaries = Vec::new();
    if only.is_empty() {
        binaries.extend(declaration.binaries.iter());
    } else {
        for selector in only {
            match declaration.select(selector) {
                Some(binary) => binaries.push(binary),
                None => problems.push(format!(
                    "--only {}: no such PLUGIN/NAME or PACKAGE_ROOT/NAME in {}",
                    toolkit_core::escape_untrusted(selector, 120),
                    crate::DECLARATION_PATH
                )),
            }
        }
    }
    if binaries.is_empty() && problems.is_empty() {
        problems.push(format!("{} declares no binaries", crate::DECLARATION_PATH));
    }
    for binary in &binaries {
        match probe.workspace_bins.get(&binary.package) {
            None => problems.push(format!(
                "{}/{}: package {} is not a member of this workspace (consumer {}); land it first or select other binaries with --only",
                binary.destination.key(), binary.name, binary.package, binary.consumer
            )),
            Some(bins) if !bins.contains(&binary.bin) => problems.push(format!(
                "{}/{}: package {} has no bin target {}",
                binary.destination.key(), binary.name, binary.package, binary.bin
            )),
            Some(_) => {}
        }
    }
    let mut steps = Vec::new();
    for platform in platforms {
        let Some(spec) = declaration.platform(*platform) else {
            problems.push(format!("platform {platform} is not declared"));
            continue;
        };
        let builder = match builder_for(spec, probe) {
            Ok(builder) => builder,
            Err(problem) => {
                problems.push(problem);
                continue;
            }
        };
        for binary in &binaries {
            steps.push(BuildStep {
                binary: (*binary).clone(),
                platform: spec.clone(),
                builder,
            });
        }
    }
    if problems.is_empty() {
        Ok(steps)
    } else {
        problems.dedup();
        Err(problems)
    }
}

fn builder_for(spec: &PlatformSpec, probe: &Probe) -> Result<Builder, String> {
    let platform = spec.platform;
    let triple = &spec.triple;
    if !probe.installed_targets.contains(triple) {
        return Err(format!(
            "platform {platform}: Rust target {triple} is not installed for {}; run `rustup target add {triple}` or build this platform on a native runner",
            probe.rustc_version
        ));
    }
    if *triple == probe.host_triple && spec.glibc.is_none() {
        return Ok(Builder::Cargo);
    }
    let mut missing = Vec::new();
    if probe.zig.is_none() {
        missing.push("zig");
    }
    if !probe.cargo_zigbuild {
        missing.push("cargo-zigbuild");
    }
    if missing.is_empty() {
        return Ok(Builder::Zigbuild);
    }
    let why = if *triple == probe.host_triple {
        format!(
            "the glibc {} floor is linked by zig",
            spec.glibc.as_deref().unwrap_or_default()
        )
    } else {
        format!(
            "{triple} is not the host target {} and zig cross-links it",
            probe.host_triple
        )
    };
    Err(format!(
        "platform {platform}: {why}, but {} not found on PATH; install it locally or build this platform on a native runner",
        missing.join(" and ")
    ))
}

/// Provenance written into every manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// 40-hex source commit.
    pub source_commit: String,
    /// Whether the checkout differed from the commit: a tracked change or an
    /// untracked, non-ignored file.
    pub source_dirty: bool,
    /// `rustc -V`.
    pub rustc: String,
}

/// One built file to install.
#[derive(Debug, Clone)]
pub struct Built {
    /// The step that built it.
    pub step: BuildStep,
    /// The built file.
    pub file: PathBuf,
}

/// Fails when `plugin`'s existing `bin/` cannot be merged with a stage from
/// `provenance`: it holds files from another or a dirty commit, or has no
/// valid manifest. `clean` allows replacing it.
///
/// # Errors
///
/// Names the conflict and the `--clean` remedy.
pub fn check_merge(
    plugins_root: &Path,
    plugin: &str,
    provenance: &Provenance,
    clean: bool,
) -> Result<(), String> {
    check_merge_at(&plugins_root.join(plugin), provenance, clean)
}

/// [`check_merge`] for any destination directory `dest_dir` (the parent of
/// its `bin/`: a plugin directory or a package destination).
///
/// # Errors
///
/// As [`check_merge`].
pub fn check_merge_at(dest_dir: &Path, provenance: &Provenance, clean: bool) -> Result<(), String> {
    existing_entries(dest_dir, provenance, clean).map(|_| ())
}

/// Refuses to stage through a symlink (#5023), with or without `--clean`:
/// the destination directory, `bin/` and every entry of `bin/` and of its
/// platform directories must be real. Returns whether `bin/` exists.
fn check_layout(dest_dir: &Path) -> Result<bool, String> {
    let staged = staged_bin_dir(Path::new(""), dest_dir)
        .map_err(|err| format!("{err}; refusing to stage through it"))?;
    if !staged {
        return Ok(false);
    }
    let bin_dir = dest_dir.join(BIN_DIR);
    let mut pending = vec![bin_dir.clone()];
    while let Some(dir) = pending.pop() {
        let entries = fs::read_dir(&dir)
            .map_err(|err| format!("{}: cannot be listed: {err}", dir.display()))?;
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(|err| format!("{}: {err}", path.display()))?;
            if kind.is_symlink() {
                return Err(format!(
                    "{}: is a symlink; staged files and platform directories must be real, so refusing to stage through it (remove it first)",
                    path.display()
                ));
            }
            if kind.is_dir() && dir == bin_dir {
                pending.push(path);
            }
        }
    }
    Ok(true)
}

fn existing_entries(
    dest_dir: &Path,
    provenance: &Provenance,
    clean: bool,
) -> Result<Vec<StagedBinary>, String> {
    let bin_dir = dest_dir.join(BIN_DIR);
    if !check_layout(dest_dir)? || clean {
        return Ok(Vec::new());
    }
    let remedy = "rerun with --clean to replace it";
    let text = read_regular(&bin_dir.join(MANIFEST_FILE))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(|| {
            format!(
                "{}: exists without a {MANIFEST_FILE}; {remedy}",
                bin_dir.display()
            )
        })?;
    let manifest = StagingManifest::parse(&text)
        .map_err(|err| format!("{}/{MANIFEST_FILE}: {err}; {remedy}", bin_dir.display()))?;
    if manifest.source_commit != provenance.source_commit
        || manifest.source_dirty
        || provenance.source_dirty
        || manifest.rustc != provenance.rustc
    {
        return Err(format!(
            "{}: holds binaries from {}{} ({}); this stage is from {}{} ({}); one manifest records one clean source, so {remedy}",
            bin_dir.display(),
            manifest.source_commit,
            if manifest.source_dirty { " (dirty)" } else { "" },
            manifest.rustc,
            provenance.source_commit,
            if provenance.source_dirty { " (dirty)" } else { "" },
            provenance.rustc,
        ));
    }
    Ok(manifest.binaries)
}

/// Installs built files into `plugins_root/<plugin>/bin/`, merging with an
/// existing stage of the same clean commit (or replacing it with `clean`),
/// writes one dispatcher per binary name and the manifest, and returns it.
///
/// # Errors
///
/// A merge conflict ([`check_merge`]), a missing plugin directory, a built
/// file that cannot be read, or a write failure.
pub fn install(
    plugins_root: &Path,
    plugin: &str,
    built: &[Built],
    provenance: &Provenance,
    clean: bool,
) -> Result<StagingManifest, String> {
    install_at(
        &plugins_root.join(plugin),
        &Destination::Plugin(plugin.to_string()),
        built,
        provenance,
        clean,
    )
}

/// [`install`] for any destination: installs into `dest_dir/bin/` (the
/// plugin directory or the package destination). Every built step must
/// belong to `destination`, and the manifest names it.
///
/// # Errors
///
/// As [`install`].
pub fn install_at(
    dest_dir: &Path,
    destination: &Destination,
    built: &[Built],
    provenance: &Provenance,
    clean: bool,
) -> Result<StagingManifest, String> {
    let plugin_dir = dest_dir;
    if !plugin_dir.is_dir() {
        return Err(format!(
            "{}: {} directory does not exist",
            plugin_dir.display(),
            destination.label()
        ));
    }
    let mut entries: BTreeMap<(String, Platform), StagedBinary> =
        existing_entries(dest_dir, provenance, clean)?
            .into_iter()
            .map(|binary| ((binary.name.clone(), binary.platform), binary))
            .collect();
    let bin_dir = plugin_dir.join(BIN_DIR);
    if clean && bin_dir.exists() {
        fs::remove_dir_all(&bin_dir)
            .map_err(|err| format!("{}: cannot remove: {err}", bin_dir.display()))?;
    }
    for item in built {
        let step = &item.step;
        if step.binary.destination != *destination {
            return Err(format!(
                "{}: built for {}",
                item.file.display(),
                step.binary.destination.label()
            ));
        }
        let bytes = fs::read(&item.file)
            .map_err(|err| format!("{}: cannot read: {err}", item.file.display()))?;
        let platform = step.platform.platform;
        let path = platform.staged_path(&step.binary.name);
        write_executable(&bin_dir.join(&path), &bytes)?;
        entries.insert(
            (step.binary.name.clone(), platform),
            StagedBinary {
                name: step.binary.name.clone(),
                platform,
                triple: step.platform.triple.clone(),
                package: step.binary.package.clone(),
                bin: step.binary.bin.clone(),
                features: step.binary.features.clone(),
                builder: step.builder.as_str().to_string(),
                path,
                size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                sha256: sha256_hex(&bytes),
            },
        );
    }
    let names: BTreeSet<String> = entries.keys().map(|(name, _)| name.clone()).collect();
    let mut dispatchers = Vec::new();
    for name in names {
        let script = dispatcher_script(&name);
        write_executable(&bin_dir.join(&name), script.as_bytes())?;
        dispatchers.push(StagedDispatcher {
            sha256: sha256_hex(script.as_bytes()),
            name,
        });
    }
    let manifest = StagingManifest {
        destination: destination.clone(),
        source_commit: provenance.source_commit.clone(),
        source_dirty: provenance.source_dirty,
        rustc: provenance.rustc.clone(),
        binaries: entries.into_values().collect(),
        dispatchers,
    };
    write_atomic(
        &bin_dir.join(MANIFEST_FILE),
        manifest.render().as_bytes(),
        false,
    )?;
    Ok(manifest)
}

fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_atomic(path, bytes, true)
}

/// Writes through a sibling temporary file and a rename, so a launcher never
/// runs a half-written binary.
fn write_atomic(path: &Path, bytes: &[u8], executable: bool) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{}: no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|err| format!("{}: cannot create: {err}", parent.display()))?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = parent.join(format!(".{file_name}.staging-{}", std::process::id()));
    let result = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        set_mode(&temp, executable)?;
        fs::rename(&temp, path)
    })();
    result.map_err(|err| {
        let _ = fs::remove_file(&temp);
        format!("{}: cannot write: {err}", path.display())
    })
}

#[cfg(unix)]
fn set_mode(path: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
    )
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _executable: bool) -> std::io::Result<()> {
    Ok(())
}

/// Runs a command in `dir` and returns trimmed stdout when it exits 0.
fn output(dir: &Path, program: &str, args: &[&str]) -> Option<String> {
    let result = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    result
        .status
        .success()
        .then(|| String::from_utf8_lossy(&result.stdout).trim().to_string())
}

impl Probe {
    /// Probes the toolchains reachable from `root` (the workspace, so
    /// `rust-toolchain.toml` selects the compiler). Nothing is installed or
    /// downloaded.
    ///
    /// # Errors
    ///
    /// When `rustc` or `cargo metadata` cannot run.
    pub fn detect(root: &Path) -> Result<Self, String> {
        let verbose = output(root, "rustc", &["-vV"])
            .ok_or("`rustc -vV` failed; is a Rust toolchain installed?")?;
        let host_triple = verbose
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .ok_or("`rustc -vV` printed no host line")?
            .to_string();
        let rustc_version = output(root, "rustc", &["-V"]).ok_or("`rustc -V` failed")?;
        let sysroot = output(root, "rustc", &["--print", "sysroot"])
            .ok_or("`rustc --print sysroot` failed")?;
        let mut installed_targets = BTreeSet::new();
        if let Ok(entries) = fs::read_dir(Path::new(&sysroot).join("lib").join("rustlib")) {
            for entry in entries.filter_map(Result::ok) {
                if entry.path().join("lib").is_dir() {
                    installed_targets.insert(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
        let zig = output(root, "zig", &["version"]);
        let cargo_zigbuild = output(root, "cargo", &["zigbuild", "--help"]).is_some();
        let metadata = output(
            root,
            "cargo",
            &[
                "metadata",
                "--no-deps",
                "--format-version",
                "1",
                "--locked",
                "--offline",
            ],
        )
        .ok_or("`cargo metadata --no-deps --locked --offline` failed")?;
        let (workspace_bins, target_dir) = parse_metadata(&metadata)?;
        Ok(Self {
            host_triple,
            rustc_version,
            installed_targets,
            zig,
            cargo_zigbuild,
            workspace_bins,
            target_dir,
        })
    }
}

/// Extracts workspace packages, their bin targets and the target directory
/// from `cargo metadata --no-deps --format-version 1` output.
///
/// # Errors
///
/// When the output is not the expected JSON.
pub fn parse_metadata(text: &str) -> Result<(BTreeMap<String, BTreeSet<String>>, PathBuf), String> {
    let root = parse_json(text).map_err(|err| format!("cargo metadata: {err}"))?;
    let target_dir = root
        .get("target_directory")
        .and_then(Json::as_str)
        .ok_or("cargo metadata: no target_directory")?;
    let Some(Json::Array(packages)) = root.get("packages") else {
        return Err("cargo metadata: no packages".to_string());
    };
    let mut bins = BTreeMap::new();
    for package in packages {
        let name = package
            .get("name")
            .and_then(Json::as_str)
            .ok_or("cargo metadata: package without a name")?;
        let mut targets = BTreeSet::new();
        if let Some(Json::Array(items)) = package.get("targets") {
            for target in items {
                let is_bin = matches!(target.get("kind"), Some(Json::Array(kinds)) if kinds.iter().any(|kind| kind.as_str() == Some("bin")));
                if let (true, Some(bin)) = (is_bin, target.get("name").and_then(Json::as_str)) {
                    targets.insert(bin.to_string());
                }
            }
        }
        bins.insert(name.to_string(), targets);
    }
    Ok((bins, PathBuf::from(target_dir)))
}

/// Reads the source commit and whether the checkout differs from it: a
/// tracked change, or any untracked file, including ignored files (#5027). Cargo
/// picks up untracked inputs on its own (an auto-discovered `build.rs`, a
/// new module, `.cargo/config.toml`, `rust-toolchain.toml`), so a build with
/// one present is not a build of the named commit. Ignored conventional local
/// noise and files under `generated_dirs` stay permitted (#5627). The latter
/// are repository-relative output directories supplied by the staging caller
/// (Cargo target and declared destination bin directories), never source directories inferred from ignore
/// rules. Ignoring an input does not make it part of the named commit.
///
/// # Errors
///
/// When `git` cannot report the commit or the status.
pub fn source_provenance(
    root: &Path,
    rustc: &str,
    generated_dirs: &[PathBuf],
) -> Result<Provenance, String> {
    let commit = output(root, "git", &["rev-parse", "HEAD"])
        .ok_or("`git rev-parse HEAD` failed; stage from a git checkout")?;
    let status = output(
        root,
        "git",
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )
    .ok_or("`git status` failed")?;
    // Exclude exact output trees before listing files. This avoids
    // enumerating Cargo artifacts without hiding ignored inputs alongside them.
    // NUL delimiters keep names with whitespace or non-UTF-8 bytes unambiguous.
    let mut ignored = Command::new("git");
    ignored.args([
        "ls-files",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--full-name",
        "-z",
        "--",
        ":/",
    ]);
    for dir in generated_dirs {
        if !dir.as_os_str().is_empty()
            && !dir.starts_with(".cargo")
            && dir
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            let dir = dir.to_str().ok_or("generated output path is not UTF-8")?;
            ignored.arg(format!(":(top,literal,exclude){dir}"));
        }
    }
    let ignored = ignored
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("cannot list ignored build inputs: {err}"))?;
    if !ignored.status.success() {
        return Err("`git ls-files` failed to list ignored build inputs".to_string());
    }
    // Do not collapse ignored directories: an exempt name must not hide an
    // unknown sibling or a Cargo config nested inside it.
    let mut cache_tags = BTreeMap::new();
    let mut ignored_input = false;
    for name in ignored
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(std::ffi::OsStr::from_bytes(name))
        };
        #[cfg(not(unix))]
        let path =
            PathBuf::from(std::str::from_utf8(name).map_err(|_| "ignored path is not UTF-8")?);
        if !ignored_non_input(root, &path, &mut cache_tags) {
            ignored_input = true;
            break;
        }
    }
    Ok(Provenance {
        source_commit: commit,
        source_dirty: !status.is_empty() || ignored_input,
        rustc: rustc.to_string(),
    })
}

fn ignored_non_input(root: &Path, path: &Path, cache_tags: &mut BTreeMap<PathBuf, bool>) -> bool {
    // Cargo runs at root, so only root/.cargo is auto-discovered here. Keep
    // *all* .cargo components visible anyway, including inside exempt names.
    if path.iter().any(|part| part == ".cargo") {
        return false;
    }
    if path == Path::new(".rsi-build-env.json") {
        return true;
    }
    if path.iter().any(|part| {
        ["node_modules", "__pycache__", ".idea", ".vscode"]
            .iter()
            .any(|name| part == *name)
    }) {
        return true;
    }
    if path.file_name().is_some_and(|name| {
        name == ".DS_Store"
            || name.as_encoded_bytes().ends_with(b".swp")
            || name.as_encoded_bytes().ends_with(b"~")
    }) {
        return true;
    }
    // A directory merely named target is not enough. Require Cargo's cache
    // marker, read without following a symlink; memoize per directory.
    let mut prefix = PathBuf::new();
    for part in path {
        prefix.push(part);
        if part == "target"
            && *cache_tags.entry(prefix.clone()).or_insert_with(|| {
                read_regular(&root.join(&prefix).join("CACHEDIR.TAG"))
                    .is_ok_and(|bytes| bytes.starts_with(b"Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.\n"))
            })
        {
            return true;
        }
    }
    false
}

/// #5592/#5603: never compile a shipped binary whose normal graph enables an
/// unsigned test-injection feature. The graph is the one this step compiles,
/// resolved under `resolver`'s network policy (production: the build's own,
/// so a cold registry cache that the build could fill is not an error). A
/// graph that cannot be resolved or validated refuses staging too.
///
/// # Errors
///
/// When the graph enables a forbidden feature or cannot be resolved.
pub fn refuse_forbidden_features(
    root: &Path,
    step: &BuildStep,
    resolver: &Resolver,
) -> Result<(), String> {
    let graph =
        crate::feature_graph::preflight(root, resolver, &step.binary, &step.platform.triple)?;
    if graph.violations.is_empty() {
        Ok(())
    } else {
        Err(graph.violations.join("; "))
    }
}

/// Runs one build step, streaming cargo's output to stderr. Ambient
/// `RUSTFLAGS` and per-target config `rustflags` are cleared so a
/// developer's local flags never reach a staged artifact.
///
/// # Errors
///
/// When the feature graph enables a forbidden feature, cargo cannot start,
/// fails, or leaves no output file.
pub fn build(root: &Path, step: &BuildStep, target_dir: &Path) -> Result<Built, String> {
    build_with(root, step, target_dir, &Resolver::production())
}

/// [`build`] with an explicit graph resolver (tests inject a fake cargo for
/// the preflight; the compile itself always runs `cargo`).
///
/// # Errors
///
/// As [`build`].
pub fn build_with(
    root: &Path,
    step: &BuildStep,
    target_dir: &Path,
    resolver: &Resolver,
) -> Result<Built, String> {
    refuse_forbidden_features(root, step, resolver)?;
    eprintln!("plugin-stage: building {}", step.display());
    let status = Command::new("cargo")
        .args(step.cargo_args())
        .current_dir(root)
        .env("CARGO_ENCODED_RUSTFLAGS", "")
        .env_remove("RUSTFLAGS")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|err| format!("{}: cannot run cargo: {err}", step.display()))?;
    if !status.success() {
        return Err(format!("{}: cargo failed ({status})", step.display()));
    }
    let file = step.output_path(target_dir);
    if !file.is_file() {
        return Err(format!(
            "{}: cargo produced no {}",
            step.display(),
            file.display()
        ));
    }
    Ok(Built {
        step: step.clone(),
        file,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECLARATION: &str = r##"{
      "schema": 1,
      "platforms": [
        {"platform": "linux-x86_64", "triple": "x86_64-unknown-linux-gnu", "glibc": "2.17"},
        {"platform": "macos-aarch64", "triple": "aarch64-apple-darwin"},
        {"platform": "windows-x86_64", "triple": "x86_64-pc-windows-gnu"}
      ],
      "binaries": [
        {"plugin": "p", "name": "guard", "package": "guard-pkg", "bin": "guard",
         "role": "guard-hook", "deny_reason": "Guard unavailable.", "consumer": "#1"},
        {"plugin": "p", "name": "tool", "package": "tool-pkg", "bin": "tool",
         "features": ["dep/sdk", "fast"], "role": "mcp-server", "consumer": "#2"}
      ]
    }"##;

    fn declaration() -> Declaration {
        Declaration::parse(DECLARATION).expect("declaration")
    }

    fn probe() -> Probe {
        Probe {
            host_triple: "aarch64-apple-darwin".to_string(),
            rustc_version: "rustc 1.94.1".to_string(),
            installed_targets: [
                "x86_64-unknown-linux-gnu",
                "aarch64-apple-darwin",
                "x86_64-pc-windows-gnu",
            ]
            .map(str::to_string)
            .into(),
            zig: Some("0.17.0".to_string()),
            cargo_zigbuild: true,
            workspace_bins: BTreeMap::from([
                (
                    "guard-pkg".to_string(),
                    BTreeSet::from(["guard".to_string()]),
                ),
                ("tool-pkg".to_string(), BTreeSet::from(["tool".to_string()])),
            ]),
            target_dir: PathBuf::from("/t"),
        }
    }

    fn all() -> Vec<Platform> {
        vec![
            Platform::LinuxX86_64,
            Platform::MacosAarch64,
            Platform::WindowsX86_64,
        ]
    }

    #[test]
    fn the_host_builds_natively_and_other_targets_through_zig() {
        let steps = plan(&declaration(), &all(), &[], &probe()).expect("plan");
        assert_eq!(steps.len(), 6);
        let find = |name: &str, platform| {
            steps
                .iter()
                .find(|step| step.binary.name == name && step.platform.platform == platform)
                .expect("step")
        };
        let mac = find("tool", Platform::MacosAarch64);
        assert_eq!(mac.builder, Builder::Cargo);
        assert_eq!(
            mac.cargo_args(),
            [
                "build",
                "--release",
                "--locked",
                "--package",
                "tool-pkg",
                "--bin",
                "tool",
                "--target",
                "aarch64-apple-darwin",
                "--features",
                "dep/sdk,fast"
            ]
        );
        assert_eq!(
            mac.output_path(Path::new("/t")),
            Path::new("/t/aarch64-apple-darwin/release/tool")
        );
        let linux = find("guard", Platform::LinuxX86_64);
        assert_eq!(linux.builder, Builder::Zigbuild);
        assert_eq!(linux.cargo_target(), "x86_64-unknown-linux-gnu.2.17");
        assert_eq!(linux.cargo_args()[0], "zigbuild");
        assert_eq!(
            linux.output_path(Path::new("/t")),
            Path::new("/t/x86_64-unknown-linux-gnu/release/guard")
        );
        let windows = find("guard", Platform::WindowsX86_64);
        assert_eq!(
            windows.output_path(Path::new("/t")),
            Path::new("/t/x86_64-pc-windows-gnu/release/guard.exe")
        );
    }

    #[test]
    fn a_glibc_floor_on_the_host_still_needs_zig() {
        let mut probe = probe();
        probe.host_triple = "x86_64-unknown-linux-gnu".to_string();
        probe.zig = None;
        let problems =
            plan(&declaration(), &[Platform::LinuxX86_64], &[], &probe).expect_err("no zig");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("glibc 2.17 floor"), "{}", problems[0]);
        assert!(problems[0].contains("zig not found"), "{}", problems[0]);
    }

    #[test]
    fn missing_targets_and_cross_linkers_fail_with_a_remedy_before_any_build() {
        let mut probe = probe();
        probe.installed_targets.remove("x86_64-pc-windows-gnu");
        probe.zig = None;
        probe.cargo_zigbuild = false;
        let problems = plan(&declaration(), &all(), &[], &probe).expect_err("problems");
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].contains("platform linux-x86_64"));
        assert!(problems[0].contains("zig and cargo-zigbuild not found on PATH"));
        assert!(problems[0].contains("native runner"));
        assert!(problems[1].contains(
            "platform windows-x86_64: Rust target x86_64-pc-windows-gnu is not installed"
        ));
        assert!(problems[1].contains("rustup target add x86_64-pc-windows-gnu"));
    }

    #[test]
    fn packages_outside_the_workspace_and_unknown_selectors_are_refused() {
        let mut probe = probe();
        probe.workspace_bins.remove("guard-pkg");
        probe.workspace_bins.insert(
            "tool-pkg".to_string(),
            BTreeSet::from(["other".to_string()]),
        );
        let problems =
            plan(&declaration(), &[Platform::MacosAarch64], &[], &probe).expect_err("problems");
        assert!(problems[0]
            .contains("package guard-pkg is not a member of this workspace (consumer #1)"));
        assert!(problems[1].contains("package tool-pkg has no bin target tool"));
        let problems = plan(
            &declaration(),
            &[Platform::MacosAarch64],
            &["p/nope".to_string()],
            &probe,
        )
        .expect_err("unknown");
        assert!(problems[0].contains("--only p/nope"));
        let steps = plan(
            &declaration(),
            &[Platform::MacosAarch64],
            &["p/guard".to_string()],
            &self::probe(),
        )
        .expect("only guard");
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].binary.name, "guard");
    }

    #[test]
    fn platform_selection_resolves_host_all_and_keys() {
        let declaration = declaration();
        let host = select_platforms(&[], &declaration, "aarch64-apple-darwin").expect("host");
        assert_eq!(host, [Platform::MacosAarch64]);
        let linux_host = select_platforms(
            &["host".to_string()],
            &declaration,
            "x86_64-unknown-linux-musl",
        )
        .expect("host by os and arch");
        assert_eq!(linux_host, [Platform::LinuxX86_64]);
        assert_eq!(
            select_platforms(&["all".to_string()], &declaration, "aarch64-apple-darwin")
                .expect("all"),
            all()
        );
        let error = select_platforms(&["linux-aarch64".to_string()], &declaration, "x")
            .expect_err("undeclared");
        assert!(error.contains("not declared"));
        let error =
            select_platforms(&["solaris".to_string()], &declaration, "x").expect_err("unknown");
        assert!(error.contains("unknown platform"));
        let error =
            select_platforms(&[], &declaration, "riscv64gc-unknown-linux-gnu").expect_err("host");
        assert!(error.contains("not one of the declared platforms"));
    }

    #[test]
    fn cargo_metadata_yields_packages_bins_and_the_target_dir() {
        let text = r#"{"packages": [
            {"name": "a", "targets": [{"kind": ["lib"], "name": "a"}, {"kind": ["bin"], "name": "a-cli"}]},
            {"name": "b", "targets": [{"kind": ["test"], "name": "t"}]}
          ], "target_directory": "/work/target", "version": 1}"#;
        let (bins, target) = parse_metadata(text).expect("metadata");
        assert_eq!(target, PathBuf::from("/work/target"));
        assert_eq!(bins["a"], BTreeSet::from(["a-cli".to_string()]));
        assert!(bins["b"].is_empty());
        assert!(parse_metadata("{}").is_err());
    }
}
