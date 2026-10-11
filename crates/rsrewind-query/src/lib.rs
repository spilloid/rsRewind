//! Read-only access to rsRewind history for the CLI and UI.
//!
//! [`QueryDb`] opens its own `SQLITE_OPEN_READ_ONLY` connection with `query_only=ON`; it cannot
//! modify the database even by accident, and callers never write SQL: user text goes through
//! [`parse_query`] and is bound as a parameter, never spliced into a statement.

mod gaps;
pub mod history;
mod parse;

pub use history::{History, MediaReader, SourceFilter, SourceInfo, SourceKind, SourceProblem};
pub use parse::{FtsExpr, parse_query};

use rsrewind_core::{
    DataDir, EventId, Gap, MediaPathError, OcrBlock, SCHEMA_VERSION, SearchHit, SearchQuery,
    SourceId, TimelineCursor, TimelineEntry, Timestamp, VisualDetail, VisualStateId,
};
use rusqlite::functions::FunctionFlags;
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior, params,
};
use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_LIMIT: u32 = 20;
pub const MAX_LIMIT: u32 = 200;

pub type Result<T, E = QueryError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("no rsRewind database at {0} (has the recorder ever run?)")]
    DatabaseMissing(PathBuf),

    #[error(
        "database schema version {found} does not match this build ({expected}); \
         run the recorder from the same rsRewind version to migrate it"
    )]
    SchemaMismatch { found: u32, expected: u32 },

    #[error("no source {0} in this data folder")]
    UnknownSource(String),

    #[error(transparent)]
    MediaPath(#[from] MediaPathError),

    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("image error: {0}")]
    Image(String),
}

/// `0` means "use the default"; anything above [`MAX_LIMIT`] is clamped.
pub fn clamp_limit(limit: u32) -> u32 {
    if limit == 0 {
        DEFAULT_LIMIT
    } else {
        limit.min(MAX_LIMIT)
    }
}

/// One read-only connection to `recall.db`.
pub struct QueryDb {
    conn: Connection,
    data: DataDir,
    /// Stamped on every result. `None` for a store opened directly (this machine's history, or
    /// any store the caller pointed at); the history facade sets it for replicas.
    source: Option<SourceId>,
}

impl std::fmt::Debug for QueryDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryDb")
            .field("data", &self.data)
            .field("source", &self.source)
            .finish()
    }
}

/// Columns shared by every timeline-shaped query (see [`timeline_entry`]).
const TIMELINE_SELECT: &str = "
    SELECT e.visual_state_id, e.started_at, e.ended_at, a.process_name, w.title, m.device_name,
           v.media_path, v.ocr_status, e.id
    FROM events e
    JOIN visual_states v ON v.id = e.visual_state_id
    LEFT JOIN applications a ON a.id = e.application_id
    LEFT JOIN windows w ON w.id = e.window_id
    LEFT JOIN monitors m ON m.id = COALESCE(e.monitor_id, v.monitor_id)";

/// The earliest observation of each visual state supplies its app, window and monitor (detail
/// pane: there is no filter to honour).
const EARLIEST_OBSERVATION_JOIN: &str = "
    LEFT JOIN events e ON e.id = (
        SELECT id FROM events
        WHERE visual_state_id = v.id AND kind = 'observation'
        ORDER BY started_at, id LIMIT 1)
    LEFT JOIN applications a ON a.id = e.application_id
    LEFT JOIN windows w ON w.id = e.window_id
    LEFT JOIN monitors m ON m.id = v.monitor_id";

/// Search: `e` is the earliest observation of the state that satisfies *every* filter, so a state
/// qualifies when any of its observation spans overlaps the time range and matches the application
/// and title, and the reported time/app/window come from that observation. Parameters: ?2 since,
/// ?3 until, ?4 app, ?5 title. Spans overlap `[since, until]` when they end at or after `since` and
/// start at or before `until`.
const MATCHING_OBSERVATION_JOIN: &str = "
    LEFT JOIN events e ON e.id = (
        SELECT x.id FROM events x
        LEFT JOIN applications xa ON xa.id = x.application_id
        LEFT JOIN windows xw ON xw.id = x.window_id
        WHERE x.visual_state_id = v.id AND x.kind = 'observation'
          AND (?2 IS NULL OR x.ended_at >= ?2)
          AND (?3 IS NULL OR x.started_at <= ?3)
          AND (?4 IS NULL OR rsr_app_matches(xa.process_name, ?4))
          AND (?5 IS NULL OR rsr_contains_ci(xw.title, ?5))
        ORDER BY x.started_at, x.id LIMIT 1)
    LEFT JOIN applications a ON a.id = e.application_id
    LEFT JOIN windows w ON w.id = e.window_id
    LEFT JOIN monitors m ON m.id = v.monitor_id";

/// A state with no observation yet (written, not yet observed) can only be judged by its capture
/// time, and only when no application/title filter is set.
const SEARCH_QUALIFIES: &str = "
    AND (e.id IS NOT NULL OR (?4 IS NULL AND ?5 IS NULL
         AND (?2 IS NULL OR v.captured_at >= ?2) AND (?3 IS NULL OR v.captured_at <= ?3)))";

impl QueryDb {
    /// Opens the existing database read-only. Never creates one.
    pub fn open(data: &DataDir) -> Result<Self> {
        let path = data.database();
        if !path.is_file() {
            return Err(QueryError::DatabaseMissing(path));
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Duration::from_millis(5_000))?;
        conn.pragma_update(None, "query_only", "ON")?;
        register_functions(&conn)?;

        let found = schema_version(&conn)?;
        if found != SCHEMA_VERSION {
            return Err(QueryError::SchemaMismatch {
                found,
                expected: SCHEMA_VERSION,
            });
        }
        Ok(Self {
            conn,
            data: data.clone(),
            source: None,
        })
    }

    /// Results from this connection are attributed to `source` from now on.
    pub(crate) fn attribute_to(&mut self, source: Option<SourceId>) {
        self.source = source;
    }

    /// A row of the `settings` table (identity and replication bookkeeping), read-only.
    pub(crate) fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// Stretches of `[from, to)` with no observation on any monitor, at least `min_gap_ms` long,
    /// each with the reason the recorder's sessions and markers give (see [`rsrewind_core::Gap`]).
    /// Oldest first.
    pub fn gaps(&self, from: Timestamp, to: Timestamp, min_gap_ms: i64) -> Result<Vec<Gap>> {
        if to <= from {
            return Ok(Vec::new());
        }
        let covered: Vec<(i64, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT started_at, ended_at FROM events
                 WHERE kind = 'observation' AND ended_at >= ?1 AND started_at <= ?2
                 ORDER BY started_at",
            )?;
            let rows =
                stmt.query_map(params![from.0, to.0], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let sessions: Vec<gaps::SessionSpan> = {
            let mut stmt = self.conn.prepare(
                "SELECT s.id, s.started_at, s.ended_at,
                        COALESCE((SELECT MAX(e.ended_at) FROM events e WHERE e.session_id = s.id),
                                 s.started_at)
                 FROM sessions s WHERE s.started_at <= ?1 ORDER BY s.started_at, s.id",
            )?;
            let rows = stmt.query_map([to.0], |row| {
                Ok(gaps::SessionSpan {
                    id: row.get(0)?,
                    start: row.get(1)?,
                    end: row.get(2)?,
                    last_seen: row.get(3)?,
                })
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        // State at `from` depends on markers since the start of the session running then; older
        // sessions cannot matter.
        let since = sessions
            .iter()
            .rev()
            .find(|s| s.start <= from.0)
            .map_or(from.0, |s| s.start);
        let markers: Vec<gaps::Marker> = {
            let mut stmt = self.conn.prepare(
                "SELECT session_id, started_at, kind FROM events
                 WHERE kind IN ('paused', 'resumed', 'idle_start', 'idle_end')
                   AND started_at >= ?1 AND started_at <= ?2
                 ORDER BY started_at, id",
            )?;
            let rows = stmt.query_map(params![since, to.0], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (session, at, kind) = row?;
                if let Some(mark) = gaps::Mark::parse(&kind) {
                    out.push(gaps::Marker { session, at, mark });
                }
            }
            out
        };
        Ok(gaps::classify(
            (from.0, to.0),
            &covered,
            &sessions,
            &markers,
            min_gap_ms,
            self.recorder_alive()?,
        )
        .into_iter()
        .map(|(a, b, reason)| Gap {
            from: Timestamp(a),
            to: Timestamp(b),
            reason,
            source: self.source,
        })
        .collect())
    }

    /// Whether a recorder is writing to this store now: a heartbeat in the last 20 s (it beats
    /// every 5 s) that is not its final "stopped" beat. Imported stores never have one.
    fn recorder_alive(&self) -> Result<bool> {
        const STALE_MS: i64 = 20_000;
        let beat: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT heartbeat_at, state FROM recorder_status WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(
            beat.is_some_and(|(at, state)| {
                state != "stopped" && Timestamp::now().0 - at < STALE_MS
            }),
        )
    }

    /// End of the last thing this store recorded (any event), if anything.
    pub(crate) fn last_activity(&self) -> Result<Option<Timestamp>> {
        Ok(self
            .conn
            .query_row("SELECT MAX(ended_at) FROM events", [], |row| {
                row.get::<_, Option<i64>>(0)
            })?
            .map(Timestamp))
    }

    /// Number of observations with a picture and the time they span, counting only those that end at
    /// or after `since` (all when `None`), with the span clipped to start no earlier than it.
    pub(crate) fn summary_since(
        &self,
        since: Option<Timestamp>,
    ) -> Result<(u64, Option<Timestamp>, Option<Timestamp>)> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*), MAX(MIN(started_at), COALESCE(?1, MIN(started_at))), MAX(ended_at) FROM events
             WHERE kind = 'observation' AND visual_state_id IS NOT NULL
               AND (?1 IS NULL OR ended_at >= ?1)",
            [since.map(|s| s.0)],
            |row| {
                Ok((
                    u64::try_from(row.get::<_, i64>(0)?).unwrap_or(0),
                    row.get::<_, Option<i64>>(1)?.map(Timestamp),
                    row.get::<_, Option<i64>>(2)?.map(Timestamp),
                ))
            },
        )?)
    }

    pub fn data_dir(&self) -> &DataDir {
        &self.data
    }

    /// Full-text search over OCR text with optional time / application / title filters.
    ///
    /// Results are ordered by bm25 (best first), then most recent first. With no searchable text,
    /// the filters alone select visual states, newest first, with the start of their OCR text as
    /// the snippet. `since`/`until` are inclusive and match any observation span of a state that overlaps them
    /// (and satisfies the other filters); the reported time, application and window are those of
    /// the earliest such observation.
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>> {
        let expr = parse_query(&query.text);
        let limit = clamp_limit(query.limit);
        let app = query
            .application
            .as_deref()
            .map(normalize_app)
            .filter(|a| !a.is_empty());
        let title = query
            .title_contains
            .as_deref()
            .map(str::to_lowercase)
            .filter(|t| !t.is_empty());
        let since = query.since.map(|t| t.0);
        let until = query.until.map(|t| t.0);

        let sql = match expr.as_match() {
            Some(_) => format!(
                "SELECT v.id, COALESCE(e.started_at, v.captured_at), a.process_name, w.title,
                        m.device_name, snippet(ocr_fts, 0, '[', ']', '…', 12), v.media_path,
                        bm25(ocr_fts)
                 FROM ocr_fts
                 JOIN visual_states v ON v.id = ocr_fts.rowid
                 {MATCHING_OBSERVATION_JOIN}
                 WHERE ocr_fts MATCH ?1 {SEARCH_QUALIFIES}
                 ORDER BY bm25(ocr_fts), COALESCE(e.started_at, v.captured_at) DESC, v.id DESC
                 LIMIT ?6"
            ),
            None => format!(
                "SELECT v.id, COALESCE(e.started_at, v.captured_at), a.process_name, w.title,
                        m.device_name,
                        COALESCE(substr((SELECT text FROM ocr_fts WHERE rowid = v.id), 1, 160), ''),
                        v.media_path, 0.0
                 FROM visual_states v
                 {MATCHING_OBSERVATION_JOIN}
                 WHERE ?1 IS NULL {SEARCH_QUALIFIES}
                 ORDER BY COALESCE(e.started_at, v.captured_at) DESC, v.id DESC
                 LIMIT ?6"
            ),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![expr.as_match(), since, until, app, title, limit],
            |row| {
                let timestamp = Timestamp(row.get(1)?);
                let relative: String = row.get(6)?;
                Ok(SearchHit {
                    visual_state_id: VisualStateId(row.get(0)?),
                    timestamp,
                    timestamp_utc: rfc3339(timestamp),
                    application: row.get(2)?,
                    window_title: row.get(3)?,
                    monitor: row.get(4)?,
                    snippet: row.get(5)?,
                    media_path: self.absolute(&relative),
                    rank: row.get(7)?,
                    source: self.source,
                })
            },
        )?;
        let hits = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        tracing::debug!(hits = hits.len(), has_text = !expr.is_empty(), "search");
        Ok(hits)
    }

    /// Observation events, newest first. `after` is the cursor of the last entry already shown
    /// ([`TimelineEntry::cursor`]); `(started_at, event_id)` is a total order, so simultaneous
    /// observations on several monitors are neither skipped nor repeated across pages.
    pub fn recent(&self, limit: u32, after: Option<TimelineCursor>) -> Result<Vec<TimelineEntry>> {
        self.recent_before(limit, after.map(|c| (c.started_at.0, c.event_id.0)))
    }

    /// [`QueryDb::recent`] with the bound as raw `(started_at, event_id)`: only entries with
    /// `(started_at, id) < bound` are returned. `(t, i64::MAX)` means "started at or before t",
    /// `(t, i64::MIN)` "started before t"; the history facade uses both to page across stores.
    pub(crate) fn recent_before(
        &self,
        limit: u32,
        bound: Option<(i64, i64)>,
    ) -> Result<Vec<TimelineEntry>> {
        let sql = format!(
            "{TIMELINE_SELECT}
             WHERE e.kind = 'observation'
               AND (?1 IS NULL OR (e.started_at, e.id) < (?1, ?2))
             ORDER BY e.started_at DESC, e.id DESC LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![bound.map(|b| b.0), bound.map(|b| b.1), clamp_limit(limit)],
            |row| self.timeline_entry(row),
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Observation events strictly after the raw bound `(started_at, event_id)`, **oldest first**:
    /// the forward counterpart of [`QueryDb::recent_before`], same total order, same bound
    /// convention (`(t, i64::MIN)` includes everything started at `t`, `(t, i64::MAX)` nothing).
    pub(crate) fn later_than(&self, limit: u32, bound: (i64, i64)) -> Result<Vec<TimelineEntry>> {
        let sql = format!(
            "{TIMELINE_SELECT}
             WHERE e.kind = 'observation' AND (e.started_at, e.id) > (?1, ?2)
             ORDER BY e.started_at ASC, e.id ASC LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![bound.0, bound.1, clamp_limit(limit)], |row| {
            self.timeline_entry(row)
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The observation covering `at` (latest-started if several monitors cover it), otherwise the
    /// nearest one that started before it.
    pub fn at(&self, at: Timestamp) -> Result<Option<TimelineEntry>> {
        let covering = format!(
            "{TIMELINE_SELECT}
             WHERE e.kind = 'observation' AND e.started_at <= ?1 AND e.ended_at >= ?1
             ORDER BY e.started_at DESC, e.id DESC LIMIT 1"
        );
        if let Some(entry) = self
            .conn
            .query_row(&covering, [at.0], |row| self.timeline_entry(row))
            .optional()?
        {
            return Ok(Some(entry));
        }
        let nearest = format!(
            "{TIMELINE_SELECT}
             WHERE e.kind = 'observation' AND e.started_at <= ?1
             ORDER BY e.started_at DESC, e.id DESC LIMIT 1"
        );
        Ok(self
            .conn
            .query_row(&nearest, [at.0], |row| self.timeline_entry(row))
            .optional()?)
    }

    /// Everything about one visual state for the detail pane.
    pub fn visual_detail(&self, id: VisualStateId) -> Result<Option<VisualDetail>> {
        self.visual_detail_with(id, || {})
    }

    /// `between` runs after the first statement and before the second; tests use it to commit a
    /// concurrent write at exactly the moment a torn read would show.
    fn visual_detail_with(
        &self,
        id: VisualStateId,
        between: impl FnOnce(),
    ) -> Result<Option<VisualDetail>> {
        // One read transaction: metadata, OCR text and blocks come from a single snapshot even if
        // the recorder commits OCR (or a deletion) while we read.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Deferred)?;
        let sql = format!(
            "SELECT v.id, v.captured_at, v.width, v.height, v.media_path, a.process_name, w.title,
                    m.device_name, v.ocr_status,
                    (SELECT text FROM ocr_fts WHERE rowid = v.id)
             FROM visual_states v
             {EARLIEST_OBSERVATION_JOIN}
             WHERE v.id = ?1"
        );
        let detail = tx
            .query_row(&sql, [id.0], |row| {
                let relative: String = row.get(4)?;
                Ok(VisualDetail {
                    visual_state_id: VisualStateId(row.get(0)?),
                    captured_at: Timestamp(row.get(1)?),
                    width: row.get(2)?,
                    height: row.get(3)?,
                    media_path: self.absolute(&relative),
                    application: row.get(5)?,
                    window_title: row.get(6)?,
                    monitor: row.get(7)?,
                    ocr_status: row.get(8)?,
                    ocr_text: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                    blocks: Vec::new(),
                    source: self.source,
                })
            })
            .optional()?;
        between();
        let Some(mut detail) = detail else {
            return Ok(None);
        };
        let mut stmt = tx.prepare(
            "SELECT text, x, y, width, height, confidence, line_index FROM ocr_blocks
             WHERE visual_state_id = ?1 ORDER BY line_index, id",
        )?;
        detail.blocks = stmt
            .query_map([id.0], |row| {
                Ok(OcrBlock {
                    text: row.get(0)?,
                    // Stored as REAL (f64); boxes are pixel coordinates, f32 is ample.
                    x: row.get::<_, f64>(1)? as f32,
                    y: row.get::<_, f64>(2)? as f32,
                    width: row.get::<_, f64>(3)? as f32,
                    height: row.get::<_, f64>(4)? as f32,
                    confidence: row.get::<_, Option<f64>>(5)?.map(|c| c as f32),
                    line_index: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(Some(detail))
    }

    fn timeline_entry(&self, row: &Row<'_>) -> rusqlite::Result<TimelineEntry> {
        let relative: String = row.get(6)?;
        Ok(TimelineEntry {
            visual_state_id: VisualStateId(row.get(0)?),
            started_at: Timestamp(row.get(1)?),
            ended_at: Timestamp(row.get(2)?),
            application: row.get(3)?,
            window_title: row.get(4)?,
            monitor: row.get(5)?,
            media_path: self.absolute(&relative),
            ocr_status: row.get(7)?,
            event_id: EventId(row.get(8)?),
            source: self.source,
        })
    }

    /// Absolute path for display/loading; empty if the stored path is unsafe (tampered row), so
    /// nothing outside the data directory is ever handed to a caller.
    fn absolute(&self, relative: &str) -> String {
        match self.data.resolve_media_checked(relative) {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(_) => {
                tracing::warn!("ignoring unsafe media path stored in the database");
                String::new()
            }
        }
    }
}

fn schema_version(conn: &Connection) -> Result<u32> {
    let has_table: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' \
         AND name = 'schema_migrations')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(0);
    }
    let version: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    Ok(u32::try_from(version).unwrap_or(u32::MAX))
}

/// `Teams`, `teams.exe`, ` TEAMS.EXE ` all normalize to `teams`.
fn normalize_app(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    match lower.strip_suffix(".exe") {
        Some(base) => base.to_owned(),
        None => lower,
    }
}

/// SQLite's NOCASE/`lower()` fold ASCII only; these fold full Unicode in Rust.
fn register_functions(conn: &Connection) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    conn.create_scalar_function("rsr_app_matches", 2, flags, |ctx| {
        let process: Option<String> = ctx.get(0)?;
        let wanted: String = ctx.get(1)?;
        Ok(process.is_some_and(|p| normalize_app(&p) == wanted))
    })?;
    conn.create_scalar_function("rsr_contains_ci", 2, flags, |ctx| {
        let haystack: Option<String> = ctx.get(0)?;
        let needle: String = ctx.get(1)?;
        Ok(haystack.is_some_and(|h| h.to_lowercase().contains(&needle)))
    })?;
    Ok(())
}

fn rfc3339(timestamp: Timestamp) -> String {
    timestamp
        .to_utc()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_are_clamped() {
        assert_eq!(clamp_limit(0), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(5), 5);
        assert_eq!(clamp_limit(200), 200);
        assert_eq!(clamp_limit(10_000), MAX_LIMIT);
    }

    #[test]
    fn connection_cannot_write_and_rejects_other_schema_versions()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let data = DataDir::new(tmp.path());
        let store = rsrewind_storage::Store::open(&data)?;
        store.begin_session(Timestamp(1), "host", "test")?;

        let db = QueryDb::open(&data)?;
        assert!(db.conn.execute("DELETE FROM sessions", []).is_err());
        assert!(
            db.conn
                .execute_batch("CREATE TABLE sneaky (x INTEGER)")
                .is_err()
        );
        drop(db);

        // Simulate a database migrated by a newer build.
        let writer = Connection::open(data.database())?;
        writer.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, 'future', 0)",
            [SCHEMA_VERSION + 1],
        )?;
        match QueryDb::open(&data) {
            Err(QueryError::SchemaMismatch { found, expected }) => {
                assert_eq!((found, expected), (SCHEMA_VERSION + 1, SCHEMA_VERSION));
            }
            other => panic!("expected SchemaMismatch, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn visual_detail_is_one_snapshot_even_if_ocr_commits_mid_read()
    -> Result<(), Box<dyn std::error::Error>> {
        use rsrewind_core::{MonitorInfo, OcrBlock};
        use rsrewind_storage::media::{encode_webp, write_webp_exclusive};
        use rsrewind_storage::{NewVisualState, Store};

        let tmp = tempfile::tempdir()?;
        let data = DataDir::new(tmp.path());
        let store = Store::open(&data)?;
        let monitor = store.upsert_monitor(
            &MonitorInfo {
                device_name: "m".into(),
                left: 0,
                top: 0,
                width: 8,
                height: 4,
                dpi: 96,
                primary: true,
            },
            Timestamp(1),
        )?;
        let frame = rsrewind_core::BgraFrame {
            width: 8,
            height: 4,
            stride: 32,
            pixels: vec![7; 128],
        };
        let bytes = encode_webp(&frame, 50)?;
        let relative = rsrewind_core::paths::media_relative_path(Timestamp(5), monitor);
        write_webp_exclusive(&data, &relative, &bytes)?;
        let id = store.insert_visual_state(&NewVisualState {
            monitor,
            captured_at: Timestamp(5),
            media_path: relative.clone(),
            width: 8,
            height: 4,
            byte_size: bytes.len() as u64,
            fingerprint: None,
            ocr_enabled: true,
        })?;
        let block = OcrBlock {
            text: "late words".into(),
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            confidence: None,
            line_index: 0,
        };
        let db = QueryDb::open(&data)?;
        let detail = db
            .visual_detail_with(id, || {
                // OCR lands between the two reads.
                let _ = store.save_ocr(id, &relative, std::slice::from_ref(&block), "t", 1);
            })?
            .ok_or("missing")?;
        // Consistent: either all of the old state or all of the new one, never "pending, no
        // text, but with blocks".
        assert_eq!(detail.ocr_status, "pending");
        assert!(
            detail.ocr_text.is_empty() && detail.blocks.is_empty(),
            "{detail:?}"
        );
        let after = db.visual_detail(id)?.ok_or("missing")?;
        assert_eq!((after.ocr_status.as_str(), after.blocks.len()), ("done", 1));
        Ok(())
    }

    #[test]
    fn app_names_normalize() {
        for name in ["Teams", "teams.exe", " TEAMS.EXE ", "tEaMs.Exe"] {
            assert_eq!(normalize_app(name), "teams");
        }
        assert_eq!(normalize_app("ÄPP.EXE"), "äpp");
    }
}
