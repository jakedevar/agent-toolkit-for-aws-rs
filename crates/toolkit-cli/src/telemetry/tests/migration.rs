//! Migration reporting: lifecycle, hold and retry, sessions, locking, the CLI
//! route and the registered hooks. Each test builds an ephemeral project with
//! a `.migration/` run and an accepted consent record in an isolated home, runs
//! the reporter as a host would (hook JSON on stdin) and asserts on the
//! requests the recording sink received and the snapshot left on disk.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};

use super::fx::{Fx, INSTALL_ID, OTHER_SESSION, RUN_ID, SESSION_ID};
use crate::telemetry::client::PostStatus;

pub(super) const PHASE_NAMES: [&str; 7] = [
    "discover", "clarify", "design", "estimate", "workshop", "generate", "feedback",
];

pub(super) fn gcp_inventory() -> Value {
    json!({"resources": [
        {"type": "google_sql_database_instance", "name": "db"},
        {"type": "google_cloud_run_service", "name": "api"},
    ]})
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn phase_status(overrides: Value) -> Value {
    let mut status = json!({
        "migration_id": "0226-1430",
        "run_id": RUN_ID,
        "owning_skill": "GCP_TO_AWS",
        "last_updated": "2026-02-26T15:35:22Z",
        "current_phase": "clarify",
        "phases": {
            "discover": "completed", "clarify": "in_progress", "design": "pending",
            "estimate": "pending", "workshop": "pending", "generate": "pending",
            "feedback": "pending",
        },
    });
    for (key, value) in overrides.as_object().expect("overrides").clone() {
        if value.is_null() && key != "owning_skill" {
            status.as_object_mut().expect("object").remove(&key);
        } else {
            status[key] = value;
        }
    }
    status
}

pub(super) fn with_phases(changes: &[(&str, &str)]) -> Value {
    let mut phases = phase_status(json!({}))["phases"].clone();
    for (name, state) in changes {
        phases[*name] = json!(state);
    }
    phases
}

pub(super) fn all_completed() -> Value {
    let mut phases = Map::new();
    for name in PHASE_NAMES {
        phases.insert(name.to_string(), json!("completed"));
    }
    Value::Object(phases)
}

/// One project with a single run directory.
pub(super) struct Project<'a> {
    pub fx: &'a Fx,
    pub root: PathBuf,
    pub run_dir: PathBuf,
}

impl<'a> Project<'a> {
    pub(super) fn new(fx: &'a Fx, status: &Value, artifacts: &[(&str, Value)]) -> Self {
        Self::named(fx, status, artifacts, "0226-1430")
    }

    pub(super) fn named(
        fx: &'a Fx,
        status: &Value,
        artifacts: &[(&str, Value)],
        run: &str,
    ) -> Self {
        let root = fx.dir.path().join(format!("project-{run}"));
        let run_dir = root.join(".migration").join(run);
        fs::create_dir_all(&run_dir).expect("run dir");
        for (name, value) in artifacts {
            fs::write(run_dir.join(name), value.to_string()).expect("artifact");
        }
        let project = Self { fx, root, run_dir };
        project.write_status(status);
        project
    }

    /// A project with the default GCP inventory.
    pub(super) fn gcp(fx: &'a Fx, status: &Value) -> Self {
        Self::new(
            fx,
            status,
            &[("gcp-resource-inventory.json", gcp_inventory())],
        )
    }

    pub(super) fn write_status(&self, status: &Value) {
        fs::write(
            self.run_dir.join(".phase-status.json"),
            serde_json::to_string_pretty(status).expect("json"),
        )
        .expect("status");
    }

    pub(super) fn write_gates(&self, gates: &Value) {
        fs::write(self.run_dir.join(".gate-failures.json"), gates.to_string()).expect("gates");
    }

    pub(super) fn snapshot_path(&self) -> PathBuf {
        self.run_dir.join(".telemetry-snapshot.json")
    }

    pub(super) fn snapshot(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(self.snapshot_path()).expect("snapshot"))
            .expect("json")
    }

    pub(super) fn has_snapshot(&self) -> bool {
        self.snapshot_path().exists()
    }

    pub(super) fn has_lock(&self) -> bool {
        self.run_dir.join(".telemetry-lock").exists()
    }

    /// Runs the reporter as a host would and returns the events this run produced.
    pub(super) fn hook(&self, args: &[&str], session: &str, payload: &Value) -> Vec<Value> {
        let before = self.fx.sink.count();
        let mut body = json!({"session_id": session, "cwd": self.root.display().to_string(), "reason": "other"});
        for (key, value) in payload.as_object().cloned().unwrap_or_default() {
            body[key] = value;
        }
        self.fx.set_stdin(&body.to_string());
        let mut words = vec!["migration"];
        words.extend_from_slice(args);
        let (code, out, err) = self.fx.run(&words);
        assert_eq!(
            code, 0,
            "the reporter is fail-open and always exits 0: {err}"
        );
        assert_eq!((out.as_str(), err.as_str()), ("", ""), "and silent");
        self.fx.sink.bodies_from(before)
    }

    pub(super) fn reconcile(&self, session: &str) -> Vec<Value> {
        self.hook(&["--reconcile"], session, &json!({}))
    }

    pub(super) fn session_end(&self, session: &str) -> Vec<Value> {
        self.hook(&["--session-end"], session, &json!({}))
    }

    pub(super) fn post_write(&self, file: &Path, session: &str) -> Vec<Value> {
        self.hook(
            &[],
            session,
            &json!({"tool_input": {"file_path": file.display().to_string()}}),
        )
    }

    /// The `reconcile` route a skill instruction uses (`toolkit telemetry reconcile`).
    pub(super) fn cli_reconcile(&self) -> Vec<Value> {
        let before = self.fx.sink.count();
        // The CLI uses the working directory.
        let (code, _, err) = self.fx.run_in(&self.root, &["reconcile"]);
        assert_eq!(code, 0, "{err}");
        self.fx.sink.bodies_from(before)
    }
}

pub(super) fn activity(body: &Value) -> &Value {
    &body["pluginTelemetryEvent"]["migrationActivity"]
}

pub(super) fn summary(bodies: &[Value]) -> Vec<(String, Option<String>, Option<String>)> {
    let mut rows: Vec<_> = bodies
        .iter()
        .map(|body| {
            let a = activity(body);
            (
                a["eventName"].as_str().expect("name").to_string(),
                a["phase"].as_str().map(str::to_string),
                a["status"].as_str().map(str::to_string),
            )
        })
        .collect();
    rows.sort();
    rows
}

pub(super) fn row(
    name: &str,
    phase: Option<&str>,
    status: Option<&str>,
) -> (String, Option<String>, Option<String>) {
    (
        name.to_string(),
        phase.map(str::to_string),
        status.map(str::to_string),
    )
}

fn completions(bodies: &[Value]) -> Vec<Value> {
    bodies
        .iter()
        .filter(|body| activity(body)["eventName"] == "RUN_COMPLETED")
        .map(|body| activity(body).clone())
        .collect()
}

fn fixture() -> Fx {
    let fx = Fx::new("migration");
    fx.accept();
    fx.write_record("ACCEPTED", RUN_ID);
    fx
}

// ---------------------------------------------------------------- lifecycle

#[test]
fn a_new_run_is_reported_once_under_the_state_file_run_id() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    let first = p.reconcile(SESSION_ID);
    let second = p.reconcile(SESSION_ID);
    assert_eq!(
        summary(&first),
        vec![
            row("PHASE_COMPLETED", Some("DISCOVER"), Some("SUCCESS")),
            row("RUN_STARTED", None, None)
        ]
    );
    for body in &first {
        assert_eq!(activity(body)["runId"], RUN_ID);
        assert_eq!(activity(body)["skill"], "GCP_TO_AWS");
        assert_eq!(activity(body)["sessionId"], SESSION_ID);
        assert_eq!(body["source"], "CLAUDE_CODE");
        assert_eq!(body["installId"], RUN_ID);
        assert!(body["occurredAt"].is_i64());
        assert_eq!(activity(body)["attributes"]["sourceProvider"], "GCP");
    }
    assert!(second.is_empty(), "an unchanged run is not re-reported");
    assert_eq!(p.snapshot()["runId"], RUN_ID);
    assert!(!p.has_lock(), "the lock is released");
}

#[test]
fn an_upper_case_run_id_is_sent_in_lower_case() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({"run_id": RUN_ID.to_uppercase()})));
    let bodies = p.reconcile(SESSION_ID);
    assert_eq!(bodies.len(), 2);
    assert!(bodies.iter().all(|body| activity(body)["runId"] == RUN_ID));
    assert_eq!(p.snapshot()["runId"], RUN_ID);
}

#[test]
fn each_later_transition_is_reported_once_and_the_terminal_event_carries_its_run_mode() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.reconcile(SESSION_ID);
    p.write_status(&phase_status(
        json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
    ));
    let later = p.reconcile(SESSION_ID);
    p.write_status(&phase_status(
        json!({"current_phase": "complete", "run_mode": "decide", "phases": all_completed()}),
    ));
    let done = p.reconcile(SESSION_ID);
    let again = p.reconcile(SESSION_ID);
    assert_eq!(
        summary(&later),
        vec![row("PHASE_COMPLETED", Some("CLARIFY"), Some("SUCCESS"))]
    );
    let finished = completions(&done);
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0]["attributes"]["runMode"], "DECIDE");
    assert!(again.is_empty());
    assert_eq!(p.snapshot()["completed"], true);
}

#[test]
fn nothing_is_sent_and_no_trace_is_left_without_accepted_consent() {
    for (index, consent) in [None, Some("OPT_OUT"), Some("REJECTED")]
        .into_iter()
        .enumerate()
    {
        let fx = Fx::new("no-consent");
        if let Some(status) = consent {
            fx.write_record(status, RUN_ID);
        }
        let p = Project::named(
            &fx,
            &phase_status(json!({})),
            &[("gcp-resource-inventory.json", gcp_inventory())],
            &format!("0226-14{index:02}"),
        );
        assert!(p.reconcile(SESSION_ID).is_empty());
        assert!(p.session_end(SESSION_ID).is_empty());
        assert!(!p.has_snapshot(), "{consent:?}");
        assert!(!p.has_lock());
    }
}

#[test]
fn an_unreadable_or_malformed_record_is_no_consent_for_migration_too() {
    for text in ["{", "[]", "{\"consentStatus\": \"ACCEPTED\"}", ""] {
        let fx = Fx::new("bad-record");
        fx.write_record_text(text);
        let p = Project::gcp(&fx, &phase_status(json!({})));
        assert!(p.reconcile(SESSION_ID).is_empty(), "{text}");
        assert!(!p.has_snapshot());
    }
}

#[test]
fn an_accepted_record_without_a_valid_install_id_sends_nothing() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    fx.write_record("ACCEPTED", "not-a-uuid");
    assert!(p.reconcile(SESSION_ID).is_empty());
    assert!(!p.has_snapshot());
}

#[test]
fn a_run_that_names_no_owning_skill_or_a_non_migration_one_emits_nothing() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({"owning_skill": null})));
    assert!(p.reconcile(SESSION_ID).is_empty());
    assert!(!p.has_snapshot());
    let q = Project::named(
        &fx,
        &phase_status(json!({"owning_skill": "AGENT_ADVISOR"})),
        &[],
        "0226-1500",
    );
    assert!(q.reconcile(SESSION_ID).is_empty());
    let r = Project::named(
        &fx,
        &phase_status(json!({"owning_skill": ["GCP_TO_AWS"]})),
        &[],
        "0226-1501",
    );
    assert!(r.reconcile(SESSION_ID).is_empty());
    let s = Project::named(
        &fx,
        &phase_status(json!({"migration_id": ""})),
        &[],
        "0226-1502",
    );
    assert!(s.reconcile(SESSION_ID).is_empty());
}

#[test]
fn one_run_identity_is_kept_when_the_state_file_is_rebuilt_with_a_fresh_run_id() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.reconcile(SESSION_ID);
    p.write_status(&phase_status(json!({
        "run_id": "0f0f0f0f-1111-4222-8333-444444444444",
        "current_phase": "design",
        "phases": with_phases(&[("clarify", "completed")]),
    })));
    let later = p.reconcile(SESSION_ID);
    assert_eq!(later.len(), 1);
    assert_eq!(activity(&later[0])["eventName"], "PHASE_COMPLETED");
    assert_eq!(activity(&later[0])["runId"], RUN_ID);
    assert_eq!(p.snapshot()["runId"], RUN_ID);
}

#[test]
fn an_enum_attribute_whose_value_is_an_inherited_key_is_omitted() {
    let fx = fixture();
    let p = Project::gcp(
        &fx,
        &phase_status(
            json!({"current_phase": "complete", "run_mode": "__proto__", "phases": all_completed()}),
        ),
    );
    let done = completions(&p.reconcile(SESSION_ID));
    assert_eq!(done.len(), 1);
    assert!(done[0]["attributes"].get("runMode").is_none());
}

// ------------------------------------------------------------ hold and retry

#[test]
fn the_snapshot_is_held_while_the_service_refuses_with_403_then_reports_in_full() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    fx.sink.respond_with(PostStatus::Status(403));
    assert_eq!(p.reconcile(SESSION_ID).len(), 2);
    assert!(!p.has_snapshot(), "nothing taken, nothing recorded");
    fx.sink.respond_with(PostStatus::Status(200));
    assert_eq!(
        summary(&p.reconcile(SESSION_ID)),
        vec![
            row("PHASE_COMPLETED", Some("DISCOVER"), Some("SUCCESS")),
            row("RUN_STARTED", None, None)
        ]
    );
    assert!(p.reconcile(SESSION_ID).is_empty());
}

#[test]
fn only_the_throttled_events_of_a_batch_are_re_sent() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.reconcile(SESSION_ID);
    p.write_status(&phase_status(json!({"current_phase": "estimate", "phases": with_phases(&[("clarify", "completed"), ("design", "completed")])})));
    fx.sink.respond_with(PostStatus::Status(429));
    assert_eq!(p.reconcile(SESSION_ID).len(), 2);
    assert_eq!(
        p.snapshot()["phases"]["clarify"],
        "in_progress",
        "a held phase keeps its previous state"
    );
    fx.sink.respond_with(PostStatus::Status(200));
    assert_eq!(
        summary(&p.reconcile(SESSION_ID)),
        vec![
            row("PHASE_COMPLETED", Some("CLARIFY"), Some("SUCCESS")),
            row("PHASE_COMPLETED", Some("DESIGN"), Some("SUCCESS"))
        ]
    );
    assert!(p.reconcile(SESSION_ID).is_empty());
}

#[test]
fn a_partly_held_batch_records_exactly_what_the_service_took() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.reconcile(SESSION_ID);
    p.write_status(&phase_status(json!({"current_phase": "estimate", "phases": with_phases(&[("clarify", "completed"), ("design", "completed")])})));
    fx.sink.respond_by(|body| {
        if body["pluginTelemetryEvent"]["migrationActivity"]["phase"] == "DESIGN" {
            PostStatus::Status(503)
        } else {
            PostStatus::Status(200)
        }
    });
    assert_eq!(p.reconcile(SESSION_ID).len(), 2);
    let phases = p.snapshot()["phases"].clone();
    assert_eq!(phases["clarify"], "completed", "taken");
    assert_eq!(phases["design"], "pending", "held");
    fx.sink.respond_with(PostStatus::Status(200));
    assert_eq!(
        summary(&p.reconcile(SESSION_ID)),
        vec![row("PHASE_COMPLETED", Some("DESIGN"), Some("SUCCESS"))]
    );
}

#[test]
fn a_400_and_an_unknown_outcome_are_not_retried() {
    for status in [
        PostStatus::Status(400),
        PostStatus::Unknown,
        PostStatus::Status(404),
        PostStatus::Status(200),
    ] {
        let fx = fixture();
        let p = Project::gcp(&fx, &phase_status(json!({})));
        fx.sink.respond_with(status);
        assert_eq!(p.reconcile(SESSION_ID).len(), 2);
        fx.sink.respond_with(PostStatus::Status(200));
        assert!(
            p.reconcile(SESSION_ID).is_empty(),
            "{status:?}: a refusal the event would earn again is not repeated"
        );
    }
}

#[test]
fn a_service_that_cannot_be_reached_leaves_the_run_unrecorded_until_it_can() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    fx.sink.respond_with(PostStatus::Unreachable);
    assert_eq!(p.reconcile(SESSION_ID).len(), 2);
    assert!(
        !p.has_snapshot(),
        "nothing could have reached the service, so nothing is recorded"
    );
    fx.sink.respond_with(PostStatus::Status(200));
    assert_eq!(p.reconcile(SESSION_ID).len(), 2);
}

#[test]
fn the_shipped_no_network_sink_records_nothing_so_nothing_is_lost() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    let (code, _, _) = run_with_shipped_sink(&fx, &p, &["migration", "--reconcile"]);
    assert_eq!(code, 0);
    assert!(
        !p.has_snapshot(),
        "unreachable holds every event for a build that can send"
    );
}

fn run_with_shipped_sink(fx: &Fx, p: &Project<'_>, words: &[&str]) -> (u8, String, String) {
    use crate::telemetry::{run_action, Ctx, NoNetworkSink};
    let words: Vec<String> = words.iter().map(|word| (*word).to_string()).collect();
    let env = |name: &str| fx.env.lock().expect("lock").get(name).cloned();
    let now = crate::telemetry::system_now;
    let stdin = |_timeout: Duration| String::new();
    let ctx = Ctx {
        home: Some(fx.home.as_path()),
        env: &env,
        cwd: p.root.as_path(),
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

#[test]
fn the_requests_of_a_batch_go_out_under_one_budget_with_the_post_timeout() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.reconcile(SESSION_ID);
    for timeout in fx.sink.timeouts() {
        assert!(timeout <= Duration::from_secs(3) && timeout >= Duration::from_millis(50));
    }
    let teardown = Project::named(
        &fx,
        &phase_status(
            json!({"current_phase": "complete", "run_mode": "decide", "phases": all_completed()}),
        ),
        &[],
        "0226-1600",
    );
    let before = fx.sink.count();
    assert!(!teardown.session_end(SESSION_ID).is_empty());
    let budget = Duration::from_secs_f64(1.5 - 0.4);
    assert!(
        fx.sink.timeouts()[before..]
            .iter()
            .all(|timeout| *timeout <= budget + Duration::from_millis(5)),
        "a SessionEnd sweep budgets itself to the default hook budget"
    );
}

// ----------------------------------------------------------------- sessions

#[test]
fn a_run_goes_to_the_session_that_last_changed_it_so_its_teardown_reports_it() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.reconcile(SESSION_ID);
    p.write_status(&phase_status(
        json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
    ));
    p.reconcile(OTHER_SESSION);
    p.write_status(&phase_status(json!({"current_phase": "estimate", "phases": with_phases(&[("clarify", "completed"), ("design", "completed")])})));
    let by_first = p.session_end(SESSION_ID);
    let by_other = p.session_end(OTHER_SESSION);
    assert!(
        by_first.is_empty(),
        "the first session no longer owns the run"
    );
    assert_eq!(
        summary(&by_other),
        vec![row("PHASE_COMPLETED", Some("DESIGN"), Some("SUCCESS"))]
    );
}

#[test]
fn an_edit_inside_the_run_moves_ownership_and_a_merely_scanned_run_keeps_its_owner() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    let sibling = p.root.join(".migration/0226-1500");
    fs::create_dir_all(&sibling).expect("sibling");
    fs::write(
        sibling.join(".phase-status.json"),
        phase_status(
            json!({"migration_id": "0226-1500", "run_id": "5a5a5a5a-1111-4222-8333-444444444444"}),
        )
        .to_string(),
    )
    .expect("sibling status");
    p.reconcile(SESSION_ID);
    let unchanged = p.post_write(&p.run_dir.join("preferences.json"), OTHER_SESSION);
    assert!(unchanged.is_empty());
    let sibling_snapshot: Value = serde_json::from_str(
        &fs::read_to_string(sibling.join(".telemetry-snapshot.json")).expect("snapshot"),
    )
    .expect("json");
    assert_eq!(
        sibling_snapshot["sessionId"], SESSION_ID,
        "a run merely scanned keeps its owner"
    );
    assert_eq!(p.snapshot()["sessionId"], OTHER_SESSION);
    p.write_status(&phase_status(
        json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
    ));
    assert!(p.session_end(SESSION_ID).is_empty());
    assert_eq!(
        summary(&p.session_end(OTHER_SESSION)),
        vec![row("PHASE_COMPLETED", Some("CLARIFY"), Some("SUCCESS"))]
    );
}

#[test]
fn a_pathless_reconcile_keeps_the_existing_owner() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.reconcile(SESSION_ID);
    assert!(p.reconcile(OTHER_SESSION).is_empty());
    assert_eq!(p.snapshot()["sessionId"], SESSION_ID);
}

#[test]
fn a_phase_that_completes_again_after_a_confirmed_re_entry_reset_is_reported() {
    let fx = fixture();
    let p = Project::gcp(
        &fx,
        &phase_status(
            json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
        ),
    );
    p.reconcile(SESSION_ID);
    p.write_status(&phase_status(
        json!({"current_phase": "clarify", "phases": with_phases(&[("clarify", "pending")])}),
    ));
    let reset = p.reconcile(SESSION_ID);
    p.write_status(&phase_status(
        json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
    ));
    let again = p.reconcile(SESSION_ID);
    assert!(reset.is_empty());
    assert_eq!(
        summary(&again),
        vec![row("PHASE_COMPLETED", Some("CLARIFY"), Some("SUCCESS"))]
    );
}

#[test]
fn teardown_never_repeats_what_it_attempted() {
    let fx = fixture();
    let p = Project::gcp(
        &fx,
        &phase_status(
            json!({"current_phase": "complete", "run_mode": "decide", "phases": all_completed()}),
        ),
    );
    assert!(!p.session_end(SESSION_ID).is_empty());
    assert!(p.session_end(SESSION_ID).is_empty());
}

#[test]
fn a_sweep_out_of_budget_leaves_the_run_untouched_for_the_next_trigger() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    // The clock advances 2 s each time it is read: the 1.1 s budget is gone by
    // the first run.
    let ticks = std::sync::atomic::AtomicU32::new(0);
    let env = |name: &str| fx.env.lock().expect("lock").get(name).cloned();
    let now = || {
        let n = ticks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        crate::telemetry::system_now() + f64::from(n) * 2.0
    };
    let stdin = |_t: Duration| {
        json!({"session_id": SESSION_ID, "cwd": p.root.display().to_string()}).to_string()
    };
    let ctx = crate::telemetry::Ctx {
        home: Some(fx.home.as_path()),
        env: &env,
        cwd: p.root.as_path(),
        telemetry_dir: Some(fx.bundle.as_path()),
        now: &now,
        sink: &fx.sink,
        stdin: &stdin,
    };
    crate::telemetry::migration::main(&["--session-end"], &ctx);
    assert_eq!(fx.sink.count(), 0);
    assert!(!p.has_snapshot());
    assert_eq!(
        p.reconcile(SESSION_ID).len(),
        2,
        "the next trigger reports it"
    );
}

// ------------------------------------------------------------------ snapshot

#[test]
fn an_artifact_of_an_unexpected_shape_costs_its_attributes_never_the_event() {
    // total_models_detected as a string used to raise inside the send pool: the
    // events went out, the snapshot was never written, and every later hook
    // repeated the whole batch.
    let fx = fixture();
    let p = Project::new(
        &fx,
        &phase_status(
            json!({"current_phase": "complete", "run_mode": "decide", "phases": all_completed()}),
        ),
        &[
            ("gcp-resource-inventory.json", gcp_inventory()),
            (
                "ai-workload-profile.json",
                json!({"summary": {"ai_source": "openai", "total_models_detected": "2"}}),
            ),
        ],
    );
    let first = p.reconcile(SESSION_ID);
    assert!(summary(&first).contains(&row("PHASE_COMPLETED", Some("DISCOVER"), Some("SUCCESS"))));
    assert!(p.has_snapshot());
    let discover = first
        .iter()
        .find(|b| activity(b)["phase"] == "DISCOVER")
        .expect("discover");
    assert!(
        activity(discover).get("attributes").is_none(),
        "its attributes are dropped"
    );
    assert!(p.reconcile(SESSION_ID).is_empty());
}

#[test]
fn a_corrupt_snapshot_is_rebuilt_without_sending_so_the_run_reports_again() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.reconcile(SESSION_ID);
    fs::write(
        p.snapshot_path(),
        format!("{{\"runId\": \"{}", &RUN_ID[..12]),
    )
    .expect("corrupt");
    assert!(
        p.reconcile(SESSION_ID).is_empty(),
        "what the corrupt snapshot had reported is unknowable"
    );
    assert_eq!(p.snapshot()["phases"], phase_status(json!({}))["phases"]);
    assert_eq!(p.snapshot()["runId"], RUN_ID);
    p.write_status(&phase_status(
        json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
    ));
    assert_eq!(
        summary(&p.reconcile(SESSION_ID)),
        vec![row("PHASE_COMPLETED", Some("CLARIFY"), Some("SUCCESS"))]
    );
    // A snapshot that is JSON but not an object is corrupt too.
    fs::write(p.snapshot_path(), "[1, 2]").expect("array");
    assert!(p.reconcile(SESSION_ID).is_empty());
}

#[test]
fn the_snapshot_has_the_documented_shape() {
    let fx = fixture();
    fx.set_clock(1_800_000_000.0);
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.reconcile(SESSION_ID);
    let raw = fs::read_to_string(p.snapshot_path()).expect("snapshot");
    assert!(
        raw.ends_with("}\n") && raw.contains("\n  \"runId\""),
        "indent=2 and a trailing newline"
    );
    let snapshot = p.snapshot();
    assert_eq!(snapshot["updatedAt"], "2027-01-15T08:00:00Z");
    assert_eq!(snapshot["via"], "hook");
    assert_eq!(snapshot["completed"], false);
    assert_eq!(snapshot["gateFailures"], json!([]));
    assert_eq!(snapshot["sessionId"], SESSION_ID);
    assert!(snapshot.get("started").is_none() && snapshot.get("completedRunModes").is_none());
}

#[test]
fn a_stale_lock_is_taken_over_and_a_fresh_one_blocks() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    fs::create_dir(p.run_dir.join(".telemetry-lock")).expect("lock");
    // Private whatever the umask: a lock other users can write is not honoured.
    set_mode(&p.run_dir.join(".telemetry-lock"), 0o700);
    assert!(
        p.reconcile(SESSION_ID).is_empty(),
        "the holder is about to report the same transitions"
    );
    assert!(
        !p.has_snapshot() && p.has_lock(),
        "a foreign lock is left alone"
    );
    fx.set_clock(crate::telemetry::system_now() + 3600.0);
    assert_eq!(
        p.reconcile(SESSION_ID).len(),
        2,
        "an abandoned lock is taken over"
    );
    assert!(!p.has_lock());
}

// ------------------------------------------------- snapshot and lock privacy

/// `chmod`: fixtures get explicit modes so no assertion depends on the umask.
fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::symlink_metadata(path)
        .expect("stat")
        .permissions()
        .mode()
        & 0o777
}

/// Records the lock directory's mode at every request, so the lock can be
/// observed while it is held (it is removed when the run is done).
#[cfg(unix)]
fn record_lock_modes(fx: &Fx, run_dir: &Path) -> std::sync::Arc<std::sync::Mutex<Vec<u32>>> {
    let modes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = std::sync::Arc::clone(&modes);
    let lock = run_dir.join(".telemetry-lock");
    fx.sink.respond_by(move |_| {
        seen.lock().expect("modes").push(mode_of(&lock));
        PostStatus::Status(200)
    });
    modes
}

/// The run directory holds nothing but what the test wrote and the snapshot:
/// no temporary file is left behind.
#[cfg(unix)]
fn no_temporaries(run_dir: &Path) {
    let leftovers: Vec<String> = fs::read_dir(run_dir)
        .expect("run dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|extension| extension == "tmp")
        })
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[cfg(unix)]
#[test]
fn a_new_snapshot_is_readable_and_writable_by_its_owner_only() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    assert_eq!(p.reconcile(SESSION_ID).len(), 2);
    assert_eq!(mode_of(&p.snapshot_path()), 0o600);
    no_temporaries(&p.run_dir);
    // A later write (a new transition) replaces it privately too.
    p.write_status(&phase_status(
        json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
    ));
    assert_eq!(p.reconcile(SESSION_ID).len(), 1);
    assert_eq!(mode_of(&p.snapshot_path()), 0o600);
    no_temporaries(&p.run_dir);
}

#[cfg(unix)]
#[test]
fn a_rebuilt_snapshot_is_readable_and_writable_by_its_owner_only() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.reconcile(SESSION_ID);
    // Corrupt but private: only the rebuild branch can write the replacement.
    fs::write(p.snapshot_path(), "[1, 2]").expect("corrupt");
    set_mode(&p.snapshot_path(), 0o600);
    assert!(
        p.reconcile(SESSION_ID).is_empty(),
        "a rebuild sends nothing"
    );
    assert_eq!(p.snapshot()["runId"], RUN_ID);
    assert_eq!(mode_of(&p.snapshot_path()), 0o600);
    no_temporaries(&p.run_dir);
}

#[cfg(unix)]
#[test]
fn the_lock_directory_is_private_while_held_including_a_taken_over_stale_one() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    let modes = record_lock_modes(&fx, &p.run_dir);
    assert_eq!(p.reconcile(SESSION_ID).len(), 2);
    assert_eq!(*modes.lock().expect("modes"), vec![0o700, 0o700]);
    assert!(!p.has_lock());

    // An abandoned lock (private, so only its age decides) is replaced by a
    // private one.
    modes.lock().expect("modes").clear();
    fs::create_dir(p.run_dir.join(".telemetry-lock")).expect("stale lock");
    set_mode(&p.run_dir.join(".telemetry-lock"), 0o755);
    fx.set_clock(crate::telemetry::system_now() + 3600.0);
    p.write_status(&phase_status(
        json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
    ));
    assert_eq!(p.reconcile(SESSION_ID).len(), 1);
    assert_eq!(*modes.lock().expect("modes"), vec![0o700]);
    assert!(!p.has_lock());
}

#[cfg(unix)]
#[test]
fn a_snapshot_other_users_can_write_is_not_trusted_and_is_replaced_privately() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    assert_eq!(p.reconcile(SESSION_ID).len(), 2);
    // Another user rewrites a world-writable snapshot to forge a run identity,
    // owner and history; read, it would re-send DISCOVER under that identity.
    fs::write(
        p.snapshot_path(),
        json!({
            "runId": OTHER_SESSION,
            "sessionId": OTHER_SESSION,
            "phases": {},
            "gateFailures": [],
            "completed": false,
        })
        .to_string(),
    )
    .expect("forged");
    set_mode(&p.snapshot_path(), 0o666);
    assert!(
        p.reconcile(SESSION_ID).is_empty(),
        "nothing is sent from untrusted state"
    );
    assert_eq!(mode_of(&p.snapshot_path()), 0o600, "replaced privately");
    let snapshot = p.snapshot();
    assert_eq!(snapshot["runId"], RUN_ID, "the forged identity is not used");
    assert_eq!(snapshot["sessionId"], SESSION_ID);
    assert_eq!(snapshot["phases"], phase_status(json!({}))["phases"]);
    no_temporaries(&p.run_dir);

    // Group-writable is not private either.
    set_mode(&p.snapshot_path(), 0o620);
    p.write_status(&phase_status(
        json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
    ));
    assert!(p.reconcile(SESSION_ID).is_empty());
    assert_eq!(mode_of(&p.snapshot_path()), 0o600);
}

#[cfg(unix)]
#[test]
fn a_lock_directory_other_users_can_write_is_not_honoured() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    // Fresh by the clock, but anyone may keep a world-writable directory fresh
    // (adding an entry updates its time), so it says nothing about a holder.
    fs::create_dir(p.run_dir.join(".telemetry-lock")).expect("lock");
    set_mode(&p.run_dir.join(".telemetry-lock"), 0o777);
    let modes = record_lock_modes(&fx, &p.run_dir);
    assert_eq!(p.reconcile(SESSION_ID).len(), 2);
    assert_eq!(
        *modes.lock().expect("modes"),
        vec![0o700, 0o700],
        "replaced by a private lock"
    );
    assert!(!p.has_lock());

    // Group-writable is not private either; a symbolic link is still refused.
    fs::create_dir(p.run_dir.join(".telemetry-lock")).expect("lock");
    set_mode(&p.run_dir.join(".telemetry-lock"), 0o770);
    p.write_status(&phase_status(
        json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
    ));
    assert_eq!(p.reconcile(SESSION_ID).len(), 1);
    assert!(!p.has_lock());
    let elsewhere = fx.dir.path().join("elsewhere");
    fs::create_dir(&elsewhere).expect("target");
    set_mode(&elsewhere, 0o777);
    std::os::unix::fs::symlink(&elsewhere, p.run_dir.join(".telemetry-lock")).expect("link");
    p.write_status(&phase_status(
        json!({"current_phase": "estimate", "phases": with_phases(&[("clarify", "completed"), ("design", "completed")])}),
    ));
    assert!(p.reconcile(SESSION_ID).is_empty());
    assert!(elsewhere.exists(), "a linked lock is never removed through");
}

/// One second before 1970: a time `duration_since(UNIX_EPOCH)` cannot convert.
#[cfg(unix)]
fn before_1970() -> std::time::SystemTime {
    std::time::UNIX_EPOCH
        .checked_sub(Duration::from_secs(1))
        .expect("pre-epoch time")
}

/// About thirty years ahead of the real clock.
#[cfg(unix)]
fn far_future() -> std::time::SystemTime {
    std::time::SystemTime::now() + Duration::from_secs(30 * 365 * 24 * 3600)
}

/// A real lock directory with the given mode and modification time, as its
/// owner can make it (`chmod`, then `touch -d`).
#[cfg(unix)]
fn plant_lock(run_dir: &Path, mode: u32, modified: std::time::SystemTime) {
    let lock = run_dir.join(".telemetry-lock");
    fs::create_dir(&lock).expect("lock");
    set_mode(&lock, mode);
    fs::File::open(&lock)
        .expect("open lock")
        .set_times(fs::FileTimes::new().set_modified(modified))
        .expect("set lock time");
    assert_eq!(
        fs::symlink_metadata(&lock)
            .expect("stat")
            .modified()
            .expect("mtime"),
        modified,
        "the filesystem kept the planted time"
    );
}

#[cfg(unix)]
#[test]
fn a_writable_lock_is_replaced_whatever_its_time() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    let modes = record_lock_modes(&fx, &p.run_dir);
    // A time before 1970 has no age to compare, which must not let a lock
    // anyone can write survive: it would block reporting for good.
    plant_lock(&p.run_dir, 0o777, before_1970());
    assert_eq!(p.reconcile(SESSION_ID).len(), 2, "reporting proceeds");
    assert_eq!(
        *modes.lock().expect("modes"),
        vec![0o700, 0o700],
        "replaced by a private lock"
    );
    assert!(!p.has_lock(), "the replacement is released");

    // Nor does a time far ahead keep a group-writable lock alive.
    modes.lock().expect("modes").clear();
    plant_lock(&p.run_dir, 0o770, far_future());
    p.write_status(&phase_status(
        json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
    ));
    assert_eq!(p.reconcile(SESSION_ID).len(), 1, "reporting proceeds");
    assert_eq!(*modes.lock().expect("modes"), vec![0o700]);
    assert!(!p.has_lock(), "the replacement is released");
}

#[cfg(unix)]
#[test]
fn a_private_lock_with_an_impossible_time_is_stale_and_a_fresh_one_still_blocks() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    // A private lock that is fresh by the clock is honoured.
    plant_lock(&p.run_dir, 0o700, std::time::SystemTime::now());
    assert!(
        p.reconcile(SESSION_ID).is_empty(),
        "the holder is about to report the same transitions"
    );
    assert!(
        p.has_lock() && !p.has_snapshot(),
        "a fresh lock is left alone"
    );
    fs::remove_dir(p.run_dir.join(".telemetry-lock")).expect("unlock");

    // No live hook made a lock dated before 1970 or decades ahead: either
    // would otherwise be held forever (or until that date).
    let modes = record_lock_modes(&fx, &p.run_dir);
    plant_lock(&p.run_dir, 0o700, before_1970());
    assert_eq!(p.reconcile(SESSION_ID).len(), 2, "reporting proceeds");
    assert!(!p.has_lock(), "the replacement is released");
    plant_lock(&p.run_dir, 0o700, far_future());
    p.write_status(&phase_status(
        json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
    ));
    assert_eq!(p.reconcile(SESSION_ID).len(), 1, "reporting proceeds");
    assert_eq!(*modes.lock().expect("modes"), vec![0o700, 0o700, 0o700]);
    assert!(!p.has_lock(), "the replacement is released");
}

#[test]
fn an_unwritable_run_directory_emits_nothing() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    // A file where the lock directory must be created is not a stale directory.
    fs::write(p.run_dir.join(".telemetry-lock"), "x").expect("file lock");
    fx.set_clock(crate::telemetry::system_now() + 3600.0);
    assert!(p.reconcile(SESSION_ID).is_empty());
}

// ------------------------------------------------------------ run discovery

#[test]
fn runs_are_found_from_the_nearest_migration_tree_walking_up_a_bounded_number_of_levels() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    let nested = p.root.join("a/b/c");
    fs::create_dir_all(&nested).expect("nested");
    // Started three levels below the project: found.
    let before = fx.sink.count();
    fx.set_stdin(
        &json!({"session_id": SESSION_ID, "cwd": nested.display().to_string()}).to_string(),
    );
    assert_eq!(fx.run(&["migration", "--reconcile"]).0, 0);
    assert_eq!(fx.sink.count() - before, 2);

    // Five levels below: out of reach.
    let fx2 = fixture();
    let q = Project::gcp(&fx2, &phase_status(json!({})));
    let deep = q.root.join("a/b/c/d/e");
    fs::create_dir_all(&deep).expect("deep");
    fx2.set_stdin(
        &json!({"session_id": SESSION_ID, "cwd": deep.display().to_string()}).to_string(),
    );
    assert_eq!(fx2.run(&["migration", "--reconcile"]).0, 0);
    assert_eq!(fx2.sink.count(), 0);
}

#[test]
fn a_sibling_projects_runs_are_not_mixed_in() {
    let fx = fixture();
    let a = Project::named(&fx, &phase_status(json!({})), &[], "0226-1430");
    let b = Project::named(
        &fx,
        &phase_status(json!({"migration_id": "x", "run_id": OTHER_SESSION})),
        &[],
        "0226-1431",
    );
    assert_eq!(a.reconcile(SESSION_ID).len(), 2);
    assert!(
        !b.has_snapshot(),
        "another project's run is not reported from this one's cwd"
    );
}

#[test]
fn a_symbolic_link_run_directory_is_not_a_run() {
    #[cfg(unix)]
    {
        let fx = fixture();
        let p = Project::gcp(&fx, &phase_status(json!({})));
        let outside = fx.dir.path().join("outside-run");
        fs::create_dir_all(&outside).expect("outside");
        fs::write(
            outside.join(".phase-status.json"),
            phase_status(json!({})).to_string(),
        )
        .expect("status");
        std::os::unix::fs::symlink(&outside, p.root.join(".migration/linked")).expect("link");
        let bodies = p.reconcile(SESSION_ID);
        assert_eq!(bodies.len(), 2, "only the real run");
        assert!(!outside.join(".telemetry-snapshot.json").exists());
    }
}

#[test]
fn a_symbolic_link_state_file_is_unreadable_so_the_run_is_skipped() {
    #[cfg(unix)]
    {
        let fx = fixture();
        let p = Project::gcp(&fx, &phase_status(json!({})));
        let real = p.run_dir.join("real-status.json");
        fs::rename(p.run_dir.join(".phase-status.json"), &real).expect("rename");
        std::os::unix::fs::symlink(&real, p.run_dir.join(".phase-status.json")).expect("link");
        assert!(p.reconcile(SESSION_ID).is_empty());
        assert!(!p.has_snapshot());
    }
}

#[test]
fn an_edit_outside_any_migration_tree_is_no_work_at_all() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    assert!(p
        .post_write(&p.root.join("src/main.rs"), SESSION_ID)
        .is_empty());
    assert!(!p.has_snapshot());
    // The run is found from the edited path's own tree even without a cwd.
    let inside = p.run_dir.join("preferences.json");
    fx.set_stdin(&json!({"session_id": SESSION_ID, "tool_input": {"file_path": inside.display().to_string()}}).to_string());
    assert_eq!(fx.run(&["migration"]).0, 0);
    assert_eq!(fx.sink.count(), 2);
}

#[test]
fn a_state_file_written_through_a_shell_command_is_reported_when_the_bash_hook_fires() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    let bash = |command: &str| json!({"tool_name": "Bash", "tool_input": {"command": command}});
    let first = p.hook(
        &[],
        SESSION_ID,
        &bash("cat > .migration/0226-1430/.phase-status.json <<'EOF'\n{...}\nEOF"),
    );
    assert_eq!(
        summary(&first),
        vec![
            row("PHASE_COMPLETED", Some("DISCOVER"), Some("SUCCESS")),
            row("RUN_STARTED", None, None)
        ]
    );
    assert!(
        p.hook(&[], SESSION_ID, &bash("git status")).is_empty(),
        "an unrelated command finds nothing new"
    );
}

#[test]
fn garbage_on_stdin_or_odd_payload_types_never_fail_the_hook() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    for stdin in [
        "not json".to_string(),
        "[1]".to_string(),
        String::new(),
        json!({"cwd": 5}).to_string(),
        json!({"cwd": ["x"], "tool_input": 3, "session_id": 7, "file_path": 9}).to_string(),
        json!({"workspace_roots": "x"}).to_string(),
        json!({"workspace_roots": []}).to_string(),
    ] {
        fx.set_stdin(&stdin);
        let (code, out, err) = fx.run(&["migration", "--reconcile"]);
        assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""), "{stdin}");
    }
    assert_eq!(
        fx.sink.count(),
        0,
        "no usable project location: nothing is reported"
    );
    assert!(!p.has_snapshot());
}

#[test]
fn workspace_roots_and_conversation_id_serve_hosts_that_send_them() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    fx.set_stdin(
        &json!({"conversation_id": SESSION_ID, "workspace_roots": [p.root.display().to_string()]})
            .to_string(),
    );
    assert_eq!(fx.run(&["migration", "--reconcile"]).0, 0);
    let bodies = fx.sink.bodies_from(0);
    assert_eq!(bodies.len(), 2);
    assert!(bodies
        .iter()
        .all(|body| activity(body)["sessionId"] == SESSION_ID));
}

// ------------------------------------------------------- run modes / endings

#[test]
fn a_run_executed_after_a_decide_only_finish_ends_again_under_its_new_run_mode() {
    let fx = fixture();
    let decided = phase_status(
        json!({"current_phase": "complete", "run_mode": "decide", "phases": with_phases(&[("clarify", "completed"), ("design", "completed"), ("estimate", "completed"), ("workshop", "completed")])}),
    );
    let p = Project::gcp(&fx, &decided);
    let first = completions(&p.reconcile(SESSION_ID));
    assert_eq!(
        first
            .iter()
            .map(|e| e["attributes"]["runMode"].clone())
            .collect::<Vec<_>>(),
        vec![json!("DECIDE")]
    );
    let mut executing = decided.clone();
    executing["run_mode"] = json!("decide_and_execute");
    executing["current_phase"] = json!("generate");
    executing["phases"] = with_phases(&[
        ("clarify", "completed"),
        ("design", "completed"),
        ("estimate", "completed"),
        ("workshop", "completed"),
        ("generate", "in_progress"),
    ]);
    p.write_status(&executing);
    assert!(
        p.reconcile(SESSION_ID).is_empty(),
        "opting in is not yet an ending"
    );
    let mut finished = decided.clone();
    finished["run_mode"] = json!("decide_and_execute");
    finished["phases"] = all_completed();
    p.write_status(&finished);
    let again = p.reconcile(SESSION_ID);
    assert_eq!(
        summary(&again),
        vec![
            row("PHASE_COMPLETED", Some("FEEDBACK"), Some("SUCCESS")),
            row("PHASE_COMPLETED", Some("GENERATE"), Some("SUCCESS")),
            row("RUN_COMPLETED", None, Some("SUCCESS")),
        ]
    );
    assert_eq!(
        completions(&again)[0]["attributes"]["runMode"],
        "DECIDE_AND_EXECUTE"
    );
    assert_eq!(
        p.snapshot()["completedRunModes"],
        json!(["DECIDE", "DECIDE_AND_EXECUTE"])
    );
    assert!(
        p.reconcile(SESSION_ID).is_empty(),
        "the same ending is never reported twice"
    );
}

#[test]
fn an_explicit_execute_request_ends_the_run_only_once_generate_is_completed() {
    let fx = fixture();
    let decided = phase_status(
        json!({"current_phase": "complete", "run_mode": "decide", "phases": with_phases(&[("clarify", "completed"), ("design", "completed"), ("estimate", "completed"), ("workshop", "completed")])}),
    );
    let p = Project::gcp(&fx, &decided);
    assert!(summary(&p.reconcile(SESSION_ID)).contains(&row(
        "RUN_COMPLETED",
        None,
        Some("SUCCESS")
    )));
    let mut flipped = decided.clone();
    flipped["run_mode"] = json!("decide_and_execute");
    p.write_status(&flipped);
    assert!(
        p.reconcile(SESSION_ID).is_empty(),
        "run_mode flipped, Generate not yet run: nothing has ended"
    );
    assert_eq!(p.snapshot()["completedRunModes"], json!(["DECIDE"]));
    flipped["phases"] = all_completed();
    p.write_status(&flipped);
    let ended = completions(&p.reconcile(SESSION_ID));
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0]["attributes"]["runMode"], "DECIDE_AND_EXECUTE");
    assert!(p.reconcile(SESSION_ID).is_empty());
}

#[test]
fn the_backbone_decides_completion_when_current_phase_is_absent() {
    let fx = fixture();
    let finished = phase_status(
        json!({"run_mode": "decide_and_execute", "phases": all_completed(), "current_phase": null}),
    );
    assert!(finished.get("current_phase").is_none());
    let done = Project::gcp(&fx, &finished);
    let ended = completions(&done.reconcile(SESSION_ID));
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0]["attributes"]["runMode"], "DECIDE_AND_EXECUTE");
    assert!(done.reconcile(SESSION_ID).is_empty());
    // A decision-only run that omits the field has not ended: Generate stays opt-in.
    let decided = phase_status(
        json!({"run_mode": "decide", "current_phase": null, "phases": with_phases(&[("clarify", "completed"), ("design", "completed"), ("estimate", "completed")])}),
    );
    let open = Project::named(
        &fx,
        &decided,
        &[("gcp-resource-inventory.json", gcp_inventory())],
        "0226-1500",
    );
    assert!(!summary(&open.reconcile(SESSION_ID)).contains(&row(
        "RUN_COMPLETED",
        None,
        Some("SUCCESS")
    )));
    assert_eq!(open.snapshot()["completed"], false);
}

#[test]
fn each_run_mode_ends_once_even_across_a_workshop_re_entry() {
    let fx = fixture();
    let ready = with_phases(&[
        ("clarify", "completed"),
        ("design", "completed"),
        ("estimate", "completed"),
        ("workshop", "completed"),
    ]);
    let decided =
        phase_status(json!({"current_phase": "complete", "run_mode": "decide", "phases": ready}));
    let p = Project::gcp(&fx, &decided);
    let modes = |bodies: Vec<Value>| {
        completions(&bodies)
            .iter()
            .map(|e| e["attributes"]["runMode"].clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(modes(p.reconcile(SESSION_ID)), vec![json!("DECIDE")]);
    let mut executed = decided.clone();
    executed["run_mode"] = json!("decide_and_execute");
    executed["phases"] = all_completed();
    p.write_status(&executed);
    assert_eq!(
        modes(p.reconcile(SESSION_ID)),
        vec![json!("DECIDE_AND_EXECUTE")]
    );
    assert_eq!(
        p.snapshot()["completedRunModes"],
        json!(["DECIDE", "DECIDE_AND_EXECUTE"])
    );
    let mut reentered = decided.clone();
    reentered["current_phase"] = json!("estimate");
    reentered["phases"] = with_phases(&[
        ("clarify", "completed"),
        ("design", "completed"),
        ("estimate", "completed"),
        ("workshop", "in_progress"),
    ]);
    reentered
        .as_object_mut()
        .expect("object")
        .remove("run_mode");
    p.write_status(&reentered);
    assert!(modes(p.reconcile(SESSION_ID)).is_empty());
    let mut again = decided.clone();
    again["phases"] = ready_phases();
    p.write_status(&again);
    assert!(
        modes(p.reconcile(SESSION_ID)).is_empty(),
        "a second decision-only ending is not reported again"
    );
    p.write_status(&executed);
    assert!(
        modes(p.reconcile(SESSION_ID)).is_empty(),
        "nor a second executed one"
    );
    assert_eq!(
        p.snapshot()["completedRunModes"],
        json!(["DECIDE", "DECIDE_AND_EXECUTE"])
    );
}

fn ready_phases() -> Value {
    with_phases(&[
        ("clarify", "completed"),
        ("design", "completed"),
        ("estimate", "completed"),
        ("workshop", "completed"),
    ])
}

#[test]
fn an_older_snapshot_that_kept_only_the_last_completed_mode_is_honoured() {
    let fx = fixture();
    let decided = phase_status(
        json!({"current_phase": "complete", "run_mode": "decide", "phases": ready_phases()}),
    );
    let p = Project::gcp(&fx, &decided);
    p.reconcile(SESSION_ID);
    let mut snapshot = p.snapshot();
    let first = snapshot
        .as_object_mut()
        .expect("object")
        .remove("completedRunModes")
        .expect("modes")[0]
        .clone();
    snapshot["completedRunMode"] = first;
    fs::write(p.snapshot_path(), snapshot.to_string()).expect("older shape");
    assert!(
        p.reconcile(SESSION_ID).is_empty(),
        "the recorded DECIDE ending is still known"
    );
    let mut executed = decided.clone();
    executed["run_mode"] = json!("decide_and_execute");
    executed["phases"] = all_completed();
    p.write_status(&executed);
    let ended: Vec<Value> = completions(&p.reconcile(SESSION_ID))
        .iter()
        .map(|e| e["attributes"]["runMode"].clone())
        .collect();
    assert_eq!(ended, vec![json!("DECIDE_AND_EXECUTE")]);
    assert_eq!(
        p.snapshot()["completedRunModes"],
        json!(["DECIDE", "DECIDE_AND_EXECUTE"])
    );
}

#[test]
fn an_llm_to_bedrock_run_in_its_dot_directory_reports_run_level_events_only() {
    let fx = fixture();
    let status = phase_status(json!({
        "migration_id": ".bedrock-0226-1430", "owning_skill": "LLM_TO_BEDROCK", "current_phase": "execute",
        "phases": {"assess": "completed", "execute": "in_progress"},
    }));
    let p = Project::named(&fx, &status, &[], ".bedrock-0226-1430");
    let first = p.reconcile(SESSION_ID);
    assert_eq!(summary(&first), vec![row("RUN_STARTED", None, None)]);
    assert_eq!(activity(&first[0])["skill"], "LLM_TO_BEDROCK");
    p.write_status(&phase_status(json!({
        "migration_id": ".bedrock-0226-1430", "owning_skill": "LLM_TO_BEDROCK", "current_phase": "complete",
        "phases": {"assess": "completed", "execute": "completed"},
    })));
    assert_eq!(
        summary(&p.reconcile(SESSION_ID)),
        vec![row("RUN_COMPLETED", None, Some("SUCCESS"))]
    );
}

#[test]
fn a_stale_reader_cannot_undo_a_newer_report() {
    // Two async hooks: the older one read the state (clarify in_progress)
    // before the newer one completed CLARIFY. The state is read again under
    // the lock, so a sweep that runs after the newer write reports it once and
    // never moves the snapshot back.
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.reconcile(SESSION_ID);
    p.write_status(&phase_status(
        json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
    ));
    let first = p.reconcile(SESSION_ID);
    let second = p.reconcile(SESSION_ID);
    assert_eq!(
        summary(&first),
        vec![row("PHASE_COMPLETED", Some("CLARIFY"), Some("SUCCESS"))]
    );
    assert!(second.is_empty());
    assert_eq!(p.snapshot()["phases"]["clarify"], "completed");
}

#[test]
fn concurrent_hooks_report_each_transition_once() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                // Each worker drives the shared state through the library entry point.
                let env = |name: &str| fx.env.lock().expect("lock").get(name).cloned();
                let now = crate::telemetry::system_now;
                let stdin = |_t: Duration| {
                    json!({"session_id": SESSION_ID, "cwd": p.root.display().to_string()})
                        .to_string()
                };
                let ctx = crate::telemetry::Ctx {
                    home: Some(fx.home.as_path()),
                    env: &env,
                    cwd: p.root.as_path(),
                    telemetry_dir: Some(fx.bundle.as_path()),
                    now: &now,
                    sink: &fx.sink,
                    stdin: &stdin,
                };
                crate::telemetry::migration::main(&["--reconcile"], &ctx);
            });
        }
    });
    assert_eq!(
        fx.sink.count(),
        2,
        "the lock lets one hook report the transitions"
    );
    assert!(p.reconcile(SESSION_ID).is_empty());
}

// ------------------------------------------------------------------ gates

#[test]
fn a_failed_gate_is_reported_once_per_phase_and_the_later_pass_as_its_own_event() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.write_gates(&json!({"clarify": {"reason": "missing", "field": "preferences.json", "at": "2026-02-26T15:40:00Z"}}));
    let first = p.reconcile(SESSION_ID);
    p.write_gates(&json!({"clarify": {"reason": "invalid", "field": "preferences.json", "at": "2026-02-26T15:41:00Z"}}));
    let repeat = p.reconcile(SESSION_ID);
    p.write_status(&phase_status(
        json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
    ));
    let passed = p.reconcile(SESSION_ID);
    let gate = first
        .iter()
        .map(activity)
        .find(|a| a["eventName"] == "GATE_FAILED")
        .expect("gate");
    assert_eq!(
        (
            gate["phase"].as_str(),
            gate["status"].as_str(),
            gate["attributes"]["failureReason"].as_str()
        ),
        (Some("CLARIFY"), Some("FAILED"), Some("MISSING"))
    );
    assert!(
        repeat.is_empty(),
        "a repeat failure of the same phase is not a new event"
    );
    assert_eq!(
        summary(&passed),
        vec![row("PHASE_COMPLETED", Some("CLARIFY"), Some("SUCCESS"))]
    );
    assert_eq!(p.snapshot()["gateFailures"], json!(["CLARIFY"]));
}

#[test]
fn malformed_gate_entries_and_unknown_reasons_are_ignored_without_consuming_the_phase() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.write_gates(&json!({"clarify": null, "design": {"reason": "__proto__", "at": "x"}, "nonsense": {"reason": "missing"}, "estimate": ["bad"]}));
    let gates = |bodies: Vec<Value>| {
        bodies
            .iter()
            .map(activity)
            .filter(|a| a["eventName"] == "GATE_FAILED")
            .cloned()
            .collect::<Vec<_>>()
    };
    let first = gates(p.reconcile(SESSION_ID));
    p.write_gates(&json!({"clarify": {"reason": "missing", "field": "preferences.json", "at": "x"}, "design": {"reason": "__proto__", "at": "x"}}));
    let second = gates(p.reconcile(SESSION_ID));
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["phase"], "DESIGN");
    assert!(first[0]
        .get("attributes")
        .is_none_or(|a| a.get("failureReason").is_none()));
    assert_eq!(second.len(), 1);
    assert_eq!(
        (
            second[0]["phase"].as_str(),
            second[0]["attributes"]["failureReason"].as_str()
        ),
        (Some("CLARIFY"), Some("MISSING"))
    );
}

#[test]
fn a_design_onward_gate_failure_carries_the_complexity_tier() {
    let fx = fixture();
    let p = Project::new(
        &fx,
        &phase_status(
            json!({"current_phase": "design", "phases": with_phases(&[("clarify", "completed")])}),
        ),
        &[
            ("gcp-resource-inventory.json", gcp_inventory()),
            (
                "migration-preview.json",
                json!({"complexity_signal": "complex"}),
            ),
        ],
    );
    p.write_gates(&json!({"design": {"reason": "missing", "field": "aws-design.json", "at": "x"}}));
    let bodies = p.reconcile(SESSION_ID);
    let gate = bodies
        .iter()
        .map(activity)
        .find(|a| a["eventName"] == "GATE_FAILED")
        .expect("gate");
    assert_eq!(gate["attributes"]["complexityTier"], "LARGE");
}

#[test]
fn a_held_gate_failure_is_re_sent_alone() {
    let fx = fixture();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.reconcile(SESSION_ID);
    p.write_gates(&json!({"design": {"reason": "invalid", "field": "aws-design.json", "at": "x"}}));
    fx.sink.respond_with(PostStatus::Status(403));
    let held = p.reconcile(SESSION_ID);
    fx.sink.respond_with(PostStatus::Status(200));
    let retried = p.reconcile(SESSION_ID);
    assert_eq!(
        summary(&held),
        vec![row("GATE_FAILED", Some("DESIGN"), Some("FAILED"))]
    );
    assert_eq!(
        summary(&retried),
        vec![row("GATE_FAILED", Some("DESIGN"), Some("FAILED"))]
    );
    assert_eq!(p.snapshot()["gateFailures"], json!(["DESIGN"]));
    assert!(p.reconcile(SESSION_ID).is_empty());
}

// -------------------------------------------------------------------- cli

fn real_plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-startup-advisor")
}

#[test]
fn the_cli_route_reports_with_the_cli_via_and_no_session_and_never_repeats() {
    for (marker, source) in [
        (None, "OTHER"),
        (Some("CODEX_SANDBOX"), "CODEX"),
        (Some("KIRO_IDE"), "KIRO"),
    ] {
        let fx = fixture();
        fx.clear_env();
        if let Some(marker) = marker {
            fx.set_env(marker, "fixture");
        }
        let p = Project::gcp(&fx, &phase_status(json!({})));
        let bodies = p.cli_reconcile();
        assert_eq!(bodies.len(), 2);
        assert!(bodies.iter().all(|b| b["source"] == source));
        assert!(
            bodies
                .iter()
                .all(|b| activity(b).get("sessionId").is_none()),
            "no caller-supplied session identity"
        );
        assert_eq!(p.snapshot()["via"], "cli");
        assert!(p.cli_reconcile().is_empty());
    }
}

#[test]
fn the_cli_route_never_reads_stdin() {
    let fx = fixture();
    fx.clear_env();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    fx.set_stdin(&json!({"session_id": SESSION_ID, "cwd": "/nowhere"}).to_string());
    assert_eq!(p.cli_reconcile().len(), 2);
}

#[test]
fn hook_hosts_skip_the_cli_route_when_the_plugin_hooks_are_reachable() {
    for markers in [
        &["CLAUDECODE"][..],
        &["CLAUDE_CODE_ENTRYPOINT"],
        &["CURSOR_AGENT"],
        &["CURSOR_AGENT", "CLAUDECODE"],
        &["CURSOR_VERSION", "CLAUDECODE"],
        &["CURSOR_PROJECT_DIR"],
    ] {
        let mut fx = fixture();
        fx.clear_env();
        for marker in markers {
            fx.set_env(marker, "1");
        }
        fx.bundle = real_plugin().join("scripts/telemetry");
        let p = Project::gcp(&fx, &phase_status(json!({})));
        assert!(p.cli_reconcile().is_empty(), "{markers:?}");
        assert_eq!(
            fx.run_in(&p.root, &["migration", "--reconcile", "--via", "cli"])
                .0,
            0
        );
        assert_eq!(
            fx.sink.count(),
            0,
            "{markers:?}: even the direct emitter flags"
        );
        assert!(!p.has_snapshot() && !p.has_lock());
        assert_eq!(
            p.hook(&["--reconcile"], SESSION_ID, &json!({})).len(),
            2,
            "the hook route still reports"
        );
        assert_eq!(p.snapshot()["via"], "hook");
    }
}

#[test]
fn a_standalone_hook_host_uses_the_cli_without_plugin_hooks() {
    let mut host = fixture();
    let standalone = host
        .dir
        .path()
        .join("installed/gcp-to-aws/references/vendored/telemetry");
    fs::create_dir_all(&standalone).expect("dir");
    fs::write(standalone.join("version.json"), "{\"version\": \"2.0.3\"}").expect("version");
    host.bundle = standalone;
    let p = Project::gcp(&host, &phase_status(json!({})));
    assert_eq!(
        p.cli_reconcile().len(),
        2,
        "CLAUDECODE is set, yet no hooks are reachable"
    );
    assert!(p.cli_reconcile().is_empty());
}

#[test]
fn the_cli_route_leaves_no_trace_without_accepted_consent_and_stops_after_an_opt_out() {
    for consent in [None, Some("OPT_OUT"), Some("REJECTED")] {
        let fx = Fx::new("cli-consent");
        if let Some(status) = consent {
            fx.write_record(status, INSTALL_ID);
        }
        fx.clear_env();
        fs::create_dir_all(fx.dir.path().join("project-0226-1430/.migration")).expect("dir");
        let p = Project::gcp(&fx, &phase_status(json!({})));
        fs::write(
            p.root.join(".migration/telemetry.json"),
            "{\"consent\":\"granted\"}",
        )
        .expect("old project consent");
        assert!(p.cli_reconcile().is_empty());
        assert!(!p.has_snapshot() && !p.has_lock());
    }
    let fx = Fx::new("cli-optout");
    fx.clear_env();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    assert_eq!(fx.run(&["consent", "show"]).0, 0);
    assert_eq!(fx.run(&["consent", "accept"]).0, 0);
    assert_eq!(
        fx.sink.bodies_from(0)[0]["pluginTelemetryEvent"],
        json!({"consentRecorded": {}})
    );
    assert_eq!(p.cli_reconcile().len(), 2);
    let before = p.snapshot();
    let sent = fx.sink.count();
    assert_eq!(fx.run(&["consent", "opt-out"]).0, 0);
    assert_eq!(fx.sink.count(), sent, "opting out sends nothing");
    p.write_status(&phase_status(
        json!({"current_phase": "complete", "run_mode": "decide", "phases": all_completed()}),
    ));
    assert!(p.cli_reconcile().is_empty());
    assert_eq!(p.snapshot(), before);
    let state: Value = serde_json::from_str(&fx.record_text().expect("record")).expect("json");
    assert_eq!(state["consentStatus"], "OPT_OUT");
    assert_eq!(state["installId"], fx.sink.bodies_from(0)[1]["installId"]);
}

#[test]
fn the_cli_route_preserves_gate_replay_and_each_completion_mode_once() {
    let fx = fixture();
    fx.clear_env();
    let p = Project::gcp(&fx, &phase_status(json!({})));
    p.cli_reconcile();
    p.write_gates(&json!({"design": {"reason": "missing", "field": "aws-design.json"}}));
    fx.sink.respond_with(PostStatus::Status(429));
    assert_eq!(activity(&p.cli_reconcile()[0])["eventName"], "GATE_FAILED");
    assert_eq!(p.snapshot()["gateFailures"], json!([]));
    fx.sink.respond_with(PostStatus::Status(200));
    assert_eq!(
        activity(&p.cli_reconcile()[0])["attributes"]["failureReason"],
        "MISSING"
    );
    assert!(p.cli_reconcile().is_empty());
    let mut phases = all_completed();
    phases["generate"] = json!("pending");
    p.write_status(&phase_status(
        json!({"current_phase": "complete", "phases": phases.clone(), "run_mode": "decide"}),
    ));
    let first = p.cli_reconcile();
    p.write_status(&phase_status(json!({"current_phase": "complete", "phases": phases.clone(), "run_mode": "decide_and_execute"})));
    assert!(p.cli_reconcile().is_empty());
    p.write_status(&phase_status(json!({"current_phase": "complete", "phases": all_completed(), "run_mode": "decide_and_execute"})));
    let second = p.cli_reconcile();
    let modes: Vec<Value> = completions(&[first, second].concat())
        .iter()
        .map(|e| e["attributes"]["runMode"].clone())
        .collect();
    assert_eq!(modes, vec![json!("DECIDE"), json!("DECIDE_AND_EXECUTE")]);
    p.write_status(&phase_status(
        json!({"current_phase": "estimate", "phases": phases}),
    ));
    p.cli_reconcile();
    p.write_status(&phase_status(json!({"current_phase": "complete", "phases": all_completed(), "run_mode": "decide_and_execute"})));
    assert!(completions(&p.cli_reconcile()).is_empty());
    assert!(p.cli_reconcile().is_empty());
}

#[test]
fn an_incomplete_bundle_without_a_version_does_not_advance_the_snapshot() {
    let fx = fixture();
    fx.clear_env();
    fs::remove_file(fx.bundle.join("version.json")).expect("remove");
    let p = Project::gcp(&fx, &phase_status(json!({})));
    assert!(p.cli_reconcile().is_empty());
    assert!(!p.has_snapshot());
    let (code, out, err) = fx.run(&["status"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("\"pluginVersion\": null"));
}

#[test]
fn the_registered_hook_commands_run_the_reporter_from_the_real_plugin_tree() {
    // The tree's hook registrations still name the reporter; the reporting-mode
    // check reads them, and sees the Python script that is still shipped.
    let mut fx = fixture();
    fx.bundle = real_plugin().join("scripts/telemetry");
    fx.with(|ctx| {
        assert_eq!(
            crate::telemetry::client::migration_reporting_mode(ctx),
            "hook"
        );
    });
    fx.set_env("CLAUDECODE", "");
    fx.set_env("CURSOR_VERSION", "1");
    fx.with(|ctx| {
        assert_eq!(
            crate::telemetry::client::migration_reporting_mode(ctx),
            "hook"
        );
    });
}
