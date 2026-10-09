//! Gaps: stretches of time with no recorded picture, and why.
//!
//! A screen that did not change is **not** a gap: the recorder extends one observation for as long
//! as the picture stays the same. A gap is time no observation covers on any monitor, and its reason
//! comes from what the recorder wrote down at the time (sessions, pause and idle markers).

use crate::{SourceId, Timestamp};
use serde::{Deserialize, Serialize};

/// Why nothing was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    /// No recorder was running (it was stopped, or the machine was off or asleep).
    RecorderOff,
    /// The recorder ended without writing its stop: it crashed, was killed, or the machine lost
    /// power. Starts at the last thing that recorder wrote.
    RecorderDied,
    /// Paused by the user.
    Paused,
    /// No input for longer than `idle_after_secs`, or the screen was locked.
    Idle,
    /// Recording, yet nothing was stored: a privacy rule excluded every screen, or capture failed.
    /// (Privacy skips are not written as events yet, so the two cannot be told apart.)
    NotStored,
}

impl GapReason {
    /// Short, human wording ("recorder off").
    pub const fn describe(self) -> &'static str {
        match self {
            Self::RecorderOff => "recorder off",
            Self::RecorderDied => "recorder stopped unexpectedly",
            Self::Paused => "paused",
            Self::Idle => "idle or locked",
            Self::NotStored => "nothing stored (privacy rule or capture problem)",
        }
    }
}

/// One gap in one source's history: `[from, to)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    pub from: Timestamp,
    pub to: Timestamp,
    pub reason: GapReason,
    /// As in [`crate::TimelineEntry::source`]: absent for this machine's own history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceId>,
}

impl Gap {
    pub fn millis(&self) -> i64 {
        (self.to.as_millis() - self.from.as_millis()).max(0)
    }
}
