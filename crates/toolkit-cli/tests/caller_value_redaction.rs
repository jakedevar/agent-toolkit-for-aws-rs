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
