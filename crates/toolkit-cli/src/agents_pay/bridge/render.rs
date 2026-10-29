//! Bridge output frames.

use super::protocol::BridgeError;
use toolkit_core::Json;

const FRAME_CAP: usize = 131_072;
const FAILURE_FRAME: &[u8] = b"{\"ok\":false,\"error\":\"AgentCore request failed\"}\n";

/// Render one complete success frame, replacing an oversized frame atomically.
pub fn frame(value: &Json) -> (Vec<u8>, i32) {
    let mut bytes = crate::aurora::pyfmt::dumps_compact(value).into_bytes();
    bytes.push(b'\n');
    if bytes.len() > FRAME_CAP {
        (FAILURE_FRAME.to_vec(), 1)
    } else {
        (bytes, 0)
    }
}

fn timestamp(value: &Json) -> Result<Json, BridgeError> {
    let text = value.as_str().ok_or(BridgeError::AgentCore)?;
    crate::cloudwatch::timefmt::parse_iso_timestamp(text).map_err(|_| BridgeError::AgentCore)?;
    if text.len() < 19 || !text.is_ascii() || &text[10..11] != "T" {
        return Err(BridgeError::AgentCore);
    }
    let tail = &text[19..];
    let (fraction, zone) = if let Some(rest) = tail.strip_prefix('.') {
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let digits = &rest[..end];
        let mut micro: String = digits.chars().take(6).collect();
        while micro.len() < 6 {
            micro.push('0');
        }
        (
            if micro == "000000" {
                String::new()
            } else {
                format!(".{micro}")
            },
            &rest[end..],
        )
    } else {
        (String::new(), tail)
    };
    let zone = match zone {
        "Z" | "+00" | "+0000" | "+00:00" | "-00" | "-0000" | "-00:00" => "+00:00".to_owned(),
        z if z.len() == 3 && (z.starts_with('+') || z.starts_with('-')) => format!("{z}:00"),
        z if z.len() == 5 && (z.starts_with('+') || z.starts_with('-')) => {
            format!("{}:{}", &z[..3], &z[3..])
        }
        z => z.to_owned(),
    };
    Ok(Json::string(format!("{}{fraction}{zone}", &text[..19])))
}

fn amount(value: &Json) -> Result<Json, BridgeError> {
    let val = value
        .get("value")
        .and_then(Json::as_str)
        .ok_or(BridgeError::AgentCore)?;
    let currency = value
        .get("currency")
        .and_then(Json::as_str)
        .ok_or(BridgeError::AgentCore)?;
    Ok(Json::object(vec![
        ("value", Json::string(val)),
        ("currency", Json::string(currency)),
    ]))
}

/// Render the S4 session model in service-model member order.
pub fn session_result(session: &Json) -> Result<Json, BridgeError> {
    let mut fields = Vec::new();
    for key in [
        "paymentSessionId",
        "paymentManagerArn",
        "limits",
        "userId",
        "expiryTimeInMinutes",
        "createdAt",
        "availableLimits",
        "updatedAt",
    ] {
        let Some(value) = session.get(key) else {
            if matches!(key, "limits" | "availableLimits") {
                continue;
            }
            return Err(BridgeError::AgentCore);
        };
        let rendered = match key {
            "createdAt" | "updatedAt" => timestamp(value)?,
            "limits" => Json::object(vec![(
                "maxSpendAmount",
                amount(value.get("maxSpendAmount").ok_or(BridgeError::AgentCore)?)?,
            )]),
            "availableLimits" => {
                if !matches!(value, Json::Object(_)) {
                    return Err(BridgeError::AgentCore);
                }
                let mut parts = Vec::new();
                if let Some(v) = value.get("availableSpendAmount") {
                    parts.push(("availableSpendAmount", amount(v)?));
                }
                if let Some(v) = value.get("updatedAt") {
                    parts.push(("updatedAt", timestamp(v)?));
                }
                Json::object(parts)
            }
            "expiryTimeInMinutes" if matches!(value, Json::Int(_)) => value.clone(),
            "paymentSessionId" | "paymentManagerArn" | "userId"
                if matches!(value, Json::String(_)) =>
            {
                value.clone()
            }
            _ => return Err(BridgeError::AgentCore),
        };
        fields.push((key, rendered));
    }
    Ok(Json::object(vec![
        ("ok", Json::Bool(true)),
        (
            "result",
            Json::object(vec![("paymentSession", Json::object(fields))]),
        ),
    ]))
}

/// Render an already verified proof payload as the reduced bridge response.
pub fn payment_result(payload: &Json) -> Result<Json, BridgeError> {
    let normalized = match payload {
        Json::String(text) => toolkit_core::parse_json(text).map_err(|_| BridgeError::AgentCore)?,
        _ => payload.clone(),
    };
    if !matches!(normalized, Json::Object(_)) {
        return Err(BridgeError::AgentCore);
    }
    Ok(Json::object(vec![
        ("ok", Json::Bool(true)),
        (
            "result",
            Json::object(vec![(
                "paymentOutput",
                Json::object(vec![(
                    "cryptoX402",
                    Json::object(vec![("payload", normalized)]),
                )]),
            )]),
        ),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::Json;

    // T-BR-OUT-CAP: the encoded document and the final LF share the cap.
    #[test]
    fn t_br_out_cap_ascii_and_escapes() {
        for prefix in ["", "é", "😀"] {
            let encoded_prefix = match prefix {
                "é" => 6,
                "😀" => 12,
                _ => 0,
            };
            for size in [131072, 131073] {
                let value =
                    Json::String(format!("{prefix}{}", "x".repeat(size - encoded_prefix - 3)));
                let (frame, status) = frame(&value);
                if size == 131072 {
                    assert_eq!(frame.len(), size);
                    assert_eq!(frame.last(), Some(&b'\n'));
                    assert_eq!(status, 0);
                } else {
                    assert_eq!(
                        frame,
                        b"{\"ok\":false,\"error\":\"AgentCore request failed\"}\n"
                    );
                    assert_eq!(status, 1);
                }
            }
        }
    }

    // T-BR-OUT-2/3/REQ: model order, timestamp normalization, reduced proof.
    #[test]
    fn t_br_out_session_and_payment() {
        let wire = toolkit_core::parse_json(r#"{"unknown":"drop","updatedAt":"2026-08-07T00:00:00.1234567Z","createdAt":"2026-08-07T00:00:00.5Z","expiryTimeInMinutes":10,"userId":"u","paymentManagerArn":"m","paymentSessionId":"s"}"#).unwrap();
        let rendered = session_result(&wire).unwrap();
        assert_eq!(
            crate::aurora::pyfmt::dumps_compact(&rendered),
            r#"{"ok":true,"result":{"paymentSession":{"paymentSessionId":"s","paymentManagerArn":"m","userId":"u","expiryTimeInMinutes":10,"createdAt":"2026-08-07T00:00:00.500000+00:00","updatedAt":"2026-08-07T00:00:00.123456+00:00"}}}"#
        );
        let bad = toolkit_core::parse_json(r#"{"updatedAt":"2026-08-07T00:00:00Z"}"#).unwrap();
        assert!(session_result(&bad).is_err());
        let payload = Json::string(r#"{"signature":"abc"}"#);
        assert_eq!(
            crate::aurora::pyfmt::dumps_compact(&payment_result(&payload).unwrap()),
            r#"{"ok":true,"result":{"paymentOutput":{"cryptoX402":{"payload":{"signature":"abc"}}}}}"#
        );
    }

    #[test]
    fn t_br_out_timestamp_and_nested_shapes() {
        let session = toolkit_core::parse_json(r#"{"paymentSessionId":"s","paymentManagerArn":"m","userId":"u","expiryTimeInMinutes":10,"createdAt":"2026-08-07T00:00:00+0530","updatedAt":"2026-08-07T00:00:00Z"}"#).unwrap();
        let result = session_result(&session).unwrap();
        assert_eq!(
            result
                .get("result")
                .unwrap()
                .get("paymentSession")
                .unwrap()
                .get("createdAt")
                .and_then(Json::as_str),
            Some("2026-08-07T00:00:00+05:30")
        );
        let mut bad = session.clone();
        if let Json::Object(fields) = &mut bad {
            fields.push(("availableLimits".into(), Json::Null));
        }
        assert!(session_result(&bad).is_err());
        let mut bad = session;
        if let Json::Object(fields) = &mut bad {
            fields
                .iter_mut()
                .find(|(key, _)| key == "createdAt")
                .unwrap()
                .1 = Json::Int(1);
        }
        assert!(session_result(&bad).is_err());
    }
}
