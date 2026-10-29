//! Model-driven Launch API output shaping.
use serde_json::{Map, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const MODEL: &str = include_str!("../../../../skills/core-skills/launch-with-aws/references/launchwithaws-2026-06-15.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InvalidResponse;

/// Shape a response using the bundled service model. The result is compact JSON.
pub(crate) fn shape(name: &str, input: &str) -> Result<String, InvalidResponse> {
    let model: Value = serde_json::from_str(MODEL).map_err(|_| InvalidResponse)?;
    let value: Value = serde_json::from_str(input).map_err(|_| InvalidResponse)?;
    let result = visit(&model["shapes"], name, &value)?;
    serde_json::to_string(&result).map_err(|_| InvalidResponse)
}

fn visit(shapes: &Value, name: &str, value: &Value) -> Result<Value, InvalidResponse> {
    let spec = shapes.get(name).ok_or(InvalidResponse)?;
    let kind = spec.get("type").and_then(Value::as_str).ok_or(InvalidResponse)?;
    match kind {
        "structure" => {
            let input = value.as_object().ok_or(InvalidResponse)?;
            let members = spec["members"].as_object().ok_or(InvalidResponse)?;
            if spec["union"].as_bool() == Some(true) {
                let (key, val) = input.iter().next().ok_or(InvalidResponse)?;
                if let Some(member) = members.get(key) {
                    let mut out = Map::new();
                    out.insert(key.clone(), visit(shapes, member["shape"].as_str().ok_or(InvalidResponse)?, val)?);
                    return Ok(Value::Object(out));
                }
                return Ok(serde_json::json!({"SDK_UNKNOWN_MEMBER":{"name":key}}));
            }
            let required = spec.get("required").and_then(Value::as_array);
            let mut out = Map::new();
            for (key, member) in members {
                let required = required.is_some_and(|list| list.iter().any(|v| v.as_str() == Some(key)));
                match input.get(key) {
                    None | Some(Value::Null) if required => return Err(InvalidResponse),
                    None | Some(Value::Null) => {},
                    Some(val) => { out.insert(key.clone(), visit(shapes, member["shape"].as_str().ok_or(InvalidResponse)?, val)?); }
                }
            }
            Ok(Value::Object(out))
        }
        "list" => {
            let name = spec["member"]["shape"].as_str().ok_or(InvalidResponse)?;
            value.as_array().ok_or(InvalidResponse)?.iter().map(|v| visit(shapes, name, v)).collect::<Result<Vec<_>, _>>().map(Value::Array)
        }
        "map" => {
            let name = spec["value"]["shape"].as_str().ok_or(InvalidResponse)?;
            let mut out = Map::new();
            for (k, v) in value.as_object().ok_or(InvalidResponse)? { if !v.is_null() { out.insert(k.clone(), visit(shapes, name, v)?); } }
            Ok(Value::Object(out))
        }
        "timestamp" => timestamp(value).map(Value::String),
        "float" | "double" => {
            let n = value.as_f64().ok_or(InvalidResponse)?;
            serde_json::Number::from_f64(n).map(Value::Number).ok_or(InvalidResponse)
        }
        "integer" | "long" => if value.is_i64() || value.is_u64() { Ok(value.clone()) } else { Err(InvalidResponse) },
        "boolean" => if value.is_boolean() { Ok(value.clone()) } else { Err(InvalidResponse) },
        "string" => if value.is_string() { Ok(value.clone()) } else { Err(InvalidResponse) },
        _ => Err(InvalidResponse),
    }
}
fn timestamp(value: &Value) -> Result<String, InvalidResponse> {
    let time = if let Some(s) = value.as_str() {
        OffsetDateTime::parse(s, &Rfc3339).map_err(|_| InvalidResponse)?
    } else {
        let seconds = value.as_f64().ok_or(InvalidResponse)?;
        if !seconds.is_finite() || seconds.abs() > 253_402_300_799.0 { return Err(InvalidResponse); }
        OffsetDateTime::from_unix_timestamp_nanos((seconds * 1_000_000.0).round() as i128 * 1000).map_err(|_| InvalidResponse)?
    };
    let offset = time.offset().whole_seconds();
    let sign = if offset < 0 { '-' } else { '+' };
    let offset = offset.unsigned_abs();
    let micros = time.microsecond();
    let fraction = if micros == 0 { String::new() } else { format!(".{micros:06}") };
    Ok(format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}{fraction}{sign}{:02}:{:02}", time.year(), u8::from(time.month()), time.day(), time.hour(), time.minute(), time.second(), offset / 3600, offset % 3600 / 60))
}

#[cfg(test)]
#[path = "shape/tests_shape.rs"]
mod tests_shape;
