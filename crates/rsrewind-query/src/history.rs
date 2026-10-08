//! The history facade: one read-only view over every store in a data folder.
//!
//! A data folder holds this machine's own store (`recall.db`) and, on a central instance, one
//! complete replica store per remote source under `sources/<id>/` (`docs/design/distributed.md`).
//! [`History`] opens all of them read-only and answers the questions a viewer asks — what
//! happened around a time, what is recent, where does this text appear, what exactly was on screen
//! in this moment, give me its picture — merging across sources by time (timeline) or rank
//! (search). Every result carries its [`SourceId`] (`None` for this machine).
//!
//! A caller never learns that sources are separate databases or that they arrived as segment
//! files, so the backend can change without touching the UI.
//!
//! Attribution is by construction: a replica is used only if its own `settings` say it is the
//! replica of exactly the id its directory is named after. Anything else is skipped and reported
//! in [`History::problems`], never guessed at.

use crate::{QueryDb, QueryError, Result, clamp_limit};
use rsrewind_core::{
    BgraFrame, DataDir, MediaPathError, SearchHit, SearchQuery, SourceId, TimelineCursor,
    TimelineEntry, Timestamp, VisualDetail, VisualStateId,
};
use std::cmp::Ordering;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Largest image file [`MediaReader::frame_bytes`] will read. Stored frames are lossy WebP of a
/// screen, typically well under a megabyte; anything this big is not one of ours.
pub const MAX_FRAME_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Largest image [`MediaReader::frame`] will decode, in pixels (about 256 MB of BGRA). Larger than
/// any real desktop (an 8K screen is 33 M pixels), small enough to bound memory.
pub const MAX_FRAME_PIXELS: u64 = 64 * 1024 * 1024;

const KEY_SOURCE_ID: &str = "source.id";
const KEY_SOURCE_ROLE: &str = "source.role";
const KEY_SOURCE_LABEL: &str = "source.label";

/// Which sources a question is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceFilter {
    #[default]
    All,
    /// This machine's own history.
    Local,
    Remote(SourceId),
}

impl SourceFilter {
    /// Exactly one source, as results report it (`None` = this machine).
    pub fn only(source: Option<SourceId>) -> Self {
        match source {
            None => Self::Local,
            Some(id) => Self::Remote(id),
        }
    }

    pub fn admits(&self, source: Option<SourceId>) -> bool {
        match self {
            Self::All => true,
            Self::Local => source.is_none(),
            Self::Remote(id) => source == Some(*id),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Recorded by this installation.
    Local,
    /// Imported from another installation.
    Replica,
}

/// One source of history in the data folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    /// `None` for this machine's own history.
    pub source: Option<SourceId>,
    pub kind: SourceKind,
    /// Display name (usually the probe's host name), with control characters removed. Never used
    /// for attribution.
    pub label: Option<String>,
    pub data_dir: PathBuf,
    /// Observations with a picture.
    pub observations: u64,
    pub first: Option<Timestamp>,
    pub last: Option<Timestamp>,
}

/// A store that exists but could not be used. The facade keeps working without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceProblem {
    pub data_dir: PathBuf,
    pub reason: String,
}

struct Lane {
    source: Option<SourceId>,
    kind: SourceKind,
    label: Option<String>,
    db: QueryDb,
}

/// Read-only view over every store under one data folder. See the module docs.
///
/// Holds one SQLite connection per source; it is `Send` but not `Sync`, so keep it on one thread
/// (a viewer runs it on a background worker) and hand [`History::media`] to other threads.
pub struct History {
    root: DataDir,
    lanes: Vec<Lane>,
    problems: Vec<SourceProblem>,
}

impl std::fmt::Debug for History {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("History")
            .field("root", &self.root)
            .field("sources", &self.lanes.len())
            .field("problems", &self.problems)
            .finish()
    }
}

impl History {
    /// Opens the local store (if there is one) and every valid replica. Never fails: a folder with
    /// no history is an empty history, and a store that cannot be used is reported in
    /// [`History::problems`].
    pub fn open(root: &DataDir) -> Self {
        let mut history = Self {
            root: root.clone(),
            lanes: Vec::new(),
            problems: Vec::new(),
        };
        if root.database().is_file() {
            match QueryDb::open(root).and_then(local_lane) {
                Ok(lane) => history.lanes.push(lane),
                Err(error) => history.problem(root.root(), error.to_string()),
            }
        }
        history.open_replicas();
        tracing::debug!(
            sources = history.lanes.len(),
            problems = history.problems.len(),
            "history opened"
        );
        history
    }

    fn open_replicas(&mut self) {
        let dir = self.root.sources_root();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => return self.problem(&dir, e.to_string()),
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink()))
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .collect();
        names.sort();
        for name in names {
            // Anything not named like a source id is not ours to interpret.
            let (Some(id), Some(data)) = (SourceId::parse(&name), self.root.source_dir(&name))
            else {
                continue;
            };
            match QueryDb::open(&data).and_then(|db| replica_lane(id, db)) {
                Ok(lane) if self.lanes.iter().any(|l| l.source == lane.source) => {
                    self.problem(data.root(), "a second store for the same source".into());
                }
                Ok(lane) => self.lanes.push(lane),
                Err(error) => self.problem(data.root(), error.to_string()),
            }
        }
    }

    fn problem(&mut self, dir: &Path, reason: String) {
        self.problems.push(SourceProblem {
            data_dir: dir.to_path_buf(),
            reason,
        });
    }

    pub fn data_dir(&self) -> &DataDir {
        &self.root
    }

    /// Stores that exist but were not opened, and why.
    pub fn problems(&self) -> &[SourceProblem] {
        &self.problems
    }

    /// Every usable source, this machine first, then replicas by id.
    pub fn sources(&self) -> Result<Vec<SourceInfo>> {
        self.lanes
            .iter()
            .map(|lane| {
                let (observations, first, last) = lane.db.summary()?;
                Ok(SourceInfo {
                    source: lane.source,
                    kind: lane.kind,
                    label: lane.label.clone(),
                    data_dir: lane.db.data_dir().root().to_path_buf(),
                    observations,
                    first,
                    last,
                })
            })
            .collect()
    }

    /// Observations, newest first across the selected sources. `after` is the cursor of the last
    /// entry already shown ([`TimelineEntry::cursor`]), or [`TimelineCursor::at_or_before`] to
    /// start at a time. The order is `(started_at, source, event_id)`, total across sources, so
    /// paging never skips or repeats an observation even when sources share timestamps and ids.
    pub fn recent(
        &self,
        filter: SourceFilter,
        limit: u32,
        after: Option<TimelineCursor>,
    ) -> Result<Vec<TimelineEntry>> {
        let limit = clamp_limit(limit);
        let mut entries = Vec::new();
        for lane in self.lanes(filter) {
            let bound = after.map(|cursor| project(cursor, lane.source));
            entries.extend(lane.db.recent_before(limit, bound)?);
        }
        entries.sort_by_key(|e| std::cmp::Reverse(e.cursor()));
        entries.truncate(limit as usize);
        Ok(entries)
    }

    /// Observations strictly after `after`, **oldest first**, across the selected sources: the
    /// forward direction of [`History::recent`], in the same total order (stepping to the next
    /// moment, where `recent` steps to the previous one).
    pub fn later(
        &self,
        filter: SourceFilter,
        limit: u32,
        after: TimelineCursor,
    ) -> Result<Vec<TimelineEntry>> {
        let limit = clamp_limit(limit);
        let mut entries = Vec::new();
        for lane in self.lanes(filter) {
            entries.extend(lane.db.later_than(limit, project(after, lane.source))?);
        }
        entries.sort_by_key(TimelineEntry::cursor);
        entries.truncate(limit as usize);
        Ok(entries)
    }

    /// The observation covering `at` (latest-started across sources if several do), otherwise the
    /// nearest one that started before it.
    pub fn at(&self, at: Timestamp, filter: SourceFilter) -> Result<Option<TimelineEntry>> {
        let mut covering: Option<TimelineEntry> = None;
        let mut nearest: Option<TimelineEntry> = None;
        for lane in self.lanes(filter) {
            let Some(entry) = lane.db.at(at)? else {
                continue;
            };
            let slot = if entry.started_at <= at && at <= entry.ended_at {
                &mut covering
            } else {
                &mut nearest
            };
            if slot
                .as_ref()
                .is_none_or(|best| entry.cursor() > best.cursor())
            {
                *slot = Some(entry);
            }
        }
        Ok(covering.or(nearest))
    }

    /// Full-text search across the selected sources: best rank first, then most recent. Each
    /// hit's `timestamp` is the earliest *matching* observation, which is the moment to cue to.
    ///
    /// bm25 ranks come from separate indexes, so across sources they are comparable in shape, not
    /// exactly; with no search text every rank is 0 and the order is simply newest first.
    pub fn search(&self, query: &SearchQuery, filter: SourceFilter) -> Result<Vec<SearchHit>> {
        let mut hits = Vec::new();
        for lane in self.lanes(filter) {
            hits.extend(lane.db.search(query)?);
        }
        hits.sort_by(|a, b| {
            a.rank
                .total_cmp(&b.rank)
                .then_with(|| b.timestamp.cmp(&a.timestamp))
                .then_with(|| b.source.cmp(&a.source))
                .then_with(|| b.visual_state_id.cmp(&a.visual_state_id))
        });
        hits.truncate(clamp_limit(query.limit) as usize);
        Ok(hits)
    }

    /// Everything about one visual state of one source, as a single consistent snapshot.
    pub fn visual_detail(
        &self,
        source: Option<SourceId>,
        id: VisualStateId,
    ) -> Result<Option<VisualDetail>> {
        self.lane(source)?.db.visual_detail(id)
    }

    /// A thread-safe reader for the pictures behind results (no database access).
    pub fn media(&self) -> MediaReader {
        MediaReader {
            roots: Arc::new(
                self.lanes
                    .iter()
                    .map(|lane| (lane.source, lane.db.data_dir().clone()))
                    .collect(),
            ),
        }
    }

    /// Shorthand for [`MediaReader::frame_bytes`].
    pub fn frame_bytes(&self, source: Option<SourceId>, media_path: &str) -> Result<Vec<u8>> {
        self.media().frame_bytes(source, media_path)
    }

    fn lanes(&self, filter: SourceFilter) -> impl Iterator<Item = &Lane> {
        self.lanes.iter().filter(move |l| filter.admits(l.source))
    }

    fn lane(&self, source: Option<SourceId>) -> Result<&Lane> {
        self.lanes
            .iter()
            .find(|l| l.source == source)
            .ok_or_else(|| unknown(source))
    }
}

fn unknown(source: Option<SourceId>) -> QueryError {
    QueryError::UnknownSource(source.map_or_else(|| "(this machine)".into(), |s| s.to_string()))
}

/// The data folder itself is normally this machine's recorder. If the caller pointed it at a
/// replica directly (`--data-dir <root>/sources/<id>`), attribute results to that source.
fn local_lane(mut db: QueryDb) -> Result<Lane> {
    let replica_of = match db.setting(KEY_SOURCE_ROLE)?.as_deref() {
        Some("replica") => db
            .setting(KEY_SOURCE_ID)?
            .as_deref()
            .and_then(SourceId::parse),
        _ => None,
    };
    Ok(match replica_of {
        Some(id) => {
            let label = clean_label(db.setting(KEY_SOURCE_LABEL)?);
            db.attribute_to(Some(id));
            Lane {
                source: Some(id),
                kind: SourceKind::Replica,
                label,
                db,
            }
        }
        None => Lane {
            source: None,
            kind: SourceKind::Local,
            label: None,
            db,
        },
    })
}

fn replica_lane(id: SourceId, mut db: QueryDb) -> Result<Lane> {
    let stored = db.setting(KEY_SOURCE_ID)?;
    let role = db.setting(KEY_SOURCE_ROLE)?;
    if stored.as_deref().and_then(SourceId::parse) != Some(id) || role.as_deref() != Some("replica")
    {
        return Err(QueryError::UnknownSource(format!(
            "{id}: the store in this folder is not the replica of {id}; it was skipped"
        )));
    }
    let label = clean_label(db.setting(KEY_SOURCE_LABEL)?);
    db.attribute_to(Some(id));
    Ok(Lane {
        source: Some(id),
        kind: SourceKind::Replica,
        label,
        db,
    })
}

/// A remote label is remote-influenced display text: drop control characters (terminal escapes,
/// bidi overrides are left to the renderer) and bound its length.
fn clean_label(label: Option<String>) -> Option<String> {
    label
        .map(|l| {
            l.chars()
                .filter(|c| !c.is_control())
                .take(120)
                .collect::<String>()
        })
        .filter(|l| !l.trim().is_empty())
}

/// The per-store bound equivalent to `cursor` in the cross-source order
/// `(started_at, source, event_id)`: a candidate is below the cursor exactly when its
/// `(started_at, event_id) < bound`, and above it exactly when `> bound` (used for both paging
/// directions).
fn project(cursor: TimelineCursor, lane: Option<SourceId>) -> (i64, i64) {
    let t = cursor.started_at.0;
    match lane.cmp(&cursor.source) {
        // Everything this lane has at `t` sorts below the cursor.
        Ordering::Less => (t, i64::MAX),
        // Nothing this lane has at `t` does.
        Ordering::Greater => (t, i64::MIN),
        Ordering::Equal => (t, cursor.event_id.0),
    }
}

/// Reads the stored pictures behind results. Cheap to clone, `Send + Sync`, and touches no
/// database, so a viewer can decode frames on other threads while queries run.
#[derive(Clone, Debug)]
pub struct MediaReader {
    roots: Arc<Vec<(Option<SourceId>, DataDir)>>,
}

impl MediaReader {
    /// The bytes of the image a result's `media_path` names, for the source that reported it.
    ///
    /// Only files inside that source's own `media/` tree are readable, by the same rules the
    /// writer uses (no `..`, no backslashes, no links below the data folder); a path that belongs
    /// to another source, or to nothing, is refused.
    pub fn frame_bytes(&self, source: Option<SourceId>, media_path: &str) -> Result<Vec<u8>> {
        let data = self
            .roots
            .iter()
            .find(|(s, _)| *s == source)
            .map(|(_, d)| d)
            .ok_or_else(|| unknown(source))?;
        let unsafe_path = || QueryError::MediaPath(MediaPathError::Unsafe(media_path.to_owned()));
        let below = Path::new(media_path)
            .strip_prefix(data.root())
            .map_err(|_| unsafe_path())?;
        let mut parts = Vec::new();
        for component in below.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_str().ok_or_else(unsafe_path)?),
                _ => return Err(unsafe_path()),
            }
        }
        let path = data.resolve_media_checked(&parts.join("/"))?;
        let io = |e| QueryError::Io {
            path: path.clone(),
            source: e,
        };
        let file = std::fs::File::open(&path).map_err(io)?;
        let len = file.metadata().map_err(io)?.len();
        if len > MAX_FRAME_FILE_BYTES {
            return Err(QueryError::Image(format!(
                "{len} bytes is larger than any stored frame"
            )));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
        file.take(MAX_FRAME_FILE_BYTES)
            .read_to_end(&mut bytes)
            .map_err(io)?;
        Ok(bytes)
    }

    /// [`MediaReader::frame_bytes`], decoded to a packed BGRA frame.
    pub fn frame(&self, source: Option<SourceId>, media_path: &str) -> Result<BgraFrame> {
        decode_webp(&self.frame_bytes(source, media_path)?)
    }
}

/// Decodes a stored still WebP into a packed BGRA frame (alpha 255 when the image has none),
/// refusing animations and anything over [`MAX_FRAME_PIXELS`] before allocating for it.
pub fn decode_webp(bytes: &[u8]) -> Result<BgraFrame> {
    let features = webp::BitstreamFeatures::new(bytes)
        .ok_or_else(|| QueryError::Image("not a WebP image".into()))?;
    if features.has_animation() {
        return Err(QueryError::Image(
            "animated WebP is not a stored frame".into(),
        ));
    }
    let pixels = u64::from(features.width()) * u64::from(features.height());
    if pixels == 0 || pixels > MAX_FRAME_PIXELS {
        return Err(QueryError::Image(format!(
            "{}x{} is not a plausible screen",
            features.width(),
            features.height()
        )));
    }
    let image = webp::Decoder::new(bytes)
        .decode()
        .ok_or_else(|| QueryError::Image("not a decodable still WebP image".into()))?;
    let (width, height) = (image.width(), image.height());
    let source: &[u8] = &image;
    let channels = if image.is_alpha() { 4 } else { 3 };
    let count = width as usize * height as usize;
    if source.len() < count * channels {
        return Err(QueryError::Image("decoded WebP buffer is short".into()));
    }
    let mut out = Vec::with_capacity(count * 4);
    for px in source.chunks_exact(channels).take(count) {
        let alpha = if channels == 4 { px[3] } else { 255 };
        out.extend_from_slice(&[px[2], px[1], px[0], alpha]);
    }
    Ok(BgraFrame {
        width,
        height,
        stride: width * 4,
        pixels: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsrewind_core::EventId;

    #[test]
    fn projection_matches_the_cross_source_order()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let a = SourceId::parse("0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").ok_or("a")?;
        let b = SourceId::parse("0bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").ok_or("b")?;
        let lanes = [None, Some(a), Some(b)];
        let cursor = TimelineCursor {
            started_at: Timestamp(100),
            event_id: EventId(5),
            source: Some(a),
        };
        // Brute force: for every candidate (t, lane, id), "below the cursor in the global order"
        // must equal "(t, id) < projected bound in that lane".
        for lane in lanes {
            let bound = project(cursor, lane);
            for t in 98..=102 {
                for id in 1..=9 {
                    let candidate = TimelineCursor {
                        started_at: Timestamp(t),
                        event_id: EventId(id),
                        source: lane,
                    };
                    assert_eq!(
                        candidate < cursor,
                        (t, id) < bound,
                        "lane {lane:?} t {t} id {id}"
                    );
                    assert_eq!(
                        candidate > cursor,
                        (t, id) > bound,
                        "forward: lane {lane:?} t {t} id {id}"
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn labels_lose_control_characters() {
        assert_eq!(
            clean_label(Some("FRONT\u{1b}[31m-DESK\n".into())).as_deref(),
            Some("FRONT[31m-DESK")
        );
        assert_eq!(clean_label(Some("\u{7}\u{7}".into())), None);
        assert_eq!(
            clean_label(Some("x".repeat(500))).map(|l| l.len()),
            Some(120)
        );
    }

    #[test]
    fn decode_refuses_garbage() {
        assert!(decode_webp(b"not an image").is_err());
        assert!(decode_webp(&[]).is_err());
    }

    #[test]
    fn filters_admit_the_right_sources() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let a = SourceId::parse("0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").ok_or("a")?;
        assert!(SourceFilter::All.admits(None) && SourceFilter::All.admits(Some(a)));
        assert!(SourceFilter::Local.admits(None) && !SourceFilter::Local.admits(Some(a)));
        assert!(SourceFilter::only(Some(a)).admits(Some(a)));
        assert!(!SourceFilter::only(Some(a)).admits(None));
        Ok(())
    }
}
