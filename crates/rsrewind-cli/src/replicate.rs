//! `export`, `import` and `sources`: moving sealed history between rsRewind installations.
//!
//! Nothing here touches a network. A segment is a file; how it travels is the operator's choice.

use anyhow::{Context, Result, bail};
use rsrewind_core::{DataDir, Timestamp};
use rsrewind_segment::Segment;
use rsrewind_storage::{
    ExportOptions, ImportOutcome, MIN_SETTLE_MS, SourceRole, Store, import_into_root, scan_outbox,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// What this build's recorder can report about each moment. Receivers must not assume more.
#[cfg(windows)]
const CAPABILITIES: &[&str] = &["ocr", "window_titles", "process_names", "multi_monitor"];
#[cfg(not(windows))]
const CAPABILITIES: &[&str] = &[];

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default()
}

#[derive(Serialize)]
struct ExportedJson {
    seq: u64,
    file: String,
    events: usize,
    states: usize,
    bytes: u64,
    sha256: String,
    skipped_missing_media: usize,
}

pub fn export(
    data: &DataDir,
    out: Option<PathBuf>,
    settle_minutes: u32,
    max_events: usize,
    label: Option<String>,
    json: bool,
) -> Result<ExitCode> {
    let settle_ms = i64::from(settle_minutes) * 60_000;
    if settle_ms < MIN_SETTLE_MS {
        bail!("--settle-minutes must be at least 1");
    }
    let store = Store::open_existing(data).with_context(|| {
        format!(
            "no rsRewind history in {} yet (start recording with `rsrewind start`)",
            data.root().display()
        )
    })?;
    let out = out.unwrap_or_else(|| data.outbox());
    let label = label.unwrap_or_else(hostname);
    let mut sealed = Vec::new();
    // A long offline stretch is several segments; keep sealing until nothing new has settled.
    while let Some(report) = store.export_segment(
        &out,
        &ExportOptions {
            label: &label,
            platform: std::env::consts::OS,
            app_version: env!("CARGO_PKG_VERSION"),
            capabilities: CAPABILITIES,
            now: Timestamp::now(),
            settle_ms,
            max_events,
        },
    )? {
        sealed.push(report);
    }
    if json {
        let rows: Vec<ExportedJson> = sealed
            .iter()
            .map(|r| ExportedJson {
                seq: r.seq,
                file: r.path.display().to_string(),
                events: r.events,
                states: r.states,
                bytes: r.bytes,
                sha256: r.content_hash.clone(),
                skipped_missing_media: r.skipped_missing_media,
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if sealed.is_empty() {
        println!("Nothing new has settled since the last export.");
    } else {
        for r in &sealed {
            println!(
                "Sealed segment {}: {} moments, {} screenshots, {} -> {}",
                r.seq,
                r.events,
                r.states,
                super::human_bytes(r.bytes),
                r.path.display()
            );
            if r.skipped_missing_media > 0 {
                println!(
                    "  {} moment(s) skipped because their screenshot is missing",
                    r.skipped_missing_media
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(Serialize)]
struct ImportedJson {
    file: String,
    result: &'static str,
    source: Option<String>,
    seq: Option<u64>,
    events: usize,
    screenshots: usize,
    skipped: usize,
    error: Option<String>,
}

pub fn import(data: &DataDir, paths: &[PathBuf], json: bool) -> Result<ExitCode> {
    let mut files = Vec::new();
    for path in paths {
        collect(path, &mut files)?;
    }
    let mut rows = Vec::new();
    let mut failed = false;
    for file in &files {
        let row = match Segment::open(file)
            .map_err(anyhow::Error::from)
            .and_then(|segment| Ok(import_into_root(data, &segment)?))
        {
            Ok(ImportOutcome::Imported(r)) => ImportedJson {
                file: file.display().to_string(),
                result: "imported",
                source: Some(r.source_id),
                seq: Some(r.seq),
                events: r.events,
                screenshots: r.states_new,
                skipped: r.skipped_fenced
                    + r.skipped_invalid_image
                    + r.skipped_unknown_kind
                    + r.skipped_dangling,
                error: None,
            },
            Ok(ImportOutcome::AlreadyImported { source_id, seq }) => ImportedJson {
                file: file.display().to_string(),
                result: "already imported",
                source: Some(source_id),
                seq: Some(seq),
                events: 0,
                screenshots: 0,
                skipped: 0,
                error: None,
            },
            Err(error) => {
                failed = true;
                ImportedJson {
                    file: file.display().to_string(),
                    result: "refused",
                    source: None,
                    seq: None,
                    events: 0,
                    screenshots: 0,
                    skipped: 0,
                    error: Some(format!("{error:#}")),
                }
            }
        };
        rows.push(row);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        for r in &rows {
            match &r.error {
                Some(error) => eprintln!("{}: refused: {error}", r.file),
                None => println!(
                    "{}: {} (source {}, segment {}): {} moments, {} screenshots{}",
                    r.file,
                    r.result,
                    r.source.as_deref().map_or("?", |s| s.get(..8).unwrap_or(s)),
                    r.seq.unwrap_or(0),
                    r.events,
                    r.screenshots,
                    if r.skipped > 0 {
                        format!(", {} skipped", r.skipped)
                    } else {
                        String::new()
                    }
                ),
            }
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// A file is taken as given; a directory contributes its `.rsseg` files in name (= sequence)
/// order. Partial uploads (`.partial`, or anything that fails verification) are never picked up
/// silently: verification refuses them with a message.
fn collect(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_dir() {
        let mut found: Vec<PathBuf> = std::fs::read_dir(path)
            .with_context(|| format!("reading {}", path.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "rsseg"))
            .collect();
        found.sort();
        out.extend(found);
    } else {
        out.push(path.to_path_buf());
    }
    Ok(())
}

#[derive(Serialize)]
struct SourceJson {
    id: String,
    label: Option<String>,
    segments: usize,
    last_segment: Option<u64>,
    data_dir: String,
}

pub fn sources(data: &DataDir, json: bool) -> Result<ExitCode> {
    let mut rows = Vec::new();
    if let Ok(entries) = std::fs::read_dir(data.sources_root()) {
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .collect();
        names.sort();
        for name in names {
            let Some(dir) = data.source_dir(&name) else {
                continue;
            };
            let store = Store::open_existing(&dir)?;
            let identity = store.source_identity()?;
            if identity.role != SourceRole::Replica {
                continue;
            }
            let imported = store.imported_segments()?;
            rows.push(SourceJson {
                id: identity.id,
                label: store.source_label()?,
                segments: imported.len(),
                last_segment: imported.last().map(|(seq, _)| *seq),
                data_dir: dir.root().display().to_string(),
            });
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if rows.is_empty() {
        println!("No remote sources have been imported here.");
    } else {
        for r in &rows {
            println!(
                "{}  {}  {} segment(s), latest {}  {}",
                r.id,
                crate::render::plain(r.label.as_deref().unwrap_or("(unnamed)")),
                r.segments,
                r.last_segment.map_or("-".into(), |s| s.to_string()),
                r.data_dir
            );
        }
        println!("Browse one with: rsrewind --data-dir <data_dir> recent   (or search)");
    }
    Ok(ExitCode::SUCCESS)
}

/// After a `forget`: remove sealed segments in the outbox that cover the forgotten range, and say
/// plainly that copies already delivered elsewhere are out of reach. A segment is immutable, so
/// one that overlaps the range is deleted whole (it was never delivered unless you moved it).
pub fn forget_exported(data: &DataDir, since: Timestamp, until: Timestamp, already_sealed: u64) {
    let scan = match scan_outbox(&data.outbox()) {
        Ok(scan) => scan,
        Err(error) => {
            eprintln!("could not inspect the outbox: {error}");
            return;
        }
    };
    let mut removed = 0usize;
    for segment in &scan.segments {
        if segment.last_ms >= since.0 && segment.first_ms < until.0 {
            match std::fs::remove_file(&segment.path) {
                Ok(()) => removed += 1,
                Err(error) => eprintln!("could not delete {}: {error}", segment.path.display()),
            }
        }
    }
    if removed > 0 {
        println!(
            "Also deleted {removed} sealed segment file(s) in the outbox that covered this time."
        );
    }
    for path in &scan.unreadable {
        eprintln!(
            "Note: {} does not verify as a segment, so it was not inspected. Delete it yourself if it may hold this history.",
            path.display()
        );
    }
    if already_sealed > 0 {
        println!(
            "Note: {already_sealed} of the deleted moments had already been sealed into segments. Copies that were moved to another machine are not touched; delete them there."
        );
    }
}
