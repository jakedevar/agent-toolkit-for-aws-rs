//! Success-path routes that rendered caller values without redaction (#4941
//! review #4971 round 2: #5008, #5009, #5010, #5011).
//!
//! Each test runs the real `toolkit` binary offline (empty environment, stdin
//! closed, a scratch working directory holding any input file the command
//! needs) far enough to reach the route, and checks what reaches stdout and
//! stderr:
//!
//! - a `--node-ids` list whose URL userinfo holds a comma is redacted whole
//!   before it is split into node ids (#5008);
//! - `dynamodb iterate-design` announces each step and an applied change
//!   without the credential in a model file name, and still runs the step on
//!   the real path (#5009);
//! - `msk sim render` reports its output path and deploy hint redacted, and
//!   still writes the file at the path as given (#5010);
//! - `msk sim render` redacts its Kafka version and instance type before a
//!   parser error quotes them (#5011).
//!
//! No AWS call, deployment or spend flag is involved: every route here is
//! local.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use toolkit_core::{parse_json, Json};

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");

const SPAWN_TIMEOUT: Duration = Duration::from_secs(30);

const REDACTED: &str = "[REDACTED]";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "toolkit-caller-route-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch");
        Self(path)
    }

    fn file(&self, name: &str, contents: &str) -> String {
        let path = self.0.join(name);
        fs::write(&path, contents).expect("write scratch file");
        path.display().to_string()
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).display().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Run {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

fn toolkit(args: &[&str], cwd: &Path) -> Run {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", cwd)
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
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status.code();
        }
        assert!(
            started.elapsed() < SPAWN_TIMEOUT,
            "toolkit timed out for {args:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    Run {
        status,
        stdout: String::from_utf8_lossy(&out_thread.join().expect("stdout thread")).into_owned(),
        stderr: String::from_utf8_lossy(&err_thread.join().expect("stderr thread")).into_owned(),
    }
}

/// Asserts that no `needle` reaches either stream of `run`, in any letter
/// case (a route may lowercase a value before it echoes it).
fn assert_absent(run: &Run, args: &[&str], needles: &[&str]) {
    for (label, stream) in [("stdout", &run.stdout), ("stderr", &run.stderr)] {
        let lower = stream.to_lowercase();
        for needle in needles {
            assert!(
                !lower.contains(&needle.to_lowercase()),
                "{needle:?} leaked on {label} for {args:?}:\n{stream}"
            );
        }
    }
}

fn json(text: &str) -> Json {
    parse_json(text).unwrap_or_else(|error| panic!("stdout is not JSON ({error:?}):\n{text}"))
}

/// Every string in `value`, nested JSON documents in strings included (the
/// dashboard body is a JSON document inside a JSON string).
fn strings(value: &Json, out: &mut Vec<String>) {
    match value {
        Json::String(text) => {
            if let Ok(inner @ (Json::Object(_) | Json::Array(_))) = parse_json(text) {
                strings(&inner, out);
            }
            out.push(text.clone());
        }
        Json::Array(items) => {
            for item in items {
                strings(item, out);
            }
        }
        Json::Object(pairs) => {
            for (key, item) in pairs {
                out.push(key.clone());
                strings(item, out);
            }
        }
        _ => {}
    }
}

/// #5008: the comma inside the userinfo used to split the URL from its
/// `@host` before redaction, so `https://user:SECRET` was written into the
/// dashboard as a node id. The whole value is redacted first.
#[test]
fn a_node_id_list_is_redacted_whole_before_it_is_split() {
    let scratch = Scratch::new();
    for node_ids in [
        "https://user:NODE_SECRET_HEAD,NODE_SECRET_TAIL@example.invalid/x",
        "n1,https://user:NODE_SECRET_HEAD,NODE_SECRET_TAIL@example.invalid/x,n2",
        "n1, password=NODE_SECRET_HEAD ,n2",
    ] {
        let args = [
            "elasticache",
            "dashboard",
            "--replication-group",
            "ordinary",
            "--node-ids",
            node_ids,
            "--no-alarms",
        ];
        let run = toolkit(&args, &scratch.0);
        assert_eq!(run.status, Some(0), "{args:?}: {}", run.stderr);
        assert_absent(&run, &args, &["NODE_SECRET", "user:"]);
        let mut found = Vec::new();
        strings(&json(&run.stdout), &mut found);
        assert!(
            found.iter().any(|text| text.contains(REDACTED)),
            "{args:?}: the redacted node id is still rendered:\n{}",
            run.stdout
        );
        for text in &found {
            assert!(!text.contains("NODE_SECRET"), "{text}");
        }
    }
    // An ordinary list is split and trimmed exactly as before.
    let args = [
        "elasticache",
        "dashboard",
        "--replication-group",
        "ordinary",
        "--node-ids",
        " n1 ,n2,,",
        "--no-alarms",
    ];
    let run = toolkit(&args, &scratch.0);
    assert_eq!(run.status, Some(0), "{}", run.stderr);
    let mut found = Vec::new();
    strings(&json(&run.stdout), &mut found);
    assert!(found.iter().any(|text| text == "n1"), "{}", run.stdout);
    assert!(found.iter().any(|text| text == "n2"), "{}", run.stdout);
    assert!(!found.iter().any(|text| text.contains(" n1")));
}

/// The iterate-design inputs of a calculator-only round whose model file
/// name holds a credential.
fn iterate_inputs(scratch: &Scratch) -> (String, String, String, String) {
    (
        scratch.file("password=ITER_SECRET.json", "{}"),
        scratch.file("config.json", "{}"),
        scratch.path("state.json"),
        scratch.path("manifest.json"),
    )
}

/// #5009: the step announcement printed the step argv raw on stdout. It is
/// shown redacted; the step itself still runs on the real model path (its
/// own failure names the missing access patterns, so it read the file).
#[test]
fn iterate_design_announces_its_steps_without_the_credential() {
    let scratch = Scratch::new();
    let (model, config, state, manifest) = iterate_inputs(&scratch);
    let args = [
        "dynamodb",
        "iterate-design",
        "--model",
        &model,
        "--config",
        &config,
        "--loop-state",
        &state,
        "--manifest",
        &manifest,
        "--calculator-only",
    ];
    let run = toolkit(&args, &scratch.0);
    assert_absent(&run, &args, &["ITER_SECRET"]);
    let announced = run
        .stdout
        .lines()
        .find(|line| line.starts_with("[iterate_design] calculate_costs"))
        .unwrap_or_else(|| panic!("no step announcement:\n{}\n{}", run.stdout, run.stderr));
    assert!(
        announced.contains("--model ") && announced.contains(&format!("password={REDACTED}")),
        "{announced}"
    );
    assert_ne!(run.status, Some(0), "an empty model has no access patterns");
    // The loop state keeps the real model path for the next round (D11).
    let saved = fs::read_to_string(&state).unwrap_or_default();
    assert!(
        saved.is_empty() || saved.contains("ITER_SECRET"),
        "the functional state must keep the real path: {saved}"
    );
}

/// #5009 (line 1219): the applied-change line names the model file.
#[test]
fn iterate_design_names_an_applied_change_without_the_credential() {
    let scratch = Scratch::new();
    let (model, config, state, manifest) = iterate_inputs(&scratch);
    let change = scratch.file("change.json", "{\"merge\": {}}");
    let args = [
        "dynamodb",
        "iterate-design",
        "--model",
        &model,
        "--config",
        &config,
        "--loop-state",
        &state,
        "--manifest",
        &manifest,
        "--apply-change",
        &change,
        "--calculator-only",
    ];
    let run = toolkit(&args, &scratch.0);
    assert_absent(&run, &args, &["ITER_SECRET"]);
    assert!(
        run.stdout.contains(&format!(
            "[iterate_design] applied change to password={REDACTED}"
        )),
        "{}\n{}",
        run.stdout,
        run.stderr
    );
    // An ordinary model name is announced as before.
    let plain = scratch.file("model.json", "{}");
    let args = [
        "dynamodb",
        "iterate-design",
        "--model",
        &plain,
        "--config",
        &config,
        "--loop-state",
        &scratch.path("state2.json"),
        "--manifest",
        &manifest,
        "--apply-change",
        &change,
        "--calculator-only",
    ];
    let run = toolkit(&args, &scratch.0);
    assert!(
        run.stdout
            .contains("[iterate_design] applied change to model.json\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains(&format!(
            "[iterate_design] calculate_costs (calculator-only): toolkit dynamodb calculate-costs --model {plain} --output "
        )),
        "{}",
        run.stdout
    );
}

/// `msk sim render` with valid sizing and `output` as the output path.
fn render_args(kafka_version: &str, instance_type: &str, output: &str) -> Vec<String> {
    [
        "msk",
        "sim",
        "render",
        "--instance-type",
        instance_type,
        "--broker-count",
        "3",
        "--kafka-version",
        kafka_version,
        "--output",
        output,
    ]
    .map(String::from)
    .to_vec()
}

/// #5010: the output path and the deploy hint are shown redacted; the file is
/// still written at the path as given, and the report keeps its schema.
#[test]
fn msk_sim_render_reports_its_output_path_without_the_credential() {
    let scratch = Scratch::new();
    let plain = scratch.path("ordinary.yaml");
    let args = render_args("3.8.x", "express.m7g.large", &plain);
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let run = toolkit(&words, &scratch.0);
    assert_eq!(run.status, Some(0), "{}", run.stderr);
    let ordinary = json(&run.stdout);
    let shown = ordinary
        .get("output_path")
        .and_then(Json::as_str)
        .expect("output_path");
    assert!(shown.ends_with("/ordinary.yaml"), "{shown}");

    let secret = scratch.path("password=RENDER_SECRET.yaml");
    let args = render_args("3.8.x", "express.m7g.large", &secret);
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let run = toolkit(&words, &scratch.0);
    assert_eq!(run.status, Some(0), "{}", run.stderr);
    assert_absent(&run, &words, &["RENDER_SECRET"]);
    assert!(Path::new(&secret).is_file(), "the file is written as given");
    let report = json(&run.stdout);
    let shown = report
        .get("output_path")
        .and_then(Json::as_str)
        .expect("output_path is a string");
    assert!(shown.ends_with(&format!("/password={REDACTED}")), "{shown}");
    let deploy = report
        .get("deploy_command")
        .and_then(Json::as_str)
        .expect("deploy_command is a string");
    assert!(deploy.contains(&format!("file://{shown} ")), "{deploy}");
    // Same members, same types, as the ordinary report.
    let (Json::Object(left), Json::Object(right)) = (&ordinary, &report) else {
        panic!("reports are objects");
    };
    let shape = |pairs: &Vec<(String, Json)>| {
        pairs
            .iter()
            .map(|(key, value)| (key.clone(), std::mem::discriminant(value)))
            .collect::<Vec<_>>()
    };
    assert_eq!(shape(left), shape(right));
}

/// #5011: a mixed-quote credential given as the Kafka version or the
/// instance type is redacted before the parser error quotes it, as
/// `msk sim compute` does. No file is written.
#[test]
fn msk_sim_render_redacts_its_parsed_values_before_quoting_them() {
    let scratch = Scratch::new();
    let output = scratch.path("never.yaml");
    let value = "password=ab'c\"RENDER_QUOTED_SECRET";
    for args in [
        render_args(value, "express.m7g.large", &output),
        render_args("3.8.x", value, &output),
        render_args("3.8.x", "token=Zq9", &output),
    ] {
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        let run = toolkit(&words, &scratch.0);
        assert_eq!(run.status, Some(2), "{words:?}: {}", run.stderr);
        assert_absent(&run, &words, &["QUOTED_SECRET", "SECRET", "Zq9"]);
        assert!(run.stderr.contains(REDACTED), "{}", run.stderr);
        assert!(!Path::new(&output).exists());
    }
    // An ordinary invalid version is quoted exactly as before.
    let args = render_args("3.9.x", "express.m7g.large", &output);
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let run = toolkit(&words, &scratch.0);
    assert!(
        run.stderr.contains("unsupported kafka_version '3.9.x'"),
        "{}",
        run.stderr
    );
}
