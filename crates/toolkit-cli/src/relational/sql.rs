//! Safe SQL construction for Aurora DSQL (port of `aurora-dsql/scripts/safe_query.py`).
//!
//! Raw SQL strings are built only from [`Safe`] parts. A [`Safe`] can be made
//! only by a validator in this module ([`allow`], [`keyword`], [`regex`],
//! [`ident`], [`integer_text`], [`integer`], [`literal`]), so
//! `build("... {x} ...", [("x", user_input)])` cannot compile: the type system
//! carries what the Python module enforced with an `isinstance` check at run
//! time. Format validation does not prove authorization; authorize the caller
//! separately. Prefer driver parameter binding in application code.
//!
//! Intentional differences from the Python module are listed in
//! `docs/migration/relational.md`: patterns are ASCII only (Python's `\d` and
//! `re.IGNORECASE` also matched non-ASCII digits and letters), error messages
//! quote the offending value with the shared diagnostic escaping instead of
//! Python's `repr`, and a malformed template is always an [`UnsafeSqlError`].

use std::collections::HashSet;
use std::fmt;
use std::hash::{BuildHasher, Hasher, RandomState};

use toolkit_core::quote_untrusted;

use crate::manifests::py_repr_str;

/// A value failed validation. Never catch it and fall back: fix the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnsafeSqlError(String);

impl UnsafeSqlError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for UnsafeSqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UnsafeSqlError {}

pub(crate) type SqlResult<T> = Result<T, UnsafeSqlError>;

/// A value that has passed validation and is safe to interpolate.
///
/// The field is private: there is no way to wrap a raw string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Safe(String);

impl Safe {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Safe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A strict-format pattern, matched against the whole value (`re.fullmatch`).
#[derive(Clone, Copy)]
pub(crate) struct Pattern {
    /// The regular expression the Python module used, shown in errors.
    source: &'static str,
    matches: fn(&str) -> bool,
}

impl Pattern {
    /// A pattern from its source text and its whole-value matcher.
    pub(crate) const fn new(source: &'static str, matches: fn(&str) -> bool) -> Self {
        Self { source, matches }
    }

    pub(crate) fn source(&self) -> &'static str {
        self.source
    }

    pub(crate) fn is_match(&self, value: &str) -> bool {
        (self.matches)(value)
    }
}

fn all_bytes(value: &str, accept: impl Fn(u8) -> bool) -> bool {
    value.bytes().all(accept)
}

fn is_tenant_slug(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && all_bytes(value, |b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
        })
}

fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, b)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

fn is_int(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    (1..=19).contains(&digits.len()) && all_bytes(digits, |b| b.is_ascii_digit())
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if !(digits(0..4) && digits(5..7) && digits(8..10)) {
        return false;
    }
    let month = (bytes[5] - b'0') * 10 + (bytes[6] - b'0');
    let day = (bytes[8] - b'0') * 10 + (bytes[9] - b'0');
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

fn is_ident(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=63).contains(&bytes.len())
        && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        && all_bytes(&value[1..], |b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `[a-z0-9-]{1,64}`
pub(crate) const TENANT_SLUG: Pattern = Pattern {
    source: "[a-z0-9-]{1,64}",
    matches: is_tenant_slug,
};
/// A UUID in the canonical 8-4-4-4-12 form, hex digits of either case.
pub(crate) const UUID: Pattern = Pattern {
    source: "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
    matches: is_uuid,
};
/// `-?[0-9]{1,19}`
pub(crate) const INT: Pattern = Pattern {
    source: "-?[0-9]{1,19}",
    matches: is_int,
};
/// `YYYY-MM-DD` with month 01-12 and day 01-31.
pub(crate) const ISO_DATE: Pattern = Pattern {
    source: r"\d{4}-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])",
    matches: is_iso_date,
};

/// A caller value inside an SQL safety error: redacted whole, bounded, then
/// quoted (#4941).
fn shown(value: &str) -> String {
    quote_untrusted(&crate::safe_text::shown_value(value))
}

/// Allowlist-validates and emits a single-quoted string literal.
pub(crate) fn allow(value: &str, allowed: &[&str], label: &str) -> SqlResult<Safe> {
    if !allowed.contains(&value) {
        return Err(UnsafeSqlError::new(format!(
            "{label} not in allowlist: {}",
            shown(value)
        )));
    }
    // Allowlisted values come from developer-controlled sets; the escape is
    // belt-and-braces in case a quote is put in the set.
    Ok(Safe(format!("'{}'", value.replace('\'', "''"))))
}

/// Allowlist-validates a SQL keyword and emits it unquoted (`ASC`, `DESC`).
pub(crate) fn keyword(value: &str, allowed: &[&str], label: &str) -> SqlResult<Safe> {
    if !allowed.contains(&value) {
        return Err(UnsafeSqlError::new(format!(
            "{label} not in allowlist: {}",
            shown(value)
        )));
    }
    Ok(Safe(value.to_string()))
}

/// Validates the whole value against `pattern` and emits a single-quoted
/// literal. Values with a single quote or a backslash are rejected: free text
/// belongs in [`literal`].
pub(crate) fn regex(value: &str, pattern: &Pattern, label: &str) -> SqlResult<Safe> {
    if !pattern.is_match(value) {
        return Err(UnsafeSqlError::new(format!(
            "{label} failed pattern {}: {}",
            py_repr_str(pattern.source()),
            shown(value)
        )));
    }
    if value.contains('\'') {
        return Err(UnsafeSqlError::new(format!(
            "{label} contains a single quote; use literal() for free text: {}",
            shown(value)
        )));
    }
    if value.contains('\\') {
        return Err(UnsafeSqlError::new(format!(
            "{label} contains a backslash; use literal() for values needing special \
             characters: {}",
            shown(value)
        )));
    }
    Ok(Safe(format!("'{value}'")))
}

/// Validates a SQL identifier (table or column) and emits it double-quoted.
pub(crate) fn ident(name: &str) -> SqlResult<Safe> {
    if !is_ident(name) {
        return Err(UnsafeSqlError::new(format!(
            "invalid identifier: {}",
            shown(name)
        )));
    }
    Ok(Safe(format!("\"{name}\"")))
}

/// Validates an integer given as text (`-?[0-9]{1,19}`).
pub(crate) fn integer_text(value: &str) -> SqlResult<Safe> {
    if is_int(value) {
        Ok(Safe(value.to_string()))
    } else {
        Err(UnsafeSqlError::new(format!(
            "invalid integer: {}",
            shown(value)
        )))
    }
}

/// An integer value. (Python's `integer(True)` rejection is a type error here.)
pub(crate) fn integer(value: i64) -> Safe {
    Safe(value.to_string())
}

/// Tags tried before giving up on a value that contains them all.
const LITERAL_TAG_ATTEMPTS: usize = 8;

fn random_tag() -> String {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u8(0);
    // Four bytes of the per-process random hash, like `secrets.token_hex(4)`.
    format!("dq_{:08x}", hasher.finish() & 0xffff_ffff)
}

/// Emits free text as a `PostgreSQL` dollar-quoted literal.
pub(crate) fn literal(value: &str) -> SqlResult<Safe> {
    literal_with(value, random_tag)
}

/// [`literal`] with an injectable tag source, so a collision is testable.
pub(crate) fn literal_with(value: &str, mut next_tag: impl FnMut() -> String) -> SqlResult<Safe> {
    for _ in 0..LITERAL_TAG_ATTEMPTS {
        let boundary = format!("${}$", next_tag());
        if !value.contains(&boundary) {
            return Ok(Safe(format!("{boundary}{value}{boundary}")));
        }
    }
    // Eight random tag collisions imply adversarial input.
    Err(UnsafeSqlError::new(
        "could not generate a unique dollar-quote tag",
    ))
}

/// One piece of a parsed template.
enum Piece {
    Text(String),
    Field(String),
}

/// Parses `str.format` markup: `{{` and `}}` are literal braces, `{name}` is a
/// field. Positional fields, conversions and format specs are rejected, as the
/// Python module rejected them, so a `Safe` value is interpolated verbatim.
fn parse_template(template: &str) -> SqlResult<Vec<Piece>> {
    let chars: Vec<char> = template.chars().collect();
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        index += 1;
        match c {
            '{' if chars.get(index) == Some(&'{') => {
                text.push('{');
                index += 1;
            }
            '}' if chars.get(index) == Some(&'}') => {
                text.push('}');
                index += 1;
            }
            '}' => {
                return Err(UnsafeSqlError::new(
                    "Single '}' encountered in format string",
                ));
            }
            '{' => {
                let mut depth = 1_usize;
                let mut inner = String::new();
                while index < chars.len() {
                    let d = chars[index];
                    index += 1;
                    match d {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    inner.push(d);
                }
                if depth != 0 {
                    return Err(UnsafeSqlError::new("expected '}' before end of string"));
                }
                if !text.is_empty() {
                    pieces.push(Piece::Text(std::mem::take(&mut text)));
                }
                pieces.push(Piece::Field(field_name(&inner)?));
            }
            other => text.push(other),
        }
    }
    if !text.is_empty() {
        pieces.push(Piece::Text(text));
    }
    Ok(pieces)
}

/// The name of one `{...}` field, after the positional, conversion and format
/// spec rejections (checked in this order, like the Python module).
fn field_name(inner: &str) -> SqlResult<String> {
    // The name ends at the first `!` or `:` outside square brackets.
    let mut in_brackets = false;
    let mut end = inner.len();
    for (position, c) in inner.char_indices() {
        match c {
            '[' => in_brackets = true,
            ']' => in_brackets = false,
            '!' | ':' if !in_brackets => {
                end = position;
                break;
            }
            _ => {}
        }
    }
    let name = &inner[..end];
    let rest = &inner[end..];
    if name.is_empty() || name.bytes().all(|b| b.is_ascii_digit()) {
        return Err(UnsafeSqlError::new(format!(
            "template contains a positional placeholder {{{name}}}; use named placeholders \
             like {{name}}"
        )));
    }
    if let Some(after) = rest.strip_prefix('!') {
        let conversion = after.split(':').next().unwrap_or("");
        return Err(UnsafeSqlError::new(format!(
            "placeholder {{{name}!{conversion}}} uses a conversion flag; Safe values must be \
             interpolated without conversion"
        )));
    }
    if let Some(spec) = rest.strip_prefix(':') {
        if !spec.is_empty() {
            return Err(UnsafeSqlError::new(format!(
                "placeholder {{{name}:{spec}}} uses a format spec; Safe values must be \
                 interpolated without formatting"
            )));
        }
    }
    Ok(name.to_string())
}

fn py_list(names: &[&str]) -> String {
    let quoted: Vec<String> = names.iter().map(|name| py_repr_str(name)).collect();
    format!("[{}]", quoted.join(", "))
}

/// Substitutes validated parts into a SQL template of `{name}` placeholders.
///
/// A template/part mismatch is rejected: a placeholder without a part, or a
/// part the template never uses (which would silently drop, for example, a
/// tenant filter).
pub(crate) fn build(template: &str, parts: &[(&str, &Safe)]) -> SqlResult<String> {
    let mut provided: Vec<&str> = parts.iter().map(|(name, _)| *name).collect();
    let unique: HashSet<&str> = provided.iter().copied().collect();
    if unique.len() != provided.len() {
        return Err(UnsafeSqlError::new("duplicate part name"));
    }
    let pieces = parse_template(template)?;
    let mut expected: Vec<&str> = pieces
        .iter()
        .filter_map(|piece| match piece {
            Piece::Field(name) => Some(name.as_str()),
            Piece::Text(_) => None,
        })
        .collect();
    expected.sort_unstable();
    expected.dedup();
    provided.sort_unstable();
    if expected != provided {
        let missing: Vec<&str> = expected
            .iter()
            .copied()
            .filter(|name| !provided.contains(name))
            .collect();
        let extra: Vec<&str> = provided
            .iter()
            .copied()
            .filter(|name| !expected.contains(name))
            .collect();
        return Err(UnsafeSqlError::new(format!(
            "template/kwargs mismatch: missing {}, extra {}",
            py_list(&missing),
            py_list(&extra)
        )));
    }
    let mut sql = String::new();
    for piece in &pieces {
        match piece {
            Piece::Text(text) => sql.push_str(text),
            Piece::Field(name) => {
                let part = parts
                    .iter()
                    .find(|(candidate, _)| candidate == name)
                    .map(|(_, safe)| *safe);
                // The sets are equal, so every field has a part.
                if let Some(safe) = part {
                    sql.push_str(safe.as_str());
                }
            }
        }
    }
    Ok(sql)
}

/// The smoke test of `python safe_query.py`: every validator and `build`.
pub(crate) fn selftest() -> Result<(), String> {
    fn check(condition: bool, message: &str) -> Result<(), String> {
        if condition {
            Ok(())
        } else {
            Err(message.to_string())
        }
    }
    #[allow(clippy::needless_pass_by_value)]
    fn expect_unsafe<T>(result: SqlResult<T>, what: &str) -> Result<(), String> {
        match result {
            Err(_) => Ok(()),
            Ok(_) => Err(format!("expected UnsafeSQLError from {what}")),
        }
    }
    fn text(result: SqlResult<Safe>) -> Result<String, String> {
        result
            .map(|safe| safe.as_str().to_string())
            .map_err(|error| error.to_string())
    }

    check(
        text(allow("tenant-1", &["tenant-1"], "value"))? == "'tenant-1'",
        "allow",
    )?;
    check(
        text(keyword("ASC", &["ASC", "DESC"], "keyword"))? == "ASC",
        "keyword",
    )?;
    check(
        text(regex("a-1", &TENANT_SLUG, "value"))? == "'a-1'",
        "regex",
    )?;
    check(text(ident("entities"))? == "\"entities\"", "ident")?;
    check(
        text(regex(
            "0F8B2C1A-0000-4000-8000-0123456789ab",
            &UUID,
            "value",
        ))? == "'0F8B2C1A-0000-4000-8000-0123456789ab'",
        "uuid",
    )?;
    check(
        text(regex("2026-10-27", &ISO_DATE, "value"))? == "'2026-10-27'",
        "date",
    )?;
    check(text(regex("-12", &INT, "value"))? == "'-12'", "int pattern")?;
    check(integer(42).as_str() == "42", "integer")?;
    check(text(integer_text("-7"))? == "-7", "integer neg")?;
    let lit = text(literal("o'reilly"))?;
    check(
        lit.starts_with("$dq_") && lit.contains("o'reilly"),
        "literal",
    )?;

    let tid = regex("acme", &TENANT_SLUG, "value").map_err(|error| error.to_string())?;
    let table = ident("entities").map_err(|error| error.to_string())?;
    let sql = build(
        "SELECT * FROM {t} WHERE tenant_id = {tid}",
        &[("t", &table), ("tid", &tid)],
    )
    .map_err(|error| error.to_string())?;
    check(
        sql == "SELECT * FROM \"entities\" WHERE tenant_id = 'acme'",
        "build",
    )?;

    let permissive = Pattern::new(".+", |value| !value.is_empty() && !value.contains('\n'));
    expect_unsafe(allow("evil", &["tenant-1"], "value"), "allow")?;
    expect_unsafe(keyword("DROP", &["ASC", "DESC"], "keyword"), "keyword")?;
    expect_unsafe(regex("'; DROP TABLE t; --", &TENANT_SLUG, "value"), "regex")?;
    expect_unsafe(ident("x\" OR 1=1 --"), "ident")?;
    expect_unsafe(regex("2026-13-01", &ISO_DATE, "value"), "date month")?;
    expect_unsafe(regex("not-a-uuid", &UUID, "value"), "uuid")?;
    expect_unsafe(regex("1.5", &INT, "value"), "int pattern")?;
    expect_unsafe(integer_text("1; DROP"), "integer")?;
    for hostile in ["x' OR 1=1 --", "it's", "'", "abc\\"] {
        expect_unsafe(regex(hostile, &permissive, "value"), "regex")?;
    }
    let col = ident("col").map_err(|error| error.to_string())?;
    let extra = ident("extra").map_err(|error| error.to_string())?;
    expect_unsafe(
        build("SELECT {x}", &[("x", &col), ("y", &extra)]),
        "build extra",
    )?;
    expect_unsafe(
        build("SELECT {x} FROM {y}", &[("x", &col)]),
        "build missing",
    )?;
    expect_unsafe(build("SELECT {x!r}", &[("x", &col)]), "build conversion")?;
    expect_unsafe(build("SELECT {x:>30}", &[("x", &col)]), "build spec")?;
    expect_unsafe(build("SELECT {}", &[("x", &col)]), "build positional")?;
    expect_unsafe(build("SELECT {0}", &[("x", &col)]), "build indexed")?;
    Ok(())
}
