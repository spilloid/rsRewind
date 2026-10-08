//! `rsrewind.exe`: one executable for the recorder, the UI and every command-line tool.

mod doctor;
mod render;
mod replicate;
mod timespec;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
#[cfg(windows)]
use rsrewind_core::Config;
use rsrewind_core::{CaptureState, DataDir, SearchQuery, Timestamp};
use rsrewind_query::QueryDb;
use rsrewind_storage::{RecorderStatus, StorageError, Store};
use serde::Serialize;
use std::path::PathBuf;
use std::process::ExitCode;

/// A heartbeat older than this means the recorder is not running (it beats every 5 s).
const STALE_HEARTBEAT_MS: i64 = 20_000;

#[derive(Parser)]
#[command(
    name = "rsrewind",
    version,
    about = "rsRewind: a local-first, searchable record of what happened on your Windows desktop."
)]
struct Cli {
    /// Use this data folder instead of %LOCALAPPDATA%\rsRewind (also: RSREWIND_DATA_DIR).
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the recorder in this console until Ctrl+C or `rsrewind stop`.
    Daemon,
    /// Start the recorder in the background.
    Start,
    /// Stop the background recorder.
    Stop,
    /// Show whether rsRewind is recording, and what it has stored.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Pause recording, indefinitely or for a number of minutes.
    Pause {
        #[arg(long, value_name = "N")]
        minutes: Option<u32>,
    },
    /// Resume recording.
    Resume,
    /// Search recognized text.
    Search {
        /// Words to find. Quote a phrase for an exact match; end a word with * for a prefix.
        text: Vec<String>,
        /// Only this application (process name, `.exe` optional).
        #[arg(long)]
        app: Option<String>,
        /// Only windows whose title contains this.
        #[arg(long)]
        title: Option<String>,
        /// Start of the time range: '2026-09-30 14:00', 'today', '2h', ...
        #[arg(long)]
        since: Option<String>,
        /// End of the time range.
        #[arg(long)]
        until: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// List the most recently recorded moments.
    Recent {
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// Permanently delete everything recorded since a time (e.g. `forget 15m`).
    Forget {
        since: String,
        /// Confirm the deletion. Without it, only show what would happen.
        #[arg(long)]
        yes: bool,
    },
    /// Check that everything rsRewind needs is in working order.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Seal recorded history into segment files you can carry to another rsRewind.
    ///
    /// Writes to <data folder>\outbox unless --out is given. Moving the files (copy, rsync,
    /// a share, a sync tool, over whatever private network you run) is up to you; rsRewind does
    /// no networking.
    Export {
        /// Where to write segments.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// Only seal history that has been quiet for this many minutes (at least 1).
        #[arg(long, value_name = "N", default_value_t = 5)]
        settle_minutes: u32,
        /// Most events in one segment; a bigger backlog becomes several segments.
        #[arg(long, value_name = "N", default_value_t = rsrewind_storage::DEFAULT_MAX_EVENTS)]
        max_events: usize,
        /// Name shown for this machine on the receiving side (default: its host name).
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Merge segment files (or folders of them) from other machines into this data folder.
    ///
    /// Each source gets its own store under <data folder>\sources\<id>. Importing a segment
    /// twice is harmless.
    Import {
        #[arg(required = true, value_name = "FILE_OR_DIR")]
        paths: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// List the remote sources whose history this data folder holds.
    Sources {
        #[arg(long)]
        json: bool,
    },
    /// Open the rsRewind window (in its own process; this command returns at once).
    Ui {
        /// Run the window in this process and wait until it is closed.
        #[arg(long)]
        foreground: bool,
    },
    /// Print the data folder.
    DataDir,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("rsrewind: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let data = match cli.data_dir {
        Some(dir) => DataDir::new(dir),
        None => DataDir::resolve()?,
    };
    match cli.command {
        Command::Daemon => daemon(&data),
        Command::Start => start(&data),
        Command::Stop => stop(&data),
        Command::Status { json } => status(&data, json),
        Command::Pause { minutes } => pause(&data, minutes),
        Command::Resume => resume(&data),
        Command::Search {
            text,
            app,
            title,
            since,
            until,
            limit,
            json,
        } => search(&data, text.join(" "), app, title, since, until, limit, json),
        Command::Recent { limit, json } => recent(&data, limit, json),
        Command::Forget { since, yes } => forget(&data, &since, yes),
        Command::Doctor { json } => doctor::run(&data, json),
        Command::Export {
            out,
            settle_minutes,
            max_events,
            label,
            json,
        } => replicate::export(&data, out, settle_minutes, max_events, label, json),
        Command::Import { paths, json } => replicate::import(&data, &paths, json),
        Command::Sources { json } => replicate::sources(&data, json),
        Command::Ui { foreground } => ui(&data, foreground),
        Command::DataDir => {
            println!("{}", data.root().display());
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[cfg(windows)]
fn load_config(data: &DataDir) -> Result<Config> {
    Config::write_default_if_missing(&data.config_file())?;
    Ok(Config::load_or_default(&data.config_file())?)
}

#[cfg(windows)]
fn init_logging(
    data: &DataDir,
    config: &Config,
    foreground: bool,
    prefix: &str,
) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;
    let filter = tracing_subscriber::EnvFilter::try_new(&config.logging.level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(prefix)
        .filename_suffix("log")
        .max_log_files(14)
        .build(data.logs())
        .context("open log folder")?;
    let (file, guard) = tracing_appender::non_blocking(appender);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file)
        .with_ansi(false);
    let console = foreground.then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(console)
        .init();
    Ok(guard)
}

#[cfg(windows)]
fn daemon(data: &DataDir) -> Result<ExitCode> {
    data.ensure()?;
    let config = load_config(data)?;
    let _guard = init_logging(data, &config, true, "rsrewind")?;
    rsrewind_daemon::run(rsrewind_daemon::RunOptions {
        data: data.clone(),
        config,
    })?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(windows)]
fn start(data: &DataDir) -> Result<ExitCode> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    if let Some(status) = live_status(data)? {
        println!("rsRewind is already recording (pid {}).", status.pid);
        return Ok(ExitCode::SUCCESS);
    }
    data.ensure()?;
    load_config(data)?;
    let exe = std::env::current_exe().context("locate rsrewind.exe")?;
    let child = std::process::Command::new(exe)
        .arg("--data-dir")
        .arg(data.root())
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .spawn()
        .context("start the recorder")?;
    let pid = child.id();
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if let Some(status) = live_status(data)?
            && status.pid == pid
        {
            println!(
                "● Recording (pid {pid}). Stop with `rsrewind stop`, pause with `rsrewind pause`."
            );
            return Ok(ExitCode::SUCCESS);
        }
    }
    bail!(
        "the recorder did not report in within 10 seconds; see the logs in {}",
        data.logs().display()
    )
}

#[cfg(windows)]
fn stop(data: &DataDir) -> Result<ExitCode> {
    if !rsrewind_daemon::win::signal_stop()? {
        println!("rsRewind is not recording in this session.");
        return Ok(ExitCode::SUCCESS);
    }
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if live_status(data)?.is_none() {
            println!("Recorder stopped.");
            return Ok(ExitCode::SUCCESS);
        }
    }
    println!("Stop requested; the recorder is finishing its current work.");
    Ok(ExitCode::SUCCESS)
}

/// The recorder's heartbeat, if it is recent and not a final "stopped" beat.
fn live_status(data: &DataDir) -> Result<Option<RecorderStatus>> {
    let store = match Store::open_existing(data) {
        Ok(store) => store,
        Err(StorageError::DatabaseMissing(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(store.read_status()?.filter(|status| {
        status.state != CaptureState::Stopped
            && Timestamp::now().as_millis() - status.heartbeat_at.as_millis() < STALE_HEARTBEAT_MS
    }))
}

#[derive(Serialize)]
struct StatusReport {
    /// recording | paused | not_running | error
    state: &'static str,
    running: bool,
    paused_until: Option<Timestamp>,
    pid: Option<u32>,
    heartbeat_age_ms: Option<i64>,
    data_dir: String,
    counters: std::collections::BTreeMap<String, u64>,
    stats: rsrewind_storage::StorageStats,
    /// Present only when the newest recording session could not enforce the privacy rules (it ran
    /// with `privacy.unenforced_ok`): names what the platform was missing. Absent otherwise, so
    /// output where the rules are enforced is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    privacy_unenforced: Option<String>,
}

/// Why the newest recording session did not enforce the privacy rules, or `None` if it did (or
/// no session recorded its capabilities). An unreadable record counts as not enforced: the badge
/// must never disappear because of a fault.
pub(crate) fn privacy_unenforced(store: &Store) -> Option<String> {
    match store.latest_session_capabilities() {
        Ok(Some((_, recorded))) if !recorded.privacy_enforced => Some(format!(
            "this platform could not provide the {}",
            recorded.privacy_gaps.join(", ")
        )),
        Ok(_) => None,
        Err(error) => Some(format!(
            "could not read what the recorder could see: {error}"
        )),
    }
}

fn status(data: &DataDir, json: bool) -> Result<ExitCode> {
    let store = match Store::open_existing(data) {
        Ok(store) => store,
        Err(StorageError::DatabaseMissing(_)) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "state": "not_running", "running": false, "recorded": false })
                );
            } else {
                println!("○ Not recording. Nothing on tape yet; start with `rsrewind start`.");
            }
            return Ok(ExitCode::SUCCESS);
        }
        Err(error) => return Err(error.into()),
    };
    let now = Timestamp::now();
    let heartbeat = store.read_status()?;
    let live = heartbeat.as_ref().filter(|s| {
        s.state != CaptureState::Stopped
            && now.as_millis() - s.heartbeat_at.as_millis() < STALE_HEARTBEAT_MS
    });
    let control = store.read_control()?.effective_at(now);
    let (state, paused_until) = match (live, control) {
        (None, _) => ("not_running", None),
        (Some(s), _) if s.state == CaptureState::Error => ("error", None),
        (Some(_), CaptureState::Paused { until }) => ("paused", until),
        (Some(_), _) => ("recording", None),
    };
    let report = StatusReport {
        state,
        running: live.is_some(),
        paused_until,
        pid: live.map(|s| s.pid),
        heartbeat_age_ms: heartbeat
            .as_ref()
            .map(|s| now.as_millis() - s.heartbeat_at.as_millis()),
        data_dir: data.root().display().to_string(),
        counters: live.map(|s| s.counters.clone()).unwrap_or_default(),
        stats: store.stats()?,
        privacy_unenforced: privacy_unenforced(&store),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(ExitCode::SUCCESS);
    }
    let headline = match (report.state, report.paused_until) {
        ("recording", _) => "● Recording".to_string(),
        ("paused", Some(until)) => format!("○ Paused until {}", until.to_local().format("%H:%M")),
        ("paused", None) => "○ Paused".to_string(),
        ("error", _) => {
            "⚠ Error: the recorder reported a problem; run `rsrewind doctor`".to_string()
        }
        _ => "○ Not recording".to_string(),
    };
    println!(
        "{headline}{}",
        report
            .pid
            .map(|p| format!("   (pid {p})"))
            .unwrap_or_default()
    );
    if let Some(why) = &report.privacy_unenforced {
        println!("⚠ Privacy rules NOT enforced: {why} (privacy.unenforced_ok = true).");
    }
    let s = &report.stats;
    println!("Data      {}", report.data_dir);
    println!(
        "History   {} moments on {} screens of history{}",
        s.observations,
        s.visual_states,
        match (s.oldest, s.newest) {
            (Some(a), Some(b)) => format!(", {a} to {b}"),
            _ => String::new(),
        }
    );
    println!(
        "Storage   {} database, {} images",
        human_bytes(s.db_bytes),
        human_bytes(s.media_bytes)
    );
    println!(
        "OCR       {} indexed, {} waiting, {} failed",
        s.visual_states.saturating_sub(s.pending_ocr + s.failed_ocr),
        s.pending_ocr,
        s.failed_ocr
    );
    Ok(ExitCode::SUCCESS)
}

fn pause(data: &DataDir, minutes: Option<u32>) -> Result<ExitCode> {
    let store = Store::open(data)?;
    let until = minutes.map(|m| Timestamp::now().saturating_add_millis(i64::from(m) * 60_000));
    store.set_control(CaptureState::Paused { until })?;
    match until {
        Some(until) => println!(
            "○ Paused until {}. Resume early with `rsrewind resume`.",
            until.to_local().format("%H:%M")
        ),
        None => println!("○ Paused. Your timeline will stay quiet until `rsrewind resume`."),
    }
    Ok(ExitCode::SUCCESS)
}

fn resume(data: &DataDir) -> Result<ExitCode> {
    let store = Store::open(data)?;
    store.set_control(CaptureState::Recording)?;
    if live_status(data)?.is_some() {
        println!("● Recording resumed.");
    } else {
        println!("Recording will resume when the recorder runs (`rsrewind start`).");
    }
    Ok(ExitCode::SUCCESS)
}

#[allow(clippy::too_many_arguments)]
fn search(
    data: &DataDir,
    text: String,
    app: Option<String>,
    title: Option<String>,
    since: Option<String>,
    until: Option<String>,
    limit: u32,
    json: bool,
) -> Result<ExitCode> {
    let now = chrono::Local::now();
    let parse = |value: Option<String>| -> Result<Option<Timestamp>> {
        value
            .map(|v| timespec::parse_timespec(&v, now).map_err(anyhow::Error::msg))
            .transpose()
    };
    let query = SearchQuery {
        text,
        since: parse(since)?,
        until: parse(until)?,
        application: app,
        title_contains: title,
        limit,
    };
    let db = open_query(data)?;
    let hits = db.search(&query)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&hits)?);
    } else {
        print!("{}", render::search_hits(&hits));
    }
    Ok(if hits.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn recent(data: &DataDir, limit: u32, json: bool) -> Result<ExitCode> {
    let db = open_query(data)?;
    let entries = db.recent(limit, None)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        print!("{}", render::timeline(&entries));
    }
    Ok(ExitCode::SUCCESS)
}

fn open_query(data: &DataDir) -> Result<QueryDb> {
    QueryDb::open(data).with_context(|| {
        format!(
            "no rsRewind history in {} yet (start recording with `rsrewind start`)",
            data.root().display()
        )
    })
}

fn forget(data: &DataDir, since: &str, yes: bool) -> Result<ExitCode> {
    let since =
        timespec::parse_timespec(since, chrono::Local::now()).map_err(anyhow::Error::msg)?;
    // The deletion fence covers [since, until): anything the recorder captured up to now is
    // refused if it is still queued. Padding `until` into the future would make the recorder drop
    // frames it is entitled to keep, so it is exactly "now" (+1 ms for the exclusive bound).
    let until = Timestamp::now().saturating_add_millis(1);
    if !yes {
        println!(
            "This permanently deletes everything recorded since {since}: screenshots, recognized text and window history.\nRun again with --yes to delete."
        );
        return Ok(ExitCode::from(2));
    }
    let store = Store::open_existing(data)?;
    // Counted first: once the range is deleted there is nothing left to compare against.
    let already_sealed = store.count_sealed_events(since, until)?;
    let report = store.delete_range(since, until)?;
    replicate::forget_exported(data, since, until, already_sealed);
    println!(
        "Deleted {} moments and {} screenshots ({} freed).",
        report.events_deleted,
        report.visual_states_deleted,
        human_bytes(report.bytes_freed)
    );
    if report.orphan_files_deleted > 0 {
        println!(
            "Also removed {} leftover screenshot files that had no database entry.",
            report.orphan_files_deleted
        );
    }
    if !report.wal_truncated {
        println!(
            "Note: the database's write-ahead log could not be cleared while another process was reading it; deleted pages are overwritten, and the log is cleared at the next checkpoint."
        );
    }
    if !report.backups_surviving.is_empty() {
        println!(
            "Note: {} database backup(s) made before a schema upgrade still exist and may contain this history. They are never deleted automatically; remove them yourself if you want them gone:",
            report.backups_surviving.len()
        );
        for backup in &report.backups_surviving {
            println!("  {backup}");
        }
    }
    for error in &report.file_errors {
        eprintln!("could not delete {error}");
    }
    Ok(if report.file_errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The window always runs in a process of its own, never inside the recorder: by default this
/// starts `rsrewind ui --foreground` detached (no console) and returns, so a crash or hang in the
/// window cannot touch recording or the terminal it was started from.
#[cfg(windows)]
fn ui(data: &DataDir, foreground: bool) -> Result<ExitCode> {
    if foreground {
        // Read the config without creating one: the viewer writes nothing to the data folder
        // except its own log.
        let config = Config::load_or_default(&data.config_file()).unwrap_or_default();
        let _guard = init_logging(data, &config, false, "rsrewind-ui")?;
        rsrewind_ui::run(data.clone())?;
        return Ok(ExitCode::SUCCESS);
    }
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    let exe = std::env::current_exe().context("find rsrewind.exe")?;
    let child = std::process::Command::new(exe)
        .arg("--data-dir")
        .arg(data.root())
        .args(["ui", "--foreground"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .context("start the rsRewind window")?;
    println!("Opened the rsRewind window (process {}).", child.id());
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(windows))]
fn daemon(_: &DataDir) -> Result<ExitCode> {
    bail!("the recorder runs on Windows only")
}
#[cfg(not(windows))]
fn start(_: &DataDir) -> Result<ExitCode> {
    bail!("the recorder runs on Windows only")
}
#[cfg(not(windows))]
fn stop(_: &DataDir) -> Result<ExitCode> {
    bail!("the recorder runs on Windows only")
}
#[cfg(not(windows))]
fn ui(_: &DataDir, _foreground: bool) -> Result<ExitCode> {
    bail!("the UI runs on Windows only")
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn bytes_are_human() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.0 GB");
    }
}
