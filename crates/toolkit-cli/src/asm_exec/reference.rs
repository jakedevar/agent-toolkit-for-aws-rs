//! Reference syntax: finding `{{resolve:secretsmanager:...}}` in text, splitting
//! a reference into its parts, and `json.dumps` of a non-string JSON member.
//! (The `repr` of a string for the server-side script is the canonical
//! `manifests::py_repr_str`.)
//!
//! Nothing here touches the network or a secret value.

use toolkit_core::Json;

use crate::manifests::py_float_repr;

/// The text that opens a reference.
const OPEN: &[u8] = b"{{resolve:secretsmanager:";

/// Default field type (the only supported one).
pub const DEFAULT_FIELD_TYPE: &str = "SecretString";

/// Default version stage.
pub const DEFAULT_VERSION_STAGE: &str = "AWSCURRENT";

/// Why a substitution stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum SubstituteError<E> {
    /// The reference text between the delimiters is not valid UTF-8.
    NotUtf8,
    /// The caller's resolver refused a reference.
    Resolver(E),
    /// The output would exceed `usize::MAX` bytes.
    TooLarge,
}

/// Whether `input` contains at least one reference (the regex
/// `\{\{resolve:secretsmanager:([^}]+)\}\}` would match).
#[must_use]
pub fn contains_reference(input: &[u8]) -> bool {
    matches!(scan(input, 0), Scan::Found { .. })
}

enum Scan {
    /// A match: reference occupies `reference_start..reference_end`, the whole
    /// match `start..end`.
    Found {
        start: usize,
        reference_start: usize,
        reference_end: usize,
        end: usize,
    },
    None,
}

/// Leftmost match at or after `from`, with the regex semantics: after the
/// opener, one or more non-`}` bytes, then exactly `}}`.
fn scan(input: &[u8], from: usize) -> Scan {
    let mut start = from;
    // First `}` at or after the current candidate; it stays valid while later
    // candidates begin before it, so the whole scan is linear.
    let mut close: Option<usize> = None;
    loop {
        let Some(tail) = input.get(start..) else {
            return Scan::None;
        };
        let Some(relative) = tail.windows(OPEN.len()).position(|window| window == OPEN) else {
            return Scan::None;
        };
        let Some(open_at) = start.checked_add(relative) else {
            return Scan::None;
        };
        let Some(reference_start) = open_at.checked_add(OPEN.len()) else {
            return Scan::None;
        };
        if close.is_none_or(|position| position < reference_start) {
            close = input
                .get(reference_start..)
                .and_then(|rest| rest.iter().position(|byte| *byte == b'}'))
                .and_then(|offset| reference_start.checked_add(offset));
        }
        let Some(reference_end) = close else {
            // No `}` anywhere after this opener, so none after a later one.
            return Scan::None;
        };
        let end = reference_end.checked_add(2);
        let closes = input
            .get(reference_end..)
            .is_some_and(|rest| rest.starts_with(b"}}"));
        if reference_end > reference_start && closes {
            if let Some(end) = end {
                return Scan::Found {
                    start: open_at,
                    reference_start,
                    reference_end,
                    end,
                };
            }
        }
        let Some(next) = open_at.checked_add(1) else {
            return Scan::None;
        };
        start = next;
    }
}

/// Replace every reference in `input` with what `resolve` returns for it,
/// in one left-to-right pass: a replacement is never scanned again.
///
/// # Errors
/// [`SubstituteError::NotUtf8`] when a reference is not UTF-8,
/// [`SubstituteError::Resolver`] with the first resolver failure (later
/// references are not resolved), [`SubstituteError::TooLarge`] when the output
/// length overflows.
pub fn substitute<E>(
    input: &[u8],
    resolve: &mut dyn FnMut(&str) -> Result<Vec<u8>, E>,
) -> Result<Vec<u8>, SubstituteError<E>> {
    let mut out: Vec<u8> = Vec::new();
    let mut copied = 0_usize;
    let mut from = 0_usize;
    while let Scan::Found {
        start,
        reference_start,
        reference_end,
        end,
    } = scan(input, from)
    {
        let reference = input
            .get(reference_start..reference_end)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .ok_or(SubstituteError::NotUtf8)?;
        let value = resolve(reference).map_err(SubstituteError::Resolver)?;
        let literal = input.get(copied..start).unwrap_or_default();
        out.len()
            .checked_add(literal.len())
            .and_then(|len| len.checked_add(value.len()))
            .ok_or(SubstituteError::TooLarge)?;
        out.extend_from_slice(literal);
        out.extend_from_slice(&value);
        copied = end;
        from = end;
    }
    out.extend_from_slice(input.get(copied..).unwrap_or_default());
    Ok(out)
}

/// A reference split into its parts (`resolve_one` in the Python).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// Secret name or ARN.
    pub secret_id: String,
    /// Field type; only [`DEFAULT_FIELD_TYPE`] is supported.
    pub field_type: String,
    /// JSON key to extract; `None` when absent or empty (the Python tests
    /// `if json_key:`).
    pub json_key: Option<String>,
    /// Version stage (empty when written as an empty segment).
    pub version_stage: String,
}

impl Reference {
    /// Split `secret-id[:field-type[:json-key[:version-stage]]]`. An ARN
    /// (`arn:` prefix with at least seven colon-separated segments) keeps its
    /// first seven segments as the secret id; segments after the third
    /// optional one are ignored, as in the Python.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let (secret_id, rest): (String, Vec<&str>) = if text.starts_with("arn:") {
            let parts: Vec<&str> = text.split(':').collect();
            if parts.len() >= 7 {
                (parts[..7].join(":"), parts[7..].to_vec())
            } else {
                (text.to_string(), Vec::new())
            }
        } else {
            let mut parts = text.splitn(4, ':');
            let id = parts.next().unwrap_or_default().to_string();
            (id, parts.collect())
        };
        let field_type = rest.first().copied().unwrap_or(DEFAULT_FIELD_TYPE);
        let json_key = rest
            .get(1)
            .copied()
            .filter(|key| !key.is_empty())
            .map(str::to_string);
        let version_stage = rest.get(2).copied().unwrap_or(DEFAULT_VERSION_STAGE);
        Self {
            secret_id,
            field_type: field_type.to_string(),
            json_key,
            version_stage: version_stage.to_string(),
        }
    }

    /// Region named by an ARN secret id (fourth segment), if any and non-empty.
    #[must_use]
    pub fn arn_region(&self) -> Option<&str> {
        if !self.secret_id.starts_with("arn:") {
            return None;
        }
        self.secret_id
            .split(':')
            .nth(3)
            .filter(|region| !region.is_empty())
    }
}

/// `json.dumps(value)` as Python writes it: `, ` and `: ` separators, ASCII
/// only, floats in `repr` form. Used for a JSON member that is not a string.
#[must_use]
pub fn python_json_dumps(value: &Json) -> String {
    let mut out = String::new();
    dump(value, &mut out);
    out
}

fn dump(value: &Json, out: &mut String) {
    use std::fmt::Write as _;
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Int(number) => {
            let _ = write!(out, "{number}");
        }
        Json::Float(number) => out.push_str(&py_float_repr(*number)),
        Json::String(text) => dump_string(text, out),
        Json::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                dump(item, out);
            }
            out.push(']');
        }
        Json::Object(pairs) => {
            out.push('{');
            for (index, (key, item)) in pairs.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                dump_string(key, out);
                out.push_str(": ");
                dump(item, out);
            }
            out.push('}');
        }
    }
}

fn dump_string(text: &str, out: &mut String) {
    use std::fmt::Write as _;
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(ch),
            c => {
                let mut units = [0_u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests;
