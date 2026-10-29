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
