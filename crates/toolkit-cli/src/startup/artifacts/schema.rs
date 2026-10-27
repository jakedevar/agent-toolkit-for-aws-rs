//! The draft-07 subset validator of `validate-artifacts.py`.
//!
//! Only the keywords in [`ENFORCED`] are evaluated; [`unsupported_keywords`]
//! reports every other validation keyword so a schema can never lose a
//! constraint silently. `pattern` is evaluated by the engine the vendored
//! spec validator uses (`jsonschema` on `fancy-regex`, bounded backtracking),
//! so no second regular-expression engine is added.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

use toolkit_core::Json;

use super::pyjson::dumps;
use super::shape::join_path;
use super::{Code, Finding};
use crate::manifests::{is_falsy, py_eq, py_float_repr, py_repr};
use crate::spec::engine;

const ENFORCED: [&str; 24] = [
    "type",
    "properties",
    "required",
    "additionalProperties",
    "enum",
    "const",
    "items",
    "pattern",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
    "minLength",
    "maxLength",
    "allOf",
    "anyOf",
    "oneOf",
    "not",
    "$ref",
];

const ANNOTATIONS: [&str; 9] = [
    "$schema",
    "$id",
    "$comment",
    "title",
    "description",
    "default",
    "examples",
    "definitions",
    "$defs",
];

/// Deepest `$ref` / combinator nesting evaluated before the run is refused
/// (Python would hit its recursion limit).
const MAX_DEPTH: usize = 200;

/// `(json-pointer, keyword)` for every keyword the subset neither enforces nor
/// knows as an annotation. `format` is an annotation here, as in Python.
pub(crate) fn unsupported_keywords(schema: &Json, pointer: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let Json::Object(members) = schema else {
        return found;
    };
    for (key, value) in members {
        if matches!(
            key.as_str(),
            "properties" | "definitions" | "$defs" | "patternProperties"
        ) {
            if let Json::Object(named) = value {
                for (name, sub) in named {
                    found.extend(unsupported_keywords(
                        sub,
                        &format!("{pointer}/{key}/{name}"),
                    ));
                }
            }
            if key == "patternProperties" {
                found.push((pointer.to_string(), key.clone()));
            }
            continue;
        }
        if matches!(key.as_str(), "items" | "additionalProperties" | "not") {
            found.extend(unsupported_keywords(value, &format!("{pointer}/{key}")));
        } else if matches!(key.as_str(), "allOf" | "anyOf" | "oneOf") {
            if let Json::Array(subs) = value {
                for (index, sub) in subs.iter().enumerate() {
                    found.extend(unsupported_keywords(
                        sub,
                        &format!("{pointer}/{key}/{index}"),
                    ));
                }
            }
        }
        if !ENFORCED.contains(&key.as_str())
            && !ANNOTATIONS.contains(&key.as_str())
            && key != "format"
        {
            found.push((pointer.to_string(), key.clone()));
        }
    }
    found
}

/// Evaluates schemas and caches compiled `pattern` matchers.
#[derive(Default)]
pub(crate) struct SubsetValidator {
    patterns: HashMap<String, Result<engine::Schema, String>>,
}

/// The run context of one validation.
pub(crate) struct Context<'a> {
    pub(crate) artifact: &'a str,
    pub(crate) contract: &'a str,
}

fn number_text(value: &Json) -> String {
    match value {
        Json::Int(number) => number.to_string(),
        Json::Float(number) => py_float_repr(*number),
        other => py_repr(other),
    }
}

fn is_number(value: &Json) -> bool {
    matches!(value, Json::Int(_) | Json::Float(_))
}

#[allow(clippy::cast_precision_loss)]
fn compare(left: &Json, right: &Json) -> Option<Ordering> {
    match (left, right) {
        (Json::Int(a), Json::Int(b)) => Some(a.cmp(b)),
        (Json::Int(a), Json::Float(b)) => (*a as f64).partial_cmp(b),
        (Json::Float(a), Json::Int(b)) => a.partial_cmp(&(*b as f64)),
        (Json::Float(a), Json::Float(b)) => a.partial_cmp(b),
        _ => None,
    }
}

fn kind_of(value: &Json) -> &'static str {
    match value {
        Json::Object(_) => "object",
        Json::Array(_) => "array",
        Json::String(_) => "string",
        Json::Bool(_) => "boolean",
        Json::Int(_) | Json::Float(_) => "number",
        Json::Null => "null",
    }
}

impl SubsetValidator {
    pub(crate) fn regex_search(&mut self, pattern: &str, text: &str) -> Result<bool, String> {
        let compiled = self.patterns.entry(pattern.to_string()).or_insert_with(|| {
            engine::compile(Json::object(vec![("pattern", Json::string(pattern))]))
        });
        match compiled {
            Ok(schema) => schema
                .validate(&Json::string(text))
                .map(|issues| issues.is_empty())
                .map_err(|reason| format!("pattern '{pattern}': {reason}")),
            Err(reason) => Err(format!("pattern '{pattern}': {reason}")),
        }
    }

    /// `validate_json_schema`.
    ///
    /// # Errors
    ///
    /// A description when a pattern cannot be compiled or evaluated, or the
    /// schema nests deeper than the evaluation limit.
    pub(crate) fn validate(
        &mut self,
        schema: &Json,
        value: &Json,
        path: &str,
        context: &Context<'_>,
        out: &mut Vec<Finding>,
        root: &Json,
    ) -> Result<(), String> {
        self.walk(schema, value, path, context, out, root, 0)
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn walk(
        &mut self,
        schema: &Json,
        value: &Json,
        path: &str,
        context: &Context<'_>,
        out: &mut Vec<Finding>,
        root: &Json,
        depth: usize,
    ) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("schema nesting or $ref chain is too deep".to_string());
        }
        let Json::Object(_) = schema else {
            return Ok(());
        };
        let shown = if path.is_empty() { "$" } else { path };
        let violation = |out: &mut Vec<Finding>, at: &str, message: String| {
            out.push(Finding::new(
                Code::SchemaViolation,
                context.artifact,
                at,
                message,
                context.contract,
            ));
        };
        if let Some(Json::String(reference)) = schema.get("$ref") {
            if let Some(pointer) = reference.strip_prefix("#/") {
                let mut target = root;
                let empty = Json::Object(Vec::new());
                for part in pointer.split('/') {
                    target = match target {
                        Json::Object(_) => target.get(part).unwrap_or(&empty),
                        _ => &empty,
                    };
                }
                return self.walk(target, value, path, context, out, root, depth + 1);
            }
            return Ok(());
        }
        if schema.get("$ref").is_some() {
            return Ok(());
        }
        if let Some(declared) = schema.get("type").filter(|t| !matches!(t, Json::Null)) {
            let types: Vec<&Json> = match declared {
                Json::Array(items) => items.iter().collect(),
                other => vec![other],
            };
            let kind = kind_of(value);
            let named = |name: &str| types.iter().any(|t| t.as_str() == Some(name));
            let ok = named(kind) || (named("integer") && matches!(value, Json::Int(_)));
            if !ok {
                let shown_type = declared
                    .as_str()
                    .map_or_else(|| py_repr(declared), str::to_string);
                violation(
                    out,
                    shown,
                    format!("is {kind}, schema type is {shown_type}"),
                );
                return Ok(());
            }
        }
        if let Some(Json::Array(allowed)) = schema.get("enum") {
            if !allowed.iter().any(|candidate| py_eq(value, candidate)) {
                let listed = py_repr(&Json::Array(allowed.clone()));
                violation(
                    out,
                    shown,
                    format!("{} not in enum {listed}", py_repr(value)),
                );
            }
        }
        if let Some(expected) = schema.get("const") {
            if !py_eq(value, expected) {
                violation(
                    out,
                    shown,
                    format!("{} != const {}", py_repr(value), py_repr(expected)),
                );
            }
        }
        if let Json::String(text) = value {
            if let Some(Json::String(pattern)) = schema.get("pattern") {
                if !self.regex_search(pattern, text)? {
                    violation(
                        out,
                        shown,
                        format!("'{text}' does not match pattern {pattern}"),
                    );
                }
            }
            let length = Json::Int(i64::try_from(text.chars().count()).unwrap_or(i64::MAX));
            if let Some(limit) = schema.get("minLength").filter(|l| is_number(l)) {
                if compare(&length, limit) == Some(Ordering::Less) {
                    violation(
                        out,
                        shown,
                        format!(
                            "length {} < minLength {}",
                            number_text(&length),
                            number_text(limit)
                        ),
                    );
                }
            }
            if let Some(limit) = schema.get("maxLength").filter(|l| is_number(l)) {
                if compare(&length, limit) == Some(Ordering::Greater) {
                    violation(
                        out,
                        shown,
                        format!(
                            "length {} > maxLength {}",
                            number_text(&length),
                            number_text(limit)
                        ),
                    );
                }
            }
        }
        if is_number(value) {
            let rules: [(&str, Ordering, bool, &str); 4] = [
                ("minimum", Ordering::Less, false, "<"),
                ("maximum", Ordering::Greater, false, ">"),
                ("exclusiveMinimum", Ordering::Less, true, "<="),
                ("exclusiveMaximum", Ordering::Greater, true, ">="),
            ];
            for (keyword, bad, inclusive, symbol) in rules {
                let Some(limit) = schema.get(keyword).filter(|l| is_number(l)) else {
                    continue;
                };
                let order = compare(value, limit);
                let failed = order == Some(bad) || (inclusive && order == Some(Ordering::Equal));
                if failed {
                    violation(
                        out,
                        shown,
                        format!(
                            "{} {symbol} {keyword} {}",
                            number_text(value),
                            number_text(limit)
                        ),
                    );
                }
            }
        }
        if let Json::Object(members) = value {
            let count = Json::Int(i64::try_from(members.len()).unwrap_or(i64::MAX));
            if let Some(limit) = schema.get("minProperties").filter(|l| is_number(l)) {
                if compare(&count, limit) == Some(Ordering::Less) {
                    violation(
                        out,
                        shown,
                        format!(
                            "{} properties < minProperties {}",
                            number_text(&count),
                            number_text(limit)
                        ),
                    );
                }
            }
            if let Some(limit) = schema.get("maxProperties").filter(|l| is_number(l)) {
                if compare(&count, limit) == Some(Ordering::Greater) {
                    violation(
                        out,
                        shown,
                        format!(
                            "{} properties > maxProperties {}",
                            number_text(&count),
                            number_text(limit)
                        ),
                    );
                }
            }
            let properties = schema.get("properties");
            if let Some(Json::Array(required)) = schema.get("required") {
                for key in required.iter().filter_map(Json::as_str) {
                    if value.get(key).is_none() {
                        violation(out, &join_path(path, key), "required by schema".to_string());
                    }
                }
            }
            let additional = schema.get("additionalProperties");
            for (key, member) in members {
                let declared = properties.and_then(|p| p.get(key));
                if let Some(sub) = declared {
                    self.walk(
                        sub,
                        member,
                        &join_path(path, key),
                        context,
                        out,
                        root,
                        depth + 1,
                    )?;
                } else if matches!(additional, Some(Json::Bool(false))) {
                    violation(
                        out,
                        &join_path(path, key),
                        "additionalProperties is false".to_string(),
                    );
                } else if let Some(sub @ Json::Object(_)) = additional {
                    self.walk(
                        sub,
                        member,
                        &join_path(path, key),
                        context,
                        out,
                        root,
                        depth + 1,
                    )?;
                }
            }
        }
        if let Json::Array(items) = value {
            let count = Json::Int(i64::try_from(items.len()).unwrap_or(i64::MAX));
            if let Some(limit) = schema.get("minItems").filter(|l| is_number(l)) {
                if compare(&count, limit) == Some(Ordering::Less) {
                    violation(
                        out,
                        shown,
                        format!(
                            "{} items < minItems {}",
                            number_text(&count),
                            number_text(limit)
                        ),
                    );
                }
            }
            if let Some(limit) = schema.get("maxItems").filter(|l| is_number(l)) {
                if compare(&count, limit) == Some(Ordering::Greater) {
                    violation(
                        out,
                        shown,
                        format!(
                            "{} items > maxItems {}",
                            number_text(&count),
                            number_text(limit)
                        ),
                    );
                }
            }
            if schema
                .get("uniqueItems")
                .is_some_and(|flag| !is_falsy(flag))
            {
                let distinct: BTreeSet<String> =
                    items.iter().map(|item| dumps(item, None, true)).collect();
                if distinct.len() != items.len() {
                    violation(out, shown, "items are not unique (uniqueItems)".to_string());
                }
            }
            if let Some(sub @ Json::Object(_)) = schema.get("items") {
                for (index, element) in items.iter().enumerate() {
                    self.walk(
                        sub,
                        element,
                        &format!("{path}[{index}]"),
                        context,
                        out,
                        root,
                        depth + 1,
                    )?;
                }
            }
        }
        if let Some(Json::Array(subs)) = schema.get("allOf") {
            for sub in subs {
                self.walk(sub, value, path, context, out, root, depth + 1)?;
            }
        }
        if let Some(sub @ Json::Object(_)) = schema.get("not") {
            let mut trial = Vec::new();
            self.walk(sub, value, path, context, &mut trial, root, depth + 1)?;
            if trial.is_empty() {
                violation(out, shown, "matches a `not` schema".to_string());
            }
        }
        for combinator in ["oneOf", "anyOf"] {
            let Some(Json::Array(subs)) = schema.get(combinator) else {
                continue;
            };
            if subs.is_empty() {
                continue;
            }
            let mut passes = 0_usize;
            for sub in subs {
                let mut trial = Vec::new();
                self.walk(sub, value, path, context, &mut trial, root, depth + 1)?;
                if trial.is_empty() {
                    passes += 1;
                }
            }
            if passes == 0 || (combinator == "oneOf" && passes > 1) {
                violation(
                    out,
                    shown,
                    format!("matches {passes} of {} {combinator} branches", subs.len()),
                );
            }
        }
        Ok(())
    }
}
