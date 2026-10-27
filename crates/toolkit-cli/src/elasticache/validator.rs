//! `toolkit elasticache validate-input` (port of `input_validator.py`):
//! validates user-supplied identifiers before they are interpolated into CLI
//! commands.
//!
//! Every check returns `Err(message)`; no message contains the rejected value
//! (only its length), so a hostile value never reaches a terminal or a log.
//! Deliberate difference from the Python: the regexes ended in `$`, which also
//! matches before a trailing newline, so `"my-cache\n"` was accepted; here the
//! whole value must match, and `\d` means ASCII digits only.

use toolkit_core::Json;

/// Outcome of one check.
pub type Check = Result<(), String>;

/// Recognized parameter keys, sorted (the usage text lists them).
pub const RECOGNIZED_KEYS: &[&str] = &[
    "cache_name",
    "engine_version",
    "kms_key_id",
    "region",
    "replication_group_id",
    "security_group_id",
    "snapshot_name",
    "subnet_id",
    "vpc_id",
];

fn all_chars(value: &str, allowed: impl Fn(char) -> bool) -> bool {
    value.chars().all(allowed)
}

fn lower_alnum_hyphen(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
}

fn alnum_hyphen(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

fn first_char(value: &str, test: impl Fn(char) -> bool) -> bool {
    value.chars().next().is_some_and(test)
}

/// Validates a cache or cluster name: 1-40 characters, lowercase letters,
/// digits and hyphens, starting with a letter, no `--`, no trailing hyphen.
pub fn validate_cache_name(name: &str) -> Check {
    if name.is_empty() {
        return Err("Cache name must not be empty".to_string());
    }
    let length = name.chars().count();
    if length > 40 {
        return Err(format!("Cache name must be 1-40 characters (got {length})"));
    }
    if !first_char(name, |c| c.is_ascii_lowercase()) {
        return Err("Cache name must start with a lowercase letter (serverless requires lowercase; node-based is case-insensitive but lowercased by AWS)".to_string());
    }
    if !all_chars(name, lower_alnum_hyphen) {
        return Err(
            "Cache name may only contain lowercase alphanumeric characters and hyphens".to_string(),
        );
    }
    if name.contains("--") {
        return Err("Cache name must not contain consecutive hyphens".to_string());
    }
    if name.ends_with('-') {
        return Err("Cache name must not end with a hyphen".to_string());
    }
    Ok(())
}

/// Validates a replication group ID: like a cache name, but upper case letters
/// are allowed.
pub fn validate_replication_group_id(rg_id: &str) -> Check {
    if rg_id.is_empty() {
        return Err("Replication group ID must not be empty".to_string());
    }
    let length = rg_id.chars().count();
    if length > 40 {
        return Err(format!(
            "Replication group ID must be 1-40 characters (got {length})"
        ));
    }
    if !first_char(rg_id, |c| c.is_ascii_alphabetic()) {
        return Err("Replication group ID must start with a letter".to_string());
    }
    if !all_chars(rg_id, alnum_hyphen) {
        return Err(
            "Replication group ID may only contain alphanumeric characters and hyphens".to_string(),
        );
    }
    if rg_id.contains("--") {
        return Err("Replication group ID must not contain consecutive hyphens".to_string());
    }
    if rg_id.ends_with('-') {
        return Err("Replication group ID must not end with a hyphen".to_string());
    }
    Ok(())
}

/// `prefix` followed by 8 to 17 lowercase hex digits.
fn prefixed_hex(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|hex| {
        (8..=17).contains(&hex.len())
            && hex
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    })
}

/// Validates a subnet ID (`subnet-` and 8-17 hex digits).
pub fn validate_subnet_id(subnet_id: &str) -> Check {
    if subnet_id.is_empty() {
        return Err("Subnet ID must not be empty".to_string());
    }
    if !prefixed_hex(subnet_id, "subnet-") {
        return Err("Subnet ID must match pattern subnet-<8-17 hex characters> (e.g. subnet-0abc1234def56789a)".to_string());
    }
    Ok(())
}

/// Validates a security group ID (`sg-` and 8-17 hex digits).
pub fn validate_security_group_id(sg_id: &str) -> Check {
    if sg_id.is_empty() {
        return Err("Security group ID must not be empty".to_string());
    }
    if !prefixed_hex(sg_id, "sg-") {
        return Err("Security group ID must match pattern sg-<8-17 hex characters> (e.g. sg-0abc1234def56789a)".to_string());
    }
    Ok(())
}

/// Validates a VPC ID (`vpc-` and 8-17 hex digits).
pub fn validate_vpc_id(vpc_id: &str) -> Check {
    if vpc_id.is_empty() {
        return Err("VPC ID must not be empty".to_string());
    }
    if !prefixed_hex(vpc_id, "vpc-") {
        return Err(
            "VPC ID must match pattern vpc-<8-17 hex characters> (e.g. vpc-0abc1234def56789a)"
                .to_string(),
        );
    }
    Ok(())
}

/// Validates a region code such as `us-east-1`.
pub fn validate_region(region: &str) -> Check {
    if region.is_empty() {
        return Err("Region must not be empty".to_string());
    }
    let parts: Vec<&str> = region.split('-').collect();
    let valid = parts.len() == 3
        && parts[0].len() == 2
        && parts[0].chars().all(|c| c.is_ascii_lowercase())
        && !parts[1].is_empty()
        && parts[1].chars().all(|c| c.is_ascii_lowercase())
        && !parts[2].is_empty()
        && parts[2].chars().all(|c| c.is_ascii_digit());
    if valid {
        Ok(())
    } else {
        Err("Region must match AWS region pattern (e.g. us-east-1, eu-west-2)".to_string())
    }
}

/// Validates an engine version: `7`, `7.1` or `7.0.4`.
pub fn validate_engine_version(version: &str) -> Check {
    if version.is_empty() {
        return Err("Engine version must not be empty".to_string());
    }
    let parts: Vec<&str> = version.split('.').collect();
    let valid = (1..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
    if valid {
        Ok(())
    } else {
        Err(
            "Engine version must match pattern <major>[.<minor>[.<patch>]] (e.g. 7, 7.1, 7.0.4)"
                .to_string(),
        )
    }
}

fn is_lower_hex(c: char) -> bool {
    c.is_ascii_digit() || ('a'..='f').contains(&c)
}

fn is_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    let widths = [8, 4, 4, 4, 12];
    groups.len() == widths.len()
        && groups
            .iter()
            .zip(widths)
            .all(|(group, width)| group.len() == width && group.chars().all(is_lower_hex))
}

fn is_kms_arn(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("arn:aws:kms:") else {
        return false;
    };
    let Some((region, rest)) = rest.split_once(':') else {
        return false;
    };
    if region.is_empty()
        || !region
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return false;
    }
    let Some((account, resource)) = rest.split_once(':') else {
        return false;
    };
    if account.len() != 12 || !account.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let Some(name) = resource
        .strip_prefix("key/")
        .or_else(|| resource.strip_prefix("alias/"))
    else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '_' || c == '-')
}

/// Validates a KMS key: a UUID, or a key or alias ARN.
pub fn validate_kms_key_id(key_id: &str) -> Check {
    if key_id.is_empty() {
        return Err("KMS key ID must not be empty".to_string());
    }
    if is_uuid(key_id) || is_kms_arn(key_id) {
        return Ok(());
    }
    Err("KMS key ID must be a UUID (e.g. 12345678-1234-1234-1234-123456789abc) or a KMS ARN (e.g. arn:aws:kms:us-east-1:123456789012:key/<key-id>)".to_string())
}

/// Validates a snapshot name: 1-255 characters for serverless snapshots, 1-40
/// for node-based ones; letters, digits and hyphens, starting with a letter.
pub fn validate_snapshot_name(name: &str, serverless: bool) -> Check {
    if name.is_empty() {
        return Err("Snapshot name must not be empty".to_string());
    }
    let max_len = if serverless { 255 } else { 40 };
    let length = name.chars().count();
    if length > max_len {
        let label = if serverless {
            "serverless"
        } else {
            "node-based"
        };
        return Err(format!(
            "Snapshot name must be 1-{max_len} characters for {label} (got {length})"
        ));
    }
    if !first_char(name, |c| c.is_ascii_alphabetic()) {
        return Err("Snapshot name must start with a letter".to_string());
    }
    if !all_chars(name, alnum_hyphen) {
        return Err(
            "Snapshot name may only contain alphanumeric characters and hyphens".to_string(),
        );
    }
    if name.contains("--") {
        return Err("Snapshot name must not contain consecutive hyphens".to_string());
    }
    if name.ends_with('-') {
        return Err("Snapshot name must not end with a hyphen".to_string());
    }
    Ok(())
}

fn validator_for(key: &str) -> Option<fn(&str) -> Check> {
    fn snapshot(name: &str) -> Check {
        validate_snapshot_name(name, false)
    }
    let validator: fn(&str) -> Check = match key {
        "cache_name" => validate_cache_name,
        "replication_group_id" => validate_replication_group_id,
        "subnet_id" => validate_subnet_id,
        "security_group_id" => validate_security_group_id,
        "region" => validate_region,
        "engine_version" => validate_engine_version,
        "vpc_id" => validate_vpc_id,
        "kms_key_id" => validate_kms_key_id,
        "snapshot_name" => snapshot,
        _ => return None,
    };
    Some(validator)
}

/// Python type name of a JSON value, for the "expected a string" message.
fn python_type_name(value: &Json) -> &'static str {
    match value {
        Json::Null => "NoneType",
        Json::Bool(_) => "bool",
        Json::Int(_) => "int",
        Json::Float(_) => "float",
        Json::String(_) => "str",
        Json::Array(_) => "list",
        Json::Object(_) => "dict",
    }
}

/// Validates every recognized key; unknown keys are skipped. Returns one
/// message per problem, in parameter order; empty means all valid.
#[must_use]
pub fn validate_all(params: &[(String, Json)]) -> Vec<String> {
    let mut errors = Vec::new();
    for (key, value) in params {
        let Some(validator) = validator_for(key) else {
            continue;
        };
        let Json::String(text) = value else {
            errors.push(format!(
                "{key}: expected a string, got {}",
                python_type_name(value)
            ));
            continue;
        };
        if let Err(message) = validator(text) {
            errors.push(format!("{key}: {message}"));
        }
    }
    errors
}
