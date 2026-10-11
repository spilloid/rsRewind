//! What the agent surface may return (`[mcp]` in `config.toml`), and the small parsers its tools share.
//! Pure: every rule here is unit-tested without a database.

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};
use rsrewind_core::{McpConfig, SourceId, Timestamp, VisualStateId};
use rsrewind_query::SourceFilter;

/// The `[mcp]` settings, applied to discovery and re-checked on every call.
#[derive(Debug, Clone)]
pub struct Ceiling {
    pub enabled: bool,
    pub allow_screenshots: bool,
    /// Oldest instant any tool may return; `None` = no limit.
    pub oldest: Option<Timestamp>,
    /// `None` = every machine; otherwise exactly these (`None` inside = this machine).
    pub sources: Option<Vec<Option<SourceId>>>,
}

impl Ceiling {
    pub fn from_config(config: &McpConfig, now: Timestamp) -> Self {
        let oldest = (config.max_age_days > 0)
            .then(|| now.saturating_sub_millis(i64::from(config.max_age_days) * 86_400_000));
        let sources = (!config.sources.is_empty()).then(|| {
            config
                .sources
                .iter()
                .filter_map(|s| match s.as_str() {
                    "this" => Some(None),
                    id => SourceId::parse(id).map(Some),
                })
                .collect()
        });
        Self {
            enabled: config.enabled,
            allow_screenshots: config.allow_screenshots,
            oldest,
            sources,
        }
    }

    /// Whether history from `source` may be returned.
    pub fn admits(&self, source: Option<SourceId>) -> bool {
        self.sources
            .as_ref()
            .is_none_or(|allowed| allowed.contains(&source))
    }

    /// Whether an instant is inside the time window.
    pub fn admits_time(&self, at: Timestamp) -> bool {
        self.oldest.is_none_or(|oldest| at >= oldest)
    }

    /// The query filters that together cover exactly the admitted machines.
    pub fn filters(&self) -> Vec<SourceFilter> {
        match &self.sources {
            None => vec![SourceFilter::All],
            Some(allowed) => allowed.iter().map(|s| SourceFilter::only(*s)).collect(),
        }
    }

    /// `since` raised to the window's start.
    pub fn clamp_since(&self, since: Option<Timestamp>) -> Option<Timestamp> {
        match (since, self.oldest) {
            (Some(s), Some(o)) => Some(s.max(o)),
            (s, o) => s.or(o),
        }
    }
}

/// A moment's handle as tools exchange it: `this:123` or `<32-hex source id>:123`.
pub fn moment_id(source: Option<SourceId>, id: VisualStateId) -> String {
    match source {
        None => format!("this:{}", id.0),
        Some(s) => format!("{s}:{}", id.0),
    }
}

pub fn parse_moment_id(text: &str) -> Option<(Option<SourceId>, VisualStateId)> {
    let (machine, id) = text.trim().rsplit_once(':')?;
    let id = VisualStateId(id.parse().ok().filter(|n: &i64| *n > 0)?);
    match machine {
        "this" => Some((None, id)),
        hex => SourceId::parse(hex).map(|s| (Some(s), id)),
    }
}

/// A time an agent passes: RFC 3339 (`2026-10-10T14:05:00-04:00`), local `2026-10-10 14:05[:30]` or
/// `2026-10-10`, or a duration ago (`30m`, `2h`, `7d`).
pub fn parse_time(text: &str, now: DateTime<Local>) -> Result<Timestamp, String> {
    let t = text.trim();
    // "N ago" for minutes, hours, days: non-negative, at most 100 years back, checked arithmetic.
    for (suffix, unit_ms) in [("m", 60_000_i64), ("h", 3_600_000), ("d", 86_400_000)] {
        if let Some(n) = t.strip_suffix(suffix).and_then(|n| n.parse::<i64>().ok()) {
            const MAX_AGO_MS: i64 = 100 * 366 * 86_400_000;
            return n
                .checked_mul(unit_ms)
                .filter(|ago| (0..=MAX_AGO_MS).contains(ago))
                .and_then(|ago| now.timestamp_millis().checked_sub(ago))
                .map(Timestamp)
                .ok_or_else(|| format!("'{t}' is not a duration between 0 and 100 years"));
        }
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
        return Ok(Timestamp(dt.timestamp_millis()));
    }
    let local = |naive: NaiveDateTime| {
        Local
            .from_local_datetime(&naive)
            .earliest()
            .map(|dt| Timestamp(dt.timestamp_millis()))
    };
    for format in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(t, format)
            && let Some(ts) = local(naive)
        {
            return Ok(ts);
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(t, "%Y-%m-%d")
        && let Some(ts) = date.and_hms_opt(0, 0, 0).and_then(local)
    {
        return Ok(ts);
    }
    Err(format!(
        "could not read the time '{t}': use RFC 3339, 'YYYY-MM-DD HH:MM', 'YYYY-MM-DD', or a duration ago like 30m, 2h, 7d"
    ))
}

/// RFC 3339 in local time, what tools return.
pub fn time_text(at: Timestamp) -> String {
    at.to_local()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// Standard base64 (RFC 4648) for image content.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(enabled: bool, days: u32, sources: &[&str]) -> McpConfig {
        McpConfig {
            enabled,
            allow_screenshots: false,
            max_age_days: days,
            sources: sources.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn the_window_and_machine_list_bound_what_is_returned() {
        let now = Timestamp(100 * 86_400_000);
        let c = Ceiling::from_config(&config(true, 30, &[]), now);
        assert_eq!(c.oldest, Some(Timestamp(70 * 86_400_000)));
        assert!(c.admits_time(Timestamp(70 * 86_400_000)));
        assert!(!c.admits_time(Timestamp(70 * 86_400_000 - 1)));
        assert_eq!(c.clamp_since(None), c.oldest);
        assert_eq!(c.clamp_since(Some(Timestamp(0))), c.oldest);
        assert_eq!(c.clamp_since(Some(now)), Some(now));
        assert!(c.admits(None) && c.admits(Some(SourceId::from_bytes([1; 16]))));
        assert_eq!(c.filters(), vec![SourceFilter::All]);

        let remote = SourceId::from_bytes([0xab; 16]);
        let only = Ceiling::from_config(&config(true, 0, &["this", &remote.to_string()]), now);
        assert_eq!(only.oldest, None);
        assert!(only.admits(None) && only.admits(Some(remote)));
        assert!(!only.admits(Some(SourceId::from_bytes([1; 16]))));
        assert_eq!(
            only.filters(),
            vec![SourceFilter::Local, SourceFilter::Remote(remote)]
        );
        let remote_only = Ceiling::from_config(&config(true, 0, &[&remote.to_string()]), now);
        assert!(
            !remote_only.admits(None),
            "this machine excluded when not listed"
        );
    }

    #[test]
    fn moment_ids_round_trip_and_reject_garbage() {
        let s = SourceId::from_bytes([0xcd; 16]);
        for (source, id) in [(None, 7), (Some(s), 12345)] {
            let text = moment_id(source, VisualStateId(id));
            assert_eq!(parse_moment_id(&text), Some((source, VisualStateId(id))));
        }
        for bad in [
            "", "this", "this:", "this:0", "this:-1", "that:5", "abc:5", "this:5x",
        ] {
            assert_eq!(parse_moment_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn times_parse_in_the_forms_agents_send() -> Result<(), String> {
        let now = Local
            .with_ymd_and_hms(2026, 10, 10, 15, 0, 0)
            .earliest()
            .ok_or("now")?;
        let ms = now.timestamp_millis();
        assert_eq!(parse_time("30m", now)?, Timestamp(ms - 30 * 60_000));
        assert_eq!(parse_time("2h", now)?, Timestamp(ms - 2 * 3_600_000));
        assert_eq!(parse_time("7d", now)?, Timestamp(ms - 7 * 86_400_000));
        assert_eq!(parse_time("2026-10-10 15:00", now)?, Timestamp(ms));
        assert_eq!(parse_time("2026-10-10T15:00:00", now)?, Timestamp(ms));
        assert_eq!(parse_time(&now.to_rfc3339(), now)?, Timestamp(ms));
        assert_eq!(
            parse_time("2026-10-10", now)?,
            Timestamp(ms - 15 * 3_600_000)
        );
        assert!(
            parse_time("9223372036854775807m", now).is_err(),
            "overflow is an error"
        );
        assert!(parse_time("-5m", now).is_err(), "no future via negatives");
        assert!(parse_time("40000d", now).is_err());
        assert!(parse_time("yesterday-ish", now).is_err());
        Ok(())
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (input, expect) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expect);
        }
    }
}
