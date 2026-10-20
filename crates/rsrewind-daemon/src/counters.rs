//! Operational counters, shared by all recorder threads and published in the heartbeat row.
//! They count events, never content. Three are gauges rather than counts: `health` (a
//! [`crate::health::Health`] code), `health_since_ms` (when it began, Unix ms) and
//! `persist_failing` (jobs the persist thread failed since it last wrote anything).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

macro_rules! counters {
    ($($name:ident),* $(,)?) => {
        #[derive(Debug, Default)]
        pub struct Counters {
            $(pub $name: AtomicU64,)*
        }

        impl Counters {
            pub fn snapshot(&self) -> BTreeMap<String, u64> {
                let mut map = BTreeMap::new();
                $(map.insert(stringify!($name).to_string(), self.$name.load(Ordering::Relaxed));)*
                map
            }
        }
    };
}

counters!(
    ticks,
    candidate_frames,
    persisted_states,
    extended_observations,
    unchanged_frames,
    dropped_queue_full,
    privacy_skips,
    paused_ticks,
    idle_ticks,
    capture_errors,
    capturer_restarts,
    persist_errors,
    storage_bytes_written,
    ocr_done,
    ocr_failed,
    ocr_ms_total,
    retention_runs,
    window_list_unknown_ticks,
    idle_unknown_ticks,
    untitled_app_window_ticks,
    health,
    health_since_ms,
    persist_failing,
);

/// Heartbeat key, not a counter: the recorder run's random stop token where `rsrewind stop`
/// signals an event named for it (Windows; see [`crate::platform::Lifecycle::stop_token`]).
/// Absent from heartbeats of recorders up to 0.1.2 and on other platforms.
pub const STOP_TOKEN_KEY: &str = "stop_token";

pub fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

pub fn add(counter: &AtomicU64, amount: u64) {
    counter.fetch_add(amount, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reports_every_counter() {
        let counters = Counters::default();
        bump(&counters.persisted_states);
        add(&counters.ocr_ms_total, 40);
        let snap = counters.snapshot();
        assert_eq!(snap.get("persisted_states"), Some(&1));
        assert_eq!(snap.get("ocr_ms_total"), Some(&40));
        assert_eq!(snap.get("ticks"), Some(&0));
        assert_eq!(snap.len(), 23);
        assert_eq!(snap.get(crate::health::HEALTH_KEY), Some(&0));
        assert_eq!(snap.get(crate::health::HEALTH_SINCE_KEY), Some(&0));
    }
}
