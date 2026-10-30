//! The persist loop on a real store, one job at a time.

use super::fakes::*;
use crate::counters::Counters;
use crate::persist::{Handoff, Job};
use rsrewind_core::{DataDir, EventKind, Timestamp};
use rsrewind_storage::Store;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn at(secs: u64) -> Timestamp {
    FakeClock::wall_at(Duration::from_secs(secs))
}

fn media_files(data: &DataDir) -> Result<Vec<std::path::PathBuf>, Box<dyn std::error::Error>> {
    let mut found = Vec::new();
    let mut stack = vec![data.media_root()];
    while let Some(dir) = stack.pop() {
        if !dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            } else {
                found.push(entry.path());
            }
        }
    }
    Ok(found)
}

static REFUSED_ONCE: AtomicBool = AtomicBool::new(false);

/// Deletion as on Windows while another program (a viewer, a scanner) holds the file open without
/// delete sharing: refused the first time, allowed after.
fn refuse_first_delete(path: &Path) -> std::io::Result<()> {
    if !REFUSED_ONCE.swap(true, Ordering::SeqCst) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the file is open in another program",
        ));
    }
    std::fs::remove_file(path)
}

/// astra review, 2026-10-19, finding 2. A picture queued before a `forget` is written to disk,
/// then refused by the forget's fence, and its file must go. Before the fix a failed delete was
/// ignored and the refusal treated as an ordinary expected drop: the screenshot stayed on disk
/// with no row, no warning and no error count. Now the failure is counted and logged, the delete
/// is retried after the next job, and the screen is marked to be offered again (it is still on
/// display after the forgotten interval).
#[test]
fn a_fenced_picture_whose_file_cannot_be_deleted_is_reported_and_deleted_later() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path().join("data"));
    let counters = Arc::new(Counters::default());
    let handoff = Arc::new(Handoff::default());
    let mut persister = persister(&data, counters.clone(), handoff.clone())?;
    persister.set_remove_file(refuse_first_delete);

    // `rsrewind forget` of 0..10 s commits while a picture taken at 5 s is still queued.
    Store::open_existing(&data)?.delete_range(at(0), at(10))?;
    let (_, info) = monitor(0);
    let device = info.device_name.clone();
    persister.step(Job::NewState {
        monitor: info,
        frame: frame(1),
        fingerprint: 1,
        at: at(5),
        subject: None,
    });
    assert_eq!(
        counters.persist_errors.load(Ordering::Relaxed),
        1,
        "a screenshot left behind must count as a failure"
    );
    assert_eq!(media_files(&data)?.len(), 1, "the delete was refused");
    assert!(handoff.take_unsaved().contains(&device));

    // The next job (any) retries the delete.
    persister.step(Job::Marker {
        kind: EventKind::IdleEnd,
        at: at(20),
        metadata: None,
    });
    assert_eq!(media_files(&data)?, Vec::<std::path::PathBuf>::new());
    assert_eq!(Store::open_existing(&data)?.stats()?.visual_states, 0);
    Ok(())
}

/// A picture deleted under the recorder (`forget`, retention) while its screen is
/// still on display: the next extension finds it gone, and the monitor is marked so the screen is
/// offered again as a new picture instead of going unrecorded until it changes (remediation F5's
/// open item). A picture that saves clears its own monitor's mark.
#[test]
fn a_picture_deleted_under_the_recorder_is_offered_again() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path().join("data"));
    let counters = Arc::new(Counters::default());
    let handoff = Arc::new(Handoff::default());
    let mut persister = persister(&data, counters.clone(), handoff.clone())?;
    let (_, info) = monitor(0);
    let device = info.device_name.clone();
    let new_state = |seconds: u64| Job::NewState {
        monitor: info.clone(),
        frame: frame(1),
        fingerprint: 1,
        at: at(seconds),
        subject: None,
    };
    let extend = |seconds: u64| Job::Extend {
        device_name: device.clone(),
        at: at(seconds),
        subject: None,
    };

    persister.step(new_state(1));
    persister.step(extend(2));
    assert!(handoff.take_unsaved().is_empty());
    Store::open_existing(&data)?.delete_range(at(0), at(3))?;
    persister.step(extend(4));
    assert_eq!(
        handoff.take_unsaved(),
        std::collections::HashSet::from([device.clone()])
    );
    persister.step(extend(5)); // marked again until a picture is saved...
    persister.step(new_state(6)); // ...which clears it
    assert!(handoff.take_unsaved().is_empty());
    assert_eq!(counters.persist_errors.load(Ordering::Relaxed), 0);
    Ok(())
}
