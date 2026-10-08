use crate::media::{self, resolve_media_path};
use crate::migrations::{self, MIGRATIONS, Migration, MigrationOutcome};
use crate::{Result, StorageError};
use rsrewind_core::{
    ApplicationContext, ApplicationId, CaptureState, DataDir, EventId, EventKind, MonitorId,
    MonitorInfo, OcrBlock, SessionCapabilities, SessionId, Timestamp, VisualStateId, WindowContext,
    WindowId,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);

/// A visual state whose image file the caller has already written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewVisualState {
    pub monitor: MonitorId,
    pub captured_at: Timestamp,
    /// Relative to the data root, forward slashes (see `rsrewind_core::paths::media_relative_path`).
    pub media_path: String,
    pub width: u32,
    pub height: u32,
    pub byte_size: u64,
    pub fingerprint: Option<u64>,
    /// `false` stores the state as `skipped` so the OCR worker never picks it up.
    pub ocr_enabled: bool,
}

/// "Monitor M showed visual state V while A/W had focus, as of `at`."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub session: SessionId,
    pub monitor: MonitorId,
    pub visual_state: VisualStateId,
    pub application: Option<ApplicationId>,
    pub window: Option<WindowId>,
    pub at: Timestamp,
    /// The previous observation is extended only if it ended at most this long before `at`.
    pub max_gap_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOcr {
    pub id: VisualStateId,
    /// The stored relative path. Hand it back to `save_ocr`/`mark_ocr`: a result is accepted only
    /// if the row still names this exact file.
    pub relative_path: String,
    /// Absolute path of the image.
    pub media_path: PathBuf,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OcrStatus {
    Pending,
    Done,
    Failed,
    Skipped,
}

impl OcrStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "skipped" => Self::Skipped,
            _ => return None,
        })
    }
}

/// The recorder's heartbeat row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecorderStatus {
    pub pid: u32,
    pub started_at: Timestamp,
    pub heartbeat_at: Timestamp,
    pub state: CaptureState,
    pub counters: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteReport {
    pub events_deleted: u64,
    pub visual_states_deleted: u64,
    pub files_deleted: u64,
    /// Rows whose file was already gone.
    pub files_missing: u64,
    /// Sum of `byte_size` of the deleted visual states.
    pub bytes_freed: u64,
    /// Files that could not be deleted (path: error). Reported, never fatal.
    pub file_errors: Vec<String>,
    /// Image files that had no database row (a crash between write and insert) and were swept.
    pub orphan_files_deleted: u64,
    /// `false` if the WAL could not be truncated (another connection was mid-read), so deleted
    /// pages may still sit in `recall.db-wal` until the next checkpoint.
    pub wal_truncated: bool,
    /// Pre-migration database backups still on disk. They hold history as it was when they were
    /// taken and are never rewritten; only `forget` (an explicit user action) reports them.
    pub backups_surviving: Vec<String>,
}

impl DeleteReport {
    fn absorb(&mut self, other: Self) {
        self.events_deleted += other.events_deleted;
        self.visual_states_deleted += other.visual_states_deleted;
        self.files_deleted += other.files_deleted;
        self.files_missing += other.files_missing;
        self.bytes_freed += other.bytes_freed;
        self.file_errors.extend(other.file_errors);
        self.orphan_files_deleted += other.orphan_files_deleted;
        self.wal_truncated = other.wal_truncated;
        self.backups_surviving.extend(other.backups_surviving);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageStats {
    pub sessions: u64,
    pub events: u64,
    pub observations: u64,
    pub visual_states: u64,
    pub ocr_blocks: u64,
    pub pending_ocr: u64,
    pub failed_ocr: u64,
    /// `recall.db` plus its `-wal` file on disk.
    pub db_bytes: u64,
    /// Sum of stored image sizes.
    pub media_bytes: u64,
    pub oldest: Option<Timestamp>,
    pub newest: Option<Timestamp>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanReport {
    /// Visual states whose image file is missing (id, stored relative path).
    pub rows_missing_file: Vec<(VisualStateId, String)>,
    /// Visual states whose stored path is not a safe relative path.
    pub rows_invalid_path: Vec<VisualStateId>,
    /// Files under `media/` that no visual state points at (including stray temp files).
    pub files_without_row: Vec<PathBuf>,
}

/// Which events a deletion pass targets. Each variant also bounds which *unreferenced* visual
/// states (e.g. left by a crash between file write and observation) are swept with them.
#[derive(Debug, Clone, Copy)]
enum Selection {
    /// Events overlapping `[since, until)`; unreferenced states captured in `[since, until)`.
    Overlapping { since: i64, until: i64 },
    /// Events that ended before the cutoff; unreferenced states captured before it.
    EndedBefore(i64),
    /// Events that started at or before the cutoff; unreferenced states captured at or before it.
    StartedAtOrBefore(i64),
}

impl Selection {
    fn event_clause(self) -> (&'static str, Vec<i64>) {
        match self {
            Self::Overlapping { since, until } => {
                ("started_at < ?1 AND ended_at >= ?2", vec![until, since])
            }
            Self::EndedBefore(cutoff) => ("ended_at < ?1", vec![cutoff]),
            Self::StartedAtOrBefore(cutoff) => ("started_at <= ?1", vec![cutoff]),
        }
    }

    /// Whether a file named for capture time `ts` and without a row belongs to this pass.
    fn includes_capture(self, ts: i64) -> bool {
        match self {
            Self::Overlapping { since, until } => ts >= since && ts < until,
            Self::EndedBefore(cutoff) => ts < cutoff,
            Self::StartedAtOrBefore(cutoff) => ts <= cutoff,
        }
    }

    fn orphan_clause(self) -> (&'static str, Vec<i64>) {
        match self {
            Self::Overlapping { since, until } => {
                ("captured_at >= ?1 AND captured_at < ?2", vec![since, until])
            }
            Self::EndedBefore(cutoff) => ("captured_at < ?1", vec![cutoff]),
            Self::StartedAtOrBefore(cutoff) => ("captured_at <= ?1", vec![cutoff]),
        }
    }
}

/// One read-write SQLite connection to `recall.db`.
pub struct Store {
    pub(crate) conn: Connection,
    data: DataDir,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("data", &self.data).finish()
    }
}

impl Store {
    /// Creates the data directory and database if needed and migrates to the current schema.
    pub fn open(data: &DataDir) -> Result<Self> {
        data.ensure()?;
        Self::open_with_migrations(data, MIGRATIONS, true).map(|(store, _)| store)
    }

    /// Opens an existing database (migrating it if this build is newer). Never creates one, so a
    /// `status` command cannot conjure an empty history by accident.
    pub fn open_existing(data: &DataDir) -> Result<Self> {
        let path = data.database();
        if !path.is_file() {
            return Err(StorageError::DatabaseMissing(path));
        }
        Self::open_with_migrations(data, MIGRATIONS, false).map(|(store, _)| store)
    }

    /// Opens with an explicit migration list. Production code passes `MIGRATIONS`; tests inject
    /// extra versions to exercise the backup-before-migrate path.
    pub(crate) fn open_with_migrations(
        data: &DataDir,
        migrations: &[Migration],
        create: bool,
    ) -> Result<(Self, MigrationOutcome)> {
        let path = data.database();
        let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        if create {
            flags |= OpenFlags::SQLITE_OPEN_CREATE;
        }
        let conn = Connection::open_with_flags(&path, flags)?;
        configure_write_connection(&conn)?;
        let outcome = migrations::migrate(&conn, data, migrations)?;
        if outcome.from != outcome.to {
            tracing::info!(
                from = outcome.from,
                to = outcome.to,
                "database schema migrated"
            );
        }
        Ok((
            Self {
                conn,
                data: data.clone(),
            },
            outcome,
        ))
    }

    pub fn data_dir(&self) -> &DataDir {
        &self.data
    }

    pub fn schema_version(&self) -> Result<u32> {
        migrations::current_version(&self.conn)
    }

    pub(crate) fn immediate(&self) -> Result<Transaction<'_>> {
        Ok(Transaction::new_unchecked(
            &self.conn,
            TransactionBehavior::Immediate,
        )?)
    }

    // ----- sessions, monitors, applications, windows -------------------------------------------

    pub fn begin_session(
        &self,
        started_at: Timestamp,
        hostname: &str,
        app_version: &str,
    ) -> Result<SessionId> {
        self.conn.execute(
            "INSERT INTO sessions (started_at, hostname, app_version) VALUES (?1, ?2, ?3)",
            params![started_at.0, hostname, app_version],
        )?;
        Ok(SessionId(self.conn.last_insert_rowid()))
    }

    /// Records what the platform could see during `session` (stored in `settings`, keyed by the
    /// session id; no schema change). Read back by `status`/`doctor` and by export, which never
    /// claims a capability any exported session lacked.
    pub fn record_session_capabilities(
        &self,
        session: SessionId,
        capabilities: &SessionCapabilities,
    ) -> Result<()> {
        let value = serde_json::to_string(capabilities)?;
        Self::set_setting(&self.conn, &session_capabilities_key(session), &value)
    }

    pub fn session_capabilities(&self, session: SessionId) -> Result<Option<SessionCapabilities>> {
        let key = session_capabilities_key(session);
        match self.setting(&key)? {
            None => Ok(None),
            Some(value) => serde_json::from_str(&value)
                .map(Some)
                .map_err(|_| StorageError::Corrupt(format!("setting {key}"))),
        }
    }

    /// The newest recording session, with its capabilities if it recorded any. A store whose
    /// newest session ran with privacy rules unenforced keeps saying so (`status`, `doctor`)
    /// until a session that enforces them starts.
    pub fn latest_session_capabilities(&self) -> Result<Option<(SessionId, SessionCapabilities)>> {
        let latest: Option<i64> =
            self.conn
                .query_row("SELECT MAX(id) FROM sessions", [], |row| row.get(0))?;
        let Some(latest) = latest.map(SessionId) else {
            return Ok(None);
        };
        Ok(self
            .session_capabilities(latest)?
            .map(|capabilities| (latest, capabilities)))
    }

    pub fn end_session(&self, session: SessionId, ended_at: Timestamp) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET ended_at = ?2 WHERE id = ?1",
            params![session.0, ended_at.0],
        )?;
        Ok(())
    }

    pub fn upsert_monitor(&self, monitor: &MonitorInfo, seen_at: Timestamp) -> Result<MonitorId> {
        let id = self.conn.query_row(
            "INSERT INTO monitors
                 (device_name, width, height, \"left\", \"top\", dpi, primary_monitor, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (device_name) DO UPDATE SET
                 width = excluded.width, height = excluded.height, \"left\" = excluded.\"left\",
                 \"top\" = excluded.\"top\", dpi = excluded.dpi,
                 primary_monitor = excluded.primary_monitor, last_seen_at = excluded.last_seen_at
             RETURNING id",
            params![
                monitor.device_name,
                monitor.width,
                monitor.height,
                monitor.left,
                monitor.top,
                monitor.dpi,
                monitor.primary,
                seen_at.0
            ],
            |row| row.get(0),
        )?;
        Ok(MonitorId(id))
    }

    pub fn upsert_application(
        &self,
        application: &ApplicationContext,
        at: Timestamp,
    ) -> Result<ApplicationId> {
        // process_name is UNIQUE COLLATE NOCASE: `teams.exe` and `Teams.exe` are one application.
        // A later sighting with an exe path fills in one we could not read before.
        let id = self.conn.query_row(
            "INSERT INTO applications (process_name, exe_path, first_seen_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (process_name) DO UPDATE SET
                 exe_path = COALESCE(excluded.exe_path, applications.exe_path)
             RETURNING id",
            params![application.process_name, application.exe_path, at.0],
            |row| row.get(0),
        )?;
        Ok(ApplicationId(id))
    }

    pub fn upsert_window(
        &self,
        application: ApplicationId,
        window: &WindowContext,
        at: Timestamp,
    ) -> Result<WindowId> {
        let tx = self.immediate()?;
        let id = upsert_window_row(&tx, application, window, at)?;
        tx.commit()?;
        Ok(WindowId(id))
    }

    // ----- visual states and events ------------------------------------------------------------

    /// Inserts the row for an image the caller has already written with
    /// [`crate::media::write_webp_exclusive`]. Refuses unsafe paths and missing files, so the
    /// database never points at something that is not there.
    pub fn insert_visual_state(&self, state: &NewVisualState) -> Result<VisualStateId> {
        let absolute = resolve_media_path(&self.data, &state.media_path)?;
        if !absolute.is_file() {
            return Err(StorageError::InvalidArgument(format!(
                "media file for {} was not written before inserting its row",
                state.media_path
            )));
        }
        let tx = self.immediate()?;
        let id = insert_state_row(&tx, state)?;
        tx.commit()?;
        Ok(id)
    }

    /// Extends the session's latest observation on this monitor if nothing changed and the gap is
    /// small enough, otherwise starts a new observation event.
    pub fn record_observation(&self, observation: &Observation) -> Result<EventId> {
        if observation.max_gap_ms < 0 {
            return Err(StorageError::InvalidArgument(
                "max_gap_ms must be >= 0".into(),
            ));
        }
        let at = observation.at.0;
        let tx = self.immediate()?;
        refuse_if_fenced(&tx, at)?;
        // A concurrent delete_range / retention may have removed the state the recorder is still
        // holding on to; a typed error lets it re-persist the frame instead of failing on an FK.
        let visual_exists: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM visual_states WHERE id = ?1)",
            [observation.visual_state.0],
            |row| row.get(0),
        )?;
        if !visual_exists {
            return Err(StorageError::VisualStateMissing(observation.visual_state));
        }
        // Retention / delete_range remove windows and applications nothing references any more; a
        // writer that cached one of those ids is told to look it up again.
        if let Some(application) = observation.application {
            let exists: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM applications WHERE id = ?1)",
                [application.0],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(StorageError::ContextMissing);
            }
        }
        if let Some(window) = observation.window {
            let exists: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM windows WHERE id = ?1)",
                [window.0],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(StorageError::ContextMissing);
            }
        }
        let last = tx
            .query_row(
                "SELECT id, visual_state_id, application_id, window_id, ended_at FROM events
                 WHERE session_id = ?1 AND monitor_id = ?2 AND kind = ?3
                 ORDER BY id DESC LIMIT 1",
                params![
                    observation.session.0,
                    observation.monitor.0,
                    EventKind::Observation.as_str()
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;

        let extend = last.and_then(|(id, visual, app, window, ended_at)| {
            let same = visual == Some(observation.visual_state.0)
                && app == observation.application.map(|a| a.0)
                && window == observation.window.map(|w| w.0);
            (same && at.saturating_sub(ended_at) <= observation.max_gap_ms).then_some(id)
        });

        let id = match extend {
            Some(id) => {
                // MAX: a clock stepping backwards must not shrink a span.
                tx.execute(
                    "UPDATE events SET ended_at = MAX(ended_at, ?2) WHERE id = ?1",
                    params![id, at],
                )?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO events
                         (session_id, kind, started_at, ended_at, monitor_id, visual_state_id,
                          application_id, window_id)
                     VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        observation.session.0,
                        EventKind::Observation.as_str(),
                        at,
                        observation.monitor.0,
                        observation.visual_state.0,
                        observation.application.map(|a| a.0),
                        observation.window.map(|w| w.0)
                    ],
                )?;
                tx.last_insert_rowid()
            }
        };
        tx.commit()?;
        Ok(EventId(id))
    }

    /// Records a point-in-time event (pause, resume, privacy skip, idle, recorder lifecycle).
    pub fn record_marker(
        &self,
        session: SessionId,
        kind: EventKind,
        at: Timestamp,
        metadata: Option<serde_json::Value>,
    ) -> Result<EventId> {
        if kind == EventKind::Observation {
            return Err(StorageError::InvalidArgument(
                "observations are recorded with record_observation".into(),
            ));
        }
        let metadata = metadata.map(|m| m.to_string());
        self.conn.execute(
            "INSERT INTO events (session_id, kind, started_at, ended_at, metadata_json)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            params![session.0, kind.as_str(), at.0, metadata],
        )?;
        Ok(EventId(self.conn.last_insert_rowid()))
    }

    // ----- OCR ---------------------------------------------------------------------------------

    /// Oldest pending visual states first. A row with an unsafe stored path is marked failed and
    /// skipped so the worker cannot be pointed outside the data directory or loop on it forever.
    pub fn next_pending_ocr(&self, limit: u32) -> Result<Vec<PendingOcr>> {
        let rows: Vec<(i64, String, u32, u32)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, media_path, width, height FROM visual_states
                 WHERE ocr_status = 'pending' ORDER BY captured_at, id LIMIT ?1",
            )?;
            stmt.query_map([limit], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        let mut pending = Vec::with_capacity(rows.len());
        for (id, relative, width, height) in rows {
            match resolve_media_path(&self.data, &relative) {
                Ok(media_path) => pending.push(PendingOcr {
                    id: VisualStateId(id),
                    relative_path: relative,
                    media_path,
                    width,
                    height,
                }),
                Err(_) => {
                    tracing::warn!(visual_state = id, "unsafe media path; marking OCR failed");
                    self.mark_ocr(
                        VisualStateId(id),
                        &relative,
                        OcrStatus::Failed,
                        Some("unsafe media path"),
                    )?;
                }
            }
        }
        Ok(pending)
    }

    /// Replaces the OCR result of one visual state atomically: blocks, FTS row and status.
    ///
    /// `expected_media_path` is the path the OCR job was started from. If the row has been deleted,
    /// or its id now names a different image, the result is discarded with
    /// [`StorageError::VisualStateMissing`]: OCR text must never attach to the wrong screenshot.
    pub fn save_ocr(
        &self,
        id: VisualStateId,
        expected_media_path: &str,
        blocks: &[OcrBlock],
        engine: &str,
        elapsed_ms: u64,
    ) -> Result<()> {
        let tx = self.immediate()?;
        let current: Option<String> = tx
            .query_row(
                "SELECT media_path FROM visual_states WHERE id = ?1",
                [id.0],
                |row| row.get(0),
            )
            .optional()?;
        if current.as_deref() != Some(expected_media_path) {
            // Usually: retention or delete_range removed it while OCR was running.
            return Err(StorageError::VisualStateMissing(id));
        }
        write_ocr_rows(&tx, id, blocks, engine, elapsed_ms)?;
        tx.commit()?;
        tracing::debug!(visual_state = id.0, lines = blocks.len(), "saved OCR");
        Ok(())
    }

    /// Like [`Store::save_ocr`], the update applies only if the row still names `expected_media_path`.
    pub fn mark_ocr(
        &self,
        id: VisualStateId,
        expected_media_path: &str,
        status: OcrStatus,
        error: Option<&str>,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE visual_states SET ocr_status = ?2, ocr_error = ?3
             WHERE id = ?1 AND media_path = ?4",
            params![id.0, status.as_str(), error, expected_media_path],
        )?;
        if changed == 0 {
            return Err(StorageError::VisualStateMissing(id));
        }
        Ok(())
    }

    // ----- control and status ------------------------------------------------------------------

    /// The requested capture state. A missing row reads as recording.
    pub fn read_control(&self) -> Result<CaptureState> {
        let row = self
            .conn
            .query_row(
                "SELECT state, paused_until FROM control WHERE id = 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()?;
        match row {
            Some((state, until)) => state_from_parts(&state, until),
            None => Ok(CaptureState::Recording),
        }
    }

    pub fn set_control(&self, state: CaptureState) -> Result<()> {
        let (name, until) = state_to_parts(state);
        self.conn.execute(
            "INSERT INTO control (id, state, paused_until, updated_at) VALUES (1, ?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET
                 state = excluded.state, paused_until = excluded.paused_until,
                 updated_at = excluded.updated_at",
            params![name, until, Timestamp::now().0],
        )?;
        Ok(())
    }

    pub fn write_status(&self, status: &RecorderStatus) -> Result<()> {
        let (name, until) = state_to_parts(status.state);
        let counters = serde_json::to_string(&status.counters)?;
        self.conn.execute(
            "INSERT INTO recorder_status
                 (id, pid, started_at, heartbeat_at, state, paused_until, counters_json)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET
                 pid = excluded.pid, started_at = excluded.started_at,
                 heartbeat_at = excluded.heartbeat_at, state = excluded.state,
                 paused_until = excluded.paused_until, counters_json = excluded.counters_json",
            params![
                status.pid,
                status.started_at.0,
                status.heartbeat_at.0,
                name,
                until,
                counters
            ],
        )?;
        Ok(())
    }

    pub fn read_status(&self) -> Result<Option<RecorderStatus>> {
        let row = self
            .conn
            .query_row(
                "SELECT pid, started_at, heartbeat_at, state, paused_until, counters_json
                 FROM recorder_status WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, u32>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((pid, started_at, heartbeat_at, state, until, counters)) = row else {
            return Ok(None);
        };
        Ok(Some(RecorderStatus {
            pid,
            started_at: Timestamp(started_at),
            heartbeat_at: Timestamp(heartbeat_at),
            state: state_from_parts(&state, until)?,
            counters: serde_json::from_str(&counters)?,
        }))
    }

    // ----- deletion and retention --------------------------------------------------------------

    /// Deletes every event overlapping `[since, until)`, then the visual states no longer referenced
    /// by any event (plus unreferenced states captured in the range), their OCR blocks, FTS rows
    /// and image files. Files are removed only after the transaction commits.
    pub fn delete_range(&self, since: Timestamp, until: Timestamp) -> Result<DeleteReport> {
        if until.0 <= since.0 {
            return Ok(DeleteReport::default());
        }
        let mut report = self.delete_selection(Selection::Overlapping {
            since: since.0,
            until: until.0,
        })?;
        report.backups_surviving = self.surviving_backups();
        Ok(report)
    }

    /// Pre-migration backups that still exist. Deleting history never rewrites them.
    pub fn surviving_backups(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.data.backups()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "db"))
            .map(|path| path.display().to_string())
            .collect();
        names.sort();
        names
    }

    /// Age then size retention, oldest first. `retention_days == 0` / `max_bytes == 0` disable
    /// the respective rule. Size is the real disk footprint: `recall.db`, its WAL and every file
    /// under `media/` (including orphans and temp files), so a cap is a cap on bytes on disk.
    pub fn apply_retention(
        &self,
        retention_days: u32,
        max_bytes: u64,
        now: Timestamp,
    ) -> Result<DeleteReport> {
        let mut report = DeleteReport::default();
        if retention_days > 0 {
            let cutoff = now.saturating_sub_millis(i64::from(retention_days) * 86_400_000);
            report.absorb(self.delete_selection(Selection::EndedBefore(cutoff.0))?);
            // Nothing older than the horizon can still be in flight, so its fences are spent.
            self.conn
                .execute("DELETE FROM deletion_fences WHERE until < ?1", [cutoff.0])?;
        }
        if max_bytes > 0 {
            report.absorb(self.enforce_size(max_bytes, now)?);
        }
        if report.events_deleted > 0 || report.visual_states_deleted > 0 {
            tracing::info!(
                events = report.events_deleted,
                visual_states = report.visual_states_deleted,
                bytes = report.bytes_freed,
                file_errors = report.file_errors.len(),
                "retention pruned history"
            );
        }
        Ok(report)
    }

    fn enforce_size(&self, max_bytes: u64, now: Timestamp) -> Result<DeleteReport> {
        let mut report = DeleteReport::default();
        loop {
            let mut total = self.disk_bytes()?;
            if total <= max_bytes {
                break;
            }
            // Deleted rows leave free pages inside a file that never shrinks by itself; give that
            // space back before deciding more history has to go.
            if self.reclaim_db_space()? {
                total = self.disk_bytes()?;
                if total <= max_bytes {
                    break;
                }
            }
            let excess = total - max_bytes;
            // Walk the oldest events, counting each visual state's bytes once, until enough would
            // be freed (or a batch is exhausted); everything started up to there goes.
            let cutoff = {
                let mut stmt = self.conn.prepare(
                    "SELECT e.started_at, e.visual_state_id, COALESCE(v.byte_size, 0)
                     FROM events e LEFT JOIN visual_states v ON v.id = e.visual_state_id
                     ORDER BY e.started_at, e.id LIMIT 500",
                )?;
                let mut rows = stmt.query([])?;
                let mut seen = BTreeSet::new();
                let mut freed: u64 = 0;
                let mut cutoff = None;
                while let Some(row) = rows.next()? {
                    let started_at: i64 = row.get(0)?;
                    let visual: Option<i64> = row.get(1)?;
                    let size: i64 = row.get(2)?;
                    cutoff = Some(started_at);
                    if let Some(visual) = visual
                        && seen.insert(visual)
                    {
                        freed = freed.saturating_add(u64::try_from(size).unwrap_or(0));
                    }
                    if freed >= excess {
                        break;
                    }
                }
                cutoff
            };
            let pass = match cutoff {
                Some(cutoff) => self.delete_selection(Selection::StartedAtOrBefore(cutoff))?,
                // No events left: only unreferenced visual states can still be freed. Spare the
                // last minute, where a state may be waiting for its first observation.
                None => self.delete_selection(Selection::StartedAtOrBefore(
                    now.saturating_sub_millis(60_000).0,
                ))?,
            };
            let progressed = pass.events_deleted > 0 || pass.visual_states_deleted > 0;
            report.absorb(pass);
            if !progressed {
                // Nothing left that we may delete (e.g. the database itself exceeds the cap).
                break;
            }
        }
        Ok(report)
    }

    fn delete_selection(&self, selection: Selection) -> Result<DeleteReport> {
        let mut report = DeleteReport::default();
        let mut doomed_files: Vec<String> = Vec::new();
        let tx = self.immediate()?;
        if let Selection::Overlapping { since, until } = selection {
            // In the same transaction as the deletion: from the moment this commits, no writer can
            // put anything back into the interval, whatever it had queued.
            tx.execute(
                "INSERT INTO deletion_fences (since, until, created_at) VALUES (?1, ?2, ?3)",
                params![since, until, Timestamp::now().0],
            )?;
        }
        {
            let (event_where, event_params) = selection.event_clause();
            let mut candidates: BTreeSet<i64> = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT DISTINCT visual_state_id FROM events
                     WHERE visual_state_id IS NOT NULL AND {event_where}"
                ))?;
                stmt.query_map(rusqlite::params_from_iter(&event_params), |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            let deleted = tx.execute(
                &format!("DELETE FROM events WHERE {event_where}"),
                rusqlite::params_from_iter(&event_params),
            )?;
            report.events_deleted = deleted as u64;

            let (orphan_where, orphan_params) = selection.orphan_clause();
            let mut stmt = tx.prepare(&format!(
                "SELECT id FROM visual_states WHERE {orphan_where}
                 AND NOT EXISTS (SELECT 1 FROM events e WHERE e.visual_state_id = visual_states.id)"
            ))?;
            let orphans = stmt
                .query_map(rusqlite::params_from_iter(&orphan_params), |row| {
                    row.get::<_, i64>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            candidates.extend(orphans);

            let mut still_used =
                tx.prepare("SELECT EXISTS (SELECT 1 FROM events WHERE visual_state_id = ?1)")?;
            let mut details =
                tx.prepare("SELECT media_path, byte_size FROM visual_states WHERE id = ?1")?;
            let mut delete_fts = tx.prepare("DELETE FROM ocr_fts WHERE rowid = ?1")?;
            // ocr_blocks go with ON DELETE CASCADE (foreign_keys is on for every write connection).
            let mut delete_state = tx.prepare("DELETE FROM visual_states WHERE id = ?1")?;
            for id in candidates {
                // Shared visual states stay while any surviving event still shows them.
                if still_used.query_row([id], |row| row.get::<_, bool>(0))? {
                    continue;
                }
                let Some((path, size)) = details
                    .query_row([id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })
                    .optional()?
                else {
                    continue;
                };
                delete_fts.execute([id])?;
                delete_state.execute([id])?;
                report.visual_states_deleted += 1;
                report.bytes_freed += u64::try_from(size).unwrap_or(0);
                doomed_files.push(path);
            }
            drop((still_used, details, delete_fts, delete_state));
            // Titles and process names of history that no longer exists are history too.
            tx.execute(
                "DELETE FROM windows
                 WHERE NOT EXISTS (SELECT 1 FROM events e WHERE e.window_id = windows.id)",
                [],
            )?;
            tx.execute(
                "DELETE FROM applications
                 WHERE NOT EXISTS (SELECT 1 FROM events e WHERE e.application_id = applications.id)
                   AND NOT EXISTS (SELECT 1 FROM windows w WHERE w.application_id = applications.id)",
                [],
            )?;
        }
        tx.commit()?;

        for relative in doomed_files {
            let path = match resolve_media_path(&self.data, &relative) {
                Ok(path) => path,
                Err(error) => {
                    report.file_errors.push(format!("{relative}: {error}"));
                    continue;
                }
            };
            match std::fs::remove_file(&path) {
                Ok(()) => report.files_deleted += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => report.files_missing += 1,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "could not delete media");
                    report.file_errors.push(format!("{}: {e}", path.display()));
                }
            }
        }
        self.sweep_orphan_media(selection, &mut report);
        // Make the deletion physical: SQLite's page contents were zeroed by secure_delete, and the
        // WAL (which keeps earlier versions of pages) is truncated.
        report.wal_truncated = self.truncate_wal();
        Ok(report)
    }

    /// `PRAGMA wal_checkpoint(TRUNCATE)`; `false` if a concurrent reader prevented it.
    fn truncate_wal(&self) -> bool {
        match self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                row.get::<_, i64>(0)
            }) {
            Ok(busy) => busy == 0,
            Err(error) => {
                tracing::warn!(%error, "WAL checkpoint failed");
                false
            }
        }
    }

    /// Truncates the WAL and rebuilds the file if it holds free pages. `true` if it tried to
    /// shrink the database.
    fn reclaim_db_space(&self) -> Result<bool> {
        self.truncate_wal();
        let free: i64 = self
            .conn
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        if free == 0 {
            return Ok(false);
        }
        if let Err(error) = self.conn.execute_batch("VACUUM") {
            // Busy (another connection mid-transaction) or out of temp space: retention still
            // proceeds by deleting history; the next pass tries again.
            tracing::warn!(%error, "could not compact the database");
            return Ok(false);
        }
        self.truncate_wal();
        Ok(true)
    }

    /// Deletes image files under `media/` that no row points at but whose *name* (capture time and
    /// monitor) places them in this pass: leftovers of a crash between file write and row insert,
    /// or between a committed delete and its unlink. Stale temp files (older than a minute, so an
    /// in-flight write is never clobbered) go with them.
    fn sweep_orphan_media(&self, selection: Selection, report: &mut DeleteReport) {
        let Ok(mut lookup) = self
            .conn
            .prepare("SELECT EXISTS (SELECT 1 FROM visual_states WHERE media_path = ?1)")
        else {
            return;
        };
        let mut stack = vec![self.data.media_root()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if metadata.is_dir() {
                    // Never descend through a junction/symlink out of the data folder.
                    if !media::is_reparse_point(&metadata) {
                        stack.push(path);
                    }
                    continue;
                }
                if !metadata.is_file() {
                    continue;
                }
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let Some((captured_at, is_temp)) = capture_time_from_name(name) else {
                    continue;
                };
                if !selection.includes_capture(captured_at) {
                    continue;
                }
                if is_temp {
                    let old_enough = metadata
                        .modified()
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(60));
                    if !old_enough {
                        continue;
                    }
                } else {
                    let Some(relative) = relative_media_path(self.data.root(), &path) else {
                        continue;
                    };
                    match lookup.query_row([relative], |row| row.get::<_, bool>(0)) {
                        Ok(false) => {}
                        _ => continue,
                    }
                }
                match std::fs::remove_file(&path) {
                    Ok(()) => report.orphan_files_deleted += 1,
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "could not delete orphan media");
                        report.file_errors.push(format!("{}: {e}", path.display()));
                    }
                }
            }
        }
    }

    // ----- diagnostics -------------------------------------------------------------------------

    pub fn stats(&self) -> Result<StorageStats> {
        let count = |sql: &str| -> Result<u64> {
            let n: i64 = self.conn.query_row(sql, [], |row| row.get(0))?;
            Ok(u64::try_from(n).unwrap_or(0))
        };
        let (oldest, newest): (Option<i64>, Option<i64>) = self.conn.query_row(
            "SELECT MIN(started_at), MAX(ended_at) FROM events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(StorageStats {
            sessions: count("SELECT COUNT(*) FROM sessions")?,
            events: count("SELECT COUNT(*) FROM events")?,
            observations: count("SELECT COUNT(*) FROM events WHERE kind = 'observation'")?,
            visual_states: count("SELECT COUNT(*) FROM visual_states")?,
            ocr_blocks: count("SELECT COUNT(*) FROM ocr_blocks")?,
            pending_ocr: count("SELECT COUNT(*) FROM visual_states WHERE ocr_status = 'pending'")?,
            failed_ocr: count("SELECT COUNT(*) FROM visual_states WHERE ocr_status = 'failed'")?,
            db_bytes: file_len(&self.data.database()) + file_len(&wal_path(&self.data.database())),
            media_bytes: self.media_bytes()?,
            oldest: oldest.map(Timestamp),
            newest: newest.map(Timestamp),
        })
    }

    pub(crate) fn media_bytes(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(byte_size), 0) FROM visual_states",
            [],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Bytes on disk: `recall.db`, its WAL, and every file under `media/` whether or not a row
    /// points at it. This, not the live row count, is what a size cap has to bound.
    pub fn disk_bytes(&self) -> Result<u64> {
        let db = self.data.database();
        let mut total = file_len(&db).saturating_add(file_len(&wal_path(&db)));
        let mut stack = vec![self.data.media_root()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if metadata.is_dir() {
                    if !media::is_reparse_point(&metadata) {
                        stack.push(entry.path());
                    }
                } else {
                    total = total.saturating_add(metadata.len());
                }
            }
        }
        Ok(total)
    }

    /// `PRAGMA quick_check`. An empty vector means the database is healthy.
    pub fn integrity_check(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("PRAGMA quick_check")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().filter(|r| r != "ok").collect())
    }

    /// Rows whose file is missing and files under `media/` with no row, each capped at `limit`.
    pub fn find_orphans(&self, limit: usize) -> Result<OrphanReport> {
        let mut report = OrphanReport::default();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT id, media_path FROM visual_states ORDER BY id")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                if report.rows_missing_file.len() >= limit
                    && report.rows_invalid_path.len() >= limit
                {
                    break;
                }
                let id = VisualStateId(row.get(0)?);
                let relative: String = row.get(1)?;
                match resolve_media_path(&self.data, &relative) {
                    Ok(path) if path.is_file() => {}
                    Ok(_) => {
                        if report.rows_missing_file.len() < limit {
                            report.rows_missing_file.push((id, relative));
                        }
                    }
                    Err(_) => {
                        if report.rows_invalid_path.len() < limit {
                            report.rows_invalid_path.push(id);
                        }
                    }
                }
            }
        }
        let mut lookup = self
            .conn
            .prepare("SELECT EXISTS (SELECT 1 FROM visual_states WHERE media_path = ?1)")?;
        let mut stack = vec![self.data.media_root()];
        while let Some(dir) = stack.pop() {
            if report.files_without_row.len() >= limit {
                break;
            }
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(StorageError::io(&dir, e)),
            };
            for entry in entries {
                let entry = entry.map_err(|e| StorageError::io(&dir, e))?;
                let path = entry.path();
                let kind = entry.file_type().map_err(|e| StorageError::io(&path, e))?;
                if kind.is_dir() {
                    stack.push(path);
                    continue;
                }
                let known = match relative_media_path(self.data.root(), &path) {
                    Some(relative) => lookup.query_row([relative], |row| row.get::<_, bool>(0))?,
                    None => false,
                };
                if !known && report.files_without_row.len() < limit {
                    report.files_without_row.push(path);
                }
            }
        }
        Ok(report)
    }

    /// Raw connection access for this crate's tests only.
    #[cfg(test)]
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}

fn configure_write_connection(conn: &Connection) -> Result<()> {
    // busy_timeout first so the journal-mode switch itself waits for other connections.
    conn.busy_timeout(BUSY_TIMEOUT)?;
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        // e.g. a filesystem without shared-memory support; correct, just less concurrent.
        tracing::warn!(mode = %mode, "SQLite refused WAL journal mode");
    }
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // Deleted content is zeroed in place instead of lingering in freed pages. (FTS5's own
    // secure-delete flag is persisted by the schema.) Per-connection, so every writer sets it.
    conn.pragma_update(None, "secure_delete", "ON")?;
    Ok(())
}

fn state_to_parts(state: CaptureState) -> (&'static str, Option<i64>) {
    match state {
        CaptureState::Recording => ("recording", None),
        CaptureState::Paused { until } => ("paused", until.map(|t| t.0)),
        CaptureState::Error => ("error", None),
        CaptureState::Stopped => ("stopped", None),
    }
}

fn state_from_parts(state: &str, until: Option<i64>) -> Result<CaptureState> {
    Ok(match state {
        "recording" => CaptureState::Recording,
        "paused" => CaptureState::Paused {
            until: until.map(Timestamp),
        },
        "error" => CaptureState::Error,
        "stopped" => CaptureState::Stopped,
        other => return Err(StorageError::Corrupt(format!("capture state {other:?}"))),
    })
}

/// The `visual_states` insert shared by the recorder and the segment importer. The caller holds
/// the write transaction and has already checked that the media file exists.
pub(crate) fn insert_state_row(
    tx: &Transaction<'_>,
    state: &NewVisualState,
) -> Result<VisualStateId> {
    let byte_size = i64::try_from(state.byte_size)
        .map_err(|_| StorageError::InvalidArgument("byte_size out of range".into()))?;
    let status = if state.ocr_enabled {
        OcrStatus::Pending
    } else {
        OcrStatus::Skipped
    };
    refuse_if_fenced(tx, state.captured_at.0)?;
    tx.execute(
        "INSERT INTO visual_states
             (monitor_id, captured_at, media_path, width, height, byte_size, fingerprint,
              ocr_status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            state.monitor.0,
            state.captured_at.0,
            state.media_path,
            state.width,
            state.height,
            byte_size,
            // Stored bit-for-bit; SQLite integers are signed.
            state.fingerprint.map(|f| f as i64),
            status.as_str()
        ],
    )?;
    Ok(VisualStateId(tx.last_insert_rowid()))
}

/// Replaces one visual state's OCR rows, FTS row and status inside the caller's transaction.
pub(crate) fn write_ocr_rows(
    tx: &Transaction<'_>,
    id: VisualStateId,
    blocks: &[OcrBlock],
    engine: &str,
    elapsed_ms: u64,
) -> Result<()> {
    let mut ordered: Vec<&OcrBlock> = blocks.iter().collect();
    ordered.sort_by_key(|b| b.line_index);
    let text = ordered
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let elapsed_ms = i64::try_from(elapsed_ms).unwrap_or(i64::MAX);
    tx.execute("DELETE FROM ocr_blocks WHERE visual_state_id = ?1", [id.0])?;
    tx.execute("DELETE FROM ocr_fts WHERE rowid = ?1", [id.0])?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO ocr_blocks
                 (visual_state_id, line_index, text, x, y, width, height, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for block in &ordered {
            insert.execute(params![
                id.0,
                block.line_index,
                block.text,
                f64::from(block.x),
                f64::from(block.y),
                f64::from(block.width),
                f64::from(block.height),
                block.confidence.map(f64::from)
            ])?;
        }
    }
    tx.execute(
        "INSERT INTO ocr_fts (rowid, text) VALUES (?1, ?2)",
        params![id.0, text],
    )?;
    tx.execute(
        "UPDATE visual_states
         SET ocr_status = 'done', ocr_error = NULL, ocr_engine = ?2, ocr_ms = ?3
         WHERE id = ?1",
        params![id.0, engine, elapsed_ms],
    )?;
    Ok(())
}

/// Window lookup-or-insert inside the caller's write transaction.
pub(crate) fn upsert_window_row(
    tx: &Transaction<'_>,
    application: ApplicationId,
    window: &WindowContext,
    at: Timestamp,
) -> Result<i64> {
    // Look up with `IS` (NULL-safe) under the write lock: UNIQUE treats NULL class names as
    // distinct, so INSERT ... ON CONFLICT alone would duplicate windows without a class.
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM windows
             WHERE application_id = ?1 AND title = ?2 AND class_name IS ?3",
            params![application.0, window.title, window.class_name],
            |row| row.get(0),
        )
        .optional()?;
    let id = match existing {
        Some(id) => id,
        None => {
            tx.execute(
                "INSERT INTO windows (application_id, title, class_name, first_seen_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![application.0, window.title, window.class_name, at.0],
            )?;
            tx.last_insert_rowid()
        }
    };
    Ok(id)
}

fn refuse_if_fenced(tx: &Transaction<'_>, at: i64) -> Result<()> {
    let fence: Option<(i64, i64)> = tx
        .query_row(
            "SELECT since, until FROM deletion_fences WHERE since <= ?1 AND ?1 < until LIMIT 1",
            [at],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match fence {
        Some((since, until)) => Err(StorageError::Fenced { at, since, until }),
        None => Ok(()),
    }
}

/// Capture time (ms) encoded in a media file name, and whether it is a writer's temp file.
/// `20260930T201404123Z_m2.webp` and `.20260930T201404123Z_m2.webp.1234.5.tmp`; anything else is
/// not ours and `None`.
fn capture_time_from_name(name: &str) -> Option<(i64, bool)> {
    let is_temp = name.starts_with('.') && name.ends_with(".tmp");
    if !is_temp && !name.ends_with(".webp") {
        return None;
    }
    let core = name.strip_prefix('.').unwrap_or(name);
    let stamp = core.get(..18)?;
    if core.as_bytes().get(18) != Some(&b'Z') {
        return None;
    }
    let parsed = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%S%3f").ok()?;
    Some((parsed.and_utc().timestamp_millis(), is_temp))
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn wal_path(database: &Path) -> PathBuf {
    let mut name = database.as_os_str().to_owned();
    name.push("-wal");
    PathBuf::from(name)
}

/// `root/media/2026/x.webp` -> `media/2026/x.webp`; `None` for non-UTF-8 names.
fn relative_media_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let parts = relative
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?;
    Some(parts.join("/"))
}

fn session_capabilities_key(session: SessionId) -> String {
    format!("session.{}.capabilities", session.0)
}
