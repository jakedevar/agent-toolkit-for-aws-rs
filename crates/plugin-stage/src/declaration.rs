//! The committed binary declaration, `packaging/plugin-binaries.json`.
//!
//! It is the source of truth the launcher contract resolves against: which
//! binaries each plugin ships, which workspace package and bin builds each
//! one, the platforms (and Rust target triples) the pipeline builds, and how
//! each binary may be launched.

use std::collections::BTreeSet;
use std::path::Path;

use toolkit_core::{parse_json, Json};

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
}

impl Role {
    /// The declaration spelling of the role.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GuardHook { .. } => "guard-hook",
            Self::McpServer => "mcp-server",
            Self::Hook => "hook",
        }
    }
}

/// One binary a plugin ships.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinarySpec {
    /// Plugin directory name under `plugins/`.
    pub plugin: String,
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
            if !names.insert((binary.plugin.as_str(), binary.name.as_str())) {
                return Err(format!(
                    "declaration: binary {}/{} is listed twice",
                    binary.plugin, binary.name
                ));
            }
        }
        Ok(())
    }

    /// The declared spec of one platform.
    #[must_use]
    pub fn platform(&self, platform: Platform) -> Option<&PlatformSpec> {
        self.platforms.iter().find(|spec| spec.platform == platform)
    }

    /// The declared binary `name` of `plugin`.
    #[must_use]
    pub fn binary(&self, plugin: &str, name: &str) -> Option<&BinarySpec> {
        self.binaries
            .iter()
            .find(|binary| binary.plugin == plugin && binary.name == name)
    }

    /// The declared binaries of one plugin.
    pub fn plugin_binaries<'a>(&'a self, plugin: &'a str) -> impl Iterator<Item = &'a BinarySpec> {
        self.binaries
            .iter()
            .filter(move |binary| binary.plugin == plugin)
    }

    /// Distinct plugins that ship at least one binary.
    #[must_use]
    pub fn plugins(&self) -> BTreeSet<&str> {
        self.binaries
            .iter()
            .map(|binary| binary.plugin.as_str())
            .collect()
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

fn parse_binary(value: &Json) -> Result<BinarySpec, String> {
    let pairs = object(value, "binary")?;
    reject_unknown(
        pairs,
        &[
            "plugin",
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
    let plugin = required_str(value, "plugin", "binary")?;
    let name = required_str(value, "name", "binary")?;
    let label = format!("binary {plugin}/{name}");
    if !is_name(plugin) || !is_name(name) {
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
                _ => {
                    return Err(format!(
                        "{label}: role {other:?} is not guard-hook, mcp-server or hook"
                    ))
                }
            }
        }
    };
    let consumer = required_str(value, "consumer", &label)?;
    Ok(BinarySpec {
        plugin: plugin.to_string(),
        name: name.to_string(),
        package: package.to_string(),
        bin: bin.to_string(),
        features,
        role,
        consumer: consumer.to_string(),
    })
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
