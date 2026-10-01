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
