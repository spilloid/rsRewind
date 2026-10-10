//! `rsrewind.exe`: one executable for the recorder, the UI and every command-line tool.

mod doctor;
mod render;
mod replicate;
mod timespec;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use rsrewind_core::{CaptureState, Config, DataDir, SearchQuery, Timestamp};
use rsrewind_query::{History, SourceFilter};
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
    /// Show when nothing was recorded, and why (recorder off, paused, idle or locked, ...).
    Gaps {
        /// Start of the time range (default: 24 hours ago).
        #[arg(long, default_value = "24h")]
        since: String,
        /// End of the time range (default: now).
        #[arg(long)]
        until: Option<String>,
        /// Ignore gaps shorter than this.
        #[arg(long, value_name = "N", default_value_t = 60)]
        min_seconds: u32,
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
        /// Light or dark; by default the window follows the system.
        #[arg(long, value_enum, default_value_t = Appearance::System)]
        appearance: Appearance,
    },
    /// Show the notification-area icon: recording state, pause, resume, forget the last 10
    /// minutes or hour, open the window, start/stop (its own process; returns at once).
    Tray {
        /// Run in this process and wait until the icon is quit.
        #[arg(long)]
        foreground: bool,
        /// Start the recorder too, if it is not running.
        #[arg(long)]
        start_recorder: bool,
        /// Start the icon and the recorder when you log in (`on`), or stop doing so (`off`).
        #[arg(long, value_name = "on|off")]
        autostart: Option<Toggle>,
    },
    /// Serve screen history to an AI assistant over MCP (stdio). Off until `--enable`.
    ///
    /// Configure your MCP client to run `rsrewind mcp`. What tools may return is set under [mcp]
    /// in config.toml; see docs/mcp.md.
    Mcp {
        /// Turn agent access on (prints what is shared, then exits).
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        /// Turn agent access off (then exits).
        #[arg(long)]
        disable: bool,
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
        Command::Gaps {
            since,
            until,
            min_seconds,
            json,
        } => gaps(&data, &since, until.as_deref(), min_seconds, json),
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
        Command::Ui {
            foreground,
            appearance,
        } => ui(&data, foreground, appearance),
        Command::Tray {
            foreground,
            start_recorder,
            autostart,
        } => tray(&data, foreground, start_recorder, autostart),
        Command::Mcp { enable, disable } => mcp(&data, enable, disable),
        Command::DataDir => {
            println!("{}", data.root().display());
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn load_config(data: &DataDir) -> Result<Config> {
    Config::write_default_if_missing(&data.config_file())?;
    Ok(Config::load_or_default(&data.config_file())?)
}

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

#[cfg(any(windows, target_os = "linux"))]
fn daemon(data: &DataDir) -> Result<ExitCode> {
    data.ensure()?;
    let config = load_config(data)?;
    let _guard = init_logging(data, &config, true, "rsrewind")?;
    #[cfg(target_os = "linux")]
    authorize_screenshots()?;
    rsrewind_daemon::run(rsrewind_daemon::RunOptions {
        data: data.clone(),
        config,
    })?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(any(windows, target_os = "linux"))]
fn start(data: &DataDir) -> Result<ExitCode> {
    if let Some(status) = live_status(data)? {
        println!("rsRewind is already recording (pid {}).", status.pid);
        return Ok(ExitCode::SUCCESS);
    }
    data.ensure()?;
    load_config(data)?;
    let exe = std::env::current_exe().context("locate the rsrewind executable")?;
    let mut command = std::process::Command::new(exe);
    command
        .arg("--data-dir")
        .arg(data.root())
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut command);
    let child = command.spawn().context("start the recorder")?;
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

#[cfg(any(windows, target_os = "linux"))]
fn stop(data: &DataDir) -> Result<ExitCode> {
    if !signal_stop(data)? {
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

#[cfg(windows)]
fn signal_stop(_: &DataDir) -> Result<bool> {
    Ok(rsrewind_daemon::win::signal_stop()?)
}

/// SIGTERM to the pid in a live heartbeat. The recorder finishes its tick, flushes and exits.
#[cfg(target_os = "linux")]
fn signal_stop(data: &DataDir) -> Result<bool> {
    let Some(status) = live_status(data)? else {
        return Ok(false);
    };
    let sent = std::process::Command::new("kill")
        .args(["-TERM", &status.pid.to_string()])
        .status()
        .context("run kill")?;
    Ok(sent.success())
}

/// KWin answers screenshot requests only for executables named in a `.desktop` file that lists
/// the ScreenShot2 interface. Written (or rewritten, if the executable moved) on every recorder
/// start, so the grant always names exactly the binary that is running; `rsrewind status` shows
/// that recording is on, as always.
#[cfg(target_os = "linux")]
fn authorize_screenshots() -> Result<()> {
    let exe = std::env::current_exe().context("locate the rsrewind executable")?;
    let apps = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .context("HOME is not set")?
        .join("applications");
    let path = apps.join("rsrewind-recorder.desktop");
    let entry = rsrewind_capture::kwin::desktop_entry(&exe);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(entry.as_str()) {
        std::fs::create_dir_all(&apps).with_context(|| format!("create {}", apps.display()))?;
        std::fs::write(&path, entry).with_context(|| format!("write {}", path.display()))?;
        tracing::info!(path = %path.display(), "installed the KWin screenshot authorization");
        // KWin reads application entries lazily; give it a moment before the first capture.
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Ok(())
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
    let history = open_history(data)?;
    let hits = history.search(&query, SourceFilter::All)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&hits)?);
    } else {
        print!("{}", render::search_hits(&hits, &source_labels(&history)?));
    }
    Ok(if hits.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn recent(data: &DataDir, limit: u32, json: bool) -> Result<ExitCode> {
    let history = open_history(data)?;
    let entries = history.recent(SourceFilter::All, limit, None)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        // Gaps between the oldest moment shown and now, so a jump in time is never silent.
        let gaps = match entries.last() {
            Some(oldest) => history.gaps(
                SourceFilter::All,
                oldest.started_at,
                Timestamp::now(),
                DISPLAY_MIN_GAP_MS,
            )?,
            None => Vec::new(),
        };
        print!(
            "{}",
            render::timeline_with_gaps(&entries, &gaps, &source_labels(&history)?)
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Gaps shorter than this are not worth a line in human output.
const DISPLAY_MIN_GAP_MS: i64 = 60_000;

fn gaps(
    data: &DataDir,
    since: &str,
    until: Option<&str>,
    min_seconds: u32,
    json: bool,
) -> Result<ExitCode> {
    let now = chrono::Local::now();
    let from = timespec::parse_timespec(since, now).map_err(anyhow::Error::msg)?;
    let to = match until {
        Some(until) => timespec::parse_timespec(until, now).map_err(anyhow::Error::msg)?,
        None => Timestamp::now(),
    };
    let history = open_history(data)?;
    let gaps = history.gaps(SourceFilter::All, from, to, i64::from(min_seconds) * 1_000)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&gaps)?);
    } else {
        print!("{}", render::gaps(&gaps, &source_labels(&history)?));
    }
    Ok(ExitCode::SUCCESS)
}

/// This machine's history and every imported source, read together. A store that exists but
/// cannot be used is reported on stderr and skipped, never attributed to another source.
fn open_history(data: &DataDir) -> Result<History> {
    let history = History::open(data);
    for problem in history.problems() {
        eprintln!(
            "rsrewind: skipped {}: {}",
            problem.data_dir.display(),
            render::plain(&problem.reason)
        );
    }
    if history.sources()?.is_empty() {
        bail!(
            "no rsRewind history in {} yet (start recording with `rsrewind start`, or bring in another machine's with `rsrewind import`)",
            data.root().display()
        );
    }
    Ok(history)
}

fn source_labels(history: &History) -> Result<render::SourceLabels> {
    Ok(history
        .sources()?
        .into_iter()
        .filter_map(|info| Some((info.source?, info.label)))
        .collect())
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Toggle {
    On,
    Off,
}

fn tray(
    data: &DataDir,
    foreground: bool,
    start_recorder: bool,
    autostart: Option<Toggle>,
) -> Result<ExitCode> {
    let exe = std::env::current_exe().context("find the rsrewind executable")?;
    if let Some(toggle) = autostart {
        return set_autostart(&exe, data, toggle);
    }
    if !foreground {
        let mut command = std::process::Command::new(&exe);
        command
            .arg("--data-dir")
            .arg(data.root())
            .args(["tray", "--foreground"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if start_recorder {
            command.arg("--start-recorder");
        }
        detach(&mut command);
        let child = command.spawn().context("start the tray icon")?;
        println!("Started the tray icon (process {}).", child.id());
        return Ok(ExitCode::SUCCESS);
    }
    data.ensure()?;
    // One icon per data folder: a second `rsrewind tray` (login plus a manual start) just exits.
    let lock_path = data.root().join("tray.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open {}", lock_path.display()))?;
    if lock.try_lock().is_err() {
        println!("The rsRewind tray icon is already running.");
        return Ok(ExitCode::SUCCESS);
    }
    let config = Config::load_or_default(&data.config_file()).unwrap_or_default();
    let _guard = init_logging(data, &config, false, "rsrewind-tray")?;
    let runner = rsrewind_tray::Runner {
        exe,
        data_dir: data.root().to_path_buf(),
    };
    if start_recorder && live_status(data)?.is_none() {
        runner.perform(rsrewind_tray::model::Action::StartRecorder);
    }
    rsrewind_tray::run(&runner)?;
    drop(lock);
    Ok(ExitCode::SUCCESS)
}

/// Login startup for the icon (which also starts the recorder): an XDG autostart entry on Linux.
#[cfg(target_os = "linux")]
fn set_autostart(exe: &std::path::Path, data: &DataDir, toggle: Toggle) -> Result<ExitCode> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("HOME is not set")?
        .join("autostart");
    let path = dir.join("rsrewind-tray.desktop");
    match toggle {
        Toggle::On => {
            std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
            let entry = format!(
                "[Desktop Entry]\nType=Application\nName=rsRewind\nComment=Screen history: tray icon and recorder\nExec=\"{}\" --data-dir \"{}\" tray --foreground --start-recorder\nX-GNOME-Autostart-enabled=true\nNoDisplay=true\n",
                exe.display(),
                data.root().display()
            );
            std::fs::write(&path, entry).with_context(|| format!("write {}", path.display()))?;
            println!(
                "rsRewind will start (tray icon and recorder) when you log in: {}",
                path.display()
            );
        }
        Toggle::Off => match std::fs::remove_file(&path) {
            Ok(()) => println!(
                "Removed {}; rsRewind no longer starts at login.",
                path.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!("rsRewind was not set to start at login.")
            }
            Err(e) => return Err(e).with_context(|| format!("remove {}", path.display())),
        },
    }
    Ok(ExitCode::SUCCESS)
}

/// Login startup on Windows: a value under the per-user `Run` key, which starts the tray (and,
/// through it, the recorder) when you sign in.
#[cfg(windows)]
fn set_autostart(exe: &std::path::Path, data: &DataDir, toggle: Toggle) -> Result<ExitCode> {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
    };
    use windows::core::w;
    let key = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let name = w!("rsRewind");
    match toggle {
        Toggle::On => {
            let command = format!(
                "\"{}\" --data-dir \"{}\" tray --start-recorder",
                exe.display(),
                data.root().display()
            );
            let wide: Vec<u16> = command.encode_utf16().chain([0]).collect();
            // SAFETY: `wide` is a NUL-terminated UTF-16 string whose byte length is passed with
            // it; the key and value names are 'static literals.
            let result = unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    key,
                    name,
                    REG_SZ.0,
                    Some(wide.as_ptr().cast()),
                    (wide.len() * 2) as u32,
                )
            };
            if result != ERROR_SUCCESS {
                bail!("could not set the login entry (error {})", result.0);
            }
            println!("rsRewind will start (tray icon and recorder) when you sign in.");
        }
        Toggle::Off => {
            // SAFETY: 'static key and value names.
            let result = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key, name) };
            if result == ERROR_SUCCESS {
                println!("rsRewind no longer starts when you sign in.");
            } else if result == ERROR_FILE_NOT_FOUND {
                println!("rsRewind was not set to start when you sign in.");
            } else {
                bail!("could not remove the login entry (error {})", result.0);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn set_autostart(_: &std::path::Path, _: &DataDir, _: Toggle) -> Result<ExitCode> {
    bail!("starting at login is set up on Linux and Windows so far")
}

/// `rsrewind mcp`: the stdio server, or `--enable` / `--disable` of `[mcp] enabled`.
/// While serving, stdout carries only protocol messages; logs go to the data folder's log files.
fn mcp(data: &DataDir, enable: bool, disable: bool) -> Result<ExitCode> {
    if enable || disable {
        data.ensure()?;
        Config::write_default_if_missing(&data.config_file())?;
        set_mcp_enabled(&data.config_file(), enable)?;
        if enable {
            println!(
                "rsRewind agent access is ON.\n\n\
                 AI assistants you connect (`rsrewind mcp` in their MCP settings) can now search your screen\n\
                 history and read its recognized text, for the last 30 days by default. Most assistants send\n\
                 what they read to their provider's servers: that is the one way rsRewind history leaves this\n\
                 computer. Screenshots stay off unless you set allow_screenshots = true under [mcp] in\n\
                 {}.\nAgents cannot resume recording or delete anything. Turn this off with `rsrewind mcp --disable`.",
                data.config_file().display()
            );
        } else {
            println!("rsRewind agent access is OFF. Connected assistants see only a status tool.");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let config = Config::load_or_default(&data.config_file())?;
    let _guard = init_logging(data, &config, false, "rsrewind-mcp")?;
    let server = rsrewind_mcp::McpServer {
        data: data.clone(),
        config: config.mcp,
        exe: std::env::current_exe().ok(),
        version: env!("CARGO_PKG_VERSION"),
    };
    let stdin = std::io::stdin();
    rsrewind_mcp::serve(&server, stdin.lock(), std::io::stdout().lock())?;
    Ok(ExitCode::SUCCESS)
}

/// Sets `enabled` under `[mcp]` in config.toml, keeping the rest of the file (and its comments).
fn set_mcp_enabled(path: &std::path::Path, on: bool) -> Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let value = if on { "true" } else { "false" };
    let mut out = Vec::new();
    let (mut in_mcp, mut seen_section, mut done) = (false, false, false);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_mcp && !done {
                out.push(format!("enabled = {value}"));
                done = true;
            }
            in_mcp = trimmed == "[mcp]";
            seen_section |= in_mcp;
        } else if in_mcp
            && trimmed.starts_with("enabled")
            && trimmed[7..].trim_start().starts_with('=')
        {
            out.push(format!("enabled = {value}"));
            done = true;
            continue;
        }
        out.push(line.to_string());
    }
    if in_mcp && !done {
        out.push(format!("enabled = {value}"));
        done = true;
    }
    if !seen_section {
        out.push(String::new());
        out.push("[mcp]".into());
        out.push(format!("enabled = {value}"));
        done = true;
    }
    debug_assert!(done);
    let new = out.join("\n") + "\n";
    // Refuse to write a file the recorder could not read back.
    Config::parse(&new)
        .map_err(|e| anyhow::anyhow!("config.toml would not parse after the change: {e}"))?;
    std::fs::write(path, new).with_context(|| format!("write {}", path.display()))
}

/// `rsrewind ui --appearance`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Appearance {
    System,
    Light,
    Dark,
}

fn ui(data: &DataDir, foreground: bool, appearance: Appearance) -> Result<ExitCode> {
    let appearance_arg = match appearance {
        Appearance::System => "system",
        Appearance::Light => "light",
        Appearance::Dark => "dark",
    };
    if foreground {
        // Read the config without creating one: the viewer writes nothing to the data folder
        // except its own log.
        let config = Config::load_or_default(&data.config_file()).unwrap_or_default();
        let _guard = init_logging(data, &config, false, "rsrewind-ui")?;
        let appearance = match appearance {
            Appearance::System => rsrewind_ui::Appearance::System,
            Appearance::Light => rsrewind_ui::Appearance::Light,
            Appearance::Dark => rsrewind_ui::Appearance::Dark,
        };
        rsrewind_ui::run_with(data.clone(), appearance)?;
        return Ok(ExitCode::SUCCESS);
    }
    let exe = std::env::current_exe().context("find the rsrewind executable")?;
    let mut command = std::process::Command::new(exe);
    command
        .arg("--data-dir")
        .arg(data.root())
        .args(["ui", "--foreground", "--appearance", appearance_arg])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut command);
    let child = command.spawn().context("start the rsRewind window")?;
    println!("Opened the rsRewind window (process {}).", child.id());
    Ok(ExitCode::SUCCESS)
}

/// No console, and out of the starting terminal's process group, so closing that terminal or
/// pressing Ctrl+C in it does not close the window.
#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(any(windows, target_os = "linux")))]
fn daemon(_: &DataDir) -> Result<ExitCode> {
    bail!("the recorder runs on Windows and KDE Plasma (Wayland) only")
}
#[cfg(not(any(windows, target_os = "linux")))]
fn start(_: &DataDir) -> Result<ExitCode> {
    bail!("the recorder runs on Windows and KDE Plasma (Wayland) only")
}
#[cfg(not(any(windows, target_os = "linux")))]
fn stop(_: &DataDir) -> Result<ExitCode> {
    bail!("the recorder runs on Windows and KDE Plasma (Wayland) only")
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

    /// Every subcommand is either answered by an MCP tool or deliberately kept from agents. A new
    /// subcommand fails this test until it is classified (AnchorDesk's parity invariant).
    #[test]
    fn every_command_is_classified_for_agents() {
        let for_agents = [
            ("search", "search"),
            ("recent", "recent"),
            ("gaps", "list_gaps"),
            ("sources", "list_sources"),
            ("status", "get_status"),
            ("pause", "pause_recording"),
        ];
        // Recording control, deletion, moving history, setup and the surfaces themselves stay with the person.
        let not_for_agents = [
            "daemon", "start", "stop", "resume", "forget", "doctor", "export", "import", "ui",
            "tray", "mcp", "data-dir", "help",
        ];
        let tools = rsrewind_mcp::tool_names();
        for (command, tool) in for_agents {
            assert!(
                tools.contains(&tool),
                "{command} maps to missing tool {tool}"
            );
        }
        for sub in Cli::command().get_subcommands() {
            let name = sub.get_name();
            assert!(
                for_agents.iter().any(|(c, _)| *c == name) || not_for_agents.contains(&name),
                "classify `{name}`: give it an MCP tool or add it to not_for_agents"
            );
        }
    }

    #[test]
    fn enabling_mcp_edits_config_toml_in_place() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config.toml");
        // No [mcp] section: one is appended; comments and other settings survive.
        std::fs::write(&path, "# mine\n[capture]\nidle_after_secs = 120\n")?;
        set_mcp_enabled(&path, true)?;
        let text = std::fs::read_to_string(&path)?;
        assert!(
            text.contains("# mine") && text.contains("idle_after_secs = 120"),
            "{text}"
        );
        assert!(Config::parse(&text)?.mcp.enabled);
        // Existing section with a value: flipped, not duplicated.
        set_mcp_enabled(&path, false)?;
        let text = std::fs::read_to_string(&path)?;
        assert_eq!(text.matches("enabled =").count(), 1, "{text}");
        assert!(!Config::parse(&text)?.mcp.enabled);
        // Section without the key, followed by another section.
        std::fs::write(
            &path,
            "[mcp]\nmax_age_days = 7\n\n[capture]\nidle_after_secs = 60\n",
        )?;
        set_mcp_enabled(&path, true)?;
        let config = Config::parse(&std::fs::read_to_string(&path)?)?;
        assert!(
            config.mcp.enabled
                && config.mcp.max_age_days == 7
                && config.capture.idle_after_secs == 60
        );
        // A file that would not parse is refused and left alone.
        std::fs::write(&path, "[mcp]\nsources = [\"not-an-id\"]\n")?;
        assert!(set_mcp_enabled(&path, true).is_err());
        assert!(std::fs::read_to_string(&path)?.contains("not-an-id"));
        Ok(())
    }

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
