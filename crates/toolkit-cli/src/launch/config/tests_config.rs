//! T-REG-1..8 (region chain) and T-URL-1..6 (base and issuer URL rules).

use std::path::Path;

use toolkit_aws::testing::FixedResolver;
use toolkit_aws::MapEnv;

use super::*;
use crate::launch::tests_support::Scratch;

fn inputs<'a>(
    env: &'a MapEnv,
    explicit: Option<&'a str>,
    aws_mcp_url: Option<&'a str>,
    state_dir: Option<&'a Path>,
    home: Option<&'a Path>,
) -> RegionInputs<'a> {
    RegionInputs {
        explicit,
        aws_mcp_url,
        env,
        state_dir,
        home,
    }
}

fn config_error(result: Result<impl std::fmt::Debug, LaunchError>) -> String {
    match result {
        Err(LaunchError::Config(message)) => message,
        other => panic!("expected a ConfigError, got {other:?}"),
    }
}

// ------------------------------------------------------------- T-REG

#[test]
fn t_reg_1_explicit_region_wins_and_is_validated() {
    let env = MapEnv::new().with(ENV_REGION, "us-east-1");
    let resolved =
        resolve_region(&inputs(&env, Some("eu-central-1"), None, None, None)).expect("explicit");
    assert_eq!(resolved.region, "eu-central-1");
    assert_eq!(resolved.source, RegionSource::Explicit);
    assert_eq!(resolved.source.as_str(), "explicit");
    assert_eq!(resolved.source.label(), "the region you asked for");
    assert_eq!(resolved.region_name(), "Europe (Frankfurt)");
    assert_eq!(
        config_error(resolve_region(&inputs(
            &env,
            Some("eu-west-1"),
            None,
            None,
            None
        ))),
        "Unsupported region 'eu-west-1'. Launch with AWS runs migrations in us-east-1, \
         eu-central-1."
    );
    // An empty explicit value is "not given" (Python truthiness).
    let resolved = resolve_region(&inputs(&env, Some(""), None, None, None)).expect("env");
    assert_eq!(resolved.source, RegionSource::Environment);
    assert_eq!(validate_region("us-east-1"), Ok("us-east-1"));
    assert_eq!(
        base_url_for_region("eu-central-1").as_deref(),
        Ok("https://launch-with-aws.eu-central-1.api.aws")
    );
    assert!(base_url_for_region("mars-1").is_err());
}

#[test]
fn t_reg_2_environment_region_is_second_and_validated() {
    let env = MapEnv::new()
        .with(ENV_REGION, "eu-central-1")
        .with("AWS_REGION", "us-west-2");
    let resolved = resolve_region(&inputs(&env, None, None, None, None)).expect("env");
    assert_eq!(
        (resolved.region, resolved.source),
        ("eu-central-1", RegionSource::Environment)
    );
    assert_eq!(
        resolved.source.label(),
        "your LAUNCH_WITH_AWS_REGION setting"
    );
    // A hostile value is shown redacted and escaped, never raw.
    let env = MapEnv::new().with(ENV_REGION, "x\u{1b}[31m");
    let message = config_error(resolve_region(&inputs(&env, None, None, None, None)));
    assert_eq!(
        message,
        "Unsupported region 'x\\x1b[31m'. Launch with AWS runs migrations in us-east-1, \
         eu-central-1."
    );
}

#[test]
fn t_reg_3_saved_region_is_used_only_while_supported() {
    let state = Scratch::new("reg3");
    state.write("config.json", b"{\n  \"region\": \"eu-central-1\"\n}");
    let env = MapEnv::new().with("AWS_REGION", "us-west-2");
    let resolved =
        resolve_region(&inputs(&env, None, None, Some(state.path()), None)).expect("saved");
    assert_eq!(
        (resolved.region, resolved.source),
        ("eu-central-1", RegionSource::Saved)
    );
    assert_eq!(resolved.source.label(), "your saved choice");
    for contents in [
        &b"{\"region\": \"ap-south-1\"}"[..],
        b"[\"eu-central-1\"]",
        b"not json",
        b"{\"region\": 7}",
    ] {
        state.write("config.json", contents);
        let resolved = resolve_region(&inputs(&env, None, None, Some(state.path()), None))
            .expect("falls through");
        assert_eq!(resolved.source, RegionSource::AwsProfile, "{contents:?}");
    }
}

#[test]
fn t_reg_4_only_an_eu_aws_mcp_endpoint_is_a_signal() {
    let env = MapEnv::new();
    let resolved = resolve_region(&inputs(
        &env,
        None,
        Some("https://aws-mcp.eu-west-1.api.aws/mcp"),
        None,
        None,
    ))
    .expect("aws-mcp");
    assert_eq!(
        (resolved.region, resolved.source),
        ("eu-central-1", RegionSource::AwsMcp)
    );
    assert_eq!(resolved.source.label(), "your AWS MCP server configuration");
    let resolved = resolve_region(&inputs(
        &env,
        None,
        Some("https://aws-mcp.us-east-1.api.aws/mcp"),
        None,
        None,
    ))
    .expect("no signal");
    assert_eq!(
        (resolved.region, resolved.source),
        ("us-east-1", RegionSource::Default)
    );
    assert_eq!(resolved.source.label(), "the default");
}

#[test]
fn t_reg_5_aws_mcp_lookalike_hosts_are_no_signal() {
    assert_eq!(
        region_from_aws_mcp_url("https://aws-mcp.eu-west-1.api.aws/mcp").as_deref(),
        Some("eu-west-1")
    );
    assert_eq!(
        region_from_aws_mcp_url("https://user@AWS-MCP.EU-WEST-1.API.AWS:443/").as_deref(),
        Some("eu-west-1"),
        "the hostname is lower-cased and userinfo and port are not part of it"
    );
    for url in [
        "https://aws-mcp.eu-west-1.api.aws.evil.com/mcp",
        "https://xaws-mcp.eu-west-1.api.aws/mcp",
        "https://evil.com/aws-mcp.eu-west-1.api.aws",
        "https://aws-mcp.eu_west.api.aws",
        "https://aws-mcp..api.aws",
        "aws-mcp.eu-west-1.api.aws",
        "https://evil.com@",
        "",
    ] {
        assert_eq!(region_from_aws_mcp_url(url), None, "{url:?}");
    }
    let env = MapEnv::new();
    let resolved = resolve_region(&inputs(
        &env,
        None,
        Some("https://aws-mcp.eu-west-1.api.aws.evil.com"),
        None,
        None,
    ))
    .expect("default");
    assert_eq!(resolved.source, RegionSource::Default);
}

#[test]
fn t_reg_6_aws_region_variables_map_to_a_cell() {
    assert_eq!(cell_for_aws_region("eu-west-3"), EU_REGION);
    assert_eq!(cell_for_aws_region("us-west-2"), DEFAULT_REGION);
    assert_eq!(cell_for_aws_region("ap-south-1"), DEFAULT_REGION);
    let env = MapEnv::new()
        .with("AWS_REGION", "eu-north-1")
        .with("AWS_DEFAULT_REGION", "us-west-2");
    let resolved = resolve_region(&inputs(&env, None, None, None, None)).expect("profile");
    assert_eq!(
        (resolved.region, resolved.source),
        ("eu-central-1", RegionSource::AwsProfile)
    );
    assert_eq!(resolved.source.label(), "your AWS profile region");
    let env = MapEnv::new()
        .with("AWS_REGION", "")
        .with("AWS_DEFAULT_REGION", "eu-south-2");
    assert_eq!(profile_region(&env, None).as_deref(), Some("eu-south-2"));
}

#[test]
fn t_reg_7_profile_region_from_the_config_file() {
    let home = Scratch::new("reg7");
    let text = "# comment\n[default]\nregion = us-west-2\n\n[profile work]\nRegion: eu-west-1\n\
                output = json\n[sso-session x]\nregion = ap-south-1\n";
    home.write(".aws/config", text.as_bytes());
    let env = MapEnv::new();
    assert_eq!(
        profile_region(&env, Some(home.path())).as_deref(),
        Some("us-west-2")
    );
    let env = MapEnv::new().with("AWS_PROFILE", "work");
    assert_eq!(
        profile_region(&env, Some(home.path())).as_deref(),
        Some("eu-west-1")
    );
    let env = MapEnv::new().with("AWS_DEFAULT_PROFILE", "work");
    assert_eq!(
        profile_region(&env, Some(home.path())).as_deref(),
        Some("eu-west-1")
    );
    let env = MapEnv::new().with("AWS_PROFILE", "missing");
    assert_eq!(profile_region(&env, Some(home.path())), None);
    // AWS_CONFIG_FILE replaces ~/.aws/config.
    let other = home.write("other-config", b"[profile p]\nregion = eu-central-2\n");
    let env = MapEnv::new()
        .with("AWS_CONFIG_FILE", other.to_str().expect("utf-8"))
        .with("AWS_PROFILE", "p");
    assert_eq!(
        profile_region(&env, Some(home.path())).as_deref(),
        Some("eu-central-2")
    );
    let resolved =
        resolve_region(&inputs(&env, None, None, None, Some(home.path()))).expect("profile");
    assert_eq!(
        (resolved.region, resolved.source),
        ("eu-central-1", RegionSource::AwsProfile)
    );
    // `[profile default]` wins over `[default]`; continuation lines extend a value.
    assert_eq!(
        parse_profile_region(
            "[default]\nregion=a\n[profile default]\nregion=b\n",
            "default"
        )
        .as_deref(),
        Some("b")
    );
    assert_eq!(
        parse_profile_region("[default]\nregion = eu-west-1\n  \n; c\n", "default").as_deref(),
        Some("eu-west-1")
    );
    assert_eq!(
        parse_profile_region("[default]\nregion =\n", "default"),
        None
    );
}

#[test]
fn t_reg_7b_indented_keys_follow_raw_config_parser_5133() {
    // RawConfigParser: a line is a continuation only when it is indented
    // deeper than the line that started the current key; an indented line
    // with no key to continue is itself a key.
    let home = Scratch::new("reg7b");
    home.write(".aws/config", b"[default]\n  region = eu-west-1\n");
    let env = MapEnv::new();
    assert_eq!(
        profile_region(&env, Some(home.path())).as_deref(),
        Some("eu-west-1")
    );
    let resolved =
        resolve_region(&inputs(&env, None, None, None, Some(home.path()))).expect("profile");
    assert_eq!(
        (resolved.region, resolved.source),
        (EU_REGION, RegionSource::AwsProfile)
    );
    assert_eq!(EU_REGION, "eu-central-1");
    for (text, expected) in [
        // Indented keys after an indented key at the same depth.
        (
            "[default]\n  output = json\n  region = eu-west-1\n",
            Some("eu-west-1"),
        ),
        // Shallower than the previous key, but still indented: a key.
        (
            "[default]\n    output = json\n  region = eu-west-1\n",
            Some("eu-west-1"),
        ),
        // Tab indentation counts like spaces.
        (
            "[profile x]\nregion = a\n[default]\n\tregion = eu-north-1\n",
            Some("eu-north-1"),
        ),
        // Deeper than an indented key: a continuation of it.
        (
            "[default]\n  region = us-east-1\n    eu-west-1\n",
            Some("us-east-1\neu-west-1"),
        ),
        ("[default]\n  output = json\n    region = eu-west-1\n", None),
        // A continuation of an unindented key, as before.
        (
            "[default]\nregion = us-east-1\n  extra\n",
            Some("us-east-1\nextra"),
        ),
        // A blank line inside a value is kept; trailing blanks are stripped.
        (
            "[default]\nregion = eu-west-1\n\n  more\n",
            Some("eu-west-1\n\nmore"),
        ),
        ("[default]\nregion = eu-west-1\n\n\n", Some("eu-west-1")),
        // A comment line inside a value adds nothing.
        ("[default]\nregion = a\n# c\n  b\n", Some("a\nb")),
        // An indented section header is a header when it continues nothing.
        (
            "[profile x]\n  [default]\n  region = eu-west-3\n",
            Some("eu-west-3"),
        ),
        // ...and part of the value when it does.
        (
            "[default]\nregion = a\n  [profile x]\n",
            Some("a\n[profile x]"),
        ),
        // A blank line does not end a value: the indented header continues it.
        (
            "[default]\nregion = a\n\n  [profile x]\n",
            Some("a\n\n[profile x]"),
        ),
    ] {
        assert_eq!(
            parse_profile_region(text, "default").as_deref(),
            expected,
            "{text:?}"
        );
    }
    // An indented key before any section is still a missing section header.
    assert_eq!(
        parse_profile_region("  region = eu-west-1\n[default]\n", "default"),
        None
    );
}

#[test]
fn t_reg_7c_section_headers_and_line_ends_follow_raw_config_parser() {
    for (text, expected) in [
        // SECTCRE `\[(?P<header>.+)\]` at the start: text after the last `]`
        // is ignored, and the header runs to the last `]`.
        ("[default] ; note\nregion = eu-west-1\n", Some("eu-west-1")),
        (
            "[profile x]]\nregion = a\n[default]\nregion = eu-west-2\n",
            Some("eu-west-2"),
        ),
        ("[]\nregion = eu-west-1\n", None),
        // Universal newlines: `\r\n` and a lone `\r` end lines.
        ("[default]\r\nregion = eu-west-1\r\n", Some("eu-west-1")),
        ("[default]\rregion = eu-west-1\r", Some("eu-west-1")),
        // U+001C..U+001F are Python whitespace: indentation and stripping.
        (
            "[default]\n\u{1f}region = eu-west-1\u{1c}\n",
            Some("eu-west-1"),
        ),
    ] {
        assert_eq!(
            parse_profile_region(text, "default").as_deref(),
            expected,
            "{text:?}"
        );
    }
}

#[test]
fn t_reg_8_malformed_config_files_are_no_signal() {
    for text in [
        "region = us-west-2\n[default]\nregion = eu-west-1\n",
        "[default]\nthis line has no delimiter\nregion = eu-west-1\n",
        "[default]\nregion = eu-west-1\n[default]\nregion = eu-west-2\n",
        "[default]\nregion = eu-west-1\nREGION = eu-west-2\n",
        "[default\nregion = eu-west-1\n",
    ] {
        assert_eq!(parse_profile_region(text, "default"), None, "{text:?}");
    }
    let home = Scratch::new("reg8");
    home.write(".aws/config", b"\xff\xfe[default]\nregion = eu-west-1\n");
    let env = MapEnv::new();
    assert_eq!(
        profile_region(&env, Some(home.path())),
        None,
        "non-UTF-8 file"
    );
    let resolved =
        resolve_region(&inputs(&env, None, None, None, Some(home.path()))).expect("default");
    assert_eq!(
        (resolved.region, resolved.source),
        ("us-east-1", RegionSource::Default)
    );
    // A directory in place of the file, and no HOME at all: no signal.
    std::fs::remove_file(home.join(".aws/config")).expect("rm");
    std::fs::create_dir(home.join(".aws/config")).expect("dir");
    assert_eq!(profile_region(&env, Some(home.path())), None);
    assert_eq!(profile_region(&env, None), None);
}

#[test]
fn expand_user_follows_python_expanduser_for_tilde_only() {
    let home = Path::new("/home/u");
    assert_eq!(expand_user("~", Some(home)), Path::new("/home/u"));
    assert_eq!(
        expand_user("~/src/app", Some(home)),
        Path::new("/home/u/src/app")
    );
    assert_eq!(
        expand_user("~other/app", Some(home)),
        Path::new("~other/app")
    );
    assert_eq!(expand_user("./~/x", Some(home)), Path::new("./~/x"));
    assert_eq!(expand_user("~/x", None), Path::new("~/x"));
}

// ------------------------------------------------------------- T-URL

fn public() -> FixedResolver {
    FixedResolver::new().answer_any(&["52.94.1.1"])
}

#[test]
fn t_url_1_base_url_scheme_must_be_https() {
    assert_eq!(
        config_error(validate_base_url("http://launch.api.aws", &public())),
        "Invalid LAUNCH_WITH_AWS_BASE_URL: scheme must be https, got 'http'. Refusing to send \
         credentials over a non-HTTPS connection."
    );
    assert_eq!(
        config_error(validate_base_url("launch.api.aws", &public())),
        "Invalid LAUNCH_WITH_AWS_BASE_URL: scheme must be https, got ''. Refusing to send \
         credentials over a non-HTTPS connection."
    );
    assert_eq!(
        validate_base_url("HTTPS://Launch.us-east-1.api.aws/v1/", &public()).as_deref(),
        Ok("HTTPS://Launch.us-east-1.api.aws/v1"),
        "accepted URLs come back with trailing slashes removed (Python rstrip)"
    );
    assert_eq!(
        config_error(validate_base_url("https:///path", &public())),
        "Invalid LAUNCH_WITH_AWS_BASE_URL: no hostname in 'https:///path'."
    );
}

#[test]
fn t_url_2_base_url_host_must_be_on_the_allowlist() {
    for ok in [
        "https://api.aws",
        "https://amazonaws.com",
        "https://launch-with-aws.eu-central-1.api.aws",
        "https://x.execute-api.us-east-1.amazonaws.com:8443/stage",
    ] {
        assert!(validate_base_url(ok, &public()).is_ok(), "{ok}");
    }
    assert_eq!(
        config_error(validate_base_url(
            "https://launch.api.aws.evil.com",
            &public()
        )),
        "Invalid LAUNCH_WITH_AWS_BASE_URL: host 'launch.api.aws.evil.com' is not in the \
         allowed domains (.api.aws, .amazonaws.com). Only official AWS endpoints are permitted."
    );
    assert!(validate_base_url("https://xapi.aws", &public()).is_err());
    assert!(validate_base_url("https://evil.com#.api.aws", &public()).is_err());
}

#[test]
fn t_url_3_base_url_userinfo_port_and_characters_are_refused() {
    assert_eq!(
        config_error(validate_base_url("https://u@launch.api.aws", &public())),
        "Invalid LAUNCH_WITH_AWS_BASE_URL: userinfo (credentials) in the URL is not allowed."
    );
    for bad in [
        "https://launch.api.aws:0",
        "https://launch.api.aws:65536",
        "https://launch.api.aws:x",
        "https://launch.api.aws/a b",
        "https://launch.api.aws/\u{7f}",
        "https://la_unch.api.aws",
        "https://launch.api.aws/\n",
    ] {
        let message = config_error(validate_base_url(bad, &public()));
        assert!(
            message.starts_with("Invalid LAUNCH_WITH_AWS_BASE_URL: "),
            "{bad:?}: {message}"
        );
        assert!(
            !message.contains('\n') && !message.contains('\u{7f}'),
            "{message:?}"
        );
    }
}

#[test]
fn t_url_4_base_url_must_resolve_to_public_addresses_only() {
    for private in ["10.0.0.1", "127.0.0.1", "169.254.169.254", "::1", "fd00::1"] {
        let resolver = FixedResolver::new().answer_any(&["52.94.1.1", private]);
        let message = config_error(validate_base_url("https://x.api.aws", &resolver));
        assert!(
            message.starts_with(
                "Invalid LAUNCH_WITH_AWS_BASE_URL: host 'x.api.aws' resolves to \
                 private/loopback/link-local address "
            ),
            "{message}"
        );
        assert!(message.ends_with(". Refusing to send credentials to a non-public address."));
    }
    let message = config_error(validate_base_url(
        "https://x.api.aws",
        &FixedResolver::new(),
    ));
    assert!(
        message.starts_with(
            "Invalid LAUNCH_WITH_AWS_BASE_URL: DNS resolution failed for 'x.api.aws': "
        ),
        "{message}"
    );
    assert!(message.ends_with(". Refusing to proceed with an unresolvable host."));
    let resolver = FixedResolver::new().answer_any(&[]);
    assert!(
        validate_base_url("https://x.api.aws", &resolver).is_err(),
        "no answer"
    );
    let resolver = public();
    validate_base_url("https://X.Api.Aws", &resolver).expect("public");
    assert_eq!(resolver.lookups(), vec!["x.api.aws".to_string()]);
}

#[test]
fn t_url_5_issuer_url_rules() {
    assert_eq!(
        validate_issuer_url("https://d-123.awsapps.com/start/").as_deref(),
        Ok("https://d-123.awsapps.com/start/"),
        "returned unchanged"
    );
    assert!(validate_issuer_url("https://identitycenter.amazonaws.com/ssoins-1").is_ok());
    assert_eq!(
        config_error(validate_issuer_url("http://d-123.awsapps.com/start")),
        "Invalid LAUNCH_WITH_AWS_IDC_ISSUER_URL: scheme must be https, got 'http'. Refusing to \
         anchor the authentication flow on a non-HTTPS issuer."
    );
    assert_eq!(
        config_error(validate_issuer_url("https://evil.com/start")),
        "Invalid LAUNCH_WITH_AWS_IDC_ISSUER_URL: host 'evil.com' is not in the allowed issuer \
         domains (.amazonaws.com, .awsapps.com). Only official AWS IAM Identity Center endpoints \
         are permitted."
    );
    for bad in [
        "https://u@d-123.awsapps.com/start",
        "https://d-123.awsapps.com:0/start",
        "https://d-123.awsapps.com:99999/start",
        "https://d-123.awsapps.com/st art",
        "https://d-123.awsapps.com/start\t",
    ] {
        assert!(validate_issuer_url(bad).is_err(), "{bad:?}");
    }
    let env = MapEnv::new();
    assert_eq!(
        resolve_issuer_url(&env),
        Ok((IDC_ISSUER_URL.to_string(), None))
    );
    let env = MapEnv::new().with(ENV_IDC_ISSUER_URL, "https://d-1.awsapps.com/start");
    assert_eq!(
        resolve_issuer_url(&env),
        Ok((
            "https://d-1.awsapps.com/start".to_string(),
            Some("Using non-default IdC issuer URL: https://d-1.awsapps.com/start".to_string())
        ))
    );
    let env = MapEnv::new().with(ENV_IDC_ISSUER_URL, "");
    assert_eq!(
        resolve_issuer_url(&env),
        Ok((IDC_ISSUER_URL.to_string(), None))
    );
}

#[test]
fn t_url_6_caller_values_are_redacted_in_every_message() {
    let secret_inputs = [
        "https://u:FAKE_SECRET@x.api.aws",
        "http://u:FAKE_SECRET@x.api.aws",
        "https://u:FAKE_SECRET@evil.com",
        "https://:FAKE_SECRET@",
    ];
    for url in secret_inputs {
        let message = config_error(validate_base_url(url, &public()));
        assert!(!message.contains("FAKE_SECRET"), "{url}: {message}");
        let message = config_error(validate_issuer_url(url));
        assert!(!message.contains("FAKE_SECRET"), "{url}: {message}");
    }
    let message = config_error(validate_base_url("ftp://AKIAIOSFODNN7EXAMPLE@x", &public()));
    assert!(!message.contains("AKIAIOSFODNN7EXAMPLE"), "{message}");
}
