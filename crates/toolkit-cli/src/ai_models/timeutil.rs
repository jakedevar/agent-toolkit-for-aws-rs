//! Python `datetime` renderings used by the model tools, on the shared
//! [`UtcTime`] of the `CloudWatch` port.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use toolkit_core::Json;

use crate::cloudwatch::timefmt::UtcTime;

/// `datetime.isoformat()` of a UTC instant: microseconds only when non-zero,
/// offset `+00:00`.
pub(super) fn iso_utc(time: UtcTime) -> String {
    let c = time.civil();
    let mut text = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        c.year, c.month, c.day, c.hour, c.minute, c.second
    );
    if c.micro != 0 {
        let _ = write!(text, ".{:06}", c.micro);
    }
    text.push_str("+00:00");
    text
}

/// `str(datetime)` of a UTC instant: a space instead of the `T`.
pub(super) fn str_utc(time: UtcTime) -> String {
    iso_utc(time).replacen('T', " ", 1)
}

/// `isoformat()` of the instant `seconds` after the epoch; a value outside the
/// `datetime` range is shown as its number, which keeps the field visible
/// instead of dropping it.
pub(super) fn epoch_to_iso(seconds: f64) -> String {
    UtcTime::from_epoch_seconds(seconds).map_or_else(|| format!("{seconds}"), iso_utc)
}

/// `str(value)` of a timestamp member of an API response: `None` for a missing
/// or null member, strings unchanged, epoch seconds as `str(datetime)`.
pub(super) fn timestamp_text(value: Option<&Json>) -> String {
    match value {
        None | Some(Json::Null) => "None".to_string(),
        Some(Json::String(text)) => text.clone(),
        Some(Json::Int(seconds)) => seconds
            .checked_mul(1_000_000)
            .and_then(UtcTime::from_epoch_micros)
            .map_or_else(|| seconds.to_string(), str_utc),
        Some(Json::Float(seconds)) => {
            UtcTime::from_epoch_seconds(*seconds).map_or_else(|| format!("{seconds}"), str_utc)
        }
        Some(other) => crate::dynamodb::pyfmt::text(other),
    }
}

/// The current instant (the system clock), or `None` if it is before 1970 or
/// outside the `datetime` range.
pub(super) fn now() -> Option<UtcTime> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let micros = i64::try_from(elapsed.as_micros()).ok()?;
    UtcTime::from_epoch_micros(micros)
}
