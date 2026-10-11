//! Navigation as plain math and plain rules: how the camera moves to a target, which mark is next,
//! what a "Go to" entry means, and which key does what. Unit-tested; the app only applies it.
//! See `docs/design/marks-keyframes.md`.

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use iced::keyboard::{Key, Modifiers, key::Named};
use std::time::Instant;

/// A cue to a target beyond the room's depth settles over this long (the brief: ~100-180 ms).
pub const CUE_MS: f32 = 180.0;
/// A glide to a visible target takes between these, by distance.
pub const GLIDE_MIN_MS: f32 = 140.0;
pub const GLIDE_MAX_MS: f32 = 240.0;
/// Where a cue starts, as a depth fraction after the target (the room shows it arriving).
const CUE_LEAD_Z: f64 = 0.12;

/// The camera moving from `from` to `to` (Unix ms) over `duration_ms`, eased out, in log-time: the
/// last part of the approach is slow and readable however long the jump.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    pub from: f64,
    pub to: f64,
    pub started: Instant,
    pub duration_ms: f32,
    /// Time scale of the log-time interpolation (the camera's knee).
    pub knee_ms: f64,
}

impl Motion {
    /// Plans a move from `cursor` to `target` for a room `depth_ms` deep with time scale `knee_ms`.
    /// Within the room's depth the camera glides from where it is (so held keys keep flowing);
    /// beyond it, it cues: it starts just after the target and settles onto it, instead of flying
    /// through hours of history. `newest` bounds where a cue may start.
    pub fn plan(
        cursor: f64,
        target: f64,
        depth_ms: f64,
        knee_ms: f64,
        newest: f64,
        started: Instant,
    ) -> Self {
        let knee = knee_ms.max(1.0);
        let distance = (target - cursor).abs();
        if distance <= depth_ms {
            let reach = (1.0 + distance / knee).ln() / (1.0 + depth_ms.max(1.0) / knee).ln();
            let duration =
                GLIDE_MIN_MS + (GLIDE_MAX_MS - GLIDE_MIN_MS) * reach.clamp(0.0, 1.0) as f32;
            return Self {
                from: cursor,
                to: target,
                started,
                duration_ms: duration,
                knee_ms: knee,
            };
        }
        let lead = knee * ((1.0 + depth_ms / knee).powf(CUE_LEAD_Z) - 1.0);
        Self {
            from: (target + lead).min(newest.max(target)),
            to: target,
            started,
            duration_ms: CUE_MS,
            knee_ms: knee,
        }
    }

    /// Eased progress 0..=1 at `now`.
    fn progress(&self, now: Instant) -> f64 {
        let t = (now.saturating_duration_since(self.started).as_secs_f32() * 1000.0
            / self.duration_ms.max(1.0))
        .clamp(0.0, 1.0);
        f64::from(1.0 - (1.0 - t).powi(3))
    }

    /// Where the camera is at `now`, and whether the move is over.
    pub fn at(&self, now: Instant) -> (f64, bool) {
        let e = self.progress(now);
        if e >= 1.0 {
            return (self.to, true);
        }
        let d = self.to - self.from;
        // Remaining distance shrinks geometrically in log-time: `to - sign * k((1+|d|/k)^(1-e) - 1)`.
        let remaining = self.knee_ms * ((1.0 + d.abs() / self.knee_ms).powf(1.0 - e) - 1.0);
        (self.to - d.signum() * remaining, false)
    }
}

/// The nearest time strictly after (`forward`) or before `cursor` among `times` (any order).
/// Half a millisecond of slack, so standing on a mark and stepping moves off it.
pub fn neighbour(times: impl IntoIterator<Item = i64>, cursor: f64, forward: bool) -> Option<i64> {
    let times = times.into_iter();
    if forward {
        times.filter(|&t| t as f64 > cursor + 0.5).min()
    } else {
        times.filter(|&t| (t as f64) < cursor - 0.5).max()
    }
}

/// What the "Go to" field understands, local time: `now`/`latest`, `oldest`/`first`, `today`,
/// `yesterday`, `14:30` (today, or yesterday if that is still ahead), `yesterday 9:00`,
/// `2026-10-08 14:00[:05]`, `2026-10-08`, and how long ago: `90s`, `15m`, `2h`, `3d`, `1w`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpTarget {
    Latest,
    Oldest,
    At(i64),
}

pub fn parse_jump(input: &str, now: DateTime<Local>) -> Result<JumpTarget, String> {
    let text = input.trim().to_ascii_lowercase();
    let local = |naive: NaiveDateTime| -> Result<JumpTarget, String> {
        Local
            .from_local_datetime(&naive)
            .earliest()
            .map(|t| JumpTarget::At(t.timestamp_millis()))
            .ok_or_else(|| "that local time does not exist (a clock change)".to_string())
    };
    let today = now.date_naive();
    let yesterday = today.pred_opt().ok_or("date out of range")?;
    match text.as_str() {
        "" => return Err("type a time, like 14:30, yesterday 9:00 or 15m".into()),
        "now" | "latest" | "end" => return Ok(JumpTarget::Latest),
        "oldest" | "first" | "start" => return Ok(JumpTarget::Oldest),
        "today" => return local(today.and_time(NaiveTime::MIN)),
        "yesterday" => return local(yesterday.and_time(NaiveTime::MIN)),
        _ => {}
    }
    if let Some(ago) = relative_ms(&text) {
        return Ok(JumpTarget::At(now.timestamp_millis().saturating_sub(ago)));
    }
    if let Some(rest) = text.strip_prefix("yesterday ")
        && let Some(time) = clock(rest)
    {
        return local(yesterday.and_time(time));
    }
    if let Some(rest) = text.strip_prefix("today ")
        && let Some(time) = clock(rest)
    {
        return local(today.and_time(time));
    }
    if let Some(time) = clock(&text) {
        let candidate = today.and_time(time);
        // A time still ahead today means the most recent one: yesterday's.
        let day = if candidate > now.naive_local() {
            yesterday
        } else {
            today
        };
        return local(day.and_time(time));
    }
    for format in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dt%H:%M:%S",
        "%Y-%m-%dt%H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(&text, format) {
            return local(naive);
        }
    }
    if let Ok(day) = NaiveDate::parse_from_str(&text, "%Y-%m-%d") {
        return local(day.and_time(NaiveTime::MIN));
    }
    Err(format!(
        "could not read \"{}\"; try 14:30, yesterday 9:00, 2026-10-08 14:00 or 15m",
        input.trim()
    ))
}

fn clock(text: &str) -> Option<NaiveTime> {
    ["%H:%M:%S", "%H:%M"]
        .iter()
        .find_map(|f| NaiveTime::parse_from_str(text.trim(), f).ok())
}

fn relative_ms(text: &str) -> Option<i64> {
    let text = text.strip_suffix(" ago").unwrap_or(text).trim();
    let unit = text.chars().last()?;
    let number: i64 = text[..text.len() - unit.len_utf8()].trim().parse().ok()?;
    if number < 0 {
        return None;
    }
    let scale: i64 = match unit {
        's' => 1_000,
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        'w' => 7 * 86_400_000,
        _ => return None,
    };
    number.checked_mul(scale)
}

/// Whether a failed `rsrewind mark rename/remove` failed because the mark no longer exists (it was
/// removed elsewhere, or forgotten with its time): the list is stale and must be read again.
pub fn mark_gone(error: &str) -> bool {
    error.contains("there is no mark")
}

/// What a key press (one no widget captured) asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    /// Previous / next moment; `extend` grows the selection instead of replacing it.
    Moment {
        forward: bool,
        extend: bool,
    },
    Keyframe {
        forward: bool,
        extend: bool,
    },
    Mark {
        forward: bool,
        extend: bool,
    },
    Oldest,
    Latest,
    /// Mark the moment on stage now, without a label.
    MarkNow,
    /// Open the panel to mark with a label.
    MarkWithLabel,
    /// Open the panel at "Go to".
    GoTo,
    /// Close the panel, or clear the selection.
    Escape,
    /// Ctrl/Cmd+A: select every moment loaded around the cursor.
    SelectAll,
    /// Ctrl/Cmd+C: copy the picture on stage.
    CopyImage,
}

pub fn shortcut(key: Key<&str>, modifiers: Modifiers) -> Option<Shortcut> {
    let shift = modifiers.shift();
    let command = modifiers.command();
    if modifiers.alt() {
        return None;
    }
    Some(match key {
        Key::Named(Named::ArrowLeft | Named::ArrowDown) if !command => Shortcut::Moment {
            forward: false,
            extend: shift,
        },
        Key::Named(Named::ArrowRight | Named::ArrowUp) if !command => Shortcut::Moment {
            forward: true,
            extend: shift,
        },
        Key::Named(Named::PageUp) => Shortcut::Keyframe {
            forward: false,
            extend: shift,
        },
        Key::Named(Named::PageDown) => Shortcut::Keyframe {
            forward: true,
            extend: shift,
        },
        Key::Named(Named::Home) => Shortcut::Oldest,
        Key::Named(Named::End) => Shortcut::Latest,
        Key::Named(Named::Escape) => Shortcut::Escape,
        Key::Character(c) if command => match c {
            "a" | "A" => Shortcut::SelectAll,
            "c" | "C" => Shortcut::CopyImage,
            _ => return None,
        },
        Key::Character(c) => match c {
            "," => Shortcut::Keyframe {
                forward: false,
                extend: false,
            },
            "." => Shortcut::Keyframe {
                forward: true,
                extend: false,
            },
            "<" => Shortcut::Keyframe {
                forward: false,
                extend: true,
            },
            ">" => Shortcut::Keyframe {
                forward: true,
                extend: true,
            },
            "[" => Shortcut::Mark {
                forward: false,
                extend: false,
            },
            "]" => Shortcut::Mark {
                forward: true,
                extend: false,
            },
            "{" => Shortcut::Mark {
                forward: false,
                extend: true,
            },
            "}" => Shortcut::Mark {
                forward: true,
                extend: true,
            },
            "m" => Shortcut::MarkNow,
            "M" => Shortcut::MarkWithLabel,
            "g" | "G" => Shortcut::GoTo,
            _ => return None,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const T: f64 = 1_790_000_000_000.0;
    const KNEE: f64 = 20_000.0;
    const DEPTH: f64 = 3_600_000.0;

    #[test]
    fn a_visible_target_is_glided_to_from_where_the_camera_is() {
        let t0 = Instant::now();
        let m = Motion::plan(T, T - 60_000.0, DEPTH, KNEE, T, t0);
        assert_eq!((m.from, m.to), (T, T - 60_000.0));
        assert!((GLIDE_MIN_MS..=GLIDE_MAX_MS).contains(&m.duration_ms));
        // Monotonic, starts where it is, ends on target, and slows down near the end.
        let mut last = m.from;
        let mut steps = Vec::new();
        for ms in (0..=300).step_by(10) {
            let (x, _) = m.at(t0 + Duration::from_millis(ms));
            assert!(x <= last + 1e-6, "{ms}: {x} > {last}");
            steps.push(last - x);
            last = x;
        }
        assert_eq!(m.at(t0), (T, false));
        assert_eq!(m.at(t0 + Duration::from_secs(1)), (T - 60_000.0, true));
        assert!(steps[1] > steps[steps.len() / 2], "{steps:?}");
        // Longer jumps take longer, within bounds.
        let short = Motion::plan(T, T - 1_000.0, DEPTH, KNEE, T, t0).duration_ms;
        let long = Motion::plan(T, T - 3_000_000.0, DEPTH, KNEE, T, t0).duration_ms;
        assert!(short < long && long <= GLIDE_MAX_MS);
    }

    #[test]
    fn a_far_target_is_cued_not_flown_through() {
        let t0 = Instant::now();
        let target = T - 30.0 * 86_400_000.0;
        let m = Motion::plan(T, target, DEPTH, KNEE, T, t0);
        assert_eq!(m.duration_ms, CUE_MS);
        assert!(
            m.from > target && m.from - target < DEPTH,
            "{}",
            m.from - target
        );
        // Forward too, but a cue never starts past the newest moment.
        let near_end = Motion::plan(T - 100.0 * DEPTH, T - 10.0, DEPTH, KNEE, T, t0);
        assert!(near_end.from <= T);
        assert_eq!(near_end.at(t0 + Duration::from_secs(1)).0, T - 10.0);
    }

    #[test]
    fn neighbours_are_strictly_before_or_after() {
        let marks = [30, 10, 20];
        assert_eq!(neighbour(marks, 20.0, true), Some(30));
        assert_eq!(neighbour(marks, 20.0, false), Some(10));
        assert_eq!(neighbour(marks, 20.2, false), Some(10), "standing on it");
        assert_eq!(neighbour(marks, 30.0, true), None);
        assert_eq!(neighbour([], 1.0, false), None);
    }

    #[test]
    fn go_to_understands_people() -> Result<(), String> {
        let now = Local
            .with_ymd_and_hms(2026, 10, 11, 12, 0, 0)
            .single()
            .ok_or("now")?;
        let at = |s: &str| -> Result<i64, String> {
            match parse_jump(s, now)? {
                JumpTarget::At(ms) => Ok(ms),
                other => Err(format!("{s}: {other:?}")),
            }
        };
        let local = |y, mo, d, h, mi| -> Result<i64, String> {
            Ok(Local
                .with_ymd_and_hms(y, mo, d, h, mi, 0)
                .single()
                .ok_or("t")?
                .timestamp_millis())
        };
        assert_eq!(at("09:30")?, local(2026, 10, 11, 9, 30)?);
        assert_eq!(
            at("14:30")?,
            local(2026, 10, 10, 14, 30)?,
            "still ahead: yesterday"
        );
        assert_eq!(at(" Yesterday 9:05 ")?, local(2026, 10, 10, 9, 5)?);
        assert_eq!(at("2026-10-08 14:00")?, local(2026, 10, 8, 14, 0)?);
        assert_eq!(at("2026-10-08")?, local(2026, 10, 8, 0, 0)?);
        assert_eq!(at("15m")?, now.timestamp_millis() - 900_000);
        assert_eq!(at("2h ago")?, now.timestamp_millis() - 7_200_000);
        assert_eq!(parse_jump("now", now)?, JumpTarget::Latest);
        assert_eq!(parse_jump("oldest", now)?, JumpTarget::Oldest);
        assert!(parse_jump("", now).is_err());
        assert!(parse_jump("tea time", now).is_err());
        assert!(parse_jump("-5m", now).is_err());
        Ok(())
    }

    #[test]
    fn keys_map_to_navigation() {
        let none = Modifiers::empty();
        assert_eq!(
            shortcut(Key::Named(Named::ArrowLeft), none),
            Some(Shortcut::Moment {
                forward: false,
                extend: false
            })
        );
        assert_eq!(
            shortcut(Key::Named(Named::ArrowRight), Modifiers::SHIFT),
            Some(Shortcut::Moment {
                forward: true,
                extend: true
            })
        );
        assert_eq!(
            shortcut(Key::Named(Named::PageDown), Modifiers::SHIFT),
            Some(Shortcut::Keyframe {
                forward: true,
                extend: true
            })
        );
        assert_eq!(
            shortcut(Key::Character("]"), none),
            Some(Shortcut::Mark {
                forward: true,
                extend: false
            })
        );
        assert_eq!(
            shortcut(Key::Character("{"), Modifiers::SHIFT),
            Some(Shortcut::Mark {
                forward: false,
                extend: true
            })
        );
        assert_eq!(shortcut(Key::Character("m"), none), Some(Shortcut::MarkNow));
        assert_eq!(
            shortcut(Key::Character("M"), Modifiers::SHIFT),
            Some(Shortcut::MarkWithLabel)
        );
        assert_eq!(
            shortcut(Key::Character("c"), Modifiers::COMMAND),
            Some(Shortcut::CopyImage)
        );
        assert_eq!(shortcut(Key::Character("c"), none), None);
        assert_eq!(shortcut(Key::Character("m"), Modifiers::ALT), None);
        assert_eq!(
            shortcut(Key::Named(Named::Escape), none),
            Some(Shortcut::Escape)
        );
    }

    #[test]
    fn a_mark_removed_elsewhere_is_recognised() {
        assert!(mark_gone("there is no mark 12"));
        assert!(!mark_gone("a mark label is at most 120 characters"));
        assert!(!mark_gone("could not find the rsrewind executable"));
    }
}
