//! The consent record, the notice and the consent commands.

use std::fs;

use super::fx::{Fx, INSTALL_ID};
use crate::telemetry::consent::{self, Status};

/// The approved wording, kept here as a second independent copy on purpose: a
/// test that imported the string it checks would pass no matter what the
/// string said. Both bracketed controls are retained exactly as the source has
/// them and mapped back before comparing.
const APPROVED_NOTICE: &str = "AWS Startup Advisor plugin \u{2014} usage data notice\n\
\n\
You've installed the AWS Startup Advisor plugin, which provides skills \
that help you build on AWS through your AI coding agent. To help us \
improve it, we'd like to collect optional usage data about how the plugin \
is used. What we collect: which skill you invoked, the plugin version, the \
host tool you're using, a randomly generated install identifier, and, for \
migration skills, the source cloud provider and which migration phase you \
start and complete. The usage data above is not linked to your identity. \
You can opt-out by [insert command/config setting]. Opting out does not \
change any plugin functionality.\n\
\n\
[Acknowledge]\n";

const PLACEHOLDER: &str = "[insert command/config setting]";
const ACK_PLACEHOLDER: &str = "[Acknowledge]";
const OPT_OUT_INSTRUCTION: &str =
    "setting \"consentStatus\" to \"OPT_OUT\" in ~/.aws-startup-advisor/plugin-telemetry.json";
const ACKNOWLEDGE_PROMPT: &str = "Do you acknowledge this notice? Reply \"yes\" to acknowledge, or \"opt out\" if you would rather no usage data was collected. Either answer is fine, and nothing is collected until you answer.";

fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn as_source(text: &str) -> String {
    words(text)
        .replace(OPT_OUT_INSTRUCTION, PLACEHOLDER)
        .replace(&words(ACKNOWLEDGE_PROMPT), ACK_PLACEHOLDER)
}

// ------------------------------------------------------------------- notice

#[test]
fn the_notice_matches_the_approved_text_word_for_word() {
    assert_eq!(as_source(&consent::disclaimer()), words(APPROVED_NOTICE));
}

#[test]
fn both_bracketed_controls_are_substituted() {
    let shown = consent::disclaimer();
    assert!(!shown.contains(PLACEHOLDER));
    assert!(!shown.contains(ACK_PLACEHOLDER));
    assert!(shown.contains('?'));
    assert!(shown.contains("yes") && shown.contains("opt out"));
    assert!(shown.contains("consentStatus") && shown.contains("OPT_OUT"));
}

#[test]
fn the_notice_names_no_script_install_path_or_slash_command() {
    let shown = consent::disclaimer();
    for word in ["opt_out.py", "python", "toolkit", "/aws-startup-advisor:"] {
        assert!(!shown.contains(word), "{word}");
    }
}

#[test]
fn no_line_exceeds_the_wrap_width_and_the_path_is_never_split() {
    let shown = consent::disclaimer();
    assert!(shown.lines().all(|line| line.chars().count() <= 79));
    assert!(shown.contains("~/.aws-startup-advisor/plugin-telemetry.json"));
}

#[test]
fn the_structure_survives_wrapping() {
    let shown = consent::disclaimer();
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(
        lines[0],
        "AWS Startup Advisor plugin \u{2014} usage data notice"
    );
    assert_eq!(lines[1], "");
    let blank = lines
        .iter()
        .skip(2)
        .position(|line| line.is_empty())
        .expect("a blank line before the question")
        + 2;
    assert_eq!(lines[blank + 1..].join(" "), words(ACKNOWLEDGE_PROMPT));
    assert!(shown.ends_with('\n') && !shown.ends_with("\n\n"));
}

#[test]
fn show_prints_the_notice_exactly_and_records_nothing() {
    let fx = Fx::new("show");
    let (status, out, err) = fx.run(&["consent", "show"]);
    assert_eq!((status, err.as_str()), (0, ""));
    assert_eq!(out, consent::disclaimer());
    assert!(!fx.home.join(".aws-startup-advisor").exists());
}

// --------------------------------------------------------------- read_state

#[test]
fn absent_is_no_consent() {
    let fx = Fx::new("absent");
    fx.with(|ctx| {
        assert_eq!(consent::read_state(ctx), None);
        assert_eq!(consent::consent_status(ctx), None);
        assert!(!consent::is_accepted(ctx));
    });
}

#[test]
fn every_invalid_record_fails_closed() {
    let fx = Fx::new("invalid");
    let uuid = INSTALL_ID;
    let bodies = [
        "{\"installId\":".to_string(),
        "not json at all".to_string(),
        "[]".to_string(),
        "null".to_string(),
        String::new(),
        format!("{{\"installId\": \"{uuid}\"}}"),
        "{\"consentStatus\": \"ACCEPTED\"}".to_string(),
        "{\"installId\": \"\", \"consentStatus\": \"ACCEPTED\"}".to_string(),
        "{\"installId\": 7, \"consentStatus\": \"ACCEPTED\"}".to_string(),
        "{\"installId\": null, \"consentStatus\": \"ACCEPTED\"}".to_string(),
        "{\"installId\": \"abc\", \"consentStatus\": \"ACCEPTED\"}".to_string(),
        format!("{{\"installId\": \"{uuid}\\n\", \"consentStatus\": \"ACCEPTED\"}}"),
        format!("{{\"installId\": \"{uuid}\", \"consentStatus\": \"accepted\"}}"),
        format!("{{\"installId\": \"{uuid}\", \"consentStatus\": \"MAYBE\"}}"),
        format!("{{\"installId\": \"{uuid}\", \"consentStatus\": \"REJECTED\"}}"),
        format!("{{\"installId\": \"{uuid}\", \"consentStatus\": true}}"),
        format!("{{\"installId\": \"{uuid}\", \"consentStatus\": [\"ACCEPTED\"]}}"),
    ];
    for body in bodies {
        fx.write_record_text(&body);
        fx.with(|ctx| {
            assert_eq!(consent::read_state(ctx), None, "{body}");
            assert!(!consent::is_accepted(ctx), "{body}");
        });
    }
}

#[test]
fn a_non_utf8_record_and_an_unreadable_path_fail_closed() {
    let fx = Fx::new("binary");
    let path = fx.consent_path();
    fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    fs::write(&path, [0xff, 0xfe, 0x00, 0x7b]).expect("write");
    fx.with(|ctx| assert!(!consent::is_accepted(ctx)));
    // A directory where the record should be.
    fs::remove_file(&path).expect("remove");
    fs::create_dir(&path).expect("dir at record path");
    fx.with(|ctx| assert!(!consent::is_accepted(ctx)));
}

#[test]
fn an_oversized_record_fails_closed() {
    let fx = Fx::new("oversized");
    let padding = " ".repeat(2 << 20);
    fx.write_record_text(&format!(
        "{{\"installId\": \"{INSTALL_ID}\", \"consentStatus\": \"ACCEPTED\"}}{padding}"
    ));
    fx.with(|ctx| assert!(!consent::is_accepted(ctx)));
}

#[test]
fn an_unknown_home_directory_means_no_consent_and_no_write() {
    let mut fx = Fx::new("nohome");
    fx.accept();
    fx.home_known = false;
    fx.with(|ctx| assert!(!consent::is_accepted(ctx)));
    let (status, out, err) = fx.run(&["consent", "accept"]);
    assert_eq!((status, out.as_str()), (1, ""));
    assert!(err.contains("Nothing was saved"));
    assert!(fx.sink.count() == 0);
}

#[test]
fn a_valid_accepted_record_is_accepted_in_either_uuid_case() {
    let fx = Fx::new("case");
    fx.write_record("ACCEPTED", &INSTALL_ID.to_uppercase());
    fx.with(|ctx| assert!(consent::is_accepted(ctx)));
}

#[test]
fn the_notices_own_instruction_works() {
    // A record carrying only the key the notice names is a supported input.
    let fx = Fx::new("handedit");
    fx.write_record_text("{\"consentStatus\": \"OPT_OUT\"}");
    fx.with(|ctx| {
        assert_eq!(consent::consent_status(ctx), Some(Status::OptOut));
        assert!(!consent::is_accepted(ctx));
    });
    let (status, out, _) = fx.run(&["consent", "session-start"]);
    assert_eq!(
        (status, out.as_str()),
        (0, ""),
        "an answered notice is silent"
    );
    let (_, status_out, _) = fx.run(&["consent", "status"]);
    assert!(status_out.contains("collection: OFF"));
}

#[test]
fn a_utf8_bom_does_not_undo_an_opt_out_or_an_acceptance() {
    let fx = Fx::new("bom");
    let path = fx.consent_path();
    fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    fs::write(&path, b"\xef\xbb\xbf{\"consentStatus\": \"OPT_OUT\"}").expect("write");
    fx.with(|ctx| assert_eq!(consent::consent_status(ctx), Some(Status::OptOut)));
    let body =
        format!("\u{feff}{{\"installId\": \"{INSTALL_ID}\", \"consentStatus\": \"ACCEPTED\"}}");
    fs::write(&path, body).expect("write");
    fx.with(|ctx| {
        assert!(consent::is_accepted(ctx));
        assert_eq!(
            consent::read_state(ctx)
                .and_then(|r| r.install_id)
                .as_deref(),
            Some(INSTALL_ID)
        );
    });
}

#[test]
fn the_hand_edit_the_notice_describes_turns_collection_off_and_keeps_the_id() {
    let fx = Fx::new("edit");
    fx.accept();
    fx.write_record("OPT_OUT", INSTALL_ID);
    fx.with(|ctx| {
        assert!(!consent::is_accepted(ctx));
        assert_eq!(
            consent::read_state(ctx)
                .and_then(|r| r.install_id)
                .as_deref(),
            Some(INSTALL_ID)
        );
    });
}

#[test]
fn no_environment_variable_overrides_the_record() {
    let fx = Fx::new("env");
    fx.accept();
    for name in ["AWS_STARTUP_ADVISOR_TELEMETRY", "DO_NOT_TRACK", "TELEMETRY"] {
        fx.set_env(name, "off");
    }
    fx.with(|ctx| assert!(consent::is_accepted(ctx)));
    let fx = Fx::new("env-off");
    fx.set_env("AWS_STARTUP_ADVISOR_TELEMETRY", "on");
    fx.with(|ctx| assert!(!consent::is_accepted(ctx)));
}

// -------------------------------------------------------------- write_state

#[test]
fn write_state_round_trips_and_has_exactly_two_keys() {
    let fx = Fx::new("write");
    let written = fx
        .with(|ctx| consent::write_state(ctx, Status::Accepted))
        .expect("write");
    let text = fx.record_text().expect("record");
    assert!(text.ends_with("}\n"));
    let value: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(value.as_object().expect("object").len(), 2);
    assert_eq!(value["consentStatus"], "ACCEPTED");
    assert_eq!(value["installId"], written.install_id.clone().expect("id"));
    assert!(consent::is_uuid(&written.install_id.expect("id")));
    fx.with(|ctx| assert!(consent::is_accepted(ctx)));
}

#[test]
fn the_install_id_is_a_version_4_uuid_and_survives_a_change_of_mind() {
    let fx = Fx::new("mind");
    let first = fx
        .with(|ctx| consent::write_state(ctx, Status::Accepted))
        .expect("accept")
        .install_id
        .expect("id");
    assert_eq!(first.as_bytes()[14], b'4');
    assert!(matches!(first.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
    let second = fx
        .with(|ctx| consent::write_state(ctx, Status::OptOut))
        .expect("opt out")
        .install_id
        .expect("id");
    assert_eq!(first, second, "flipping the answer is not a new install");
    assert_ne!(consent::uuid4(), consent::uuid4());
}

#[test]
fn a_missing_or_garbage_install_id_is_replaced_not_preserved() {
    let fx = Fx::new("garbage");
    fx.write_record_text("{\"installId\": \"nonsense\", \"consentStatus\": \"OPT_OUT\"}");
    let written = fx
        .with(|ctx| consent::write_state(ctx, Status::Accepted))
        .expect("write");
    assert!(consent::is_uuid(&written.install_id.expect("id")));
    fx.with(|ctx| assert!(consent::is_accepted(ctx)));
    let fx = Fx::new("minimal");
    fx.write_record_text("{\"consentStatus\": \"OPT_OUT\"}");
    let written = fx
        .with(|ctx| consent::write_state(ctx, Status::Accepted))
        .expect("write");
    assert!(consent::is_uuid(&written.install_id.expect("id")));
}

#[test]
fn write_state_leaves_no_temporary_files_behind() {
    let fx = Fx::new("tmpfiles");
    for status in [Status::Accepted, Status::OptOut, Status::Accepted] {
        fx.with(|ctx| consent::write_state(ctx, status))
            .expect("write");
    }
    let names: Vec<String> = fs::read_dir(fx.home.join(".aws-startup-advisor"))
        .expect("dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names, vec!["plugin-telemetry.json".to_string()]);
}

#[cfg(unix)]
#[test]
fn the_record_is_private_to_the_user() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fx::new("mode");
    fx.with(|ctx| consent::write_state(ctx, Status::Accepted))
        .expect("write");
    let mode = fs::metadata(fx.consent_path())
        .expect("meta")
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "group/other have no access: {mode:o}");
}

// ---------------------------------------------------------------- commands

#[test]
fn accept_writes_accepted_and_tells_the_user_how_to_opt_out() {
    let fx = Fx::new("accept");
    let (status, out, err) = fx.run(&["consent", "accept"]);
    assert_eq!((status, err.as_str()), (0, ""));
    assert!(out.starts_with("Acknowledged \u{2014} usage data collection is ON."));
    assert!(out.contains("toolkit telemetry consent opt-out"));
    assert!(out.contains("\"consentStatus\" to \"OPT_OUT\""));
    assert!(out.contains(&fx.consent_path().display().to_string()));
    fx.with(|ctx| assert!(consent::is_accepted(ctx)));
}

#[test]
fn opt_out_writes_opt_out_sends_nothing_and_says_features_still_work() {
    let fx = Fx::new("optout");
    fx.accept();
    let (status, out, _) = fx.run(&["consent", "opt-out"]);
    assert_eq!(status, 0);
    assert!(out.contains("Every feature of the plugin still works."));
    fx.with(|ctx| assert_eq!(consent::consent_status(ctx), Some(Status::OptOut)));
    assert_eq!(fx.sink.count(), 0, "an opt-out is never reported");
}

#[test]
fn accept_and_opt_out_fail_loudly_when_they_cannot_write() {
    // A file where the state directory should go: it cannot be created.
    let fx = Fx::new("blocked");
    fs::write(fx.home.join(".aws-startup-advisor"), "").expect("blocker");
    let (status, out, err) = fx.run(&["consent", "accept"]);
    assert_eq!(
        (status, out.as_str()),
        (1, ""),
        "must not report success it did not achieve"
    );
    assert!(err.contains("Could not record your choice"));
    let (status, out, err) = fx.run(&["consent", "opt-out"]);
    assert_eq!((status, out.as_str()), (1, ""));
    assert!(err.contains("the notice will be shown again next session"));
    assert_eq!(fx.sink.count(), 0, "a failed acknowledgement sends nothing");
}

#[test]
fn status_reports_on_off_and_nothing_recorded() {
    let fx = Fx::new("status");
    let (_, none, _) = fx.run(&["consent", "status"]);
    assert!(none.contains("Nothing is recorded yet"));
    assert!(none.contains("Expected record location:"));
    fx.run(&["consent", "accept"]);
    let (status, on, _) = fx.run(&["consent", "status"]);
    assert_eq!(status, 0);
    assert!(on.contains("collection: ON"));
    assert!(on.contains("toolkit telemetry consent opt-out"));
    fx.write_record("OPT_OUT", INSTALL_ID);
    let (_, off, _) = fx.run(&["consent", "status"]);
    assert!(off.contains("collection: OFF") && off.contains("opted out"));
    assert!(off.contains("toolkit telemetry consent accept"));
    fx.write_record_text("garbage");
    let (_, corrupt, _) = fx.run(&["consent", "status"]);
    assert!(
        corrupt.contains("Nothing is recorded yet"),
        "a corrupt record reports like none"
    );
}

#[test]
fn usage_errors_exit_2_and_help_exits_0() {
    let fx = Fx::new("usage");
    for words in [
        &["consent"][..],
        &["consent", "bogus"],
        &["consent", "show", "extra"],
        &["consent", "check"],
        &["consent", "status", "x"],
        &["consent", "session-start", "x"],
        &["bogus"],
        &[],
    ] {
        assert_eq!(fx.run(words).0, 2, "{words:?}");
    }
    let (status, out, err) = fx.run(&["consent", "--help"]);
    assert_eq!((status, out.as_str()), (0, ""));
    assert!(err.starts_with("usage: toolkit telemetry consent"));
}

#[test]
fn the_session_start_hook_speaks_once_and_only_while_nothing_is_decided() {
    let fx = Fx::new("session");
    let (status, out, err) = fx.run(&["consent", "session-start"]);
    assert_eq!((status, err.as_str()), (0, ""));
    assert!(out.contains("toolkit telemetry consent show"));
    assert!(out.contains("VERBATIM"));
    assert!(out.contains("toolkit telemetry consent accept"));
    assert!(out.contains("toolkit telemetry consent opt-out"));
    assert!(out.contains("END YOUR TURN THERE"));
    assert!(out.contains("There is no environment variable"));
    assert!(
        !out.contains('{') && !out.contains("python"),
        "plain text, no JSON envelope"
    );
    assert!(
        out.ends_with("instead.\n"),
        "ends with the instruction's own newline"
    );
    for answered in ["ACCEPTED", "OPT_OUT"] {
        fx.write_record(answered, INSTALL_ID);
        assert_eq!(fx.run(&["consent", "session-start"]).1, "", "{answered}");
    }
    fx.write_record_text("{\"installId\":");
    assert!(
        !fx.run(&["consent", "session-start"]).1.is_empty(),
        "a corrupt record asks again"
    );
}

#[test]
fn the_session_start_hook_exits_zero_even_when_home_is_undiscoverable() {
    let mut fx = Fx::new("session-nohome");
    fx.home_known = false;
    let (status, out, _) = fx.run(&["consent", "session-start"]);
    assert_eq!(status, 0);
    assert!(out.contains("VERBATIM"));
}
