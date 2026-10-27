//! Offline, deterministic tests for `toolkit bedrock-resolve` (#4403).
//!
//! They port `test_model_identity.py`, `test_image_input.py`,
//! `test_bedrock_pricing.py` and `test_resolve_source_model.py` and add the
//! negative cases of the Rust contract: fail-closed resolution, recorded
//! inputs only (no network, no AWS), links refused for every owned read and
//! hostile caller text (ESC, BEL, CR, LF, U+202E, a fake secret marker) never
//! reaching a diagnostic or stdout raw. Fixtures live in unique temporary
//! directories removed on drop.

use std::ffi::OsString;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_core::{parse_json, Json};

use super::identity::{same_model, same_model_family};
use super::image;
use super::pricing::{self, Identity, PriceList, WellFormedId};
use super::pricing_table::STATIC_FALLBACK;
use super::source;
use crate::safe_text::is_unsafe;
use crate::COMMANDS;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-bedrock-resolve-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn file(&self, name: &str, content: &str) -> String {
        self.bytes(name, content.as_bytes())
    }

    fn bytes(&self, name: &str, content: &[u8]) -> String {
        let path = self.0.join(name);
        fs::write(&path, content).expect("write fixture");
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    fn link(&self, name: &str, target: &str) -> String {
        let path = self.0.join(name);
        std::os::unix::fs::symlink(target, &path).expect("create symlink");
        path.to_string_lossy().into_owned()
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn exec(args: &[&str]) -> (u8, String, String) {
    let os: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = crate::run(&os, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

fn json(text: &str) -> Json {
    parse_json(text).expect("valid JSON")
}

/// Text that tries every trick the diagnostic rule guards against.
const HOSTILE: &str = "x\u{1b}[31m\u{7}\r\nFORGED: line\u{202e}gnp.exe";
/// A fake credential; it must never be printed.
const SECRET: &str = "FAKE-SECRET-MARKER-9f3a7c1e";

/// No output line holds an unsafe character, and no line was forged.
fn assert_clean(label: &str, text: &str) {
    for line in text.lines() {
        assert!(
            !line.chars().any(is_unsafe),
            "{label} holds an unsafe character: {line:?}"
        );
        assert!(
            !line.starts_with("FORGED"),
            "{label} holds a forged line: {line:?}"
        );
    }
    assert!(!text.contains(SECRET), "{label} holds the secret: {text:?}");
}

// ---------------------------------------------------------------------------
// identity (test_model_identity.py)

#[test]
fn only_equal_resolved_identities_qualify() {
    let cases: [(&str, &str, &str, bool); 14] = [
        (
            "anthropic",
            "claude-opus-4-8",
            "global.anthropic.claude-opus-5-5",
            false,
        ),
        (
            "anthropic",
            "claude-opus-5",
            "us.anthropic.claude-opus-5-5",
            false,
        ),
        (
            "anthropic",
            "claude-opus-5-5",
            "global.anthropic.claude-opus-5-5",
            true,
        ),
        (
            "anthropic",
            "claude-haiku-4-5-20251001",
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            true,
        ),
        (
            "anthropic",
            "claude-haiku-4-5-20251001",
            "us.anthropic.claude-haiku-4-5-20251002-v1:0",
            false,
        ),
        (
            "anthropic",
            "claude-opus-5-5",
            "us.anthropic.claude-sonnet-5",
            false,
        ),
        (
            "anthropic",
            "claude-opus-latest",
            "anthropic.claude-opus-latest",
            false,
        ),
        (
            "anthropic",
            "claude-opus-4-*",
            "anthropic.claude-opus-5-5",
            false,
        ),
        (
            "anthropic",
            "claude-opus-5-5",
            "arn:aws:bedrock:us-east-1:123:application-inference-profile/test",
            false,
        ),
        ("openai", "gpt-5.6-sol", "openai.gpt-5.6-sol", true),
        ("openai", "gpt-5.6-sol", "us.openapi.gpt-5.6-sol", false),
        ("openai", "gpt-5.5", "openai.gpt-5.6-sol", false),
        ("openai", "gpt-oss-120b", "openai.gpt-oss-120b", false),
        (
            "custom",
            "claude-opus-5-5",
            "anthropic.claude-opus-5-5",
            false,
        ),
    ];
    for (provider, source, target, expected) in cases {
        assert_eq!(
            same_model(provider, source, target),
            expected,
            "{provider} {source} {target}"
        );
    }
}

#[test]
fn the_project_shortcut_requires_every_nonempty_mapping_to_match() {
    let same = json(
        r#"{"source_model": "claude-opus-5-5", "aws_model_id": "global.anthropic.claude-opus-5-5"}"#,
    );
    let changed = json(
        r#"{"source_model": "claude-opus-4-8", "aws_model_id": "global.anthropic.claude-opus-5-5"}"#,
    );
    assert!(same_model_family("anthropic", std::slice::from_ref(&same)));
    assert!(!same_model_family("anthropic", &[same.clone(), changed]));
    assert!(!same_model_family("anthropic", &[]));
    assert!(!same_model_family("anthropic", &[json("{}")]));
    // A null, a non-text value or a row that is not an object is never the same
    // model (the Python function raised on a row that was not a dict).
    for row in [
        r#"{"source_model": null, "aws_model_id": "anthropic.claude-opus-5-5"}"#,
        r#"{"source_model": 5, "aws_model_id": "anthropic.claude-opus-5-5"}"#,
        r"[1, 2]",
        r#""claude-opus-5-5""#,
    ] {
        assert!(!same_model_family("anthropic", &[json(row)]), "{row}");
    }
    assert!(!same_model_family("custom", &[same]));
}

#[test]
fn the_identity_command_prints_the_scripts_json() {
    let (status, out, err) = exec(&[
        "bedrock-resolve",
        "identity",
        "--provider",
        "anthropic",
        "--source",
        "claude-opus-4-8",
        "--target",
        "global.anthropic.claude-opus-5-5",
    ]);
    assert_eq!(
        (status, out.as_str(), err.as_str()),
        (0, "{\"same_model\": false}\n", "")
    );
    let (status, out, _) = exec(&[
        "bedrock-resolve",
        "identity",
        "--provider=anthropic",
        "--source=claude-opus-5-5",
        "--target=global.anthropic.claude-opus-5-5",
    ]);
    assert_eq!((status, out.as_str()), (0, "{\"same_model\": true}\n"));
}

#[test]
fn the_identity_command_checks_a_mappings_file() {
    let dir = TempDir::new("mappings");
    let file = dir.file(
        "m.json",
        r#"[{"source_model": "gpt-5.5", "aws_model_id": "global.openai.gpt-5.5-v2"}]"#,
    );
    let (status, out, _) = exec(&[
        "bedrock-resolve",
        "identity",
        "--provider",
        "openai",
        "--mappings",
        &file,
    ]);
    assert_eq!(
        (status, out.as_str()),
        (0, "{\"same_model_family\": true}\n")
    );
    let empty = dir.file("e.json", "[]");
    let (_, out, _) = exec(&[
        "bedrock-resolve",
        "identity",
        "--provider",
        "openai",
        "--mappings",
        &empty,
    ]);
    assert_eq!(out, "{\"same_model_family\": false}\n");
    // Not a list: an error, never a verdict.
    let object = dir.file("o.json", "{}");
    let (status, out, err) = exec(&[
        "bedrock-resolve",
        "identity",
        "--provider",
        "openai",
        "--mappings",
        &object,
    ]);
    assert_eq!((status, out.as_str()), (1, ""));
    assert!(err.contains("JSON array"), "{err}");
}

#[test]
fn identity_arguments_follow_the_script_usage() {
    for args in [
        &["bedrock-resolve", "identity"][..],
        &["bedrock-resolve", "identity", "--provider", "openai"],
        &[
            "bedrock-resolve",
            "identity",
            "--provider",
            "openai",
            "--source",
            "a",
        ],
        &[
            "bedrock-resolve",
            "identity",
            "--source",
            "a",
            "--target",
            "b",
        ],
        &[
            "bedrock-resolve",
            "identity",
            "--provider",
            "openai",
            "--mappings",
            "m",
            "--source",
            "a",
        ],
        &[
            "bedrock-resolve",
            "identity",
            "--provider",
            "openai",
            "--source",
            "--target",
        ],
        &[
            "bedrock-resolve",
            "identity",
            "--provider",
            "openai",
            "--source",
            "a",
            "--target",
            "b",
            "extra",
        ],
    ] {
        let (status, out, err) = exec(args);
        assert_eq!((status, out.as_str()), (2, ""), "{args:?}");
        assert!(err.starts_with("error: "), "{args:?}: {err}");
    }
}

// ---------------------------------------------------------------------------
// image (test_image_input.py)

const RAW: &[u8] = b"\xff\xd8\xff\xe0fake-jpeg-bytes";

#[test]
fn the_jpg_extension_normalizes_to_jpeg() {
    assert_eq!(image::converse_format("cat.jpg"), Ok("jpeg"));
    assert_eq!(image::converse_format("cat.jpeg"), Ok("jpeg"));
    assert_eq!(image::mime_type("cat.jpg").as_deref(), Ok("image/jpeg"));
    assert!(!image::data_url("cat.jpg", RAW)
        .unwrap()
        .contains("image/jpg;"));
}

#[test]
fn the_extension_case_is_ignored() {
    assert_eq!(image::converse_format("CAT.JPG"), Ok("jpeg"));
    assert_eq!(image::mime_type("shot.PNG").as_deref(), Ok("image/png"));
    assert_eq!(image::converse_format("dir.d/a.b.WebP"), Ok("webp"));
}

#[test]
fn the_other_supported_formats() {
    for (name, format) in [("a.png", "png"), ("a.gif", "gif"), ("a.webp", "webp")] {
        assert_eq!(image::converse_format(name), Ok(format));
        assert_eq!(image::mime_type(name), Ok(format!("image/{format}")));
    }
}

#[test]
fn unsupported_types_are_refused_rather_than_guessed() {
    for name in [
        "a.bmp", "a.tiff", "a.svg", "a.heic", "noext", ".jpg", "a.", "x/.", "",
    ] {
        let error = image::converse_format(name).unwrap_err();
        assert!(
            error.starts_with("unsupported image type"),
            "{name}: {error}"
        );
    }
    // pathlib semantics: a dot file has no suffix, a directory suffix is not
    // the file's.
    assert!(image::converse_format("dir.jpg/file").is_err());
    assert_eq!(image::converse_format("./a.png"), Ok("png"));
    assert_eq!(image::converse_format("a.png/."), Ok("png"));
}

#[test]
fn base64_matches_the_standard_vectors() {
    for (raw, expected) in [
        (&b""[..], ""),
        (b"f", "Zg=="),
        (b"fo", "Zm8="),
        (b"foo", "Zm9v"),
        (b"foob", "Zm9vYg=="),
        (b"fooba", "Zm9vYmE="),
        (b"foobar", "Zm9vYmFy"),
        (b"\xff\xfe\xfd", "//79"),
    ] {
        assert_eq!(image::base64_encode(raw), expected);
    }
}

fn texts(value: &Json, key: &str) -> Vec<String> {
    let Some(Json::Array(items)) = value.get(key) else {
        panic!("no {key} list in {value:?}");
    };
    items.iter().map(Json::to_compact_string).collect()
}

#[test]
fn the_responses_message_is_wrapped_as_a_user_message() {
    let message = image::responses_message("describe", Some("cat.jpg"), Some(RAW), "auto").unwrap();
    assert_eq!(message.get("role").and_then(Json::as_str), Some("user"));
    let Some(Json::Array(content)) = message.get("content") else {
        panic!("content is a list");
    };
    let kinds: Vec<_> = content
        .iter()
        .filter_map(|block| block.get("type").and_then(Json::as_str))
        .collect();
    assert_eq!(kinds, ["input_text", "input_image"]);
    let url = content[1].get("image_url").and_then(Json::as_str).unwrap();
    assert!(url.starts_with("data:image/jpeg;base64,"));
    assert!(url.ends_with(&image::base64_encode(RAW)));
    assert_eq!(
        content[1].get("detail").and_then(Json::as_str),
        Some("auto")
    );
}

#[test]
fn the_text_only_responses_message_omits_the_image_block_and_detail() {
    let message = image::responses_message("describe", None, None, "auto").unwrap();
    assert_eq!(
        message.to_compact_string(),
        r#"{"role":"user","content":[{"type":"input_text","text":"describe"}]}"#
    );
}

#[test]
fn the_converse_message_puts_the_image_before_the_text() {
    let message = image::converse_message("describe", Some("cat.jpg"), Some(RAW)).unwrap();
    let content = texts(&message, "content");
    assert_eq!(
        content[0],
        format!(
            r#"{{"image":{{"format":"jpeg","source":{{"bytes":{{"base64":"{}"}}}}}}}}"#,
            image::base64_encode(RAW)
        )
    );
    assert_eq!(content[1], r#"{"text":"describe"}"#);
    assert_eq!(
        image::converse_message("describe", None, None)
            .unwrap()
            .to_compact_string(),
        r#"{"role":"user","content":[{"text":"describe"}]}"#
    );
}

#[test]
fn the_chat_message_keeps_the_mime_normalization() {
    let message = image::chat_message("d", Some("a.JPG"), Some(RAW)).unwrap();
    let content = texts(&message, "content");
    assert!(
        content[1].contains("data:image/jpeg;base64,"),
        "{}",
        content[1]
    );
    assert_eq!(content[0], r#"{"type":"text","text":"d"}"#);
}

#[test]
fn an_image_path_without_bytes_is_an_error() {
    let expected = "raw image bytes are required when image_path is given";
    assert_eq!(
        image::converse_message("d", Some("cat.jpg"), None).unwrap_err(),
        expected
    );
    assert_eq!(
        image::responses_message("d", Some("cat.jpg"), None, "auto").unwrap_err(),
        expected
    );
    assert_eq!(
        image::chat_message("d", Some("cat.jpg"), None).unwrap_err(),
        expected
    );
    // The bytes are checked before the extension, like the script.
    assert_eq!(
        image::converse_message("d", Some("cat.bmp"), None).unwrap_err(),
        expected
    );
}

#[test]
fn every_sdk_detail_level_is_accepted_and_others_are_refused() {
    for detail in ["low", "high", "auto", "original"] {
        let message = image::responses_message("d", Some("cat.png"), Some(RAW), detail).unwrap();
        assert!(message
            .to_compact_string()
            .contains(&format!(r#""detail":"{detail}""#)));
    }
    let error = image::responses_message("d", Some("cat.png"), Some(RAW), "ultra").unwrap_err();
    assert!(error.starts_with("invalid image detail"), "{error}");
    // Checked before the bytes.
    assert!(
        image::responses_message("d", Some("cat.png"), None, "ultra")
            .unwrap_err()
            .starts_with("invalid image detail")
    );
    assert_eq!(image::DETAIL_LEVELS, ["low", "high", "auto", "original"]);
}

#[test]
fn the_image_command_builds_every_payload_from_a_file() {
    let dir = TempDir::new("image");
    let file = dir.bytes("cat.jpg", RAW);
    let b64 = image::base64_encode(RAW);

    let (status, out, err) = exec(&[
        "bedrock-resolve",
        "image",
        "--emit",
        "format",
        "--image",
        &file,
    ]);
    assert_eq!(
        (status, out.as_str(), err.as_str()),
        (0, "{\n  \"format\": \"jpeg\"\n}\n", "")
    );
    let (_, out, _) = exec(&[
        "bedrock-resolve",
        "image",
        "--emit",
        "mime",
        "--image",
        "x.PNG",
    ]);
    assert_eq!(out, "{\n  \"mime_type\": \"image/png\"\n}\n");
    let (_, out, _) = exec(&[
        "bedrock-resolve",
        "image",
        "--emit",
        "data-url",
        "--image",
        &file,
    ]);
    assert_eq!(
        out,
        format!("{{\n  \"data_url\": \"data:image/jpeg;base64,{b64}\"\n}}\n")
    );

    let (status, out, _) = exec(&[
        "bedrock-resolve",
        "image",
        "--emit",
        "responses",
        "--image",
        &file,
        "--prompt",
        "describe",
        "--detail",
        "high",
    ]);
    assert_eq!(status, 0);
    let message = json(&out);
    assert_eq!(
        texts(&message, "content")[1],
        format!(
            r#"{{"type":"input_image","image_url":"data:image/jpeg;base64,{b64}","detail":"high"}}"#
        )
    );
    let (_, out, _) = exec(&[
        "bedrock-resolve",
        "image",
        "--emit",
        "converse",
        "--prompt",
        "hi",
    ]);
    assert_eq!(
        json(&out).to_compact_string(),
        r#"{"role":"user","content":[{"text":"hi"}]}"#
    );
    let prompt = dir.file("prompt.txt", "-starts with a dash\nsecond line");
    let (_, out, _) = exec(&[
        "bedrock-resolve",
        "image",
        "--emit",
        "chat",
        "--prompt-file",
        &prompt,
    ]);
    assert!(
        out.contains(r#""text": "-starts with a dash\nsecond line""#),
        "{out}"
    );
}

#[test]
fn the_image_command_fails_closed() {
    let dir = TempDir::new("image-fail");
    let bmp = dir.bytes("a.bmp", b"BM");
    let missing = dir.0.join("missing.png").to_string_lossy().into_owned();
    let jpg = dir.bytes("a.jpg", RAW);

    // Unsupported extension: an error, before the file is read.
    let (status, out, err) = exec(&[
        "bedrock-resolve",
        "image",
        "--emit",
        "data-url",
        "--image",
        &bmp,
    ]);
    assert_eq!((status, out.as_str()), (1, ""));
    assert!(err.contains("unsupported image type 'bmp'"), "{err}");
    let (status, _, err) = exec(&[
        "bedrock-resolve",
        "image",
        "--emit",
        "mime",
        "--image",
        "noext",
    ]);
    assert_eq!(status, 1);
    assert!(err.contains("unsupported image type ''"), "{err}");
    // A missing file.
    let (status, out, err) = exec(&[
        "bedrock-resolve",
        "image",
        "--emit",
        "chat",
        "--prompt",
        "x",
        "--image",
        &missing,
    ]);
    assert_eq!((status, out.as_str()), (1, ""));
    assert!(
        err.contains("cannot read") && err.contains("does not exist"),
        "{err}"
    );
    // Bad detail.
    let (status, _, err) = exec(&[
        "bedrock-resolve",
        "image",
        "--emit",
        "responses",
        "--prompt",
        "x",
        "--image",
        &jpg,
        "--detail",
        "ultra",
    ]);
    assert_eq!(status, 1);
    assert!(err.contains("invalid image detail 'ultra'"), "{err}");
    // Usage errors.
    for args in [
        &["bedrock-resolve", "image"][..],
        &["bedrock-resolve", "image", "--emit", "bogus"],
        &["bedrock-resolve", "image", "--emit", "mime"],
        &["bedrock-resolve", "image", "--emit", "converse"],
        &[
            "bedrock-resolve",
            "image",
            "--emit",
            "chat",
            "--prompt",
            "a",
            "--prompt-file",
            "b",
        ],
        &[
            "bedrock-resolve",
            "image",
            "--emit",
            "chat",
            "--prompt",
            "-dash",
        ],
        &[
            "bedrock-resolve",
            "image",
            "--emit",
            "mime",
            "--image",
            "a.png",
            "--prompt",
            "x",
        ],
        &[
            "bedrock-resolve",
            "image",
            "--emit",
            "chat",
            "--prompt",
            "x",
            "--detail",
            "low",
        ],
    ] {
        let (status, out, _) = exec(args);
        assert_eq!((status, out.as_str()), (2, ""), "{args:?}");
    }
}

#[test]
fn an_oversized_image_is_refused() {
    let dir = TempDir::new("image-big");
    let path = dir.0.join("big.png");
    let file = fs::File::create(&path).expect("create");
    file.set_len(super::MAX_IMAGE_BYTES + 1)
        .expect("sparse file");
    let (status, out, err) = exec(&[
        "bedrock-resolve",
        "image",
        "--emit",
        "data-url",
        "--image",
        &path.to_string_lossy(),
    ]);
    assert_eq!((status, out.as_str()), (1, ""));
    assert!(err.contains("larger than"), "{err}");
}

// ---------------------------------------------------------------------------
// pricing (test_bedrock_pricing.py)

fn rates(price: &pricing::Price) -> (Option<f64>, Option<f64>) {
    (price.input_per_1k_usd, price.output_per_1k_usd)
}

fn lookup(region: &str, id: &str) -> pricing::Price {
    pricing::lookup(region, id, None)
}

#[test]
fn astra_rates_resolve_from_the_static_table() {
    for (id, input, output) in [
        ("openai.gpt-6-astra", 0.011, 0.055),
        ("us.openai.gpt-6-astra", 0.011, 0.055),
        ("global.openai.gpt-6-astra", 0.010, 0.050),
    ] {
        let price = lookup("us-west-2", id);
        assert!(price.available, "{id}");
        assert_eq!(rates(&price), (Some(input), Some(output)), "{id}");
        assert!(price.note.contains("<=272K"), "{}", price.note);
        assert!(
            price.note.contains("2x input / 1.5x output"),
            "{}",
            price.note
        );
        assert!(
            price.note.contains("static pricing table"),
            "{}",
            price.note
        );
    }
}

#[test]
fn unknown_astra_ids_never_inherit_another_rate() {
    for id in [
        "openai.gpt-6",
        "openai.gpt-6-astra-pro",
        "openai.gpt-6-astra-v1:0",
        "us.openai.gpt-6",
        "global.openai.gpt-6-astra-v1:0",
        "in.openai.gpt-6-astra",
        "eu.openai.gpt-6-astra",
    ] {
        assert_eq!(pricing::static_fallback(id), None, "{id}");
        let price = lookup("us-west-2", id);
        assert!(!price.available, "{id}");
        assert_eq!(rates(&price), (None, None), "{id}");
    }
}

fn price_item(entries: &[(&str, &str, &str)]) -> Json {
    let dimensions = entries
        .iter()
        .map(|(name, usd, description)| {
            (
                (*name).to_string(),
                json(&format!(
                    r#"{{"unit": "1K tokens", "pricePerUnit": {{"USD": "{usd}"}}, "description": "{description}"}}"#
                )),
            )
        })
        .collect();
    Json::object(vec![(
        "terms",
        Json::object(vec![(
            "OnDemand",
            Json::object(vec![(
                "x",
                Json::object(vec![("priceDimensions", Json::Object(dimensions))]),
            )]),
        )]),
    )])
}

#[test]
fn price_dimensions_are_the_base_input_and_output_rates() {
    let item = price_item(&[
        ("d1", "0.003", "Input tokens for Claude"),
        ("d2", "0.015", "Output tokens for Claude"),
    ]);
    assert_eq!(
        pricing::parse_price_dimensions(&item),
        Ok((Some(0.003), Some(0.015)))
    );
    // Cache read/write dimensions never override the base rates.
    let cached = price_item(&[
        ("d1", "0.003", "Input tokens for Claude"),
        ("d2", "0.015", "Output tokens for Claude"),
        ("d3", "0.00030", "Cache read input tokens for Claude"),
        ("d4", "0.00375", "Cache write input tokens for Claude"),
    ]);
    assert_eq!(
        pricing::parse_price_dimensions(&cached),
        Ok((Some(0.003), Some(0.015)))
    );
    // Batch dimensions are skipped.
    let batch = price_item(&[
        ("d1", "0.001", "Input token price for batch inference"),
        ("d2", "0.003", "Input tokens for Claude"),
        ("d3", "0.015", "Output tokens for Claude"),
    ]);
    assert_eq!(
        pricing::parse_price_dimensions(&batch),
        Ok((Some(0.003), Some(0.015)))
    );
    // The parity probe of check-bedrock-pricing-parity.py.
    let probe = price_item(&[
        ("input", "0.003", "Input tokens"),
        ("output", "0.015", "Output tokens"),
        ("cache", "0.0003", "Cache read input tokens"),
    ]);
    assert_eq!(
        pricing::parse_price_dimensions(&probe),
        Ok((Some(0.003), Some(0.015)))
    );
    // Nothing matching: no rates; a bare item: no rates.
    assert_eq!(
        pricing::parse_price_dimensions(&json("{}")),
        Ok((None, None))
    );
    assert_eq!(
        pricing::parse_price_dimensions(&price_item(&[("d", "1", "Something else")])),
        Ok((None, None))
    );
}

#[test]
fn malformed_price_items_are_errors_that_repeat_no_value() {
    for item in [
        r"[]",
        r#"{"terms": null}"#,
        r#"{"terms": {"OnDemand": []}}"#,
        r#"{"terms": {"OnDemand": {"x": {"priceDimensions": []}}}}"#,
        r#"{"terms": {"OnDemand": {"x": {"priceDimensions": {"d": 1}}}}}"#,
        r#"{"terms": {"OnDemand": {"x": {"priceDimensions": {"d": {"pricePerUnit": {"USD": "not-a-number-FORGED"}}}}}}}"#,
        r#"{"terms": {"OnDemand": {"x": {"priceDimensions": {"d": {"description": 5}}}}}}"#,
    ] {
        let error = pricing::parse_price_dimensions(&json(item)).unwrap_err();
        assert!(!error.contains("FORGED"), "{error}");
    }
    // An empty USD string and a missing rate are zero, like `or 0`.
    assert_eq!(
        pricing::parse_price_dimensions(&json(
            r#"{"terms": {"OnDemand": {"x": {"priceDimensions": {"d": {"pricePerUnit": {"USD": ""}, "description": "input tokens"}}}}}}"#
        )),
        Ok((Some(0.0), None))
    );
}

#[test]
fn unavailable_is_a_banner_not_an_exception() {
    let price = pricing::unavailable("network error");
    assert!(!price.available);
    assert_eq!(price.note, "Pricing unavailable: network error");
    assert_eq!(
        price.to_json().to_compact_string(),
        r#"{"available":false,"input_per_1k_usd":null,"output_per_1k_usd":null,"note":"Pricing unavailable: network error"}"#
    );
}

#[test]
fn the_static_fallback_returns_known_models() {
    // The Geo inference-profile id carries the 10% premium over the bare rate.
    let geo = pricing::static_fallback("us.anthropic.claude-haiku-4-5-20251001-v1:0").unwrap();
    assert!(geo.available);
    assert_eq!(rates(&geo), (Some(0.0011), Some(0.0055)));
    let sonnet = pricing::static_fallback("us.anthropic.claude-sonnet-5").unwrap();
    assert_eq!(rates(&sonnet), (Some(0.0022), Some(0.011)));
    let bare = pricing::static_fallback("anthropic.claude-sonnet-5").unwrap();
    assert_eq!(rates(&bare), (Some(0.002), Some(0.010)));
    let sonnet46 = pricing::static_fallback("us.anthropic.claude-sonnet-4-6").unwrap();
    assert_eq!(rates(&sonnet46), (Some(0.0033), Some(0.0165)));
    let opus = pricing::static_fallback("us.anthropic.claude-opus-4-8").unwrap();
    assert_eq!(rates(&opus), (Some(0.0055), Some(0.0275)));
    assert_eq!(pricing::static_fallback("totally.fake.model-id"), None);
    assert_eq!(
        geo.note, "static fallback (PriceList API had no entry)",
        "an exact hit keeps the table note"
    );
}

#[test]
fn opus_4_8_is_priced_in_every_id_shape() {
    // Dated forms are built by concatenation: a stale plan can carry a
    // fabricated dated form and the lookup must repair it.
    let dated = "-20250610-v1:0";
    for (id, expected) in [
        ("anthropic.claude-opus-4-8".to_string(), (0.005, 0.025)),
        ("us.anthropic.claude-opus-4-8".to_string(), (0.0055, 0.0275)),
        (format!("anthropic.claude-opus-4-8{dated}"), (0.005, 0.025)),
        (
            format!("us.anthropic.claude-opus-4-8{dated}"),
            (0.0055, 0.0275),
        ),
    ] {
        let price = lookup("us-east-1", &id);
        assert!(price.available, "{id}");
        assert_eq!(rates(&price), (Some(expected.0), Some(expected.1)), "{id}");
    }
    let dated_sonnet =
        pricing::static_fallback("us.anthropic.claude-sonnet-4-6-20250514-v1:0").unwrap();
    assert_eq!(dated_sonnet.input_per_1k_usd, Some(0.0033));
    assert_eq!(
        dated_sonnet.note,
        "static fallback (matched us.anthropic.claude-sonnet-4-6)"
    );
    // The separator guard keeps ...opus-4-85 from matching the ...opus-4-8 family.
    assert_eq!(
        pricing::static_fallback("anthropic.claude-opus-4-85-v1:0").map(|price| price.note),
        None
    );
}

#[test]
fn truncated_empty_or_partial_ids_are_never_priced() {
    // #4684: the old forward `starts_with` rule priced these as the first
    // table row (Haiku 4.5). They fail closed: unavailable, no rate.
    for id in [
        "anthropic",
        "anthropic.claude",
        "anthropic.claude-sonnet",
        "anthropic.claude-haiku-4",
        "us.anthropic.claude",
        "openai.gpt-",
        "openai",
        "amazon",
        "amazon.nova",
        "us.",
        "-v1",
        "-v1:0",
        "",
        "-",
        // A separator boundary alone is not enough.
        "anthropic.claude-opus-4-8-foo",
        "anthropic.claude-opus-4-8-",
        "us.anthropic.claude-sonnet-5-preview",
        "anthropic.claude-opus-4-85-v1:0",
        "anthropic.claude-opus-4-8-v",
        "anthropic.claude-opus-4-8-vx1",
        "anthropic.claude-opus-4-8-v1:",
        "anthropic.claude-opus-4-8-2025061",
        "anthropic.claude-opus-4-8-20250610-extra",
    ] {
        assert_eq!(pricing::static_fallback(id).map(|p| p.note), None, "{id:?}");
        let price = lookup("us-east-1", id);
        assert!(!price.available, "{id:?}");
        assert_eq!(rates(&price), (None, None), "{id:?}");
        assert!(price.note.starts_with("Pricing unavailable"), "{id:?}");
    }
}

#[test]
fn a_base_shared_by_several_rows_is_ambiguous() {
    // Synthetic table: both rows normalize to `x.model`. The base alone, and
    // either dated spelling, must not pick one of them.
    let table: [(&str, f64, f64); 3] = [
        ("x.model-20250101-v1:0", 1.0, 2.0),
        ("x.model-20260101-v1:0", 3.0, 4.0),
        ("x.other-v1:0", 5.0, 6.0),
    ];
    for id in [
        "x.model",
        "x.model-20250101",
        "x.model-20250101-v2:0",
        "x.model-20990101",
        "x.model-v2:0",
    ] {
        assert!(pricing::unique_normalized_row(&table, id).is_none(), "{id}");
        assert_eq!(pricing::classify(&table, id), Identity::Rejected, "{id}");
    }
    // An exact key names one row even when its normal form is shared.
    assert_eq!(
        pricing::unique_normalized_row(&table, "x.model-20250101-v1:0").map(|row| row.1),
        Some(1.0)
    );
    assert_eq!(
        pricing::unique_normalized_row(&table, "x.other-20260301-v1:0").map(|row| row.0),
        Some("x.other-v1:0")
    );
    // The real table keeps one row per normal form, so nothing in it is
    // ambiguous (a new row that breaks this makes its family unpriced).
    for (key, _, _) in STATIC_FALLBACK {
        assert!(
            pricing::unique_normalized_row(STATIC_FALLBACK, key).is_some(),
            "{key}"
        );
    }
}

#[test]
fn every_supported_normalized_id_still_prices_exactly_one_row() {
    // The documented normalization: a date stamp and/or `-v<N>[:<M>]`
    // revision, on ids that resolve to exactly one recognized row.
    for (id, key, input) in [
        (
            "anthropic.claude-haiku-4-5",
            "anthropic.claude-haiku-4-5-20251001-v1:0",
            0.001,
        ),
        (
            "us.anthropic.claude-haiku-4-5-20251001",
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            0.0011,
        ),
        (
            "eu.anthropic.claude-haiku-4-5-20260101-v2:0",
            "eu.anthropic.claude-haiku-4-5-20251001-v1:0",
            0.0011,
        ),
        (
            "global.anthropic.claude-haiku-4-5-v1:0",
            "global.anthropic.claude-haiku-4-5-20251001-v1:0",
            0.001,
        ),
        (
            "anthropic.claude-sonnet-5-v1:0",
            "anthropic.claude-sonnet-5",
            0.002,
        ),
        (
            "us.anthropic.claude-sonnet-5-20260101-v1:0",
            "us.anthropic.claude-sonnet-5",
            0.0022,
        ),
        (
            "us.anthropic.claude-sonnet-4-6-20250514-v1:0",
            "us.anthropic.claude-sonnet-4-6",
            0.0033,
        ),
        (
            "global.anthropic.claude-opus-4-8-20250610",
            "global.anthropic.claude-opus-4-8",
            0.005,
        ),
        ("amazon.nova-pro", "amazon.nova-pro-v1:0", 0.0008),
        ("amazon.nova-lite-v2:0", "amazon.nova-lite-v1:0", 0.00006),
    ] {
        let price = pricing::static_fallback(id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!(price.input_per_1k_usd, Some(input), "{id}");
        assert_eq!(
            price.note,
            format!("static fallback (matched {key})"),
            "{id}"
        );
    }
}

#[test]
fn global_and_eu_anthropic_prefixes_price_their_inference_option() {
    for (id, input, output) in [
        ("global.anthropic.claude-sonnet-4-6", 0.003, 0.015),
        ("eu.anthropic.claude-sonnet-4-6", 0.0033, 0.0165),
        (
            "global.anthropic.claude-haiku-4-5-20251001-v1:0",
            0.001,
            0.005,
        ),
        (
            "eu.anthropic.claude-haiku-4-5-20251001-v1:0",
            0.0011,
            0.0055,
        ),
        ("global.anthropic.claude-opus-4-8", 0.005, 0.025),
        ("eu.anthropic.claude-opus-4-8", 0.0055, 0.0275),
        ("global.anthropic.claude-sonnet-5", 0.002, 0.010),
        ("eu.anthropic.claude-sonnet-5", 0.0022, 0.011),
    ] {
        let price = pricing::static_fallback(id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!(rates(&price), (Some(input), Some(output)), "{id}");
    }
    let price = lookup("eu-west-1", "eu.anthropic.claude-sonnet-4-6");
    assert_eq!(rates(&price), (Some(0.0033), Some(0.0165)));
}

#[test]
fn display_names_are_derived_from_model_ids() {
    for (id, name) in [
        (
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "Claude Haiku 4.5",
        ),
        ("amazon.nova-lite-v1:0", "Nova Lite"),
        ("anthropic.claude-sonnet-5", "Claude Sonnet 5"),
        ("anthropic.claude-3-haiku-20240307-v1:0", "Claude 3 Haiku"),
        ("apac.amazon.nova-pro-v1:0", "Nova Pro"),
    ] {
        assert_eq!(
            pricing::display_name_guess(WellFormedId::parse(id).unwrap()),
            name,
            "{id}"
        );
    }
}

#[test]
fn table_models_are_served_without_a_price_list() {
    let price = lookup("us-east-1", "amazon.nova-pro-v1:0");
    assert!(price.available);
    assert_eq!(price.input_per_1k_usd, Some(0.0008));
    assert!(price
        .note
        .starts_with("static pricing table (verified 2026-10-05"));
}

#[test]
fn proprietary_gpt_detection_excludes_gpt_oss() {
    for id in [
        "openai.gpt-5.6-luna",
        "openai.gpt-5.5",
        "openai.gpt-6-astra",
        "openai.gpt-6-sol",
        "openai.gpt-6-luna",
        "openai.gpt-6.1-sol",
    ] {
        assert!(pricing::is_mantle_gpt(id), "{id}");
    }
    for id in ["openai.gpt-oss-120b-1:0", "anthropic.claude-sonnet-4-6"] {
        assert!(!pricing::is_mantle_gpt(id), "{id}");
    }
}

fn near(a: Option<f64>, b: f64) -> bool {
    a.is_some_and(|a| (a - b).abs() < 1e-9)
}

#[test]
fn gpt_rates_follow_the_inference_option() {
    // Bare mantle ids and Geo CRIS (us., in.) are 1.10x OpenAI standard (the
    // data-residency tier); Global CRIS is exactly the standard price.
    let standard: [(&str, f64, f64); 9] = [
        ("gpt-5.6-sol", 0.004, 0.020),
        ("gpt-5.6-terra", 0.002, 0.012),
        ("gpt-5.6-luna", 0.0002, 0.0012),
        ("gpt-5.5", 0.005, 0.030),
        ("gpt-5.4", 0.0025, 0.015),
        ("gpt-6-astra", 0.010, 0.050),
        ("gpt-6-sol", 0.0020, 0.010),
        ("gpt-6-luna", 0.00010, 0.00050),
        ("gpt-6.1-sol", 0.0020, 0.010),
    ];
    let mut seen = 0;
    for (id, input, output) in STATIC_FALLBACK {
        let Some((_, base)) = id.split_once("openai.") else {
            continue;
        };
        let (_, standard_in, standard_out) = standard
            .iter()
            .find(|(name, _, _)| *name == base)
            .unwrap_or_else(|| panic!("{id}"));
        let factor = if id.starts_with("global.") { 1.0 } else { 1.10 };
        assert!(near(Some(*input), standard_in * factor), "{id}");
        assert!(near(Some(*output), standard_out * factor), "{id}");
        seen += 1;
    }
    assert_eq!(seen, 25);
}

#[test]
fn cris_forms_never_partial_match() {
    for probe in [
        "us.openai.gpt-5.6",
        "global.openai.gpt-5.6",
        "in.openai.gpt-5.6-sol",
        "in.openai.gpt-6-sol",
        "in.openai.gpt-6.1-sol",
    ] {
        assert_eq!(pricing::static_fallback(probe), None, "{probe}");
    }
    let price = lookup("us-east-1", "global.openai.gpt-5.6-luna");
    assert_eq!(rates(&price), (Some(0.0002), Some(0.0012)));
}

#[test]
fn every_proprietary_gpt_model_is_priced() {
    for id in [
        "openai.gpt-5.6-sol",
        "openai.gpt-5.6-terra",
        "openai.gpt-5.6-luna",
        "openai.gpt-5.5",
        "openai.gpt-5.4",
        "openai.gpt-6-astra",
        "openai.gpt-6-sol",
        "openai.gpt-6-luna",
        "openai.gpt-6.1-sol",
    ] {
        let price = lookup("us-east-1", id);
        assert!(price.available, "{id}");
        assert!(
            price.input_per_1k_usd > Some(0.0) && price.output_per_1k_usd > Some(0.0),
            "{id}"
        );
    }
    assert_eq!(
        lookup("us-east-1", "openai.gpt-5.6-luna").input_per_1k_usd,
        Some(0.00022)
    );
}

#[test]
fn an_unpriced_gpt_says_unavailable_not_nonexistent() {
    for id in [
        "openai.gpt-5.6",
        "openai.gpt-5.6-nova-pro-preview",
        "eu.openai.gpt-5.6-sol",
    ] {
        let price = lookup("us-east-1", id);
        assert!(!price.available, "{id}");
        assert_eq!(rates(&price), (None, None));
        assert!(
            price
                .note
                .contains("does NOT mean the model is unavailable"),
            "{}",
            price.note
        );
        assert!(
            price.note.contains("aws.amazon.com/bedrock/pricing"),
            "{}",
            price.note
        );
    }
}

#[test]
fn opus_5_5_pricing_is_region_and_profile_aware() {
    let global = lookup("us-east-1", "global.anthropic.claude-opus-5-5");
    assert_eq!(rates(&global), (Some(0.004), Some(0.020)));
    assert_eq!(
        global.note,
        "AWS Price List API AmazonBedrockFoundationModels; verified 2026-09-24; Standard tier, us-east-1, global inference; no Batch"
    );
    assert!(global.note.contains("global inference"));
    let geo = lookup("eu-west-1", "eu.anthropic.claude-opus-5-5");
    assert_eq!(rates(&geo), (Some(0.0044), Some(0.022)));
    let gov = lookup("us-gov-west-1", "us.anthropic.claude-opus-5-5");
    assert_eq!(rates(&gov), (Some(0.0048), Some(0.024)));
    let mantle = lookup("ap-southeast-4", "anthropic.claude-opus-5-5");
    assert_eq!(rates(&mantle), (Some(0.0044), Some(0.022)));
    assert!(mantle.note.contains("mantle inference"));
    let london = lookup("eu-west-2", "anthropic.claude-opus-5-5");
    assert_eq!(rates(&london), (Some(0.0044), Some(0.022)));
    assert!(
        london.note.contains("verified 2026-10-03") && london.note.contains("runtime-in-region")
    );
    // The profile ARN form is priced as its profile id.
    let arn = lookup(
        "us-east-1",
        "arn:aws:bedrock:us-east-1:123456789012:inference-profile/global.anthropic.claude-opus-5-5",
    );
    assert_eq!(rates(&arn), (Some(0.004), Some(0.020)));
    // Everything else is unavailable, never another model's rate.
    for (region, id) in [
        ("us-east-1", "anthropic.claude-opus-5-5-v1:0"),
        ("us-west-2", "anthropic.claude-opus-5-5"),
        ("eu-west-1", "us.anthropic.claude-opus-5-5"),
        ("us-east-1", "apac.anthropic.claude-opus-5-5"),
        ("us-east-1", "global.anthropic.claude-opus-5"),
    ] {
        let price = lookup(region, id);
        assert!(!price.available, "{region} {id}");
        assert_eq!(rates(&price), (None, None));
        assert!(
            price.note.contains("no verified Opus 5.5 rate"),
            "{}",
            price.note
        );
    }
    // #4698: an id without a provider, or an ARN that is not a profile ARN, is
    // rejected before the Opus 5.5 rule is consulted.
    for id in [
        "claude-opus-5-5",
        "arn:aws:bedrock:us-east-1:12345:inference-profile/global.anthropic.claude-opus-5-5",
    ] {
        assert_eq!(lookup("us-east-1", id), pricing::rejected(), "{id}");
    }
}

fn recorded(items: &[&str]) -> PriceList {
    let items: Vec<Json> = items.iter().map(|text| Json::string(*text)).collect();
    PriceList::from_json(&Json::Array(vec![Json::object(vec![
        ("region", Json::string("us-east-1")),
        ("model", Json::string("Claude 3 Haiku")),
        ("price_list", Json::Array(items)),
    ])]))
    .expect("a valid recorded price list")
}

#[test]
fn a_recorded_price_list_prices_a_model_outside_the_table() {
    let item = price_item(&[
        ("d1", "0.00025", "Input tokens"),
        ("d2", "0.00125", "Output tokens"),
    ])
    .to_compact_string();
    let list = recorded(&[&item]);
    let id = "anthropic.claude-3-haiku-20240307-v1:0";
    let price = pricing::lookup("us-east-1", id, Some(&list));
    assert!(price.available);
    assert_eq!(rates(&price), (Some(0.00025), Some(0.00125)));
    assert_eq!(
        price.note,
        "live Pricing API (matched display name 'Claude 3 Haiku')"
    );
    assert_eq!(
        price.to_json().to_compact_string(),
        r#"{"input_per_1k_usd":0.00025,"output_per_1k_usd":0.00125,"available":true,"note":"live Pricing API (matched display name 'Claude 3 Haiku')"}"#
    );
    // Another region or no answer for the display name: unavailable.
    let other = pricing::lookup("eu-west-1", id, Some(&list));
    assert!(!other.available);
    assert_eq!(
        other.note,
        "Pricing unavailable: not in static table and no PriceList entry for display name 'Claude 3 Haiku' in eu-west-1"
    );
    // The static table still wins over a recorded answer.
    let table = pricing::lookup("us-east-1", "amazon.nova-pro-v1:0", Some(&list));
    assert_eq!(table.input_per_1k_usd, Some(0.0008));
}

#[test]
fn an_unusable_recorded_item_is_unavailable_never_a_guess() {
    let id = "anthropic.claude-3-haiku-20240307-v1:0";
    let no_rates = price_item(&[("d", "1", "Other")]).to_compact_string();
    let cases = [
        (
            "not json FORGED",
            "the recorded price item is not valid JSON",
        ),
        ("[]", "the price item has an unexpected structure"),
        (no_rates.as_str(), "rates not found in price item"),
    ];
    for (item, note) in cases {
        let price = pricing::lookup("us-east-1", id, Some(&recorded(&[item])));
        assert!(!price.available, "{item}");
        assert!(price.note.ends_with(note), "{}", price.note);
        assert!(!price.note.contains("FORGED"));
    }
    // An item that is not JSON text, and an entry without items.
    let object = PriceList::from_json(&json(
        r#"[{"region": "us-east-1", "model": "Claude 3 Haiku", "price_list": [{}]}]"#,
    ))
    .unwrap();
    assert!(pricing::lookup("us-east-1", id, Some(&object))
        .note
        .ends_with("is not JSON text"));
    let empty = recorded(&[]);
    assert!(pricing::lookup("us-east-1", id, Some(&empty))
        .note
        .contains("no PriceList entry"));
    // Without any recorded answer the live client is not available.
    let price = lookup("us-east-1", id);
    assert!(!price.available);
    assert!(
        price.note.contains("no live Pricing API client"),
        "{}",
        price.note
    );
}

#[test]
fn a_recorded_price_list_is_validated() {
    for document in [
        "{}",
        "[1]",
        r#"[{"region": "r"}]"#,
        r#"[{"region": "r", "model": "m", "price_list": "x"}]"#,
        r#"[{"region": 1, "model": "m", "price_list": []}]"#,
    ] {
        assert!(PriceList::from_json(&json(document)).is_err(), "{document}");
    }
    assert_eq!(PriceList::from_json(&json("[]")), Ok(PriceList(Vec::new())));
}

/// The offline response of #4698: a valid `get_products` answer for the
/// display name `Claude Opus 4.8` in us-east-1 (0.005 in, 0.025 out).
const OPUS_4_8_RESPONSE: &str = r#"[{"region":"us-east-1","model":"Claude Opus 4.8","price_list":["{\"terms\":{\"OnDemand\":{\"x\":{\"priceDimensions\":{\"input\":{\"description\":\"input tokens\",\"pricePerUnit\":{\"USD\":\"0.005\"}},\"output\":{\"description\":\"output tokens\",\"pricePerUnit\":{\"USD\":\"0.025\"}}}}}}}"]}]"#;

fn opus_4_8_response() -> PriceList {
    PriceList::from_json(&json(OPUS_4_8_RESPONSE)).expect("the #4698 fixture")
}

/// Ids whose identity is malformed, truncated or a respelling of a table row:
/// rejected before any source of price, whatever a recorded answer holds.
const REJECTED_IDS: &[&str] = &[
    "",
    "-",
    ":",
    "anthropic",
    "openai",
    "amazon",
    "us.",
    ".anthropic.claude-opus-4-8",
    "openai.gpt-",
    "-v1",
    "-v1:0",
    "anthropic.claude-opus-4-8-",
    "anthropic.claude-opus-4-8.",
    "anthropic.claude-opus-4-8--v1:0",
    // Malformed revisions (#4698): truncated, repeated, arbitrary, non-ASCII.
    "anthropic.claude-opus-4-8-v1:",
    "anthropic.claude-opus-4-8-v1:0:0",
    "anthropic.claude-opus-4-8-v1::0",
    "anthropic.claude-opus-4-8:arbitrary",
    "anthropic.claude-opus-4-8-v:0",
    "anthropic.claude-opus-4-8-x1:0",
    "anthropic.claude-opus-4-8-v\u{661}:0",
    "anthropic.claude-opus-4-8-v1:\u{660}",
    "anthropic.claude-opus-4-8-v\u{ff11}:0",
    "anthropic.claude-opus-4-8-v1:0:28k",
    "anthropic.claude-opus-4-8-v1:0\n",
    "anthropic.claude-opus-4-8-v1:0 ",
    "us.anthropic.claude-opus-4-8:0",
    // Respellings that only a display name would equate with a table row.
    "anthropic.claude-opus-4.8",
    "us.anthropic.claude-opus-4.8-20250610-v1:0",
    "anthropic.claude-OPUS-4-8",
    "Anthropic.Claude-Opus-4-8",
    "anthropic.claude-opus-4-8:0",
    "anthropic.claude-opus-4-\u{e5f}",
    "arn:aws:bedrock:us-east-1:1:inference-profile/anthropic.claude-opus-4-8",
];

/// Well-formed ids that name no table row: unavailable without a matching
/// recorded answer, and the #4698 response names none of them.
const UNMATCHED_IDS: &[&str] = &[
    "anthropic.claude",
    "anthropic.claude-sonnet",
    "anthropic.claude-haiku-4",
    "us.anthropic.claude",
    "amazon.nova",
    "anthropic.claude-opus-4-8-foo",
    "us.anthropic.claude-sonnet-5-preview",
    "anthropic.claude-opus-4-85-v1:0",
    "anthropic.claude-opus-4-8-v",
    "anthropic.claude-opus-4-8-vx1",
    // A seven-digit date is not a date stamp: the id stays distinct.
    "anthropic.claude-opus-4-8-2025061",
    "anthropic.claude-opus-4-8-2025061-v1:0",
    "anthropic.claude-opus-4-8-20250610-extra",
];

#[test]
fn rejected_ids_are_never_priced_even_by_a_matching_recorded_answer() {
    // #4698: the recorded response is valid and prices `Claude Opus 4.8`; it
    // must not validate a malformed, truncated or respelled id.
    let list = opus_4_8_response();
    for id in REJECTED_IDS {
        assert_eq!(
            pricing::classify(STATIC_FALLBACK, id),
            Identity::Rejected,
            "{id:?}"
        );
        assert_eq!(
            WellFormedId::parse(id).is_some(),
            {
                // Respellings are well-formed text; the rest is not.
                matches!(
                    *id,
                    "anthropic.claude-opus-4.8"
                        | "us.anthropic.claude-opus-4.8-20250610-v1:0"
                        | "us.anthropic.claude-opus-4-8:0"
                        | "anthropic.claude-opus-4-8:0"
                )
            },
            "{id:?}"
        );
        for price_list in [None, Some(&list)] {
            let price = pricing::lookup("us-east-1", id, price_list);
            assert_eq!(price, pricing::rejected(), "{id:?}");
            assert_eq!(
                price.to_json().to_compact_string(),
                format!(
                    r#"{{"available":false,"input_per_1k_usd":null,"output_per_1k_usd":null,"note":"{}"}}"#,
                    price.note
                ),
                "{id:?}"
            );
        }
        assert_eq!(pricing::static_fallback(id), None, "{id:?}");
    }
}

#[test]
fn every_negative_pricing_input_stays_unpriced_with_a_recorded_answer() {
    let list = opus_4_8_response();
    for id in REJECTED_IDS.iter().chain(UNMATCHED_IDS) {
        let price = pricing::lookup("us-east-1", id, Some(&list));
        assert!(!price.available, "{id:?}");
        assert_eq!(rates(&price), (None, None), "{id:?}");
        assert!(price.note.starts_with("Pricing unavailable: "), "{id:?}");
    }
    for id in UNMATCHED_IDS {
        assert!(
            matches!(
                pricing::classify(STATIC_FALLBACK, id),
                Identity::WellFormedUnknown(_)
            ),
            "{id:?}"
        );
        let shown = pricing::display_name_guess(WellFormedId::parse(id).unwrap());
        assert_ne!(shown, "Claude Opus 4.8", "{id:?}");
    }
}

#[test]
fn the_pricing_command_rejects_malformed_revisions_with_a_recorded_answer() {
    // The #4698 reproduction, through the command.
    let dir = TempDir::new("price-4698");
    let file = dir.file("prices.json", OPUS_4_8_RESPONSE);
    let (status, out, err) = exec(&[
        "bedrock-resolve",
        "pricing",
        "--region",
        "us-east-1",
        "--models",
        "anthropic.claude-opus-4-8-v1:,anthropic.claude-opus-4-8-v1:0:0,anthropic.claude-opus-4-8:arbitrary,apac.anthropic.claude-opus-4-8",
        "--price-list",
        &file,
    ]);
    assert_eq!((status, err.as_str()), (0, ""));
    let answer = json(&out);
    for id in [
        "anthropic.claude-opus-4-8-v1:",
        "anthropic.claude-opus-4-8-v1:0:0",
        "anthropic.claude-opus-4-8:arbitrary",
    ] {
        let entry = answer.get(id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!(entry, &pricing::rejected().to_json(), "{id}");
    }
    // The response itself is valid: a well-formed id outside the table that
    // shows as that display name is priced from it.
    let apac = answer.get("apac.anthropic.claude-opus-4-8").unwrap();
    assert_eq!(apac.get("available"), Some(&Json::Bool(true)));
    assert_eq!(apac.get("input_per_1k_usd"), Some(&Json::Float(0.005)));
}

#[test]
fn well_formed_ids_outside_the_table_still_use_a_recorded_answer() {
    let list = opus_4_8_response();
    for id in [
        "apac.anthropic.claude-opus-4-8",
        "apac.anthropic.claude-opus-4-8-20250610-v1:0",
        "apac.anthropic.claude-opus-4-8-v2",
    ] {
        assert!(
            matches!(
                pricing::classify(STATIC_FALLBACK, id),
                Identity::WellFormedUnknown(_)
            ),
            "{id}"
        );
        let price = pricing::lookup("us-east-1", id, Some(&list));
        assert!(price.available, "{id}");
        assert_eq!(rates(&price), (Some(0.005), Some(0.025)), "{id}");
        assert_eq!(
            price.note,
            "live Pricing API (matched display name 'Claude Opus 4.8')"
        );
    }
    // The `-<N>:<M>` revision of the open-weight models is well-formed.
    let item = price_item(&[
        ("d1", "0.00015", "Input tokens"),
        ("d2", "0.0006", "Output tokens"),
    ])
    .to_compact_string();
    let oss = PriceList::from_json(&Json::Array(vec![Json::object(vec![
        ("region", Json::string("us-west-2")),
        ("model", Json::string("Gpt Oss 120b 1")),
        ("price_list", Json::Array(vec![Json::string(item)])),
    ])]))
    .unwrap();
    let price = pricing::lookup("us-west-2", "openai.gpt-oss-120b-1:0", Some(&oss));
    assert_eq!(rates(&price), (Some(0.00015), Some(0.0006)));
    // Table rows and their normalized forms are still the table's answer, and
    // the table wins over a recorded answer for the same display name.
    for id in [
        "anthropic.claude-opus-4-8",
        "anthropic.claude-opus-4-8-v1:0",
        "anthropic.claude-opus-4-8-20250610-v1:0",
        "us.anthropic.claude-opus-4-8-20250610",
    ] {
        assert!(
            matches!(
                pricing::classify(STATIC_FALLBACK, id),
                Identity::KnownRow { .. }
            ),
            "{id}"
        );
        let price = pricing::lookup("us-east-1", id, Some(&list));
        assert!(price.available, "{id}");
        assert!(price.note.starts_with("static pricing table"), "{id}");
    }
}

#[test]
fn every_table_row_is_a_well_formed_known_row() {
    for row in STATIC_FALLBACK {
        assert!(WellFormedId::parse(row.0).is_some(), "{}", row.0);
        assert_eq!(
            pricing::classify(STATIC_FALLBACK, row.0),
            Identity::KnownRow { row, exact: true },
            "{}",
            row.0
        );
    }
}

#[test]
fn the_pricing_command_prints_the_scripts_json() {
    let (status, out, err) = exec(&[
        "bedrock-resolve",
        "pricing",
        "--region",
        "us-east-1",
        "--models",
        " amazon.nova-micro-v1:0 , ,openai.gpt-5.6, amazon.nova-micro-v1:0",
    ]);
    assert_eq!((status, err.as_str()), (0, ""));
    let expected = "{\n  \"amazon.nova-micro-v1:0\": {\n    \"input_per_1k_usd\": 3.5e-05,\n    \"output_per_1k_usd\": 0.00014,\n    \"available\": true,\n    \"note\": \"static pricing table (verified 2026-10-05 against aws.amazon.com/bedrock/pricing, the Bedrock model cards' Geo/Global inference ID tables, and the vendored pricing cache)\"\n  },\n  \"openai.gpt-5.6\": {\n    \"available\": false,\n    \"input_per_1k_usd\": null,\n    \"output_per_1k_usd\": null,\n    \"note\": \"Pricing unavailable: the AWS PriceList API does not carry OpenAI's proprietary GPT models, and openai.gpt-5.6 is not in the static table. This does NOT mean the model is unavailable. Read the rate from the OpenAI tab of aws.amazon.com/bedrock/pricing and add it to STATIC_FALLBACK. Do not derive it from a percentage change to an older rate.\"\n  }\n}\n";
    assert_eq!(out, expected);
    // No model at all is an empty object, like the script.
    let (status, out, _) = exec(&[
        "bedrock-resolve",
        "pricing",
        "--region",
        "r",
        "--models",
        " , ",
    ]);
    assert_eq!((status, out.as_str()), (0, "{}\n"));
}

#[test]
fn the_pricing_command_reads_a_recorded_price_list() {
    let dir = TempDir::new("price-list");
    let item = price_item(&[
        ("d1", "0.003", "Input tokens"),
        ("d2", "0.015", "Output tokens"),
    ])
    .to_compact_string();
    let list = Json::Array(vec![Json::object(vec![
        ("region", Json::string("us-east-1")),
        ("model", Json::string("Claude 3 Haiku")),
        ("price_list", Json::Array(vec![Json::string(item)])),
    ])]);
    let file = dir.file("list.json", &list.to_compact_string());
    let (status, out, _) = exec(&[
        "bedrock-resolve",
        "pricing",
        "--region",
        "us-east-1",
        "--models",
        "anthropic.claude-3-haiku-20240307-v1:0",
        "--price-list",
        &file,
    ]);
    assert_eq!(status, 0);
    assert!(out.contains("\"input_per_1k_usd\": 0.003,"), "{out}");
    let bad = dir.file("bad.json", "{}");
    let (status, out, err) = exec(&[
        "bedrock-resolve",
        "pricing",
        "--region",
        "r",
        "--models",
        "m",
        "--price-list",
        &bad,
    ]);
    assert_eq!((status, out.as_str()), (1, ""));
    assert!(err.contains("JSON array"), "{err}");
}

#[test]
fn the_pricing_command_bounds_and_disambiguates_its_models() {
    let many: Vec<String> = (0..300).map(|n| format!("m{n}")).collect();
    let (status, out, _) = exec(&[
        "bedrock-resolve",
        "pricing",
        "--region",
        "r",
        "--models",
        &many.join(","),
    ]);
    assert_eq!((status, out.as_str()), (2, ""));
    // Two long ids that only differ past the display bound are refused rather
    // than shown under one name.
    let long = "a".repeat(130);
    let models = format!("{long}1,{long}2");
    let (status, out, err) = exec(&[
        "bedrock-resolve",
        "pricing",
        "--region",
        "r",
        "--models",
        &models,
    ]);
    assert_eq!((status, out.as_str()), (2, ""));
    assert!(err.contains("indistinguishable"), "{err}");
    for args in [
        &["bedrock-resolve", "pricing", "--models", "m"][..],
        &["bedrock-resolve", "pricing", "--region", "r"],
        &[
            "bedrock-resolve",
            "pricing",
            "--region",
            "r",
            "--models",
            "m",
            "extra",
        ],
    ] {
        assert_eq!(exec(args).0, 2, "{args:?}");
    }
}

/// The table rows of `bedrock_pricing.py` (`"key": {"input_per_1k_usd": N,
/// "output_per_1k_usd": M}`), read as text: the migration source of truth
/// until the Python file is retired (#4415, which deletes this test with it).
fn python_table_rows() -> Option<Vec<(String, f64, f64)>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/aws-startup-advisor/skills/llm-to-bedrock/scripts/bedrock_pricing.py");
    let text = fs::read_to_string(path).ok()?;
    let start = text.find("STATIC_FALLBACK = {")?;
    let body = &text[start..];
    let end = body.find("\n}\n")?;
    let mut rows = Vec::new();
    for line in body[..end].lines().skip(1) {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('"') else {
            continue;
        };
        let (key, rest) = rest.split_once("\":")?;
        let rest = rest.trim().strip_prefix("{\"input_per_1k_usd\":")?;
        let (input, rest) = rest.split_once(", \"output_per_1k_usd\":")?;
        let output = rest.trim().trim_end_matches(',').strip_suffix('}')?;
        rows.push((
            key.to_string(),
            input.trim().parse().ok()?,
            output.trim().parse().ok()?,
        ));
    }
    Some(rows)
}

#[test]
fn the_rate_table_matches_the_python_source() {
    // The pricing_behavior comparison of check-bedrock-pricing-parity.py
    // (item 37 of docs/migration/startup-validators.md): the table the lookup
    // serves is the one the Python module serves, row for row and in order.
    let Some(rows) = python_table_rows() else {
        return; // retired with the Python module (#4415)
    };
    assert_eq!(rows.len(), STATIC_FALLBACK.len(), "row count");
    for ((py_key, py_in, py_out), (key, input, output)) in rows.iter().zip(STATIC_FALLBACK) {
        assert_eq!(py_key, key);
        assert!(py_in.to_bits() == input.to_bits(), "{key} input");
        assert!(py_out.to_bits() == output.to_bits(), "{key} output");
    }
}

#[test]
fn the_parity_probes_of_the_python_check_resolve_as_before() {
    // The probes of `pricing_behavior`: every table key, two fabricated dated
    // forms and an unlisted GPT tier.
    for (key, input, output) in STATIC_FALLBACK {
        let exact = pricing::static_fallback(key).unwrap_or_else(|| panic!("{key}"));
        assert_eq!(rates(&exact), (Some(*input), Some(*output)), "{key}");
        let price = lookup("us-east-1", key);
        assert_eq!(rates(&price), (Some(*input), Some(*output)), "{key}");
    }
    let probes = [
        ("anthropic.claude-opus-4-8-20250610-v1:0", Some(0.005)),
        ("us.anthropic.claude-sonnet-4-6-20250514-v1:0", Some(0.0033)),
        ("openai.gpt-5.6", None),
    ];
    for (probe, input) in probes {
        assert_eq!(
            pricing::static_fallback(probe).and_then(|price| price.input_per_1k_usd),
            input,
            "{probe}"
        );
    }
    assert_eq!(
        pricing::parse_price_dimensions(&price_item(&[
            ("input", "0.003", "Input tokens"),
            ("output", "0.015", "Output tokens"),
            ("cache", "0.0003", "Cache read input tokens"),
        ])),
        Ok((Some(0.003), Some(0.015)))
    );
    assert_eq!(
        pricing::display_name_guess(
            WellFormedId::parse("us.anthropic.claude-haiku-4-5-20251001-v1:0").unwrap()
        ),
        "Claude Haiku 4.5"
    );
    assert_eq!(STATIC_FALLBACK.len(), 44);
}

// ---------------------------------------------------------------------------
// source model resolution (test_resolve_source_model.py)

fn ids(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn resolved(catalog: &[&str], plan: &str) -> Json {
    source::resolve(&ids(catalog), plan)
}

fn status(value: &Json) -> &str {
    value.get("status").and_then(Json::as_str).expect("status")
}

#[test]
fn an_exact_match_wins() {
    let out = resolved(&["gpt-5.4", "gpt-5.4-2026-03-05"], "gpt-5.4");
    assert_eq!(
        out.to_compact_string(),
        r#"{"status":"exact","resolved_id":"gpt-5.4"}"#
    );
}

#[test]
fn a_date_or_version_suffix_is_the_same_line() {
    let out = resolved(&["gpt-5.4-2026-03-05"], "gpt-5.4");
    assert_eq!(status(&out), "prefix");
    assert_eq!(
        out.get("resolved_id").and_then(Json::as_str),
        Some("gpt-5.4-2026-03-05")
    );
    assert_eq!(
        status(&resolved(&["claude-3-5-sonnet-2"], "claude-3-5-sonnet")),
        "prefix"
    );
}

#[test]
fn the_shortest_safe_variant_is_picked() {
    let out = resolved(
        &["gpt-5.4-2026-03-05-1234", "gpt-5.4-2026-03-05"],
        "gpt-5.4",
    );
    assert_eq!(
        out.get("resolved_id").and_then(Json::as_str),
        Some("gpt-5.4-2026-03-05")
    );
    assert_eq!(
        out.to_compact_string(),
        r#"{"status":"prefix","resolved_id":"gpt-5.4-2026-03-05","all_hits":["gpt-5.4-2026-03-05"]}"#
    );
}

#[test]
fn tied_snapshots_are_ambiguous_in_either_catalog_order() {
    // #4685: two distinct dated snapshots of the same length have no
    // deterministic winner. Never a resolved_id; both stay in all_hits.
    let expected = r#"{"status":"not_found","candidates":["gpt-5.4-2026-03-05","gpt-5.4-2026-04-01"],"ambiguous_prefix":true,"all_hits":["gpt-5.4-2026-03-05","gpt-5.4-2026-04-01"]}"#;
    for catalog in [
        ["gpt-5.4-2026-03-05", "gpt-5.4-2026-04-01"],
        ["gpt-5.4-2026-04-01", "gpt-5.4-2026-03-05"],
    ] {
        let out = resolved(&catalog, "gpt-5.4");
        assert_eq!(status(&out), "not_found", "{catalog:?}");
        assert_eq!(out.get("resolved_id"), None, "{catalog:?}");
        assert_eq!(
            out.get("ambiguous_prefix"),
            Some(&Json::Bool(true)),
            "{catalog:?}"
        );
        let hits = out.get("all_hits").expect("all_hits");
        assert_eq!(
            hits.to_compact_string(),
            r#"["gpt-5.4-2026-03-05","gpt-5.4-2026-04-01"]"#
        );
        assert_eq!(out.to_compact_string(), expected, "{catalog:?}");
    }
    // Two versions of equal length tie as well; a shorter one still wins.
    assert_eq!(
        status(&resolved(
            &["gpt-5.4-3", "gpt-5.4-4", "gpt-5.4-3.1"],
            "gpt-5.4"
        )),
        "not_found"
    );
    assert_eq!(
        resolved(&["gpt-5.4-3.1", "gpt-5.4-3", "gpt-5.4-3.2"], "gpt-5.4")
            .get("resolved_id")
            .and_then(Json::as_str),
        Some("gpt-5.4-3")
    );
}

#[test]
fn a_duplicated_catalog_id_is_one_candidate_not_a_tie() {
    let out = resolved(
        &[
            "gpt-5.4-2026-03-05",
            "gpt-5.4-2026-03-05",
            "gpt-5.4-2026-03-05-1234",
        ],
        "gpt-5.4",
    );
    assert_eq!(
        out.to_compact_string(),
        r#"{"status":"prefix","resolved_id":"gpt-5.4-2026-03-05","all_hits":["gpt-5.4-2026-03-05"]}"#
    );
}

#[test]
fn alphabetic_suffixes_never_auto_resolve() {
    let catalog = ["gpt-5.4-mini", "gpt-5.4-pro", "gpt-5.4-latest"];
    let out = resolved(&catalog, "gpt-5.4");
    assert_eq!(status(&out), "not_found");
    assert_eq!(out.get("ambiguous_prefix"), Some(&Json::Bool(true)));
    assert_eq!(
        out.to_compact_string(),
        r#"{"status":"not_found","candidates":["gpt-5.4-mini","gpt-5.4-pro","gpt-5.4-latest"],"ambiguous_prefix":true}"#
    );
}

#[test]
fn a_different_line_never_matches() {
    let out = resolved(&["gpt-5", "gpt-54", "gpt-5.5"], "gpt-5.4");
    assert_eq!(status(&out), "not_found");
    assert_eq!(out.get("ambiguous_prefix"), None);
}

#[test]
fn no_match_ranks_by_the_longest_common_prefix() {
    let out = resolved(
        &["claude-3-5-haiku", "gemini-1.5-pro", "gpt-4o"],
        "claude-3-5-sonnet",
    );
    assert_eq!(status(&out), "not_found");
    assert_eq!(texts(&out, "candidates")[0], "\"claude-3-5-haiku\"");
    // At most five candidates, stable on ties.
    let many = ["a", "b", "c", "d", "e", "f", "g"];
    assert_eq!(texts(&resolved(&many, "z"), "candidates").len(), 5);
    assert_eq!(texts(&resolved(&many, "z"), "candidates")[0], "\"a\"");
    let hits: Vec<String> = (0..7).map(|n| format!("m-mini{n}")).collect();
    let hits: Vec<&str> = hits.iter().map(String::as_str).collect();
    assert_eq!(texts(&resolved(&hits, "m"), "candidates").len(), 5);
    // An empty catalog resolves nothing.
    assert_eq!(
        resolved(&[], "gpt-4o").to_compact_string(),
        r#"{"status":"not_found","candidates":[]}"#
    );
}

#[test]
fn the_safe_variant_boundary() {
    assert!(source::safe_variant("gpt-5.4-2026-03-05", "gpt-5.4"));
    assert!(source::safe_variant("gpt-5.4-3.1", "gpt-5.4"));
    assert!(source::safe_variant("gpt-5.4", "gpt-5.4"));
    assert!(!source::safe_variant("gpt-5.4-turbo", "gpt-5.4"));
    assert!(!source::safe_variant(
        "gpt-5.4-2026-03-05-preview",
        "gpt-5.4"
    ));
    assert!(!source::safe_variant("gpt-5.4-", "gpt-5.4"));
    assert!(!source::safe_variant("gpt-5.4-.5", "gpt-5.4"));
    assert!(!source::safe_variant("gpt-5.4-2026-3-05", "gpt-5.4"));
    // Stricter than the script (docs/migration/bedrock-resolve.md): a trailing
    // newline and non-ASCII digits are not a safe suffix.
    assert!(!source::safe_variant("gpt-5.4-2026-03-05\n", "gpt-5.4"));
    assert!(!source::safe_variant("gpt-5.4-3\n", "gpt-5.4"));
    assert!(!source::safe_variant("gpt-5.4-\u{661}\u{662}", "gpt-5.4"));
}

#[test]
fn a_resolution_with_a_unicode_plan_id_counts_characters() {
    let out = resolved(&["modèle-2026-03-05"], "modèle");
    assert_eq!(status(&out), "prefix");
}

fn page(text: &str) -> Json {
    json(text)
}

#[test]
fn the_anthropic_pagination_follows_has_more() {
    // A model on page 2 resolves, it is not reported not_found.
    let pages = Json::Array(vec![
        page(
            r#"{"data": [{"id": "claude-3-5-haiku-20241022"}], "has_more": true, "last_id": "claude-3-5-haiku-20241022"}"#,
        ),
        page(r#"{"data": [{"id": "claude-3-5-sonnet-20241022"}], "has_more": false}"#),
    ]);
    let catalog = source::catalog_ids("ANTHROPIC_API_KEY", &pages).unwrap();
    assert_eq!(
        catalog,
        ids(&["claude-3-5-haiku-20241022", "claude-3-5-sonnet-20241022"])
    );
    assert_eq!(
        status(&source::resolve(&catalog, "claude-3-5-sonnet")),
        "prefix"
    );
}

#[test]
fn the_gemini_pagination_follows_the_next_page_token() {
    let pages = Json::Array(vec![
        page(r#"{"models": [{"name": "models/gemini-1.5-flash"}], "nextPageToken": "tok2"}"#),
        page(r#"{"models": [{"name": "models/gemini-1.5-pro"}]}"#),
    ]);
    let catalog = source::catalog_ids("GEMINI_API_KEY", &pages).unwrap();
    assert!(
        catalog.contains(&"gemini-1.5-pro".to_string()),
        "{catalog:?}"
    );
    assert_eq!(catalog.len(), 2);
    // A page without models is an empty page.
    assert_eq!(
        source::catalog_ids("GEMINI_API_KEY", &page("{}")),
        Ok(Vec::new())
    );
}

#[test]
fn the_openai_catalog_is_one_page() {
    let one = page(r#"{"data": [{"id": "gpt-4o"}, {"id": "gpt-5.4"}]}"#);
    assert_eq!(
        source::catalog_ids("OPENAI_API_KEY", &one),
        Ok(ids(&["gpt-4o", "gpt-5.4"]))
    );
    let flat = page(r#"["gpt-4o", "o3"]"#);
    assert_eq!(
        source::catalog_ids("OPENAI_API_KEY", &flat),
        Ok(ids(&["gpt-4o", "o3"]))
    );
    assert_eq!(
        source::catalog_ids("OPENAI_API_KEY", &page("[]")),
        Ok(Vec::new())
    );
}

#[test]
fn an_incomplete_or_malformed_catalog_is_an_error_never_a_not_found() {
    let first = r#"{"data": [{"id": "a"}], "has_more": true}"#;
    let last = r#"{"data": [{"id": "b"}], "has_more": false}"#;
    // The listing stops before its last page.
    let error = source::catalog_ids("ANTHROPIC_API_KEY", &page(&format!("[{first}]"))).unwrap_err();
    assert!(error.contains("ends before the last page"), "{error}");
    // It continues past it.
    let error =
        source::catalog_ids("ANTHROPIC_API_KEY", &page(&format!("[{last}, {last}]"))).unwrap_err();
    assert!(error.contains("continues past"), "{error}");
    // OpenAI has one page.
    assert!(source::catalog_ids("OPENAI_API_KEY", &page(&format!("[{last}, {last}]"))).is_err());
    for (key, document) in [
        ("OPENAI_API_KEY", r#"{"data": 1}"#),
        ("OPENAI_API_KEY", r"{}"),
        ("OPENAI_API_KEY", r#"{"data": [{"id": 5}]}"#),
        ("OPENAI_API_KEY", r#"{"data": [{}]}"#),
        ("GEMINI_API_KEY", r#"{"models": [{"name": 5}]}"#),
        ("GEMINI_API_KEY", r#"{"models": {}}"#),
        ("OPENAI_API_KEY", r#""gpt""#),
        ("OPENAI_API_KEY", r#"["a", 1]"#),
        ("OPENAI_API_KEY", r"5"),
    ] {
        assert!(
            source::catalog_ids(key, &page(document)).is_err(),
            "{key} {document}"
        );
    }
}

#[test]
fn the_env_file_is_authoritative_for_the_provider() {
    let pairs = source::parse_env_file(
        "SOMETHING_ELSE=x\n GEMINI_API_KEY = spaced \nGEMINI_API_KEY=AIza-file\nnoequals\n=\n",
    );
    assert_eq!(
        pairs,
        vec![
            ("SOMETHING_ELSE".to_string(), "x".to_string()),
            ("GEMINI_API_KEY ".to_string(), " spaced".to_string()),
            ("GEMINI_API_KEY".to_string(), "AIza-file".to_string()),
            (String::new(), String::new()),
        ]
    );
    // An ambient OPENAI_API_KEY cannot select OpenAI: only the file counts.
    assert_eq!(source::pick_provider(&pairs, ""), Some("GEMINI_API_KEY"));
    // SOURCE_PROVIDER beats file-order guessing.
    let pairs = source::parse_env_file("OPENAI_API_KEY=a\nGEMINI_API_KEY=c\n");
    assert_eq!(
        source::pick_provider(&pairs, "google"),
        Some("GEMINI_API_KEY")
    );
    assert_eq!(
        source::pick_provider(&pairs, "GEMINI"),
        Some("GEMINI_API_KEY")
    );
    assert_eq!(
        source::pick_provider(&pairs, "anthropic"),
        None,
        "not in the file"
    );
    assert_eq!(
        source::pick_provider(&pairs, "mistral"),
        None,
        "unknown provider"
    );
    assert_eq!(source::pick_provider(&pairs, ""), Some("OPENAI_API_KEY"));
    assert_eq!(source::pick_provider(&[], ""), None);
    // The first `=` splits; the value keeps later ones.
    assert_eq!(
        source::parse_env_file("OPENAI_API_KEY=a=b=="),
        vec![("OPENAI_API_KEY".to_string(), "a=b==".to_string())]
    );
}

#[test]
fn secrets_are_redacted_in_every_textual_form() {
    let secret = "sk-abc\r\nXYZ-LONG-FRAGMENT-123".to_string();
    let secrets = vec![secret.clone(), String::new()];
    assert_eq!(
        source::redact(&format!("a {secret} b"), &secrets),
        "a *** b"
    );
    // Rendered escaped, as an exception message shows it.
    assert_eq!(
        source::redact("a sk-abc\\r\\nXYZ-LONG-FRAGMENT-123 b", &secrets),
        "a *** b"
    );
    // A fragment between control characters.
    assert_eq!(
        source::redact("only XYZ-LONG-FRAGMENT-123 here", &secrets),
        "only *** here"
    );
    // Short fragments and empty secrets are left alone.
    assert_eq!(source::redact("sk-ab", &secrets), "sk-ab");
    assert_eq!(source::redact("nothing", &[String::new()]), "nothing");
    // Text is redacted before it is escaped, so an escape cannot hide a secret.
    assert!(!source::shown(&format!("x{SECRET}y\u{1b}"), &[SECRET.to_string()]).contains(SECRET));
}

fn write_env(dir: &TempDir, content: &str) -> String {
    dir.file(".source-provider-env", content)
}

#[test]
fn the_source_model_command_resolves_a_recorded_catalog() {
    let dir = TempDir::new("source");
    let env = write_env(&dir, &format!("OPENAI_API_KEY={SECRET}\n"));
    let catalog = dir.file(
        "catalog.json",
        r#"{"data": [{"id": "gpt-5.4-2026-03-05"}, {"id": "gpt-5.4-mini"}]}"#,
    );
    let (code, out, err) = exec(&[
        "bedrock-resolve",
        "source-model",
        &env,
        "--catalog",
        &catalog,
        "--plan-model-id",
        "gpt-5.4",
        "--source-provider",
        "openai",
    ]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        out,
        "{\"status\": \"prefix\", \"resolved_id\": \"gpt-5.4-2026-03-05\", \"all_hits\": [\"gpt-5.4-2026-03-05\"]}\n"
    );
    let (code, out, _) = exec(&[
        "bedrock-resolve",
        "source-model",
        &env,
        "--catalog",
        &catalog,
        "--plan-model-id",
        "gpt-5.4-mini",
    ]);
    assert_eq!(
        (code, out.as_str()),
        (
            0,
            "{\"status\": \"exact\", \"resolved_id\": \"gpt-5.4-mini\"}\n"
        )
    );
    let (code, out, _) = exec(&[
        "bedrock-resolve",
        "source-model",
        &env,
        "--catalog",
        &catalog,
        "--plan-model-id",
        "gpt-9",
    ]);
    assert_eq!(code, 0);
    assert!(
        out.starts_with("{\"status\": \"not_found\", \"candidates\": ["),
        "{out}"
    );
}

#[test]
fn a_missing_key_exits_2_before_any_catalog_is_read() {
    let dir = TempDir::new("no-key");
    let env = write_env(&dir, "SOMETHING_ELSE=x\n");
    let (code, out, err) = exec(&[
        "bedrock-resolve",
        "source-model",
        &env,
        "--plan-model-id",
        "gpt-4o",
    ]);
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (2, "{\"status\": \"no_key\"}\n", "")
    );
    // A stated provider that is not in the file is also no key.
    let env = write_env(&dir, "OPENAI_API_KEY=key-for-links-0001\n");
    let (code, out, _) = exec(&[
        "bedrock-resolve",
        "source-model",
        &env,
        "--plan-model-id",
        "x",
        "--source-provider",
        "anthropic",
    ]);
    assert_eq!((code, out.as_str()), (2, "{\"status\": \"no_key\"}\n"));
}

#[test]
fn an_unusable_catalog_is_a_redacted_error_answer_with_exit_3() {
    let dir = TempDir::new("source-error");
    let env = write_env(&dir, &format!("OPENAI_API_KEY={SECRET}\n"));
    let bad = dir.file("bad.json", "{ not json");
    let page = dir.file(
        "page1.json",
        r#"[{"data": [{"id": "a"}], "has_more": true}]"#,
    );
    let provider = ["--source-provider", "anthropic"];
    let anthropic_env = write_env(&dir, &format!("ANTHROPIC_API_KEY={SECRET}\n"));
    let cases: [(&str, Vec<&str>); 4] = [
        (
            "no recorded catalog",
            vec![&env, "--plan-model-id", "gpt-4o"],
        ),
        (
            "not valid JSON",
            vec![&env, "--plan-model-id", "gpt-4o", "--catalog", &bad],
        ),
        (
            "cannot read",
            vec![
                &env,
                "--plan-model-id",
                "gpt-4o",
                "--catalog",
                "/no/such/catalog.json",
            ],
        ),
        (
            "ends before the last page",
            vec![
                &anthropic_env,
                "--plan-model-id",
                "x",
                "--catalog",
                &page,
                provider[0],
                provider[1],
            ],
        ),
    ];
    for (needle, args) in cases {
        let mut full = vec!["bedrock-resolve", "source-model"];
        full.extend(args);
        let (code, out, err) = exec(&full);
        assert_eq!((code, err.as_str()), (3, ""), "{needle}");
        assert!(
            out.starts_with("{\"status\": \"error\", \"detail\": \""),
            "{out}"
        );
        assert!(out.contains(needle), "{needle}: {out}");
        assert_clean("error answer", &out);
    }
}

#[test]
fn an_unreadable_env_file_or_a_missing_argument_is_not_an_answer() {
    let (code, out, err) = exec(&[
        "bedrock-resolve",
        "source-model",
        "/no/such/env",
        "--plan-model-id",
        "x",
    ]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.starts_with("error: cannot read /no/such/env:"), "{err}");
    // The script prints its usage and exits 2.
    for args in [
        &["bedrock-resolve", "source-model"][..],
        &["bedrock-resolve", "source-model", "a", "b"],
    ] {
        let (code, out, err) = exec(args);
        assert_eq!((code, out.as_str()), (2, ""), "{args:?}");
        assert!(err.contains("ENV_FILE"), "{err}");
    }
    let dir = TempDir::new("no-plan");
    let env = write_env(&dir, "OPENAI_API_KEY=key-for-links-0001\n");
    if std::env::var_os("PLAN_MODEL_ID").is_none() {
        let (code, _, err) = exec(&["bedrock-resolve", "source-model", &env]);
        assert_eq!(code, 2);
        assert!(err.contains("PLAN_MODEL_ID is not set"), "{err}");
    }
}

// ---------------------------------------------------------------------------
// links are refused for every owned read

#[cfg(unix)]
#[test]
fn symbolic_links_are_refused_for_every_owned_read() {
    let dir = TempDir::new("links");
    let real_json = dir.file("real.json", "[]");
    let real_env = dir.file("real.env", "OPENAI_API_KEY=key-for-links-0001\n");
    let real_png = dir.bytes("real.png", RAW);
    let real_prompt = dir.file("real.txt", "hi");
    let link_json = dir.link("link.json", &real_json);
    let link_env = dir.link("link.env", &real_env);
    let link_png = dir.link("link.png", &real_png);
    let link_prompt = dir.link("link.txt", &real_prompt);

    let cases: [(Vec<&str>, &str); 6] = [
        (
            vec!["identity", "--provider", "openai", "--mappings", &link_json],
            "mappings",
        ),
        (
            vec![
                "pricing",
                "--region",
                "r",
                "--models",
                "m",
                "--price-list",
                &link_json,
            ],
            "price list",
        ),
        (
            vec!["image", "--emit", "data-url", "--image", &link_png],
            "image",
        ),
        (
            vec!["image", "--emit", "chat", "--prompt-file", &link_prompt],
            "prompt",
        ),
        (
            vec!["source-model", &link_env, "--plan-model-id", "x"],
            "env file",
        ),
        (
            vec![
                "image", "--emit", "chat", "--prompt", "x", "--image", &link_png,
            ],
            "message image",
        ),
    ];
    for (args, what) in cases {
        let mut full = vec!["bedrock-resolve"];
        full.extend(args);
        let (code, out, err) = exec(&full);
        assert_eq!((code, out.as_str()), (1, ""), "{what}: {err}");
        assert!(
            err.contains("symbolic links are not followed"),
            "{what}: {err}"
        );
    }
    // The catalog is an owned read as well, and its refusal is an error answer.
    let env = write_env(&dir, "OPENAI_API_KEY=key-for-links-0001\n");
    let (code, out, _) = exec(&[
        "bedrock-resolve",
        "source-model",
        &env,
        "--plan-model-id",
        "x",
        "--catalog",
        &link_json,
    ]);
    assert_eq!(code, 3);
    assert!(out.contains("symbolic links are not followed"), "{out}");
    // A directory is not a regular file either.
    let (code, _, err) = exec(&[
        "bedrock-resolve",
        "image",
        "--emit",
        "data-url",
        "--image",
        &dir.to_string_lossy(),
    ]);
    assert_eq!(code, 1, "{err}");
}

// ---------------------------------------------------------------------------
// hostile caller text never reaches a diagnostic or stdout raw

#[test]
fn hostile_text_in_identity_diagnostics_is_escaped() {
    let missing = format!("/no/such/{HOSTILE}.json");
    let (code, out, err) = exec(&[
        "bedrock-resolve",
        "identity",
        "--provider",
        "openai",
        "--mappings",
        &missing,
    ]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert_clean("identity stderr", &err);
    assert!(err.contains("\\x1b"), "{err}");
    // Hostile values are only compared, never echoed.
    let (code, out, err) = exec(&[
        "bedrock-resolve",
        "identity",
        "--provider",
        HOSTILE,
        "--source",
        HOSTILE,
        "--target",
        HOSTILE,
    ]);
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (0, "{\"same_model\": false}\n", "")
    );
}

#[test]
fn hostile_text_in_image_diagnostics_and_payloads_is_escaped() {
    let hostile_path = format!("{HOSTILE}.bmp");
    for emit in ["format", "mime", "data-url"] {
        let (code, out, err) = exec(&[
            "bedrock-resolve",
            "image",
            "--emit",
            emit,
            "--image",
            &hostile_path,
        ]);
        assert_eq!((code, out.as_str()), (1, ""), "{emit}");
        assert_clean(emit, &err);
        assert!(err.contains("unsupported image type 'bmp'"), "{err}");
    }
    let (code, _, err) = exec(&[
        "bedrock-resolve",
        "image",
        "--emit",
        "responses",
        "--prompt",
        "x",
        "--detail",
        HOSTILE,
    ]);
    assert_eq!(code, 1);
    assert_clean("detail", &err);
    let missing = format!("/no/such/{HOSTILE}.png");
    let (code, _, err) = exec(&[
        "bedrock-resolve",
        "image",
        "--emit",
        "chat",
        "--prompt",
        "x",
        "--image",
        &missing,
    ]);
    assert_eq!(code, 1);
    assert_clean("missing image", &err);
    // A hostile prompt is the payload: JSON escapes it and no line is forged.
    for emit in ["converse", "responses", "chat"] {
        let (code, out, err) = exec(&[
            "bedrock-resolve",
            "image",
            "--emit",
            emit,
            "--prompt",
            HOSTILE,
        ]);
        assert_eq!((code, err.as_str()), (0, ""), "{emit}");
        assert_clean(emit, &out);
        assert!(
            out.contains("\\u001b") && out.contains("\\u202e") && out.contains("\\r\\n"),
            "{out}"
        );
        assert!(json(&out).to_compact_string().contains("FORGED"));
    }
}

#[test]
fn hostile_text_in_pricing_output_is_escaped() {
    let models = format!("{HOSTILE},anthropic.claude-opus-5-5,openai.gpt-5.6-{SECRET}");
    let region = format!("eu\u{1b}[2J-{HOSTILE}");
    let (code, out, err) = exec(&[
        "bedrock-resolve",
        "pricing",
        "--region",
        &region,
        "--models",
        &models,
    ]);
    assert_eq!((code, err.as_str()), (0, ""));
    // The secret here is a model-id suffix the caller typed, not a credential
    // of this process; only the control characters are the subject.
    for line in out.lines() {
        assert!(!line.chars().any(is_unsafe), "unsafe in {line:?}");
        assert!(!line.trim_start().starts_with("FORGED"), "{line:?}");
    }
    assert!(out.contains("\\\\x1b") || out.contains("\\x1b"), "{out}");
    // The shown keys round-trip as JSON.
    let Json::Object(members) = json(&out) else {
        panic!("an object")
    };
    assert_eq!(members.len(), 3);
    let (code, _, err) = exec(&[
        "bedrock-resolve",
        "pricing",
        "--region",
        "r",
        "--models",
        "m",
        "--price-list",
        &format!("/no/{HOSTILE}"),
    ]);
    assert_eq!(code, 1);
    assert_clean("price list", &err);
}

#[test]
fn hostile_text_in_source_model_output_is_escaped_and_secrets_never_printed() {
    let dir = TempDir::new("hostile-source");
    let env = write_env(
        &dir,
        &format!("OPENAI_API_KEY={SECRET}\nANTHROPIC_API_KEY=other-{SECRET}\n"),
    );
    // Hostile ids in the catalog and a plan id that embeds the secret.
    let catalog_text = Json::object(vec![(
        "data",
        Json::Array(vec![
            Json::object(vec![("id", Json::string(format!("gpt-5.4-{HOSTILE}")))]),
            Json::object(vec![("id", Json::string(format!("gpt-5.4-{SECRET}")))]),
            Json::object(vec![("id", Json::string("gpt-5.4-2026-03-05"))]),
        ]),
    )])
    .to_compact_string();
    let catalog = dir.file("catalog.json", &catalog_text);

    for plan in [
        format!("gpt-9-{HOSTILE}"),
        format!("gpt-9-{SECRET}"),
        "gpt-5.4".to_string(),
        format!("gpt-5.4-{HOSTILE}"),
    ] {
        let (code, out, err) = exec(&[
            "bedrock-resolve",
            "source-model",
            &env,
            "--catalog",
            &catalog,
            "--plan-model-id",
            &plan,
            "--source-provider",
            "openai",
        ]);
        assert!(matches!(code, 0 | 3), "{plan}: {code}");
        assert_clean("stdout", &out);
        assert_clean("stderr", &err);
        assert!(out.lines().count() == 1, "one line: {out:?}");
    }
    // A hostile id is never offered as the resolved model.
    let hostile_only = dir.file(
        "hostile.json",
        &Json::object(vec![(
            "data",
            Json::Array(vec![Json::object(vec![(
                "id",
                Json::string(format!("gpt-5.4-{HOSTILE}")),
            )])]),
        )])
        .to_compact_string(),
    );
    let exact_plan = format!("gpt-5.4-{HOSTILE}");
    let (code, out, _) = exec(&[
        "bedrock-resolve",
        "source-model",
        &env,
        "--catalog",
        &hostile_only,
        "--plan-model-id",
        &exact_plan,
        "--source-provider",
        "openai",
    ]);
    assert_eq!(code, 3, "{out}");
    assert!(
        out.contains("cannot be shown safely") && !out.contains("resolved_id"),
        "{out}"
    );
    // The same for a resolved id that is, or contains, the secret.
    let secret_plan = format!("gpt-5.4-{SECRET}");
    let (code, out, _) = exec(&[
        "bedrock-resolve",
        "source-model",
        &env,
        "--catalog",
        &catalog,
        "--plan-model-id",
        &secret_plan,
        "--source-provider",
        "openai",
    ]);
    assert_eq!(code, 3, "{out}");
    assert_clean("secret answer", &out);
}

#[test]
fn hostile_paths_in_source_model_diagnostics_are_escaped_and_redacted() {
    let dir = TempDir::new("hostile-paths");
    let env = write_env(
        &dir,
        &format!("OPENAI_API_KEY={SECRET}\nANTHROPIC_API_KEY=other-{SECRET}\n"),
    );
    let hostile_env = format!("/no/such/{HOSTILE}");
    let (code, out, err) = exec(&[
        "bedrock-resolve",
        "source-model",
        &hostile_env,
        "--plan-model-id",
        "x",
    ]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert_clean("env path", &err);
    let secret_catalog = format!("/no/such/{SECRET}.json");
    let (code, out, _) = exec(&[
        "bedrock-resolve",
        "source-model",
        &env,
        "--catalog",
        &secret_catalog,
        "--plan-model-id",
        "x",
        "--source-provider",
        "openai",
    ]);
    assert_eq!(code, 3);
    assert!(out.contains("***"), "{out}");
    assert_clean("secret path", &out);
}

#[test]
fn hostile_text_in_a_malformed_catalog_never_reaches_the_answer() {
    let dir = TempDir::new("hostile-catalog");
    let env = write_env(&dir, &format!("OPENAI_API_KEY={SECRET}\n"));
    let catalog = dir.file(
        "c.json",
        &format!("{{\"data\": \"{HOSTILE}\"}}").replace('\r', " "),
    );
    let (code, out, err) = exec(&[
        "bedrock-resolve",
        "source-model",
        &env,
        "--catalog",
        &catalog,
        "--plan-model-id",
        "x",
        "--source-provider",
        "openai",
    ]);
    assert_eq!((code, err.as_str()), (3, ""));
    assert_clean("answer", &out);
    assert!(!out.contains("FORGED"), "{out}");
}

// ---------------------------------------------------------------------------
// routing

#[test]
fn the_commands_are_registered_once_with_the_canonical_router_settings() {
    let ours: Vec<_> = COMMANDS
        .iter()
        .filter(|spec| spec.path.first() == Some(&"bedrock-resolve"))
        .collect();
    let names: Vec<_> = ours.iter().map(|spec| spec.path[1]).collect();
    assert_eq!(names, ["identity", "image", "pricing", "source-model"]);
    for spec in ours {
        assert!(spec.strict_values, "{:?}", spec.path);
        assert!(!spec.passthrough, "{:?}", spec.path);
        assert!(spec.flags.is_empty(), "{:?}", spec.path);
        assert!(!spec.value_flags.is_empty(), "{:?}", spec.path);
    }
    assert_eq!(
        COMMANDS
            .iter()
            .filter(|spec| spec.path == ["bedrock-resolve", "pricing"])
            .count(),
        1
    );
    // Every command is in the help text.
    let (_, help, _) = exec(&["help"]);
    for name in ["identity", "image", "pricing", "source-model"] {
        assert!(
            help.contains(&format!("toolkit bedrock-resolve {name}")),
            "{name}"
        );
    }
}

#[test]
fn value_flags_are_strict_and_unknown_flags_are_usage_errors() {
    for args in [
        &[
            "bedrock-resolve",
            "pricing",
            "--region",
            "",
            "--models",
            "m",
        ][..],
        &["bedrock-resolve", "pricing", "--region", "--models", "m"],
        &["bedrock-resolve", "pricing", "--region=-x", "--models", "m"],
        &["bedrock-resolve", "pricing", "--bogus", "x"],
        &["bedrock-resolve", "bogus"],
    ] {
        let (code, out, err) = exec(args);
        assert_eq!((code, out.as_str()), (2, ""), "{args:?}");
        assert!(err.starts_with("error: "), "{err}");
    }
}
