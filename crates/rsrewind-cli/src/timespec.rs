//! Human time expressions for `--since`, `--until` and `open --at`.
//!
//! Accepted (local time unless stated):
//! - `2026-09-30 14:32`, `2026-09-30 14:32:05`, `2026-09-30T14:32`, `2026-09-30`
//! - RFC 3339 with an explicit offset: `2026-09-30T14:32:00Z`
//! - `today`, `yesterday` (local midnight), `now`
//! - relative durations back from now: `15m`, `2h`, `3d`, `1w`

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use rsrewind_core::Timestamp;

pub fn parse_timespec(input: &str, now: DateTime<Local>) -> Result<Timestamp, String> {
    let text = input.trim();
    let lower = text.to_ascii_lowercase();
    match lower.as_str() {
        "now" => return Ok(Timestamp::from(now.with_timezone(&Utc))),
        "today" => return local_midnight(now.date_naive()),
        "yesterday" => {
            let day = now
                .date_naive()
                .pred_opt()
                .ok_or_else(|| "date out of range".to_string())?;
            return local_midnight(day);
        }
        _ => {}
    }

    if let Some(millis) = relative_millis(&lower) {
        return Ok(Timestamp::from(now.with_timezone(&Utc)).saturating_sub_millis(millis));
    }

    if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
        return Ok(Timestamp::from(dt.with_timezone(&Utc)));
    }

    for format in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return local_to_timestamp(naive);
        }
    }
    if let Ok(day) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return local_midnight(day);
    }

    Err(format!(
        "could not understand time '{input}'. Try '2026-09-30 14:32', 'today', 'yesterday', '2h' or '3d'."
    ))
}

fn relative_millis(text: &str) -> Option<i64> {
    let unit = text.chars().last()?;
    let number: i64 = text[..text.len() - unit.len_utf8()].trim().parse().ok()?;
    if number < 0 {
        return None;
    }
    let scale: i64 = match unit {
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        'w' => 7 * 86_400_000,
        _ => return None,
    };
    number.checked_mul(scale)
}

fn local_midnight(day: NaiveDate) -> Result<Timestamp, String> {
    local_to_timestamp(day.and_hms_opt(0, 0, 0).ok_or("invalid date")?)
}

fn local_to_timestamp(naive: NaiveDateTime) -> Result<Timestamp, String> {
    // `earliest` resolves the DST fall-back hour deterministically; a spring-forward gap has no
    // valid local time at all, which we report rather than guess.
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|dt| Timestamp::from(dt.with_timezone(&Utc)))
        .ok_or_else(|| format!("{naive} does not exist in the local time zone (DST change)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 9, 30, 20, 0, 0)
            .earliest()
            .unwrap_or_else(Local::now)
    }

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Timestamp {
        Local
            .with_ymd_and_hms(y, mo, d, h, mi, s)
            .earliest()
            .map(|dt| Timestamp::from(dt.with_timezone(&Utc)))
            .unwrap_or(Timestamp(0))
    }

    #[test]
    fn absolute_local_times() {
        let expected = local(2026, 9, 30, 14, 32, 0);
        assert_eq!(parse_timespec("2026-09-30 14:32", now()), Ok(expected));
        assert_eq!(parse_timespec("2026-09-30T14:32", now()), Ok(expected));
        assert_eq!(
            parse_timespec("2026-09-30 14:32:05", now()),
            Ok(local(2026, 9, 30, 14, 32, 5))
        );
        assert_eq!(
            parse_timespec("2026-09-30", now()),
            Ok(local(2026, 9, 30, 0, 0, 0))
        );
    }

    #[test]
    fn rfc3339_respects_offset() {
        assert_eq!(
            parse_timespec("2026-09-30T14:32:00Z", now()),
            Ok(Timestamp(
                Utc.with_ymd_and_hms(2026, 9, 30, 14, 32, 0)
                    .single()
                    .map_or(0, |d| d.timestamp_millis())
            ))
        );
    }

    #[test]
    fn keywords_and_relative() {
        let n = Timestamp::from(now().with_timezone(&Utc));
        assert_eq!(parse_timespec("now", now()), Ok(n));
        assert_eq!(
            parse_timespec("TODAY", now()),
            Ok(local(2026, 9, 30, 0, 0, 0))
        );
        assert_eq!(
            parse_timespec("yesterday", now()),
            Ok(local(2026, 9, 29, 0, 0, 0))
        );
        assert_eq!(
            parse_timespec("2h", now()),
            Ok(n.saturating_sub_millis(7_200_000))
        );
        assert_eq!(
            parse_timespec("15m", now()),
            Ok(n.saturating_sub_millis(900_000))
        );
        assert_eq!(
            parse_timespec("1w", now()),
            Ok(n.saturating_sub_millis(604_800_000))
        );
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            "",
            "soon",
            "-2h",
            "2x",
            "h",
            "2026-13-01",
            "99999999999999999999d",
        ] {
            assert!(parse_timespec(bad, now()).is_err(), "{bad}");
        }
    }
}
