//! T-OC-FINAL-BODY, -META and -BOUND (B2 part) and B1's shared final-output
//! registry fixtures through the matcher (#5401 ruling 2): the pure
//! `sanitize_output` scrub of `bridge/output_scrub.rs` (R-OC-FINAL, D-OC19).
//!
//! Every assertion reads the complete frame. The tree values, the nested
//! text and the encoded frame are checked for the secrets independently of
//! the matcher under test ([`assert_no_secret`]).

use std::io::Cursor;

use toolkit_core::encoding::base64_standard;
use toolkit_core::{parse_json, sha256_hex, Json};

use super::output_scrub::{sanitize_frame, sanitize_frame_within, FAILED_SAFELY};
use super::protocol::read_request;

const FAILURE: &str = "{\"ok\":false,\"error\":\"AgentCore request failed\"}\n";
const INPUT: &str = "{\"ok\":false,\"error\":\"invalid bridge request\"}\n";

/// A synthetic PAYMENT-SIGNATURE value (base64 of a compact JSON header).
fn header() -> String {
    base64_standard(
        format!(
            "{{\"x402Version\":2,\"accepted\":{{\"scheme\":\"exact\"}},\"payload\":{{\"signature\":\"{}\"}}}}",
            signature()
        )
        .as_bytes(),
    )
}

/// A synthetic signed-payload `signature` (0x and 128 hex digits).
fn signature() -> String {
    format!("0x{}{}", sha256_hex(b"sig-a"), sha256_hex(b"sig-b"))
}

fn registry() -> Vec<String> {
    vec![header(), signature()]
}

/// `JSON.stringify(details, null, 2)` for a flat object.
fn pretty(details: &Json) -> String {
    let Json::Object(members) = details else {
        panic!("flat object")
    };
    let lines: Vec<String> = members
        .iter()
        .map(|(key, value)| {
            format!(
                "  {}: {}",
                Json::string(key.clone()).to_compact_string(),
                value.to_compact_string()
            )
        })
        .collect();
    format!("{{\n{}\n}}", lines.join(",\n"))
}

fn request_with_text(details: &Json, text: &str, secrets: &[String]) -> Json {
    Json::object(vec![
        ("operation", Json::string("sanitize_output")),
        (
            "candidate",
            Json::object(vec![
                (
                    "content",
                    Json::Array(vec![Json::object(vec![
                        ("type", Json::string("text")),
                        ("text", Json::string(text)),
                    ])]),
                ),
                ("details", details.clone()),
            ]),
        ),
        (
            "secrets",
            Json::Array(secrets.iter().map(|s| Json::string(s.clone())).collect()),
        ),
    ])
}

fn request(details: &Json, secrets: &[String]) -> Json {
    request_with_text(details, &pretty(details), secrets)
}

/// The request through B1's validator, then the scrub: (frame, exit).
fn run_within(request: &Json, budget: Option<usize>) -> (String, i32) {
    let validated = read_request(&mut Cursor::new(request.to_compact_string()))
        .expect("a valid sanitize_output request");
    let (bytes, status) = match budget {
        Some(budget) => sanitize_frame_within(&validated, budget),
        None => sanitize_frame(&validated),
    };
    (String::from_utf8(bytes).expect("an ASCII frame"), status)
}

fn run(request: &Json) -> (String, i32) {
    run_within(request, None)
}

/// The `result` of a success frame, after checking its envelope and LF.
fn result_of(frame: &str) -> Json {
    assert!(frame.ends_with('\n'), "{frame}");
    assert!(frame.is_ascii(), "{frame}");
    let document = parse_json(frame.trim_end_matches('\n')).expect("one JSON document");
    assert_eq!(document.get("ok"), Some(&Json::Bool(true)), "{frame}");
    document.get("result").cloned().expect("a result")
}

fn details_of(result: &Json) -> Json {
    result.get("details").cloned().expect("details")
}

fn text_of(result: &Json) -> String {
    let Some(Json::Array(content)) = result.get("content") else {
        panic!("content")
    };
    assert_eq!(content.len(), 1);
    assert_eq!(content[0].get("type").and_then(Json::as_str), Some("text"));
    content[0]
        .get("text")
        .and_then(Json::as_str)
        .expect("text")
        .to_owned()
}

/// Order-insensitive equality of objects (the parsed text and details).
fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Object(left), Json::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| same(value, other)))
        }
        (Json::Array(left), Json::Array(right)) => {
            left.len() == right.len() && left.iter().zip(right).all(|(a, b)| same(a, b))
        }
        _ => a == b,
    }
}

fn strings(value: &Json, out: &mut Vec<String>) {
    match value {
        Json::String(text) => out.push(text.clone()),
        Json::Array(items) => items.iter().for_each(|item| strings(item, out)),
        Json::Object(members) => {
            for (key, item) in members {
                out.push(key.clone());
                strings(item, out);
            }
        }
        _ => {}
    }
}

/// Independent of the matcher: no secret (whole, or any 32-byte piece of
/// one) in the encoded frame, in any tree string or in the nested text,
/// ignoring ASCII case.
fn assert_no_secret(frame: &str, secrets: &[String]) {
    let result = result_of(frame);
    let mut texts = vec![frame.to_owned(), text_of(&result)];
    strings(&result, &mut texts);
    for secret in secrets {
        let lower = secret.to_ascii_lowercase();
        let pieces: Vec<&str> = if lower.len() < 32 {
            vec![lower.as_str()]
        } else {
            (0..=lower.len() - 32)
                .map(|at| &lower[at..at + 32])
                .collect()
        };
        for text in &texts {
            let text = text.to_ascii_lowercase();
            for piece in &pieces {
                assert!(!text.contains(piece), "{piece} in {text}");
            }
        }
    }
}

/// The fixed refusal wrapper, in both channels.
fn assert_refused(frame: &str, status: i32) {
    assert_eq!(status, 0, "{frame}");
    let result = result_of(frame);
    let refusal = Json::object(vec![
        ("paid", Json::Bool(false)),
        ("reason", Json::string(FAILED_SAFELY)),
        ("refused", Json::Bool(true)),
    ]);
    assert!(same(&details_of(&result), &refusal), "{frame}");
    assert_eq!(
        text_of(&result),
        format!("{{\"paid\":false,\"reason\":\"{FAILED_SAFELY}\",\"refused\":true}}")
    );
    let Json::Object(members) = &result else {
        panic!("object")
    };
    assert_eq!(members.len(), 2, "{frame}");
}

/// A body hit: `[REDACTED]` spans in details.body, `redacted: true` last,
/// the text the compact rendering of the same details, no secret anywhere.
fn assert_redacted(frame: &str, status: i32, body: &str, secrets: &[String]) {
    assert_eq!(status, 0, "{frame}");
    let result = result_of(frame);
    let details = details_of(&result);
    assert_eq!(details.get("body").and_then(Json::as_str), Some(body));
    let Json::Object(members) = &details else {
        panic!("object")
    };
    assert_eq!(
        members.last(),
        Some(&("redacted".to_owned(), Json::Bool(true)))
    );
    let text = text_of(&result);
    assert_eq!(text, crate::aurora::pyfmt::dumps_compact(&details));
    assert!(same(&parse_json(&text).unwrap(), &details));
    assert_no_secret(frame, secrets);
}

fn paid(body: &str) -> Json {
    Json::object(vec![
        ("paid", Json::Bool(true)),
        ("refused", Json::Bool(false)),
        ("status_code", Json::Int(200)),
        ("content_type", Json::string("text/plain")),
        ("url", Json::string("https://merchant.test/pay/weather")),
        ("content_returned", Json::Bool(true)),
        ("body", Json::string(body)),
        ("truncated", Json::Bool(false)),
        ("untrusted", Json::Bool(true)),
    ])
}

fn withheld() -> Json {
    Json::object(vec![
        ("paid", Json::Bool(true)),
        ("refused", Json::Bool(false)),
        ("status_code", Json::Int(200)),
        ("content_type", Json::string("text/plain")),
        ("body_sha256", Json::string(sha256_hex(b"merchant body"))),
        ("body_bytes", Json::Int(13)),
        ("url", Json::string("https://merchant.test/pay/weather")),
        ("content_returned", Json::Bool(false)),
    ])
}

fn with(details: &Json, key: &str, value: Json) -> Json {
    let Json::Object(mut members) = details.clone() else {
        panic!("object")
    };
    match members.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value,
        None => members.push((key.to_owned(), value)),
    }
    Json::Object(members)
}

fn percent(text: &str) -> String {
    text.bytes().map(|b| format!("%{b:02X}")).collect()
}

#[test]
fn t_oc_final_shared_registry_fixtures_through_the_matcher() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-agents/skills/agents-pay/packages/openclaw/test/fixtures/bridge-contract/final-output/registry-cases.json");
    let fixture = parse_json(&std::fs::read_to_string(path).unwrap()).unwrap();
    let Some(Json::Array(cases)) = fixture.get("cases") else {
        panic!("cases")
    };
    assert_eq!(cases.len(), 9);
    for case in cases {
        let name = case.get("name").and_then(Json::as_str).unwrap();
        let request = case.get("request").unwrap();
        let expected = case.get("expected").unwrap();
        let (frame, status) = run(request);
        assert_eq!(status, 0, "{name}: {frame}");
        let result = result_of(&frame);
        assert!(
            same(&details_of(&result), &details_of(expected)),
            "{name}: {frame}"
        );
        assert_eq!(text_of(&result), text_of(expected), "{name}");
        let secrets: Vec<String> = match request.get("secrets") {
            Some(Json::Array(items)) => items
                .iter()
                .map(|s| s.as_str().unwrap().to_owned())
                .collect(),
            _ => panic!("secrets"),
        };
        assert_no_secret(&frame, &secrets);
        // A clean case is returned unchanged, text bytes included.
        if same(expected, request.get("candidate").unwrap()) {
            assert_eq!(
                text_of(&result),
                text_of(request.get("candidate").unwrap()),
                "{name}"
            );
        }
    }
}

#[test]
fn t_oc_final_body_exact_header_and_signature_echoes_are_redacted() {
    let secrets = registry();
    for (body, expected) in [
        (header(), "[REDACTED]".to_owned()),
        (signature(), "[REDACTED]".to_owned()),
        (
            format!("you sent {} and {}", header(), signature()),
            "you sent [REDACTED] and [REDACTED]".to_owned(),
        ),
        (
            format!("{0}\n{0}\n{0}", signature()),
            "[REDACTED]\n[REDACTED]\n[REDACTED]".to_owned(),
        ),
        (
            format!("ECHO {} END", signature().to_ascii_uppercase()),
            "ECHO [REDACTED] END".to_owned(),
        ),
    ] {
        let (frame, status) = run(&request(&paid(&body), &secrets));
        assert_redacted(&frame, status, &expected, &secrets);
    }
}

#[test]
fn t_oc_final_body_fragments_and_encodings_are_redacted() {
    let secrets = registry();
    let sig = signature();
    let head = header();
    let fragment = &sig[40..73];
    // Every tenth character JSON-escaped: no literal run reaches 32 bytes.
    let escaped: String = sig[2..42]
        .chars()
        .enumerate()
        .map(|(at, c)| {
            if at % 10 == 9 {
                format!("\\u{:04x}", u32::from(c))
            } else {
                c.to_string()
            }
        })
        .collect();
    for (label, body, expected) in [
        (
            "33-byte signature fragment",
            format!("a {fragment} b"),
            "a [REDACTED] b".to_owned(),
        ),
        (
            "header fragment",
            format!("x{}y", &head[50..90]),
            "x[REDACTED]y".to_owned(),
        ),
        ("json escape", escaped, "[REDACTED]".to_owned()),
        (
            "base64 fragment",
            format!("see {}", base64_standard(fragment.as_bytes())),
            "see [REDACTED]".to_owned(),
        ),
        (
            "base64 of the whole signature",
            base64_standard(sig.as_bytes()),
            "[REDACTED]".to_owned(),
        ),
        (
            "percent fragment",
            format!("q={}&r=1", percent(fragment)),
            "q=[REDACTED]&r=1".to_owned(),
        ),
    ] {
        let (frame, status) = run(&request(&paid(&body), &secrets));
        assert_eq!(status, 0, "{label}");
        let result = result_of(&frame);
        assert_eq!(
            details_of(&result).get("body").and_then(Json::as_str),
            Some(expected.as_str()),
            "{label}: {frame}"
        );
        assert_redacted(&frame, status, &expected, &secrets);
    }
}

#[test]
fn t_oc_final_body_withheld_or_clean_output_is_unchanged() {
    let secrets = registry();
    for details in [
        withheld(),
        paid("an ordinary paid body with no proof in it"),
        paid("caf\u{e9} \u{6f22}\u{5b57} \u{1f600} eyJ4NDAyVmVyc2lvbiI6Mi 0x"),
        paid(&signature()[..31]),
    ] {
        let text = pretty(&details);
        let (frame, status) = run(&request(&details, &secrets));
        assert_eq!(status, 0, "{frame}");
        let result = result_of(&frame);
        assert_eq!(text_of(&result), text, "the text bytes are kept");
        assert_eq!(details_of(&result), details, "details are kept");
        assert!(details_of(&result).get("redacted").is_none());
        assert_no_secret(&frame, &secrets);
    }
}

#[test]
fn t_oc_final_meta_echoes_outside_the_body_refuse_the_whole_result() {
    let secrets = registry();
    let sig = signature();
    let head = header();
    let cases = vec![
        (
            "content type, body withheld",
            with(
                &withheld(),
                "content_type",
                Json::string(format!("text/{head}")),
            ),
        ),
        (
            "content type, body returned",
            with(
                &paid("clean"),
                "content_type",
                Json::string(format!("text/{}", &sig[2..40])),
            ),
        ),
        (
            "url",
            with(
                &withheld(),
                "url",
                Json::string(format!("https://merchant.test/?p={}", percent(&sig[5..45]))),
            ),
        ),
        (
            "other metadata",
            with(&withheld(), "status_text", Json::string(sig.clone())),
        ),
        (
            "failure reason",
            Json::object(vec![
                ("paid", Json::Bool(false)),
                ("refused", Json::Bool(true)),
                ("reason", Json::string(format!("merchant said {head}"))),
            ]),
        ),
        (
            "replay 402 reason",
            Json::object(vec![
                ("paid", Json::Bool(false)),
                ("refused", Json::Bool(false)),
                ("status_code", Json::Int(402)),
                ("reason", Json::string(base64_standard(sig.as_bytes()))),
            ]),
        ),
        (
            "body hit plus metadata hit",
            with(&paid(&sig), "content_type", Json::string(sig.clone())),
        ),
        (
            "no proof exemption",
            with(
                &withheld(),
                "payload",
                Json::object(vec![("signature", Json::string(sig.clone()))]),
            ),
        ),
        ("a key", with(&withheld(), &sig, Json::Bool(true))),
        (
            "nested body",
            with(
                &withheld(),
                "extra",
                Json::object(vec![("body", Json::string(sig.clone()))]),
            ),
        ),
    ];
    for (label, details) in cases {
        let (frame, status) = run(&request(&details, &secrets));
        assert_refused(&frame, status);
        assert_no_secret(&frame, &secrets);
        assert!(!frame.contains("REDACTED"), "{label}");
    }
}

#[test]
fn t_oc_final_bound_registry_ambiguity_and_residual_hits_fail_closed() {
    let secrets = registry();
    // A registry entry shorter than 8 bytes cannot be matched: input error.
    let (frame, status) = run(&request(&paid("clean"), &["short".to_owned()]));
    assert_eq!((frame.as_str(), status), (INPUT, 2));
    let (frame, status) = run(&request(&paid("clean"), &[String::new()]));
    assert_eq!((frame.as_str(), status), (INPUT, 2));
    // An empty registry checks nothing and keeps a clean candidate.
    let details = paid("clean");
    let (frame, status) = run(&request(&details, &[]));
    assert_eq!(status, 0);
    assert_eq!(details_of(&result_of(&frame)), details);
    // Ambiguous: a body hit where the candidate already says `redacted`.
    let ambiguous = with(&paid(&signature()), "redacted", Json::Bool(false));
    let (frame, status) = run(&request(&ambiguous, &secrets));
    assert_refused(&frame, status);
    // A residual hit after rendering: the redacted rendering itself holds a
    // registry entry.
    let marker = vec![header(), "\"redacted\":true".to_owned()];
    let (frame, status) = run(&request(&paid(&header()), &marker));
    assert_refused(&frame, status);
    // A text that differs from details only in escapes is still checked:
    // the escaped text hit is the body hit.
    let details = paid(&signature());
    let text = pretty(&details).replace(&signature()[..2], "\\u0030x");
    let (frame, status) = run(&request_with_text(&details, &text, &secrets));
    assert_redacted(&frame, status, "[REDACTED]", &secrets);
}

#[test]
fn t_oc_final_bound_work_budget_exhaustion_fails_closed() {
    let secrets = registry();
    for details in [paid("clean"), paid(&signature()), withheld()] {
        for budget in [0, 10, 1000] {
            let (frame, status) = run_within(&request(&details, &secrets), Some(budget));
            assert_refused(&frame, status);
        }
        // The default budget is ample for a full request.
        let (frame, status) = run(&request(&details, &secrets));
        assert_eq!(status, 0, "{frame}");
        assert!(!frame.contains(FAILED_SAFELY));
    }
}

/// The complete encoded frame, LF included, is capped at 131072 bytes:
/// exactly the cap is returned whole, one byte more is the fixed failure.
#[test]
fn t_oc_final_bound_frame_cap_boundary() {
    let secrets = registry();
    let build = |accented: usize, ascii: usize, escaped_in_text: bool| {
        let body = format!("{}{}", "\u{e9}".repeat(accented), "a".repeat(ascii));
        let details = paid(&body);
        let mut text = pretty(&details);
        if escaped_in_text {
            text = text.replacen('\u{e9}', "\\u00e9", 1);
        }
        request_with_text(&details, &text, &secrets)
    };
    let measure = |escaped: bool| {
        let (frame, status) = run(&build(100, 0, escaped));
        assert_eq!(status, 0);
        frame.len()
    };
    let lengths = [measure(false), measure(true)];
    assert_eq!(lengths[1], lengths[0] + 1, "one escape adds one frame byte");
    for target in [131_072_usize, 131_073] {
        let escaped = (target - lengths[0]) % 2 == 1;
        let remaining = target - lengths[usize::from(escaped)];
        // Each accented character adds 12 frame bytes, each ASCII one 2.
        let request = build(100 + remaining / 12, (remaining % 12) / 2, escaped);
        assert!(request.to_compact_string().len() <= 131_072);
        let (frame, status) = run(&request);
        if target == 131_072 {
            assert_eq!((frame.len(), status), (131_072, 0));
            assert!(frame.ends_with("}}\n"));
        } else {
            assert_eq!((frame.as_str(), status), (FAILURE, 1));
        }
    }
    // Astral characters escape to surrogate pairs and overflow as a whole.
    let details = paid(&"\u{1f600}".repeat(6000));
    let (frame, status) = run(&request(&details, &secrets));
    assert_eq!((frame.as_str(), status), (FAILURE, 1));
}
