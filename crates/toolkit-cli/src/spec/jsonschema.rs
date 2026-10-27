//! A small JSON Schema (draft 2020-12) evaluator for the vendored Agent
//! Plugins schemas.
//!
//! `tools/validate_spec.py` used the `jsonschema` package. The toolkit has no
//! such dependency (AC3), so this module evaluates the keywords the vendored
//! schemas use, reproducing `jsonschema`'s error wording and ordering:
//!
//! - `type`, `const`, `enum`, `minLength`, `maxLength`, `pattern`,
//! - `properties`, `required`, `additionalProperties` (boolean or schema),
//!   `items`, `propertyNames`,
//! - `not`, `oneOf` and local `$ref` pointers (`#`, `#/$defs/name`).
//!
//! Annotation keywords (`$schema`, `$id`, `title`, `description`, `$defs`, ...)
//! are ignored. [`check_schema`] rejects any other keyword and any pattern the
//! bundled regex engine cannot compile, so a schema that would need a feature
//! this evaluator lacks fails loudly instead of validating too little.
//!
//! Errors are reported as [`Issue`]s with a path of property names and array
//! indexes. [`validate`] returns them sorted by path (stable, so equal paths
//! keep schema order), the order the Python validator printed them in.

use toolkit_core::Json;

use super::regex::Regex;
use crate::manifests::py_repr;

/// Keywords that carry no validation behaviour.
const ANNOTATIONS: &[&str] = &[
    "$schema",
    "$id",
    "$comment",
    "$defs",
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
];

/// Keywords this evaluator implements.
const SUPPORTED: &[&str] = &[
    "type",
    "const",
    "enum",
    "minLength",
    "maxLength",
    "pattern",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "propertyNames",
    "not",
    "oneOf",
    "$ref",
];

/// How deep `$ref` chains and nested subschemas may go.
const MAX_SCHEMA_DEPTH: usize = 64;

/// One step of an instance path.
///
/// `Index` sorts before `Key`; the vendored schemas never put both at the same
/// position.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Segment {
    Index(usize),
    Key(String),
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

/// Verifies that `schema` only uses features this evaluator supports.
///
/// # Errors
///
/// Returns a description of the first unsupported keyword, malformed keyword
/// value or uncompilable pattern, with the schema location.
pub(crate) fn check_schema(schema: &Json) -> Result<(), String> {
    check_subschema(schema, "#", 0)
}

fn check_subschema(schema: &Json, location: &str, depth: usize) -> Result<(), String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(format!("{location}: schema nesting is too deep"));
    }
    let members = match schema {
        Json::Bool(_) => return Ok(()),
        Json::Object(members) => members,
        _ => return Err(format!("{location}: a schema must be an object or boolean")),
    };
    for (keyword, value) in members {
        let here = format!("{location}/{keyword}");
        if ANNOTATIONS.contains(&keyword.as_str()) {
            if keyword == "$defs" {
                let Json::Object(defs) = value else {
                    return Err(format!("{here}: must be an object"));
                };
                for (name, sub) in defs {
                    check_subschema(sub, &format!("{here}/{name}"), depth + 1)?;
                }
            }
            continue;
        }
        if !SUPPORTED.contains(&keyword.as_str()) {
            return Err(format!("{here}: schema keyword is not supported"));
        }
        check_keyword(keyword, value, &here, depth)?;
    }
    Ok(())
}

fn check_keyword(keyword: &str, value: &Json, here: &str, depth: usize) -> Result<(), String> {
    match keyword {
        "type" => check_type_names(value, here),
        "minLength" | "maxLength" => match value {
            Json::Int(number) if *number >= 0 => Ok(()),
            _ => Err(format!("{here}: must be a non-negative integer")),
        },
        "pattern" => {
            let Json::String(pattern) = value else {
                return Err(format!("{here}: must be a string"));
            };
            Regex::new(pattern)
                .map(|_| ())
                .map_err(|reason| format!("{here}: unsupported pattern {pattern:?}: {reason}"))
        }
        "required" => match value {
            Json::Array(items) if items.iter().all(|item| matches!(item, Json::String(_))) => {
                Ok(())
            }
            _ => Err(format!("{here}: must be an array of strings")),
        },
        "enum" => match value {
            Json::Array(_) => Ok(()),
            _ => Err(format!("{here}: must be an array")),
        },
        "$ref" => match value {
            Json::String(_) => Ok(()),
            _ => Err(format!("{here}: must be a string")),
        },
        "properties" => {
            let Json::Object(properties) = value else {
                return Err(format!("{here}: must be an object"));
            };
            for (name, sub) in properties {
                check_subschema(sub, &format!("{here}/{name}"), depth + 1)?;
            }
            Ok(())
        }
        "oneOf" => {
            let Json::Array(alternatives) = value else {
                return Err(format!("{here}: must be an array"));
            };
            for (index, sub) in alternatives.iter().enumerate() {
                check_subschema(sub, &format!("{here}/{index}"), depth + 1)?;
            }
            Ok(())
        }
        "additionalProperties" | "items" | "propertyNames" | "not" => {
            check_subschema(value, here, depth + 1)
        }
        // `const` accepts any value.
        _ => Ok(()),
    }
}

fn check_type_names(value: &Json, here: &str) -> Result<(), String> {
    const NAMES: &[&str] = &[
        "null", "boolean", "object", "array", "string", "number", "integer",
    ];
    let valid = |item: &Json| matches!(item, Json::String(name) if NAMES.contains(&name.as_str()));
    let ok = match value {
        Json::Array(items) => !items.is_empty() && items.iter().all(valid),
        other => valid(other),
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{here}: must be a JSON type name or an array of them"
        ))
    }
}

/// Validates `instance` against `root`, returning every issue sorted by path.
///
/// `root` must already have passed [`check_schema`].
pub(crate) fn validate(root: &Json, instance: &Json) -> Vec<Issue> {
    let evaluator = Evaluator { root };
    let mut issues = Vec::new();
    let mut path = Vec::new();
    evaluator.evaluate(root, instance, &mut path, &mut issues, 0);
    issues.sort_by(|a, b| a.path.cmp(&b.path));
    issues
}

struct Evaluator<'a> {
    root: &'a Json,
}

impl Evaluator<'_> {
    fn evaluate(
        &self,
        schema: &Json,
        instance: &Json,
        path: &mut Vec<Segment>,
        issues: &mut Vec<Issue>,
        depth: usize,
    ) {
        let members = match schema {
            Json::Bool(false) => {
                push(
                    issues,
                    path,
                    format!("False schema does not allow {}", py_repr(instance)),
                );
                return;
            }
            Json::Object(members) => members,
            _ => return,
        };
        if depth > MAX_SCHEMA_DEPTH {
            push(issues, path, "schema reference nesting is too deep".into());
            return;
        }
        for (keyword, value) in members {
            self.keyword(schema, keyword, value, instance, path, issues, depth);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn keyword(
        &self,
        schema: &Json,
        keyword: &str,
        value: &Json,
        instance: &Json,
        path: &mut Vec<Segment>,
        issues: &mut Vec<Issue>,
        depth: usize,
    ) {
        match keyword {
            "type" => check_type(value, instance, path, issues),
            "const" => {
                if !json_equal(value, instance) {
                    push(issues, path, format!("{} was expected", py_repr(value)));
                }
            }
            "enum" => {
                let matches = matches!(value, Json::Array(options) if options.iter().any(|option| json_equal(option, instance)));
                if !matches {
                    push(
                        issues,
                        path,
                        format!("{} is not one of {}", py_repr(instance), py_repr(value)),
                    );
                }
            }
            "minLength" | "maxLength" | "pattern" => {
                check_string(keyword, value, instance, path, issues);
            }
            "properties" => self.properties(value, instance, path, issues, depth),
            "required" => check_required(value, instance, path, issues),
            "additionalProperties" => {
                self.additional_properties(schema, value, instance, path, issues, depth);
            }
            "items" => {
                if let Json::Array(elements) = instance {
                    for (index, element) in elements.iter().enumerate() {
                        path.push(Segment::Index(index));
                        self.evaluate(value, element, path, issues, depth + 1);
                        path.pop();
                    }
                }
            }
            "propertyNames" => {
                if let Json::Object(members) = instance {
                    for (name, _) in members {
                        let key = Json::String(name.clone());
                        self.evaluate(value, &key, path, issues, depth + 1);
                    }
                }
            }
            "not" => {
                if self.is_valid(value, instance, depth + 1) {
                    push(
                        issues,
                        path,
                        format!(
                            "{} should not be valid under {}",
                            py_repr(instance),
                            py_repr(value)
                        ),
                    );
                }
            }
            "oneOf" => self.one_of(value, instance, path, issues, depth),
            "$ref" => self.reference(value, instance, path, issues, depth),
            _ => {}
        }
    }

    fn is_valid(&self, schema: &Json, instance: &Json, depth: usize) -> bool {
        let mut issues = Vec::new();
        self.evaluate(schema, instance, &mut Vec::new(), &mut issues, depth);
        issues.is_empty()
    }

    fn properties(
        &self,
        value: &Json,
        instance: &Json,
        path: &mut Vec<Segment>,
        issues: &mut Vec<Issue>,
        depth: usize,
    ) {
        let (Json::Object(properties), Json::Object(_)) = (value, instance) else {
            return;
        };
        for (name, sub) in properties {
            if let Some(member) = instance.get(name) {
                path.push(Segment::Key(name.clone()));
                self.evaluate(sub, member, path, issues, depth + 1);
                path.pop();
            }
        }
    }

    fn additional_properties(
        &self,
        schema: &Json,
        value: &Json,
        instance: &Json,
        path: &mut Vec<Segment>,
        issues: &mut Vec<Issue>,
        depth: usize,
    ) {
        let Json::Object(members) = instance else {
            return;
        };
        let declared = schema.get("properties");
        let mut extras: Vec<&(String, Json)> = members
            .iter()
            .filter(|(name, _)| declared.is_none_or(|properties| properties.get(name).is_none()))
            .collect();
        if extras.is_empty() {
            return;
        }
        match value {
            Json::Bool(false) => {
                extras.sort_by(|a, b| a.0.cmp(&b.0));
                let names: Vec<String> = extras
                    .iter()
                    .map(|(name, _)| py_repr(&Json::String(name.clone())))
                    .collect();
                let verb = if names.len() == 1 { "was" } else { "were" };
                push(
                    issues,
                    path,
                    format!(
                        "Additional properties are not allowed ({} {verb} unexpected)",
                        names.join(", ")
                    ),
                );
            }
            Json::Bool(true) => {}
            sub => {
                for (name, member) in extras {
                    path.push(Segment::Key(name.clone()));
                    self.evaluate(sub, member, path, issues, depth + 1);
                    path.pop();
                }
            }
        }
    }

    fn one_of(
        &self,
        value: &Json,
        instance: &Json,
        path: &[Segment],
        issues: &mut Vec<Issue>,
        depth: usize,
    ) {
        let Json::Array(alternatives) = value else {
            return;
        };
        let mut valid = alternatives
            .iter()
            .filter(|alternative| self.is_valid(alternative, instance, depth + 1));
        let Some(first) = valid.next() else {
            push(
                issues,
                path,
                format!(
                    "{} is not valid under any of the given schemas",
                    py_repr(instance)
                ),
            );
            return;
        };
        let mut matching: Vec<&Json> = valid.collect();
        if !matching.is_empty() {
            matching.push(first);
            let reprs: Vec<String> = matching.into_iter().map(py_repr).collect();
            push(
                issues,
                path,
                format!(
                    "{} is valid under each of {}",
                    py_repr(instance),
                    reprs.join(", ")
                ),
            );
        }
    }

    fn reference(
        &self,
        value: &Json,
        instance: &Json,
        path: &mut Vec<Segment>,
        issues: &mut Vec<Issue>,
        depth: usize,
    ) {
        let Json::String(reference) = value else {
            return;
        };
        match resolve_pointer(self.root, reference) {
            Some(target) => self.evaluate(target, instance, path, issues, depth + 1),
            None => push(
                issues,
                path,
                format!("unresolvable reference {}", py_repr(value)),
            ),
        }
    }
}

fn push(issues: &mut Vec<Issue>, path: &[Segment], message: String) {
    issues.push(Issue {
        path: path.to_vec(),
        message,
    });
}

/// Resolves `#` or `#/a/b` (with `~0` / `~1` escapes) inside `root`.
fn resolve_pointer<'a>(root: &'a Json, reference: &str) -> Option<&'a Json> {
    let pointer = reference.strip_prefix('#')?;
    if pointer.is_empty() {
        return Some(root);
    }
    let mut current = root;
    for token in pointer.strip_prefix('/')?.split('/') {
        let key = token.replace("~1", "/").replace("~0", "~");
        current = match current {
            Json::Object(_) => current.get(&key)?,
            Json::Array(items) => items.get(key.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

fn type_matches(name: &str, instance: &Json) -> bool {
    match name {
        "null" => matches!(instance, Json::Null),
        "boolean" => matches!(instance, Json::Bool(_)),
        "object" => matches!(instance, Json::Object(_)),
        "array" => matches!(instance, Json::Array(_)),
        "string" => matches!(instance, Json::String(_)),
        "number" => matches!(instance, Json::Int(_) | Json::Float(_)),
        "integer" => match instance {
            Json::Int(_) => true,
            Json::Float(number) => number.fract() == 0.0,
            _ => false,
        },
        _ => false,
    }
}

fn check_type(value: &Json, instance: &Json, path: &[Segment], issues: &mut Vec<Issue>) {
    let names: Vec<&str> = match value {
        Json::String(name) => vec![name.as_str()],
        Json::Array(items) => items.iter().filter_map(Json::as_str).collect(),
        _ => return,
    };
    if names.iter().any(|name| type_matches(name, instance)) {
        return;
    }
    let reprs: Vec<String> = names
        .iter()
        .map(|name| py_repr(&Json::String((*name).to_string())))
        .collect();
    push(
        issues,
        path,
        format!("{} is not of type {}", py_repr(instance), reprs.join(", ")),
    );
}

fn check_string(
    keyword: &str,
    value: &Json,
    instance: &Json,
    path: &[Segment],
    issues: &mut Vec<Issue>,
) {
    let Json::String(text) = instance else {
        return;
    };
    match (keyword, value) {
        ("minLength", Json::Int(limit)) => {
            if i64::try_from(text.chars().count()).unwrap_or(i64::MAX) < *limit {
                let message = if *limit == 1 {
                    format!("{} should be non-empty", py_repr(instance))
                } else {
                    format!("{} is too short", py_repr(instance))
                };
                push(issues, path, message);
            }
        }
        ("maxLength", Json::Int(limit)) => {
            if i64::try_from(text.chars().count()).unwrap_or(i64::MAX) > *limit {
                push(issues, path, format!("{} is too long", py_repr(instance)));
            }
        }
        ("pattern", Json::String(pattern)) => {
            match Regex::new(pattern).and_then(|regex| regex.is_match(text)) {
                Ok(true) => {}
                Ok(false) => push(
                    issues,
                    path,
                    format!("{} does not match {}", py_repr(instance), py_repr(value)),
                ),
                Err(reason) => push(
                    issues,
                    path,
                    format!(
                        "{} could not be matched against {}: {reason}",
                        py_repr(instance),
                        py_repr(value)
                    ),
                ),
            }
        }
        _ => {}
    }
}

fn check_required(value: &Json, instance: &Json, path: &[Segment], issues: &mut Vec<Issue>) {
    let (Json::Array(names), Json::Object(_)) = (value, instance) else {
        return;
    };
    for name in names.iter().filter_map(Json::as_str) {
        if instance.get(name).is_none() {
            push(
                issues,
                path,
                format!(
                    "{} is a required property",
                    py_repr(&Json::String(name.to_string()))
                ),
            );
        }
    }
}

#[allow(clippy::float_cmp)]
/// JSON equality as `jsonschema` applies it: numbers compare by value, a
/// boolean is never equal to a number, object member order is irrelevant.
pub(crate) fn json_equal(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Null, Json::Null) => true,
        (Json::Bool(x), Json::Bool(y)) => x == y,
        (Json::String(x), Json::String(y)) => x == y,
        (Json::Int(x), Json::Int(y)) => x == y,
        (Json::Int(_) | Json::Float(_), Json::Int(_) | Json::Float(_)) => {
            number_value(a) == number_value(b)
        }
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_equal(p, q))
        }
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, p)| b.get(key).is_some_and(|q| json_equal(p, q)))
        }
        _ => false,
    }
}

#[allow(clippy::cast_precision_loss)]
fn number_value(value: &Json) -> f64 {
    match value {
        Json::Int(number) => *number as f64,
        Json::Float(number) => *number,
        _ => f64::NAN,
    }
}

#[cfg(test)]
mod tests {
    use toolkit_core::parse_json;

    use super::{check_schema, json_equal, validate, Issue};

    fn json(text: &str) -> toolkit_core::Json {
        parse_json(text).expect("fixture parses")
    }

    fn run(schema: &str, instance: &str) -> Vec<String> {
        let schema = json(schema);
        check_schema(&schema).expect("supported schema");
        validate(&schema, &json(instance))
            .iter()
            .map(|issue: &Issue| format!("{} (at {})", issue.message, issue.path_text()))
            .collect()
    }

    #[test]
    fn type_keyword_names_the_python_repr_of_the_value() {
        assert_eq!(
            run(r#"{"type":"string"}"#, "5"),
            ["5 is not of type 'string' (at <root>)"]
        );
        assert_eq!(
            run(r#"{"type":["string","null"]}"#, "[1]"),
            ["[1] is not of type 'string', 'null' (at <root>)"]
        );
        assert!(run(r#"{"type":["string","null"]}"#, "null").is_empty());
        assert!(run(r#"{"type":"integer"}"#, "3").is_empty());
        assert!(run(r#"{"type":"integer"}"#, "3.0").is_empty());
        assert_eq!(run(r#"{"type":"integer"}"#, "3.5").len(), 1);
        assert!(run(r#"{"type":"number"}"#, "3.5").is_empty());
        assert_eq!(run(r#"{"type":"number"}"#, "true").len(), 1);
        assert_eq!(
            run(r#"{"type":"object"}"#, r#""x""#),
            ["'x' is not of type 'object' (at <root>)"]
        );
    }

    #[test]
    fn const_and_enum_compare_by_value() {
        assert_eq!(
            run(r#"{"const":"a"}"#, r#""b""#),
            ["'a' was expected (at <root>)"]
        );
        assert!(run(r#"{"const":1}"#, "1.0").is_empty());
        assert_eq!(run(r#"{"const":1}"#, "true").len(), 1);
        assert!(run(r#"{"const":{"a":1,"b":2}}"#, r#"{"b":2,"a":1}"#).is_empty());
        assert_eq!(
            run(r#"{"enum":["x","y"]}"#, r#""z""#),
            ["'z' is not one of ['x', 'y'] (at <root>)"]
        );
        assert!(run(r#"{"enum":["x","y"]}"#, r#""y""#).is_empty());
    }

    #[test]
    fn string_length_and_pattern_keywords() {
        assert_eq!(
            run(r#"{"minLength":1}"#, r#""""#),
            ["'' should be non-empty (at <root>)"]
        );
        assert_eq!(
            run(r#"{"minLength":3}"#, r#""ab""#),
            ["'ab' is too short (at <root>)"]
        );
        assert_eq!(
            run(r#"{"maxLength":2}"#, r#""abc""#),
            ["'abc' is too long (at <root>)"]
        );
        // Length counts characters, not bytes.
        assert!(run(r#"{"maxLength":2}"#, r#""éé""#).is_empty());
        assert_eq!(
            run(r#"{"pattern":"^a+$"}"#, r#""b""#),
            ["'b' does not match '^a+$' (at <root>)"]
        );
        assert!(run(r#"{"pattern":"^a+$"}"#, r#""aa""#).is_empty());
        // Non-strings are not subject to string keywords.
        assert!(run(r#"{"minLength":5,"pattern":"x"}"#, "1").is_empty());
    }

    #[test]
    fn properties_required_and_additional_properties() {
        let schema = r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"integer"},"c":{}},"required":["a","c"],"additionalProperties":false}"#;
        assert_eq!(
            run(schema, r#"{"a":1,"b":"x","z":0,"y":0}"#),
            [
                "'c' is a required property (at <root>)",
                "Additional properties are not allowed ('y', 'z' were unexpected) (at <root>)",
                "1 is not of type 'string' (at a)",
                "'x' is not of type 'integer' (at b)",
            ]
        );
        assert_eq!(
            run(schema, r#"{"a":"s","c":1,"q":1}"#),
            ["Additional properties are not allowed ('q' was unexpected) (at <root>)"]
        );
        assert!(run(schema, r#"{"a":"s","c":1}"#).is_empty());
        // Required and properties ignore non-objects.
        assert_eq!(
            run(schema, "[]"),
            ["[] is not of type 'object' (at <root>)"]
        );
    }

    #[test]
    fn additional_properties_can_be_a_schema() {
        let schema = r#"{"properties":{"k":{}},"additionalProperties":{"type":"string"}}"#;
        assert_eq!(
            run(schema, r#"{"k":1,"x":"s","y":2}"#),
            ["2 is not of type 'string' (at y)"]
        );
    }

    #[test]
    fn items_and_property_names() {
        assert_eq!(
            run(
                r#"{"type":"array","items":{"type":"string"}}"#,
                r#"["a",1,"b",null]"#
            ),
            [
                "1 is not of type 'string' (at 1)",
                "None is not of type 'string' (at 3)"
            ]
        );
        let schema = r#"{"propertyNames":{"not":{"enum":["A","B"]}}}"#;
        assert!(run(schema, r#"{"C":1}"#).is_empty());
        assert_eq!(
            run(schema, r#"{"A":1}"#),
            ["'A' should not be valid under {'enum': ['A', 'B']} (at <root>)"]
        );
    }

    #[test]
    fn one_of_and_references() {
        let schema = r##"{"$defs":{"s":{"type":"string"},"n":{"type":"integer"}},"oneOf":[{"$ref":"#/$defs/s"},{"$ref":"#/$defs/n"}]}"##;
        assert!(run(schema, r#""x""#).is_empty());
        assert!(run(schema, "4").is_empty());
        assert_eq!(
            run(schema, "[]"),
            ["[] is not valid under any of the given schemas (at <root>)"]
        );
        let both = r#"{"oneOf":[{"type":"number"},{"type":"integer"}]}"#;
        assert_eq!(
            run(both, "3"),
            ["3 is valid under each of {'type': 'integer'}, {'type': 'number'} (at <root>)"]
        );
    }

    #[test]
    fn unresolvable_reference_is_an_issue() {
        let schema = json(r##"{"$ref":"#/$defs/missing"}"##);
        check_schema(&schema).expect("supported");
        let issues = validate(&schema, &json("1"));
        assert_eq!(issues.len(), 1);
        assert_eq!(
            issues[0].message,
            "unresolvable reference '#/$defs/missing'"
        );
    }

    #[test]
    fn false_schema_rejects_everything() {
        assert_eq!(
            run(r#"{"properties":{"a":false}}"#, r#"{"a":1}"#),
            ["False schema does not allow 1 (at a)"]
        );
    }

    #[test]
    fn issues_are_sorted_by_path_and_stable() {
        let schema =
            r#"{"required":["z"],"properties":{"b":{"type":"string"},"a":{"type":"string"}}}"#;
        assert_eq!(
            run(schema, r#"{"a":1,"b":2}"#),
            [
                "'z' is a required property (at <root>)",
                "1 is not of type 'string' (at a)",
                "2 is not of type 'string' (at b)",
            ]
        );
    }

    #[test]
    fn unsupported_schemas_are_rejected_up_front() {
        for schema in [
            r#"{"anyOf":[]}"#,
            r#"{"minimum":1}"#,
            r#"{"pattern":"\\d"}"#,
            r#"{"type":"strng"}"#,
            r#"{"properties":{"a":{"format":"uri"}}}"#,
            r#"{"minLength":-1}"#,
            r#"{"required":"a"}"#,
            "[]",
        ] {
            assert!(check_schema(&json(schema)).is_err(), "{schema}");
        }
    }

    #[test]
    fn json_equality_follows_python_rules() {
        assert!(json_equal(&json("1"), &json("1.0")));
        assert!(!json_equal(&json("1"), &json("true")));
        assert!(!json_equal(&json("0"), &json("false")));
        assert!(json_equal(
            &json("[1,{\"a\":null}]"),
            &json("[1.0,{\"a\":null}]")
        ));
        assert!(!json_equal(&json("[1]"), &json("[1,2]")));
        assert!(!json_equal(&json(r#"{"a":1}"#), &json(r#"{"b":1}"#)));
    }
}
