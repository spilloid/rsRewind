//! Sealing closed history into segments (`export_segment`) and merging segments from other
//! sources into a store (`import_segment`). See `docs/design/distributed.md`.
//!
//! The file format lives in `rsrewind-segment`; this module is the part that needs SQL.
//!
//! Two roles, never mixed in one store:
//!
//! * **origin** - a store the local recorder writes. It has a random source id and can export.
//! * **replica** - a store that exists only to hold one remote source's history. It is written
//!   by `import_segment` and nothing else, and refuses segments from any other source. A central
//!   instance keeps one replica per probe under `sources/<id>/`, so cross-endpoint attribution
//!   mistakes are impossible by construction rather than by care.

use crate::media::{resolve_media_path, webp_dimensions, write_webp_exclusive};
use crate::store::{Store, insert_state_row, upsert_window_row, write_ocr_rows};
use crate::{NewVisualState, Result, StorageError};
use rsrewind_core::paths::media_relative_path;
use rsrewind_core::{
    ApplicationId, DataDir, EventKind, MonitorId, MonitorInfo, OcrBlock, SessionId, Timestamp,
    VisualStateId, WindowContext, WindowId,
};
use rsrewind_segment::{
    ApplicationRec, EventRec, ExportCursor, FORMAT_VERSION, Manifest, MonitorRec, OcrRec, OcrState,
    Segment, SessionRec, SourceInfo, StateRec, WindowRec, file_name, seq_from_file_name,
    write_segment,
};
use rusqlite::{OptionalExtension, Transaction, params};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A segment only seals events that ended at least this long ago, because the recorder extends
/// the latest observation in place while the screen is unchanged. The recorder's extension window
/// is a few capture ticks; a minute leaves a wide margin and lets OCR finish first.
pub const MIN_SETTLE_MS: i64 = 60_000;
pub const DEFAULT_SETTLE_MS: i64 = 5 * 60_000;
pub const DEFAULT_MAX_EVENTS: usize = 5_000;

const KEY_SOURCE_ID: &str = "source.id";
const KEY_SOURCE_ROLE: &str = "source.role";
const KEY_SOURCE_LABEL: &str = "source.label";
const KEY_EXPORT_CUT: &str = "export.cut_ms";
const KEY_EXPORT_MAX_EVENT: &str = "export.max_event_id";
const KEY_EXPORT_NEXT_SEQ: &str = "export.next_seq";
const KEY_IMPORT_PREFIX: &str = "import.seq.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRole {
    Origin,
    Replica,
}

impl SourceRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Origin => "origin",
            Self::Replica => "replica",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIdentity {
    /// 32 lowercase hex characters.
    pub id: String,
    pub role: SourceRole,
}

pub struct ExportOptions<'a> {
    /// Display name only (never used for attribution). Usually the hostname.
    pub label: &'a str,
    pub platform: &'a str,
    pub app_version: &'a str,
    pub capabilities: &'a [&'a str],
    pub now: Timestamp,
    /// Only events that ended at least this long before `now` are sealed. At least
    /// [`MIN_SETTLE_MS`].
    pub settle_ms: i64,
    /// Upper bound on events per segment; a longer backlog becomes several segments.
    pub max_events: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportReport {
    pub seq: u64,
    pub path: PathBuf,
    pub content_hash: String,
    pub bytes: u64,
    pub events: usize,
    pub states: usize,
    /// Events dropped because the image they point at is gone from disk.
    pub skipped_missing_media: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub source_id: String,
    pub seq: u64,
    pub events: usize,
    pub states_new: usize,
    /// States this store already had from an earlier segment (an observation can outlive the
    /// segment that first carried its image).
    pub states_reused: usize,
    /// Dropped because they fall inside an interval deleted on this store.
    pub skipped_fenced: usize,
    /// Dropped because the image is not a valid still WebP of the declared size.
    pub skipped_invalid_image: usize,
    /// Events of a kind this build does not know.
    pub skipped_unknown_kind: usize,
    /// Events whose image was skipped, so they would have pointed at nothing.
    pub skipped_dangling: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    Imported(ImportReport),
    /// Exactly this segment (same source, sequence and content) was imported before. Nothing
    /// changed.
    AlreadyImported {
        source_id: String,
        seq: u64,
    },
}

impl Store {
    // ----- settings ----------------------------------------------------------------------------

    pub(crate) fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten())
    }

    pub(crate) fn set_setting(conn: &rusqlite::Connection, key: &str, value: &str) -> Result<()> {
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn setting_i64(&self, key: &str, default: i64) -> Result<i64> {
        match self.setting(key)? {
            Some(value) => value
                .parse()
                .map_err(|_| StorageError::Corrupt(format!("setting {key} = {value:?}"))),
            None => Ok(default),
        }
    }

    // ----- identity ----------------------------------------------------------------------------

    /// This store's source identity. A store that has none yet becomes an origin with a fresh
    /// random id: minted once, kept for the life of the data directory. Reinstalling without the
    /// data directory therefore yields a *new* source, which is the safe failure (a new lane on
    /// the receiver) rather than a silently merged or overwritten one.
    pub fn source_identity(&self) -> Result<SourceIdentity> {
        if self.setting(KEY_SOURCE_ID)?.is_none() {
            let mut bytes = [0u8; 16];
            getrandom::fill(&mut bytes).map_err(|e| StorageError::Entropy(e.to_string()))?;
            let id = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
            let tx = self.immediate()?;
            // OR IGNORE: if another process minted one first, theirs wins and we read it back.
            tx.execute(
                "INSERT OR IGNORE INTO settings (key, value) VALUES (?1, ?2)",
                params![KEY_SOURCE_ID, id],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO settings (key, value) VALUES (?1, ?2)",
                params![KEY_SOURCE_ROLE, SourceRole::Origin.as_str()],
            )?;
            tx.commit()?;
        }
        let id = self
            .setting(KEY_SOURCE_ID)?
            .ok_or_else(|| StorageError::Corrupt("source id vanished".into()))?;
        let role = match self.setting(KEY_SOURCE_ROLE)?.as_deref() {
            Some("replica") => SourceRole::Replica,
            Some("origin") | None => SourceRole::Origin,
            Some(other) => return Err(StorageError::Corrupt(format!("source role {other:?}"))),
        };
        Ok(SourceIdentity { id, role })
    }

    /// Makes this (fresh) store the replica of `source_id`, or confirms it already is.
    fn adopt_replica_identity(&self, source_id: &str) -> Result<()> {
        let tx = self.immediate()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [KEY_SOURCE_ID],
                |row| row.get(0),
            )
            .optional()?;
        match existing {
            None => {
                Self::set_setting(&tx, KEY_SOURCE_ID, source_id)?;
                Self::set_setting(&tx, KEY_SOURCE_ROLE, SourceRole::Replica.as_str())?;
            }
            Some(found) => {
                let role: Option<String> = tx
                    .query_row(
                        "SELECT value FROM settings WHERE key = ?1",
                        [KEY_SOURCE_ROLE],
                        |row| row.get(0),
                    )
                    .optional()?;
                if found != source_id || role.as_deref() != Some("replica") {
                    return Err(StorageError::SourceMismatch {
                        expected: found,
                        found: source_id.to_owned(),
                    });
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The display name a replica last heard from its source. Never used for attribution.
    pub fn source_label(&self) -> Result<Option<String>> {
        self.setting(KEY_SOURCE_LABEL)
    }

    /// How far export has got. `cut_ms == 0` and `next_seq == 1` mean "never exported".
    pub fn export_status(&self) -> Result<ExportStatus> {
        Ok(ExportStatus {
            next_seq: u64::try_from(self.setting_i64(KEY_EXPORT_NEXT_SEQ, 1)?).unwrap_or(1),
            cut_ms: self.setting_i64(KEY_EXPORT_CUT, 0)?,
        })
    }

    /// Events overlapping `[since, until)` that have already been sealed into a segment, so a copy
    /// may exist outside this store. Call it *before* deleting the range.
    pub fn count_sealed_events(&self, since: Timestamp, until: Timestamp) -> Result<u64> {
        let cut = self.setting_i64(KEY_EXPORT_CUT, 0)?;
        if cut == 0 {
            return Ok(0);
        }
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM events
             WHERE ended_at >= ?1 AND started_at < ?2 AND ended_at <= ?3",
            params![since.0, until.0, cut],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    // ----- export ------------------------------------------------------------------------------

    /// Seals the next closed slice of history into `out_dir` as one segment file.
    ///
    /// Returns `None` when nothing new has settled. History is selected by `ended_at`, not by id:
    /// an observation that has been on screen for an hour keeps extending, and must not hold back
    /// everything recorded after it. An event is sealed exactly once, when it has been quiet for
    /// `settle_ms`, and a settled event can never change afterwards (the recorder only extends an
    /// observation across a gap far smaller than the minimum settle time).
    ///
    /// Sealing does not delete anything and does not need the network. Local retention still
    /// applies: history that retention removes before it settles is never exported.
    ///
    /// Crash safety: the segment file is published atomically, then the watermark advances. If the
    /// process dies in between, the next call finds the published file in `out_dir`, advances the
    /// watermark from its recorded cursor, and carries on - it never seals the same history under
    /// a second sequence number.
    pub fn export_segment(
        &self,
        out_dir: &Path,
        options: &ExportOptions<'_>,
    ) -> Result<Option<ExportReport>> {
        if options.settle_ms < MIN_SETTLE_MS {
            return Err(StorageError::InvalidArgument(format!(
                "settle time must be at least {MIN_SETTLE_MS} ms"
            )));
        }
        if options.max_events == 0 {
            return Err(StorageError::InvalidArgument(
                "max_events must be > 0".into(),
            ));
        }
        let identity = self.source_identity()?;
        if identity.role != SourceRole::Origin {
            return Err(StorageError::NotAnOrigin(identity.id));
        }
        std::fs::create_dir_all(out_dir).map_err(|e| StorageError::io(out_dir, e))?;
        self.recover_outbox(out_dir, &identity.id)?;

        let prev_cut = self.setting_i64(KEY_EXPORT_CUT, 0)?;
        let prev_max_id = self.setting_i64(KEY_EXPORT_MAX_EVENT, 0)?;
        let seq = u64::try_from(self.setting_i64(KEY_EXPORT_NEXT_SEQ, 1)?)
            .map_err(|_| StorageError::Corrupt("export sequence".into()))?;
        let cutoff = options.now.0.saturating_sub(options.settle_ms);

        // Candidates: settled, and either newly settled since the last cut or inserted after the
        // last sealed id (the second clause catches rows stamped earlier than the cut by a clock
        // that stepped backwards).
        const SELECTION: &str = "ended_at <= ?1 AND (ended_at > ?2 OR id > ?3)";
        let mut upper = cutoff;
        let eligible: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM events WHERE {SELECTION}"),
            params![cutoff, prev_cut, prev_max_id],
            |row| row.get(0),
        )?;
        if eligible > options.max_events as i64 {
            // Cut at the max_events-th event's end time, then take every event that ended by
            // then, so events sharing that instant are not split across segments.
            upper = self.conn.query_row(
                &format!(
                    "SELECT ended_at FROM events WHERE {SELECTION}
                     ORDER BY ended_at, id LIMIT 1 OFFSET ?4"
                ),
                params![cutoff, prev_cut, prev_max_id, options.max_events as i64 - 1],
                |row| row.get(0),
            )?;
        }

        let rows: Vec<EventRow> = {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT id, session_id, kind, started_at, ended_at, monitor_id, visual_state_id,
                        application_id, window_id, metadata_json
                 FROM events WHERE {SELECTION} ORDER BY ended_at, id"
            ))?;
            stmt.query_map(params![upper, prev_cut, prev_max_id], |row| {
                Ok(EventRow {
                    id: row.get(0)?,
                    session: row.get(1)?,
                    kind: row.get(2)?,
                    started_at: row.get(3)?,
                    ended_at: row.get(4)?,
                    monitor: row.get(5)?,
                    state: row.get(6)?,
                    application: row.get(7)?,
                    window: row.get(8)?,
                    metadata_json: row.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?
        };

        if rows.is_empty() {
            // Nothing to seal, but the settled boundary still moves forward.
            Self::set_setting(
                &self.conn,
                KEY_EXPORT_CUT,
                &prev_cut.max(cutoff).to_string(),
            )?;
            return Ok(None);
        }

        let mut build = ManifestBuilder::default();
        let mut skipped_missing_media = 0usize;
        let mut max_id = prev_max_id;
        for row in &rows {
            max_id = max_id.max(row.id);
            let state = match row.state {
                Some(id) => match self.load_state(&mut build, id)? {
                    Some(index) => Some(index),
                    None => {
                        skipped_missing_media += 1;
                        continue;
                    }
                },
                None => None,
            };
            let session = self.intern_session(&mut build, row.session)?;
            let monitor = row
                .monitor
                .map(|id| self.intern_monitor(&mut build, id))
                .transpose()?;
            let application = row
                .application
                .map(|id| self.intern_application(&mut build, id))
                .transpose()?;
            let window = row
                .window
                .map(|id| self.intern_window(&mut build, id))
                .transpose()?;
            build.events.push(EventRec {
                kind: row.kind.clone(),
                started_at: row.started_at,
                ended_at: row.ended_at,
                session,
                monitor,
                state,
                application,
                window,
                metadata_json: row.metadata_json.clone(),
            });
        }

        let manifest = Manifest {
            format: FORMAT_VERSION,
            source: SourceInfo {
                id: identity.id.clone(),
                label: options.label.to_owned(),
                platform: options.platform.to_owned(),
                app_version: options.app_version.to_owned(),
                capabilities: self.segment_capabilities(options.capabilities, &build)?,
            },
            seq,
            created_at: options.now.0,
            cursor: ExportCursor {
                cut_ms: prev_cut.max(upper),
                max_event_id: max_id,
            },
            sessions: build.sessions,
            monitors: build.monitors,
            applications: build.applications,
            windows: build.windows,
            states: build.states,
            events: build.events,
        };
        let (events, states) = (manifest.events.len(), manifest.states.len());
        let blob_paths = build.blob_paths;
        let written = write_segment(
            &out_dir.join(file_name(&identity.id, seq)),
            &manifest,
            |i| std::fs::read(&blob_paths[i]),
        )?;

        let tx = self.immediate()?;
        Self::set_setting(&tx, KEY_EXPORT_CUT, &manifest.cursor.cut_ms.to_string())?;
        Self::set_setting(
            &tx,
            KEY_EXPORT_MAX_EVENT,
            &manifest.cursor.max_event_id.to_string(),
        )?;
        Self::set_setting(&tx, KEY_EXPORT_NEXT_SEQ, &(seq + 1).to_string())?;
        tx.commit()?;

        tracing::info!(seq, events, states, bytes = written.bytes, "sealed segment");
        Ok(Some(ExportReport {
            seq,
            path: written.path,
            content_hash: written.content_hash,
            bytes: written.bytes,
            events,
            states,
            skipped_missing_media,
        }))
    }

    /// Advances the watermark past any segment already published in `out_dir` but not yet
    /// reflected in the settings (a crash between publishing and committing the watermark).
    fn recover_outbox(&self, out_dir: &Path, source_id: &str) -> Result<()> {
        let next = u64::try_from(self.setting_i64(KEY_EXPORT_NEXT_SEQ, 1)?).unwrap_or(1);
        let prefix = format!("seg-{}-", source_id.get(..8).unwrap_or(source_id));
        let mut pending: Vec<(u64, PathBuf)> = std::fs::read_dir(out_dir)
            .map_err(|e| StorageError::io(out_dir, e))?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_owned();
                let seq = seq_from_file_name(&name)?;
                (name.starts_with(&prefix) && seq >= next).then(|| (seq, entry.path()))
            })
            .collect();
        pending.sort();
        for (seq, path) in pending {
            let corrupt = |reason: String| StorageError::OutboxCorrupt {
                path: path.clone(),
                reason,
            };
            let segment = Segment::open(&path).map_err(|e| corrupt(e.to_string()))?;
            let manifest = segment.manifest();
            if manifest.source.id != source_id || manifest.seq != seq {
                return Err(corrupt("file name disagrees with its manifest".into()));
            }
            let tx = self.immediate()?;
            let cut = self
                .setting_i64(KEY_EXPORT_CUT, 0)?
                .max(manifest.cursor.cut_ms);
            let max_id = self
                .setting_i64(KEY_EXPORT_MAX_EVENT, 0)?
                .max(manifest.cursor.max_event_id);
            Self::set_setting(&tx, KEY_EXPORT_CUT, &cut.to_string())?;
            Self::set_setting(&tx, KEY_EXPORT_MAX_EVENT, &max_id.to_string())?;
            Self::set_setting(&tx, KEY_EXPORT_NEXT_SEQ, &(seq + 1).to_string())?;
            tx.commit()?;
            tracing::warn!(seq, "recovered an unrecorded segment from the outbox");
        }
        Ok(())
    }

    /// Adds visual state `id` to the manifest; `None` if its image is not readable.
    fn load_state(&self, build: &mut ManifestBuilder, id: i64) -> Result<Option<u32>> {
        if let Some(&index) = build.state_index.get(&id) {
            return Ok(Some(index));
        }
        type Row = (
            i64,
            i64,
            String,
            u32,
            u32,
            Option<i64>,
            String,
            Option<String>,
            Option<i64>,
        );
        let row: Option<Row> = self
            .conn
            .query_row(
                "SELECT monitor_id, captured_at, media_path, width, height, fingerprint,
                        ocr_status, ocr_engine, ocr_ms
                 FROM visual_states WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .optional()?;
        let Some((monitor, captured_at, media, width, height, fingerprint, ocr, engine, ms)) = row
        else {
            return Ok(None);
        };
        let Ok(path) = resolve_media_path(self.data_dir(), &media) else {
            return Ok(None);
        };
        let len = match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() && meta.len() > 0 => meta.len(),
            _ => return Ok(None),
        };
        let ocr_state = match ocr.as_str() {
            "done" => OcrState::Done,
            "failed" => OcrState::Failed,
            "skipped" => OcrState::Skipped,
            _ => OcrState::Pending,
        };
        let ocr_blocks = if ocr_state == OcrState::Done {
            let mut stmt = self.conn.prepare(
                "SELECT text, x, y, width, height, confidence, line_index
                 FROM ocr_blocks WHERE visual_state_id = ?1 ORDER BY line_index",
            )?;
            stmt.query_map([id], |row| {
                Ok(OcrRec {
                    text: row.get(0)?,
                    x: row.get::<_, f64>(1)? as f32,
                    y: row.get::<_, f64>(2)? as f32,
                    width: row.get::<_, f64>(3)? as f32,
                    height: row.get::<_, f64>(4)? as f32,
                    confidence: row.get::<_, Option<f64>>(5)?.map(|c| c as f32),
                    line_index: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?
        } else {
            Vec::new()
        };
        let monitor = self.intern_monitor(build, monitor)?;
        let index = build.states.len() as u32;
        build.states.push(StateRec {
            monitor,
            captured_at,
            width,
            height,
            fingerprint: fingerprint.map(|f| f as u64),
            blob_len: len,
            ocr_state,
            ocr_engine: engine,
            ocr_ms: ms.and_then(|m| u64::try_from(m).ok()),
            ocr_blocks,
        });
        build.blob_paths.push(path);
        build.state_index.insert(id, index);
        Ok(Some(index))
    }

    fn intern_session(&self, build: &mut ManifestBuilder, id: i64) -> Result<u32> {
        if let Some(&i) = build.session_index.get(&id) {
            return Ok(i);
        }
        let rec = self.conn.query_row(
            "SELECT started_at, ended_at, hostname, app_version FROM sessions WHERE id = ?1",
            [id],
            |row| {
                Ok(SessionRec {
                    started_at: row.get(0)?,
                    ended_at: row.get(1)?,
                    hostname: row.get(2)?,
                    app_version: row.get(3)?,
                })
            },
        )?;
        let i = build.sessions.len() as u32;
        build.sessions.push(rec);
        build.session_index.insert(id, i);
        Ok(i)
    }

    /// The build's capability tags narrowed to what every session in the segment recorded it
    /// could see. A session from before capabilities were recorded narrows nothing (it ran the
    /// one recorder that existed then, whose tags are the build's own).
    fn segment_capabilities(
        &self,
        build_tags: &[&str],
        build: &ManifestBuilder,
    ) -> Result<Vec<String>> {
        let mut tags: Vec<String> = build_tags.iter().map(|c| (*c).to_owned()).collect();
        for &session in build.session_index.keys() {
            if let Some(recorded) = self.session_capabilities(SessionId(session))? {
                let allowed = recorded.capabilities.segment_tags();
                tags.retain(|tag| allowed.contains(&tag.as_str()));
            }
        }
        Ok(tags)
    }

    fn intern_monitor(&self, build: &mut ManifestBuilder, id: i64) -> Result<u32> {
        if let Some(&i) = build.monitor_index.get(&id) {
            return Ok(i);
        }
        let rec = self.conn.query_row(
            "SELECT device_name, \"left\", \"top\", width, height, dpi, primary_monitor
             FROM monitors WHERE id = ?1",
            [id],
            |row| {
                Ok(MonitorRec {
                    device_name: row.get(0)?,
                    left: row.get(1)?,
                    top: row.get(2)?,
                    width: row.get(3)?,
                    height: row.get(4)?,
                    dpi: row.get(5)?,
                    primary: row.get::<_, i64>(6)? != 0,
                })
            },
        )?;
        let i = build.monitors.len() as u32;
        build.monitors.push(rec);
        build.monitor_index.insert(id, i);
        Ok(i)
    }

    fn intern_application(&self, build: &mut ManifestBuilder, id: i64) -> Result<u32> {
        if let Some(&i) = build.application_index.get(&id) {
            return Ok(i);
        }
        let rec = self.conn.query_row(
            "SELECT process_name, exe_path FROM applications WHERE id = ?1",
            [id],
            |row| {
                Ok(ApplicationRec {
                    process_name: row.get(0)?,
                    exe_path: row.get(1)?,
                })
            },
        )?;
        let i = build.applications.len() as u32;
        build.applications.push(rec);
        build.application_index.insert(id, i);
        Ok(i)
    }

    fn intern_window(&self, build: &mut ManifestBuilder, id: i64) -> Result<u32> {
        if let Some(&i) = build.window_index.get(&id) {
            return Ok(i);
        }
        let (application, title, class_name): (i64, String, Option<String>) = self.conn.query_row(
            "SELECT application_id, title, class_name FROM windows WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let application = self.intern_application(build, application)?;
        let i = build.windows.len() as u32;
        build.windows.push(WindowRec {
            application,
            title,
            class_name,
        });
        build.window_index.insert(id, i);
        Ok(i)
    }

    // ----- import ------------------------------------------------------------------------------

    /// `(sequence, content hash)` of every segment this store has imported, ascending.
    pub fn imported_segments(&self) -> Result<Vec<(u64, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM settings WHERE key LIKE 'import.seq.%'")?;
        let mut out: Vec<(u64, String)> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .filter_map(|(key, hash)| {
                Some((key.strip_prefix(KEY_IMPORT_PREFIX)?.parse().ok()?, hash))
            })
            .collect();
        out.sort();
        Ok(out)
    }

    /// Merges a verified segment into this store, which must be the replica of the segment's
    /// source (see [`import_into_root`] for the usual entry point).
    ///
    /// Idempotent: importing the same segment again changes nothing and says so. The same
    /// `(source, seq)` with different content is refused as a conflict. All rows land in one
    /// transaction; image files are written first and never replace an existing file with
    /// different bytes, so a crash leaves at worst unreferenced images that a re-import adopts.
    pub fn import_segment(&self, segment: &Segment) -> Result<ImportOutcome> {
        let manifest = segment.manifest();
        let source_id = manifest.source.id.clone();
        self.adopt_replica_identity(&source_id)?;

        let key = format!("{KEY_IMPORT_PREFIX}{}", manifest.seq);
        if let Some(have) = self.setting(&key)? {
            return if have == segment.content_hash() {
                Ok(ImportOutcome::AlreadyImported {
                    source_id,
                    seq: manifest.seq,
                })
            } else {
                Err(StorageError::SegmentConflict {
                    source_id,
                    seq: manifest.seq,
                    have,
                    got: segment.content_hash().to_owned(),
                })
            };
        }

        let mut report = ImportReport {
            source_id,
            seq: manifest.seq,
            ..ImportReport::default()
        };

        // Monitors first, in their own small transaction: their ids name the image files.
        let seen_at = Timestamp(
            manifest
                .states
                .iter()
                .map(|s| s.captured_at)
                .max()
                .unwrap_or(0),
        );
        let monitors: Vec<MonitorId> = manifest
            .monitors
            .iter()
            .map(|m| {
                self.upsert_monitor(
                    &MonitorInfo {
                        device_name: m.device_name.clone(),
                        left: m.left,
                        top: m.top,
                        width: m.width,
                        height: m.height,
                        dpi: m.dpi,
                        primary: m.primary,
                    },
                    seen_at,
                )
            })
            .collect::<Result<_>>()?;

        // Images next (file-before-row). `None` = this state will not be imported.
        let mut placed: Vec<Option<(String, bool)>> = Vec::with_capacity(manifest.states.len());
        for (index, state) in manifest.states.iter().enumerate() {
            let relative = media_relative_path(
                Timestamp(state.captured_at),
                monitors[state.monitor as usize],
            );
            let known: Option<i64> = self
                .conn
                .query_row(
                    "SELECT id FROM visual_states WHERE media_path = ?1",
                    [&relative],
                    |row| row.get(0),
                )
                .optional()?;
            if known.is_some() {
                placed.push(Some((relative, true)));
                continue;
            }
            if self.is_fenced(state.captured_at, state.captured_at + 1)? {
                report.skipped_fenced += 1;
                placed.push(None);
                continue;
            }
            let bytes = segment.read_blob(index)?;
            if webp_dimensions(&bytes) != Some((state.width, state.height)) {
                report.skipped_invalid_image += 1;
                placed.push(None);
                continue;
            }
            place_media(self.data_dir(), &relative, &bytes)?;
            placed.push(Some((relative, false)));
        }

        let tx = self.immediate()?;
        let mut states: Vec<Option<VisualStateId>> = Vec::with_capacity(placed.len());
        for (state, slot) in manifest.states.iter().zip(&placed) {
            let Some((relative, known)) = slot else {
                states.push(None);
                continue;
            };
            if *known {
                let id: i64 = tx.query_row(
                    "SELECT id FROM visual_states WHERE media_path = ?1",
                    [relative],
                    |row| row.get(0),
                )?;
                report.states_reused += 1;
                states.push(Some(VisualStateId(id)));
                continue;
            }
            let new = NewVisualState {
                monitor: monitors[state.monitor as usize],
                captured_at: Timestamp(state.captured_at),
                media_path: relative.clone(),
                width: state.width,
                height: state.height,
                byte_size: state.blob_len,
                fingerprint: state.fingerprint,
                ocr_enabled: state.ocr_state == OcrState::Pending,
            };
            match insert_state_row(&tx, &new) {
                Ok(id) => {
                    match state.ocr_state {
                        OcrState::Done => {
                            let blocks: Vec<OcrBlock> = state
                                .ocr_blocks
                                .iter()
                                .map(|b| OcrBlock {
                                    text: b.text.clone(),
                                    x: b.x,
                                    y: b.y,
                                    width: b.width,
                                    height: b.height,
                                    confidence: b.confidence,
                                    line_index: b.line_index,
                                })
                                .collect();
                            write_ocr_rows(
                                &tx,
                                id,
                                &blocks,
                                state.ocr_engine.as_deref().unwrap_or("imported"),
                                state.ocr_ms.unwrap_or(0),
                            )?;
                        }
                        OcrState::Failed => {
                            tx.execute(
                                "UPDATE visual_states SET ocr_status = 'failed',
                                     ocr_error = 'failed at the source' WHERE id = ?1",
                                [id.0],
                            )?;
                        }
                        OcrState::Pending | OcrState::Skipped => {}
                    }
                    report.states_new += 1;
                    states.push(Some(id));
                }
                // A forget landed between the pre-check and now.
                Err(StorageError::Fenced { .. }) => {
                    report.skipped_fenced += 1;
                    states.push(None);
                }
                Err(e) => return Err(e),
            }
        }

        // Sessions, applications, windows.
        let mut sessions = Vec::with_capacity(manifest.sessions.len());
        for s in &manifest.sessions {
            let existing: Option<(i64, Option<i64>)> = tx
                .query_row(
                    "SELECT id, ended_at FROM sessions WHERE started_at = ?1 AND hostname = ?2",
                    params![s.started_at, s.hostname],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let id = match existing {
                Some((id, ended)) => {
                    if let Some(new_end) = s.ended_at {
                        let end = ended.map_or(new_end, |e| e.max(new_end));
                        tx.execute(
                            "UPDATE sessions SET ended_at = ?2 WHERE id = ?1",
                            params![id, end],
                        )?;
                    }
                    id
                }
                None => {
                    tx.execute(
                        "INSERT INTO sessions (started_at, ended_at, hostname, app_version)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![s.started_at, s.ended_at, s.hostname, s.app_version],
                    )?;
                    tx.last_insert_rowid()
                }
            };
            sessions.push(id);
        }
        let mut applications = Vec::with_capacity(manifest.applications.len());
        for a in &manifest.applications {
            let id: i64 = tx.query_row(
                "INSERT INTO applications (process_name, exe_path, first_seen_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT (process_name) DO UPDATE SET
                     exe_path = COALESCE(excluded.exe_path, applications.exe_path)
                 RETURNING id",
                params![a.process_name, a.exe_path, seen_at.0],
                |row| row.get(0),
            )?;
            applications.push(ApplicationId(id));
        }
        let mut windows = Vec::with_capacity(manifest.windows.len());
        for w in &manifest.windows {
            let id = upsert_window_row(
                &tx,
                applications[w.application as usize],
                &WindowContext {
                    title: w.title.clone(),
                    class_name: w.class_name.clone(),
                },
                seen_at,
            )?;
            windows.push(WindowId(id));
        }

        // Events.
        for event in &manifest.events {
            if EventKind::parse(&event.kind).is_none() {
                report.skipped_unknown_kind += 1;
                continue;
            }
            if overlaps_fence(&tx, event.started_at, event.ended_at)? {
                report.skipped_fenced += 1;
                continue;
            }
            let state = match event.state {
                Some(i) => match states[i as usize] {
                    Some(id) => Some(id.0),
                    None => {
                        report.skipped_dangling += 1;
                        continue;
                    }
                },
                None => None,
            };
            tx.execute(
                "INSERT INTO events
                     (session_id, kind, started_at, ended_at, monitor_id, visual_state_id,
                      application_id, window_id, metadata_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    sessions[event.session as usize],
                    event.kind,
                    event.started_at,
                    event.ended_at,
                    event.monitor.map(|i| monitors[i as usize].0),
                    state,
                    event.application.map(|i| applications[i as usize].0),
                    event.window.map(|i| windows[i as usize].0),
                    event.metadata_json
                ],
            )?;
            report.events += 1;
        }

        Self::set_setting(&tx, &key, segment.content_hash())?;
        // Display name only; the latest segment's label wins.
        let label: String = manifest.source.label.chars().take(120).collect();
        Self::set_setting(&tx, KEY_SOURCE_LABEL, &label)?;
        tx.commit()?;
        tracing::info!(
            seq = report.seq,
            events = report.events,
            states = report.states_new,
            "imported segment"
        );
        Ok(ImportOutcome::Imported(report))
    }

    fn is_fenced(&self, start: i64, end: i64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM deletion_fences WHERE since <= ?2 AND ?1 < until)",
            params![start, end],
            |row| row.get(0),
        )?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportStatus {
    pub next_seq: u64,
    /// Everything that ended at or before this time has been sealed (0 = never exported).
    pub cut_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxSegment {
    pub path: PathBuf,
    pub seq: u64,
    pub bytes: u64,
    /// Earliest start and latest end among the segment's events.
    pub first_ms: i64,
    pub last_ms: i64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct OutboxScan {
    pub segments: Vec<OutboxSegment>,
    /// `.rsseg` files that do not verify. Never touched automatically.
    pub unreadable: Vec<PathBuf>,
}

/// Verifies and summarises every `.rsseg` file in `dir` (a missing directory is an empty outbox).
pub fn scan_outbox(dir: &Path) -> Result<OutboxScan> {
    let mut scan = OutboxScan::default();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(scan),
        Err(e) => return Err(StorageError::io(dir, e)),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "rsseg"))
        .collect();
    paths.sort();
    for path in paths {
        match Segment::open(&path) {
            Ok(segment) => {
                let m = segment.manifest();
                scan.segments.push(OutboxSegment {
                    bytes: std::fs::metadata(&path).map_or(0, |meta| meta.len()),
                    seq: m.seq,
                    first_ms: m.events.iter().map(|e| e.started_at).min().unwrap_or(0),
                    last_ms: m.events.iter().map(|e| e.ended_at).max().unwrap_or(0),
                    path,
                });
            }
            Err(_) => scan.unreadable.push(path),
        }
    }
    Ok(scan)
}

/// Imports `segment` into the replica store for its source under `root/sources/<id>/`, creating
/// that store the first time the source is seen. The usual entry point for a central instance.
pub fn import_into_root(root: &DataDir, segment: &Segment) -> Result<ImportOutcome> {
    let id = &segment.manifest().source.id;
    let dir = root.source_dir(id).ok_or_else(|| {
        StorageError::InvalidArgument("source id is not a valid directory name".into())
    })?;
    let store = Store::open(&dir)?;
    store.import_segment(segment)
}

fn overlaps_fence(tx: &Transaction<'_>, start: i64, end: i64) -> Result<bool> {
    // A point event at `t` is inside a fence [since, until) when since <= t < until; a span is
    // refused if any part of it is.
    Ok(tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM deletion_fences WHERE since <= ?2 AND ?1 < until)",
        params![start, end],
        |row| row.get(0),
    )?)
}

/// Writes an image, accepting an identical file that is already there (a re-import after a crash)
/// and refusing a different one.
fn place_media(data: &DataDir, relative: &str, bytes: &[u8]) -> Result<()> {
    match write_webp_exclusive(data, relative, bytes) {
        Ok(_) => Ok(()),
        Err(StorageError::MediaExists(path)) => {
            let existing = std::fs::read(&path).map_err(|e| StorageError::io(&path, e))?;
            if existing == bytes {
                Ok(())
            } else {
                Err(StorageError::MediaConflict(path))
            }
        }
        Err(e) => Err(e),
    }
}

struct EventRow {
    id: i64,
    session: i64,
    kind: String,
    started_at: i64,
    ended_at: i64,
    monitor: Option<i64>,
    state: Option<i64>,
    application: Option<i64>,
    window: Option<i64>,
    metadata_json: Option<String>,
}

#[derive(Default)]
struct ManifestBuilder {
    sessions: Vec<SessionRec>,
    monitors: Vec<MonitorRec>,
    applications: Vec<ApplicationRec>,
    windows: Vec<WindowRec>,
    states: Vec<StateRec>,
    events: Vec<EventRec>,
    blob_paths: Vec<PathBuf>,
    session_index: BTreeMap<i64, u32>,
    monitor_index: BTreeMap<i64, u32>,
    application_index: BTreeMap<i64, u32>,
    window_index: BTreeMap<i64, u32>,
    state_index: BTreeMap<i64, u32>,
}
