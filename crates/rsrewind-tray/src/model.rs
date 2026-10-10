//! What the tray shows and what its menu does, independent of any platform's tray API.
//!
//! The tray never opens the database: it learns the recorder's state from `rsrewind status --json`
//! and acts by running `rsrewind` subcommands ([`Action::cli_args`]). So it can do nothing the
//! command line cannot, and a crashed or stuck tray cannot stop or corrupt recording.

use serde::Deserialize;
use std::time::{Duration, Instant};

/// How long a "forget" stays armed waiting for the confirming second click.
pub const CONFIRM_WINDOW: Duration = Duration::from_secs(10);

/// The recorder as `rsrewind status --json` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorder {
    Recording,
    /// `until` is local `HH:MM`, or `None` for "until resumed".
    Paused {
        until: Option<String>,
    },
    NotRunning,
    Error,
    /// The status command itself failed or printed something unreadable.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub recorder: Recorder,
    /// Set when the newest session ran without its privacy rules enforced.
    pub privacy_unenforced: bool,
}

impl Status {
    pub const UNKNOWN: Self = Self {
        recorder: Recorder::Unknown,
        privacy_unenforced: false,
    };

    /// Parses `rsrewind status --json`. `local_time` formats a Unix-ms instant as `HH:MM`.
    pub fn parse(json: &str, local_time: impl Fn(i64) -> String) -> Self {
        #[derive(Deserialize)]
        struct Raw {
            state: String,
            paused_until: Option<i64>,
            #[serde(default)]
            privacy_unenforced: Option<String>,
        }
        let Ok(raw) = serde_json::from_str::<Raw>(json) else {
            return Self::UNKNOWN;
        };
        let recorder = match raw.state.as_str() {
            "recording" => Recorder::Recording,
            "paused" => Recorder::Paused {
                until: raw.paused_until.map(&local_time),
            },
            "not_running" => Recorder::NotRunning,
            "error" => Recorder::Error,
            _ => Recorder::Unknown,
        };
        Self {
            recorder,
            privacy_unenforced: raw.privacy_unenforced.is_some(),
        }
    }

    /// One line for the tooltip and the top of the menu.
    pub fn headline(&self) -> String {
        let state = match &self.recorder {
            Recorder::Recording => "Recording".to_string(),
            Recorder::Paused { until: Some(t) } => format!("Paused until {t}"),
            Recorder::Paused { until: None } => "Paused".to_string(),
            Recorder::NotRunning => "Not recording (recorder stopped)".to_string(),
            Recorder::Error => "Recorder error: see `rsrewind doctor`".to_string(),
            Recorder::Unknown => "Status unknown".to_string(),
        };
        if self.privacy_unenforced {
            format!("{state} · privacy rules NOT enforced")
        } else {
            state
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Pause for this many minutes; `None` until resumed.
    Pause(Option<u32>),
    Resume,
    /// First click arms, second click within [`CONFIRM_WINDOW`] deletes.
    Forget(u32),
    OpenWindow,
    StartRecorder,
    StopRecorder,
    Quit,
}

impl Action {
    /// The `rsrewind` arguments that carry out this action (after `--data-dir <dir>`), or `None`
    /// for actions the tray handles itself.
    pub fn cli_args(self) -> Option<Vec<String>> {
        let v = |args: &[&str]| Some(args.iter().map(|s| s.to_string()).collect());
        match self {
            Self::Pause(Some(minutes)) => v(&["pause", "--minutes", &minutes.to_string()]),
            Self::Pause(None) => v(&["pause"]),
            Self::Resume => v(&["resume"]),
            Self::Forget(minutes) => v(&["forget", &format!("{minutes}m"), "--yes"]),
            Self::OpenWindow => v(&["ui"]),
            Self::StartRecorder => v(&["start"]),
            Self::StopRecorder => v(&["stop"]),
            Self::Quit => None,
        }
    }
}

/// A forget waiting for its confirming click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Armed {
    pub minutes: u32,
    pub at: Instant,
}

/// What a click on a menu item should do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// Run this action.
    Run(Action),
    /// Arm this forget (the menu then asks for confirmation).
    Arm(Armed),
}

/// Applies the two-step confirmation to forgets. Everything else runs at once (and disarms).
pub fn click(action: Action, armed: Option<Armed>, now: Instant) -> Click {
    match action {
        Action::Forget(minutes) => match armed {
            Some(a) if a.minutes == minutes && now.duration_since(a.at) <= CONFIRM_WINDOW => {
                Click::Run(action)
            }
            _ => Click::Arm(Armed { minutes, at: now }),
        },
        other => Click::Run(other),
    }
}

/// One menu row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// Text only (the state headline).
    Label(String),
    Separator,
    Button {
        label: String,
        action: Action,
        enabled: bool,
    },
}

const FORGET_CHOICES: [u32; 2] = [10, 60];

fn forget_label(minutes: u32) -> String {
    if minutes.is_multiple_of(60) {
        let hours = minutes / 60;
        format!(
            "Forget the last {hours} hour{}",
            if hours == 1 { "" } else { "s" }
        )
    } else {
        format!("Forget the last {minutes} minutes")
    }
}

/// The menu for this state. A forget armed less than [`CONFIRM_WINDOW`] ago shows its
/// confirmation label in place.
pub fn menu(status: &Status, armed: Option<Armed>, now: Instant) -> Vec<Item> {
    let running = matches!(
        status.recorder,
        Recorder::Recording | Recorder::Paused { .. }
    );
    let paused = matches!(status.recorder, Recorder::Paused { .. });
    let button = |label: &str, action, enabled| Item::Button {
        label: label.to_string(),
        action,
        enabled,
    };
    let mut items = vec![Item::Label(status.headline()), Item::Separator];
    if paused {
        items.push(button("Resume recording", Action::Resume, true));
    } else {
        items.push(button(
            "Pause for 15 minutes",
            Action::Pause(Some(15)),
            running,
        ));
        items.push(button("Pause for 1 hour", Action::Pause(Some(60)), running));
        items.push(button("Pause until resumed", Action::Pause(None), running));
    }
    items.push(Item::Separator);
    for minutes in FORGET_CHOICES {
        let live =
            armed.filter(|a| a.minutes == minutes && now.duration_since(a.at) <= CONFIRM_WINDOW);
        let label = match live {
            Some(_) => format!(
                "Click again to delete the last {} for good",
                forget_span(minutes)
            ),
            None => format!("{}…", forget_label(minutes)),
        };
        items.push(Item::Button {
            label,
            action: Action::Forget(minutes),
            enabled: true,
        });
    }
    items.push(Item::Separator);
    items.push(button("Open rsRewind", Action::OpenWindow, true));
    if running {
        items.push(button("Stop the recorder", Action::StopRecorder, true));
    } else {
        items.push(button("Start the recorder", Action::StartRecorder, true));
    }
    items.push(Item::Separator);
    items.push(button(
        "Quit the tray icon (recording continues)",
        Action::Quit,
        true,
    ));
    items
}

fn forget_span(minutes: u32) -> String {
    if minutes.is_multiple_of(60) {
        let hours = minutes / 60;
        if hours == 1 {
            "hour".to_string()
        } else {
            format!("{hours} hours")
        }
    } else {
        format!("{minutes} minutes")
    }
}

/// Which artwork the icon uses: red while recording, green while not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    Red,
    Green,
}

/// A small mark in the bottom-right corner, so state never depends on colour alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    None,
    /// Paused: two bars.
    Pause,
    /// Recorder not running: a square.
    Stop,
    /// Privacy rules not enforced, or the recorder is in trouble: an amber dot.
    Warning,
}

pub fn look(status: &Status) -> (Base, Badge) {
    match status.recorder {
        Recorder::Recording if status.privacy_unenforced => (Base::Red, Badge::Warning),
        Recorder::Recording => (Base::Red, Badge::None),
        Recorder::Paused { .. } => (Base::Green, Badge::Pause),
        Recorder::NotRunning => (Base::Green, Badge::Stop),
        Recorder::Error | Recorder::Unknown => (Base::Green, Badge::Warning),
    }
}

/// `rgba` (a `size`x`size` RGBA8 image) with `badge` drawn over its bottom-right corner, converted
/// to ARGB32 in network byte order (what StatusNotifierItem expects).
pub fn compose(rgba: &[u8], size: u32, badge: Badge) -> Vec<u8> {
    let n = size as usize;
    let mut out = Vec::with_capacity(n * n * 4);
    for px in rgba.as_chunks::<4>().0.iter().take(n * n) {
        out.extend_from_slice(&[px[3], px[0], px[1], px[2]]);
    }
    out.resize(n * n * 4, 0);
    if badge == Badge::None {
        return out;
    }
    let s = size as f32;
    let (cx, cy, r) = (s * 0.74, s * 0.74, s * 0.26);
    let fill: [u8; 3] = match badge {
        Badge::Warning => [255, 176, 32],
        _ => [24, 24, 28],
    };
    let mut paint = |x: usize, y: usize, rgb: [u8; 3], coverage: f32| {
        let i = (y * n + x) * 4;
        let k = coverage.clamp(0.0, 1.0);
        let blend = |under: u8, over: u8| {
            (f32::from(under) * (1.0 - k) + f32::from(over) * k).round() as u8
        };
        out[i] = blend(out[i], 255);
        out[i + 1] = blend(out[i + 1], rgb[0]);
        out[i + 2] = blend(out[i + 2], rgb[1]);
        out[i + 3] = blend(out[i + 3], rgb[2]);
    };
    for y in 0..n {
        for x in 0..n {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            // Disc with a 1 px light rim for contrast on any panel colour.
            paint(x, y, [235, 235, 235], r + 0.5 - d);
            paint(x, y, fill, r - 0.5 - d);
            let (gx, gy) = ((px - cx) / r, (py - cy) / r);
            let glyph = match badge {
                Badge::Pause => gy.abs() < 0.45 && (0.12..0.36).contains(&gx.abs()),
                Badge::Stop => gx.abs() < 0.36 && gy.abs() < 0.36,
                _ => false,
            };
            if glyph {
                paint(x, y, [245, 245, 245], 1.0);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hhmm(ms: i64) -> String {
        format!("t{ms}")
    }

    #[test]
    fn status_json_becomes_a_state_and_garbage_is_unknown() {
        let s = Status::parse(
            r#"{"state":"recording","running":true,"paused_until":null}"#,
            hhmm,
        );
        assert_eq!(s.recorder, Recorder::Recording);
        assert!(!s.privacy_unenforced);
        let p = Status::parse(r#"{"state":"paused","paused_until":42}"#, hhmm);
        assert_eq!(
            p.recorder,
            Recorder::Paused {
                until: Some("t42".into())
            }
        );
        assert_eq!(p.headline(), "Paused until t42");
        let u = Status::parse(
            r#"{"state":"recording","paused_until":null,"privacy_unenforced":"no window list"}"#,
            hhmm,
        );
        assert_eq!(u.headline(), "Recording · privacy rules NOT enforced");
        assert_eq!(Status::parse("", hhmm), Status::UNKNOWN);
        assert_eq!(
            Status::parse(r#"{"state":"weird","paused_until":null}"#, hhmm).recorder,
            Recorder::Unknown
        );
    }

    #[test]
    fn forget_needs_a_second_click_on_the_same_choice_within_the_window() {
        let t0 = Instant::now();
        let first = click(Action::Forget(10), None, t0);
        let Click::Arm(armed) = first else {
            panic!("first click must arm, got {first:?}");
        };
        assert_eq!(
            click(Action::Forget(10), Some(armed), t0 + Duration::from_secs(3)),
            Click::Run(Action::Forget(10))
        );
        // Too late: arms again instead of deleting.
        assert!(matches!(
            click(
                Action::Forget(10),
                Some(armed),
                t0 + Duration::from_secs(11)
            ),
            Click::Arm(_)
        ));
        // A different span does not confirm this one.
        assert!(matches!(
            click(Action::Forget(60), Some(armed), t0 + Duration::from_secs(1)),
            Click::Arm(_)
        ));
        // Other actions run at once.
        assert_eq!(
            click(Action::Resume, Some(armed), t0),
            Click::Run(Action::Resume)
        );
    }

    #[test]
    fn the_menu_follows_the_state_and_shows_the_confirmation() {
        let t0 = Instant::now();
        let recording = Status {
            recorder: Recorder::Recording,
            privacy_unenforced: false,
        };
        let labels = |items: &[Item]| -> Vec<String> {
            items
                .iter()
                .filter_map(|i| match i {
                    Item::Button { label, .. } | Item::Label(label) => Some(label.clone()),
                    Item::Separator => None,
                })
                .collect()
        };
        let m = labels(&menu(&recording, None, t0));
        assert!(m.contains(&"Pause for 15 minutes".to_string()));
        assert!(m.contains(&"Forget the last 10 minutes…".to_string()));
        assert!(m.contains(&"Forget the last 1 hour…".to_string()));
        assert!(m.contains(&"Stop the recorder".to_string()));
        let armed = Armed {
            minutes: 10,
            at: t0,
        };
        let m = labels(&menu(&recording, Some(armed), t0 + Duration::from_secs(2)));
        assert!(
            m.contains(&"Click again to delete the last 10 minutes for good".to_string()),
            "{m:?}"
        );
        assert!(m.contains(&"Forget the last 1 hour…".to_string()));
        let m = labels(&menu(&recording, Some(armed), t0 + Duration::from_secs(20)));
        assert!(m.contains(&"Forget the last 10 minutes…".to_string()));

        let stopped = Status {
            recorder: Recorder::NotRunning,
            privacy_unenforced: false,
        };
        let items = menu(&stopped, None, t0);
        assert!(labels(&items).contains(&"Start the recorder".to_string()));
        // Pausing a stopped recorder is offered but disabled.
        assert!(items.iter().any(|i| matches!(
            i,
            Item::Button {
                action: Action::Pause(Some(15)),
                enabled: false,
                ..
            }
        )));
        let paused = Status {
            recorder: Recorder::Paused { until: None },
            privacy_unenforced: false,
        };
        assert!(labels(&menu(&paused, None, t0)).contains(&"Resume recording".to_string()));
    }

    #[test]
    fn actions_map_to_the_cli() {
        assert_eq!(
            Action::Forget(10).cli_args(),
            Some(vec!["forget".into(), "10m".into(), "--yes".into()])
        );
        assert_eq!(
            Action::Pause(Some(15)).cli_args(),
            Some(vec!["pause".into(), "--minutes".into(), "15".into()])
        );
        assert_eq!(Action::Pause(None).cli_args(), Some(vec!["pause".into()]));
        assert_eq!(Action::Quit.cli_args(), None);
    }

    #[test]
    fn each_state_has_its_own_artwork_and_badge() {
        let st = |recorder, privacy_unenforced| Status {
            recorder,
            privacy_unenforced,
        };
        assert_eq!(
            look(&st(Recorder::Recording, false)),
            (Base::Red, Badge::None)
        );
        assert_eq!(
            look(&st(Recorder::Recording, true)),
            (Base::Red, Badge::Warning)
        );
        assert_eq!(
            look(&st(Recorder::Paused { until: None }, false)),
            (Base::Green, Badge::Pause)
        );
        assert_eq!(
            look(&st(Recorder::NotRunning, false)),
            (Base::Green, Badge::Stop)
        );
        assert_eq!(
            look(&st(Recorder::Unknown, false)),
            (Base::Green, Badge::Warning)
        );
    }

    #[test]
    fn compose_converts_to_argb_and_badges_only_the_corner() {
        let size = 32u32;
        // Opaque blue everywhere.
        let rgba: Vec<u8> = (0..size * size).flat_map(|_| [10, 20, 200, 255]).collect();
        let plain = compose(&rgba, size, Badge::None);
        assert_eq!(&plain[..4], &[255, 10, 20, 200], "RGBA becomes ARGB");
        let paused = compose(&rgba, size, Badge::Pause);
        let at = |v: &[u8], x: u32, y: u32| v[((y * size + x) * 4) as usize..][..4].to_vec();
        // Top-left untouched; the badge centre (between the bars) is the dark disc; a bar is light.
        assert_eq!(at(&paused, 2, 2), at(&plain, 2, 2));
        let (cx, cy) = ((size as f32 * 0.74) as u32, (size as f32 * 0.74) as u32);
        assert!(at(&paused, cx, cy)[1] < 60, "{:?}", at(&paused, cx, cy));
        let bar_x = (size as f32 * 0.74 + size as f32 * 0.26 * 0.24) as u32;
        assert!(
            at(&paused, bar_x, cy)[1] > 200,
            "{:?}",
            at(&paused, bar_x, cy)
        );
        let stopped = compose(&rgba, size, Badge::Stop);
        assert!(
            at(&stopped, cx, cy)[1] > 200,
            "the stop square covers the centre"
        );
        let warning = compose(&rgba, size, Badge::Warning);
        assert_eq!(at(&warning, cx, cy)[1..], [255, 176, 32]);
        // A short buffer is padded, not a panic.
        assert_eq!(compose(&[], 4, Badge::Stop).len(), 64);
    }
}
