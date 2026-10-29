//! Hour buckets, second-precision stamps and the month windows of
//! `metering_core.py`, on the shared [`UtcTime`].

use std::fmt::Write as _;

use crate::cloudwatch::timefmt::{Civil, UtcTime};

const MICROS_PER_HOUR: i64 = 3_600_000_000;

/// `now.replace(minute=0, second=0, microsecond=0)`.
pub(super) fn floor_hour(now: UtcTime) -> UtcTime {
    let micros = now.epoch_micros();
    UtcTime::from_epoch_micros(micros - micros.rem_euclid(MICROS_PER_HOUR)).unwrap_or(now)
}

/// `time + hours` (negative moves back); a result outside Python's
/// `datetime` range leaves the time unchanged.
pub(super) fn add_hours(time: UtcTime, hours: i64) -> UtcTime {
    hours
        .checked_mul(MICROS_PER_HOUR)
        .and_then(|delta| time.checked_add_micros(delta))
        .unwrap_or(time)
}

/// `strftime("%Y-%m-%dT%H")`, the hour bucket (GSI HASH).
pub(super) fn hour_bucket(time: UtcTime) -> String {
    let c = time.civil();
    format!("{:04}-{:02}-{:02}T{:02}", c.year, c.month, c.day, c.hour)
}

/// `strftime("%Y-%m-%dT%H:%M:%SZ")`, the audit stamp.
pub(super) fn stamp(time: UtcTime) -> String {
    time.format_seconds_z()
}

/// Seconds since the epoch, as `int(dt.timestamp())`.
pub(super) fn epoch(time: UtcTime) -> i64 {
    time.epoch_seconds()
}

/// `(year, month)` of the time.
pub(super) fn year_month(time: UtcTime) -> (i64, i64) {
    let c = time.civil();
    (c.year, c.month)
}

fn digits(text: &str, min: usize, max: usize) -> Option<(i64, &str)> {
    let count = text
        .bytes()
        .take(max)
        .take_while(u8::is_ascii_digit)
        .count();
    if count < min {
        return None;
    }
    Some((text[..count].parse().ok()?, &text[count..]))
}

fn civil(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> Option<UtcTime> {
    UtcTime::from_civil(Civil {
        year,
        month,
        day,
        hour,
        minute,
        second,
        micro: 0,
    })
    .ok()
}

/// `datetime.strptime(text, "%Y-%m-%dT%H")` read as UTC: a four digit year and
/// one or two digits for the other fields, like `strptime`.
pub(super) fn parse_hour_bucket(text: &str) -> Option<UtcTime> {
    let (year, rest) = digits(text, 4, 4)?;
    let (month, rest) = digits(rest.strip_prefix('-')?, 1, 2)?;
    let (day, rest) = digits(rest.strip_prefix('-')?, 1, 2)?;
    let (hour, rest) = digits(rest.strip_prefix('T')?, 1, 2)?;
    if !rest.is_empty() {
        return None;
    }
    civil(year, month, day, hour, 0, 0)
}

/// The exact second-precision suffix `YYYY-MM-DDTHH:MM:SS` of a raw-usage sort
/// key: `strptime` accepts it and formatting it again gives the same text, so
/// a fractional, truncated or unpadded suffix is refused.
pub(super) fn parse_second_stamp(text: &str) -> Option<UtcTime> {
    let bytes = text.as_bytes();
    if bytes.len() != 19 || !text.is_ascii() {
        return None;
    }
    let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    if separators.iter().any(|(at, sep)| bytes[*at] != *sep) {
        return None;
    }
    let field = |from: usize, to: usize| -> Option<i64> {
        let part = &text[from..to];
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    civil(
        field(0, 4)?,
        field(5, 7)?,
        field(8, 10)?,
        field(11, 13)?,
        field(14, 16)?,
        field(17, 19)?,
    )
}

/// `datetime.strptime(text, "%Y-%m-%d")` read as UTC midnight.
pub(super) fn parse_date(text: &str) -> Option<UtcTime> {
    let (year, rest) = digits(text, 4, 4)?;
    let (month, rest) = digits(rest.strip_prefix('-')?, 1, 2)?;
    let (day, rest) = digits(rest.strip_prefix('-')?, 1, 2)?;
    if !rest.is_empty() {
        return None;
    }
    civil(year, month, day, 0, 0, 0)
}

/// `str(datetime)` of an aware UTC time: `2026-01-02 03:04:05+00:00`, with the
/// microseconds when non-zero.
pub(super) fn python_str(time: UtcTime) -> String {
    let c = time.civil();
    let mut text = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        c.year, c.month, c.day, c.hour, c.minute, c.second
    );
    if c.micro != 0 {
        let _ = write!(text, ".{:06}", c.micro);
    }
    text.push_str("+00:00");
    text
}

#[cfg(test)]
pub(super) fn at(text: &str) -> UtcTime {
    crate::cloudwatch::timefmt::parse_iso_timestamp(text).expect("fixture time")
}
