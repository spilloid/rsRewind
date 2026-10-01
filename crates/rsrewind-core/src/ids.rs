//! Strongly typed row identifiers.
//!
//! These are SQLite rowids. Wrapping them stops a `VisualStateId` from being passed where an
//! `EventId` is expected — the kind of mix-up that would silently attach OCR text to the wrong
//! screenshot.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! row_id {
    ($($name:ident),* $(,)?) => {$(
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub i64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    )*};
}

row_id!(
    EventId,
    VisualStateId,
    SessionId,
    MonitorId,
    ApplicationId,
    WindowId
);
