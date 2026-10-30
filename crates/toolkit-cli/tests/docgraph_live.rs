#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::process::Command;

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .env_clear()
        .env("NEPTUNE_SEED_CONFIRM_WIPE", "1")
        .env("TOOLKIT_DOCGRAPH_LIVE", "1")
        .env("TOOLKIT_DOCGRAPH_CONFIRM_WRITE", "1")
        .env("TOOLKIT_DOCGRAPH_CONFIRM_DELETE", "1")
        .env("TOOLKIT_DOCGRAPH_CONFIRM_CREATE", "1")
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn environment_cannot_select_live_mode() {
    for args in [
        &["neptune", "seed", "--dry-run", "--confirm-wipe"][..],
        &["neptune", "memory", "--operation", "retrieve"][..],
        &["documentdb", "wa-review"][..],
    ] {
        let clean = Command::new(env!("CARGO_BIN_EXE_toolkit"))
            .env_clear()
            .args(args)
            .output()
            .unwrap();
        let (status, stdout, stderr) = run(args);
        assert_eq!(Some(status), clean.status.code());
        assert_eq!(stdout.as_bytes(), clean.stdout);
        assert_eq!(stderr.as_bytes(), clean.stderr);
    }
}

#[test]
#[cfg(feature = "docgraph-live")]
fn live_wipe_ignores_ambient_confirmation() {
    let (status, stdout, stderr) = run(&[
        "neptune",
        "seed",
        "--live",
        "--endpoint",
        SEED_HOST,
        "--network-policy",
        "/does/not/exist",
    ]);
    assert_eq!(status, 1);
    assert!(stdout.is_empty());
    assert!(stderr.contains("Re-run with --confirm-wipe."));
    assert!(!stderr.contains("NEPTUNE_SEED_CONFIRM_WIPE"));
    assert!(!stderr.contains("/does/not/exist"));
}

#[test]
#[cfg(feature = "docgraph-live")]
fn live_consent_precedes_input_files_and_only_argv_counts() {
    for (op, required) in [
        ("query", "--confirm-write"),
        ("consolidate", "--confirm-delete"),
        ("create-memory", "--confirm-create"),
        ("store-message", "--confirm-write"),
    ] {
        let (status, stdout, stderr) = run(&[
            "neptune",
            "memory",
            "--live",
            "--operation",
            op,
            "--input",
            "/does/not/exist",
        ]);
        assert_eq!(status, 1, "{op}: {stderr}");
        assert!(stdout.is_empty());
        assert!(stderr.contains(required), "{op}: {stderr}");
        assert!(!stderr.contains("/does/not/exist"));
    }
    let (status, _, stderr) = run(&[
        "neptune",
        "memory",
        "--live",
        "--operation",
        "query",
        "--confirm-write",
        "--input",
        "/does/not/exist",
    ]);
    assert_eq!(status, 1);
    assert!(stderr.contains("--confirm-delete"));
}

#[test]
#[cfg(feature = "docgraph-live")]
fn live_memory_requires_consent_for_every_mutating_operation() {
    for (operation, flag) in [
        ("query", "--confirm-write"),
        ("consolidate", "--confirm-delete"),
        ("create-graphrag", "--confirm-create"),
        ("create-memory", "--confirm-create"),
        ("remember-relationship", "--confirm-write"),
        ("remember-entity", "--confirm-write"),
        ("store-message", "--confirm-write"),
        ("ingest-document", "--confirm-write"),
        ("store-embedding", "--confirm-write"),
        ("link-chunk-entity", "--confirm-write"),
        ("link-entities", "--confirm-write"),
        ("remember-embedding", "--confirm-write"),
        ("ingest-entity", "--confirm-write"),
        ("ingest-chunk", "--confirm-write"),
    ] {
        let (status, stdout, stderr) = run(&[
            "neptune",
            "memory",
            "--live",
            "--operation",
            operation,
            "--input",
            "/does/not/exist",
            "--network-policy",
            "/also/does/not/exist",
        ]);
        assert_eq!(status, 1, "{operation}: {stderr}");
        assert!(stdout.is_empty(), "{operation}: {stdout}");
        assert!(stderr.contains(flag), "{operation}: {stderr}");
        assert!(!stderr.contains("/does/not/exist"));
        assert!(!stderr.contains("/also/does/not/exist"));
    }
}

#[test]
#[cfg(feature = "docgraph-live")]
fn live_rejects_recorded_and_dry_run_before_files() {
    for args in [
        &[
            "documentdb",
            "wa-review",
            "--live",
            "--recorded",
            "/does/not/exist",
        ][..],
        &["neptune", "seed", "--live", "--recorded", "/does/not/exist"][..],
        &[
            "neptune",
            "memory",
            "--live",
            "--dry-run",
            "--input",
            "/does/not/exist",
        ][..],
    ] {
        let (status, _, stderr) = run(args);
        assert_eq!(status, 2, "{stderr}");
        assert!(!stderr.contains("/does/not/exist"));
    }
}

#[test]
#[cfg(not(feature = "docgraph-live"))]
fn live_selection_is_unavailable_without_feature() {
    for args in [
        &["neptune", "seed", "--live"][..],
        &["neptune", "memory", "--live"][..],
        &["documentdb", "wa-review", "--live"][..],
    ] {
        let (status, stdout, stderr) = run(args);
        assert_eq!(status, 2);
        assert!(stdout.is_empty());
        assert!(stderr.contains("built without docgraph-live"), "{stderr}");
    }
}

#[test]
#[cfg(not(feature = "docgraph-live"))]
fn review_without_recording_fails_before_writing_reports() {
    let (status, stdout, stderr) = run(&[
        "documentdb",
        "wa-review",
        "--cluster-id",
        "example",
        "--region",
        "us-east-1",
        "--output",
        "/does/not/exist",
    ]);
    assert_eq!(status, 1, "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("built without docgraph-live"), "{stderr}");
    assert!(!stderr.contains("/does/not/exist"), "{stderr}");
}

#[test]
#[cfg(not(feature = "docgraph-live"))]
fn neptune_without_recording_fails_before_input_or_network() {
    for args in [
        &["neptune", "seed", "--confirm-wipe"][..],
        &[
            "neptune",
            "memory",
            "--operation",
            "retrieve",
            "--input",
            "/does/not/exist",
        ][..],
    ] {
        let (status, stdout, stderr) = run(args);
        assert_eq!(status, 1, "{stderr}");
        assert!(stdout.is_empty(), "{stdout}");
        assert!(stderr.contains("built without docgraph-live"), "{stderr}");
        assert!(!stderr.contains("/does/not/exist"), "{stderr}");
    }
}

// ---- T-DG-05c/d (S3, #5515): live seed consent, then the policy, then the network ----

#[test]
#[cfg(feature = "docgraph-live")]
fn live_seed_requires_standard_endpoint_before_side_effects() {
    let network = ForbiddenNetwork::start();
    let fake = format!("127.0.0.1:{}", network.port);
    let dir = std::env::temp_dir().join(format!("docgraph-01b-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for endpoint in [
        None,
        Some("localhost"),
        Some("127.0.0.1"),
        Some("example.com"),
        Some("db.us-east-1.neptune-graph.amazonaws.com"),
        Some("db.us-east-1.neptune.amazonaws.com:8182"),
        Some("DB.us-east-1.neptune.amazonaws.com"),
        Some("db.invalid.neptune.amazonaws.com"),
    ] {
        for confirm in [false, true] {
            let mut args = vec![
                "neptune",
                "seed",
                "--live",
                "--network-policy",
                "/does/not/exist",
            ];
            if confirm {
                args.push("--confirm-wipe");
            }
            if let Some(host) = endpoint {
                args.extend(["--endpoint", host]);
            }
            let out = run_hostile(&args, &dir, &fake);
            let stderr = String::from_utf8(out.stderr).unwrap();
            assert_eq!(
                out.status.code(),
                Some(2),
                "{endpoint:?}, {confirm}: {stderr}"
            );
            assert!(out.stdout.is_empty());
            assert!(stderr.contains("--endpoint"), "{stderr}");
            assert!(!stderr.contains("/does/not/exist"));
        }
    }
    assert_eq!(network.accepted(), 0);
    assert!(provider_never_ran(&dir));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(feature = "docgraph-live")]
const SEED_HOST: &str = "db.cluster-abc.us-east-1.neptune.amazonaws.com";

#[cfg(feature = "docgraph-live")]
fn policy_file(name: &str, body: &str) -> String {
    let dir = std::env::temp_dir().join(format!("docgraph-s3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
#[cfg(feature = "docgraph-live")]
fn live_seed_consent_shaped_policy_keys_and_ambient_wipe_confirm_nothing() {
    let consent = policy_file(
        "consent.json",
        &format!(
            r#"{{"host":"{SEED_HOST}","port":8182,"region":"us-east-1","live":true,"confirm_wipe":true}}"#
        ),
    );
    // NEPTUNE_SEED_CONFIRM_WIPE=1 is set by `run`; the policy cannot confirm.
    let (status, stdout, stderr) = run(&[
        "neptune",
        "seed",
        "--live",
        "--endpoint",
        SEED_HOST,
        "--network-policy",
        &consent,
    ]);
    assert_eq!(status, 1, "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("Refusing to wipe the graph"), "{stderr}");
    assert!(stderr.contains("Re-run with --confirm-wipe."), "{stderr}");
    assert!(!stderr.contains("NEPTUNE_SEED_CONFIRM_WIPE"), "{stderr}");
    // With argv consent the policy is read, and its consent-shaped keys are
    // refused before any connection is attempted.
    let (status, stdout, stderr) = run(&[
        "neptune",
        "seed",
        "--live",
        "--confirm-wipe",
        "--endpoint",
        SEED_HOST,
        "--network-policy",
        &consent,
    ]);
    assert_eq!(status, 1, "{stderr}");
    assert!(stdout.is_empty(), "nothing ran: {stdout}");
    assert!(stderr.contains("unknown key"), "{stderr}");
    assert!(!stderr.contains(&consent), "{stderr}");
}

#[test]
#[cfg(feature = "docgraph-live")]
fn live_seed_refuses_policy_mismatches_before_credentials_or_dns() {
    let valid = policy_file(
        "valid.json",
        &format!(r#"{{"host":"{SEED_HOST}","port":8182,"region":"us-east-1"}}"#),
    );
    let other_port = policy_file(
        "port.json",
        &format!(r#"{{"host":"{SEED_HOST}","port":8183,"region":"us-east-1"}}"#),
    );
    let other_host = "db.cluster-xyz.us-east-1.neptune.amazonaws.com";
    let base = ["neptune", "seed", "--live", "--confirm-wipe", "--endpoint"];
    for (case, tail, code, expected) in [
        (
            "no policy",
            vec![SEED_HOST],
            2,
            "--network-policy is required",
        ),
        (
            "unreadable policy",
            vec![SEED_HOST, "--network-policy", "/does/not/exist"],
            1,
            "could not be read",
        ),
        (
            "endpoint is not the policy host",
            vec![other_host, "--network-policy", valid.as_str()],
            2,
            "--endpoint must be the network policy's host",
        ),
        (
            "not the Gremlin port",
            vec![SEED_HOST, "--network-policy", other_port.as_str()],
            1,
            "Gremlin port 8182",
        ),
        (
            "region mismatch",
            vec![
                SEED_HOST,
                "--network-policy",
                valid.as_str(),
                "--region",
                "us-west-2",
            ],
            1,
            "differs from the network policy's region",
        ),
    ] {
        let args: Vec<&str> = base.iter().copied().chain(tail).collect();
        let (status, stdout, stderr) = run(&args);
        assert_eq!(status, code, "{case}: {stderr}");
        assert!(stdout.is_empty(), "{case}: {stdout}");
        assert!(stderr.contains(expected), "{case}: {stderr}");
        assert!(!stderr.contains("/does/not/exist"), "{case}: {stderr}");
    }
}

// ---- T-DG-09a: no unignored live entry; ordinary runs never reach a network ----

/// Identifiers only a live test would use: the production database transport
/// and resolver. A test naming one, or the exact acceptance gate variable,
/// must be `#[ignore]` and gated on the `docgraph-live` feature.
const LIVE_IDENTS: [&str; 6] = [
    "for_database",
    "for_docgraph_service",
    "SystemDatabaseResolver",
    "TcpDialer",
    "connect_live",
    "run_live_review",
];

fn live_tokens(stream: proc_macro2::TokenStream, gate: &str) -> bool {
    stream.into_iter().any(|tree| match tree {
        proc_macro2::TokenTree::Group(group) => live_tokens(group.stream(), gate),
        proc_macro2::TokenTree::Ident(ident) => LIVE_IDENTS.iter().any(|live| ident == live),
        proc_macro2::TokenTree::Literal(literal) => literal.to_string() == format!("\"{gate}\""),
        proc_macro2::TokenTree::Punct(_) => false,
    })
}

/// Whether a test body names a live identifier or the exact gate variable,
/// including inside macro invocations.
struct BodyScan<'g> {
    gate: &'g str,
    live: bool,
}

impl<'ast> syn::visit::Visit<'ast> for BodyScan<'_> {
    fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
        self.live |= LIVE_IDENTS.iter().any(|live| ident == live);
    }

    fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
        self.live |= literal.value() == self.gate;
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.live |= live_tokens(mac.tokens.clone(), self.gate);
        syn::visit::visit_macro(self, mac);
    }
}

struct Inventory {
    gate: String,
    file: String,
    tests: usize,
    live: usize,
    offenders: Vec<String>,
}

impl<'ast> syn::visit::Visit<'ast> for Inventory {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        let is_test = item.attrs.iter().any(|attr| {
            attr.path()
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "test")
        });
        if is_test {
            self.tests += 1;
            let mut scan = BodyScan {
                gate: &self.gate,
                live: false,
            };
            syn::visit::Visit::visit_block(&mut scan, &item.block);
            if scan.live {
                self.live += 1;
                let ignored = item.attrs.iter().any(|attr| attr.path().is_ident("ignore"));
                let gated = item.attrs.iter().any(|attr| {
                    matches!(&attr.meta, syn::Meta::List(list)
                        if list.path.is_ident("cfg")
                            && list.tokens.to_string().contains("\"docgraph-live\""))
                });
                if !(ignored && gated) {
                    self.offenders
                        .push(format!("{}::{}", self.file, item.sig.ident));
                }
            }
        }
        syn::visit::visit_item_fn(self, item);
    }
}

fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn test_inventories_hold_no_unignored_live_entry() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for dir in [
        "toolkit-cli/src",
        "toolkit-cli/tests",
        "toolkit-aws/src",
        "toolkit-aws/tests",
    ] {
        rust_files(&crates.join(dir), &mut files);
    }
    let mut inventory = Inventory {
        gate: concat!("TOOLKIT_DOCGRAPH_", "LIVE_TESTS").to_string(),
        file: String::new(),
        tests: 0,
        live: 0,
        offenders: Vec::new(),
    };
    let mut saw_database_tests = false;
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap();
        let tree = syn::parse_file(&source).unwrap_or_else(|error| panic!("{path:?}: {error}"));
        inventory.file = path.display().to_string();
        saw_database_tests |= path.ends_with("sdk/database/tests.rs");
        syn::visit::Visit::visit_file(&mut inventory, &tree);
    }
    assert!(
        saw_database_tests,
        "the scan covers the database connector tests"
    );
    assert!(
        inventory.tests > 500,
        "the scan found {} tests",
        inventory.tests
    );
    assert!(
        inventory.offenders.is_empty(),
        "live tests must be #[ignore] and cfg(feature = \"docgraph-live\"): {:?}",
        inventory.offenders
    );
}

#[test]
fn the_inventory_rule_flags_an_unignored_live_test() {
    let gate = concat!("TOOLKIT_DOCGRAPH_", "LIVE_TESTS");
    let source = format!(
        "#[test] fn unguarded() {{ let _ = std::env::var(\"{gate}\"); }}\n\
         #[test] fn transport() {{ SdkTransport::for_database(); }}\n\
         #[test] fn review() {{ run_live_review(); }}\n\
         #[test] #[ignore] #[cfg(feature = \"docgraph-live\")] fn gated() {{ SystemDatabaseResolver; }}\n\
         #[test] fn ordinary() {{ SdkTransport::for_database_with_resolver(); }}"
    );
    let mut inventory = Inventory {
        gate: gate.to_string(),
        file: "fixture".to_string(),
        tests: 0,
        live: 0,
        offenders: Vec::new(),
    };
    syn::visit::Visit::visit_file(&mut inventory, &syn::parse_file(&source).unwrap());
    assert_eq!(inventory.tests, 5);
    assert_eq!(inventory.live, 4);
    assert_eq!(
        inventory.offenders,
        vec![
            "fixture::unguarded".to_string(),
            "fixture::transport".to_string(),
            "fixture::review".to_string()
        ]
    );
}

/// A loopback listener that only counts connection attempts: every proxy,
/// endpoint and metadata variable below points at it.
struct ForbiddenNetwork {
    port: u16,
    accepted: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ForbiddenNetwork {
    fn start() -> Self {
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (count, halt) = (accepted.clone(), stop.clone());
        std::thread::spawn(move || {
            while !halt.load(Ordering::SeqCst) {
                if listener.accept().is_ok() {
                    count.fetch_add(1, Ordering::SeqCst);
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        });
        Self {
            port,
            accepted,
            stop,
        }
    }

    fn accepted(&self) -> usize {
        std::thread::sleep(std::time::Duration::from_millis(50));
        self.accepted.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for ForbiddenNetwork {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A hostile shared AWS configuration: a `credential_process` that would leave
/// a marker, an endpoint at the forbidden network, and live/consent-shaped
/// keys. Nothing may read it into an action.
fn hostile_config(dir: &std::path::Path, fake: &str) -> std::path::PathBuf {
    let path = dir.join("hostile-aws-config");
    let marker = dir.join("provider-ran");
    std::fs::write(
        &path,
        format!(
            "[default]\ncredential_process = sh -c 'touch {}'\nendpoint_url = https://{fake}\n\
             live = true\nconfirm_wipe = true\nconfirm_write = true\nconfirm_delete = true\n\
             confirm_create = true\nregion = eu-west-1\n",
            marker.display()
        ),
    )
    .unwrap();
    path
}

/// No credential provider ran in `dir` (the hostile `credential_process`).
fn provider_never_ran(dir: &std::path::Path) -> bool {
    !dir.join("provider-ran").exists()
}

/// Run the toolkit with canary credentials, the acceptance flag and every
/// proxy, endpoint and metadata variable aimed at the forbidden network.
fn run_hostile(args: &[&str], dir: &std::path::Path, fake: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .current_dir(dir)
        .env_clear()
        .env(concat!("TOOLKIT_DOCGRAPH_", "LIVE_TESTS"), "1")
        .env("AWS_ACCESS_KEY_ID", "AKIAFORBIDDENCANARY0")
        .env("AWS_SECRET_ACCESS_KEY", "forbidden-secret-canary")
        .env("AWS_REGION", "us-east-1")
        .env("AWS_CONFIG_FILE", hostile_config(dir, fake))
        .env("AWS_SHARED_CREDENTIALS_FILE", hostile_config(dir, fake))
        .env("AWS_PROFILE", "default")
        .env("AWS_REGION", "eu-west-1")
        .env("AWS_DEFAULT_REGION", "eu-west-1")
        .env("RUST_LOG", "trace")
        .env("HTTPS_PROXY", format!("http://{fake}"))
        .env("HTTP_PROXY", format!("http://{fake}"))
        .env("ALL_PROXY", format!("http://{fake}"))
        .env("AWS_ENDPOINT_URL", format!("https://{fake}"))
        .env(
            "AWS_EC2_METADATA_SERVICE_ENDPOINT",
            format!("http://{fake}"),
        )
        .env(
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
            format!("http://{fake}/creds"),
        )
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn ordinary_and_live_shaped_runs_never_reach_the_network() {
    let network = ForbiddenNetwork::start();
    let fake = format!("127.0.0.1:{}", network.port);
    let dir = std::env::temp_dir().join(format!("docgraph-09a-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let policy = dir.join("policy.json");
    std::fs::write(
        &policy,
        r#"{"host":"db.cluster-abc.us-east-1.neptune.amazonaws.com","port":8182,"region":"us-east-1","private_ranges":["10.1.0.0/16"],"live":true}"#,
    )
    .unwrap();
    let input = dir.join("input.json");
    std::fs::write(&input, "{}").unwrap();
    let (policy, input) = (
        policy.to_str().unwrap().to_string(),
        input.to_str().unwrap().to_string(),
    );
    let (policy, input) = (policy.as_str(), input.as_str());
    let invocations: Vec<Vec<&str>> = vec![
        vec![
            "documentdb",
            "wa-review",
            "--cluster-id",
            "c1",
            "--region",
            "us-east-1",
        ],
        vec![
            "documentdb",
            "wa-review",
            "--cluster-id",
            "c1",
            // An explicit argv region is required even beside hostile AWS
            // configuration. A complete live invocation now reaches AWS.
            "--live",
        ],
        vec!["neptune", "seed", "--dry-run", "--confirm-wipe"],
        vec![
            "neptune",
            "seed",
            "--live",
            "--confirm-wipe",
            "--endpoint",
            "db.cluster-abc.us-east-1.neptune.amazonaws.com",
            "--network-policy",
            policy,
        ],
        vec![
            "neptune",
            "memory",
            "--operation",
            "retrieve",
            "--input",
            input,
        ],
        vec![
            "neptune",
            "memory",
            "--live",
            "--operation",
            "consolidate",
            "--confirm-write",
            "--confirm-delete",
            "--input",
            input,
            "--network-policy",
            policy,
        ],
    ];
    for args in &invocations {
        let out = run_hostile(args, &dir, &fake);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        if args.contains(&"--live") {
            assert_ne!(
                out.status.code(),
                Some(0),
                "{args:?} must not claim live success"
            );
        }
        for canary in ["forbidden-secret-canary", "AKIAFORBIDDENCANARY0"] {
            assert!(
                !stderr.contains(canary) && !stdout.contains(canary),
                "{args:?}"
            );
        }
        assert_eq!(network.accepted(), 0, "{args:?} reached the network");
    }
    assert!(provider_never_ran(&dir), "a credential provider ran");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[cfg(feature = "docgraph-live")]
fn live_review_argv_preflight_precedes_environment_files_and_network() {
    let network = ForbiddenNetwork::start();
    let fake = format!("127.0.0.1:{}", network.port);
    let dir = workdir("review-preflight");
    for (extra, expected) in [
        (vec![], "--cluster-id, --region"),
        (
            vec!["--cluster-id", "c1"],
            "arguments are required: --region",
        ),
        (
            vec!["--region", "us-east-1"],
            "arguments are required: --cluster-id",
        ),
        (
            vec!["--cluster-id", "bad/id", "--region", "us-east-1"],
            "--cluster-id must",
        ),
        (
            vec!["--cluster-id", "c1", "--region", "invalid"],
            "--region must",
        ),
        (
            vec!["--cluster-id", "c1", "--region", "cn-north-1"],
            "standard AWS partition",
        ),
        (
            vec![
                "--cluster-id",
                "c1",
                "--region",
                "us-east-1",
                "--now",
                "invalid",
            ],
            "--now must",
        ),
    ] {
        let mut args = vec![
            "documentdb",
            "wa-review",
            "--live",
            "--analysis-data",
            "/does/not/exist",
            "--network-policy",
            "/also/does/not/exist",
            "--output",
            "reports",
        ];
        args.extend(extra);
        let out = run_hostile(&args, &dir, &fake);
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
        assert!(!stderr.contains("/does/not/exist"), "{stderr}");
        assert!(!dir.join("reports").exists());
    }
    assert_eq!(network.accepted(), 0);
    assert!(provider_never_ran(&dir));
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---- T-DG-01a golden replay; T-DG-01e consent carriers; T-DG-02b/c CLI ----

/// A fresh working directory for one test.
fn workdir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("docgraph-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run with an empty environment (the golden baseline).
fn run_clean(args: &[&str], dir: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .current_dir(dir)
        .env_clear()
        .args(args)
        .output()
        .unwrap()
}

fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The seed replay recording, rebuilt from the dry-run statement list: every
/// statement answers `[]` and the four verification queries the values the
/// recorded seed contract expects.
fn seed_recording(dry_run: &str) -> String {
    let answers = [
        ("g.V().count()", "[12]"),
        ("g.E().count()", "[14]"),
        (
            "g.V().has('Account','id','A2').out('USES').in('USES').dedup().values('id').toList()",
            r#"["A1", "A2", "A3"]"#,
        ),
        (
            "g.V().has('Person','id','U1').out('FOLLOWS').out('FOLLOWS').dedup().values('name').toList()",
            r#"["Dave"]"#,
        ),
    ];
    let mut calls = Vec::new();
    for line in dry_run.lines().skip(1) {
        let (_, statement) = line.split_once(". ").expect("numbered statement");
        let statement = match statement.strip_prefix('[') {
            Some(tagged) => tagged.split_once("] ").expect("tag").1,
            None => statement,
        };
        let request = match statement.split_once(" {") {
            Some((gremlin, bindings)) => format!(
                r#"{{"gremlin":{},"bindings":{{{bindings}}}"#,
                json_string(gremlin)
            ),
            None => format!(r#"{{"gremlin":{}}}"#, json_string(statement)),
        };
        let results = answers
            .iter()
            .find(|(query, _)| *query == statement)
            .map_or("[]", |(_, results)| results);
        calls.push(format!(
            r#"{{"request": {request}, "response": {{"results": {results}}}}}"#
        ));
    }
    format!(r#"{{"calls": [{}]}}"#, calls.join(","))
}

/// T-DG-01a: the golden seed dry run and replay, memory dry run and review
/// fallback produce the fixed contract output under this build's features
/// (the suite runs this file with and without `docgraph-live`), and a hostile
/// AWS configuration, proxy/endpoint environment and canary credentials
/// change nothing: identical output and status, zero sockets, no provider.
#[test]
fn golden_replays_are_unchanged_by_hostile_configuration() {
    let network = ForbiddenNetwork::start();
    let fake = format!("127.0.0.1:{}", network.port);
    let dir = workdir("01a");
    let dry = run_hostile(&["neptune", "seed", "--dry-run"], &dir, &fake);
    let dry_text = String::from_utf8(dry.stdout.clone()).unwrap();
    assert_eq!(dry.status.code(), Some(0));
    let lines: Vec<&str> = dry_text.lines().collect();
    assert_eq!(lines[0], "Dry run: nothing is sent to Neptune.");
    assert_eq!(
        lines[1],
        "1. [destructive, needs --confirm-wipe] g.V().drop()"
    );
    assert_eq!(lines.len(), 2 + 29 + 4);

    let recording = dir.join("seed.json");
    std::fs::write(&recording, seed_recording(&dry_text)).unwrap();
    let input = dir.join("input.json");
    std::fs::write(&input, r#"{"graph_id":"g-abc123def4"}"#).unwrap();
    let (recording, input) = (
        recording.to_str().unwrap().to_string(),
        input.to_str().unwrap().to_string(),
    );
    let out = dir.join("out").to_str().unwrap().to_string();
    let invocations: Vec<Vec<&str>> = vec![
        vec!["neptune", "seed", "--dry-run"],
        vec![
            "neptune",
            "seed",
            "--confirm-wipe",
            "--recorded",
            &recording,
            "--endpoint",
            "n.example",
        ],
        vec![
            "neptune",
            "memory",
            "--operation",
            "consolidate",
            "--input",
            &input,
            "--dry-run",
        ],
        vec![
            "documentdb",
            "wa-review",
            "--cluster-id",
            "prod-docdb",
            "--region",
            "us-east-1",
            "--output",
            &out,
            "--now",
            "2026-01-31T12:00:00Z",
        ],
    ];
    for args in &invocations {
        let hostile = run_hostile(args, &dir, &fake);
        let clean = run_clean(args, &dir);
        assert_eq!(hostile.status.code(), clean.status.code(), "{args:?}");
        assert_eq!(hostile.stdout, clean.stdout, "{args:?}");
        assert_eq!(hostile.stderr, clean.stderr, "{args:?}");
    }
    let replay = run_hostile(&invocations[1], &dir, &fake);
    let stdout = String::from_utf8(replay.stdout).unwrap();
    assert_eq!(
        replay.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert!(stdout.starts_with(
        "Connecting to Neptune at n.example:8182...\nClearing ALL graph data at n.example...\nSeeding social graph...\n"
    ));
    assert!(stdout.ends_with("Endpoint: wss://n.example:8182/gremlin\n"));
    let memory = run_hostile(&invocations[2], &dir, &fake);
    assert_eq!(memory.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&memory.stdout).contains("7776000000"));
    assert_eq!(network.accepted(), 0, "a golden replay reached the network");
    assert!(provider_never_ran(&dir), "a credential provider ran");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Live- and consent-shaped keys every carrier file below holds.
#[cfg(feature = "docgraph-live")]
const CONSENT_CARRIER: &str = r#"{"live":true,"confirm_wipe":true,"confirm_write":true,"confirm_delete":true,"confirm_create":true,"--live":true,"--confirm-wipe":true,"calls":[]}"#;

/// T-DG-01e: recorded responses, analysis/capture envelopes, memory inputs,
/// network-policy files, the shared AWS configuration and the environment
/// cannot supply `--live` or any `--confirm-*`. Missing argv consent is
/// refused before any file is read; argv consent plus carriers still cannot
/// enable live work in this build; non-live replay ignores them as consent.
#[test]
#[cfg(feature = "docgraph-live")]
fn consent_carriers_never_authorize_live_work_or_missing_consent() {
    let network = ForbiddenNetwork::start();
    let fake = format!("127.0.0.1:{}", network.port);
    let dir = workdir("01e");
    let carrier = dir.join("carrier.json");
    std::fs::write(&carrier, CONSENT_CARRIER).unwrap();
    let carrier = carrier.to_str().unwrap().to_string();
    let c = carrier.as_str();
    for (args, status, required) in &carrier_rows(c) {
        let out = run_hostile(args, &dir, &fake);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(*status), "{args:?}: {stderr}");
        assert!(stderr.contains(required), "{args:?}: {stderr}");
        assert!(
            !stderr.contains(c),
            "{args:?} read or echoed a carrier file"
        );
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    // Non-live replay: a recording's consent keys are not --confirm-wipe.
    let out = run_hostile(&["neptune", "seed", "--recorded", c], &dir, &fake);
    assert_eq!(out.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("Clearing ALL graph data"));
    assert_eq!(network.accepted(), 0, "a carrier reached the network");
    assert!(provider_never_ran(&dir), "a credential provider ran");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// T-DG-02b/c: no credential arguments exist (a supplied one is a usage error
/// that echoes nothing), and `RUST_LOG` enables no logging of canaries.
#[test]
fn credential_arguments_are_rejected_without_echo() {
    let network = ForbiddenNetwork::start();
    let fake = format!("127.0.0.1:{}", network.port);
    let dir = workdir("02b");
    let canary = "argumentsecretcanary0";
    for args in [
        vec![
            "neptune",
            "seed",
            "--live",
            "--confirm-wipe",
            "--aws-secret-access-key",
            canary,
        ],
        vec![
            "neptune",
            "memory",
            "--live",
            "--operation",
            "retrieve",
            "--access-key-id",
            canary,
        ],
        vec!["neptune", "seed", "--dry-run", "--secret-key", canary],
        vec![
            "documentdb",
            "wa-review",
            "--cluster-id",
            "c1",
            "--region",
            "us-east-1",
            "--profile",
            canary,
        ],
        vec![
            "documentdb",
            "wa-review",
            "--cluster-id",
            "c1",
            "--region",
            "us-east-1",
            "--session-token",
            canary,
        ],
    ] {
        let out = run_hostile(&args, &dir, &fake);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        for text in [&out.stdout, &out.stderr] {
            let text = String::from_utf8_lossy(text);
            assert!(!text.contains(canary), "{args:?} echoed: {text}");
            assert!(!text.contains("forbidden-secret-canary"), "{args:?}");
        }
    }
    assert_eq!(network.accepted(), 0);
    assert!(provider_never_ran(&dir));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The T-DG-01e table: (args, exit status, required stderr text). `@` in an
/// argument list stands for the carrier file.
#[cfg(feature = "docgraph-live")]
fn carrier_rows(c: &str) -> Vec<(Vec<&str>, i32, &'static str)> {
    let endpoint = "db.cluster-abc.us-east-1.neptune.amazonaws.com";
    let table: [(&'static str, i32, &'static str); 8] = [
        (
            "neptune seed --live --endpoint ENDPOINT --network-policy @",
            1,
            "--confirm-wipe",
        ),
        (
            "neptune memory --live --operation consolidate --input @ --network-policy @",
            1,
            "--confirm-delete",
        ),
        (
            "neptune memory --live --operation query --input @",
            1,
            "--confirm-write",
        ),
        (
            "neptune memory --live --operation create-memory --input @",
            1,
            "--confirm-create",
        ),
        (
            // Data cannot supply the required argv region (AWS_REGION is
            // also present). Success routing is tested with injected seams.
            "documentdb wa-review --live --cluster-id c1 --analysis-data @",
            2,
            "arguments are required: --region",
        ),
        // A collection capture manifest has no live mode at all.
        (
            "documentdb normalize-capture --live --capture @ --output @",
            2,
            "",
        ),
        (
            // S3 (#5515): with argv consent the live seed reads the policy, which
            // refuses every consent-shaped key before credentials or DNS.
            "neptune seed --live --confirm-wipe --endpoint ENDPOINT --network-policy @",
            1,
            "unknown key",
        ),
        (
            "neptune memory --live --operation consolidate --confirm-write --confirm-delete \
             --input @ --network-policy @",
            1,
            // S4 reads input only after argv consent. A capture/manifest is
            // not operation input and cannot confer graph/consent authority.
            "missing graph_id",
        ),
    ];
    table
        .iter()
        .map(|(args, status, required)| {
            let args = args
                .split_whitespace()
                .map(|word| match word {
                    "@" => c,
                    "ENDPOINT" => endpoint,
                    word => word,
                })
                .collect();
            (args, *status, *required)
        })
        .collect()
}

#[test]
#[cfg(feature = "docgraph-live")]
fn live_memory_refuses_policy_region_and_input_failures_before_clients() {
    let network = ForbiddenNetwork::start();
    let fake = format!("127.0.0.1:{}", network.port);
    let dir = workdir("s4-preflight");
    let input = policy_file(
        "s4-query.json",
        r#"{"graph_id":"g-abc123def4","query":"RETURN 1"}"#,
    );
    let p = policy_file(
        "s4-valid.json",
        r#"{"host":"g-abc123def4.us-east-1.neptune-graph.amazonaws.com","port":443,"region":"us-east-1"}"#,
    );
    let consent = policy_file(
        "s4-consent.json",
        r#"{"host":"g-abc123def4.us-east-1.neptune-graph.amazonaws.com","port":443,"region":"us-east-1","confirm_write":true}"#,
    );
    let wrong = policy_file(
        "s4-wrong.json",
        r#"{"host":"g-other00000.us-east-1.neptune-graph.amazonaws.com","port":443,"region":"us-east-1"}"#,
    );
    let base = [
        "neptune",
        "memory",
        "--live",
        "--operation",
        "query",
        "--confirm-write",
        "--confirm-delete",
        "--input",
        &input,
    ];
    for (tail, code, expected) in [
        (vec![], 2, "--network-policy is required"),
        (vec!["--network-policy", &consent], 1, "unknown key"),
        (
            vec!["--network-policy", &wrong],
            1,
            "unsupported or invalid live memory request",
        ),
        (
            vec!["--network-policy", &p, "--region", "us-west-2"],
            1,
            "differs from the network policy",
        ),
    ] {
        let args: Vec<_> = base.iter().copied().chain(tail).collect();
        let output = run_hostile(&args, &dir, &fake);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(code), "{stderr}");
        assert!(output.stdout.is_empty());
        assert!(stderr.contains(expected), "{stderr}");
        assert!(!stderr.contains(&input));
        assert!(!stderr.contains("required memory credentials"));
    }
    let unsafe_create = policy_file("s4-invalid-create.json", r#"{"memory_gb":15}"#);
    let (code, stdout, stderr) = run(&[
        "neptune",
        "memory",
        "--live",
        "--operation",
        "create-memory",
        "--confirm-create",
        "--input",
        &unsafe_create,
    ]);
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert!(stderr.contains("unsupported or invalid live memory request"));
    assert_eq!(network.accepted(), 0);
    assert!(provider_never_ran(&dir));
    std::fs::remove_dir_all(&dir).unwrap();
}
