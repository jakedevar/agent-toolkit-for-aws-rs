//! JSON Schema (draft 2020-12) evaluation for the vendored Agent Plugins
//! schemas, built on the `jsonschema` crate.
//!
//! `tools/validate_spec.py` used the Python `jsonschema` package. This module
//! is the offline Rust counterpart and keeps three promises:
//!
//! - **Local and bounded.** Only references inside the schema document are
//!   resolved; nothing is retrieved (the crate's HTTP and file resolvers are
//!   not compiled in and the validator is built `offline`). Regular
//!   expressions run on `fancy-regex` with an explicit backtracking limit.
//! - **Fail closed.** [`compile`] rejects, before any instance is looked at,
//!   references that are not local, do not resolve, do not point at a schema or
//!   form a cycle, plus patterns the engines would accept but Python's `re`
//!   rejects (a quantifier on an anchor). [`Schema::validate`] turns a regex
//!   match the engine could not finish into an `Err`, never into a match, a
//!   non-match or a finding, and the crate re-runs `not`/`oneOf`/`anyOf` under
//!   both possible outcomes so a combinator cannot flip it into a pass.
//! - **Published messages.** Findings keep the wording and ordering of the
//!   Python tool (`'x' is not of type 'string'`, `Additional properties are
//!   not allowed ('y' was unexpected)`, ...), rebuilt from the typed error
//!   kind rather than the crate's own sentences, with instance values rendered
//!   as Python `repr` of the original document so key order is preserved.

use std::collections::BTreeSet;

use jsonschema::error::{TypeKind, ValidationErrorKind};
use jsonschema::{PatternOptions, ValidationError, Validator};
use serde_json::Value;
use toolkit_core::Json;

use crate::manifests::{py_repr, py_repr_str};
use crate::safe_text::{safe_quoted, safe_text};

/// Steps the backtracking regex VM may take for one match before the match is
/// reported as unfinished. The vendored patterns need well under a thousand;
/// the bound keeps hostile patterns or inputs cheap.
const BACKTRACK_LIMIT: usize = 100_000;

/// Deepest schema nesting and longest reference chain accepted.
const MAX_SCHEMA_DEPTH: usize = 64;

/// One step of an instance path. `Index` sorts before `Key`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Segment {
    Index(usize),
    Key(String),
}

/// `$`, `$.key` and `$[0]` style path of a finding; keys are escaped with
/// [`safe_text`] so a hostile property name cannot forge a report line.
pub(crate) fn escaped_path(path: &[Segment]) -> String {
    use std::fmt::Write as _;

    let mut out = String::from("$");
    for segment in path {
        match segment {
            Segment::Index(index) => {
                let _ = write!(out, "[{index}]");
            }
            Segment::Key(key) => {
                out.push('.');
                out.push_str(&safe_text(key));
            }
        }
    }
    out
}

/// One schema violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Issue {
    pub(crate) path: Vec<Segment>,
    pub(crate) message: String,
}

impl Issue {
    /// `a/b/0/c`, or `<root>` for the document itself.
    pub(crate) fn path_text(&self) -> String {
        if self.path.is_empty() {
            return "<root>".to_string();
        }
        let parts: Vec<String> = self
            .path
            .iter()
            .map(|segment| match segment {
                Segment::Index(index) => index.to_string(),
                Segment::Key(key) => key.clone(),
            })
            .collect();
        parts.join("/")
    }
}

/// A compiled schema together with the document it came from.
pub(crate) struct Schema {
    root: Json,
    validator: Validator,
}

/// Checks `root` and builds the validator.
///
/// # Errors
///
/// Returns a description when the schema uses a non-local, dangling,
/// non-schema or recursive reference, a nested `$id`, a pattern that is not a
/// valid regular expression, or anything else the engine cannot compile.
pub(crate) fn compile(root: Json) -> Result<Schema, String> {
    check_schema(&root)?;
    let value = to_value(&root)?;
    let validator = jsonschema::draft202012::options()
        .offline()
        .with_pattern_options(PatternOptions::fancy_regex().backtrack_limit(BACKTRACK_LIMIT))
        .build(&value)
        .map_err(|error| error.to_string())?;
    Ok(Schema { root, validator })
}

impl Schema {
    /// Evaluates `instance` and returns the violations sorted by instance
    /// path, then by position in the schema.
    ///
    /// # Errors
    ///
    /// Returns a description when evaluation itself fails: a regular
    /// expression match the engine could not finish, or a reference that
    /// cannot be followed at run time. Such a failure is never reported as a
    /// violation and never as a pass.
    pub(crate) fn validate(&self, instance: &Json) -> Result<Vec<Issue>, String> {
        self.evaluate(instance, false)
    }

    /// [`Schema::validate`] with the same paths and order, but no finding
    /// repeats a value of the instance: a finding names the JSON type of the
    /// offending value instead (`a string value is not one of [...]`) and
    /// property names are printed through `safe_quoted`. For diagnostics about
    /// untrusted documents (#4602).
    ///
    /// # Errors
    ///
    /// Same as [`Schema::validate`].
    pub(crate) fn validate_redacted(&self, instance: &Json) -> Result<Vec<Issue>, String> {
        self.evaluate(instance, true)
    }

    fn evaluate(&self, instance: &Json, redact: bool) -> Result<Vec<Issue>, String> {
        let value = to_value(instance)?;
        let mut found: Vec<(Vec<Segment>, Vec<usize>, String)> = Vec::new();
        for error in self.validator.iter_errors(&value) {
            if let Some(reason) = operational_failure(&error) {
                return Err(reason);
            }
            let tokens = pointer_tokens(error.instance_path().as_str());
            found.push((
                classify(instance, &tokens),
                self.schema_order(error.schema_path().as_str()),
                self.message(&error, instance, None, redact),
            ));
        }
        found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        Ok(found
            .into_iter()
            .map(|(path, _, message)| Issue { path, message })
            .collect())
    }

    /// The position of each step of `schema_path` inside its parent schema
    /// object, so findings at one instance path keep the keyword order of the
    /// schema document (the order the Python validator reported them in).
    fn schema_order(&self, schema_path: &str) -> Vec<usize> {
        let mut current = Some(&self.root);
        let mut order = Vec::new();
        for token in pointer_tokens(schema_path) {
            match current {
                Some(Json::Object(members)) => {
                    order.push(
                        members
                            .iter()
                            .position(|(key, _)| *key == token)
                            .unwrap_or(usize::MAX),
                    );
                    current = members
                        .iter()
                        .find(|(key, _)| *key == token)
                        .map(|(_, value)| value);
                }
                Some(Json::Array(items)) => {
                    let index = token.parse::<usize>().unwrap_or(usize::MAX);
                    order.push(index);
                    current = items.get(index);
                }
                _ => {
                    order.push(usize::MAX);
                    current = None;
                }
            }
        }
        order
    }

    /// The value of the schema keyword `error` was raised by, when the error's
    /// schema location ends in `keyword`.
    fn keyword_value(&self, error: &ValidationError<'_>, keyword: &str) -> Option<&Json> {
        let tokens = pointer_tokens(error.schema_path().as_str());
        if tokens.last().map(String::as_str) != Some(keyword) {
            return None;
        }
        pointer_get(&self.root, &tokens)
    }

    /// The Python-tool wording of `error`. `shown` overrides the instance for
    /// errors raised on a property name rather than on a value.
    fn message(
        &self,
        error: &ValidationError<'_>,
        document: &Json,
        shown: Option<&Json>,
        redact: bool,
    ) -> String {
        let fallback;
        let instance: &Json = if let Some(instance) = shown {
            instance
        } else if let Some(instance) =
            pointer_get(document, &pointer_tokens(error.instance_path().as_str()))
        {
            instance
        } else {
            fallback = to_json(error.instance());
            &fallback
        };
        let shown = if redact {
            redacted_label(instance)
        } else {
            py_repr(instance)
        };
        match error.kind() {
            ValidationErrorKind::Type { kind } => {
                format!("{shown} is not of type {}", self.type_names(error, kind))
            }
            ValidationErrorKind::Constant { expected_value } => {
                let expected = self
                    .keyword_value(error, "const")
                    .map_or_else(|| to_json(expected_value), Json::clone);
                format!("{} was expected", py_repr(&expected))
            }
            ValidationErrorKind::Enum { options } => {
                let options = self
                    .keyword_value(error, "enum")
                    .map_or_else(|| to_json(options), Json::clone);
                format!("{shown} is not one of {}", py_repr(&options))
            }
            ValidationErrorKind::MinLength { limit } if *limit == 1 => {
                format!("{shown} should be non-empty")
            }
            ValidationErrorKind::MinLength { .. } => format!("{shown} is too short"),
            ValidationErrorKind::MaxLength { .. } => format!("{shown} is too long"),
            ValidationErrorKind::Pattern { pattern } => {
                format!("{shown} does not match {}", py_repr_str(pattern))
            }
            ValidationErrorKind::Required { property } => {
                format!("{} is a required property", py_repr(&to_json(property)))
            }
            ValidationErrorKind::AdditionalProperties { unexpected } => {
                additional_properties_message(unexpected, redact)
            }
            ValidationErrorKind::Not { schema } => {
                let schema = self
                    .keyword_value(error, "not")
                    .map_or_else(|| to_json(schema), Json::clone);
                format!("{shown} should not be valid under {}", py_repr(&schema))
            }
            ValidationErrorKind::OneOfNotValid { .. } => {
                format!("{shown} is not valid under any of the given schemas")
            }
            ValidationErrorKind::OneOfMultipleValid { context } => {
                self.multiple_valid_message(error, context, &shown)
            }
            ValidationErrorKind::FalseSchema => format!("False schema does not allow {shown}"),
            ValidationErrorKind::PropertyNames { error: inner } => {
                let name = to_json(inner.instance());
                self.message(inner, document, Some(&name), redact)
            }
            _ if redact => self.redacted_fallback(error, &shown),
            _ => error.to_string(),
        }
    }
}

impl Schema {
    /// The wording of a redacted finding for a keyword without its own arm:
    /// numeric bounds name their limit, everything else names the keyword.
    fn redacted_fallback(&self, error: &ValidationError<'_>, shown: &str) -> String {
        if let Some(limit) = self.keyword_value(error, "minimum") {
            return format!("{shown} is less than the minimum of {}", py_repr(limit));
        }
        if let Some(limit) = self.keyword_value(error, "maximum") {
            return format!("{shown} is greater than the maximum of {}", py_repr(limit));
        }
        let tokens = pointer_tokens(error.schema_path().as_str());
        let keyword = tokens.last().map_or("schema", String::as_str);
        format!("{shown} violates the schema keyword '{keyword}'")
    }

    /// `'string'` or `'string', 'null'`: the Python reprs of the expected
    /// types, in the order the schema lists them (the crate's type set does
    /// not keep it).
    fn type_names(&self, error: &ValidationError<'_>, kind: &TypeKind) -> String {
        let names: Vec<String> = match (kind, self.keyword_value(error, "type")) {
            (TypeKind::Multiple(_), Some(Json::Array(listed))) => listed
                .iter()
                .filter_map(Json::as_str)
                .map(py_repr_str)
                .collect(),
            (TypeKind::Single(name), _) => vec![py_repr_str(&name.to_string())],
            (TypeKind::Multiple(set), _) => set
                .iter()
                .map(|name| py_repr_str(&name.to_string()))
                .collect(),
        };
        names.join(", ")
    }
}

/// `a string value`: how a redacted finding refers to the offending value.
fn redacted_label(instance: &Json) -> String {
    let kind = instance.type_name();
    let article = if matches!(kind, "array" | "integer" | "object") {
        "an"
    } else {
        "a"
    };
    format!("{article} {kind} value")
}

fn additional_properties_message(unexpected: &[String], redact: bool) -> String {
    let mut names: Vec<&String> = unexpected.iter().collect();
    names.sort();
    let verb = if names.len() == 1 { "was" } else { "were" };
    let listed: Vec<String> = names
        .into_iter()
        .map(|name| {
            if redact {
                safe_quoted(name)
            } else {
                py_repr_str(name)
            }
        })
        .collect();
    format!(
        "Additional properties are not allowed ({} {verb} unexpected)",
        listed.join(", ")
    )
}

impl Schema {
    /// `oneOf` matched more than one alternative: Python names every match.
    fn multiple_valid_message(
        &self,
        error: &ValidationError<'_>,
        context: &[Vec<ValidationError<'static>>],
        shown: &str,
    ) -> String {
        let valid: Vec<usize> = context
            .iter()
            .enumerate()
            .filter(|(_, branch)| branch.is_empty())
            .map(|(index, _)| index)
            .collect();
        match (self.keyword_value(error, "oneOf"), valid.split_first()) {
            (Some(Json::Array(items)), Some((first, others))) => {
                // Python lists the later matches, then the first.
                let reprs: Vec<String> = others
                    .iter()
                    .chain(std::iter::once(first))
                    .filter_map(|index| items.get(*index))
                    .map(py_repr)
                    .collect();
                format!("{shown} is valid under each of {}", reprs.join(", "))
            }
            _ => format!("{shown} is valid under each of the given schemas"),
        }
    }
}

/// A description when `error` reports that evaluation failed rather than that
/// the instance is invalid.
fn operational_failure(error: &ValidationError<'_>) -> Option<String> {
    let at = || {
        let path = error.instance_path().as_str();
        if path.is_empty() {
            "<root>".to_string()
        } else {
            pointer_tokens(path).join("/")
        }
    };
    match error.kind() {
        ValidationErrorKind::BacktrackLimitExceeded { .. }
        | ValidationErrorKind::RegexEngineFailure { .. } => Some(format!(
            "a regular expression could not be evaluated within its limit ({error}) at {}",
            at()
        )),
        ValidationErrorKind::Referencing(reason) => Some(format!(
            "a schema reference could not be followed ({reason}) at {}",
            at()
        )),
        ValidationErrorKind::PropertyNames { error: inner } => operational_failure(inner),
        _ => None,
    }
}

// ---- JSON conversion and pointers -------------------------------------------------

fn to_value(json: &Json) -> Result<Value, String> {
    Ok(match json {
        Json::Null => Value::Null,
        Json::Bool(flag) => Value::Bool(*flag),
        Json::Int(number) => Value::from(*number),
        Json::Float(number) => Value::Number(
            serde_json::Number::from_f64(*number)
                .ok_or_else(|| format!("the number {number} is not finite"))?,
        ),
        Json::String(text) => Value::String(text.clone()),
        Json::Array(items) => Value::Array(
            items
                .iter()
                .map(to_value)
                .collect::<Result<Vec<Value>, String>>()?,
        ),
        Json::Object(members) => {
            let mut map = serde_json::Map::new();
            for (key, member) in members {
                map.insert(key.clone(), to_value(member)?);
            }
            Value::Object(map)
        }
    })
}

fn to_json(value: &Value) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Bool(flag) => Json::Bool(*flag),
        Value::Number(number) => number.as_i64().map_or_else(
            || Json::Float(number.as_f64().unwrap_or(f64::NAN)),
            Json::Int,
        ),
        Value::String(text) => Json::String(text.clone()),
        Value::Array(items) => Json::Array(items.iter().map(to_json).collect()),
        Value::Object(map) => Json::Object(
            map.iter()
                .map(|(key, member)| (key.clone(), to_json(member)))
                .collect(),
        ),
    }
}

/// The unescaped tokens of a JSON pointer (`/a/b~1c` -> `a`, `b/c`).
fn pointer_tokens(pointer: &str) -> Vec<String> {
    pointer
        .split('/')
        .skip(1)
        .map(|token| token.replace("~1", "/").replace("~0", "~"))
        .collect()
}

/// Follows `tokens` from `root`; arrays are indexed by number.
fn pointer_get<'a>(root: &'a Json, tokens: &[String]) -> Option<&'a Json> {
    let mut current = root;
    for token in tokens {
        current = match current {
            Json::Object(members) => members
                .iter()
                .find(|(key, _)| key == token)
                .map(|(_, value)| value)?,
            Json::Array(items) => items.get(token.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// Turns pointer tokens into path segments: a token is an index exactly when
/// it addresses an array element of `instance`.
fn classify(instance: &Json, tokens: &[String]) -> Vec<Segment> {
    let mut current = Some(instance);
    let mut path = Vec::with_capacity(tokens.len());
    for token in tokens {
        match (current, token.parse::<usize>()) {
            (Some(Json::Array(items)), Ok(index)) => {
                path.push(Segment::Index(index));
                current = items.get(index);
            }
            (Some(node @ Json::Object(_)), _) => {
                path.push(Segment::Key(token.clone()));
                current = node.get(token);
            }
            _ => {
                path.push(Segment::Key(token.clone()));
                current = None;
            }
        }
    }
    path
}

/// Follows a `#` or `#/pointer` reference inside `root`.
fn resolve_reference<'a>(root: &'a Json, reference: &str) -> Option<&'a Json> {
    let pointer = reference.strip_prefix('#')?;
    if pointer.is_empty() {
        return Some(root);
    }
    if !pointer.starts_with('/') {
        return None;
    }
    pointer_get(root, &pointer_tokens(pointer))
}

// ---- schema pre-checks ----------------------------------------------------------------

fn check_schema(root: &Json) -> Result<(), String> {
    walk(root, root, "#", 0)?;
    let mut stack = vec!["#".to_string()];
    let mut done = BTreeSet::new();
    check_acyclic(root, root, &mut stack, &mut done)
}

fn walk(root: &Json, node: &Json, at: &str, depth: usize) -> Result<(), String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(format!("{at}: schema nesting is too deep"));
    }
    match node {
        Json::Object(members) => {
            for (key, value) in members {
                let here = format!("{at}/{key}");
                match (key.as_str(), value) {
                    ("$ref", Json::String(target)) => check_reference(root, target, &here)?,
                    ("$dynamicRef" | "$recursiveRef", Json::String(_)) => {
                        return Err(format!(
                            "{here}: dynamic references are not supported; use a local '$ref'"
                        ));
                    }
                    ("$id", Json::String(_)) if depth > 0 => {
                        return Err(format!(
                            "{here}: a nested '$id' changes how references resolve and is not supported"
                        ));
                    }
                    ("pattern", Json::String(pattern)) => {
                        check_pattern(pattern).map_err(|reason| format!("{here}: {reason}"))?;
                    }
                    ("patternProperties", Json::Object(patterns)) => {
                        for (pattern, _) in patterns {
                            check_pattern(pattern).map_err(|reason| format!("{here}: {reason}"))?;
                        }
                    }
                    _ => {}
                }
                walk(root, value, &here, depth + 1)?;
            }
        }
        Json::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                walk(root, item, &format!("{at}/{index}"), depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_reference(root: &Json, target: &str, at: &str) -> Result<(), String> {
    if !target.starts_with('#') {
        return Err(format!(
            "{at}: reference {target:?} is not local; only '#' and '#/pointer' references \
             inside the schema are supported and nothing is retrieved"
        ));
    }
    if target.contains('%') {
        return Err(format!(
            "{at}: reference {target:?} uses percent-encoding, which is not supported"
        ));
    }
    match resolve_reference(root, target) {
        None => Err(format!(
            "{at}: reference {target:?} does not resolve to a location in the schema"
        )),
        Some(Json::Object(_) | Json::Bool(_)) => Ok(()),
        Some(_) => Err(format!(
            "{at}: reference {target:?} points at a value that is not a schema"
        )),
    }
}

/// Rejects recursive references: they would let a schema recurse without
/// consuming the instance. The vendored schemas have none.
fn check_acyclic(
    root: &Json,
    node: &Json,
    stack: &mut Vec<String>,
    done: &mut BTreeSet<String>,
) -> Result<(), String> {
    let mut targets = Vec::new();
    collect_references(node, &mut targets);
    for target in targets {
        if stack.contains(&target) {
            return Err(format!(
                "reference {target:?} is recursive; recursive schemas are not supported"
            ));
        }
        if done.contains(&target) {
            continue;
        }
        if stack.len() > MAX_SCHEMA_DEPTH {
            return Err("reference chain is too deep".to_string());
        }
        let Some(resolved) = resolve_reference(root, &target) else {
            return Err(format!("reference {target:?} does not resolve"));
        };
        stack.push(target.clone());
        check_acyclic(root, resolved, stack, done)?;
        stack.pop();
        done.insert(target);
    }
    Ok(())
}

fn collect_references(node: &Json, out: &mut Vec<String>) {
    match node {
        Json::Object(members) => {
            for (key, value) in members {
                match (key.as_str(), value) {
                    ("$ref", Json::String(target)) => out.push(target.clone()),
                    _ => collect_references(value, out),
                }
            }
        }
        Json::Array(items) => {
            for item in items {
                collect_references(item, out);
            }
        }
        _ => {}
    }
}

/// Rejects a quantifier applied to an anchor (`^*`, `$+`, `\b{2}`), which
/// Python's `re` refuses ("nothing to repeat") but `fancy-regex` accepts.
/// Everything else invalid is rejected by the engine when the schema is built.
fn check_pattern(pattern: &str) -> Result<(), String> {
    #[derive(PartialEq)]
    enum Previous {
        Start,
        Atom,
        Anchor,
        Quantifier,
    }
    let chars: Vec<char> = pattern.chars().collect();
    let mut previous = Previous::Start;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        let mut quantifier = false;
        match c {
            '\\' => {
                previous = if matches!(chars.get(index + 1), Some('b' | 'B' | 'A' | 'z' | 'Z')) {
                    Previous::Anchor
                } else {
                    Previous::Atom
                };
                index += 1;
            }
            '[' => {
                // Skip the class, honouring escapes and a leading `]`.
                index += 1;
                if chars.get(index) == Some(&'^') {
                    index += 1;
                }
                if chars.get(index) == Some(&']') {
                    index += 1;
                }
                while index < chars.len() && chars[index] != ']' {
                    if chars[index] == '\\' {
                        index += 1;
                    }
                    index += 1;
                }
                previous = Previous::Atom;
            }
            '(' | '|' => previous = Previous::Start,
            '^' | '$' => previous = Previous::Anchor,
            '*' | '+' | '?' => quantifier = true,
            '{' => {
                if let Some(length) = counted_repetition(&chars[index..]) {
                    quantifier = true;
                    index += length - 1;
                } else {
                    previous = Previous::Atom;
                }
            }
            _ => previous = Previous::Atom,
        }
        if quantifier {
            if previous == Previous::Anchor {
                return Err(format!(
                    "pattern {pattern:?} applies a quantifier to an anchor (nothing to repeat)"
                ));
            }
            // A `?` after a quantifier makes it lazy; it is not another one.
            previous = if previous == Previous::Quantifier && c == '?' {
                Previous::Atom
            } else {
                Previous::Quantifier
            };
        }
        index += 1;
    }
    Ok(())
}

/// The length of a `{n}`, `{n,}` or `{n,m}` repetition at the start of
/// `chars`, if there is one.
fn counted_repetition(chars: &[char]) -> Option<usize> {
    let mut index = 1;
    let digits = |from: usize| {
        chars[from..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count()
    };
    let first = digits(index);
    if first == 0 {
        return None;
    }
    index += first;
    if chars.get(index) == Some(&',') {
        index += 1 + digits(index + 1);
    }
    (chars.get(index) == Some(&'}')).then_some(index + 1)
}
