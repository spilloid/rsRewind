//! The event model.
//!
//! rsRewind records *events* in time. A screenshot is one kind of evidence attached to an event,
//! not the unit of record: an observation event says "from T1 to T2, monitor M showed visual state
//! V while process P / window W had focus". Future evidence (audio segments, meeting context,
//! manual markers) attaches to the same timeline without reshaping it.

use crate::Timestamp;
use serde::{Deserialize, Serialize};
use std::fmt;

/// What kind of thing happened. Stored as a stable lowercase string in SQLite (`as_str`), so new
/// variants can be added without renumbering anything already on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A monitor showed a visual state for a span of time. Extended in place while nothing changes.
    Observation,
    /// Recording was paused by the user (metadata carries an optional `until`).
    Paused,
    /// Recording resumed.
    Resumed,
    /// Capture was skipped because a privacy rule matched. No pixels are stored for these.
    PrivacySkip,
    /// The user went idle / came back.
    IdleStart,
    IdleEnd,
    /// Recorder process lifecycle.
    RecorderStart,
    RecorderStop,
}

impl EventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observation => "observation",
            Self::Paused => "paused",
            Self::Resumed => "resumed",
            Self::PrivacySkip => "privacy_skip",
            Self::IdleStart => "idle_start",
            Self::IdleEnd => "idle_end",
            Self::RecorderStart => "recorder_start",
            Self::RecorderStop => "recorder_stop",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "observation" => Self::Observation,
            "paused" => Self::Paused,
            "resumed" => Self::Resumed,
            "privacy_skip" => Self::PrivacySkip,
            "idle_start" => Self::IdleStart,
            "idle_end" => Self::IdleEnd,
            "recorder_start" => Self::RecorderStart,
            "recorder_stop" => Self::RecorderStop,
            _ => return None,
        })
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A physical display as Windows reports it.
///
/// `device_name` (e.g. `\\.\DISPLAY1`) is the stable key: an `HMONITOR` handle changes across
/// hotplug and sleep/wake, the device name usually does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub device_name: String,
    /// Virtual-desktop rectangle in physical pixels.
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// Effective DPI (96 = 100% scaling).
    pub dpi: u32,
    pub primary: bool,
}

/// The process that owned the foreground window.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApplicationContext {
    /// Executable file name, e.g. `Teams.exe`. The privacy and filter key.
    pub process_name: String,
    /// Full image path when Windows lets us read it (elevated processes may refuse).
    pub exe_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WindowContext {
    pub title: String,
    pub class_name: Option<String>,
}

/// Who had focus at the moment of an observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusContext {
    pub application: ApplicationContext,
    pub window: WindowContext,
    /// Process id at capture time. Not persisted as identity (pids are reused), only logged.
    pub pid: u32,
}

/// A captured frame: tightly packed or strided 32-bit BGRA, top-down, as Windows produces it.
#[derive(Clone, PartialEq, Eq)]
pub struct BgraFrame {
    pub width: u32,
    pub height: u32,
    /// Bytes per row; `>= width * 4`.
    pub stride: u32,
    pub pixels: Vec<u8>,
}

impl BgraFrame {
    /// Returns a frame with `stride == width * 4`, copying only if needed.
    pub fn into_packed(self) -> Self {
        let row = self.width as usize * 4;
        if self.stride as usize == row {
            return self;
        }
        let mut packed = Vec::with_capacity(row * self.height as usize);
        for y in 0..self.height as usize {
            let start = y * self.stride as usize;
            packed.extend_from_slice(&self.pixels[start..start + row]);
        }
        Self {
            width: self.width,
            height: self.height,
            stride: self.width * 4,
            pixels: packed,
        }
    }

    /// Byte length the frame must have to be well-formed.
    pub fn expected_len(&self) -> usize {
        if self.height == 0 {
            return 0;
        }
        self.stride as usize * (self.height as usize - 1) + self.width as usize * 4
    }

    pub fn is_well_formed(&self) -> bool {
        self.stride >= self.width.saturating_mul(4) && self.pixels.len() >= self.expected_len()
    }
}

impl fmt::Debug for BgraFrame {
    // Never dump pixel data into logs or panic messages.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BgraFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("bytes", &self.pixels.len())
            .finish()
    }
}

/// One recognized line of text with its bounding box in the pixel space of the stored image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrBlock {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Windows.Media.Ocr reports no confidence; kept for engines that do.
    pub confidence: Option<f32>,
    /// Reading order within the image.
    pub line_index: u32,
}

/// The recorder's externally visible state. Always discoverable: never a hidden mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CaptureState {
    Recording,
    /// `until` is `None` for an indefinite pause that only `resume` ends.
    Paused {
        until: Option<Timestamp>,
    },
    Error,
    Stopped,
}

impl CaptureState {
    /// The effective state at `now`: an expired timed pause reads as recording.
    pub fn effective_at(self, now: Timestamp) -> Self {
        match self {
            Self::Paused { until: Some(until) } if until <= now => Self::Recording,
            other => other,
        }
    }

    /// A pause that never shortens or ends an existing one: what an agent may ask for. An indefinite
    /// pause stays indefinite; between two deadlines the later wins; with no pause in force (or an
    /// expired one) the request applies as given. Errors and stops are left alone.
    pub fn extend_pause(self, requested_until: Option<Timestamp>, now: Timestamp) -> Self {
        match self.effective_at(now) {
            Self::Paused { until: None } => self,
            Self::Paused {
                until: Some(current),
            } => Self::Paused {
                until: requested_until.map(|r| r.max(current)),
            },
            Self::Recording => Self::Paused {
                until: requested_until,
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_pause_never_shortens_or_ends_one_in_force() {
        let now = Timestamp(1_000);
        let later = Some(Timestamp(5_000));
        let sooner = Some(Timestamp(2_000));
        let indefinite = CaptureState::Paused { until: None };
        // The person's indefinite pause survives an agent's one-minute pause.
        assert_eq!(indefinite.extend_pause(sooner, now), indefinite);
        // A longer person's pause is not shortened; a shorter one is extended.
        let long = CaptureState::Paused { until: later };
        assert_eq!(long.extend_pause(sooner, now), long);
        assert_eq!(
            CaptureState::Paused { until: sooner }.extend_pause(later, now),
            long
        );
        // An indefinite request outlasts any deadline.
        assert_eq!(long.extend_pause(None, now), indefinite);
        // Nothing in force (recording, or a pause that already ended): the request applies.
        assert_eq!(
            CaptureState::Recording.extend_pause(sooner, now),
            CaptureState::Paused { until: sooner }
        );
        let expired = CaptureState::Paused {
            until: Some(Timestamp(500)),
        };
        assert_eq!(
            expired.extend_pause(sooner, now),
            CaptureState::Paused { until: sooner }
        );
        assert_eq!(
            CaptureState::Stopped.extend_pause(sooner, now),
            CaptureState::Stopped
        );
    }

    #[test]
    fn event_kind_round_trips() {
        for kind in [
            EventKind::Observation,
            EventKind::Paused,
            EventKind::Resumed,
            EventKind::PrivacySkip,
            EventKind::IdleStart,
            EventKind::IdleEnd,
            EventKind::RecorderStart,
            EventKind::RecorderStop,
        ] {
            assert_eq!(EventKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(EventKind::parse("nope"), None);
    }

    #[test]
    fn packs_strided_frames() {
        let frame = BgraFrame {
            width: 2,
            height: 2,
            stride: 12,
            pixels: vec![
                1, 1, 1, 1, 2, 2, 2, 2, 0, 0, 0, 0, 3, 3, 3, 3, 4, 4, 4, 4, 0, 0, 0, 0,
            ],
        };
        assert!(frame.is_well_formed());
        let packed = frame.into_packed();
        assert_eq!(packed.stride, 8);
        assert_eq!(
            packed.pixels,
            vec![1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4]
        );
    }

    #[test]
    fn short_buffers_are_rejected() {
        let frame = BgraFrame {
            width: 4,
            height: 2,
            stride: 16,
            pixels: vec![0; 20],
        };
        assert!(!frame.is_well_formed());
    }

    #[test]
    fn timed_pause_expires() {
        let until = Timestamp(1_000);
        let paused = CaptureState::Paused { until: Some(until) };
        assert_eq!(paused.effective_at(Timestamp(999)), paused);
        assert_eq!(
            paused.effective_at(Timestamp(1_000)),
            CaptureState::Recording
        );
        let indefinite = CaptureState::Paused { until: None };
        assert_eq!(indefinite.effective_at(Timestamp(i64::MAX)), indefinite);
    }
}
