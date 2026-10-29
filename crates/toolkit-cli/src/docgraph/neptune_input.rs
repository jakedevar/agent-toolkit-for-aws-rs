//! `toolkit neptune validate-input` (port of the Neptune skill's
//! `input_validator.py`): validates user-supplied identifiers before they
//! reach a create, modify or delete call.
//!
//! The messages are those of the script. Deliberate differences, recorded in
//! `docs/migration/docgraph.md`: the Python patterns ended in `$`, which also
//! matches before a trailing newline, so `"prod\n"` was accepted; here the
//! whole value must match. `\d` meant any Unicode digit; here ASCII digits.
//! A value shown in a message is escaped (control, bidirectional and invisible
//! characters) and bounded after the whole value went through the shared
//! redactor. The region rule is the shared `toolkit_aws::is_valid_region` (it
//! also bounds the length at 32).

use toolkit_aws::is_valid_region;

use crate::aurora::args::parse_py_int;
use crate::safe_text::safe_text;

/// `MIN_PROVISIONED_MEMORY` and `MAX_PROVISIONED_MEMORY` (m-NCU).
const MIN_MEMORY: i64 = 16;
const MAX_MEMORY: i64 = 24576;

/// Scalar keys, then list keys; each group sorted.
const SCALAR_KEYS: [&str; 9] = [
    "cluster_id",
    "graph_id",
    "region",
    "vpc_id",
    "kms_key",
    "iam_role",
    "s3_uri",
    "snapshot_id",
    "provisioned_memory",
];
const LIST_KEYS: [&str; 2] = ["subnet_ids", "security_group_ids"];

/// `sorted(list(VALIDATORS) + list(LIST_VALIDATORS))` as Python prints it.
pub(super) fn valid_keys_repr() -> String {
    let mut keys: Vec<&str> = SCALAR_KEYS
        .iter()
        .chain(LIST_KEYS.iter())
        .copied()
        .collect();
    keys.sort_unstable();
    let quoted: Vec<String> = keys.iter().map(|key| format!("'{key}'")).collect();
    format!("[{}]", quoted.join(", "))
}

fn all(value: &str, test: impl Fn(char) -> bool) -> bool {
    value.chars().all(test)
}

fn lower_alnum(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}

fn hex(c: char) -> bool {
    c.is_ascii_digit() || ('a'..='f').contains(&c)
}

fn hex_or_hyphen(c: char) -> bool {
    hex(c) || c == '-'
}

fn alnum_hyphen(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

fn starts_with_letter(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
}

/// `^[a-zA-Z][a-zA-Z0-9\-]{0,max-1}$`.
pub(super) fn identifier(value: &str, max: usize) -> bool {
    starts_with_letter(value) && value.chars().count() <= max && all(value, alnum_hyphen)
}

/// `^<prefix>[0-9a-f]{8,17}$`.
fn ec2_id(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|rest| (8..=17).contains(&rest.len()) && all(rest, hex))
}

/// `arn:aws(?:-cn|-us-gov)?:` and what follows it.
fn after_arn_prefix(value: &str) -> Option<&str> {
    let rest = value.strip_prefix("arn:aws")?;
    rest.strip_prefix("-cn:")
        .or_else(|| rest.strip_prefix("-us-gov:"))
        .or_else(|| rest.strip_prefix(':'))
}

fn twelve_digits(value: &str) -> bool {
    value.len() == 12 && all(value, |c| c.is_ascii_digit())
}

fn kms_key(value: &str) -> bool {
    if value.len() == 36 && all(value, hex_or_hyphen) {
        return true;
    }
    let Some(rest) = after_arn_prefix(value).and_then(|rest| rest.strip_prefix("kms:")) else {
        return false;
    };
    let parts: Vec<&str> = rest.split(':').collect();
    let [region, account, key] = parts.as_slice() else {
        return false;
    };
    !region.is_empty()
        && all(region, |c| lower_alnum(c) || c == '-')
        && twelve_digits(account)
        && key
            .strip_prefix("key/")
            .is_some_and(|id| !id.is_empty() && all(id, hex_or_hyphen))
}

fn iam_role(value: &str) -> bool {
    let Some(rest) = after_arn_prefix(value).and_then(|rest| rest.strip_prefix("iam::")) else {
        return false;
    };
    let Some((account, role)) = rest.split_once(':') else {
        return false;
    };
    twelve_digits(account)
        && role
            .strip_prefix("role/")
            .is_some_and(|name| !name.is_empty() && !name.contains('\n'))
}

fn s3_uri(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("s3://") else {
        return false;
    };
    let (bucket, path) = match rest.split_once('/') {
        Some((bucket, path)) => (bucket, Some(path)),
        None => (rest, None),
    };
    let edge = |c: char| lower_alnum(c);
    let bucket_ok = (3..=63).contains(&bucket.len())
        && bucket.chars().next().is_some_and(edge)
        && bucket.chars().last().is_some_and(edge)
        && all(bucket, |c| lower_alnum(c) || c == '.' || c == '-');
    bucket_ok && path.is_none_or(|path| !path.is_empty() && !path.contains('\n'))
}

fn scalar_error(key: &str, value: &str) -> Option<String> {
    let v = safe_text(value);
    match key {
        "cluster_id" => (!identifier(value, 63)).then(|| {
            format!("cluster_id '{v}' invalid. Must start with letter, alphanumeric + hyphens, max 63 chars.")
        }),
        "graph_id" => {
            let ok = value
                .strip_prefix("g-")
                .is_some_and(|rest| rest.len() >= 10 && all(rest, lower_alnum));
            (!ok).then(|| {
                format!("graph_id '{v}' invalid. Must match 'g-' followed by lowercase alphanumeric (e.g., g-abc123def4).")
            })
        }
        "region" => (!is_valid_region(value)).then(|| {
            format!("region '{v}' invalid. Expected format: us-east-1, eu-west-2, etc.")
        }),
        "vpc_id" => (!ec2_id(value, "vpc-")).then(|| {
            format!("vpc_id '{v}' invalid. Expected format: vpc-0abc12345def67890.")
        }),
        "kms_key" => (!kms_key(value))
            .then(|| format!("kms_key '{v}' invalid. Expected KMS key ARN or UUID.")),
        "iam_role" => (!iam_role(value)).then(|| {
            format!("iam_role '{v}' invalid. Expected format: arn:aws:iam::123456789012:role/RoleName.")
        }),
        "s3_uri" => (!s3_uri(value)).then(|| {
            format!("s3_uri '{v}' invalid. Expected format: s3://bucket-name/path/.")
        }),
        "snapshot_id" => (!identifier(value, 255)).then(|| {
            format!("snapshot_id '{v}' invalid. Must start with letter, max 255 chars.")
        }),
        "provisioned_memory" => match parse_py_int(value) {
            None => Some(format!(
                "provisioned_memory '{v}' invalid. Must be an integer."
            )),
            Some(memory) if !(MIN_MEMORY..=MAX_MEMORY).contains(&memory) => Some(format!(
                "provisioned_memory {memory} invalid. Valid range: {MIN_MEMORY}-{MAX_MEMORY} m-NCU."
            )),
            Some(_) => None,
        },
        _ => None,
    }
}

fn list_errors(key: &str, value: &str) -> Vec<String> {
    let (prefix, label, example) = if key == "subnet_ids" {
        ("subnet-", "subnet_id", "subnet-0abc12345def67890")
    } else {
        ("sg-", "security_group_id", "sg-0abc12345def67890")
    };
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !ec2_id(item, prefix))
        .map(|item| {
            format!(
                "{label} '{}' invalid. Expected format: {example}.",
                safe_text(item)
            )
        })
        .collect()
}

/// `validate_all(**kwargs)`: the errors, in the order of the keys.
pub(super) fn validate_all(pairs: &[(String, String)]) -> Vec<String> {
    let mut errors = Vec::new();
    for (key, value) in pairs {
        if SCALAR_KEYS.contains(&key.as_str()) {
            errors.extend(scalar_error(key, value));
        } else if LIST_KEYS.contains(&key.as_str()) {
            errors.extend(list_errors(key, value));
        } else {
            errors.push(format!(
                "Unknown validator key: '{}'. Valid keys: {}",
                safe_text(key),
                valid_keys_repr()
            ));
        }
    }
    errors
}
