//! Read-only access to rsRewind history for the CLI and UI.
//!
//! [`QueryDb`] opens its own `SQLITE_OPEN_READ_ONLY` connection with `query_only=ON`; it cannot
//! modify the database even by accident, and callers never write SQL: user text goes through
//! [`parse_query`] and is bound as a parameter, never spliced into a statement.

mod parse;

pub use parse::{FtsExpr, parse_query};

use rsrewind_core::{
    DataDir, OcrBlock, SearchHit, SearchQuery, TimelineEntry, Timestamp, VisualDetail,
    VisualStateId,
};
use rsrewind_storage::SCHEMA_VERSION;
use rsrewind_storage::media::resolve_media_path;
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params};
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
}

impl std::fmt::Debug for QueryDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryDb").field("data", &self.data).finish()
    }
}

/// Columns shared by every timeline-shaped query (see [`timeline_entry`]).
const TIMELINE_SELECT: &str = "
    SELECT e.visual_state_id, e.started_at, e.ended_at, a.process_name, w.title, m.device_name,
           v.media_path, v.ocr_status
    FROM events e
    JOIN visual_states v ON v.id = e.visual_state_id
    LEFT JOIN applications a ON a.id = e.application_id
    LEFT JOIN windows w ON w.id = e.window_id
    LEFT JOIN monitors m ON m.id = COALESCE(e.monitor_id, v.monitor_id)";

/// The earliest observation of each visual state supplies its timestamp, app, window and monitor.
const EARLIEST_OBSERVATION_JOIN: &str = "
    LEFT JOIN events e ON e.id = (
        SELECT id FROM events
        WHERE visual_state_id = v.id AND kind = 'observation'
        ORDER BY started_at, id LIMIT 1)
    LEFT JOIN applications a ON a.id = e.application_id
    LEFT JOIN windows w ON w.id = e.window_id
    LEFT JOIN monitors m ON m.id = v.monitor_id";

/// Filters shared by both search shapes. Parameters: ?2 since, ?3 until, ?4 app, ?5 title.
const SEARCH_FILTERS: &str = "
    AND (?2 IS NULL OR COALESCE(e.started_at, v.captured_at) >= ?2)
    AND (?3 IS NULL OR COALESCE(e.started_at, v.captured_at) <= ?3)
    AND (?4 IS NULL OR rsr_app_matches(a.process_name, ?4))
    AND (?5 IS NULL OR rsr_contains_ci(w.title, ?5))";

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
        })
    }

    pub fn data_dir(&self) -> &DataDir {
        &self.data
    }

    /// Full-text search over OCR text with optional time / application / title filters.
    ///
    /// Results are ordered by bm25 (best first), then most recent first. With no searchable text,
    /// the filters alone select visual states, newest first, with the start of their OCR text as
    /// the snippet. `since`/`until` are inclusive and apply to each state's first observation.
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
                 {EARLIEST_OBSERVATION_JOIN}
                 WHERE ocr_fts MATCH ?1 {SEARCH_FILTERS}
                 ORDER BY bm25(ocr_fts), COALESCE(e.started_at, v.captured_at) DESC, v.id DESC
                 LIMIT ?6"
            ),
            None => format!(
                "SELECT v.id, COALESCE(e.started_at, v.captured_at), a.process_name, w.title,
                        m.device_name,
                        COALESCE(substr((SELECT text FROM ocr_fts WHERE rowid = v.id), 1, 160), ''),
                        v.media_path, 0.0
                 FROM visual_states v
                 {EARLIEST_OBSERVATION_JOIN}
                 WHERE ?1 IS NULL {SEARCH_FILTERS}
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
                })
            },
        )?;
        let hits = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        tracing::debug!(hits = hits.len(), has_text = !expr.is_empty(), "search");
        Ok(hits)
    }

    /// Observation events, newest first, optionally strictly before `before` (for paging).
    pub fn recent(&self, limit: u32, before: Option<Timestamp>) -> Result<Vec<TimelineEntry>> {
        let sql = format!(
            "{TIMELINE_SELECT}
             WHERE e.kind = 'observation' AND (?1 IS NULL OR e.started_at < ?1)
             ORDER BY e.started_at DESC, e.id DESC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![before.map(|t| t.0), clamp_limit(limit)], |row| {
            self.timeline_entry(row)
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The observation covering `at` (latest-started if several monitors cover it), otherwise the
    /// nearest one that started before it.
    pub fn at(&self, at: Timestamp) -> Result<Option<TimelineEntry>> {
        // A covering event is one of the last few to start before `at` unless one span outlives
        // dozens of later ones; scanning a bounded window keeps this an index range read.
        let sql = format!(
            "{TIMELINE_SELECT}
             WHERE e.kind = 'observation' AND e.started_at <= ?1
             ORDER BY e.started_at DESC, e.id DESC LIMIT 64"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let candidates = stmt
            .query_map([at.0], |row| self.timeline_entry(row))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let covering = candidates.iter().position(|e| e.ended_at >= at);
        Ok(match covering {
            Some(index) => candidates.into_iter().nth(index),
            None => candidates.into_iter().next(),
        })
    }

    /// Everything about one visual state for the detail pane.
    pub fn visual_detail(&self, id: VisualStateId) -> Result<Option<VisualDetail>> {
        let sql = format!(
            "SELECT v.id, v.captured_at, v.width, v.height, v.media_path, a.process_name, w.title,
                    m.device_name, v.ocr_status,
                    (SELECT text FROM ocr_fts WHERE rowid = v.id)
             FROM visual_states v
             {EARLIEST_OBSERVATION_JOIN}
             WHERE v.id = ?1"
        );
        let detail = self
            .conn
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
                })
            })
            .optional()?;
        let Some(mut detail) = detail else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare(
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
        })
    }

    /// Absolute path for display/loading; empty if the stored path is unsafe (tampered row), so
    /// nothing outside the data directory is ever handed to a caller.
    fn absolute(&self, relative: &str) -> String {
        match resolve_media_path(&self.data, relative) {
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
    fn app_names_normalize() {
        for name in ["Teams", "teams.exe", " TEAMS.EXE ", "tEaMs.Exe"] {
            assert_eq!(normalize_app(name), "teams");
        }
        assert_eq!(normalize_app("ÄPP.EXE"), "äpp");
    }
}
