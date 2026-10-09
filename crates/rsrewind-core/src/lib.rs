//! Shared domain vocabulary for rsRewind.
//!
//! Everything here is plain data or pure logic: no SQLite, no Windows capture, no UI. Crates on
//! either side of a boundary (capture → daemon → storage → query → UI/CLI) agree on these types so
//! that none of them has to know how the others are implemented.

pub mod capabilities;
pub mod config;
pub mod error;
pub mod event;
pub mod gap;
pub mod ids;
pub mod paths;
pub mod privacy;
pub mod schema;
pub mod search;
pub mod source;
pub mod time;

pub use capabilities::{Capabilities, SessionCapabilities};
pub use config::Config;
pub use error::{CoreError, Result};
pub use event::{
    ApplicationContext, BgraFrame, CaptureState, EventKind, FocusContext, MonitorInfo, OcrBlock,
    WindowContext,
};
pub use gap::{Gap, GapReason};
pub use ids::{ApplicationId, EventId, MonitorId, SessionId, VisualStateId, WindowId};
pub use paths::{DataDir, MediaPathError};
pub use privacy::{PrivacyDecision, PrivacyPolicy};
pub use schema::SCHEMA_VERSION;
pub use search::{SearchHit, SearchQuery, TimelineCursor, TimelineEntry, VisualDetail};
pub use source::SourceId;
pub use time::Timestamp;
