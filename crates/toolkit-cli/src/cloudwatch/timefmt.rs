//! UTC timestamps for the CloudWatch ports (`di_formatting.py` and the
//! datetime handling of `di_status_assessment.py`).
//!
//! [`UtcTime`] is a microsecond-resolution UTC instant in Python's
//! `datetime` range (years 1 to 9999). It renders the AWS-CLI ISO shape the
//! renderers depend on and parses the ISO 8601 subset that
//! `datetime.fromisoformat` accepts, with the same error texts, so the
//! `ERROR: Invalid start_time format ... Error: <text>` messages of the
//! operations stay identical. A naive input (no offset) is read as UTC, the
//! rule `di_status_tools._parse_iso_timestamp` documents.

use std::fmt::Write as _;

use toolkit_core::Json;

use crate::dynamodb::pyfmt;

const MICROS_PER_SECOND: i64 = 1_000_000;
const SECONDS_PER_DAY: i64 = 86_400;
const MIN_MICROS: i64 = -62_135_596_800 * MICROS_PER_SECOND;
const MAX_MICROS: i64 = 253_402_300_799 * MICROS_PER_SECOND + 999_999;

/// A UTC instant with microsecond resolution (years 1 to 9999).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcTime {
    micros: i64,
}

/// Broken-down calendar fields of a [`UtcTime`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    /// Year, 1 to 9999.
    pub year: i64,
    /// Month, 1 to 12.
    pub month: i64,
    /// Day of month, 1 to 31.
    pub day: i64,
    /// Hour, 0 to 23.
    pub hour: i64,
    /// Minute, 0 to 59.
    pub minute: i64,
    /// Second, 0 to 59.
    pub second: i64,
    /// Microsecond, 0 to 999999.
    pub micro: i64,
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted = (month + 9) % 12;
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

impl UtcTime {
    /// The instant `micros` microseconds after the Unix epoch, or `None`
    /// outside years 1 to 9999.
    #[must_use]
    pub fn from_epoch_micros(micros: i64) -> Option<Self> {
        (MIN_MICROS..=MAX_MICROS)
            .contains(&micros)
            .then_some(Self { micros })
    }

    /// The instant `seconds` (possibly fractional) after the Unix epoch.
    #[must_use]
    pub fn from_epoch_seconds(seconds: f64) -> Option<Self> {
        if !seconds.is_finite() {
            return None;
        }
        let micros = (seconds * 1_000_000.0).round();
        if !((MIN_MICROS as f64)..=(MAX_MICROS as f64)).contains(&micros) {
            return None;
        }
        Self::from_epoch_micros(micros as i64)
    }

    /// Builds an instant from calendar fields, validating them like
    /// `datetime(...)` does (the error is Python's `ValueError` text).
    ///
    /// # Errors
    ///
    /// The `ValueError` message for an out-of-range field.
    pub fn from_civil(fields: Civil) -> Result<Self, String> {
        if !(1..=9999).contains(&fields.year) {
            return Err(format!("year {} is out of range", fields.year));
        }
        if !(1..=12).contains(&fields.month) {
            return Err("month must be in 1..12".to_string());
        }
        if fields.day < 1 || fields.day > days_in_month(fields.year, fields.month) {
            return Err("day is out of range for month".to_string());
        }
        if !(0..=23).contains(&fields.hour) {
            return Err("hour must be in 0..23".to_string());
        }
        if !(0..=59).contains(&fields.minute) {
            return Err("minute must be in 0..59".to_string());
        }
        if !(0..=59).contains(&fields.second) {
            return Err("second must be in 0..59".to_string());
        }
        let days = days_from_civil(fields.year, fields.month, fields.day);
        let seconds =
            days * SECONDS_PER_DAY + fields.hour * 3_600 + fields.minute * 60 + fields.second;
        Ok(Self {
            micros: seconds * MICROS_PER_SECOND + fields.micro,
        })
    }

    /// Microseconds since the Unix epoch.
    #[must_use]
    pub fn epoch_micros(self) -> i64 {
        self.micros
    }

    /// `int(dt.timestamp())`: whole seconds since the epoch, truncated
    /// toward zero.
    #[must_use]
    pub fn epoch_seconds(self) -> i64 {
        self.micros / MICROS_PER_SECOND
    }

    /// The calendar fields.
    #[must_use]
    pub fn civil(self) -> Civil {
        let total_seconds = self.micros.div_euclid(MICROS_PER_SECOND);
        let micro = self.micros.rem_euclid(MICROS_PER_SECOND);
        let days = total_seconds.div_euclid(SECONDS_PER_DAY);
        let in_day = total_seconds.rem_euclid(SECONDS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        Civil {
            year,
            month,
            day,
            hour: in_day / 3_600,
            minute: in_day % 3_600 / 60,
            second: in_day % 60,
            micro,
        }
    }

    /// This instant moved by `delta_micros` (negative moves backwards), or
    /// `None` when the result leaves years 1 to 9999 (Python raises
    /// `OverflowError` there).
    #[must_use]
    pub fn checked_add_micros(self, delta_micros: i64) -> Option<Self> {
        Self::from_epoch_micros(self.micros.checked_add(delta_micros)?)
    }

    /// `strftime("%Y-%m-%dT%H:%M:%SZ")` of the UTC instant: the AWS-CLI ISO
    /// shape, whole seconds.
    #[must_use]
    pub fn format_seconds_z(self) -> String {
        let c = self.civil();
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            c.year, c.month, c.day, c.hour, c.minute, c.second
        )
    }

    /// `isoformat().replace("+00:00", "Z")`: microseconds only when non-zero.
    #[must_use]
    pub fn isoformat_z(self) -> String {
        let c = self.civil();
        let mut text = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            c.year, c.month, c.day, c.hour, c.minute, c.second
        );
        if c.micro != 0 {
            let _ = write!(text, ".{:06}", c.micro);
        }
        text.push('Z');
        text
    }
}

/// `format_timestamp(value, default)` from `di_formatting.py`.
///
/// `null`/missing and `""` give `default`; a string passes through
/// unchanged; any other JSON value is shown with `str()`. SDK `datetime`
/// values do not exist in JSON: the client boundary (#4381) hands them over
/// as strings rendered by [`UtcTime::format_seconds_z`].
#[must_use]
pub fn format_timestamp(value: Option<&Json>, default: &str) -> String {
    match value {
        None | Some(Json::Null) => default.to_string(),
        Some(Json::String(text)) if text.is_empty() => default.to_string(),
        Some(other) => pyfmt::text(other),
    }
}

/// Reads an API timestamp field handed over as JSON: an ISO string (parsed
/// like [`parse_iso_timestamp`]) or a number of seconds since the epoch.
///
/// # Errors
///
/// A description when the value is neither.
pub fn time_from_json(value: &Json) -> Result<UtcTime, String> {
    match value {
        Json::String(text) => parse_iso_timestamp(text),
        Json::Int(seconds) => seconds
            .checked_mul(MICROS_PER_SECOND)
            .and_then(UtcTime::from_epoch_micros)
            .ok_or_else(|| "date value out of range".to_string()),
        Json::Float(seconds) => UtcTime::from_epoch_seconds(*seconds)
            .ok_or_else(|| "date value out of range".to_string()),
        other => Err(format!(
            "expected an ISO 8601 string or epoch seconds, got {}",
            other.type_name()
        )),
    }
}

fn invalid(text: &str) -> String {
    format!("Invalid isoformat string: {}", pyfmt::repr_str(text))
}

fn digits(text: &str) -> Option<i64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

struct Offset {
    micros: i64,
}

fn parse_offset(text: &str, original: &str) -> Result<Offset, String> {
    let (sign, rest) = match text.as_bytes().first() {
        Some(b'+') => (1_i64, &text[1..]),
        Some(b'-') => (-1_i64, &text[1..]),
        _ => return Err(invalid(original)),
    };
    let (hour, minute, second, micro) = parse_clock(rest).ok_or_else(|| invalid(original))?;
    if hour > 23 || minute > 59 || second > 59 {
        return if hour > 23 {
            Err("offset must be a timedelta strictly between -timedelta(hours=24) and timedelta(hours=24)".to_string())
        } else {
            Err(invalid(original))
        };
    }
    let micros = ((hour * 60 + minute) * 60 + second) * MICROS_PER_SECOND + micro;
    Ok(Offset {
        micros: sign * micros,
    })
}

/// `HH`, `HH:MM`, `HH:MM:SS`, `HH:MM:SS.f` or the basic forms `HHMM`,
/// `HHMMSS`, `HHMMSS.f`; the fraction keeps at most six digits.
fn parse_clock(text: &str) -> Option<(i64, i64, i64, i64)> {
    let (main, fraction) = match text.find(['.', ',']) {
        Some(index) => (&text[..index], Some(&text[index + 1..])),
        None => (text, None),
    };
    let (hour, minute, second) = if main.contains(':') {
        let parts: Vec<&str> = main.split(':').collect();
        if parts.is_empty() || parts.len() > 3 || parts.iter().any(|part| part.len() != 2) {
            return None;
        }
        (
            digits(parts[0])?,
            parts.get(1).map_or(Some(0), |part| digits(part))?,
            parts.get(2).map_or(Some(0), |part| digits(part))?,
        )
    } else {
        match main.len() {
            2 => (digits(main)?, 0, 0),
            4 => (digits(&main[..2])?, digits(&main[2..])?, 0),
            6 => (
                digits(&main[..2])?,
                digits(&main[2..4])?,
                digits(&main[4..])?,
            ),
            _ => return None,
        }
    };
    let micro = match fraction {
        None => 0,
        Some(fraction) => {
            if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let mut padded: String = fraction.chars().take(6).collect();
            while padded.len() < 6 {
                padded.push('0');
            }
            padded.parse().ok()?
        }
    };
    Some((hour, minute, second, micro))
}

fn parse_date(text: &str) -> Option<(i64, i64, i64)> {
    if text.len() == 10 && text.as_bytes()[4] == b'-' && text.as_bytes()[7] == b'-' {
        return Some((
            digits(&text[..4])?,
            digits(&text[5..7])?,
            digits(&text[8..])?,
        ));
    }
    if text.len() == 8 {
        return Some((
            digits(&text[..4])?,
            digits(&text[4..6])?,
            digits(&text[6..])?,
        ));
    }
    None
}

/// `datetime.fromisoformat(value.replace("Z", "+00:00"))`, with a naive
/// result read as UTC, converted to a [`UtcTime`].
///
/// Supported: `YYYY-MM-DD` / `YYYYMMDD`, optionally followed by one
/// separator character and `HH[:MM[:SS[.ffffff]]]` (or the basic form), then
/// an optional `+HH[:MM[:SS]]` / `-HH[:MM[:SS]]` offset. ISO week dates are
/// not supported (see `docs/migration/cw-domain.md`).
///
/// # Errors
///
/// Python's `ValueError` text: `Invalid isoformat string: '...'` for a
/// malformed value, or the field-range message (`month must be in 1..12`,
/// `day is out of range for month`, ...).
pub fn parse_iso_timestamp(value: &str) -> Result<UtcTime, String> {
    let text = value.replace('Z', "+00:00");
    let original = text.as_str();
    if !original.is_ascii() || original.len() < 8 {
        return Err(invalid(original));
    }
    let (date_text, rest) = if original.as_bytes().get(4) == Some(&b'-') {
        if original.len() < 10 {
            return Err(invalid(original));
        }
        (&original[..10], &original[10..])
    } else {
        (&original[..8], &original[8..])
    };
    let (year, month, day) = parse_date(date_text).ok_or_else(|| invalid(original))?;

    let (clock, offset) = if rest.is_empty() {
        ((0, 0, 0, 0), Offset { micros: 0 })
    } else {
        let time_part = &rest[1..];
        if time_part.is_empty() {
            return Err(invalid(original));
        }
        let split = time_part.find(['+', '-']);
        let (clock_text, offset_text) = match split {
            Some(index) => (&time_part[..index], Some(&time_part[index..])),
            None => (time_part, None),
        };
        let clock = parse_clock(clock_text).ok_or_else(|| invalid(original))?;
        let offset = match offset_text {
            Some(text) => parse_offset(text, original)?,
            None => Offset { micros: 0 },
        };
        (clock, offset)
    };
    let (hour, minute, second, micro) = clock;
    let local = UtcTime::from_civil(Civil {
        year,
        month,
        day,
        hour,
        minute,
        second,
        micro,
    })?;
    local
        .checked_add_micros(-offset.micros)
        .ok_or_else(|| "date value out of range".to_string())
}
