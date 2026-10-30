//! Caller values are redacted where they are rendered, not after (#4941
//! review #4971: #4978, #4979, #4980, #4981).
//!
//! Each test runs the real `toolkit` binary offline (empty environment, stdin
//! closed, a scratch working directory) with a credential-shaped command-line
//! value, and checks what reaches stdout and stderr:
//!
//! - two distinct credentials that share a long prefix are both hidden, in
//!   either argument order (#4978);
//! - a value with mixed single and double quotes is redacted before a
//!   Python-style `repr` or quoting renders it (#4979);
//! - a complete labelled credential is hidden whatever its length (#4980);
//! - JSON stdout stays valid and keeps its typed and fixed fields when a
//!   credential value collides with one of their literals (#4981).

#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use toolkit_core::{parse_json, Json};

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");

const SPAWN_TIMEOUT: Duration = Duration::from_secs(30);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "toolkit-caller-value-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Run {
    stdout: String,
    stderr: String,
}

fn toolkit(args: &[&str]) -> Run {
    let scratch = Scratch::new();
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(&scratch.0)
        .env_clear()
        .env("HOME", &scratch.0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn toolkit");
    let mut out_pipe = child.stdout.take().expect("stdout pipe");
    let mut err_pipe = child.stderr.take().expect("stderr pipe");
    let out_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = out_pipe.read_to_end(&mut bytes);
        bytes
    });
    let err_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = err_pipe.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    loop {
        if child.try_wait().expect("wait").is_some() {
            break;
        }
        assert!(
            started.elapsed() < SPAWN_TIMEOUT,
            "toolkit timed out for {args:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    Run {
        stdout: String::from_utf8_lossy(&out_thread.join().expect("stdout thread")).into_owned(),
        stderr: String::from_utf8_lossy(&err_thread.join().expect("stderr thread")).into_owned(),
    }
}

/// Asserts that no `needle` reaches either stream of `run`.
fn assert_absent(run: &Run, args: &[&str], needles: &[&str]) {
    for (label, stream) in [("stdout", &run.stdout), ("stderr", &run.stderr)] {
        for needle in needles {
            assert!(
                !stream.contains(needle),
                "{needle:?} leaked on {label} for {args:?}:\n{stream}"
            );
        }
    }
}

/// The parsed JSON document `elasticache preflight --json` printed.
fn preflight_json(host: &str) -> Json {
    let args = ["elasticache", "preflight", "--host", host, "--json"];
    let run = toolkit(&args);
    parse_json(&run.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not valid JSON for {args:?} ({error:?}):\n{}",
            run.stdout
        )
    })
}

fn member<'a>(json: &'a Json, path: &[&str]) -> &'a Json {
    let mut current = json;
    for key in path {
        current = current
            .get(key)
            .unwrap_or_else(|| panic!("no {key:?} in {}", current.to_compact_string()));
    }
    current
}

fn first_finding_status(json: &Json) -> &str {
    match member(json, &["findings"]) {
        Json::Array(findings) => findings
            .first()
            .and_then(|finding| finding.get("status"))
            .and_then(Json::as_str)
            .expect("a first finding with a status"),
        other => panic!("findings is not an array: {}", other.to_compact_string()),
    }
}

/// #4978: two credentials sharing a ten-character prefix. The router keeps the
/// last `--host`, but both values are caller text; neither may survive in any
/// part, in either order.
#[test]
fn two_credentials_sharing_a_prefix_are_both_hidden_in_either_order() {
    let longer = "password=SHAREDPFX_ALPHA_LONGER_ONE";
    let shorter = "token=SHAREDPFX_BRAVO_TWO";
    for (first, second) in [(longer, shorter), (shorter, longer)] {
        for json in [true, false] {
            let mut args = vec![
                "elasticache",
                "preflight",
                "--host",
                first,
                "--host",
                second,
            ];
            if json {
                args.push("--json");
            }
            let run = toolkit(&args);
            assert_absent(
                &run,
                &args,
                &["SHAREDPF", "ALPHA", "LONGER_ONE", "BRAVO", "TWO"],
            );
            assert!(
                run.stdout.contains("[REDACTED]"),
                "the host is shown redacted for {args:?}:\n{}",
                run.stdout
            );
        }
    }
}

/// #4979: a value with a single and a double quote, rendered by a Python
/// `repr` (`msk`) or by quoting (`elasticache price`, `aurora`).
#[test]
fn a_value_with_mixed_quotes_is_redacted_before_it_is_quoted() {
    let value = "password=ab'c\"QUOTED_SECRET";
    for args in [
        ["msk", "size", "--avg-data-in-mbs", value],
        ["msk", "size", "--num-partitions", value],
        ["msk", "size", "--pricing-source", value],
        ["elasticache", "price", "--engine", value],
        ["elasticache", "price", "--mode", value],
        ["aurora", "io", "--engine", value],
    ] {
        let run = toolkit(&args);
        assert_absent(&run, &args, &["QUOTED_SE", "ab\\'c", "ab'c", "c\"QUOTED"]);
        assert!(
            run.stderr.contains("[REDACTED]"),
            "the value is reported redacted for {args:?}:\n{}",
            run.stderr
        );
    }
    // The same quotes without a credential are still rendered as before.
    let run = toolkit(&["msk", "size", "--avg-data-in-mbs", "ab'c\"d"]);
    assert!(
        run.stderr
            .contains("argument --avg-data-in-mbs: invalid float value: 'ab\\'c\"d'"),
        "{}",
        run.stderr
    );
}

/// #4980: a complete labelled credential of one, two or three characters is
/// hidden on stdout and stderr.
#[test]
fn a_short_labelled_credential_is_hidden_on_every_stream() {
    for value in [
        "password=abc",
        "token=ab",
        "password=x",
        "secret=Qz",
        "api_key=k9z",
    ] {
        for args in [
            vec!["elasticache", "preflight", "--host", value, "--json"],
            vec!["elasticache", "preflight", "--host", value],
            vec!["msk", "size", "--avg-data-in-mbs", value],
            vec!["elasticache", "price", "--engine", value],
        ] {
            let run = toolkit(&args);
            assert_absent(&run, &args, &[value]);
        }
    }
}

/// #4981: the host value collides with the numeric port and with the fixed
/// `FAIL` status. The JSON stays valid, `port` stays the number 6379, the
/// connectivity status stays `FAIL` and only the host is redacted.
#[test]
fn json_stdout_keeps_its_schema_when_a_credential_collides_with_a_literal() {
    let json = preflight_json("password=6379");
    assert_eq!(member(&json, &["info", "port"]), &Json::Int(6379));
    assert_eq!(
        member(&json, &["info", "host"]).as_str(),
        Some("password=[REDACTED]")
    );
    assert_eq!(first_finding_status(&json), "FAIL");

    let json = preflight_json("token=FAIL");
    assert_eq!(first_finding_status(&json), "FAIL");
    assert_eq!(member(&json, &["info", "port"]), &Json::Int(6379));
    assert_eq!(
        member(&json, &["info", "host"]).as_str(),
        Some("token=[REDACTED]")
    );

    // An ordinary host is printed exactly as given.
    let json = preflight_json("redis.example.internal");
    assert_eq!(
        member(&json, &["info", "host"]).as_str(),
        Some("redis.example.internal")
    );
}

/// The process-level property: several distinct credentials that overlap one
/// another (shared prefixes of different lengths, different labels), passed
/// in every order, leave no distinctive part of any of them in the output.
#[test]
fn several_overlapping_credentials_leave_nothing_in_any_order() {
    let values = [
        "password=OVERLAPXX_ALPHA_ONE_LONG",
        "token=OVERLAPXX_ALPHA_TWO",
        "secret=OVERLAPXX_BETA3",
        "api_key=OVERLAP_FOUR",
    ];
    let markers = [
        "OVERLAPX", "ALPHA", "ONE_LONG", "BETA3", "_TWO", "_FOUR", "OVERLAP_",
    ];
    let orders: [[usize; 4]; 4] = [[0, 1, 2, 3], [3, 2, 1, 0], [1, 3, 0, 2], [2, 0, 3, 1]];
    for order in orders {
        let mut args = vec!["elasticache", "preflight"];
        for index in order {
            args.extend(["--host", values[index]]);
        }
        args.push("--json");
        let run = toolkit(&args);
        assert_absent(&run, &args, &markers);
        // A repr-rendered value and a plain one, in each order.
        let mut args = vec!["msk", "size"];
        for (flag, index) in ["--avg-data-in-mbs", "--peak-data-in-mbs"]
            .into_iter()
            .zip(order)
        {
            args.extend([flag, values[index]]);
        }
        let run = toolkit(&args);
        assert_absent(&run, &args, &markers);
    }
}

/// #4978's exact reported input, in both orders.
#[test]
fn the_reported_shared_prefix_input_leaves_no_suffix() {
    let longer = "password=xxxxxxxxOTHER_VALUE_MUCH_LONGER";
    let shorter = "token=xxxxxxxxFAKE_SECRET";
    for (first, second) in [(longer, shorter), (shorter, longer)] {
        let args = [
            "elasticache",
            "preflight",
            "--host",
            first,
            "--host",
            second,
            "--json",
        ];
        let run = toolkit(&args);
        assert_absent(
            &run,
            &args,
            &["xxxxxxxx", "FAKE_SECRET", "OTHER_VALUE", "MUCH_LONGER"],
        );
    }
}

/// #4979's exact reported input, and the route the value rule alone misses:
/// the flag repeated with the bare credential, which reaches `repr` as an
/// ordinary value and gets its quote escaped.
#[test]
fn a_repeated_flag_cannot_show_an_escaped_credential() {
    let labelled = "password=ab'c\"FAKE_SECRET";
    let bare = "ab'c\"FAKE_SECRET";
    for (command, flag) in [
        (&["msk", "size"][..], "--avg-data-in-mbs"),
        (&["elasticache", "price"][..], "--engine"),
        (&["aurora", "io"][..], "--engine"),
    ] {
        for values in [&[labelled][..], &[labelled, bare], &[bare, labelled]] {
            let mut args = command.to_vec();
            for value in values {
                args.extend([flag, value]);
            }
            let run = toolkit(&args);
            assert_absent(&run, &args, &["FAKE_SECRET", "ab\\'c", "ab'c"]);
            assert!(
                run.stderr.contains("[REDACTED]"),
                "{args:?}: {}",
                run.stderr
            );
        }
    }
}

/// #4979's reported `elasticache price` input keeps the fixed text around the
/// value: only the credential is hidden, not letters of the choices.
#[test]
fn a_credential_with_quotes_leaves_the_diagnostic_readable() {
    let args = [
        "elasticache",
        "price",
        "--engine",
        "password=ab'c\"FAKE_SECRET",
    ];
    let run = toolkit(&args);
    let first_line = run.stderr.lines().next().unwrap_or_default();
    assert_eq!(
        first_line,
        "error: argument --engine: invalid choice: 'password=[REDACTED]' \
         (choose from 'valkey', 'redis', 'memcached')"
    );
}

/// #4980 across arguments: the bare piece (one to three characters and
/// longer) of a credential another argument taught, on both streams.
#[test]
fn a_repeated_flag_cannot_show_a_bare_credential() {
    for (labelled, bare) in [
        ("password=Q", "Q"),
        ("token=Qz", "Qz"),
        ("password=Qz9", "Qz9"),
        ("https://Qz9@h.invalid", "Qz9"),
        ("https://u:p@h.invalid", "u:p"),
        ("token=FAKE_SECRET", "FAKE_SECRET"),
        ("https://user:FAKE_SECRET@h.invalid", "FAKE_SECRET"),
    ] {
        for values in [[labelled, bare], [bare, labelled]] {
            let host = [
                "elasticache",
                "preflight",
                "--host",
                values[0],
                "--host",
                values[1],
                "--json",
            ];
            let run = toolkit(&host);
            assert_absent(&run, &host, &[bare]);
            let size = [
                "msk",
                "size",
                "--avg-data-in-mbs",
                values[0],
                "--avg-data-in-mbs",
                values[1],
            ];
            let run = toolkit(&size);
            let first_line = run.stderr.lines().next().unwrap_or_default();
            assert!(!first_line.contains(bare), "{size:?}: {first_line}");
        }
    }
}

/// #4980 against #4981: the mask is never weakened and the document still
/// parses, because it applies to caller strings where they are built. Port
/// and status keep their values when the bare credential equals them.
#[test]
fn hiding_a_short_credential_keeps_the_json_schema() {
    for (labelled, bare) in [
        ("password=6379", "6379"),
        ("password=6", "6"),
        ("token=FAIL", "FAIL"),
        ("token=F", "F"),
    ] {
        for values in [[labelled, bare], [bare, labelled]] {
            let args = [
                "elasticache",
                "preflight",
                "--host",
                values[0],
                "--host",
                values[1],
                "--json",
            ];
            let run = toolkit(&args);
            let json = parse_json(&run.stdout).unwrap_or_else(|error| {
                panic!("not JSON for {args:?} ({error:?}):\n{}", run.stdout)
            });
            assert_eq!(
                member(&json, &["info", "port"]),
                &Json::Int(6379),
                "{args:?}"
            );
            assert_eq!(first_finding_status(&json), "FAIL", "{args:?}");
            let host = member(&json, &["info", "host"]).as_str().expect("a host");
            assert!(host.contains("[REDACTED]"), "{args:?}: {host}");
            assert!(!host.contains(bare), "{args:?}: {host}");
        }
    }
}

/// #4981 on the one JSON document of stderr: `msk sim`'s `{"error": ...}` keeps
/// its key and stays valid when a credential equals a part of its syntax.
#[test]
fn a_json_error_on_stderr_keeps_its_schema() {
    for value in ["password=error", "password=e", "password=:", "password=\\"] {
        let args = ["msk", "sim", "compute", "--instance-type", value];
        let run = toolkit(&args);
        let json = parse_json(run.stderr.trim())
            .unwrap_or_else(|error| panic!("not JSON for {args:?} ({error:?}):\n{}", run.stderr));
        let message = member(&json, &["error"]).as_str().expect("an error string");
        assert!(message.contains("[REDACTED]"), "{args:?}: {message}");
    }
    // A bare credential taught by the first occurrence is masked inside it.
    let args = [
        "msk",
        "sim",
        "compute",
        "--instance-type",
        "password=FAKE_SECRET",
        "--instance-type",
        "FAKE_SECRET",
    ];
    let run = toolkit(&args);
    assert_absent(&run, &args, &["FAKE_SECRET"]);
    let json = parse_json(run.stderr.trim()).expect("a JSON error");
    assert!(member(&json, &["error"]).as_str().is_some());
}

/// #5012's exact reachable input at N = 60000, in both orders, through the real
/// binary. The operation count is pinned by the unit tests; here the bound only
/// rules out the quadratic rescan, which would take far longer than this.
#[test]
fn the_reported_overlap_input_finishes_and_hides_the_credential() {
    let secret = "a".repeat(60_000);
    let labelled = format!("password={secret}");
    for (first, second) in [
        (labelled.as_str(), secret.as_str()),
        (secret.as_str(), labelled.as_str()),
    ] {
        let args = [
            "msk",
            "size",
            "--discount-pct",
            first,
            "--discount-pct",
            second,
        ];
        let run = toolkit(&args);
        assert_absent(&run, &["msk", "size"], &["aaaaaaaa"]);
        assert!(run.stderr.contains("[REDACTED]"), "{:.300}", run.stderr);
    }
}

/// #5620: normalization must retain credentials made only of backslashes.
#[test]
fn backslash_only_credentials_are_hidden_on_stdout_in_both_orders() {
    for count in [1, 2, 3, 4, 9] {
        let bare = "\\".repeat(count);
        let labelled = format!("password={bare}");
        for values in [
            [labelled.as_str(), bare.as_str()],
            [bare.as_str(), labelled.as_str()],
        ] {
            let run = toolkit(&[
                "elasticache",
                "preflight",
                "--host",
                values[0],
                "--host",
                values[1],
                "--json",
            ]);
            let json = parse_json(&run.stdout).expect("preflight JSON");
            let host = member(&json, &["info", "host"]).as_str().expect("host");
            assert!(!host.contains('\\'), "{count}: {host}");
            assert!(host.contains("[REDACTED]"), "{host}");
            assert_eq!(member(&json, &["info", "port"]), &Json::Int(6379));
        }
    }
}

#[test]
fn backslash_only_credentials_are_hidden_on_stderr_in_both_orders() {
    for count in [1, 2, 3, 4, 9] {
        let bare = "\\".repeat(count);
        let labelled = format!("password={bare}");
        for values in [
            [labelled.as_str(), bare.as_str()],
            [bare.as_str(), labelled.as_str()],
        ] {
            let run = toolkit(&[
                "msk",
                "size",
                "--avg-data-in-mbs",
                values[0],
                "--avg-data-in-mbs",
                values[1],
            ]);
            let error = run.stderr.lines().next().expect("diagnostic");
            assert!(!error.contains('\\'), "{count}: {error}");
            assert!(error.contains("[REDACTED]"), "{error}");
        }
    }
}

/// The #5621 class audit found Launch's error writer also masked JSON syntax.
#[test]
fn launch_json_errors_keep_their_key_and_mask_the_message() {
    for secret in ["error", "e"] {
        let labelled = format!("token={secret}");
        for values in [[labelled.as_str(), secret], [secret, labelled.as_str()]] {
            let run = toolkit(&[
                "launch",
                "resolve-region",
                "--region",
                values[0],
                "--region",
                values[1],
            ]);
            assert!(run.stdout.is_empty(), "{}", run.stdout);
            let json = parse_json(run.stderr.trim()).expect("Launch error JSON");
            let message = member(&json, &["error"]).as_str().expect("error message");
            assert!(message.contains("[REDACTED]"), "{message}");
            assert!(!message.contains(secret), "{message}");
        }
    }
}
