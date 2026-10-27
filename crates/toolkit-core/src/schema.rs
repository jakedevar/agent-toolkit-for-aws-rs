//! A deliberately small structural schema for [`Json`] values.
//!
//! It describes shape only: scalar types, arrays of one element schema and
//! objects with required/optional fields. It is not JSON Schema. Validation is
//! deterministic: violations are reported in schema field order, then unknown
//! members in document order, each with a `$`-rooted path such as
//! `$.items[2].name`.

use std::fmt;

use crate::json::Json;

/// Expected shape of a [`Json`] value.
#[derive(Debug, Clone, PartialEq)]
pub enum Schema {
    /// Any value.
    Any,
    /// `null`.
    Null,
    /// A boolean.
    Bool,
    /// An integer ([`Json::Int`] only; a float such as `1.0` is not one).
    Integer,
    /// An integer or a float.
    Number,
    /// A string.
    String,
    /// An array whose elements all match the inner schema.
    Array(Box<Schema>),
    /// An object with the described members.
    Object(ObjectSchema),
}

impl Schema {
    fn type_name(&self) -> &'static str {
        match self {
            Self::Any => "any value",
            Self::Null => "null",
            Self::Bool => "boolean",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::String => "string",
            Self::Array(_) => "array",
            Self::Object(_) => "object",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Field {
    name: String,
    schema: Schema,
    required: bool,
}

/// Members of an object schema. Unknown members are rejected unless
/// [`ObjectSchema::allow_unknown`] is set.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ObjectSchema {
    fields: Vec<Field>,
    allow_unknown: bool,
}

impl ObjectSchema {
    /// An object schema with no members.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a member that must be present.
    #[must_use]
    pub fn required(mut self, name: &str, schema: Schema) -> Self {
        self.fields.push(Field {
            name: name.to_string(),
            schema,
            required: true,
        });
        self
    }

    /// Adds a member that may be absent.
    #[must_use]
    pub fn optional(mut self, name: &str, schema: Schema) -> Self {
        self.fields.push(Field {
            name: name.to_string(),
            schema,
            required: false,
        });
        self
    }

    /// Accepts members that the schema does not describe.
    #[must_use]
    pub fn allow_unknown(mut self) -> Self {
        self.allow_unknown = true;
        self
    }
}

impl From<ObjectSchema> for Schema {
    fn from(object: ObjectSchema) -> Self {
        Self::Object(object)
    }
}

/// One schema mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// `$`-rooted location of the offending value.
    pub path: String,
    /// What is wrong there.
    pub message: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// Checks `value` against `schema`.
///
/// # Errors
///
/// Returns every [`Violation`] found, in deterministic order; the `Err`
/// vector is never empty.
pub fn validate(schema: &Schema, value: &Json) -> Result<(), Vec<Violation>> {
    let mut violations = Vec::new();
    check(schema, value, "$", &mut violations);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

fn check(schema: &Schema, value: &Json, path: &str, out: &mut Vec<Violation>) {
    match (schema, value) {
        (Schema::Any, _)
        | (Schema::Null, Json::Null)
        | (Schema::Bool, Json::Bool(_))
        | (Schema::Integer, Json::Int(_))
        | (Schema::Number, Json::Int(_) | Json::Float(_))
        | (Schema::String, Json::String(_)) => {}
        (Schema::Array(element), Json::Array(items)) => {
            for (index, item) in items.iter().enumerate() {
                check(element, item, &format!("{path}[{index}]"), out);
            }
        }
        (Schema::Object(object), Json::Object(pairs)) => {
            check_object(object, pairs, path, out);
        }
        (expected, found) => out.push(Violation {
            path: path.to_string(),
            message: format!(
                "expected {}, found {}",
                expected.type_name(),
                found.type_name()
            ),
        }),
    }
}

fn check_object(
    object: &ObjectSchema,
    pairs: &[(String, Json)],
    path: &str,
    out: &mut Vec<Violation>,
) {
    for field in &object.fields {
        let member_path = format!("{path}.{}", field.name);
        match pairs.iter().find(|(name, _)| *name == field.name) {
            Some((_, member)) => check(&field.schema, member, &member_path, out),
            None if field.required => out.push(Violation {
                path: member_path,
                message: "missing required member".to_string(),
            }),
            None => {}
        }
    }
    if !object.allow_unknown {
        for (name, _) in pairs {
            if !object.fields.iter().any(|field| field.name == *name) {
                out.push(Violation {
                    path: format!("{path}.{name}"),
                    message: "unknown member".to_string(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse_json;

    fn person() -> Schema {
        ObjectSchema::new()
            .required("name", Schema::String)
            .required("age", Schema::Integer)
            .optional("tags", Schema::Array(Box::new(Schema::String)))
            .into()
    }

    fn check_text(schema: &Schema, text: &str) -> Result<(), Vec<Violation>> {
        validate(schema, &parse_json(text).expect("fixture is valid json"))
    }

    fn messages(result: Result<(), Vec<Violation>>) -> Vec<String> {
        result
            .expect_err("expected violations")
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn conforming_documents_pass() {
        assert!(check_text(&person(), "{\"name\":\"a\",\"age\":3}").is_ok());
        assert!(check_text(
            &person(),
            "{\"age\":3,\"name\":\"a\",\"tags\":[\"x\",\"y\"]}"
        )
        .is_ok());
        assert!(check_text(&Schema::Any, "[1,{}]").is_ok());
        assert!(check_text(&Schema::Number, "1.5").is_ok());
        assert!(check_text(&Schema::Number, "2").is_ok());
        assert!(check_text(&Schema::Null, "null").is_ok());
        assert!(check_text(&Schema::Bool, "false").is_ok());
    }

    #[test]
    fn type_mismatches_are_located_by_path() {
        assert_eq!(
            messages(check_text(&person(), "{\"name\":1,\"age\":\"3\"}")),
            vec![
                "$.name: expected string, found integer",
                "$.age: expected integer, found string"
            ]
        );
        assert_eq!(
            messages(check_text(
                &person(),
                "{\"name\":\"a\",\"age\":3,\"tags\":[\"x\",2]}"
            )),
            vec!["$.tags[1]: expected string, found integer"]
        );
        assert_eq!(
            messages(check_text(&person(), "[]")),
            vec!["$: expected object, found array"]
        );
    }

    #[test]
    fn integer_schema_rejects_floats() {
        assert_eq!(
            messages(check_text(&Schema::Integer, "1.0")),
            vec!["$: expected integer, found number"]
        );
    }

    #[test]
    fn missing_required_and_unknown_members_are_reported() {
        assert_eq!(
            messages(check_text(&person(), "{\"extra\":true}")),
            vec![
                "$.name: missing required member",
                "$.age: missing required member",
                "$.extra: unknown member"
            ]
        );
    }

    #[test]
    fn allow_unknown_accepts_extra_members() {
        let schema: Schema = ObjectSchema::new()
            .required("id", Schema::Integer)
            .allow_unknown()
            .into();
        assert!(check_text(&schema, "{\"id\":1,\"anything\":[1,2]}").is_ok());
        assert!(check_text(&schema, "{\"anything\":1}").is_err());
    }

    #[test]
    fn violation_order_is_deterministic() {
        let first = messages(check_text(&person(), "{\"z\":1,\"a\":2}"));
        let second = messages(check_text(&person(), "{\"z\":1,\"a\":2}"));
        assert_eq!(first, second);
        assert_eq!(
            first.last().map(String::as_str),
            Some("$.a: unknown member")
        );
    }
}
