//! The event client, the skill-invoked hook and the status command.

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use toolkit_core::Json;

use super::fx::{Fx, INSTALL_ID, VERSION};
use crate::telemetry::client::PostRequest;
use crate::telemetry::client::{self, PostStatus, DEFAULT_ENDPOINT, ENDPOINT_ENV};
use crate::telemetry::skill;
use crate::telemetry::{EventSink, NoNetworkSink};

fn consent_event() -> Json {
    Json::object(vec![("consentRecorded", Json::Object(Vec::new()))])
}

fn post(fx: &Fx, event: &Json, install_id: Option<&str>) -> bool {
    fx.with(|ctx| client::post_event(ctx, event, install_id, "https://collector.invalid/v1/e"))
}

// --------------------------------------------------------------- endpoint

#[test]
fn the_endpoint_defaults_to_prod_honors_the_override_and_ignores_a_blank_one() {
    let fx = Fx::new("endpoint");
    fx.with(|ctx| assert_eq!(client::endpoint(ctx), DEFAULT_ENDPOINT));
    assert!(DEFAULT_ENDPOINT.contains("prod.startup-advisor-extension"));
    fx.set_env(ENDPOINT_ENV, "https://beta.invalid/v1/x");
    fx.with(|ctx| assert_eq!(client::endpoint(ctx), "https://beta.invalid/v1/x"));
    fx.set_env(ENDPOINT_ENV, "   ");
    fx.with(|ctx| assert_eq!(client::endpoint(ctx), DEFAULT_ENDPOINT));
}

// --------------------------------------------------------------- the gate

#[test]
fn nothing_is_sent_without_an_accepted_record() {
    let cases: [(&str, Option<&str>); 5] = [
        ("none", None),
        ("opted-out", Some("{\"installId\": \"11111111-2222-3333-4444-555555555555\", \"consentStatus\": \"OPT_OUT\"}")),
        ("hand-edited", Some("{\"consentStatus\": \"OPT_OUT\"}")),
        ("corrupt", Some("{\"installId\":")),
        ("not-a-uuid", Some("{\"installId\": \"abc\", \"consentStatus\": \"ACCEPTED\"}")),
    ];
    for (label, record) in cases {
        let fx = Fx::new(label);
        if let Some(text) = record {
            fx.write_record_text(text);
        }
        assert!(!post(&fx, &consent_event(), Some(INSTALL_ID)), "{label}");
        assert!(
            !fx.with(|ctx| client::send_event(ctx, &consent_event())),
            "{label}"
        );
        assert_eq!(fx.sink.count(), 0, "{label}");
    }
}

#[test]
fn a_record_that_turns_into_an_opt_out_stops_the_next_request() {
    // The gate is re-checked per request, not remembered by the caller.
    let fx = Fx::new("recheck");
    fx.accept();
    assert!(post(&fx, &consent_event(), Some(INSTALL_ID)));
    fx.write_record("OPT_OUT", INSTALL_ID);
    assert!(!post(&fx, &consent_event(), Some(INSTALL_ID)));
    assert_eq!(fx.sink.count(), 1);
}

#[test]
fn a_missing_install_id_or_plugin_version_sends_nothing() {
    let fx = Fx::new("missing");
    fx.accept();
    assert!(!post(&fx, &consent_event(), None));
    assert!(!post(&fx, &consent_event(), Some("")));
    fs::remove_file(fx.bundle.join("version.json")).expect("remove");
    assert!(
        !post(&fx, &consent_event(), Some(INSTALL_ID)),
        "no made-up version"
    );
    fs::write(fx.bundle.join("version.json"), "{\"version\": \"  \"}").expect("blank");
    assert!(!post(&fx, &consent_event(), Some(INSTALL_ID)));
    fs::write(fx.bundle.join("version.json"), "{\"version\": 7}").expect("number");
    assert!(!post(&fx, &consent_event(), Some(INSTALL_ID)));
    assert_eq!(fx.sink.count(), 0);
}

#[test]
fn a_non_2xx_answer_is_false_and_never_an_error() {
    let fx = Fx::new("non2xx");
    fx.accept();
    for status in [
        PostStatus::Status(403),
        PostStatus::Status(400),
        PostStatus::Status(500),
        PostStatus::Unreachable,
        PostStatus::Unknown,
    ] {
        fx.sink.respond_with(status);
        assert!(!post(&fx, &consent_event(), Some(INSTALL_ID)), "{status:?}");
    }
    fx.sink.respond_with(PostStatus::Status(204));
    assert!(post(&fx, &consent_event(), Some(INSTALL_ID)));
}

#[test]
fn the_shipped_sink_reaches_nobody() {
    let sink = NoNetworkSink;
    let status = sink.post(&PostRequest {
        url: "https://collector.invalid/",
        body: "{}",
        content_type: "application/json",
        user_agent: "x",
        timeout: Duration::from_secs(1),
    });
    assert_eq!(status, PostStatus::Unreachable);
}

// ---------------------------------------------------------------- payload

#[test]
fn the_payload_carries_every_required_field_and_the_fixed_headers() {
    let fx = Fx::new("payload");
    fx.accept();
    fx.set_clock(1_759_276_800.123_9);
    assert!(post(&fx, &consent_event(), Some(INSTALL_ID)));
    let body = &fx.sink.bodies_from(0)[0];
    let keys: Vec<&str> = body
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec![
            "installId",
            "occurredAt",
            "pluginTelemetryEvent",
            "pluginVersion",
            "source"
        ]
    );
    assert_eq!(body["installId"], INSTALL_ID);
    assert_eq!(body["source"], "CLAUDE_CODE");
    assert_eq!(body["pluginVersion"], VERSION);
    assert_eq!(
        body["occurredAt"], 1_759_276_800_123_i64,
        "epoch milliseconds, not seconds"
    );
    assert_eq!(body["pluginTelemetryEvent"], json!({"consentRecorded": {}}));
    assert_eq!(
        fx.sink.headers(),
        vec![(
            "application/json".to_string(),
            "aws-startup-advisor-plugin".to_string()
        )]
    );
    assert_eq!(
        fx.sink.urls(),
        vec!["https://collector.invalid/v1/e".to_string()]
    );
    assert_eq!(fx.sink.timeouts(), vec![Duration::from_secs(3)]);
}

#[test]
fn occurred_at_follows_the_injected_clock_and_is_never_in_the_future() {
    let fx = Fx::new("clock");
    fx.accept();
    let before = crate::telemetry::system_now();
    assert!(post(&fx, &consent_event(), Some(INSTALL_ID)));
    let sent = fx.sink.bodies_from(0)[0]["occurredAt"]
        .as_f64()
        .expect("number");
    assert!((sent - before * 1000.0).abs() < 60_000.0);
    assert!(sent <= crate::telemetry::system_now() * 1000.0 + 1000.0);
}

#[test]
fn the_wire_body_is_json_in_python_dumps_format() {
    let fx = Fx::new("wire");
    fx.accept();
    let event = Json::object(vec![("note", Json::string("caf\u{e9}"))]);
    let body = fx
        .with(|ctx| client::prepare(ctx, &event, Some(INSTALL_ID)))
        .expect("body");
    assert!(
        body.starts_with("{\"installId\": \"11111111-2222-3333-4444-555555555555\", \"source\": ")
    );
    assert!(body.contains("caf\\u00e9"), "ensure_ascii, as json.dumps");
    assert!(body.contains("\"pluginTelemetryEvent\": {\"note\":"));
}

#[test]
fn the_version_comes_from_the_bundle_then_plugin_manifests_of_a_recognised_layout() {
    let fx = Fx::new("version");
    fx.with(|ctx| assert_eq!(client::plugin_version(ctx).as_deref(), Some(VERSION)));

    // A canonical scripts/telemetry directory falls back to plugin manifests.
    let plugin = fx.dir.path().join("plugin");
    let canonical = plugin.join("scripts/telemetry");
    fs::create_dir_all(&canonical).expect("dir");
    fs::create_dir_all(plugin.join(".claude-plugin")).expect("dir");
    fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        "{\"version\": \" 4.5.6 \"}",
    )
    .expect("manifest");
    let mut other = Fx::new("version-canonical");
    other.bundle = canonical.clone();
    other.with(|ctx| assert_eq!(client::plugin_version(ctx).as_deref(), Some("4.5.6")));
    // The bundle's own version.json wins.
    fs::write(canonical.join("version.json"), "{\"version\": \"7.0.0\"}").expect("version");
    other.with(|ctx| assert_eq!(client::plugin_version(ctx).as_deref(), Some("7.0.0")));

    // A vendored bundle falls back to its enclosing plugin.
    let vendored = plugin.join("skills/gcp-to-aws/references/vendored/telemetry");
    fs::create_dir_all(&vendored).expect("dir");
    other.bundle = vendored;
    other.with(|ctx| assert_eq!(client::plugin_version(ctx).as_deref(), Some("4.5.6")));

    // A bare references/ directory is never a plugin.
    let stray = fx.dir.path().join("stray/references/vendored/telemetry");
    fs::create_dir_all(&stray).expect("dir");
    fs::write(
        fx.dir.path().join("stray/plugin.json"),
        "{\"version\": \"9.9.9\"}",
    )
    .expect("stray");
    other.bundle = stray;
    other.with(|ctx| assert_eq!(client::plugin_version(ctx), None));
}

// ----------------------------------------------------------------- source

#[test]
fn host_markers_are_detected_in_precedence_order() {
    let fx = Fx::new("source");
    let source = |fx: &Fx| fx.with(client::detect_source);
    fx.clear_env();
    assert_eq!(source(&fx), "OTHER", "an unknown host is not guessed");
    fx.set_env("AWS_STARTUP_ADVISOR_SOURCE", "CURSOR");
    assert_eq!(source(&fx), "OTHER", "no variable can set the source");
    fx.set_env("CLAUDECODE", "1");
    assert_eq!(source(&fx), "CLAUDE_CODE");
    fx.set_env("CURSOR_AGENT", "1");
    assert_eq!(
        source(&fx),
        "CURSOR",
        "Cursor markers precede the Claude compatibility ones"
    );
    fx.set_env("CODEX_SANDBOX", "seatbelt");
    assert_eq!(
        source(&fx),
        "CODEX",
        "explicit Codex precedes the inheritable CURSOR_AGENT"
    );
    fx.set_env("KIRO_IDE", "1");
    assert_eq!(source(&fx), "CODEX");
    fx.set_env("CODEX_SANDBOX", "  ");
    assert_eq!(source(&fx), "KIRO", "a blank marker does not count");
    fx.set_env("CURSOR_VERSION", "1");
    assert_eq!(source(&fx), "CURSOR");
}

// ----------------------------------------------------------- reporting mode

fn plugin_with_hooks(fx: &Fx, host_dir: &str, hooks: &Value, script: bool) -> std::path::PathBuf {
    let plugin = fx.dir.path().join(format!("hooked-{host_dir}"));
    fs::create_dir_all(plugin.join(host_dir)).expect("dir");
    fs::create_dir_all(plugin.join("scripts/telemetry/metric_emission")).expect("dir");
    fs::write(
        plugin.join(host_dir).join("plugin.json"),
        json!({"name": "aws-startup-advisor", "hooks": "hooks/hooks.json", "version": "1.2.3"})
            .to_string(),
    )
    .expect("manifest");
    fs::create_dir_all(plugin.join(host_dir).join("hooks")).expect("dir");
    // `hooks` is relative to the plugin root.
    fs::create_dir_all(plugin.join("hooks")).expect("dir");
    fs::write(
        plugin.join("hooks/hooks.json"),
        json!({ "hooks": hooks }).to_string(),
    )
    .expect("hooks");
    if script {
        fs::write(
            plugin.join("scripts/telemetry/metric_emission/migration.py"),
            "",
        )
        .expect("script");
    }
    plugin
}

#[test]
fn reporting_mode_is_hook_only_for_a_reachable_registered_migration_hook() {
    let python = "sh -c 'python3 \"$0\"' \"${CLAUDE_PLUGIN_ROOT}/scripts/telemetry/metric_emission/migration.py\" --reconcile";
    let claude = json!({"Stop": [{"hooks": [{"type": "command", "command": python}]}]});
    let fx = Fx::new("mode-claude");
    let plugin = plugin_with_hooks(&fx, ".claude-plugin", &claude, true);
    let mut host = Fx::new("mode-claude-host");
    host.bundle = plugin.join("scripts/telemetry");
    host.with(|ctx| assert_eq!(client::migration_reporting_mode(ctx), "hook"));

    // The Python marker needs the script to exist; a missing one means cli.
    let no_script = plugin_with_hooks(&fx, ".claude-plugin", &claude, false);
    fs::remove_file(no_script.join("scripts/telemetry/metric_emission/migration.py")).ok();
    host.bundle = no_script.join("scripts/telemetry");
    host.with(|ctx| assert_eq!(client::migration_reporting_mode(ctx), "cli"));

    // This binary's own action counts, with no script on disk.
    let rust = json!({"PostToolUse": [{"matcher": "Write", "hooks": [{"command": "toolkit telemetry migration"}]}]});
    let rust_fx = Fx::new("mode-rust");
    let rust_plugin = plugin_with_hooks(&rust_fx, ".claude-plugin", &rust, false);
    fs::remove_file(rust_plugin.join("scripts/telemetry/metric_emission/migration.py")).ok();
    host.bundle = rust_plugin.join("scripts/telemetry");
    host.with(|ctx| assert_eq!(client::migration_reporting_mode(ctx), "hook"));

    // Another host's source is always cli.
    host.set_env("CODEX_SANDBOX", "1");
    host.with(|ctx| assert_eq!(client::migration_reporting_mode(ctx), "cli"));
}

#[test]
fn reporting_mode_is_cli_for_a_malformed_registration_or_a_foreign_plugin() {
    let host = |bundle: &Path| {
        let mut host = Fx::new("mode-bad");
        host.bundle = bundle.join("scripts/telemetry");
        host
    };
    let command = "migration.py scripts/telemetry/metric_emission/migration.py";
    let fx = Fx::new("mode-bad-plugins");
    for hooks in [
        json!({"Stop": "nope"}),
        json!({"Stop": [{"hooks": "nope"}]}),
        json!({"Stop": [{"hooks": [7]}]}),
        json!({"Stop": [{"hooks": [{"command": 7}]}]}),
        json!({"Stop": [{"hooks": [{"command": "other"}]}]}),
        json!({}),
    ] {
        let plugin = plugin_with_hooks(&fx, ".claude-plugin", &hooks, true);
        host(&plugin)
            .with(|ctx| assert_eq!(client::migration_reporting_mode(ctx), "cli", "{hooks}"));
    }
    // A manifest naming another plugin, or a hooks path escaping the root.
    let good = json!({"Stop": [{"hooks": [{"command": command}]}]});
    let plugin = plugin_with_hooks(&fx, ".claude-plugin", &good, true);
    host(&plugin).with(|ctx| assert_eq!(client::migration_reporting_mode(ctx), "hook"));
    fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        json!({"name": "other", "hooks": "hooks/hooks.json"}).to_string(),
    )
    .expect("rename");
    host(&plugin).with(|ctx| assert_eq!(client::migration_reporting_mode(ctx), "cli"));
    fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        json!({"name": "aws-startup-advisor", "hooks": "../outside.json"}).to_string(),
    )
    .expect("escape");
    fs::write(
        plugin.parent().expect("parent").join("outside.json"),
        json!({"hooks": good}).to_string(),
    )
    .expect("outside");
    host(&plugin).with(|ctx| assert_eq!(client::migration_reporting_mode(ctx), "cli"));
}

#[test]
fn cursor_hooks_are_recognised_through_the_cursor_manifest() {
    let cursor = json!({"afterFileEdit": [{"command": "x scripts/telemetry/metric_emission/migration.py --reconcile"}]});
    let fx = Fx::new("mode-cursor");
    let plugin = plugin_with_hooks(&fx, ".cursor-plugin", &cursor, true);
    let mut host = Fx::new("mode-cursor-host");
    host.bundle = plugin.join("scripts/telemetry");
    host.clear_env();
    host.set_env("CURSOR_VERSION", "1");
    host.with(|ctx| assert_eq!(client::migration_reporting_mode(ctx), "hook"));
    host.clear_env();
    host.set_env("CLAUDECODE", "1");
    host.with(|ctx| assert_eq!(client::migration_reporting_mode(ctx), "cli"));
}

// ---------------------------------------------------------------- status

#[test]
fn status_is_read_only_and_reports_the_shared_client_view() {
    let fx = Fx::new("status");
    let (code, out, err) = fx.run(&["status"]);
    assert_eq!((code, err.as_str()), (0, ""));
    let report: Value = serde_json::from_str(&out).expect("json");
    assert_eq!(
        report,
        json!({"source": "CLAUDE_CODE", "reportingMode": "cli", "consentStatus": null, "pluginVersion": VERSION})
    );
    assert!(
        out.starts_with("{\"source\": \"CLAUDE_CODE\", \"reportingMode\": \"cli\""),
        "key order as the Python status"
    );
    fx.accept();
    let report: Value = serde_json::from_str(&fx.run(&["status"]).1).expect("json");
    assert_eq!(report["consentStatus"], "ACCEPTED");
    fx.write_record("OPT_OUT", INSTALL_ID);
    let report: Value = serde_json::from_str(&fx.run(&["status"]).1).expect("json");
    assert_eq!(report["consentStatus"], "OPT_OUT");
    assert_eq!(fx.sink.count(), 0, "status never sends");
    assert!(
        fx.record_text().expect("record").contains("OPT_OUT"),
        "and never writes"
    );
}

// -------------------------------------------------------- consent recorded

#[test]
fn accept_emits_consent_recorded_once_and_a_dead_sink_does_not_fail_it() {
    let fx = Fx::new("accept-emit");
    let (code, _, _) = fx.run(&["consent", "accept"]);
    assert_eq!(code, 0);
    assert_eq!(fx.sink.bodies_from(0).len(), 1);
    assert_eq!(
        fx.sink.bodies_from(0)[0]["pluginTelemetryEvent"],
        json!({"consentRecorded": {}})
    );
    assert_eq!(
        fx.sink.bodies_from(0)[0]["installId"]
            .as_str()
            .map(str::len),
        Some(36)
    );

    let dead = Fx::new("accept-dead");
    dead.sink.respond_with(PostStatus::Unreachable);
    let (code, out, _) = dead.run(&["consent", "accept"]);
    assert_eq!(code, 0);
    assert!(out.contains("collection is ON"));
    assert!(
        !out.contains("collector"),
        "the user is told nothing about the request"
    );
}

#[test]
fn accept_with_the_shipped_sink_in_the_real_binary_leaves_the_process_silent() {
    // The default build sends nothing; the acknowledgement still works.
    let fx = Fx::new("accept-shipped");
    let (code, out, err) = run_with_shipped_sink(&fx, &["consent", "accept"]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.contains("collection is ON"));
}

fn run_with_shipped_sink(fx: &Fx, words: &[&str]) -> (u8, String, String) {
    use crate::telemetry::{run_action, Ctx};
    let words: Vec<String> = words.iter().map(|word| (*word).to_string()).collect();
    let env = |name: &str| fx.env.lock().expect("lock").get(name).cloned();
    let now = crate::telemetry::system_now;
    let stdin = |_timeout: Duration| String::new();
    let ctx = Ctx {
        home: Some(fx.home.as_path()),
        env: &env,
        cwd: fx.dir.path(),
        telemetry_dir: Some(fx.bundle.as_path()),
        now: &now,
        sink: &NoNetworkSink,
        stdin: &stdin,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = {
        let mut output = crate::Output::with_diagnostics(&mut out, &mut err);
        match run_action(&words, &ctx, &mut output) {
            Ok(()) => output.exit_status(),
            Err(error) => error.exit_code(),
        }
    };
    (
        code,
        String::from_utf8(out).expect("utf-8"),
        String::from_utf8(err).expect("utf-8"),
    )
}

// ----------------------------------------------------- skill-invoked hook

fn skill_payload(skill: &str) -> String {
    json!({"tool_name": "Skill", "tool_input": {"skill": skill}}).to_string()
}

fn events(fx: &Fx) -> Vec<Value> {
    fx.sink
        .bodies_from(0)
        .into_iter()
        .map(|body| body["pluginTelemetryEvent"].clone())
        .collect()
}

#[test]
fn the_skill_name_maps_onto_a_plugin_skill_id_with_qualifiers_and_casing_tolerated() {
    assert_eq!(skill::skill_id(Some("gcp-to-aws")), Some("GCP_TO_AWS"));
    assert_eq!(
        skill::skill_id(Some("aws-startup-advisor:gcp-to-aws")),
        Some("GCP_TO_AWS")
    );
    assert_eq!(
        skill::skill_id(Some("  Some-Other:LLM-To-Bedrock  ")),
        Some("LLM_TO_BEDROCK")
    );
    for name in [
        None,
        Some(""),
        Some("shared"),
        Some("gcp-to-aws:"),
        Some("not-ours"),
        Some("GCP_TO_AWS"),
    ] {
        assert_eq!(skill::skill_id(name), None, "{name:?}");
    }
}

#[test]
fn every_mapped_skill_is_a_modelled_enum_member_and_every_member_is_reachable() {
    let mapped: std::collections::BTreeSet<&str> =
        skill::SKILL_IDS.iter().map(|(_, id)| *id).collect();
    let modelled: std::collections::BTreeSet<&str> =
        client::PLUGIN_SKILL_IDS.iter().copied().collect();
    assert_eq!(mapped, modelled);
    assert_eq!(mapped.len(), 12);
    for (directory, id) in skill::SKILL_IDS {
        assert_eq!(
            id.to_lowercase().replace('_', "-"),
            directory,
            "UPPER_SNAKE_CASE of the directory"
        );
    }
}

#[test]
fn the_skill_name_comes_from_the_flag_first_then_the_tool_payload() {
    let payload = serde_json_to_json(&json!({"tool_input": {"skill": "gcp-to-aws"}}));
    let none = Json::Object(Vec::new());
    assert_eq!(
        skill::skill_name(&[], &payload).as_deref(),
        Some("gcp-to-aws")
    );
    assert_eq!(
        skill::skill_name(&["--skill", "heroku-to-aws"], &payload).as_deref(),
        Some("heroku-to-aws")
    );
    assert_eq!(
        skill::skill_name(&["--skill=azure-to-aws"], &payload).as_deref(),
        Some("azure-to-aws")
    );
    assert_eq!(skill::skill_name(&["--skill"], &none), None);
    assert_eq!(skill::skill_name(&["--other"], &none), None);
    for odd in [
        json!({"tool_input": "x"}),
        json!({"tool_input": {"skill": 7}}),
        json!({"tool_input": {}}),
        json!({}),
    ] {
        assert_eq!(
            skill::skill_name(&[], &serde_json_to_json(&odd)),
            None,
            "{odd}"
        );
    }
}

fn serde_json_to_json(value: &Value) -> Json {
    toolkit_core::parse_json(&value.to_string()).expect("json")
}

#[test]
fn the_skill_hook_reports_the_skill_from_the_payload_or_the_flag_with_the_recorded_id() {
    let fx = Fx::new("skill-payload");
    fx.accept();
    fx.set_stdin(&skill_payload("aws-startup-advisor:gcp-to-aws"));
    let (code, out, err) = fx.run(&["skill-invoked"]);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""), "silent");
    assert_eq!(
        events(&fx),
        vec![json!({"skillInvoked": {"skill": "GCP_TO_AWS", "eventName": "SKILL_INVOKED"}})]
    );
    assert_eq!(fx.sink.bodies_from(0)[0]["installId"], INSTALL_ID);

    let flagged = Fx::new("skill-flag");
    flagged.accept();
    flagged.run(&["skill-invoked", "--skill", "heroku-to-aws"]);
    assert_eq!(
        events(&flagged)[0]["skillInvoked"]["skill"],
        "HEROKU_TO_AWS"
    );
    flagged.run(&["skill-invoked", "--skill=llm-to-bedrock"]);
    assert_eq!(
        events(&flagged)[1]["skillInvoked"]["skill"],
        "LLM_TO_BEDROCK"
    );
}

#[test]
fn the_skill_hook_sends_nothing_without_consent_a_skill_of_ours_or_a_payload() {
    for (label, record) in [
        ("none", None),
        ("opt-out", Some("{\"installId\": \"11111111-2222-3333-4444-555555555555\", \"consentStatus\": \"OPT_OUT\"}")),
        ("invalid", Some("{\"consentStatus\": \"ACCEPTED\"}")),
        ("corrupt", Some("{")),
    ] {
        let fx = Fx::new(label);
        if let Some(text) = record {
            fx.write_record_text(text);
        }
        fx.set_stdin(&skill_payload("gcp-to-aws"));
        let (code, _, _) = fx.run(&["skill-invoked"]);
        assert_eq!(code, 0, "{label}");
        assert_eq!(fx.sink.count(), 0, "{label}");
    }
    let fx = Fx::new("skill-not-ours");
    fx.accept();
    for stdin in [
        skill_payload("some-other-skill"),
        "not json".to_string(),
        "[]".to_string(),
        String::new(),
        "{}".to_string(),
    ] {
        fx.set_stdin(&stdin);
        assert_eq!(fx.run(&["skill-invoked"]).0, 0);
    }
    assert_eq!(fx.sink.count(), 0);
}

#[test]
fn the_skill_hook_exits_zero_when_the_service_is_unreachable() {
    let fx = Fx::new("skill-dead");
    fx.accept();
    fx.sink.respond_with(PostStatus::Unreachable);
    fx.set_stdin(&skill_payload("gcp-to-aws"));
    let (code, out, err) = fx.run(&["skill-invoked"]);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
}

#[test]
fn the_skill_hook_does_not_read_stdin_unless_consent_is_accepted() {
    let fx = Fx::new("skill-lazy");
    let read = std::sync::atomic::AtomicBool::new(false);
    let env = |_: &str| None::<String>;
    let now = crate::telemetry::system_now;
    let stdin = |_t: Duration| {
        read.store(true, std::sync::atomic::Ordering::SeqCst);
        String::new()
    };
    let ctx = crate::telemetry::Ctx {
        home: Some(fx.home.as_path()),
        env: &env,
        cwd: fx.dir.path(),
        telemetry_dir: Some(fx.bundle.as_path()),
        now: &now,
        sink: &fx.sink,
        stdin: &stdin,
    };
    skill::main(&[], &ctx);
    assert!(!read.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn an_unmodelled_skill_id_is_dropped_before_the_wire() {
    let fx = Fx::new("unmodelled");
    fx.accept();
    for id in ["gcp-to-aws", "NOT_A_SKILL", ""] {
        assert!(!fx.with(|ctx| client::emit_skill_invocation_metric(
            ctx,
            INSTALL_ID,
            id,
            "https://collector.invalid/"
        )));
    }
    assert_eq!(fx.sink.count(), 0);
    assert!(fx.with(|ctx| client::emit_skill_invocation_metric(
        ctx,
        INSTALL_ID,
        "GCP_TO_AWS",
        "https://collector.invalid/"
    )));
}
