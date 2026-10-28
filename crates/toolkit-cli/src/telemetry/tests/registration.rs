//! The command is registered once, as a passthrough, and documented.

use std::ffi::OsString;

use crate::{route, Routed, COMMANDS, USAGE};

fn words(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn one_passthrough_command_is_registered() {
    let specs: Vec<_> = COMMANDS
        .iter()
        .filter(|spec| spec.path.first() == Some(&"telemetry"))
        .collect();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].path, &["telemetry"]);
    assert!(
        specs[0].passthrough,
        "hook words are never a usage failure of the router"
    );
}

#[test]
fn every_word_after_the_command_reaches_the_handler_verbatim() {
    let Ok(Routed::Run(spec, invocation)) = route(
        COMMANDS,
        &words(&[
            "telemetry",
            "migration",
            "--reconcile",
            "--via",
            "cli",
            "-h",
            "--",
            "-",
        ]),
    ) else {
        panic!("telemetry routes");
    };
    assert_eq!(spec.path, &["telemetry"]);
    assert_eq!(
        invocation.positionals,
        words(&["migration", "--reconcile", "--via", "cli", "-h", "--", "-"])
    );
}

#[test]
fn the_usage_text_documents_every_subcommand() {
    let block = USAGE.split("toolkit telemetry").nth(1).expect("usage line");
    for word in [
        "consent show|status|accept|opt-out|session-start",
        "status",
        "reconcile",
        "skill-invoked",
        "migration",
        "bundles",
        "--telemetry-dir DIR",
    ] {
        assert!(block.contains(word), "{word}");
    }
    assert!(USAGE.contains("docs/migration/telemetry.md"));
}

fn exec(values: &[&str]) -> (u8, String, String) {
    let args: Vec<OsString> = values.iter().map(OsString::from).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = crate::run(&args, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("utf-8"),
        String::from_utf8(err).expect("utf-8"),
    )
}

#[test]
fn the_real_entry_point_prints_the_notice_and_rejects_unknown_words_without_echoing_them() {
    let (status, out, err) = exec(&["telemetry", "consent", "show"]);
    assert_eq!((status, err.as_str()), (0, ""));
    assert_eq!(out, crate::telemetry::consent::disclaimer());

    let hostile = "\u{1b}[31mFORGED\u{7}\r\u{202e}gpj.exe";
    for values in [
        vec!["telemetry", hostile],
        vec!["telemetry", "consent", hostile],
        vec!["telemetry", "bundles", hostile],
    ] {
        let (status, out, err) = exec(&values);
        assert_eq!(status, 2, "{values:?}");
        assert_eq!(out, "");
        assert!(
            !err.contains("FORGED") && !err.contains('\u{1b}') && !err.contains('\u{202e}'),
            "{err:?}"
        );
    }
    let (status, _, err) = exec(&["telemetry"]);
    assert_eq!(status, 2);
    assert!(err.starts_with("error: invalid arguments"));
}
