//! `rsrewind doctor`: every check reports ok / warn / fail with one plain sentence.

use anyhow::Result;
use rsrewind_core::{Config, DataDir, Timestamp};
use rsrewind_storage::{MIGRATIONS, SCHEMA_VERSION, StorageError, Store};
use serde::Serialize;
use std::process::ExitCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub level: Level,
    pub detail: String,
}

fn check(name: &'static str, level: Level, detail: impl Into<String>) -> Check {
    Check {
        name,
        level,
        detail: detail.into(),
    }
}

pub fn run(data: &DataDir, json: bool) -> Result<ExitCode> {
    let checks = collect(data);
    let worst = checks
        .iter()
        .map(|c| c.level)
        .max_by_key(|l| match l {
            Level::Ok => 0,
            Level::Warn => 1,
            Level::Fail => 2,
        })
        .unwrap_or(Level::Ok);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "result": worst, "checks": checks })
            )?
        );
    } else {
        for c in &checks {
            let tag = match c.level {
                Level::Ok => "[ ok ]",
                Level::Warn => "[warn]",
                Level::Fail => "[FAIL]",
            };
            println!("{tag} {:<14} {}", c.name, c.detail);
        }
    }
    Ok(match worst {
        Level::Fail => ExitCode::FAILURE,
        _ => ExitCode::SUCCESS,
    })
}

fn collect(data: &DataDir) -> Vec<Check> {
    let mut checks = Vec::new();

    // Data folder
    let probe = data.root().join(".rsrewind-doctor-probe");
    match data
        .ensure()
        .map_err(|e| e.to_string())
        .and_then(|()| std::fs::write(&probe, b"ok").map_err(|e| e.to_string()))
    {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            checks.push(check(
                "data folder",
                Level::Ok,
                data.root().display().to_string(),
            ));
        }
        Err(error) => checks.push(check(
            "data folder",
            Level::Fail,
            format!("{} is not writable: {error}", data.root().display()),
        )),
    }

    // Configuration
    let config = match Config::load_or_default(&data.config_file()) {
        Ok(config) => {
            checks.push(check(
                "config",
                Level::Ok,
                if data.config_file().exists() {
                    data.config_file().display().to_string()
                } else {
                    "no config.toml yet; defaults apply".into()
                },
            ));
            Some(config)
        }
        Err(error) => {
            checks.push(check("config", Level::Fail, error.to_string()));
            None
        }
    };

    // Database
    let store = match Store::open_existing(data) {
        Ok(store) => Some(store),
        Err(StorageError::DatabaseMissing(_)) => {
            checks.push(check(
                "database",
                Level::Warn,
                "no database yet; it is created when recording starts",
            ));
            None
        }
        Err(StorageError::SchemaTooNew { found, supported }) => {
            checks.push(check(
                "database",
                Level::Fail,
                format!("made by a newer rsRewind (schema {found}, this build knows {supported}); upgrade rsRewind"),
            ));
            None
        }
        Err(error) => {
            checks.push(check("database", Level::Fail, error.to_string()));
            None
        }
    };
    if let Some(store) = &store {
        checks.push(replication_check(data, store, config.as_ref()));
        // Only ever a warning line: where the rules are enforced, doctor's output is unchanged.
        if let Some(why) = crate::privacy_unenforced(store) {
            checks.push(check(
                "privacy",
                Level::Warn,
                format!(
                    "privacy rules NOT enforced in the latest recording session: {why}; it ran \
                     because privacy.unenforced_ok = true"
                ),
            ));
        }
    }
    if let Some(store) = &store {
        match store.integrity_check() {
            Ok(problems) if problems.is_empty() => checks.push(check(
                "database",
                Level::Ok,
                format!(
                    "integrity ok, schema v{} of {} known migration(s)",
                    store.schema_version().unwrap_or(0),
                    MIGRATIONS.len()
                ),
            )),
            Ok(problems) => checks.push(check(
                "database",
                Level::Fail,
                format!("integrity check reported: {}", problems.join("; ")),
            )),
            Err(error) => checks.push(check("database", Level::Fail, error.to_string())),
        }
        debug_assert!(SCHEMA_VERSION as usize >= MIGRATIONS.len());

        match store.find_orphans(20) {
            Ok(report) => {
                let missing = report.rows_missing_file.len() + report.rows_invalid_path.len();
                let stray = report.files_without_row.len();
                let level = if missing > 0 { Level::Warn } else { Level::Ok };
                checks.push(check(
                    "media",
                    level,
                    match (missing, stray) {
                        (0, 0) => "every stored screen has its image, and no stray files".to_string(),
                        _ => format!("{missing} stored screen(s) missing an image, {stray} stray file(s) (first 20 checked)"),
                    },
                ));
            }
            Err(error) => checks.push(check("media", Level::Warn, error.to_string())),
        }
    }

    // Disk space
    #[cfg(windows)]
    match free_bytes(data.root()) {
        Some(free) => {
            let level = if free < 2 << 30 {
                Level::Fail
            } else if free < 10 << 30 {
                Level::Warn
            } else {
                Level::Ok
            };
            checks.push(check(
                "disk space",
                level,
                format!("{} free on the data drive", crate::human_bytes(free)),
            ));
        }
        None => checks.push(check(
            "disk space",
            Level::Warn,
            "could not read free space",
        )),
    }

    // Capture
    #[cfg(windows)]
    {
        let _ = rsrewind_capture::enable_dpi_awareness();
        match rsrewind_capture::monitors() {
            Ok(monitors) if monitors.is_empty() => {
                checks.push(check("capture", Level::Fail, "Windows reports no displays"))
            }
            Ok(monitors) => {
                // Starting a capture session proves Windows.Graphics.Capture works here; no frame
                // is read or stored.
                let (handle, info) = &monitors[0];
                match rsrewind_capture::MonitorCapturer::new(*handle) {
                    Ok(_) => checks.push(check(
                        "capture",
                        Level::Ok,
                        format!(
                            "{} display(s); screen capture works on {} ({}x{})",
                            monitors.len(),
                            info.device_name,
                            info.width,
                            info.height
                        ),
                    )),
                    Err(error) => checks.push(check(
                        "capture",
                        Level::Fail,
                        format!("Windows.Graphics.Capture failed: {error}"),
                    )),
                }
            }
            Err(error) => checks.push(check("capture", Level::Fail, error.to_string())),
        }
    }

    #[cfg(target_os = "macos")]
    {
        if !rsrewind_capture::macos::screen_recording_permission(false) {
            checks.push(check("capture", Level::Fail, "Grant rsRewind Screen & System Audio Recording in System Settings → Privacy & Security, then restart the app"));
        } else {
            match rsrewind_capture::macos::monitors() {
                Ok(displays) if !displays.is_empty() => checks.push(check("capture", Level::Ok, format!("{} display(s); screen-recording permission granted; interactive capture still needs verification", displays.len()))),
                Ok(_) => checks.push(check("capture", Level::Fail, "macOS reports no displays")),
                Err(_) => checks.push(check("capture", Level::Fail, "Could not enumerate macOS displays")),
            }
            match rsrewind_capture::macos::visible_windows() {
                Ok(_) => checks.push(check(
                    "window context",
                    Level::Ok,
                    "Native visible-window context is available",
                )),
                Err(_) => checks.push(check(
                    "window context",
                    Level::Fail,
                    "Window context is unavailable; recording will skip ticks until it is known",
                )),
            }
        }
        if rsrewind_capture::macos::idle_millis().is_none() {
            checks.push(check(
                "idle/session",
                Level::Warn,
                "Idle or active-session state is unknown; nothing will be recorded",
            ));
        }
    }

    // OCR
    #[cfg(windows)]
    if let Some(config) = &config {
        if !config.ocr.enabled {
            checks.push(check(
                "ocr",
                Level::Warn,
                "OCR is turned off in config.toml; search will find nothing new",
            ));
        } else {
            let language = Some(config.ocr.language.as_str()).filter(|l| !l.trim().is_empty());
            match rsrewind_ocr::OcrEngine::new(language) {
                Ok(_) => checks.push(check(
                    "ocr",
                    Level::Ok,
                    format!(
                        "Windows OCR ready; languages installed: {}",
                        rsrewind_ocr::OcrEngine::available_languages().join(", ")
                    ),
                )),
                Err(error) => checks.push(check("ocr", Level::Fail, error.to_string())),
            }
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(config) = &config {
        if !config.ocr.enabled {
            checks.push(check(
                "ocr",
                Level::Warn,
                "OCR is turned off; search will find nothing new",
            ));
        } else {
            let language = Some(config.ocr.language.as_str()).filter(|l| !l.trim().is_empty());
            match rsrewind_ocr::VisionEngine::new(language) {
                Ok(_) => checks.push(check(
                    "ocr",
                    Level::Ok,
                    "Apple Vision OCR is available on device",
                )),
                Err(_) => checks.push(check(
                    "ocr",
                    Level::Fail,
                    "Apple Vision OCR initialization failed",
                )),
            }
        }
    }
    #[cfg(target_os = "linux")]
    if let Some(config) = &config {
        let language = Some(config.ocr.language.as_str()).filter(|l| !l.trim().is_empty());
        match rsrewind_ocr::TesseractEngine::new(language) {
            Ok(engine) => checks.push(check(
                "ocr",
                Level::Ok,
                format!("Tesseract ready (language {})", engine.language()),
            )),
            Err(message) => checks.push(check(
                "ocr",
                Level::Warn,
                format!("no text recognition yet: {message}; recorded moments wait until then"),
            )),
        }
    }

    // Privacy
    if let Some(config) = &config {
        let rules =
            config.privacy.excluded_processes.len() + config.privacy.excluded_title_patterns.len();
        checks.push(check(
            "privacy",
            if rules == 0 { Level::Warn } else { Level::Ok },
            if rules == 0 {
                "no exclusion rules: every window, including password managers, can be recorded"
                    .to_string()
            } else {
                format!(
                    "{} application and {} window-title exclusion(s)",
                    config.privacy.excluded_processes.len(),
                    config.privacy.excluded_title_patterns.len()
                )
            },
        ));
    }

    // Recorder and backlog
    if let Some(store) = &store {
        let now = Timestamp::now();
        match store.read_status() {
            Ok(Some(status)) => {
                let age = now.as_millis() - status.heartbeat_at.as_millis();
                let running = age < 20_000 && status.state != rsrewind_core::CaptureState::Stopped;
                checks.push(check(
                    "recorder",
                    if running { Level::Ok } else { Level::Warn },
                    if running {
                        format!(
                            "running (pid {}), last heartbeat {}s ago",
                            status.pid,
                            age / 1000
                        )
                    } else {
                        format!("not running; last seen {}", status.heartbeat_at)
                    },
                ));
            }
            Ok(None) => checks.push(check("recorder", Level::Warn, "has never run")),
            Err(error) => checks.push(check("recorder", Level::Warn, error.to_string())),
        }
        if let Ok(stats) = store.stats() {
            checks.push(check(
                "ocr backlog",
                if stats.pending_ocr > 500 {
                    Level::Warn
                } else {
                    Level::Ok
                },
                format!("{} waiting, {} failed", stats.pending_ocr, stats.failed_ocr),
            ));
        }
    }

    checks
}

#[cfg(windows)]
fn free_bytes(path: &std::path::Path) -> Option<u64> {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::core::HSTRING;
    let path = HSTRING::from(path.as_os_str());
    let mut free = 0u64;
    // SAFETY: `path` is a valid NUL-terminated wide string for the duration of the call and
    // `free` is a valid out-pointer.
    unsafe { GetDiskFreeSpaceExW(&path, Some(&mut free), None, None) }.ok()?;
    Some(free)
}

/// Sealed-segment backlog and whether history could be deleted before it is exported.
fn replication_check(data: &DataDir, store: &Store, config: Option<&Config>) -> Check {
    const NAME: &str = "replication";
    let (status, scan) = match (
        store.export_status(),
        rsrewind_storage::scan_outbox(&data.outbox()),
    ) {
        (Ok(status), Ok(scan)) => (status, scan),
        (Err(e), _) => return check(NAME, Level::Warn, e.to_string()),
        (_, Err(e)) => return check(NAME, Level::Warn, e.to_string()),
    };
    if status.cut_ms == 0 && scan.segments.is_empty() && scan.unreadable.is_empty() {
        return check(
            NAME,
            Level::Ok,
            "not exporting (run `rsrewind export` to seal history for another machine)",
        );
    }
    if !scan.unreadable.is_empty() {
        return check(
            NAME,
            Level::Warn,
            format!(
                "{} outbox file(s) do not verify and will not import",
                scan.unreadable.len()
            ),
        );
    }
    let bytes: u64 = scan.segments.iter().map(|s| s.bytes).sum();
    let lag_days = (Timestamp::now().0 - status.cut_ms).max(0) / 86_400_000;
    if let Some(config) = config {
        let keep = i64::from(config.storage.retention_days);
        if keep > 0 && lag_days * 2 >= keep && status.cut_ms > 0 {
            return check(
                NAME,
                Level::Warn,
                format!(
                    "last export covered history up to {lag_days} day(s) ago and retention keeps {keep}; run `rsrewind export` before retention deletes unexported history"
                ),
            );
        }
    }
    check(
        NAME,
        Level::Ok,
        format!(
            "{} sealed segment(s) in the outbox ({}); exported through {lag_days} day(s) ago",
            scan.segments.len(),
            super::human_bytes(bytes)
        ),
    )
}
