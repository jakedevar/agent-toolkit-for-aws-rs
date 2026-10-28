//! Names and identities that reach an IAM policy, an ARN or a shell script.
//!
//! The scripts only required the resource prefix to start with
//! `ddb-skill-bench-`. The prefix is interpolated into IAM resource ARNs
//! (`table/<prefix>*`) and into the teardown guard, so a `*`, `/` or quote in
//! it would widen the grant or break out of the script. These checks are a
//! deliberate tightening, recorded in `docs/migration/dynamodb-deploy.md`.
//!
//! No check here echoes the rejected value: it can come from a recorded
//! document or a run manifest, where it may be a credential or other secret
//! text (escaping is not redaction). A diagnostic names the field and the shape
//! it must have, nothing else.

use toolkit_core::{is_unsafe_char, ToolkitError};

use super::model::die;

/// The namespace every resource of a benchmark run carries.
pub(crate) const REQUIRED_PREFIX: &str = "ddb-skill-bench-";
/// Caller-identity substrings that make the preflight refuse.
pub(crate) const PROD_MARKERS: [&str; 4] = ["prod", "production", "prd", "live"];
/// Longest resource prefix: keeps `<prefix>bench-role` inside the 64-character
/// IAM role and Lambda function name limits, so no generated name is truncated.
pub(crate) const MAX_PREFIX_CHARS: usize = 48;

fn name_char(c: char, extra: &[char]) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || extra.contains(&c)
}

/// A resource prefix: starts with [`REQUIRED_PREFIX`], only `A-Za-z0-9_-`.
pub(crate) fn check_prefix(prefix: &str) -> Result<(), ToolkitError> {
    if !prefix.starts_with(REQUIRED_PREFIX) {
        return Err(die(format!(
            "resource_prefix must start with '{REQUIRED_PREFIX}' so teardown can scope deletions \
             safely. Tip: omit resource_prefix entirely and the deploy generates a valid one \
             ({REQUIRED_PREFIX}<date>-<uuid8>) for you."
        )));
    }
    if prefix.chars().count() > MAX_PREFIX_CHARS || !prefix.chars().all(|c| name_char(c, &[])) {
        return Err(die(format!(
            "resource_prefix must be at most {MAX_PREFIX_CHARS} characters of letters, digits, \
             '-' and '_' (it becomes part of IAM resource ARNs and AWS resource names)."
        )));
    }
    Ok(())
}

/// A `DynamoDB` table name part: 1 to 255 characters of `A-Za-z0-9_.-` (the full
/// name, prefix included, is always far above the 3-character minimum). `what`
/// names the field in the diagnostic.
pub(crate) fn check_table_name(what: &str, name: &str) -> Result<(), ToolkitError> {
    let length = name.chars().count();
    if !(1..=255).contains(&length) || !name.chars().all(|c| name_char(c, &['.'])) {
        return Err(die(format!(
            "{what} must be 1 to 255 characters of letters, digits, '-', '_' and '.'."
        )));
    }
    Ok(())
}

/// A Lambda function or IAM role name this tool generates.
pub(crate) fn check_resource_name(what: &str, name: &str) -> Result<(), ToolkitError> {
    if name.is_empty() || name.chars().count() > 64 || !name.chars().all(|c| name_char(c, &[])) {
        return Err(die(format!(
            "{what} must be 1 to 64 characters of letters, digits, '-' and '_'."
        )));
    }
    Ok(())
}

/// An IAM inline policy name: up to 128 characters of `A-Za-z0-9_+=,.@-`.
pub(crate) fn check_policy_name(name: &str) -> Result<(), ToolkitError> {
    let ok = !name.is_empty()
        && name.chars().count() <= 128
        && name
            .chars()
            .all(|c| name_char(c, &['+', '=', ',', '.', '@']));
    if ok {
        Ok(())
    } else {
        Err(die(
            "policy name must be 1 to 128 characters of letters, digits and '_+=,.@-'.",
        ))
    }
}

/// An AWS region such as `us-east-1`: lowercase letters, digits and hyphens.
pub(crate) fn check_region(region: &str) -> Result<(), ToolkitError> {
    let shaped = (5..=32).contains(&region.len())
        && region.starts_with(|c: char| c.is_ascii_lowercase())
        && region.ends_with(|c: char| c.is_ascii_digit())
        && region.contains('-')
        && region
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if shaped {
        Ok(())
    } else {
        Err(die("region is not an AWS region name such as 'us-east-1'."))
    }
}

/// A 12-digit AWS account id: it is written into the role trust policy. `what`
/// names the field in the diagnostic; the value is never shown.
pub(crate) fn check_account(what: &str, account: &str) -> Result<(), ToolkitError> {
    if account.len() == 12 && account.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(die(format!("{what} must be a 12-digit AWS account id.")))
    }
}

/// Free text (an AWS profile name, a run id) that must not carry control or
/// bidirectional characters. `what` names the field.
pub(crate) fn check_plain(what: &str, text: &str, max_chars: usize) -> Result<(), ToolkitError> {
    if text.chars().count() > max_chars || text.chars().any(is_unsafe_char) {
        return Err(die(format!(
            "{what} must be at most {max_chars} printable characters."
        )));
    }
    Ok(())
}

/// The shape of a caller ARN (`arn:<partition>:iam|sts::<account>:<resource>`):
/// the account field must equal the validated `account`. `what` names the field
/// in the diagnostic; the value is never shown.
pub(crate) fn check_caller_arn(what: &str, arn: &str, account: &str) -> Result<(), ToolkitError> {
    let fields: Vec<&str> = arn.splitn(6, ':').collect();
    let shaped = arn.len() <= 2048
        && matches!(
            fields.as_slice(),
            ["arn", "aws" | "aws-cn" | "aws-us-gov", "iam" | "sts", "", arn_account, resource]
                if *arn_account == account
                    && !resource.is_empty()
                    && resource.chars().all(arn_char)
        );
    if shaped {
        Ok(())
    } else {
        Err(die(format!(
            "{what} must be a non-empty IAM or STS ARN (arn:<aws|aws-cn|aws-us-gov>:<iam|sts>::\
             <account>:<resource>) whose account is the caller's account."
        )))
    }
}

fn arn_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "+=,.@_-/:".contains(c)
}

/// An IAM account alias: 3 to 63 lowercase letters, digits and hyphens. A
/// mixed-case value is not a real alias and is refused, so a credential-shaped
/// string is never shown.
pub(crate) fn check_alias(what: &str, alias: &str) -> Result<(), ToolkitError> {
    if (3..=63).contains(&alias.len())
        && alias
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        Ok(())
    } else {
        Err(die(format!(
            "{what} must be 3 to 63 lowercase letters, digits and hyphens."
        )))
    }
}
