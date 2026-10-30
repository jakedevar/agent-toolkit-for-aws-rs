//! The committed binary declaration, `packaging/plugin-binaries.json`.
//!
//! It is the source of truth the launcher contract resolves against: which
//! binaries each plugin ships, which workspace package and bin builds each
//! one, the platforms (and Rust target triples) the pipeline builds, and how
//! each binary may be launched.

use std::collections::BTreeSet;
use std::path::Path;

use toolkit_core::diag::DEFAULT_MAX_CHARS;
use toolkit_core::{escape_untrusted, parse_json, quote_untrusted, Json};

use crate::platform::Platform;

/// Repository-relative path of the declaration.
pub const DECLARATION_PATH: &str = "packaging/plugin-binaries.json";

/// A platform the pipeline builds and the Rust target that builds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformSpec {
    /// Platform key.
    pub platform: Platform,
    /// Rust target triple.
    pub triple: String,
    /// Oldest glibc the Linux build must run on (built through
    /// `cargo zigbuild` with `<triple>.<glibc>`); `None` elsewhere.
    pub glibc: Option<String>,
}

/// Where a binary is staged: a plugin `bin/` directory or a package
/// destination (#4411 B3, R-PKG-1).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Destination {
    /// `plugins/<plugin>/bin/` (the plugin directory name).
    Plugin(String),
    /// `<package_root>/bin/` (a repository-relative path to an npm package
    /// directory inside the source tree).
    PackageRoot(String),
}

impl Destination {
    /// The plugin name or the package root path.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Plugin(key) | Self::PackageRoot(key) => key,
        }
    }

    /// The destination directory (the parent of its `bin/`).
    #[must_use]
    pub fn dir(&self, repo_root: &Path, plugins_root: &Path) -> std::path::PathBuf {
        match self {
            Self::Plugin(plugin) => plugins_root.join(plugin),
            Self::PackageRoot(root) => repo_root.join(root),
        }
    }

    /// `plugin NAME` or `package_root PATH`, for messages.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Plugin(plugin) => {
                format!("plugin {}", escape_untrusted(plugin, DEFAULT_MAX_CHARS))
            }
            Self::PackageRoot(root) => {
                format!("package_root {}", escape_untrusted(root, DEFAULT_MAX_CHARS))
            }
        }
    }
}

/// How a staged binary may be launched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// A `PreToolUse` guard. Every launcher must be the canonical fail-closed
    /// command ([`crate::launcher::guard_hook_command`]) that prints a deny
    /// decision carrying `deny_reason` when the binary is missing or fails.
    GuardHook {
        /// Reason shown to the user in the fallback deny.
        deny_reason: String,
    },
    /// A stdio MCP server: the server `command` is exactly the dispatcher.
    McpServer,
    /// Any other (non-guarding) hook command.
    Hook,
    /// Invoked by package code or a plugin skill, never by a launcher
    /// registration (R-PKG-2, #5586): `plugin-stage launcher` refuses it.
    Subprocess,
}

impl Role {
    /// The declaration spelling of the role.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GuardHook { .. } => "guard-hook",
            Self::McpServer => "mcp-server",
            Self::Hook => "hook",
            Self::Subprocess => "subprocess",
        }
    }
}

/// One binary a plugin or package destination ships.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinarySpec {
    /// Where it is staged.
    pub destination: Destination,
    /// Staged name: the dispatcher is `bin/<name>`.
    pub name: String,
    /// Workspace package that builds it.
    pub package: String,
    /// Cargo bin target inside `package`.
    pub bin: String,
    /// Cargo features (`feature` or `dependency/feature`).
    pub features: Vec<String>,
    /// Launch role.
    pub role: Role,
    /// Issue that consumes the binary, for messages (for example `#4412`).
    pub consumer: String,
}

impl BinarySpec {
    /// The plugin directory name, for a plugin destination.
    #[must_use]
    pub fn plugin(&self) -> Option<&str> {
        match &self.destination {
            Destination::Plugin(plugin) => Some(plugin),
            Destination::PackageRoot(_) => None,
        }
    }
}

/// The parsed, validated declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// Platforms the pipeline builds, in declaration order.
    pub platforms: Vec<PlatformSpec>,
    /// Declared binaries, in declaration order.
    pub binaries: Vec<BinarySpec>,
}

impl Declaration {
    /// Reads and validates the declaration file.
    ///
    /// # Errors
    ///
    /// Returns a message naming the file when it cannot be read or is invalid.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| format!("{}: cannot read: {err}", path.display()))?;
        Self::parse(&text).map_err(|err| format!("{}: {err}", path.display()))
    }

    /// Parses and validates declaration text.
    ///
    /// # Errors
    ///
    /// Returns the first validation failure.
    pub fn parse(text: &str) -> Result<Self, String> {
        let root = parse_json(text).map_err(|err| format!("invalid JSON: {err}"))?;
        let pairs = object(&root, "declaration")?;
        reject_unknown(
            pairs,
            &["schema", "note", "platforms", "binaries"],
            "declaration",
        )?;
        match root.get("schema") {
            Some(Json::Int(1)) => {}
            _ => return Err("declaration: `schema` must be 1".to_string()),
        }
        let platforms = match root.get("platforms") {
            Some(Json::Array(items)) => items
                .iter()
                .map(parse_platform)
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err("declaration: `platforms` must be an array".to_string()),
        };
        let binaries = match root.get("binaries") {
            Some(Json::Array(items)) => items
                .iter()
                .map(parse_binary)
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err("declaration: `binaries` must be an array".to_string()),
        };
        let declaration = Self {
            platforms,
            binaries,
        };
        declaration.check_unique()?;
        Ok(declaration)
    }

    fn check_unique(&self) -> Result<(), String> {
        if self.platforms.is_empty() {
            return Err("declaration: `platforms` is empty".to_string());
        }
        let mut seen = BTreeSet::new();
        for spec in &self.platforms {
            if !seen.insert(spec.platform) {
                return Err(format!(
                    "declaration: platform {} is listed twice",
                    spec.platform
                ));
            }
        }
        let mut names = BTreeSet::new();
        for binary in &self.binaries {
            if !names.insert((&binary.destination, binary.name.as_str())) {
                return Err(format!(
                    "declaration: binary {}/{} is listed twice",
                    binary.destination.key(),
                    binary.name
                ));
            }
        }
        let plugins = self.plugins();
        let roots = self.package_roots();
        for root in &roots {
            if plugins.contains(root) {
                return Err(format!(
                    "declaration: package_root {root} is also a plugin name, so PLUGIN/NAME selectors would be ambiguous"
                ));
            }
            for other in &roots {
                let inside_bin = root
                    .strip_prefix(other)
                    .and_then(|rest| rest.strip_prefix('/'))
                    .is_some_and(|rest| {
                        rest == crate::BIN_DIR || rest.starts_with(&format!("{}/", crate::BIN_DIR))
                    });
                if inside_bin {
                    return Err(format!(
                        "declaration: package_root {root} lies inside the bin/ of package_root {other}"
                    ));
                }
            }
        }
        Ok(())
    }

    /// The declared spec of one platform.
    #[must_use]
    pub fn platform(&self, platform: Platform) -> Option<&PlatformSpec> {
        self.platforms.iter().find(|spec| spec.platform == platform)
    }

    /// The declared binary `name` of `plugin` (plugin destinations only: a
    /// package binary is never resolved by a plugin launcher).
    #[must_use]
    pub fn binary(&self, plugin: &str, name: &str) -> Option<&BinarySpec> {
        self.binaries
            .iter()
            .find(|binary| binary.plugin() == Some(plugin) && binary.name == name)
    }

    /// The declared binaries of one plugin.
    pub fn plugin_binaries<'a>(&'a self, plugin: &'a str) -> impl Iterator<Item = &'a BinarySpec> {
        self.binaries
            .iter()
            .filter(move |binary| binary.plugin() == Some(plugin))
    }

    /// The declared binaries of one package destination.
    pub fn package_binaries<'a>(&'a self, root: &'a str) -> impl Iterator<Item = &'a BinarySpec> {
        self.binaries.iter().filter(
            move |binary| matches!(&binary.destination, Destination::PackageRoot(r) if r == root),
        )
    }

    /// The declared binaries of one destination.
    pub fn destination_binaries<'a>(
        &'a self,
        destination: &'a Destination,
    ) -> impl Iterator<Item = &'a BinarySpec> {
        self.binaries
            .iter()
            .filter(move |binary| &binary.destination == destination)
    }

    /// Distinct plugins that ship at least one binary.
    #[must_use]
    pub fn plugins(&self) -> BTreeSet<&str> {
        self.binaries
            .iter()
            .filter_map(BinarySpec::plugin)
            .collect()
    }

    /// Distinct package destinations.
    #[must_use]
    pub fn package_roots(&self) -> BTreeSet<&str> {
        self.binaries
            .iter()
            .filter_map(|binary| match &binary.destination {
                Destination::PackageRoot(root) => Some(root.as_str()),
                Destination::Plugin(_) => None,
            })
            .collect()
    }

    /// Every distinct destination (plugins sort first).
    #[must_use]
    pub fn destinations(&self) -> BTreeSet<&Destination> {
        self.binaries
            .iter()
            .map(|binary| &binary.destination)
            .collect()
    }

    /// Resolves a `PLUGIN/NAME` or `PACKAGE_ROOT/NAME` selector (the name is
    /// the last path segment).
    #[must_use]
    pub fn select(&self, selector: &str) -> Option<&BinarySpec> {
        let (key, name) = selector.rsplit_once('/')?;
        self.binaries
            .iter()
            .find(|binary| binary.destination.key() == key && binary.name == name)
    }
}

fn parse_platform(value: &Json) -> Result<PlatformSpec, String> {
    let pairs = object(value, "platform")?;
    reject_unknown(pairs, &["platform", "triple", "glibc"], "platform")?;
    let key = required_str(value, "platform", "platform")?;
    let platform = Platform::from_key(key).ok_or_else(|| {
        format!(
            "platform: unknown key {key:?} (expected one of {})",
            Platform::ALL.map(Platform::key).join(", ")
        )
    })?;
    let triple = required_str(value, "triple", key)?;
    if !is_token(triple, b"_.-") || !platform.matches_triple(triple) {
        return Err(format!(
            "platform {key}: triple {triple:?} is not a {} {} target",
            platform.os(),
            platform.arch()
        ));
    }
    let glibc = optional_str(value, "glibc", key)?;
    if let Some(version) = glibc {
        let mut parts = version.split('.');
        let numeric = |part: Option<&str>| {
            part.is_some_and(|text| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        };
        if platform.os() != "linux"
            || !numeric(parts.next())
            || !numeric(parts.next())
            || parts.next().is_some()
        {
            return Err(format!(
                "platform {key}: `glibc` must be a MAJOR.MINOR version and is only valid for Linux"
            ));
        }
    }
    Ok(PlatformSpec {
        platform,
        triple: triple.to_string(),
        glibc: glibc.map(str::to_string),
    })
}

/// R-PKG-1: an entry names exactly one of `plugin` or `package_root`.
fn parse_destination(value: &Json) -> Result<Destination, String> {
    match (value.get("plugin"), value.get("package_root")) {
        (Some(_), None) => {
            let plugin = required_str(value, "plugin", "binary")?;
            if !is_name(plugin) {
                return Err(format!(
                    "binary in plugin {}: plugin must be lower-case ASCII letters, digits, `-` or `_`, starting with a letter or digit",
                    quote_untrusted(plugin)
                ));
            }
            Ok(Destination::Plugin(plugin.to_string()))
        }
        (None, Some(_)) => {
            let root = required_str(value, "package_root", "binary")?;
            check_package_root_path(root)?;
            Ok(Destination::PackageRoot(root.to_string()))
        }
        _ => Err("binary: an entry names exactly one of `plugin` or `package_root`".to_string()),
    }
}

/// R-PKG-2: a package destination has no launcher files, so its binaries
/// are spawned by code. A plugin may also ship a skill subprocess (#5586);
/// that role still cannot be registered as a hook or an MCP server.
fn check_role_destination(
    label: &str,
    destination: &Destination,
    role: &Role,
) -> Result<(), String> {
    match (destination, role) {
        (Destination::PackageRoot(_), Role::Subprocess) | (Destination::Plugin(_), _) => Ok(()),
        (Destination::PackageRoot(_), _) => Err(format!(
            "{label}: a package_root binary must have role subprocess (code spawns it; no launcher can reach a package bin/)"
        )),
    }
}

fn parse_binary(value: &Json) -> Result<BinarySpec, String> {
    let pairs = object(value, "binary")?;
    reject_unknown(
        pairs,
        &[
            "plugin",
            "package_root",
            "name",
            "package",
            "bin",
            "features",
            "role",
            "deny_reason",
            "consumer",
        ],
        "binary",
    )?;
    let destination = parse_destination(value)?;
    let name = required_str(value, "name", "binary")?;
    let label = format!(
        "binary {}/{}",
        destination.key(),
        escape_untrusted(name, DEFAULT_MAX_CHARS)
    );
    if !is_name(name) {
        return Err(format!(
            "{label}: plugin and name must be lower-case ASCII letters, digits, `-` or `_`, starting with a letter or digit"
        ));
    }
    if Platform::from_key(name).is_some() {
        return Err(format!("{label}: name collides with a platform directory"));
    }
    let package = required_str(value, "package", &label)?;
    let bin = required_str(value, "bin", &label)?;
    if !is_token(package, b"_-") || !is_token(bin, b"_-") {
        return Err(format!(
            "{label}: package and bin must be cargo target names"
        ));
    }
    let features = match value.get("features") {
        None => Vec::new(),
        Some(Json::Array(items)) => items
            .iter()
            .map(|item| match item.as_str() {
                Some(feature) if is_token(feature, b"_-/") => Ok(feature.to_string()),
                _ => Err(format!(
                    "{label}: every feature must be a cargo feature name"
                )),
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(format!("{label}: `features` must be an array")),
    };
    let deny_reason = optional_str(value, "deny_reason", &label)?;
    let role = match required_str(value, "role", &label)? {
        "guard-hook" => {
            let reason =
                deny_reason.ok_or_else(|| format!("{label}: a guard-hook needs `deny_reason`"))?;
            if !is_safe_reason(reason) {
                return Err(format!(
                    "{label}: `deny_reason` must be non-empty and use only letters, digits, spaces and . , : ; ( ) - _ /"
                ));
            }
            Role::GuardHook {
                deny_reason: reason.to_string(),
            }
        }
        other => {
            if deny_reason.is_some() {
                return Err(format!(
                    "{label}: `deny_reason` is only valid for a guard-hook"
                ));
            }
            match other {
                "mcp-server" => Role::McpServer,
                "hook" => Role::Hook,
                "subprocess" => Role::Subprocess,
                _ => {
                    return Err(format!(
                        "{label}: role {} is not guard-hook, mcp-server, hook or subprocess",
                        quote_untrusted(other)
                    ))
                }
            }
        }
    };
    check_role_destination(&label, &destination, &role)?;
    let consumer = required_str(value, "consumer", &label)?;
    Ok(BinarySpec {
        destination,
        name: name.to_string(),
        package: package.to_string(),
        bin: bin.to_string(),
        features,
        role,
        consumer: consumer.to_string(),
    })
}

/// Checks a declared package destination on disk (R-PKG-1): every path
/// component is a real directory (no symlink), and it holds a regular
/// `package.json` and a regular `.gitignore` that ignores `bin/`. Returns the
/// problems; empty means the destination is usable.
#[must_use]
pub fn check_package_root(repo_root: &Path, root: &str) -> Vec<String> {
    let label = format!("package_root {}", quote_untrusted(root));
    if let Err(problem) = check_package_root_path(root) {
        return vec![problem];
    }
    let mut dir = repo_root.to_path_buf();
    for component in root.split('/') {
        dir.push(component);
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return vec![format!(
                    "{label}: path component {} is a symlink; a package destination must be a real directory inside the checkout",
                    quote_untrusted(component)
                )]
            }
            Ok(meta) if meta.is_dir() => {}
            _ => {
                return vec![format!(
                    "{label}: {} is not a directory",
                    quote_untrusted(component)
                )]
            }
        }
    }
    let mut problems = Vec::new();
    let regular = |name: &str| {
        std::fs::symlink_metadata(dir.join(name)).is_ok_and(|meta| meta.file_type().is_file())
    };
    if !regular("package.json") {
        problems.push(format!(
            "{label}: has no regular package.json, so it is not an npm package"
        ));
    }
    if regular(".gitignore") {
        let text = std::fs::read_to_string(dir.join(".gitignore")).unwrap_or_default();
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        let ignores = lines.iter().any(|line| matches!(*line, "bin/" | "/bin/"));
        // verify-package stays git-free, so it cannot evaluate a pattern: any
        // negation (`!*/`, `!*`, `!b?n`) might re-include bin/ and is refused.
        let reincludes = lines.iter().any(|line| line.starts_with('!'));
        if !ignores || reincludes {
            problems.push(format!(
                "{label}: .gitignore must ignore bin/ (a `bin/` line and no `!` negation line), so staged binaries are never committed"
            ));
        }
    } else {
        problems.push(format!(
            "{label}: has no regular .gitignore; it must ignore bin/"
        ));
    }
    problems
}

/// The textual rules for a `package_root` (R-PKG-1): a normalized
/// repository-relative path of plain segments, not inside a `plugins/*/bin/`.
fn check_package_root_path(root: &str) -> Result<(), String> {
    let refused = |why: &str| {
        Err(format!(
            "binary: package_root {} {why}",
            quote_untrusted(root)
        ))
    };
    let segments: Vec<&str> = root.split('/').collect();
    let plain = |segment: &&str| {
        !segment.is_empty()
            && *segment != "."
            && *segment != ".."
            && segment.len() <= 128
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    if root.len() > 512 || !segments.iter().all(plain) {
        return refused(
            "must be a repository-relative path of plain segments ([A-Za-z0-9._-], no `.`, `..`, empty, absolute or backslash parts)",
        );
    }
    if segments.len() >= 3 && segments[0] == "plugins" && segments[2] == crate::BIN_DIR {
        return refused("lies inside a plugins/*/bin/ directory");
    }
    Ok(())
}

/// Lower-case plugin or binary name: `[a-z0-9][a-z0-9_-]{0,63}`.
#[must_use]
pub fn is_name(value: &str) -> bool {
    value.len() <= 64
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn is_token(value: &str, extra: &[u8]) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
}

/// A reason embedded verbatim in a single-quoted `sh -c` script, a `printf`
/// argument and a JSON string, so it may contain no quoting or format
/// characters.
fn is_safe_reason(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 300
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" .,:;()-_/".contains(&b))
}

pub(crate) fn object<'a>(value: &'a Json, what: &str) -> Result<&'a [(String, Json)], String> {
    match value {
        Json::Object(pairs) => Ok(pairs),
        other => Err(format!(
            "{what}: expected an object, found {}",
            other.type_name()
        )),
    }
}

pub(crate) fn reject_unknown(
    pairs: &[(String, Json)],
    allowed: &[&str],
    what: &str,
) -> Result<(), String> {
    match pairs
        .iter()
        .find(|(key, _)| !allowed.contains(&key.as_str()))
    {
        Some((key, _)) => Err(format!("{what}: unknown field {key:?}")),
        None => Ok(()),
    }
}

pub(crate) fn required_str<'a>(value: &'a Json, key: &str, what: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Json::as_str)
        .ok_or_else(|| format!("{what}: `{key}` must be a string"))
}

pub(crate) fn optional_str<'a>(
    value: &'a Json,
    key: &str,
    what: &str,
) -> Result<Option<&'a str>, String> {
    match value.get(key) {
        None => Ok(None),
        Some(Json::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("{what}: `{key}` must be a string")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r##"{
      "schema": 1,
      "platforms": [
        {"platform": "linux-x86_64", "triple": "x86_64-unknown-linux-gnu", "glibc": "2.17"},
        {"platform": "windows-x86_64", "triple": "x86_64-pc-windows-gnu"}
      ],
      "binaries": [
        {"plugin": "aws-core", "name": "guard", "package": "guard-pkg", "bin": "guard",
         "role": "guard-hook", "deny_reason": "Guard is unavailable. Reinstall.", "consumer": "#1"},
        {"plugin": "aws-core", "name": "toolkit", "package": "toolkit-cli", "bin": "toolkit",
         "features": ["toolkit-aws/sdk"], "role": "mcp-server", "consumer": "#2"}
      ]
    }"##;

    fn with(from: &str, to: &str) -> Result<Declaration, String> {
        assert!(VALID.contains(from), "fixture lacks {from}");
        Declaration::parse(&VALID.replacen(from, to, 1))
    }

    #[test]
    fn a_valid_declaration_parses() {
        let declaration = Declaration::parse(VALID).expect("valid");
        assert_eq!(declaration.platforms.len(), 2);
        assert_eq!(declaration.platforms[0].glibc.as_deref(), Some("2.17"));
        let guard = declaration.binary("aws-core", "guard").expect("guard");
        assert_eq!(
            guard.role,
            Role::GuardHook {
                deny_reason: "Guard is unavailable. Reinstall.".to_string()
            }
        );
        assert_eq!(
            declaration
                .binary("aws-core", "toolkit")
                .expect("toolkit")
                .features,
            vec!["toolkit-aws/sdk".to_string()]
        );
        assert_eq!(
            declaration.plugins().into_iter().collect::<Vec<_>>(),
            ["aws-core"]
        );
    }

    #[test]
    fn the_committed_declaration_is_valid() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(DECLARATION_PATH);
        Declaration::load(&path).expect("committed declaration");
    }

    #[test]
    fn invalid_declarations_are_refused_with_a_reason() {
        let cases = [
            ("\"schema\": 1", "\"schema\": 2", "schema"),
            (
                "\"linux-x86_64\", \"triple\"",
                "\"linux-armv7\", \"triple\"",
                "unknown key",
            ),
            (
                "x86_64-unknown-linux-gnu",
                "aarch64-unknown-linux-gnu",
                "not a linux x86_64",
            ),
            ("\"2.17\"", "\"2\"", "MAJOR.MINOR"),
            (
                "\"x86_64-pc-windows-gnu\"}",
                "\"x86_64-pc-windows-gnu\", \"glibc\": \"2.17\"}",
                "only valid for Linux",
            ),
            (
                "\"name\": \"guard\"",
                "\"name\": \"../guard\"",
                "lower-case",
            ),
            (
                "\"name\": \"guard\"",
                "\"name\": \"linux-x86_64\"",
                "platform directory",
            ),
            (
                "\"package\": \"guard-pkg\"",
                "\"package\": \"-x\"",
                "cargo target names",
            ),
            (
                "\"deny_reason\": \"Guard is unavailable. Reinstall.\"",
                "\"deny_reason\": \"it's $HOME\"",
                "deny_reason",
            ),
            (
                ", \"deny_reason\": \"Guard is unavailable. Reinstall.\"",
                "",
                "needs `deny_reason`",
            ),
            (
                "\"role\": \"mcp-server\"",
                "\"role\": \"mcp-server\", \"deny_reason\": \"x\"",
                "only valid for a guard-hook",
            ),
            (
                "\"role\": \"mcp-server\"",
                "\"role\": \"daemon\"",
                "not guard-hook",
            ),
            (
                "\"consumer\": \"#2\"",
                "\"consumer\": \"#2\", \"extra\": true",
                "unknown field",
            ),
            (
                "\"name\": \"toolkit\"",
                "\"name\": \"guard\"",
                "listed twice",
            ),
            ("[\"toolkit-aws/sdk\"]", "[\"a b\"]", "feature"),
        ];
        for (from, to, needle) in cases {
            let error = with(from, to).expect_err(to);
            assert!(error.contains(needle), "{to}: {error}");
        }
    }

    #[test]
    fn names_are_strict() {
        assert!(is_name("secret-safety"));
        assert!(is_name("a_1"));
        assert!(!is_name(""));
        assert!(!is_name("-x"));
        assert!(!is_name("A"));
        assert!(!is_name("a.b"));
        assert!(!is_name("a/b"));
    }
}
