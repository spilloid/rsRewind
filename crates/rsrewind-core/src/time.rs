//! UTC timestamps with millisecond precision.
//!
//! Stored in SQLite as a plain integer (Unix milliseconds) so that range queries are index-friendly
//! and the database stays readable from any tool. Local time only appears at presentation edges.

use chrono::{DateTime, Local, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub fn now() -> Self {
        Self(Utc::now().timestamp_millis())
    }

    pub fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    pub fn as_millis(self) -> i64 {
        self.0
    }

    pub fn to_utc(self) -> DateTime<Utc> {
        Utc.timestamp_millis_opt(self.0)
            .single()
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
    }

    pub fn to_local(self) -> DateTime<Local> {
        self.to_utc().with_timezone(&Local)
    }

    pub fn saturating_sub_millis(self, millis: i64) -> Self {
        Self(self.0.saturating_sub(millis))
    }

    pub fn saturating_add_millis(self, millis: i64) -> Self {
        Self(self.0.saturating_add(millis))
    }
}

impl From<DateTime<Utc>> for Timestamp {
    fn from(value: DateTime<Utc>) -> Self {
        Self(value.timestamp_millis())
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_local().format("%Y-%m-%d %H:%M:%S"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_chrono() {
        let ts = Timestamp::from_millis(1_790_000_000_123);
        assert_eq!(Timestamp::from(ts.to_utc()), ts);
    }

    #[test]
    fn out_of_range_millis_do_not_panic() {
        assert_eq!(Timestamp(i64::MAX).to_utc(), DateTime::<Utc>::UNIX_EPOCH);
    }
}
