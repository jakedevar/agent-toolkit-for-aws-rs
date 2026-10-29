//! Trusted runtime configuration and bounded environment fallbacks.
use super::{amount::CapDecimal, pyval};
use crate::safe_text::{shown_path, shown_value};
use std::path::{Path, PathBuf};
use toolkit_core::{parse_json, Json};

/// Resolve the current account's home independently of caller-controlled environment.
///
/// # Errors
/// Returns an error when account lookup fails or the home is relative.
#[cfg(unix)]
pub fn runtime_config_path() -> Result<PathBuf, String> {
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .map_err(|_| {
            "The current uid has no OS account home directory. Payments are refused.".to_owned()
        })?
        .ok_or_else(|| {
            "The current uid has no OS account home directory. Payments are refused.".to_owned()
        })?;
    if !user.dir.is_absolute() {
        return Err(
            "The OS account home directory is not absolute. Payments are refused.".to_owned(),
        );
    }
    Ok(user.dir.join(".agents-pay/config.json"))
}
/// Resolve the current account's home independently of caller-controlled environment.
///
/// # Errors
/// Always returns an error on this platform.
#[cfg(not(unix))]
pub fn runtime_config_path() -> Result<PathBuf, String> {
    Err("Cannot resolve the runtime payment config from the OS account on this platform. Payments are refused.".to_owned())
}

/// Load a checked config file.
///
/// # Errors
/// Returns an error if the file or policy fails validation.
#[cfg(unix)]
pub fn load_config(path: &Path) -> Result<Json, String> {
    load_config_with_hook(path, || {})
}
/// Load a config with a hook after the parent descriptor is checked, for race testing.
///
/// # Errors
/// Returns an error if the file or policy fails validation.
#[cfg(unix)]
#[doc(hidden)]
pub fn load_config_with_hook(path: &Path, hook: impl FnOnce()) -> Result<Json, String> {
    let mut uid = None;
    load_config_with_owner(path, hook, || {
        *uid.get_or_insert_with(|| rustix::process::getuid().as_raw())
    })
}

/// Load a config with an injected expected owner for each ownership check.
///
/// # Errors
/// Returns an error if the file or policy fails validation.
#[cfg(unix)]
#[doc(hidden)]
pub fn load_config_with_owner(
    path: &Path,
    hook: impl FnOnce(),
    mut expected_owner: impl FnMut() -> u32,
) -> Result<Json, String> {
    use rustix::fs::{fstat, open, openat, FileType, Mode, OFlags};
    use std::io::Read;
    let display = shown_path(path);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| format!("Cannot stat the directory containing {display}."))?;
    let dir = open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| {
        if e == rustix::io::Errno::NOENT {
            missing(&display)
        } else {
            format!("Cannot stat the directory containing {display}.")
        }
    })?;
    let ds = fstat(&dir).map_err(|_| format!("Cannot stat the directory containing {display}."))?;
    if ds.st_uid != expected_owner() {
        return Err(format!(
            "Directory {} is not owned by the current user.",
            shown_path(parent)
        ));
    }
    if ds.st_mode & 0o022 != 0 {
        return Err(format!("Directory {} is group/world-writable ({}); another user could replace the config file. Run: chmod 700 {}", shown_path(parent), filemode(ds.st_mode), shown_value(&parent.to_string_lossy())));
    }
    hook();
    let fd = openat(
        &dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| {
        if e == rustix::io::Errno::LOOP {
            format!("Payment config {display} is a symlink; refusing to follow it.")
        } else if e == rustix::io::Errno::NOENT {
            missing(&display)
        } else {
            format!("Cannot open payment config {display}: {e}")
        }
    })?;
    let st = fstat(&fd).map_err(|e| format!("Cannot stat payment config {display}: {e}"))?;
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
        return Err(format!("Payment config {display} is not a regular file."));
    }
    if st.st_uid != expected_owner() {
        return Err(format!(
            "Payment config {display} is not owned by the current user."
        ));
    }
    if st.st_mode & 0o077 != 0 {
        return Err(format!(
            "Payment config {display} has mode {}; expected 0600. Run: chmod 600 {}",
            filemode(st.st_mode),
            shown_value(&path.to_string_lossy())
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::from(fd)
        .take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Cannot read payment config {display}: {e}"))?;
    if bytes.len() > 1_048_576 {
        return Err(format!("Payment config {display} is too large."));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("Payment config {display} is not valid JSON: invalid UTF-8"))?;
    let raw =
        parse_json(text).map_err(|e| format!("Payment config {display} is not valid JSON: {e}"))?;
    normalize(&raw, &display)
}
#[cfg(not(unix))]
/// This platform cannot securely open runtime payment configs.
///
/// # Errors
/// Always returns an error.
pub fn load_config(_path: &Path) -> Result<Json, String> {
    Err("Cannot resolve the runtime payment config from the OS account on this platform. Payments are refused.".to_owned())
}
#[cfg(unix)]
fn missing(path: &str) -> String {
    format!("No payment config at {path}. Payments are refused until an operator creates one (agents_pay_admin.py init-config). This is intentional: there is no permissive default.")
}
/// Python `stat.filemode`: the type character, then the nine permission
/// characters with the set-id and sticky bits.
#[must_use]
pub(crate) fn filemode(mode: u32) -> String {
    let mut s = String::from(match mode & 0o170_000 {
        0o040_000 => "d",
        0o120_000 => "l",
        0o020_000 => "c",
        0o060_000 => "b",
        0o010_000 => "p",
        0o140_000 => "s",
        _ => "-",
    });
    for (i, c) in ['r', 'w', 'x', 'r', 'w', 'x', 'r', 'w', 'x']
        .into_iter()
        .enumerate()
    {
        let executable = mode & (1 << (8 - i)) != 0;
        let special = match i {
            2 => Some((0o4000, 's', 'S')),
            5 => Some((0o2000, 's', 'S')),
            8 => Some((0o1000, 't', 'T')),
            _ => None,
        };
        s.push(match special {
            Some((bit, lower, upper)) if mode & bit != 0 => {
                if executable {
                    lower
                } else {
                    upper
                }
            }
            _ if executable => c,
            _ => '-',
        });
    }
    s
}
// recorded-shape: operator configuration file is not an AWS response.
fn normalize(raw: &Json, path: &str) -> Result<Json, String> {
    if !matches!(raw, Json::Object(_)) {
        return Err(format!("Payment config {path} must be a JSON object."));
    }
    let resources = match raw.get("resources") {
        Some(v @ Json::Object(_)) => v.clone(),
        _ => Json::Object(vec![]),
    };
    let policy = match raw.get("policy") {
        Some(v @ Json::Object(_)) => v.clone(),
        _ => (*raw).clone(),
    };
    let Json::Object(mut fields) = policy else {
        unreachable!()
    };
    let (key, value) = fields.iter().find(|(k, _)| k == "max_per_payment_usd").or_else(|| fields.iter().find(|(k, _)| k == "max_amount_usd")).map(|(k,v)| (k.clone(), v.clone())).ok_or_else(|| "Payment policy is missing 'max_per_payment_usd'. This is the PER-PAYMENT ceiling and is not the same thing as the session budget: the session budget is cumulative, so without a per-payment cap a single hostile challenge can drain the whole budget in one transaction.".to_owned())?;
    let cap = CapDecimal::from_json(&value)
        .map_err(|_| format!("Payment policy '{key}' is not a decimal number."))?;
    if !cap.is_positive() {
        return Err(format!("Payment policy '{key}' must be greater than zero."));
    }
    fields.retain(|(k, _)| k != "max_per_payment_usd" && k != "_resources");
    fields.push(("max_per_payment_usd".into(), value));
    fields.push(("_resources".into(), resources));
    Ok(Json::Object(fields))
}
/// Resolve a resource with the file winning over injected environment values.
///
/// # Errors
/// Returns an error for a truthy non-scalar resource.
pub fn resolve_resource(
    policy: &Json,
    key: &str,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Option<String>, String> {
    if let Some(value) = policy
        .get("_resources")
        .and_then(|v| v.get(key))
        .filter(|v| pyval::truthy(v))
    {
        return pyval::py_str(value)
            .map(Some)
            .ok_or_else(|| format!("Payment config resource '{key}' must be a string."));
    }
    let name = match key {
        "payment_manager_arn" => "PAYMENT_MANAGER_ARN",
        "payment_instrument_id" => "PAYMENT_INSTRUMENT_ID",
        "payment_session_id" => "PAYMENT_SESSION_ID",
        "user_id" => "PAYMENT_USER_ID",
        "region" => "AWS_REGION",
        _ => return Ok(None),
    };
    Ok(env(name))
}
/// Parse and bound a Python-style float tunable; non-finite values fall back.
#[must_use]
pub fn bounded_env(raw: Option<&str>, default: f64, min: f64, max: f64) -> f64 {
    let Some(raw) = raw else { return default };
    let raw = raw.trim();
    let bytes = raw.as_bytes();
    if bytes.iter().enumerate().any(|(i, b)| {
        *b == b'_'
            && (i == 0
                || i + 1 == bytes.len()
                || !bytes[i - 1].is_ascii_digit()
                || !bytes[i + 1].is_ascii_digit())
    }) {
        return default;
    }
    let cleaned = raw.replace('_', "");
    match cleaned.parse::<f64>() {
        Ok(v) if v.is_finite() && v >= min && v <= max => v,
        _ => default,
    }
}

/// Load the runtime account's configuration without consulting path environment variables.
///
/// # Errors
/// Returns an error if account lookup or config validation fails.
pub fn load_runtime_config() -> Result<Json, String> {
    load_config(&runtime_config_path()?)
}

/// Resolve a required resource or explain how to provision it.
///
/// # Errors
/// Returns an error when the resource is missing or invalid.
pub fn require_resource(
    policy: &Json,
    key: &str,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    match resolve_resource(policy, key, env)? {
        Some(value) if !value.is_empty() => Ok(value),
        _ => {
            let name = match key {
                "payment_manager_arn" => "PAYMENT_MANAGER_ARN".to_owned(),
                "payment_instrument_id" => "PAYMENT_INSTRUMENT_ID".to_owned(),
                "payment_session_id" => "PAYMENT_SESSION_ID".to_owned(),
                "user_id" => "PAYMENT_USER_ID".to_owned(),
                "region" => "AWS_REGION".to_owned(),
                _ => key.to_uppercase(),
            };
            Err(format!("Missing payment resource '{key}'. Add it to the config (agents_pay_admin.py init-config) or set {name}. Provision resources with the trusted setup path first."))
        }
    }
}

/// Runtime bounds for HTTP and payment retry operations.
#[derive(Debug, PartialEq)]
pub struct Tunables {
    pub max_body_bytes: usize,
    pub timeout_seconds: f64,
    pub max_payment_attempts: usize,
}
impl Tunables {
    /// Read the named values from an injected environment source.
    #[must_use]
    // Values are bounded positive and truncation matches Python int().
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn from_env(env: &impl Fn(&str) -> Option<String>) -> Self {
        Self {
            max_body_bytes: bounded_env(
                env("X402_MAX_BODY_BYTES").as_deref(),
                262_144.0,
                1024.0,
                67_108_864.0,
            ) as usize,
            timeout_seconds: bounded_env(env("X402_TIMEOUT_SECONDS").as_deref(), 20.0, 1.0, 120.0),
            max_payment_attempts: bounded_env(
                env("X402_MAX_PAYMENT_ATTEMPTS").as_deref(),
                5.0,
                1.0,
                10.0,
            ) as usize,
        }
    }
}
