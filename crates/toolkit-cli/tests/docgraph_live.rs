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
        assert!(stderr.contains("--live requires the docgraph-live feature"));
    }
}

// ---- T-DG-09a: no unignored live entry; ordinary runs never reach a network ----

/// Identifiers only a live test would use: the production database transport
/// and resolver. A test naming one, or the exact acceptance gate variable,
/// must be `#[ignore]` and gated on the `docgraph-live` feature.
const LIVE_IDENTS: [&str; 3] = ["for_database", "SystemDatabaseResolver", "TcpDialer"];

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
                    self.offenders.push(format!("{}::{}", self.file, item.sig.ident));
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
    for dir in ["toolkit-cli/src", "toolkit-cli/tests", "toolkit-aws/src", "toolkit-aws/tests"] {
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
    assert!(saw_database_tests, "the scan covers the database connector tests");
    assert!(inventory.tests > 500, "the scan found {} tests", inventory.tests);
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
    assert_eq!(inventory.tests, 4);
    assert_eq!(inventory.live, 3);
    assert_eq!(
        inventory.offenders,
        vec!["fixture::unguarded".to_string(), "fixture::transport".to_string()]
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
    let (policy, input) = (policy.to_str().unwrap(), input.to_str().unwrap());
    let invocations: Vec<Vec<&str>> = vec![
        vec!["documentdb", "wa-review", "--cluster-id", "c1", "--region", "us-east-1"],
        vec!["documentdb", "wa-review", "--cluster-id", "c1", "--region", "us-east-1", "--live"],
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
        vec!["neptune", "memory", "--operation", "retrieve", "--input", input],
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
        let out = Command::new(env!("CARGO_BIN_EXE_toolkit"))
            .env_clear()
            .env(concat!("TOOLKIT_DOCGRAPH_", "LIVE_TESTS"), "1")
            .env("AWS_ACCESS_KEY_ID", "AKIAFORBIDDENCANARY0")
            .env("AWS_SECRET_ACCESS_KEY", "forbidden-secret-canary")
            .env("AWS_REGION", "us-east-1")
            .env("HTTPS_PROXY", format!("http://{fake}"))
            .env("HTTP_PROXY", format!("http://{fake}"))
            .env("ALL_PROXY", format!("http://{fake}"))
            .env("AWS_ENDPOINT_URL", format!("https://{fake}"))
            .env("AWS_EC2_METADATA_SERVICE_ENDPOINT", format!("http://{fake}"))
            .env("AWS_CONTAINER_CREDENTIALS_FULL_URI", format!("http://{fake}/creds"))
            .args(args)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        if args.contains(&"--live") {
            assert_ne!(out.status.code(), Some(0), "{args:?} must not claim live success");
        }
        for canary in ["forbidden-secret-canary", "AKIAFORBIDDENCANARY0"] {
            assert!(!stderr.contains(canary) && !stdout.contains(canary), "{args:?}");
        }
        assert_eq!(network.accepted(), 0, "{args:?} reached the network");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
