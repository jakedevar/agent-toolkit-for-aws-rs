//! T-CLI, T-HDR (CLI rows), T-CRED-5, T-EP (argument rows) and T-SCOPE
//! (#4413 S3): the plugin and documented argument vectors parse to the
//! expected configuration; everything else is a usage error that echoes no
//! value.

use std::time::Duration;

use toolkit_aws::diag::Level;
use toolkit_aws::MapEnv;
use toolkit_core::ErrorKind;

use crate::mcp_proxy::cli::{parse, ProxyArgs};
use crate::mcp_proxy::scope::{
    metadata_region, proxy_config, signing_scope, ScopeError, USER_AGENT,
};
use crate::{route, Routed, COMMANDS};

const AWS_MCP: &str = "https://aws-mcp.us-east-1.api.aws/mcp";

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

fn parsed(words: &[&str]) -> Result<ProxyArgs, String> {
    let words = args(words);
    match route(COMMANDS, &words) {
        Ok(Routed::Run(spec, invocation)) => {
            assert_eq!(spec.path, &["mcp-proxy"]);
            parse(&invocation).map_err(|error| {
                assert!(matches!(error.kind(), ErrorKind::Usage));
                assert_eq!(error.exit_code(), 2);
                error.message().to_string()
            })
        }
        Ok(_) => panic!("help or version for {words:?}"),
        Err(error) => {
            assert!(matches!(error.kind(), ErrorKind::Usage), "{words:?}");
            Err(error.message().to_string())
        }
    }
}

fn refused(words: &[&str]) -> String {
    match parsed(words) {
        Ok(_) => panic!("accepted: {words:?}"),
        Err(message) => message,
    }
}

#[test]
fn t_cli_the_six_plugin_vectors_parse_to_the_expected_config() {
    for label in [
        "agent-toolkit-core",
        "agent-toolkit-data-analytics",
        "agent-toolkit-startup-advisor",
    ] {
        let metadata = format!("INSTALL_SOURCE={label}");
        // `.mcp.json` and `mcp.json` carry the same arguments.
        for _file in [".mcp.json", "mcp.json"] {
            let a =
                parsed(&["mcp-proxy", AWS_MCP, "--skip-auth", "--metadata", &metadata]).unwrap();
            assert_eq!(a.target.url(), AWS_MCP);
            assert!(a.skip_auth && !a.read_only);
            assert_eq!(
                a.metadata,
                vec![("INSTALL_SOURCE".to_string(), label.to_string())]
            );
            assert_eq!(a.retries, 0);
            assert_eq!(
                (
                    a.timeout,
                    a.connect_timeout,
                    a.read_timeout,
                    a.write_timeout,
                    a.tool_timeout
                ),
                (
                    Duration::from_secs(180),
                    Duration::from_secs(60),
                    Duration::from_secs(120),
                    Duration::from_secs(180),
                    Duration::from_secs(300)
                )
            );
            assert_eq!(a.log_level, Level::Error);
            let scope = signing_scope(a.target.host(), None, None, &MapEnv::new()).unwrap();
            assert_eq!(
                (scope.service.as_str(), scope.region.as_str()),
                ("aws-mcp", "us-east-1")
            );
            let region = metadata_region(a.target.host(), None, &MapEnv::new());
            let config = proxy_config(&a, region).unwrap();
            assert_eq!(config.url, AWS_MCP);
            assert_eq!(
                config.metadata,
                vec![
                    ("AWS_REGION".to_string(), "us-east-1".to_string()),
                    ("INSTALL_SOURCE".to_string(), label.to_string())
                ]
            );
            assert_eq!(config.user_agent.as_deref(), Some(USER_AGENT));
            assert!(config.headers.is_empty());
            assert_eq!(config.limits.tool_timeout(), Duration::from_secs(300));
            assert_eq!(config.limits.request_timeout(), Duration::from_secs(180));
        }
    }
}

#[test]
fn t_cli_the_documented_aidevops_and_readme_vectors() {
    let a = parsed(&[
        "mcp-proxy",
        "https://connect.aidevops.us-west-2.api.aws/mcp",
        "--service",
        "aidevops",
        "--region",
        "us-west-2",
    ])
    .unwrap();
    let scope = signing_scope(
        a.target.host(),
        a.service.as_deref(),
        a.region.as_deref(),
        &MapEnv::new(),
    )
    .unwrap();
    assert_eq!(
        (scope.service.as_str(), scope.region.as_str()),
        ("aidevops", "us-west-2")
    );

    // README: `--metadata AWS_REGION=us-west-2` overrides the automatic value.
    let a = parsed(&["mcp-proxy", AWS_MCP, "--metadata", "AWS_REGION=us-west-2"]).unwrap();
    let config = proxy_config(&a, Some("us-east-1".to_string())).unwrap();
    assert_eq!(
        config.metadata,
        vec![("AWS_REGION".to_string(), "us-west-2".to_string())]
    );

    // Every flag in one vector, `--flag=value` spelling included.
    let a = parsed(&[
        "mcp-proxy",
        AWS_MCP,
        "--profile=dev",
        "--read-only",
        "--retries",
        "3",
        "--timeout=90",
        "--connect-timeout",
        "5",
        "--read-timeout",
        "30.5",
        "--write-timeout",
        "60",
        "--tool-timeout",
        "120",
        "--log-level",
        "debug",
        "--header",
        "X-Amzn-AgentAccess-Streaming-Session-Url=https://session.example/HEADER-CANARY",
    ])
    .unwrap();
    assert_eq!(a.profile.as_deref(), Some("dev"));
    assert!(a.read_only);
    assert_eq!(a.retries, 3);
    assert_eq!(a.read_timeout, Duration::from_secs_f64(30.5));
    assert_eq!(a.log_level, Level::Debug);
    let config = proxy_config(&a, None).unwrap();
    assert!(config.read_only);
    assert_eq!(config.retries, 3);
    assert_eq!(config.limits.tool_timeout(), Duration::from_secs(120));
    assert_eq!(config.limits.request_timeout(), Duration::from_secs(90));
    assert_eq!(config.timeouts.connect(), Duration::from_secs(5));
    let name = "x-amzn-agentaccess-streaming-session-url";
    assert_eq!(
        config.headers.get(name),
        Some("https://session.example/HEADER-CANARY")
    );
    assert!(config.headers.is_sensitive(name));
    assert!(!format!("{:?}", config.headers).contains("HEADER-CANARY"));
}

#[test]
fn t_cli_unknown_or_unsupported_upstream_forms_are_usage_errors() {
    for words in [
        vec!["mcp-proxy"],
        vec!["mcp-proxy", AWS_MCP, "--profile", "a", "b"],
        vec!["mcp-proxy", AWS_MCP, "--disable-telemetry"],
        vec!["mcp-proxy", AWS_MCP, "--metadata", "A=1", "B=2"],
        vec!["mcp-proxy", AWS_MCP, "--metadata", "=x"],
        vec!["mcp-proxy", AWS_MCP, "--metadata", "novalue"],
        vec!["mcp-proxy", AWS_MCP, "--retries", "11"],
        vec!["mcp-proxy", AWS_MCP, "--retries", "-1"],
        vec!["mcp-proxy", AWS_MCP, "--timeout", "0"],
        vec!["mcp-proxy", AWS_MCP, "--tool-timeout", "3601"],
        vec!["mcp-proxy", AWS_MCP, "--read-timeout", "abc"],
        vec!["mcp-proxy", AWS_MCP, "--connect-timeout", "NaN"],
        vec!["mcp-proxy", AWS_MCP, "--log-level", "TRACE"],
        vec!["mcp-proxy", AWS_MCP, "--region", "not a region"],
        vec!["mcp-proxy", AWS_MCP, "--service", "Bad_Service"],
        vec!["mcp-proxy", AWS_MCP, "--service", "-"],
        vec!["mcp-proxy", "-"],
        vec!["mcp-proxy", AWS_MCP, AWS_MCP],
    ] {
        refused(&words);
    }
}

#[test]
fn t_hdr_cli_owned_and_duplicate_headers_are_usage_errors() {
    for header in [
        "Host=evil.example",
        "HOST=evil.example",
        "hOsT=evil.example",
        "Content-Length=0",
        "Transfer-Encoding=chunked",
        "Connection=close",
        "Keep-Alive=5",
        "TE=trailers",
        "Upgrade=h2c",
        "Expect=100-continue",
        "Proxy-Authorization=Basic SEADERCANARY",
        "Mcp-Session-Id=x",
        "MCP-PROTOCOL-VERSION=x",
        "Last-Event-Id=1",
        "Accept=*/*",
        "Content-Type=text/plain",
        "User-Agent=x",
        "Authorization=AWS4-HMAC-SHA256 SEADERCANARY",
        "X-Amz-Date=20260101T000000Z",
        "X-Amz-Security-Token=SEADERCANARY",
        "X-Amz-Content-Sha256=x",
        "Date=x",
        "no-equals-sign",
        "Bad Name=x",
        "X-Ok=line\nbreak",
    ] {
        let message = refused(&["mcp-proxy", AWS_MCP, "--header", header]);
        assert!(
            !message.contains("SEADERCANARY") && !message.contains("evil"),
            "{message}"
        );
    }
    let message = refused(&[
        "mcp-proxy",
        AWS_MCP,
        "--header",
        "X-A=1",
        "--header",
        "x-a=2",
    ]);
    assert!(message.contains("twice"), "{message}");
}

#[test]
fn t_cred_5_credential_flags_are_refused_without_echo() {
    for words in [
        vec!["mcp-proxy", AWS_MCP, "--secret-access-key", "CREDCANARY0"],
        vec!["mcp-proxy", AWS_MCP, "--aws-session-token=CREDCANARY0"],
        vec![
            "mcp-proxy",
            AWS_MCP,
            "--access-key-id",
            "AKIACREDCANARY00000",
        ],
        vec![
            "mcp-proxy",
            AWS_MCP,
            "--aws-secret-access-key",
            "CREDCANARY0",
        ],
    ] {
        let message = refused(&words);
        assert!(!message.contains("CANARY"), "{message}");
    }
}

#[test]
fn t_ep_refused_endpoints_never_echo_the_url() {
    for url in [
        "http://aws-mcp.us-east-1.api.aws/mcp",
        "https://localhost/mcp",
        "https://127.0.0.1/mcp",
        "https://example.com/EPCANARY",
        "https://aws-mcp.cn-north-1.amazonaws.com.cn/mcp",
        "https://user:EPCANARY@aws-mcp.us-east-1.api.aws/mcp",
        "https://aws-mcp.us-east-1.api.aws/mcp?x=1",
        "https://aws-mcp.us-east-1.api.aws/mcp?",
        "https://aws-mcp.us-east-1.api.aws/mcp#f",
        "https://aws-mcp.us-east-1.api.aws/mcp?token=EPCANARY",
    ] {
        let message = refused(&["mcp-proxy", url]);
        assert!(
            !message.contains("EPCANARY") && !message.contains("example"),
            "{message}"
        );
        assert!(message.contains("https URL of an AWS host"), "{message}");
    }
    // Accepted: the four documented host shapes; the request URL is McpTarget's.
    for url in [
        AWS_MCP,
        "https://connect.aidevops.us-east-1.api.aws/mcp",
        "https://agentaccess-mcp.us-east-1.api.aws/mcp",
        "https://abc.bedrock-agentcore.us-west-2.amazonaws.com/runtimes/x/invocations",
    ] {
        let a = parsed(&["mcp-proxy", url, "--region", "us-east-1"]).unwrap();
        assert_eq!(proxy_config(&a, None).unwrap().url, a.target.url());
        assert_eq!(a.target.url(), url);
    }
}

#[test]
fn t_scope_service_and_region_precedence() {
    let none = MapEnv::new();
    let env = MapEnv::new().with("AWS_REGION", "eu-central-1");
    let scope = |host: &str, service: Option<&str>, region: Option<&str>, env: &MapEnv| {
        signing_scope(host, service, region, env).map(|s| (s.service, s.region))
    };
    let ok = |service: &str, region: &str| Ok((service.to_string(), region.to_string()));
    assert_eq!(
        scope("aws-mcp.us-east-1.api.aws", None, None, &none),
        ok("aws-mcp", "us-east-1")
    );
    // --region beats the host's region, which beats AWS_REGION.
    assert_eq!(
        scope("aws-mcp.us-east-1.api.aws", None, Some("us-west-2"), &env),
        ok("aws-mcp", "us-west-2")
    );
    assert_eq!(
        scope("aws-mcp.us-east-1.api.aws", None, None, &env),
        ok("aws-mcp", "us-east-1")
    );
    assert_eq!(
        scope("agentaccess-mcp.eu-west-1.api.aws", None, None, &none),
        ok("agentaccess-mcp", "eu-west-1")
    );
    assert_eq!(
        scope(
            "abc.bedrock-agentcore.us-west-2.amazonaws.com",
            None,
            None,
            &none
        ),
        ok("bedrock-agentcore", "us-west-2")
    );
    // aidevops: the host gives no region; --service/--region or AWS_REGION.
    assert_eq!(
        scope(
            "connect.aidevops.us-east-1.api.aws",
            Some("aidevops"),
            Some("us-east-1"),
            &none
        ),
        ok("aidevops", "us-east-1")
    );
    assert_eq!(
        scope("connect.aidevops.us-east-1.api.aws", None, None, &env),
        ok("connect", "eu-central-1")
    );
    assert_eq!(
        scope("connect.aidevops.us-east-1.api.aws", None, None, &none),
        Err(ScopeError::NoRegion)
    );
    let bad = MapEnv::new().with("AWS_REGION", "not a region");
    assert_eq!(
        scope("connect.aidevops.us-east-1.api.aws", None, None, &bad),
        Err(ScopeError::NoRegion)
    );
    // AWS_REGION metadata: session region, then AWS_REGION, then the host.
    let host = "aws-mcp.us-east-1.api.aws";
    assert_eq!(
        metadata_region(host, None, &none).as_deref(),
        Some("us-east-1")
    );
    assert_eq!(
        metadata_region(host, None, &env).as_deref(),
        Some("eu-central-1")
    );
    assert_eq!(
        metadata_region(host, Some("ap-south-1"), &env).as_deref(),
        Some("ap-south-1")
    );
    let default = MapEnv::new()
        .with("AWS_REGION", "eu-central-1")
        .with("AWS_DEFAULT_REGION", "sa-east-1");
    assert_eq!(
        metadata_region(host, Some("ap-south-1"), &default).as_deref(),
        Some("sa-east-1")
    );
    assert_eq!(
        metadata_region("connect.aidevops.us-east-1.api.aws", None, &none),
        None
    );
}

#[test]
fn the_user_agent_is_a_fixed_product_token() {
    assert!(USER_AGENT.starts_with("agent-toolkit-mcp-proxy/"));
    assert!(!USER_AGENT.contains(' '));
}
