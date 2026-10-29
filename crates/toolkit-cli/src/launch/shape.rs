//! Model-driven Launch API output shaping (R-API-SHAPE, design section 1.4).
//!
//! Values are [`toolkit_core::Json`], whose objects keep insertion order, so
//! the model's member order and a map's response order reach the output
//! without a crate-wide ordered-map feature of `serde_json`.
use std::sync::OnceLock;

use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use toolkit_core::{parse_json, Json};

const MODEL: &str = include_str!(
    "../../../../skills/core-skills/launch-with-aws/references/launchwithaws-2026-06-15.json"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InvalidResponse;

fn shapes() -> Result<&'static Json, InvalidResponse> {
    static MODEL_SHAPES: OnceLock<Option<Json>> = OnceLock::new();
    MODEL_SHAPES
        .get_or_init(|| {
            parse_json(MODEL)
                .ok()
                .and_then(|model| model.get("shapes").cloned())
        })
        .as_ref()
        .ok_or(InvalidResponse)
}

/// Shape a response body using the bundled service model.
pub(crate) fn shape(name: &str, input: &str) -> Result<Json, InvalidResponse> {
    let value = parse_json(input).map_err(|_| InvalidResponse)?;
    visit(shapes()?, name, &value)
}

fn text<'a>(spec: &'a Json, key: &str) -> Result<&'a str, InvalidResponse> {
    spec.get(key).and_then(Json::as_str).ok_or(InvalidResponse)
}

fn member_shape<'a>(spec: &'a Json, key: &str) -> Result<&'a str, InvalidResponse> {
    spec.get(key)
        .and_then(|member| member.get("shape"))
        .and_then(Json::as_str)
        .ok_or(InvalidResponse)
}

// recorded-shape: the bundled service model drives this recursive validator; every structure, list and map is matched against its model type and any other shape is rejected as an invalid response.
fn visit(shapes: &Json, name: &str, value: &Json) -> Result<Json, InvalidResponse> {
    let spec = shapes.get(name).ok_or(InvalidResponse)?;
    match text(spec, "type")? {
        "structure" => {
            let Json::Object(input) = value else {
                return Err(InvalidResponse);
            };
            let Some(Json::Object(members)) = spec.get("members") else {
                return Err(InvalidResponse);
            };
            if spec.get("union") == Some(&Json::Bool(true)) {
                // botocore ignores `__type` and `null` members, requires exactly one
                // member set and turns an unknown one into SDK_UNKNOWN_MEMBER
                // (section 1.4 point 5).
                let mut set = input
                    .iter()
                    .filter(|(k, v)| k != "__type" && *v != Json::Null);
                let (key, val) = set.next().ok_or(InvalidResponse)?;
                if set.next().is_some() {
                    return Err(InvalidResponse);
                }
                return Ok(match members.iter().find(|(m, _)| m == key) {
                    Some((_, member)) => Json::Object(vec![(
                        key.clone(),
                        visit(shapes, text(member, "shape")?, val)?,
                    )]),
                    None => Json::Object(vec![(
                        "SDK_UNKNOWN_MEMBER".into(),
                        Json::Object(vec![("name".into(), Json::String(key.clone()))]),
                    )]),
                });
            }
            let required: Vec<&str> = match spec.get("required") {
                Some(Json::Array(list)) => list.iter().filter_map(Json::as_str).collect(),
                _ => Vec::new(),
            };
            let mut out = Vec::new();
            for (key, member) in members {
                match value.get(key) {
                    None | Some(Json::Null) if required.contains(&key.as_str()) => {
                        return Err(InvalidResponse)
                    }
                    None | Some(Json::Null) => {}
                    Some(val) => out.push((key.clone(), visit(shapes, text(member, "shape")?, val)?)),
                }
            }
            Ok(Json::Object(out))
        }
        "list" => {
            let item = member_shape(spec, "member")?;
            let Json::Array(items) = value else {
                return Err(InvalidResponse);
            };
            items
                .iter()
                .map(|v| visit(shapes, item, v))
                .collect::<Result<Vec<_>, _>>()
                .map(Json::Array)
        }
        "map" => {
            let item = member_shape(spec, "value")?;
            let Json::Object(entries) = value else {
                return Err(InvalidResponse);
            };
            let mut out = Vec::new();
            for (k, v) in entries {
                if *v != Json::Null {
                    out.push((k.clone(), visit(shapes, item, v)?));
                }
            }
            Ok(Json::Object(out))
        }
        "timestamp" => timestamp(value).map(Json::String),
        "float" | "double" => match value {
            #[allow(clippy::cast_precision_loss)]
            Json::Int(n) => Ok(Json::Float(*n as f64)),
            Json::Float(n) if n.is_finite() => Ok(Json::Float(*n)),
            _ => Err(InvalidResponse),
        },
        "integer" | "long" => match value {
            Json::Int(_) => Ok(value.clone()),
            _ => Err(InvalidResponse),
        },
        "boolean" => match value {
            Json::Bool(_) => Ok(value.clone()),
            _ => Err(InvalidResponse),
        },
        "string" => match value {
            Json::String(_) => Ok(value.clone()),
            _ => Err(InvalidResponse),
        },
        _ => Err(InvalidResponse),
    }
}

/// botocore's `parse_timestamp` as printed by `str(datetime)`: an RFC 3339
/// string keeps its offset, an epoch number is UTC; microseconds are shown
/// only when non-zero; a fraction finer than a microsecond is truncated
/// (dateutil) for strings and rounded half to even (`fromtimestamp`) for
/// epoch numbers.
fn timestamp(value: &Json) -> Result<String, InvalidResponse> {
    let time = match value {
        Json::String(s) => OffsetDateTime::parse(s, &Rfc3339).map_err(|_| InvalidResponse)?,
        Json::Int(seconds) => {
            OffsetDateTime::from_unix_timestamp(*seconds).map_err(|_| InvalidResponse)?
        }
        Json::Float(seconds) => {
            if !seconds.is_finite() || seconds.abs() > 253_402_300_799.0 {
                return Err(InvalidResponse);
            }
            // Python `datetime._fromtimestamp`: `frac, t = modf(t)`,
            // `us = round(frac * 1e6)` (half to even), then carry.
            let mut whole = seconds.trunc();
            let mut micros = ((seconds - whole) * 1e6).round_ties_even();
            if micros >= 1e6 {
                whole += 1.0;
                micros -= 1e6;
            } else if micros < 0.0 {
                whole -= 1.0;
                micros += 1e6;
            }
            #[allow(clippy::cast_possible_truncation)]
            let nanos = i128::from(whole as i64) * 1_000_000_000 + i128::from(micros as i64) * 1000;
            OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| InvalidResponse)?
        }
        _ => return Err(InvalidResponse),
    };
    if !(1..=9999).contains(&time.year()) {
        return Err(InvalidResponse);
    }
    let offset = time.offset().whole_seconds();
    let sign = if offset < 0 { '-' } else { '+' };
    let offset = offset.unsigned_abs();
    let micros = time.microsecond();
    let fraction = if micros == 0 {
        String::new()
    } else {
        format!(".{micros:06}")
    };
    Ok(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}{fraction}{sign}{:02}:{:02}",
        time.year(),
        u8::from(time.month()),
        time.day(),
        time.hour(),
        time.minute(),
        time.second(),
        offset / 3600,
        offset % 3600 / 60
    ))
}

#[cfg(test)]
#[path = "shape/tests_shape.rs"]
mod tests_shape;
