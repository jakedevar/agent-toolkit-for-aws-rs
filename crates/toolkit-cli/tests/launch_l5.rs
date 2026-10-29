//! Offline Launch L5 command contracts.
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn invoke(args: &[&str]) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "launch-l5-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(args)
        .env("HOME", &dir)
        .env_remove("LAUNCH_WITH_AWS_REGION")
        .env_remove("LAUNCH_WITH_AWS_BASE_URL")
        .env_remove("AWS_REGION")
        .env_remove("AWS_DEFAULT_REGION")
        .env_remove("AWS_PROFILE")
        .output()
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    output
}
#[test]
fn t_cmd_resolve_region_default() {
    let output = invoke(&["launch", "resolve-region"]);
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "{\n  \"region\": \"us-east-1\",\n  \"regionName\": \"US East (N. Virginia)\",\n  \"source\": \"default\",\n  \"sourceLabel\": \"the default\",\n  \"baseUrl\": \"https://launch-with-aws.us-east-1.api.aws\",\n  \"otherRegions\": [\n    {\n      \"region\": \"eu-central-1\",\n      \"regionName\": \"Europe (Frankfurt)\"\n    }\n  ]\n}\n");
    assert!(output.stderr.is_empty());
}
#[test]
fn t_cmd_session_status_without_session() {
    let output = invoke(&["launch", "session-status"]);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{\n  \"authenticated\": false\n}\n");
    assert!(output.stderr.is_empty());
}
#[test]
fn t_cmd_sign_out_without_session() {
    let output = invoke(&["launch", "sign-out"]);
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"{\n  \"signedOut\": true,\n  \"sessionRemoved\": false\n}\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn t_cli_missing_required_arguments_use_json() {
    for name in [
        "create-launch",
        "get-launch",
        "delete-launch",
        "start-launch-execution",
        "get-launch-status",
        "get-launch-download-url",
    ] {
        let output = invoke(&["launch", name]);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!("{{\"error\": \"Missing required argument for {name}\"}}\n")
        );
    }
}
#[test]
fn t_cli_rejects_bare_dash_as_positional() {
    for name in [
        "create-launch",
        "get-launch",
        "delete-launch",
        "start-launch-execution",
        "get-launch-status",
        "get-launch-download-url",
    ] {
        let output = invoke(&["launch", name, "-"]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}
#[cfg(not(feature = "aws-sdk"))]
#[test]
fn t_cmd_network_commands_fail_without_network_support() {
    for args in [
        &["launch", "get-launch", "l-1"][..],
        &["launch", "delete-launch", "l-1"],
        &["launch", "start-launch-execution", "l-1"],
        &["launch", "get-launch-status", "l-1"],
        &["launch", "list-launches"],
    ] {
        let output = invoke(args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, b"{\"error\": \"This toolkit build has no network support; install a build with the aws-sdk feature.\"}\n");
    }
}

#[test]
fn t_api_create_name_validation_precedes_network() {
    let long_name = "x".repeat(129);
    let output = invoke(&["launch", "create-launch", ".", &long_name]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"{\"error\": \"Invalid launch name: it must be 1 to 128 characters.\"}\n"
    );
}

#[test]
fn t_cmd_create_rejects_credential_bearing_url_without_echoing_credentials() {
    let output = invoke(&[
        "launch",
        "create-launch",
        "https://user:FAKE-URL-SECRET@example.test/repo",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("create-launch takes a local directory"));
    assert!(!diagnostic.contains("FAKE-URL-SECRET"));
}

#[test]
fn t_cli_invalid_id_extra_positionals_and_dash_values() {
    let long = "x".repeat(2049);
    let output = invoke(&["launch", "get-launch", &long]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"{\"error\": \"Invalid launch id: it must be 1 to 2048 characters.\"}\n"
    );
    for args in [
        &["launch", "delete-launch", "a", "b"][..],
        &["launch", "list-launches", "extra"],
        &["launch", "get-launch", "l-1", "--region", "-"],
        &["launch", "get-launch", "l-1", "--region=-"],
        &["launch", "list-launches", "--region"],
        &["launch", "get-launch", "l-1", "--access-key", "AKIAFAKE"],
    ] {
        let output = invoke(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
}

#[cfg(not(feature = "aws-sdk"))]
#[test]
fn t_cmd_every_network_command_validates_locally_then_refuses_without_network() {
    let source = std::env::temp_dir();
    let source = source.to_str().unwrap();
    for args in [
        &["launch", "create-launch", source][..],
        &[
            "launch",
            "create-launch",
            source,
            "name",
            "--region",
            "eu-central-1",
        ],
        &["launch", "get-launch", "l-1", "execution,plan"],
        &["launch", "get-launch-download-url", "l-1"],
        &["launch", "list-launches", "--region", "eu-central-1"],
    ] {
        let output = invoke(args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, b"{\"error\": \"This toolkit build has no network support; install a build with the aws-sdk feature.\"}\n");
    }
}

/// T-TOK-1..5 for the sign-in commands and the helper (#5197): with a seeded
/// session (expired access token, live client), `auth-start` spawns the
/// helper, the browser step is replayed with a seeded code, and `auth-wait`
/// reports the default build's exchange failure. No seeded secret, code or
/// state reaches any stdout or stderr, the pending record, the helper's argv
/// or its environment; the state appears only in the sign-in URL (by design).
#[cfg(all(target_os = "linux", not(feature = "aws-sdk")))]
#[test]
fn t_tok_sign_in_and_helper_matrix() {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    const SECRETS: [&str; 4] = [
        "FAKE-CLIENT-SECRET-0001",
        "FAKE-ACCESS-TOKEN-0001",
        "FAKE-REFRESH-TOKEN-0001",
        "FAKE-AUTH-CODE-0001",
    ];
    let clean = |label: &str, text: &[u8], extra: &[&str]| {
        let text = String::from_utf8_lossy(text);
        for (index, secret) in SECRETS.iter().chain(extra).enumerate() {
            assert!(
                !text.contains(secret),
                "{label}: seeded value #{index} leaked"
            );
        }
    };
    let home = std::env::temp_dir().join(format!(
        "launch-l5-tok-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let state_dir = home.join(".launch-with-aws");
    std::fs::create_dir_all(&state_dir).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let session = format!(
        "{{\"client_id\":\"client\",\"client_secret\":\"{}\",\"client_expires_at\":{},\"authorize_endpoint\":\"https://oidc.us-east-1.amazonaws.com/authorize\",\"token_endpoint\":\"https://oidc.us-east-1.amazonaws.com/token\",\"scopes\":[\"launch:access\"],\"access_token\":\"{}\",\"refresh_token\":\"{}\",\"token_expires_at\":{}}}",
        SECRETS[0],
        now + 3600,
        SECRETS[1],
        SECRETS[2],
        now - 1
    );
    std::fs::write(state_dir.join("session.json"), session).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_toolkit"))
            .args(args)
            .env("HOME", &home)
            .env_remove("LAUNCH_WITH_AWS_BASE_URL")
            .output()
            .unwrap()
    };
    let start = run(&["launch", "auth-start"]);
    assert!(start.status.success());
    clean("auth-start stdout", &start.stdout, &[]);
    clean("auth-start stderr", &start.stderr, &[]);
    let response: serde_json::Value = serde_json::from_slice(&start.stdout).unwrap();
    let pid = response["pid"].as_u64().unwrap();
    let port = u16::try_from(response["port"].as_u64().unwrap()).unwrap();
    let state = response["signInUrl"]
        .as_str()
        .unwrap()
        .split('&')
        .find_map(|v| v.strip_prefix("state="))
        .unwrap()
        .to_owned();
    assert!(state.len() >= 32);
    let argv = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
    clean("helper argv", &argv, &[&state]);
    let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap();
    clean("helper environment", &environ, &[&state]);
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    write!(
        stream,
        "GET /?state={state}&code={} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n",
        SECRETS[3]
    )
    .unwrap();
    let mut page = Vec::new();
    let _ = stream.read_to_end(&mut page);
    clean("callback page", &page, &[&state]);
    let wait = run(&["launch", "auth-wait", &pid.to_string()]);
    assert_eq!(wait.status.code(), Some(1));
    clean("auth-wait stdout", &wait.stdout, &[&state]);
    clean("auth-wait stderr", &wait.stderr, &[&state]);
    assert!(String::from_utf8_lossy(&wait.stderr).starts_with("{\"error\": \""));
    for entry in std::fs::read_dir(&state_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|n| n == "session.json") || !path.is_file() {
            continue;
        }
        clean("pending record", &std::fs::read(&path).unwrap(), &[&state]);
    }
    for args in [&["launch", "session-status"][..], &["launch", "sign-out"]] {
        let output = run(args);
        assert!(output.status.success());
        clean("offline command", &output.stdout, &[&state]);
        clean("offline command", &output.stderr, &[&state]);
    }
    std::fs::remove_dir_all(home).unwrap();
}

/// The stored session secrets of the #5406 matrix.
#[cfg(unix)]
const EARLY_SECRETS: [&str; 3] = [
    "FAKE-ACCESS-TOKEN-0001",
    "FAKE-REFRESH-TOKEN-0001",
    "FAKE-CLIENT-SECRET-0001",
];

/// One early refusal: label, arguments and extra environment.
#[cfg(unix)]
struct EarlyCase {
    label: &'static str,
    args: Vec<String>,
    env: Vec<(&'static str, String)>,
}

/// Every early refusal that echoes a caller or configuration value, with
/// `secret` as that value.
#[cfg(unix)]
fn early_route_cases(secret: &str) -> Vec<EarlyCase> {
    let owned = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
    let no_host = format!("https:///{secret}");
    #[cfg_attr(not(feature = "aws-sdk"), allow(unused_mut))]
    let mut routes: Vec<&[&str]> = vec![
        &["launch", "resolve-region"],
        &["launch", "auth-start"],
        &["launch", "auth-wait", "4242"],
        &["launch", "download-snapshot", "l-1"],
    ];
    #[cfg(feature = "aws-sdk")]
    routes.extend([
        &["launch", "get-launch", "l-1"][..],
        &["launch", "list-launches"],
        &["launch", "create-launch", "."],
    ]);
    let mut cases = Vec::new();
    for route in routes {
        let mut explicit = owned(route);
        explicit.extend(["--region".to_owned(), secret.to_owned()]);
        cases.push(EarlyCase {
            label: "explicit region",
            args: explicit,
            env: vec![],
        });
        cases.push(EarlyCase {
            label: "region environment",
            args: owned(route),
            env: vec![("LAUNCH_WITH_AWS_REGION", secret.to_owned())],
        });
        cases.push(EarlyCase {
            label: "base URL override",
            args: owned(route),
            env: vec![("LAUNCH_WITH_AWS_BASE_URL", no_host.clone())],
        });
    }
    cases.push(EarlyCase {
        label: "repository URL",
        args: owned(&[
            "launch",
            "create-launch",
            &format!("https://github.com/o/{secret}"),
        ]),
        env: vec![],
    });
    cases
}

/// A fresh `$HOME` whose state directory holds a session with an expired
/// access token and a live client (any token work would refresh); returns
/// (home, state dir, session record).
#[cfg(unix)]
fn early_home() -> (std::path::PathBuf, std::path::PathBuf, String) {
    let home = std::env::temp_dir().join(format!(
        "launch-l5-early-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let state_dir = home.join(".launch-with-aws");
    std::fs::create_dir_all(&state_dir).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let session = format!(
        "{{\"client_id\":\"client\",\"client_secret\":\"{}\",\"client_expires_at\":{},\"authorize_endpoint\":\"https://oidc.us-east-1.amazonaws.com/authorize\",\"token_endpoint\":\"https://oidc.us-east-1.amazonaws.com/token\",\"scopes\":[\"launch:access\"],\"access_token\":\"{}\",\"refresh_token\":\"{}\",\"token_expires_at\":{}}}",
        EARLY_SECRETS[2],
        now + 3600,
        EARLY_SECRETS[0],
        EARLY_SECRETS[1],
        now - 1
    );
    std::fs::write(state_dir.join("session.json"), &session).unwrap();
    (home, state_dir, session)
}

/// #5406: with a stored session, a caller or configuration value equal to one
/// of its secrets is never echoed by any launch route's early refusal
/// (region, region environment, base-URL override, repository URL), the
/// route makes no refresh (the session record is unchanged) and starts no
/// sign-in (no other state file appears). Assertions never print the
/// inspected text; every leaking case is named (route, label, secret index),
/// so a red run shows the whole matrix.
#[cfg(unix)]
#[test]
fn t_tok_early_refusals_with_a_stored_session_never_echo_its_secrets() {
    const GENERIC: &str =
        "{\"error\": \"The operation failed. Re-run with logging enabled for details.\"}\n";
    let (home, state_dir, session) = early_home();
    let session_path = state_dir.join("session.json");
    let listing = || {
        let mut names: Vec<_> = std::fs::read_dir(&state_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let before = listing();
    let mut leaked = Vec::new();
    for (number, secret) in EARLY_SECRETS.into_iter().enumerate() {
        for case in early_route_cases(secret) {
            let mut command = Command::new(env!("CARGO_BIN_EXE_toolkit"));
            command
                .args(&case.args)
                .env("HOME", &home)
                .env_remove("LAUNCH_WITH_AWS_REGION")
                .env_remove("LAUNCH_WITH_AWS_BASE_URL")
                .env_remove("AWS_REGION")
                .env_remove("AWS_DEFAULT_REGION")
                .env_remove("AWS_PROFILE");
            for (name, value) in &case.env {
                command.env(name, value);
            }
            let output = command.output().unwrap();
            let (route, label) = (&case.args[1], case.label);
            let shown = [&output.stdout, &output.stderr]
                .map(|text| String::from_utf8_lossy(text).into_owned());
            if shown
                .iter()
                .any(|text| EARLY_SECRETS.iter().any(|s| text.contains(s)))
            {
                leaked.push(format!("{route} {label} (secret #{number})"));
                continue;
            }
            assert!(
                output.status.code() == Some(1)
                    && output.stdout.is_empty()
                    && output.stderr == GENERIC.as_bytes(),
                "{route} {label}: expected the generic failure"
            );
            assert!(
                std::fs::read(&session_path).unwrap() == session.as_bytes(),
                "{route} {label}: the session record changed"
            );
            assert!(listing() == before, "{route} {label}: state files changed");
        }
    }
    std::fs::remove_dir_all(home).unwrap();
    assert!(
        leaked.is_empty(),
        "{} early refusals leaked a seeded secret: {leaked:?}",
        leaked.len()
    );
}

/// [`early_home`] with its client secret replaced by `client_secret` (a
/// proper JSON string, so it may hold a quote).
#[cfg(unix)]
fn early_home_with_client_secret(
    client_secret: &str,
) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let (home, state_dir, session) = early_home();
    let mut record: serde_json::Value = serde_json::from_str(&session).unwrap();
    record["client_secret"] = serde_json::json!(client_secret);
    let session = record.to_string();
    std::fs::write(state_dir.join("session.json"), &session).unwrap();
    (home, state_dir, session)
}

/// `%XX` decoding (either hex case, `+` as space) of `text`.
#[cfg(unix)]
fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                i += 1;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The quoted-value escaping of the launch messages (`text::repr`: `\\`,
/// `\'`, `\"`) undone.
#[cfg(unix)]
fn repr_unescaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('\\', Some(next @ ('\\' | '\'' | '"'))) => {
                out.push(next);
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

/// Whether `secret` can be recovered from `stream`: in its raw text, in any
/// string of a JSON line once decoded, or in either after undoing the
/// message quoting or percent-decoding.
#[cfg(unix)]
fn recoverable(stream: &[u8], secret: &str) -> bool {
    fn strings(value: &serde_json::Value, found: &mut Vec<String>) {
        match value {
            serde_json::Value::String(text) => found.push(text.clone()),
            serde_json::Value::Array(items) => {
                for item in items {
                    strings(item, found);
                }
            }
            serde_json::Value::Object(members) => {
                for (key, member) in members {
                    found.push(key.clone());
                    strings(member, found);
                }
            }
            _ => {}
        }
    }
    let text = String::from_utf8_lossy(stream).into_owned();
    let mut found = vec![text.clone()];
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
        strings(&value, &mut found);
    }
    for line in text.lines() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            strings(&value, &mut found);
        }
    }
    found.iter().any(|t| {
        t.contains(secret)
            || repr_unescaped(t).contains(secret)
            || percent_decoded(t).contains(secret)
    })
}

/// Run each early-refusal case with `home` and assert the #5406/#5426
/// contract: exit 1, empty stdout, the generic failure on stderr, `secret`
/// not recoverable from either stream after JSON- and percent-decoding, and
/// the session record unchanged. Returns the leaking cases (route, label);
/// assertion messages never print the inspected text.
#[cfg(unix)]
fn decoded_refusals(
    home: &std::path::Path,
    session_path: &std::path::Path,
    session: &str,
    secret: &str,
    cases: Vec<EarlyCase>,
) -> Vec<String> {
    const GENERIC: &str =
        "{\"error\": \"The operation failed. Re-run with logging enabled for details.\"}\n";
    let mut leaked = Vec::new();
    for case in cases {
        let mut command = Command::new(env!("CARGO_BIN_EXE_toolkit"));
        command
            .args(&case.args)
            .env("HOME", home)
            .env_remove("LAUNCH_WITH_AWS_REGION")
            .env_remove("LAUNCH_WITH_AWS_BASE_URL")
            .env_remove("AWS_REGION")
            .env_remove("AWS_DEFAULT_REGION")
            .env_remove("AWS_PROFILE");
        for (name, value) in &case.env {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        let (route, label) = (&case.args[1], case.label);
        if recoverable(&output.stdout, secret) || recoverable(&output.stderr, secret) {
            leaked.push(format!("{route} {label}"));
            continue;
        }
        assert!(
            output.status.code() == Some(1)
                && output.stdout.is_empty()
                && output.stderr == GENERIC.as_bytes(),
            "{route} {label}: expected the generic failure"
        );
        assert!(
            std::fs::read(session_path).unwrap() == session.as_bytes(),
            "{route} {label}: the session record changed"
        );
    }
    leaked
}

/// #5426 (reviewer reproducer 1): a stored client secret holding a double
/// quote, passed back as the region (argument or environment) or in a
/// no-host base URL, is JSON-escaped by the error line; it is still refused,
/// and the decoded message does not carry it. The second secret also holds
/// a backslash, which the message quoting doubles and the JSON line doubles
/// again, so only the JSON forms of the sink match it (the first one's
/// JSON escape equals its `"`-quoted message escape).
#[cfg(unix)]
#[test]
fn t_tok_json_escaped_client_secret_is_refused() {
    let mut leaked = Vec::new();
    for (number, secret) in [
        "opaque-client-\"-secret-0001",
        "opaque\\client-\"-secret-0002",
    ]
    .into_iter()
    .enumerate()
    {
        let (home, state_dir, session) = early_home_with_client_secret(secret);
        let cases = early_route_cases(secret)
            .into_iter()
            .filter(|case| case.label != "repository URL")
            .collect();
        leaked.extend(
            decoded_refusals(
                &home,
                &state_dir.join("session.json"),
                &session,
                secret,
                cases,
            )
            .into_iter()
            .map(|case| format!("{case} (secret #{number})")),
        );
        std::fs::remove_dir_all(home).unwrap();
    }
    assert!(
        leaked.is_empty(),
        "{} routes printed a recoverable client secret: {leaked:?}",
        leaked.len()
    );
}

/// #5426 (reviewer reproducer 2): a stored client secret holding `/`, `+`
/// and `=`, percent-encoded (upper- and lower-case hex) in the path of a
/// no-host base URL, is still refused, and the percent-decoded output does
/// not carry it.
#[cfg(unix)]
#[test]
fn t_tok_percent_encoded_client_secret_is_refused() {
    const SECRET: &str = "opaque/client+secret=0001";
    let (home, state_dir, session) = early_home_with_client_secret(SECRET);
    let mut cases = Vec::new();
    for encoded in [
        "opaque%2Fclient%2Bsecret%3D0001",
        "opaque%2fclient%2bsecret%3d0001",
    ] {
        cases.extend(
            early_route_cases(encoded)
                .into_iter()
                .filter(|case| case.label == "base URL override"),
        );
    }
    let leaked = decoded_refusals(
        &home,
        &state_dir.join("session.json"),
        &session,
        SECRET,
        cases,
    );
    std::fs::remove_dir_all(home).unwrap();
    assert!(
        leaked.is_empty(),
        "{} routes printed a recoverable client secret: {leaked:?}",
        leaked.len()
    );
}
