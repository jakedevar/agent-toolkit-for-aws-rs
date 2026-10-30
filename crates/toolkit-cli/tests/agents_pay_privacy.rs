//! #5102: compile the actual Approved implementation in a sibling-module
//! fixture. The data/formatting stubs avoid linking the payment runtime;
//! rustc checks the production field and constructor visibility unchanged.
#![cfg(unix)]
#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_OUTPUT: AtomicUsize = AtomicUsize::new(0);

fn compile_sibling(body: &str) -> std::process::Output {
    let approved = include_str!("../src/agents_pay/policy/approved.rs");
    let source = format!(
        r#"
        #![allow(dead_code)]
        extern crate self as toolkit_core;
        pub struct Json;
        mod agents_pay {{
            mod amount {{
                pub fn usd_text(amount: &str) -> String {{ amount.to_owned() }}
            }}
            pub mod policy {{
                pub mod approved {{ {approved} }}
                // A sibling of approved.rs inside policy also cannot mutate fields.
                mod sibling {{ {body} }}
            }}
            // A spending sibling outside policy cannot invoke the constructor.
            mod flow {{ {body} }}
        }}
        "#
    );
    // Keep compiler artifacts next to this test binary in the daemon's stamped
    // target. rustc needs a writable output directory even for metadata only.
    let output_path = std::env::current_exe().unwrap().with_file_name(format!(
        "agents_pay_privacy_{}_{}.rmeta",
        std::process::id(),
        NEXT_OUTPUT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut child = Command::new("rustc")
        .args([
            "--edition=2021",
            "--crate-type=lib",
            "--crate-name=agents_pay_sibling_privacy",
            "--emit=metadata",
            "--error-format=json",
            "-o",
        ])
        .arg(&output_path)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("offline rustc fixture");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(source.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    if output_path.exists() {
        std::fs::remove_file(output_path).unwrap();
    }
    output
}

fn errors(output: &std::process::Output) -> Vec<serde_json::Value> {
    String::from_utf8(output.stderr.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("rustc JSON diagnostic"))
        .filter(|diagnostic| diagnostic["level"] == "error" && !diagnostic["code"].is_null())
        .collect()
}

#[test]
fn t_order_ctor_actual_source_allows_read_only_siblings() {
    let output = compile_sibling(
        "fn read(approved: crate::agents_pay::policy::approved::Approved) {\n\
         let _ = (approved.entry(), approved.amount(), approved.token());\n\
         }",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn t_order_ctor_actual_source_rejects_sibling_mutation_and_construction() {
    for (body, code, count) in [
        (
            "fn change(mut approved: crate::agents_pay::policy::approved::Approved) { approved.amount = String::new(); approved.token = String::new(); approved.entry = crate::Json; }",
            "E0616",
            6,
        ),
        (
            "fn forge() { let _ = crate::agents_pay::policy::approved::Approved { amount: String::new(), ..panic!() }; }",
            "E0451",
            2,
        ),
        (
            "fn construct() { let _ = crate::agents_pay::policy::approved::Approved::new(crate::Json, String::new(), 2, None, String::new(), panic!()); }",
            "E0624",
            1,
        ),
    ] {
        let output = compile_sibling(body);
        assert!(!output.status.success(), "sibling bypass compiled: {body}");
        let diagnostics = errors(&output);
        assert_eq!(diagnostics.len(), count, "{diagnostics:?}");
        assert!(
            diagnostics.iter().all(|error| error["code"]["code"] == code),
            "expected only {code}: {diagnostics:?}"
        );
    }
}
