//! Bridge output frames.

use super::protocol::BridgeError;
use toolkit_core::Json;

const FRAME_CAP: usize = 131_072;
const FAILURE_FRAME: &[u8] = b"{\"ok\":false,\"error\":\"AgentCore request failed\"}\n";

/// Fixed failure frames never include untrusted input.
#[must_use]
pub fn error_frame(error: BridgeError) -> (Vec<u8>, i32) {
    let (bytes, status): (&[u8], i32) = match error {
        BridgeError::Input => (b"{\"ok\":false,\"error\":\"invalid bridge request\"}\n", 2),
        BridgeError::AgentCore => (FAILURE_FRAME, 1),
        BridgeError::Unavailable => (
            b"{\"ok\":false,\"error\":\"payment support unavailable\"}\n",
            1,
        ),
    };
    (bytes.to_vec(), status)
}

/// Render one complete success frame, replacing an oversized frame atomically.
#[must_use]
pub fn frame(value: &Json) -> (Vec<u8>, i32) {
    let mut bytes = crate::aurora::pyfmt::dumps_compact(value).into_bytes();
    bytes.push(b'\n');
    if bytes.len() > FRAME_CAP {
        (FAILURE_FRAME.to_vec(), 1)
    } else {
        (bytes, 0)
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "ISO normalization and validation share one bounded input"
)]
fn timestamp(value: &Json) -> Result<Json, BridgeError> {
    let text = value.as_str().ok_or(BridgeError::AgentCore)?;
    if !text.is_ascii() {
        return Err(BridgeError::AgentCore);
    }
    let date_len = if text.as_bytes().get(4) == Some(&b'-') {
        10
    } else {
        8
    };
    if text.len() < date_len {
        return Err(BridgeError::AgentCore);
    }
    let date = &text[..date_len];
    let date = if date_len == 8 {
        format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..8])
    } else {
        date.to_owned()
    };
    let rest = &text[date_len..];
    let time = if rest.is_empty() {
        "00:00:00".to_owned()
    } else {
        if !matches!(rest.as_bytes().first(), Some(b'T' | b't' | b' ')) {
            return Err(BridgeError::AgentCore);
        }
        let raw = &rest[1..];
        let split = raw.find(['Z', 'z', '+', '-']).unwrap_or(raw.len());
        let (clock, zone) = raw.split_at(split);
        let frac = clock.find(['.', ',']).unwrap_or(clock.len());
        let (clock, fraction) = clock.split_at(frac);
        let digits: String = clock.chars().filter(|c| *c != ':').collect();
        if !matches!(digits.len(), 2 | 4 | 6)
            || !digits.bytes().all(|b| b.is_ascii_digit())
            || (clock.contains(':')
                && !(clock.len() == 5 && clock.as_bytes()[2] == b':'
                    || clock.len() == 8
                        && clock.as_bytes()[2] == b':'
                        && clock.as_bytes()[5] == b':'))
        {
            return Err(BridgeError::AgentCore);
        }
        let minute = digits.get(2..4).unwrap_or("00");
        let second = digits.get(4..6).unwrap_or("00");
        let (minute, second, fraction) = if !fraction.is_empty() && digits.len() < 6 {
            let raw = &fraction[1..];
            if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(BridgeError::AgentCore);
            }
            let mut padded: String = raw.chars().take(6).collect();
            while padded.len() < 6 {
                padded.push('0');
            }
            let part: u32 = padded.parse().map_err(|_| BridgeError::AgentCore)?;
            let converted = format!("{:02}", part * 60 / 1_000_000);
            if digits.len() == 2 {
                (converted, "00".to_owned(), String::new())
            } else {
                (minute.to_owned(), converted, String::new())
            }
        } else {
            (
                minute.to_owned(),
                second.to_owned(),
                fraction.replace(',', "."),
            )
        };
        format!(
            "{}:{minute}:{second}{}{}",
            &digits[..2],
            fraction,
            zone.replace('z', "Z")
        )
    };
    let canonical = format!("{date}T{time}");
    let zone_at = canonical[19..]
        .find(['Z', '+', '-'])
        .map_or(canonical.len(), |index| index + 19);
    crate::cloudwatch::timefmt::parse_iso_timestamp(&canonical[..zone_at])
        .map_err(|_| BridgeError::AgentCore)?;
    if zone_at < canonical.len() {
        let offset_probe = format!("2000-01-01T00:00:00{}", &canonical[zone_at..]);
        crate::cloudwatch::timefmt::parse_iso_timestamp(&offset_probe)
            .map_err(|_| BridgeError::AgentCore)?;
    }
    let tail = &canonical[19..];
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
        "Z" | "z" | "+00" | "+0000" | "+00:00" | "-00" | "-0000" | "-00:00" => "+00:00".to_owned(),
        z if z.len() == 3 && (z.starts_with('+') || z.starts_with('-')) => format!("{z}:00"),
        z if z.len() == 5 && (z.starts_with('+') || z.starts_with('-')) => {
            format!("{}:{}", &z[..3], &z[3..])
        }
        z => z.to_owned(),
    };
    Ok(Json::string(format!(
        "{}{fraction}{zone}",
        &canonical[..19]
    )))
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
///
/// # Errors
/// Returns an `AgentCore` error when required model data is missing or malformed.
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
                let value = toolkit_aws::recorded::optional_object(session, key)
                    .map_err(|_| BridgeError::AgentCore)?
                    .ok_or(BridgeError::AgentCore)?;
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
///
/// # Errors
/// Returns an `AgentCore` error when the payload is not a JSON object.
// recorded-shape: proof payload is required as an object; every other shape is rejected.
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

    // T-BR-OUT-2: dateutil accepts ISO date-only and shortened time forms.
    #[test]
    fn t_br_out_timestamp_iso_forms() {
        for (input, expected) in [
            ("2026-08-07", "2026-08-07T00:00:00"),
            ("2026-08-07 12:34", "2026-08-07T12:34:00"),
            ("2026-08-07T12:34Z", "2026-08-07T12:34:00+00:00"),
            ("20260807T123456+0530", "2026-08-07T12:34:56+05:30"),
            ("2026-08-07t12:34:56-04", "2026-08-07T12:34:56-04:00"),
            ("2026-08-07T12:34:56.000000Z", "2026-08-07T12:34:56+00:00"),
            (
                "2026-08-07T12:34:56.1234567+05:30",
                "2026-08-07T12:34:56.123456+05:30",
            ),
            ("2026-08-07T12:34:56,5", "2026-08-07T12:34:56.500000"),
        ] {
            assert_eq!(
                timestamp(&Json::string(input)).unwrap().as_str(),
                Some(expected),
                "{input}"
            );
        }
        for input in [
            "2026-02-30",
            "2026-08-07T25:00:00Z",
            "2026-08-07T12:34:60Z",
            "2026-08-07T12:34:56+24:00",
            "2026-08-07T12:34:56.",
            "2026-13-07T12:34:56Z",
            "not-a-date",
        ] {
            assert!(timestamp(&Json::string(input)).is_err(), "{input}");
        }
    }

    #[test]
    fn t_br_out_timestamp_fractional_minutes() {
        assert_eq!(
            timestamp(&Json::string("2026-08-07T12:34.5Z"))
                .unwrap()
                .as_str(),
            Some("2026-08-07T12:34:30+00:00")
        );
        assert_eq!(
            timestamp(&Json::string("2026-08-07T12.5Z"))
                .unwrap()
                .as_str(),
            Some("2026-08-07T12:30:00+00:00")
        );
        assert_eq!(
            timestamp(&Json::string("2026-08-07T12:34.25+0530"))
                .unwrap()
                .as_str(),
            Some("2026-08-07T12:34:15+05:30")
        );
    }

    #[test]
    fn t_br_out_timestamp_offset_at_calendar_bounds() {
        for input in ["0001-01-01T00:00:00+01:00", "9999-12-31T23:59:59-01:00"] {
            assert_eq!(
                timestamp(&Json::string(input)).unwrap().as_str(),
                Some(input)
            );
        }
    }

    // T-BR-OUT-CAP: the encoded document and the final LF share the cap.
    #[test]
    fn t_br_out_cap_ascii_and_escapes() {
        for prefix in ["", "é", "😀"] {
            let encoded_prefix = match prefix {
                "é" => 6,
                "😀" => 12,
                _ => 0,
            };
            for size in [131_072, 131_073] {
                let value =
                    Json::String(format!("{prefix}{}", "x".repeat(size - encoded_prefix - 3)));
                let (frame, status) = frame(&value);
                if size == 131_072 {
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

    #[test]
    fn t_br_out_cap_session_fixtures() {
        let spec = toolkit_core::parse_json(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/openclaw/frame-cases.json"
        )))
        .unwrap();
        let Json::Array(cases) = spec.get("cases").unwrap() else {
            panic!("fixture cases")
        };
        let session = |amount: String| {
            Json::object(vec![
                ("paymentSessionId", Json::string("s")),
                ("paymentManagerArn", Json::string("m")),
                (
                    "limits",
                    Json::object(vec![(
                        "maxSpendAmount",
                        Json::object(vec![
                            ("value", Json::string(amount)),
                            ("currency", Json::string("USD")),
                        ]),
                    )]),
                ),
                ("userId", Json::string("u")),
                ("expiryTimeInMinutes", Json::Int(10)),
                ("createdAt", Json::string("2026-08-07T00:00:00Z")),
                ("updatedAt", Json::string("2026-08-07T00:00:00Z")),
            ])
        };
        for case in cases {
            let text = case.get("text").unwrap().as_str().unwrap();
            let Some(Json::Int(repeat)) = case.get("repeat") else {
                panic!("repeat")
            };
            let prefix = text.repeat(usize::try_from(*repeat).unwrap());
            let initial = session_result(&session(prefix.clone())).unwrap();
            let initial_len = crate::aurora::pyfmt::dumps_compact(&initial).len() + 1;
            for target in [131_072, 131_073] {
                let value = session_result(&session(format!(
                    "{prefix}{}",
                    "x".repeat(target - initial_len)
                )))
                .unwrap();
                let expected = format!("{}\n", crate::aurora::pyfmt::dumps_compact(&value));
                assert_eq!(expected.len(), target);
                let (actual, code) = frame(&value);
                if target == 131_072 {
                    assert_eq!(actual, expected.as_bytes());
                    assert_eq!(code, 0);
                } else {
                    assert_eq!(actual, FAILURE_FRAME);
                    assert_eq!(code, 1);
                }
            }
        }
    }

    #[test]
    fn t_br_out_cap_shared_exact_bytes() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../plugins/aws-agents/skills/agents-pay/packages/openclaw/test/fixtures/bridge-contract/output-frame-cap",
        );
        for (name, prefix) in [("ascii", ""), ("bmp", "é"), ("astral", "😀")] {
            for target in [131_072, 131_073] {
                let expected = std::fs::read(root.join(format!("{name}-{target}.json"))).unwrap();
                assert_eq!(expected.len(), target);
                let session = Json::object(vec![
                    ("paymentSessionId", Json::string("s")),
                    ("paymentManagerArn", Json::string("m")),
                    (
                        "limits",
                        Json::object(vec![(
                            "maxSpendAmount",
                            Json::object(vec![
                                ("value", Json::string(prefix)),
                                ("currency", Json::string("USD")),
                            ]),
                        )]),
                    ),
                    ("userId", Json::string("u")),
                    ("expiryTimeInMinutes", Json::Int(10)),
                    ("createdAt", Json::string("2026-08-07T00:00:00Z")),
                    ("updatedAt", Json::string("2026-08-07T00:00:00Z")),
                ]);
                let base = session_result(&session).unwrap();
                let padding = target - (crate::aurora::pyfmt::dumps_compact(&base).len() + 1);
                let mut session = session;
                if let Json::Object(fields) = &mut session {
                    let limits = &mut fields.iter_mut().find(|(k, _)| k == "limits").unwrap().1;
                    if let Json::Object(fields) = limits {
                        if let Json::Object(amount) = &mut fields[0].1 {
                            amount[0].1 = Json::string(format!("{prefix}{}", "x".repeat(padding)));
                        }
                    }
                }
                let value = session_result(&session).unwrap();
                let encoded = format!("{}\n", crate::aurora::pyfmt::dumps_compact(&value));
                assert_eq!(encoded.as_bytes(), expected);
                let (actual, status) = frame(&value);
                if target == 131_072 {
                    assert_eq!(actual, expected);
                    assert_eq!(status, 0);
                } else {
                    assert_eq!(
                        actual,
                        std::fs::read(root.join("replacement.json")).unwrap()
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
    fn t_br_out_shared_wire_and_frame() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-agents/skills/agents-pay/packages/openclaw/test/fixtures/bridge-contract");
        let wire = toolkit_core::parse_json(
            &std::fs::read_to_string(root.join("get_payment_session.wire.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            frame(&session_result(&wire).unwrap()),
            (
                std::fs::read(root.join("get_payment_session.bridge-output.json")).unwrap(),
                0
            )
        );
        let wire = toolkit_core::parse_json(
            &std::fs::read_to_string(root.join("process_payment.wire.json")).unwrap(),
        )
        .unwrap();
        let payload = wire
            .get("paymentOutput")
            .unwrap()
            .get("cryptoX402")
            .unwrap()
            .get("payload")
            .unwrap();
        assert_eq!(
            frame(&payment_result(payload).unwrap()),
            (
                std::fs::read(root.join("process_payment.bridge-output.json")).unwrap(),
                0
            )
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

    // #5411, T-BR-OUT-REQ: removing exactly one model-required member from an
    // otherwise valid session is an AgentCore failure (exit 1), member by member.
    #[test]
    fn t_br_out_req_each_required_member() {
        let valid = toolkit_core::parse_json(
            r#"{"paymentSessionId":"s","paymentManagerArn":"m","limits":{"maxSpendAmount":{"value":"1","currency":"USD"}},"userId":"u","expiryTimeInMinutes":10,"createdAt":"2026-08-07T00:00:00Z","availableLimits":{"availableSpendAmount":{"value":"1","currency":"USD"},"updatedAt":"2026-08-07T00:00:00Z"},"updatedAt":"2026-08-07T00:00:00Z"}"#,
        )
        .unwrap();
        assert!(session_result(&valid).is_ok());
        let without = |key: &str| {
            let Json::Object(fields) = &valid else {
                unreachable!()
            };
            Json::Object(fields.iter().filter(|(k, _)| k != key).cloned().collect())
        };
        for key in [
            "paymentSessionId",
            "paymentManagerArn",
            "userId",
            "expiryTimeInMinutes",
            "createdAt",
            "updatedAt",
        ] {
            let session = without(key);
            assert_eq!(
                session_result(&session),
                Err(BridgeError::AgentCore),
                "{key}"
            );
        }
        // `limits` and `availableLimits` are optional in the model.
        assert!(session_result(&without("limits")).is_ok());
        assert!(session_result(&without("availableLimits")).is_ok());
        // Required members inside `limits.maxSpendAmount`.
        let limits_without = |key: &str| {
            let Json::Object(fields) = &valid else {
                unreachable!()
            };
            let limits = if key == "maxSpendAmount" {
                Json::object(vec![])
            } else {
                let amount = [("value", "1"), ("currency", "USD")]
                    .into_iter()
                    .filter(|(k, _)| *k != key)
                    .map(|(k, v)| (k.to_owned(), Json::string(v)))
                    .collect();
                Json::object(vec![("maxSpendAmount", Json::Object(amount))])
            };
            Json::Object(
                fields
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            if k == "limits" {
                                limits.clone()
                            } else {
                                v.clone()
                            },
                        )
                    })
                    .collect(),
            )
        };
        assert_eq!(
            error_frame(BridgeError::AgentCore),
            (FAILURE_FRAME.to_vec(), 1)
        );
        for key in ["maxSpendAmount", "value", "currency"] {
            assert_eq!(
                session_result(&limits_without(key)),
                Err(BridgeError::AgentCore),
                "limits.{key}"
            );
        }
    }

    #[test]
    fn t_br_out_error_vocabulary() {
        for (error, expected, status) in [
            (BridgeError::Input, "invalid bridge request", 2),
            (BridgeError::AgentCore, "AgentCore request failed", 1),
            (BridgeError::Unavailable, "payment support unavailable", 1),
        ] {
            let (actual, exit) = error_frame(error);
            assert_eq!(
                actual,
                format!("{{\"ok\":false,\"error\":\"{expected}\"}}\n").as_bytes()
            );
            assert_eq!(exit, status);
            assert!(actual.len() <= 131_072);
        }
    }
}
