//! The `--network-policy FILE` schema (R-DG-03; #5366): the only source of
//! the narrow database endpoint capability.
//!
//! The file holds exactly `host`, `port`, `region` and optional
//! `private_ranges`. Any other key, including live- or consent-shaped ones
//! such as `live` or `confirm_wipe`, is refused rather than ignored: live
//! selection and every `--confirm-*` flag are argv-only (R-DG-01), so a
//! policy can never imply them. Values are never echoed in errors.
//!
//! Not wired to a live adapter in S1: `--live` is still refused before any
//! file is read. S3/S4 read the file only after argv consent is checked.

use serde_json::Value;
use toolkit_aws::resolve_region;
use toolkit_aws::sdk::database::DatabaseEndpoint;
use toolkit_aws::{AwsError, Env};

/// Largest policy file accepted.
pub const MAX_POLICY_BYTES: usize = 16 * 1024;

const KEYS: [&str; 4] = ["host", "port", "region", "private_ranges"];

fn invalid(reason: &str) -> AwsError {
    AwsError::invalid_config("network_policy", reason)
}

/// Parse and validate a network policy.
///
/// # Errors
/// `InvalidConfig` (field `network_policy`) for oversize or malformed JSON,
/// an unknown key, a missing or mistyped member, or any rule of
/// [`DatabaseEndpoint::new`].
pub fn parse(bytes: &[u8]) -> Result<DatabaseEndpoint, AwsError> {
    if bytes.len() > MAX_POLICY_BYTES {
        return Err(invalid("the network policy file is too large"));
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| invalid("the network policy is not valid JSON"))?;
    // recorded-shape: an operator network-policy file, not a service response; every other shape is rejected
    let Value::Object(map) = value else {
        return Err(invalid("the network policy must be a JSON object"));
    };
    if map.keys().any(|key| !KEYS.contains(&key.as_str())) {
        return Err(invalid(
            "the network policy has an unknown key; live selection and consent are argv-only",
        ));
    }
    let host = map
        .get("host")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("the network policy needs a string host"))?;
    let port = map
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .ok_or_else(|| invalid("the network policy needs a port between 1 and 65535"))?;
    let region = map
        .get("region")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("the network policy needs a string region"))?;
    let ranges: Vec<&str> = match map.get("private_ranges") {
        None => Vec::new(),
        // recorded-shape: an operator network-policy file, not a service response; every other shape is rejected
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .ok_or_else(|| invalid("each private range must be a string"))
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(invalid("private_ranges must be an array of strings")),
    };
    DatabaseEndpoint::new(host, port, region, &ranges)
}

/// The signing region of a live database operation (R-DG-02): the shared
/// precedence (explicit `--region`, `AWS_REGION`, `AWS_DEFAULT_REGION`,
/// `us-east-1`; a profile region never counts), which must equal the policy's
/// region. Checked before credentials, signing or any send.
///
/// # Errors
/// `InvalidConfig` (field `region`) for an invalid region or a mismatch with
/// the policy; neither region is echoed on a mismatch.
pub fn signing_region(
    policy: &DatabaseEndpoint,
    explicit: Option<&str>,
    env: &dyn Env,
) -> Result<String, AwsError> {
    let resolved = resolve_region(explicit, env)?;
    if resolved.name() != policy.region() {
        return Err(AwsError::invalid_config(
            "region",
            "the resolved region differs from the network policy's region; nothing was signed or sent",
        ));
    }
    Ok(resolved.name().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "db.cluster-abc.us-east-1.neptune.amazonaws.com";

    fn field(error: &AwsError) -> bool {
        matches!(error, AwsError::InvalidConfig { field, .. } if *field == "network_policy")
    }

    #[test]
    fn a_policy_holds_exactly_the_capability() {
        let endpoint = parse(
            format!(
                r#"{{"host":"{HOST}","port":8182,"region":"us-east-1","private_ranges":["10.1.0.0/16"]}}"#
            )
            .as_bytes(),
        )
        .expect("valid policy");
        assert_eq!(endpoint.host(), HOST);
        assert_eq!(endpoint.port(), 8182);
        assert!(endpoint.admits("10.1.2.3".parse().expect("ip")));
        assert!(!endpoint.admits("10.2.0.1".parse().expect("ip")));
        parse(format!(r#"{{"host":"{HOST}","port":8182,"region":"us-east-1"}}"#).as_bytes())
            .expect("public-only policy");
    }

    #[test]
    fn consent_and_live_shaped_keys_are_refused_not_ignored() {
        let reference = parse(
            format!(r#"{{"host":"{HOST}","port":8182,"region":"us-east-1","zzz":1}}"#).as_bytes(),
        )
        .expect_err("unknown key")
        .to_string();
        for key in [
            "live",
            "confirm_wipe",
            "confirm-wipe",
            "--confirm-write",
            "confirm_delete",
            "confirm_create",
            "allow_any",
            "proxy",
        ] {
            let text =
                format!(r#"{{"host":"{HOST}","port":8182,"region":"us-east-1","{key}":true}}"#);
            let error = parse(text.as_bytes()).expect_err(key);
            assert!(field(&error), "{key}");
            assert!(error.to_string().contains("argv-only"), "{key}");
            assert_eq!(error.to_string(), reference, "{key} is not echoed");
        }
    }

    #[test]
    fn malformed_policies_are_refused_without_echo() {
        let secret = "AKIACANARYCANARY0000";
        for text in [
            String::new(),
            "[]".to_string(),
            format!(r#"{{"host":"{secret}","port":8182,"region":"us-east-1"}}"#),
            format!(r#"{{"host":"{HOST}","port":"8182","region":"us-east-1"}}"#),
            format!(r#"{{"host":"{HOST}","port":70000,"region":"us-east-1"}}"#),
            format!(r#"{{"host":"{HOST}","port":8182}}"#),
            format!(r#"{{"host":"{HOST}","port":8182,"region":"us-west-2"}}"#),
            format!(
                r#"{{"host":"{HOST}","port":8182,"region":"us-east-1","private_ranges":"10.1.0.0/16"}}"#
            ),
            format!(r#"{{"host":"{HOST}","port":8182,"region":"us-east-1","private_ranges":[7]}}"#),
            format!(
                r#"{{"host":"{HOST}","port":8182,"region":"us-east-1","private_ranges":["127.0.0.0/16"]}}"#
            ),
            " ".repeat(MAX_POLICY_BYTES + 1),
        ] {
            let error = parse(text.as_bytes()).expect_err(&text);
            assert!(field(&error), "{text}");
            assert!(!error.to_string().contains(secret));
        }
    }

    /// T-DG-02b: region precedence, explicit mismatch and a profile region
    /// that never changes the signing scope.
    #[test]
    fn the_signing_region_follows_precedence_and_must_match_the_policy() {
        use toolkit_aws::MapEnv;
        let policy =
            parse(format!(r#"{{"host":"{HOST}","port":8182,"region":"us-east-1"}}"#).as_bytes())
                .expect("policy");
        let empty = MapEnv::new();
        assert_eq!(
            signing_region(&policy, None, &empty).expect("default"),
            "us-east-1"
        );
        let env = MapEnv::new()
            .with("AWS_REGION", "us-east-1")
            .with("AWS_DEFAULT_REGION", "eu-west-1");
        assert_eq!(
            signing_region(&policy, None, &env).expect("AWS_REGION wins"),
            "us-east-1"
        );
        let error = signing_region(&policy, Some("eu-west-1"), &env).expect_err("explicit wins");
        assert!(matches!(error, AwsError::InvalidConfig { field, .. } if field == "region"));
        assert!(!error.to_string().contains("eu-west-1"), "{error}");
        let env = MapEnv::new().with("AWS_DEFAULT_REGION", "us-west-2");
        assert!(
            signing_region(&policy, None, &env).is_err(),
            "default-region mismatch"
        );
        // A selected profile whose file names another region changes nothing.
        let env = MapEnv::new().with("AWS_PROFILE", "elsewhere").with(
            "AWS_CONFIG_FILE",
            "/nonexistent/config-with-region-eu-west-1",
        );
        assert_eq!(
            signing_region(&policy, None, &env).expect("profile ignored"),
            "us-east-1"
        );
        assert!(signing_region(&policy, Some("not a region"), &empty).is_err());
    }
}
