//! AppKit owns the main-thread event loop; CLI status and actions run on workers.
//! The bridge passes only menu JSON and row indices, never recording data.
use crate::model::{self, Action, Armed, Click, Item, Recorder, Status};
use crate::{POLL, Runner};
use serde::Serialize;
use std::ffi::{CString, c_char, c_void};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, sync_channel},
};
use std::time::Instant;

// SAFETY: declarations match the @_cdecl functions in native/Tray.swift; the build script
// statically links that bridge only on macOS. Its callbacks are synchronous C ABI functions.
unsafe extern "C" {
    fn rsrewind_tray_run(
        context: *mut c_void,
        snapshot: extern "C" fn(*mut c_void) -> *const c_char,
        click: extern "C" fn(*mut c_void, i32),
    ) -> i32;
    fn rsrewind_tray_quit();
}

#[derive(Serialize)]
struct Row {
    label: String,
    id: i32,
    enabled: bool,
    separator: bool,
}
#[derive(Serialize)]
struct Snapshot {
    headline: String,
    symbol: &'static str,
    recording: bool,
    warning: bool,
    rows: Vec<Row>,
}

fn snapshot(
    status: &Status,
    armed: Option<Armed>,
    now: Instant,
) -> (Snapshot, Vec<Option<Action>>) {
    let mut actions = Vec::new();
    let rows = model::menu(status, armed, now)
        .into_iter()
        .enumerate()
        .map(|(i, item)| {
            let (label, enabled, separator, action) = match item {
                Item::Label(label) => (label, false, false, None),
                Item::Separator => (String::new(), false, true, None),
                Item::Button {
                    label,
                    enabled,
                    action,
                } => (label, enabled, false, enabled.then_some(action)),
            };
            actions.push(action);
            Row {
                label,
                id: i as i32,
                enabled,
                separator,
            }
        })
        .collect();
    let symbol = match status.recorder {
        Recorder::Recording => "record.circle.fill",
        Recorder::Paused { .. } => "pause.circle.fill",
        Recorder::NotRunning => "stop.circle",
        Recorder::Error | Recorder::Unknown => "exclamationmark.circle",
    };
    (
        Snapshot {
            headline: status.headline(),
            symbol,
            recording: matches!(status.recorder, Recorder::Recording),
            warning: status.privacy_unenforced
                || matches!(status.recorder, Recorder::Unknown | Recorder::Error),
            rows,
        },
        actions,
    )
}

struct Tray {
    runner: Runner,
    status: Status,
    statuses: Receiver<Status>,
    received_at: Instant,
    armed: Option<Armed>,
    actions: Vec<Option<Action>>,
    json: Option<CString>,
    stop: Arc<AtomicBool>,
}
impl Drop for Tray {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

extern "C" fn refresh(context: *mut c_void) -> *const c_char {
    // SAFETY: run() passes an exclusive pointer to its live Tray. AppKit invokes both callbacks
    // synchronously on the main thread only, and copies this string before another callback.
    let tray = unsafe { &mut *context.cast::<Tray>() };
    while let Ok(status) = tray.statuses.try_recv() {
        tray.status = status;
        tray.received_at = Instant::now();
    }
    if tray.received_at.elapsed() > POLL * 3 {
        tray.status = Status::UNKNOWN;
    }
    let (state, actions) = snapshot(&tray.status, tray.armed, Instant::now());
    let Ok(json) = serde_json::to_string(&state)
        .and_then(|s| CString::new(s).map_err(serde::ser::Error::custom))
    else {
        return std::ptr::null();
    };
    tray.actions = actions;
    tray.json = Some(json);
    tray.json.as_ref().map_or(std::ptr::null(), |s| s.as_ptr())
}

extern "C" fn clicked(context: *mut c_void, id: i32) {
    // SAFETY: the bridge calls this on the main thread while the Tray passed by run() is live;
    // callbacks never overlap and Swift retains no Rust references after returning.
    let tray = unsafe { &mut *context.cast::<Tray>() };
    let Some(action) = usize::try_from(id)
        .ok()
        .and_then(|i| tray.actions.get(i))
        .copied()
        .flatten()
    else {
        return;
    };
    match model::click(action, tray.armed, Instant::now()) {
        Click::Arm(armed) => tray.armed = Some(armed),
        Click::Run(action) => {
            tray.armed = None;
            if action == Action::Quit {
                // SAFETY: AppKit stop is invoked on its owning main thread. It only stops the
                // tray event loop; capture belongs to a different process.
                unsafe { rsrewind_tray_quit() };
                return;
            }
            // Each action has its own worker: a long-lived `ui` child cannot block status,
            // pause, forget or quit. Quitting the tray does not wait on the UI child.
            let runner = tray.runner.clone();
            if let Err(error) = std::thread::Builder::new()
                .name("tray-action".into())
                .spawn(move || runner.perform(action))
            {
                tracing::warn!(%error, "could not start menu action worker");
            }
        }
    }
}

pub fn run(runner: &Runner) -> anyhow::Result<()> {
    let (send, statuses) = sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker_runner = runner.clone();
    std::thread::Builder::new()
        .name("tray-status".into())
        .spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let status = worker_runner.status();
                // Keep one bounded result; no blocking on the UI thread or unbounded backlog.
                if send.try_send(status).is_err() && worker_stop.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(POLL);
            }
        })?;
    let mut tray = Tray {
        runner: runner.clone(),
        status: Status::UNKNOWN,
        statuses,
        received_at: Instant::now(),
        armed: None,
        actions: Vec::new(),
        json: None,
        stop,
    };
    // SAFETY: Swift checks main-thread ownership before creating AppKit objects and runs
    // synchronously. The stack Tray lives throughout run; callback signatures match the C ABI.
    let result = unsafe { rsrewind_tray_run((&mut tray as *mut Tray).cast(), refresh, clicked) };
    anyhow::ensure!(
        result == 0,
        "the macOS menu bar must run on the application's main thread"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_snapshot_preserves_confirmation_disabled_actions_and_privacy() -> anyhow::Result<()> {
        let now = Instant::now();
        let status = Status {
            recorder: Recorder::NotRunning,
            privacy_unenforced: true,
        };
        let (wire, actions) = snapshot(
            &status,
            Some(Armed {
                minutes: 10,
                at: now,
            }),
            now,
        );
        assert!(wire.warning);
        assert!(wire.headline.contains("privacy rules NOT enforced"));
        assert_eq!(wire.symbol, "stop.circle");
        let pause = wire
            .rows
            .iter()
            .position(|r| r.label == "Pause for 15 minutes")
            .ok_or_else(|| anyhow::anyhow!("missing pause row"))?;
        assert!(!wire.rows[pause].enabled);
        assert_eq!(actions[pause], None);
        let forget = wire
            .rows
            .iter()
            .position(|r| r.label.starts_with("Click again"))
            .ok_or_else(|| anyhow::anyhow!("missing forget confirmation"))?;
        assert_eq!(actions[forget], Some(Action::Forget(10)));
        for (index, row) in wire.rows.iter().enumerate() {
            assert_eq!(row.id as usize, index);
        }
        assert_eq!(actions.last(), Some(&Some(Action::Quit)));
        Ok(())
    }
    #[test]
    fn callback_discards_stale_status() -> anyhow::Result<()> {
        let (_, statuses) = sync_channel(1);
        let mut tray = Tray {
            runner: Runner {
                exe: "unused-test-executable".into(),
                data_dir: "unused".into(),
            },
            status: Status {
                recorder: Recorder::Recording,
                privacy_unenforced: false,
            },
            statuses,
            received_at: Instant::now() - POLL * 4,
            armed: None,
            actions: Vec::new(),
            json: None,
            stop: Arc::new(AtomicBool::new(false)),
        };
        let context = (&mut tray as *mut Tray).cast();
        assert!(!refresh(context).is_null());
        assert_eq!(tray.status, Status::UNKNOWN);
        let json: serde_json::Value = serde_json::from_slice(
            tray.json
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("missing snapshot"))?
                .as_bytes(),
        )?;
        assert_eq!(json["symbol"], "exclamationmark.circle");
        Ok(())
    }
    #[test]
    fn states_are_distinct_without_relying_on_colour() {
        let statuses = [
            Recorder::Recording,
            Recorder::Paused { until: None },
            Recorder::NotRunning,
            Recorder::Unknown,
        ];
        let symbols: Vec<_> = statuses
            .into_iter()
            .map(|recorder| {
                snapshot(
                    &Status {
                        recorder,
                        privacy_unenforced: false,
                    },
                    None,
                    Instant::now(),
                )
                .0
                .symbol
            })
            .collect();
        for (i, symbol) in symbols.iter().enumerate() {
            assert!(!symbols[..i].contains(symbol));
        }
    }
}
