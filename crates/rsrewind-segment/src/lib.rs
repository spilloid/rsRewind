//! The sealed history segment: rsRewind's unit of replication.
//!
//! A segment is one immutable file holding a closed slice of one source's history: events, the
//! sessions/monitors/applications/windows they mention, and the screenshots and OCR text attached
//! to them. It carries **no database row ids** and no absolute paths: everything inside refers to
//! everything else by index into the segment's own tables, so a segment can be imported into any
//! store without id collisions and without leaking the origin's database layout.
//!
//! ```text
//! "RSSEG001"                      8 bytes   magic + format generation
//! manifest_len                    u64 LE
//! manifest                        UTF-8 JSON ([`Manifest`])
//! blob 0, blob 1, ...             WebP images, in `states` order; lengths come from the manifest
//! sha256                          32 bytes  over every byte above
//! "RSSEGEND"                      8 bytes
//! ```
//!
//! The trailing digest makes a truncated or corrupted file *detectable* (a partial upload never
//! verifies) and doubles as the segment's content identity. It is an integrity check, not
//! authentication: anyone who can write a file can compute it. See `docs/design/distributed.md`
//! for what is and is not defended at this layer.
//!
//! This crate is pure format: no SQLite, no network, no Windows.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const MAGIC: &[u8; 8] = b"RSSEG001";
pub const END_MAGIC: &[u8; 8] = b"RSSEGEND";
/// Manifest schema version. Readers refuse a higher number rather than guessing.
pub const FORMAT_VERSION: u32 = 1;
/// Upper bound on the manifest, so a corrupt length cannot make a reader allocate gigabytes.
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024 * 1024;
/// Upper bound on one image blob.
pub const MAX_BLOB_BYTES: u64 = 256 * 1024 * 1024;

const TRAILER_LEN: u64 = 32 + 8;

pub type Result<T, E = SegmentError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum SegmentError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("not an rsRewind segment (bad magic): {0}")]
    BadMagic(PathBuf),
    #[error("segment is truncated or incomplete: {0}")]
    Truncated(PathBuf),
    #[error("segment digest mismatch (corrupt or tampered): {0}")]
    DigestMismatch(PathBuf),
    #[error("segment manifest is invalid: {0}")]
    Manifest(String),
    #[error("segment format version {found} is newer than this build supports ({supported})")]
    UnsupportedVersion { found: u32, supported: u32 },
    #[error("refusing to overwrite existing segment file {0}")]
    Exists(PathBuf),
    #[error("blob {index} was {actual} bytes but the manifest declared {declared}")]
    BlobLength {
        index: usize,
        declared: u64,
        actual: u64,
    },
}

impl SegmentError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

// ----- manifest -------------------------------------------------------------------------------

/// Who produced the segment. `id` is the only thing that attributes history to an endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// 32 lowercase hex characters, random, minted once per installation (see
    /// [`is_valid_source_id`]).
    pub id: String,
    /// Human-readable name for display only. Never used for attribution.
    pub label: String,
    /// `windows`, `macos`, `linux`, ... Informational.
    pub platform: String,
    pub app_version: String,
    /// What this source's recorder can actually provide, e.g. `window_titles`, `process_names`,
    /// `ocr`. A consumer must not assume absent metadata means "nothing was happening".
    pub capabilities: Vec<String>,
}

/// Where the exporter's watermark stood after this segment. Lets a crashed exporter resume from
/// the last segment on disk without ever re-sealing the same history under a new sequence number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportCursor {
    pub cut_ms: i64,
    pub max_event_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRec {
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub hostname: String,
    pub app_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorRec {
    pub device_name: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationRec {
    pub process_name: String,
    pub exe_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRec {
    /// Index into `applications`.
    pub application: u32,
    pub title: String,
    pub class_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrRec {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub confidence: Option<f32>,
    pub line_index: u32,
}

/// `pending` means "no usable text yet": the receiver may run its own OCR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OcrState {
    Pending,
    Done,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateRec {
    /// Index into `monitors`.
    pub monitor: u32,
    pub captured_at: i64,
    pub width: u32,
    pub height: u32,
    pub fingerprint: Option<u64>,
    /// Length of this state's WebP image in the blob region. Offsets are implied by order.
    pub blob_len: u64,
    pub ocr_state: OcrState,
    pub ocr_engine: Option<String>,
    pub ocr_ms: Option<u64>,
    /// Present only when `ocr_state` is `done`.
    pub ocr_blocks: Vec<OcrRec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRec {
    /// Stable lowercase kind string (`EventKind::as_str`). Kept as text so a receiver that does
    /// not know a newer kind can skip it instead of failing the whole segment.
    pub kind: String,
    pub started_at: i64,
    pub ended_at: i64,
    /// Index into `sessions`.
    pub session: u32,
    pub monitor: Option<u32>,
    /// Index into `states`.
    pub state: Option<u32>,
    pub application: Option<u32>,
    pub window: Option<u32>,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub source: SourceInfo,
    /// Per-source, strictly increasing, gap-free from 1. `(source.id, seq)` names a segment.
    pub seq: u64,
    pub created_at: i64,
    pub cursor: ExportCursor,
    pub sessions: Vec<SessionRec>,
    pub monitors: Vec<MonitorRec>,
    pub applications: Vec<ApplicationRec>,
    pub windows: Vec<WindowRec>,
    pub states: Vec<StateRec>,
    pub events: Vec<EventRec>,
}

/// A source id is exactly 32 lowercase hex characters. It becomes a directory name on the
/// receiver, so this check is also a path-safety check.
pub fn is_valid_source_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Manifest {
    /// Structural validation: every index in range, sane lengths, ordered times. A manifest that
    /// passes can be imported without bounds checks failing halfway through.
    pub fn validate(&self) -> Result<()> {
        let bad = |what: String| Err(SegmentError::Manifest(what));
        if self.format > FORMAT_VERSION {
            return Err(SegmentError::UnsupportedVersion {
                found: self.format,
                supported: FORMAT_VERSION,
            });
        }
        if self.format == 0 {
            return bad("format version 0".into());
        }
        if !is_valid_source_id(&self.source.id) {
            return bad("source id is not 32 lowercase hex characters".into());
        }
        if self.seq == 0 {
            return bad("sequence numbers start at 1".into());
        }
        for (i, w) in self.windows.iter().enumerate() {
            if w.application as usize >= self.applications.len() {
                return bad(format!("window {i} names a missing application"));
            }
        }
        for (i, s) in self.states.iter().enumerate() {
            if s.monitor as usize >= self.monitors.len() {
                return bad(format!("state {i} names a missing monitor"));
            }
            if s.blob_len == 0 || s.blob_len > MAX_BLOB_BYTES {
                return bad(format!("state {i} has an invalid image length"));
            }
            if s.ocr_state != OcrState::Done && !s.ocr_blocks.is_empty() {
                return bad(format!("state {i} has OCR blocks but is not `done`"));
            }
        }
        let in_range = |idx: Option<u32>, len: usize| idx.is_none_or(|i| (i as usize) < len);
        for (i, e) in self.events.iter().enumerate() {
            if e.ended_at < e.started_at {
                return bad(format!("event {i} ends before it starts"));
            }
            if e.session as usize >= self.sessions.len()
                || !in_range(e.monitor, self.monitors.len())
                || !in_range(e.state, self.states.len())
                || !in_range(e.application, self.applications.len())
                || !in_range(e.window, self.windows.len())
            {
                return bad(format!("event {i} has an out-of-range reference"));
            }
        }
        Ok(())
    }

    fn blob_region_len(&self) -> u64 {
        self.states.iter().map(|s| s.blob_len).sum()
    }
}

// ----- writing --------------------------------------------------------------------------------

/// What a finished write produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub path: PathBuf,
    /// Hex SHA-256 of the file's content: the segment's content identity.
    pub content_hash: String,
    pub bytes: u64,
}

struct HashingWriter<W: Write> {
    inner: W,
    hasher: Sha256,
    written: u64,
}

impl<W: Write> HashingWriter<W> {
    fn put(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.hasher.update(bytes);
        self.written += bytes.len() as u64;
        self.inner.write_all(bytes)
    }
}

/// Writes `manifest` and the blobs it declares to `destination`.
///
/// `blob(i)` supplies the i-th image's bytes; its length must equal `states[i].blob_len`. The
/// file is built under a temporary name, flushed to disk, then published without replacing
/// anything: an existing `destination` is an error, so a re-export can never silently swap the
/// bytes behind a segment name a receiver may already have seen.
pub fn write_segment(
    destination: &Path,
    manifest: &Manifest,
    mut blob: impl FnMut(usize) -> std::io::Result<Vec<u8>>,
) -> Result<Written> {
    manifest.validate()?;
    if destination.exists() {
        return Err(SegmentError::Exists(destination.to_path_buf()));
    }
    let manifest_bytes =
        serde_json::to_vec(manifest).map_err(|e| SegmentError::Manifest(e.to_string()))?;
    if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(SegmentError::Manifest("manifest too large".into()));
    }

    let temp = destination.with_extension("rsseg.partial");
    let result = (|| {
        let file = File::create(&temp).map_err(|e| SegmentError::io(&temp, e))?;
        let mut out = HashingWriter {
            inner: BufWriter::new(file),
            hasher: Sha256::new(),
            written: 0,
        };
        let io = |e| SegmentError::io(&temp, e);
        out.put(MAGIC).map_err(io)?;
        out.put(&(manifest_bytes.len() as u64).to_le_bytes())
            .map_err(io)?;
        out.put(&manifest_bytes).map_err(io)?;
        for (index, state) in manifest.states.iter().enumerate() {
            let bytes = blob(index).map_err(io)?;
            if bytes.len() as u64 != state.blob_len {
                return Err(SegmentError::BlobLength {
                    index,
                    declared: state.blob_len,
                    actual: bytes.len() as u64,
                });
            }
            out.put(&bytes).map_err(io)?;
        }
        let digest = out.hasher.clone().finalize();
        out.inner.write_all(&digest).map_err(io)?;
        out.inner.write_all(END_MAGIC).map_err(io)?;
        let bytes = out.written + TRAILER_LEN;
        let file = out.inner.into_inner().map_err(|e| io(e.into_error()))?;
        file.sync_all().map_err(io)?;
        Ok((hex(&digest), bytes))
    })();

    let (content_hash, bytes) = match result {
        Ok(done) => done,
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            return Err(e);
        }
    };
    // Hard-link-then-unlink is the portable "rename without replace".
    let published = std::fs::hard_link(&temp, destination);
    let _ = std::fs::remove_file(&temp);
    published.map_err(|e| {
        if destination.exists() {
            SegmentError::Exists(destination.to_path_buf())
        } else {
            SegmentError::io(destination, e)
        }
    })?;
    Ok(Written {
        path: destination.to_path_buf(),
        content_hash,
        bytes,
    })
}

// ----- reading --------------------------------------------------------------------------------

/// A verified segment on disk. Opening one reads the whole file once to check its digest, then
/// serves blobs by seeking; memory use is the manifest plus one image at a time.
#[derive(Debug)]
pub struct Segment {
    path: PathBuf,
    manifest: Manifest,
    content_hash: String,
    blob_start: u64,
}

impl Segment {
    pub fn open(path: &Path) -> Result<Self> {
        let io = |e| SegmentError::io(path, e);
        let file = File::open(path).map_err(io)?;
        let total = file.metadata().map_err(io)?.len();
        if total < (MAGIC.len() as u64 + 8 + TRAILER_LEN) {
            return Err(SegmentError::Truncated(path.to_path_buf()));
        }
        let mut reader = BufReader::new(file);

        let mut head = [0u8; 8];
        reader.read_exact(&mut head).map_err(io)?;
        if &head != MAGIC {
            return Err(SegmentError::BadMagic(path.to_path_buf()));
        }
        let mut len = [0u8; 8];
        reader.read_exact(&mut len).map_err(io)?;
        let manifest_len = u64::from_le_bytes(len);
        let blob_start = 16 + manifest_len;
        if manifest_len > MAX_MANIFEST_BYTES || blob_start + TRAILER_LEN > total {
            return Err(SegmentError::Truncated(path.to_path_buf()));
        }

        // Verify the digest over everything before the trailer, hashing as we go.
        let mut hasher = Sha256::new();
        hasher.update(head);
        hasher.update(len);
        let mut manifest_bytes = vec![0u8; manifest_len as usize];
        reader.read_exact(&mut manifest_bytes).map_err(io)?;
        hasher.update(&manifest_bytes);
        let mut remaining = total - blob_start - TRAILER_LEN;
        let mut buf = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let take = remaining.min(buf.len() as u64) as usize;
            reader.read_exact(&mut buf[..take]).map_err(io)?;
            hasher.update(&buf[..take]);
            remaining -= take as u64;
        }
        let mut trailer = [0u8; 40];
        reader.read_exact(&mut trailer).map_err(io)?;
        let digest = hasher.finalize();
        if &trailer[32..] != END_MAGIC {
            return Err(SegmentError::Truncated(path.to_path_buf()));
        }
        if trailer[..32] != digest[..] {
            return Err(SegmentError::DigestMismatch(path.to_path_buf()));
        }

        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| SegmentError::Manifest(e.to_string()))?;
        manifest.validate()?;
        if blob_start + manifest.blob_region_len() + TRAILER_LEN != total {
            return Err(SegmentError::Manifest(
                "blob lengths do not add up to the file size".into(),
            ));
        }
        Ok(Self {
            path: path.to_path_buf(),
            manifest,
            content_hash: hex(&digest),
            blob_start,
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Hex SHA-256 of the whole file.
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    /// The i-th state's WebP bytes.
    pub fn read_blob(&self, index: usize) -> Result<Vec<u8>> {
        let io = |e| SegmentError::io(&self.path, e);
        let state = self
            .manifest
            .states
            .get(index)
            .ok_or_else(|| SegmentError::Manifest(format!("no state {index}")))?;
        let offset = self.blob_start
            + self.manifest.states[..index]
                .iter()
                .map(|s| s.blob_len)
                .sum::<u64>();
        let mut file = File::open(&self.path).map_err(io)?;
        file.seek(SeekFrom::Start(offset)).map_err(io)?;
        let mut bytes = vec![0u8; state.blob_len as usize];
        file.read_exact(&mut bytes).map_err(io)?;
        Ok(bytes)
    }
}

/// `seg-<first 8 of source id>-<seq, 10 digits>.rsseg`. Sorts by sequence number; the file name
/// is a convenience for humans and tools, never trusted over the manifest.
pub fn file_name(source_id: &str, seq: u64) -> String {
    let short = source_id.get(..8).unwrap_or(source_id);
    format!("seg-{short}-{seq:010}.rsseg")
}

/// Parses `seq` back out of a name produced by [`file_name`].
pub fn seq_from_file_name(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".rsseg")?;
    stem.rsplit('-').next()?.parse().ok()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests;
