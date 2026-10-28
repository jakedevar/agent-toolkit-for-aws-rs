//! Snapshot payload parsing helpers (`di_snapshot_parsing.py`).
//!
//! Snapshots arrive as OTLP-style log records: metadata in top-level
//! `attributes` (`aws.di.*`), resource info in `resource.attributes`,
//! captures and stack under `body`, trace/span ids at the root. The parser
//! builds the compact summary the sample-snapshot tool returns for oversize
//! records. The two Logs Insights escapers are the injection guards for the
//! queries the snapshot tools build.

use std::collections::BTreeSet;

use toolkit_core::{parse_json, Json};

use super::pytext::{get_array, members, string_array};
use crate::dynamodb::pyfmt;

/// `re.escape(value).replace("/", "\\/")`: escapes a dynamic value for a
/// `/.../` Logs Insights regex. Python 3.7+ escapes only the regex special
/// characters, so letters, digits and `_` stay as they are.
#[must_use]
pub fn escape_logs_insights_regex(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '(' | ')' | '[' | ']' | '{' | '}' | '?' | '*' | '+' | '-' | '|' | '^' | '$' | '\\'
            | '.' | '&' | '~' | '#' | ' ' | '\t' | '\n' | '\r' | '\u{0b}' | '\u{0c}' => {
                escaped.push('\\');
                escaped.push(c);
            }
            '/' => escaped.push_str("\\/"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// Escapes a value for a double-quoted Logs Insights string literal.
///
/// Backslashes are escaped first (so the escapes added next are not
/// themselves re-escaped), then double quotes, so an embedded quote cannot
/// terminate the literal and inject caller-controlled query syntax. This is
/// distinct from [`escape_logs_insights_regex`], which escapes for `/.../`
/// regex context.
#[must_use]
pub fn escape_logs_insights_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The value when it is a JSON object, else `null` (reads as an empty dict).
fn dict(value: Option<&Json>) -> &Json {
    match value {
        // recorded-shape: the snapshot body a log event carries, parsed with the Python's tolerant rules (#4380); not a response member.
        Some(object @ Json::Object(_)) => object,
        _ => &Json::Null,
    }
}

fn clone_or_null(value: Option<&Json>) -> Json {
    value.cloned().unwrap_or(Json::Null)
}

fn is_true(value: Option<&Json>) -> bool {
    matches!(value, Some(Json::Bool(true)))
}

/// A compact preview of one snapshot `CapturedValue`.
#[must_use]
pub fn preview_captured_value(captured: &Json) -> Json {
    // recorded-shape: the snapshot body a log event carries (#4380); not a response member.
    let Json::Object(_) = captured else {
        return captured.clone();
    };

    let mut preview: Vec<(String, Json)> = Vec::new();
    match captured.get("type") {
        None | Some(Json::Null) => {}
        Some(Json::String(text)) if text.is_empty() => {}
        Some(value_type) => preview.push(("type".to_string(), value_type.clone())),
    }

    if is_true(captured.get("is_null")) {
        preview.push(("is_null".to_string(), Json::Bool(true)));
        return Json::Object(preview);
    }

    if let Some(reason) = captured.get("not_captured_reason") {
        preview.push(("not_captured_reason".to_string(), reason.clone()));
        return Json::Object(preview);
    }

    if let Some(value) = captured.get("value") {
        preview.push(("value".to_string(), value.clone()));
        if is_true(captured.get("truncated")) {
            preview.push(("truncated".to_string(), Json::Bool(true)));
        }
        if let Some(size) = captured.get("size") {
            preview.push(("size".to_string(), size.clone()));
        }
        return Json::Object(preview);
    }

    if let Some(fields) = captured.get("fields").and_then(members) {
        // Expand one level: primitive field values directly, nested
        // objects collapsed to just their type.
        let mut fields_preview: Vec<(String, Json)> = Vec::new();
        for (name, field) in fields {
            // recorded-shape: the snapshot body a log event carries (#4380); not a response member.
            let shown = if let Json::Object(_) = field {
                if is_true(field.get("is_null")) {
                    Json::Null
                } else if let Some(reason) = field.get("not_captured_reason") {
                    Json::string(format!("<{}>", pyfmt::text(reason)))
                } else if let Some(value) = field.get("value") {
                    value.clone()
                } else {
                    Json::string(format!(
                        "<{}>",
                        field
                            .get("type")
                            .map_or_else(|| "object".to_string(), pyfmt::text)
                    ))
                }
            } else {
                field.clone()
            };
            fields_preview.push((name.clone(), shown));
        }
        preview.push(("fields_preview".to_string(), Json::Object(fields_preview)));
        if let Some(size) = captured.get("size") {
            preview.push(("size".to_string(), size.clone()));
        }
        return Json::Object(preview);
    }

    // recorded-shape: the snapshot body a log event carries, parsed with the Python's tolerant rules (#4380); not a response member.
    if let Some(Json::Array(elements)) = captured.get("elements") {
        preview.push((
            "element_count".to_string(),
            Json::Int(i64::try_from(elements.len()).unwrap_or(i64::MAX)),
        ));
        if let Some(first) = elements.first() {
            preview.push(("first_element".to_string(), preview_captured_value(first)));
        }
        return Json::Object(preview);
    }

    // recorded-shape: the snapshot body a log event carries, parsed with the Python's tolerant rules (#4380); not a response member.
    if let Some(Json::Array(entries)) = captured.get("entries") {
        preview.push((
            "entry_count".to_string(),
            Json::Int(i64::try_from(entries.len()).unwrap_or(i64::MAX)),
        ));
        return Json::Object(preview);
    }

    if preview.is_empty() {
        captured.clone()
    } else {
        Json::Object(preview)
    }
}

fn names_and_previews(map: &Json) -> (Vec<String>, Json) {
    let pairs = members(map).unwrap_or(&[]);
    let names = pairs.iter().map(|(name, _)| name.clone()).collect();
    let previews = Json::Object(
        pairs
            .iter()
            .map(|(name, value)| (name.clone(), preview_captured_value(value)))
            .collect(),
    );
    (names, previews)
}

fn names_only(map: &Json) -> Json {
    let names: Vec<String> = members(map)
        .unwrap_or(&[])
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    string_array(&names)
}

fn throwable_summary(throwable: &Json) -> Json {
    let frames = match throwable.get("stacktrace") {
        // recorded-shape: the snapshot body a log event carries, parsed with the Python's tolerant rules (#4380); not a response member.
        Some(Json::Array(items)) => items.len(),
        _ => 0,
    };
    Json::object(vec![
        ("type", clone_or_null(throwable.get("type"))),
        ("message", clone_or_null(throwable.get("message"))),
        (
            "stacktrace_frame_count",
            Json::Int(i64::try_from(frames).unwrap_or(i64::MAX)),
        ),
    ])
}

/// Orders numeric line keys first (by value), the rest last (lexically).
/// The key is a tuple so the two kinds never compare across types.
fn line_key(text: &str) -> (u8, usize, String, String) {
    if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
        let trimmed = text.trim_start_matches('0');
        (0, trimmed.len(), trimmed.to_string(), text.to_string())
    } else {
        (1, 0, text.to_string(), String::new())
    }
}

fn some_or_null(entries: Vec<(String, Json)>) -> Json {
    if entries.is_empty() {
        Json::Null
    } else {
        Json::Object(entries)
    }
}

/// Extracts the key debugging fields from a raw CloudWatch Logs snapshot
/// result (an object with `@message` and `@timestamp`).
///
/// A `@message` that is not a JSON object is read as an empty snapshot, so
/// every field falls back to `null` / empty instead of failing.
#[must_use]
pub fn parse_snapshot_fields(result: &Json) -> Json {
    let snapshot_data = match result.get("@message") {
        Some(Json::String(message)) => match parse_json(message) {
            // recorded-shape: the snapshot body a log event carries, parsed with the Python's tolerant rules (#4380); not a response member.
            Ok(parsed @ Json::Object(_)) => parsed,
            _ => Json::Object(Vec::new()),
        },
        _ => Json::Object(Vec::new()),
    };

    let attributes = dict(snapshot_data.get("attributes"));
    let body = dict(snapshot_data.get("body"));

    let location = Json::object(vec![
        (
            "class_name",
            clone_or_null(attributes.get("aws.di.class_name")),
        ),
        (
            "method_name",
            clone_or_null(attributes.get("aws.di.method_name")),
        ),
        (
            "file_path",
            clone_or_null(attributes.get("aws.di.file_path")),
        ),
        (
            "code_unit",
            clone_or_null(attributes.get("aws.di.code_unit")),
        ),
        (
            "instrumentation_level",
            clone_or_null(attributes.get("aws.di.instrumentation_level")),
        ),
        (
            "instrumentation_type",
            clone_or_null(attributes.get("aws.di.instrumentation_type")),
        ),
    ]);

    let trace = Json::object(vec![
        ("traceId", clone_or_null(snapshot_data.get("traceId"))),
        ("spanId", clone_or_null(snapshot_data.get("spanId"))),
    ]);

    let stack = get_array(body, "stack");
    let captures = dict(body.get("captures"));
    let entry_capture = dict(captures.get("entry"));
    let return_capture = dict(captures.get("return"));
    let line_captures = dict(captures.get("lines"));

    let entry_arguments = dict(entry_capture.get("arguments"));
    let entry_locals = dict(entry_capture.get("locals"));
    let return_arguments = dict(return_capture.get("arguments"));
    let return_locals = dict(return_capture.get("locals"));
    let return_value = return_capture.get("return_value");
    let throwable = dict(return_capture.get("throwable"));

    let mut line_locals: Vec<(String, Json)> = Vec::new();
    let mut line_local_previews: Vec<(String, Json)> = Vec::new();
    let mut line_arguments: Vec<(String, Json)> = Vec::new();
    let mut line_argument_previews: Vec<(String, Json)> = Vec::new();
    let mut line_return_values: Vec<(String, Json)> = Vec::new();
    let mut line_throwables: Vec<(String, Json)> = Vec::new();
    for (line_number, line_capture) in members(line_captures).unwrap_or(&[]) {
        // recorded-shape: the snapshot body a log event carries (#4380); not a response member.
        if !matches!(line_capture, Json::Object(_)) {
            continue;
        }
        let locals_map = dict(line_capture.get("locals"));
        if members(locals_map).is_some_and(|pairs| !pairs.is_empty()) {
            let (_, previews) = names_and_previews(locals_map);
            line_locals.push((line_number.clone(), names_only(locals_map)));
            line_local_previews.push((line_number.clone(), previews));
        }

        let args_map = dict(line_capture.get("arguments"));
        if members(args_map).is_some_and(|pairs| !pairs.is_empty()) {
            let (_, previews) = names_and_previews(args_map);
            line_arguments.push((line_number.clone(), names_only(args_map)));
            line_argument_previews.push((line_number.clone(), previews));
        }

        if let Some(value) = line_capture
            .get("return_value")
            .filter(|v| **v != Json::Null)
        {
            line_return_values.push((line_number.clone(), preview_captured_value(value)));
        }

        // recorded-shape: the snapshot body a log event carries, parsed with the Python's tolerant rules (#4380); not a response member.
        if let Some(value @ Json::Object(pairs)) = line_capture.get("throwable") {
            if !pairs.is_empty() {
                line_throwables.push((line_number.clone(), throwable_summary(value)));
            }
        }
    }

    let stack_preview: Vec<Json> = stack
        .iter()
        .take(5)
        // recorded-shape: the snapshot body a log event carries (#4380); not a response member.
        .filter(|frame| matches!(frame, Json::Object(_)))
        .map(|frame| {
            Json::object(vec![
                ("file_path", clone_or_null(frame.get("file_path"))),
                ("function", clone_or_null(frame.get("function"))),
                ("line_number", clone_or_null(frame.get("line_number"))),
            ])
        })
        .collect();

    let all_line_numbers: BTreeSet<&str> = line_locals
        .iter()
        .chain(&line_arguments)
        .chain(&line_return_values)
        .chain(&line_throwables)
        .map(|(line, _)| line.as_str())
        .collect();
    let mut ordered: Vec<&str> = all_line_numbers.into_iter().collect();
    ordered.sort_by_key(|line| line_key(line));

    let (_, entry_argument_previews) = names_and_previews(entry_arguments);
    let (_, entry_local_previews) = names_and_previews(entry_locals);
    let (_, return_argument_previews) = names_and_previews(return_arguments);
    let (_, return_local_previews) = names_and_previews(return_locals);

    let throwable_present = members(throwable).is_some_and(|pairs| !pairs.is_empty());

    Json::object(vec![
        ("@timestamp", clone_or_null(result.get("@timestamp"))),
        (
            "snapshot_id",
            clone_or_null(attributes.get("aws.di.snapshot_id")),
        ),
        (
            "timeUnixNano",
            clone_or_null(snapshot_data.get("timeUnixNano")),
        ),
        (
            "duration_ms",
            clone_or_null(attributes.get("aws.di.duration_ms")),
        ),
        (
            "location_hash",
            clone_or_null(attributes.get("aws.di.location_hash")),
        ),
        ("location", location),
        ("trace", trace),
        ("stack_preview", Json::Array(stack_preview)),
        (
            "stack_frame_count",
            Json::Int(i64::try_from(stack.len()).unwrap_or(i64::MAX)),
        ),
        ("entry_argument_names", names_only(entry_arguments)),
        ("entry_arguments", entry_argument_previews),
        ("entry_local_names", names_only(entry_locals)),
        ("entry_locals", entry_local_previews),
        ("return_argument_names", names_only(return_arguments)),
        ("return_arguments", return_argument_previews),
        ("return_local_names", names_only(return_locals)),
        ("return_locals", return_local_previews),
        (
            "return_value",
            match return_value {
                Some(value) if *value != Json::Null => preview_captured_value(value),
                _ => Json::Null,
            },
        ),
        (
            "throwable",
            if throwable_present {
                throwable_summary(throwable)
            } else {
                Json::Null
            },
        ),
        (
            "line_numbers",
            Json::Array(ordered.iter().map(|line| Json::string(*line)).collect()),
        ),
        ("line_locals", Json::Object(line_locals)),
        ("line_local_previews", Json::Object(line_local_previews)),
        ("line_arguments", some_or_null(line_arguments)),
        (
            "line_argument_previews",
            some_or_null(line_argument_previews),
        ),
        ("line_return_values", some_or_null(line_return_values)),
        ("line_throwables", some_or_null(line_throwables)),
        ("raw_snapshot", snapshot_data.clone()),
    ])
}
