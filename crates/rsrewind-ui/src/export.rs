//! Exports of selected moments: one picture (clipboard, PNG) or a silent clip (GIF, and WebM when an
//! `ffmpeg` executable is on PATH). See `docs/design/marks-keyframes.md` §"Exports".
//!
//! Everything here runs on an export thread of the window's own process, never on the UI thread and
//! never in the recorder: pictures are read through `rsrewind-query`'s [`MediaReader`] (no
//! database), encoded in pure Rust (`png`, `gif`), and written as new files under
//! `<data folder>/exports/`. An exported file is a copy that leaves rsRewind's history: `forget` and
//! retention do not reach it, which the window says before writing one. rsRewind records no audio,
//! so every clip is silent.
//!
//! WebM is the one exception to "pure Rust": rsRewind does not link libvpx or bundle FFmpeg. When an
//! `ffmpeg` executable is found on PATH at run time, frames are written as PNGs to a scratch folder,
//! `ffmpeg` runs as a child process (as `rsrewind-ocr` runs Tesseract), and the scratch folder is
//! removed whatever happens. Without one, WebM is not offered.
//!
//! Nothing here logs what was on screen: only counts, sizes and timings, at debug level.

use crate::thumb::{self, Bgra};
use rsrewind_core::{SourceId, Timestamp};
use rsrewind_query::{MediaReader, QueryError};
use std::ffi::OsString;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

/// Most frames in one clip.
pub const MAX_CLIP_FRAMES: usize = 300;
/// Most pixels in one clip (frames x canvas): about 300 frames at 1280 x 800.
pub const MAX_CLIP_PIXELS: u64 = 300 * 1280 * 800;
/// Largest-side choices for a clip, and the default.
pub const CLIP_SIZES: [u32; 4] = [480, 720, 960, 1280];
pub const DEFAULT_CLIP_SIZE: u32 = 960;
/// A frame shows for its real time on screen, but at least / at most this long, so a clip neither
/// flickers nor sits through a long idle stretch.
pub const MIN_DELAY_MS: i64 = 250;
pub const MAX_DELAY_MS: i64 = 1_500;
/// A clip longer than this is played faster (every delay scaled down), never cut.
pub const MAX_CLIP_MS: i64 = 60_000;
/// No frame is shorter than this after scaling (browsers slow down faster GIF frames).
const FLOOR_DELAY_MS: i64 = 60;

/// One picked moment's picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub source: Option<SourceId>,
    /// As the query layer reported it (absolute, inside that source's `media/`).
    pub media_path: String,
    pub at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipFormat {
    Gif,
    Webm,
}

impl ClipFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Gif => "gif",
            Self::Webm => "webm",
        }
    }
}

/// Shared between the export thread and the window: how far along, and whether to stop.
#[derive(Debug, Default)]
pub struct Progress {
    done: AtomicUsize,
    total: AtomicUsize,
    cancel: AtomicBool,
}

impl Progress {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// `(done, total)` frames.
    pub fn get(&self) -> (usize, usize) {
        (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }

    fn start(&self, total: usize) {
        self.total.store(total, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
    }

    fn tick(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }

    fn check(&self) -> Result<(), String> {
        if self.cancelled() {
            Err(CANCELLED.into())
        } else {
            Ok(())
        }
    }
}

/// The error text of a cancelled export.
pub const CANCELLED: &str = "cancelled";

/// Where exports go: a folder of the data folder, shown to the user after every export.
pub fn exports_dir(data_root: &Path) -> PathBuf {
    data_root.join("exports")
}

/// Somewhere decoded pictures come from: the history's media in the window, fakes in tests.
pub trait Pictures {
    fn load(&self, frame: &Frame) -> Result<Bgra, String>;
}

impl Pictures for MediaReader {
    fn load(&self, frame: &Frame) -> Result<Bgra, String> {
        let decoded = self
            .frame(frame.source, &frame.media_path)
            .map_err(|e| unreadable(frame.at, &e))?;
        thumb::downscale(
            &decoded.pixels,
            decoded.width,
            decoded.height,
            decoded.stride,
            decoded.width,
            decoded.height,
        )
        .ok_or_else(|| format!("the picture from {} is malformed", local(frame.at)))
    }
}

/// A clear sentence for a picture that cannot be read, above all one that is gone: deleted with
/// `forget` or by retention after it was picked. Never a blank image.
pub fn unreadable(at: Timestamp, error: &QueryError) -> String {
    match error {
        QueryError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => format!(
            "the picture from {} is gone (deleted, forgotten or trimmed by retention)",
            local(at)
        ),
        QueryError::MediaPath(_) => format!(
            "the picture from {} is not readable (its stored path is missing or unsafe)",
            local(at)
        ),
        other => format!("could not read the picture from {}: {other}", local(at)),
    }
}

fn local(at: Timestamp) -> String {
    at.to_local().format("%a %b %-d %H:%M:%S").to_string()
}

/// How long each frame shows (ms): the time to the next moment, kept within
/// [`MIN_DELAY_MS`]..=[`MAX_DELAY_MS`]; the last shows for [`MAX_DELAY_MS`]. A clip that would run
/// longer than [`MAX_CLIP_MS`] is sped up evenly. `starts` are moment starts, oldest first.
pub fn delays_ms(starts: &[i64]) -> Vec<i64> {
    let mut delays: Vec<i64> = starts
        .windows(2)
        .map(|w| (w[1] - w[0]).clamp(MIN_DELAY_MS, MAX_DELAY_MS))
        .collect();
    if !starts.is_empty() {
        delays.push(MAX_DELAY_MS);
    }
    let total: i64 = delays.iter().sum();
    if total > MAX_CLIP_MS {
        for d in &mut delays {
            *d = (*d * MAX_CLIP_MS / total).max(FLOOR_DELAY_MS);
        }
    }
    delays
}

/// `width` x `height` shrunk to fit `max_dim` on its longer side (never enlarged), rounded down to
/// even sizes (video encoders need them), at least 2 x 2.
pub fn fit(width: u32, height: u32, max_dim: u32) -> (u32, u32) {
    let longest = width.max(height).max(1);
    let ratio = (f64::from(max_dim.max(2)) / f64::from(longest)).min(1.0);
    let even = |v: u32| ((f64::from(v) * ratio).round() as u32 / 2 * 2).max(2);
    (even(width), even(height))
}

/// Refuses clips the window should not try to build.
pub fn check_clip(frames: usize, canvas: (u32, u32)) -> Result<(), String> {
    if frames < 2 {
        return Err("pick at least two moments for a clip".into());
    }
    if frames > MAX_CLIP_FRAMES {
        return Err(format!(
            "{frames} moments is more than a clip may hold ({MAX_CLIP_FRAMES}); pick a shorter stretch"
        ));
    }
    let pixels = u64::from(canvas.0) * u64::from(canvas.1) * frames as u64;
    if pixels > MAX_CLIP_PIXELS {
        return Err(format!(
            "{frames} frames at {}x{} is too large; choose a smaller size or fewer moments",
            canvas.0, canvas.1
        ));
    }
    Ok(())
}

/// `frame` scaled to fit `canvas` and centred on black, as RGBA.
fn letterbox(frame: &Bgra, canvas: (u32, u32)) -> Result<Vec<u8>, String> {
    let fitted = thumb::downscale(
        &frame.pixels,
        frame.width,
        frame.height,
        frame.width * 4,
        canvas.0,
        canvas.1,
    )
    .ok_or("a picture is malformed")?;
    let (cw, ch) = (canvas.0 as usize, canvas.1 as usize);
    let mut out = vec![0u8; cw * ch * 4];
    for px in out.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    let (fw, fh) = (fitted.width as usize, fitted.height as usize);
    let (ox, oy) = ((cw - fw.min(cw)) / 2, (ch - fh.min(ch)) / 2);
    for y in 0..fh.min(ch) {
        for x in 0..fw.min(cw) {
            let s = (y * fw + x) * 4;
            let d = ((y + oy) * cw + x + ox) * 4;
            out[d] = fitted.pixels[s + 2];
            out[d + 1] = fitted.pixels[s + 1];
            out[d + 2] = fitted.pixels[s];
            out[d + 3] = 255;
        }
    }
    Ok(out)
}

/// RGBA pixels as a PNG file's bytes.
pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    }
    Ok(bytes)
}

/// A full-size picture as RGBA (for the clipboard).
pub fn picture_rgba(
    pictures: &impl Pictures,
    frame: &Frame,
) -> Result<(u32, u32, Vec<u8>), String> {
    let picture = pictures.load(frame)?;
    Ok((picture.width, picture.height, picture.to_rgba()))
}

/// Saves one picture, full size, as a new PNG in `dir`. Returns its path.
pub fn save_png(pictures: &impl Pictures, frame: &Frame, dir: &Path) -> Result<PathBuf, String> {
    let picture = pictures.load(frame)?;
    let png = encode_png(&picture.to_rgba(), picture.width, picture.height)?;
    let (mut reserved, mut file) = reserve(dir, &stem(frame.at, 1), "png")?;
    file.write_all(&png)
        .and_then(|()| file.flush())
        .map_err(|e| format!("could not write {}: {e}", reserved.part.display()))?;
    drop(file);
    reserved.finish()
}

/// Encodes `frames` (oldest first) into a silent clip in `dir`, fitted to `max_dim`. `ffmpeg` is
/// required for WebM. Cancel through `progress`: the partial file and any scratch folder are
/// removed. Returns the new file's path.
pub fn clip(
    pictures: &impl Pictures,
    frames: &[Frame],
    format: ClipFormat,
    max_dim: u32,
    dir: &Path,
    ffmpeg: Option<&Path>,
    progress: &Progress,
) -> Result<PathBuf, String> {
    let ffmpeg = match (format, ffmpeg) {
        (ClipFormat::Webm, None) => {
            return Err("WebM needs an ffmpeg executable on PATH; none was found".into());
        }
        (_, ffmpeg) => ffmpeg,
    };
    if frames.len() > MAX_CLIP_FRAMES || frames.len() < 2 {
        check_clip(frames.len(), (2, 2))?;
    }
    progress.start(frames.len());
    let started = std::time::Instant::now();
    let first = pictures.load(&frames[0])?;
    let canvas = fit(first.width, first.height, max_dim);
    check_clip(frames.len(), canvas)?;
    let delays = delays_ms(&frames.iter().map(|f| f.at.0).collect::<Vec<_>>());
    let (mut reserved, file) = reserve(dir, &stem(frames[0].at, frames.len()), format.extension())?;
    match format {
        ClipFormat::Gif => write_gif(pictures, frames, first, canvas, &delays, file, progress)?,
        ClipFormat::Webm => {
            // ffmpeg writes the reserved part file itself (`-y`); our handle must be closed first.
            drop(file);
            let ffmpeg = ffmpeg.ok_or("WebM needs ffmpeg")?;
            let scratch = reserved.scratch()?;
            write_webm(
                pictures,
                frames,
                first,
                canvas,
                &delays,
                &reserved.part,
                &scratch,
                ffmpeg,
                progress,
            )?;
        }
    }
    let path = reserved.finish()?;
    tracing::debug!(
        frames = frames.len(),
        width = canvas.0,
        height = canvas.1,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "clip exported"
    );
    Ok(path)
}

fn write_gif(
    pictures: &impl Pictures,
    frames: &[Frame],
    first: Bgra,
    canvas: (u32, u32),
    delays: &[i64],
    file: std::fs::File,
    progress: &Progress,
) -> Result<(), String> {
    let (w, h) = (
        u16::try_from(canvas.0).map_err(|_| "too wide for a GIF")?,
        u16::try_from(canvas.1).map_err(|_| "too tall for a GIF")?,
    );
    let mut encoder =
        gif::Encoder::new(BufWriter::new(file), w, h, &[]).map_err(|e| e.to_string())?;
    encoder
        .set_repeat(gif::Repeat::Infinite)
        .map_err(|e| e.to_string())?;
    let mut picture = Some(first);
    for (i, frame) in frames.iter().enumerate() {
        progress.check()?;
        let current = match picture.take() {
            Some(p) => p,
            None => pictures.load(frame)?,
        };
        let mut rgba = letterbox(&current, canvas)?;
        // NeuQuant palette per frame; speed 10 is the crate's balance of quality and time.
        let mut out = gif::Frame::from_rgba_speed(w, h, &mut rgba, 10);
        let delay_cs = (delays.get(i).copied().unwrap_or(MAX_DELAY_MS) + 5) / 10;
        out.delay = u16::try_from(delay_cs.max(2)).unwrap_or(u16::MAX);
        encoder.write_frame(&out).map_err(|e| e.to_string())?;
        progress.tick();
    }
    progress.check()?;
    encoder
        .into_inner()
        .map_err(|e| e.to_string())?
        .flush()
        .map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
fn write_webm(
    pictures: &impl Pictures,
    frames: &[Frame],
    first: Bgra,
    canvas: (u32, u32),
    delays: &[i64],
    part: &Path,
    scratch: &Path,
    ffmpeg: &Path,
    progress: &Progress,
) -> Result<(), String> {
    let mut list = String::from("ffconcat version 1.0\n");
    let mut picture = Some(first);
    for (i, frame) in frames.iter().enumerate() {
        progress.check()?;
        let current = match picture.take() {
            Some(p) => p,
            None => pictures.load(frame)?,
        };
        let rgba = letterbox(&current, canvas)?;
        let name = format!("f{i:05}.png");
        std::fs::write(scratch.join(&name), encode_png(&rgba, canvas.0, canvas.1)?)
            .map_err(|e| format!("could not write a scratch frame: {e}"))?;
        let delay = delays.get(i).copied().unwrap_or(MAX_DELAY_MS);
        list.push_str(&format!(
            "file '{name}'\nduration {:.3}\n",
            delay as f64 / 1000.0
        ));
        // Encoding is the slow half; frames count for the first half of the bar.
        if i % 2 == 1 {
            progress.tick();
        }
    }
    // The concat demuxer honours the last duration only if the last file is listed again.
    list.push_str(&format!("file 'f{:05}.png'\n", frames.len() - 1));
    let list_path = scratch.join("frames.ffconcat");
    std::fs::write(&list_path, list).map_err(|e| format!("could not write the frame list: {e}"))?;
    let log_path = scratch.join("ffmpeg.log");
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let mut command = Command::new(ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(["-f", "concat", "-safe", "0", "-i"])
        .arg(&list_path)
        // Silent by construction: rsRewind never records audio, and -an keeps it that way.
        .args([
            "-an",
            "-c:v",
            "libvpx-vp9",
            "-b:v",
            "0",
            "-crf",
            "34",
            "-row-mt",
            "1",
        ])
        .args(["-pix_fmt", "yuv420p", "-f", "webm"])
        .arg(part)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not run ffmpeg: {e}"))?;
    let status = loop {
        if progress.cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CANCELLED.into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(format!("ffmpeg: {e}")),
        }
    };
    if !status.success() {
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        let last = log
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        return Err(format!("ffmpeg failed ({status}): {}", last.trim()));
    }
    let (_, total) = progress.get();
    progress.done.store(total, Ordering::Relaxed);
    Ok(())
}

/// `rsrewind-20261011-143005` or `rsrewind-20261011-143005-12f` (local time of the first moment).
fn stem(at: Timestamp, frames: usize) -> String {
    let time = at.to_local().format("%Y%m%d-%H%M%S");
    if frames > 1 {
        format!("rsrewind-{time}-{frames}f")
    } else {
        format!("rsrewind-{time}")
    }
}

/// An export's claim on its output name, held for as long as the export runs:
///
/// - `.<name>.lock`, created first and held with an exclusive lock: while it is locked, the export
///   is alive (in this window or another);
/// - `.<name>.part`, created with `create_new`, the file being written, renamed to `<name>` only
///   when complete;
/// - `.<name>.frames/`, WebM's scratch folder of full-size PNG frames.
///
/// Dropping it (success, error, cancel, panic) removes the scratch folder, an unfinished part file
/// and the lock. A crash cannot run `Drop`, so [`sweep`] removes what a dead export left: it runs
/// when the window opens and before every export.
struct Reservation {
    path: PathBuf,
    part: PathBuf,
    frames: PathBuf,
    lock_path: PathBuf,
    _lock: std::fs::File,
    finished: bool,
}

/// What every name an export leaves behind starts with (`.` + the `rsrewind-` stem).
const LEFTOVER_PREFIX: &str = ".rsrewind-";
/// A lock file this young may belong to an export that has not taken its lock yet.
const LOCK_GRACE: Duration = Duration::from_secs(60);

fn reserve(dir: &Path, stem: &str, ext: &str) -> Result<(Reservation, std::fs::File), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    sweep(dir);
    let create_new = |path: &Path| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
    };
    for n in 1..1_000 {
        let name = if n == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}-{n}.{ext}")
        };
        let path = dir.join(&name);
        if path.exists() {
            continue;
        }
        let lock_path = dir.join(format!(".{name}.lock"));
        let lock = match create_new(&lock_path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not write {}: {e}", lock_path.display())),
        };
        if lock.try_lock().is_err() {
            let _ = std::fs::remove_file(&lock_path);
            continue;
        }
        let part = dir.join(format!(".{name}.part"));
        let frames = dir.join(format!(".{name}.frames"));
        let reserved = Reservation {
            path,
            part,
            frames,
            lock_path,
            _lock: lock,
            finished: false,
        };
        // A leftover under this name without a live lock is stale: we hold the lock now.
        remove_leftover(&reserved.part);
        remove_leftover(&reserved.frames);
        match create_new(&reserved.part) {
            Ok(file) => return Ok((reserved, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not write {}: {e}", reserved.part.display())),
        }
    }
    Err(format!(
        "too many exports named {stem} in {}",
        dir.display()
    ))
}

impl Reservation {
    /// The (new, empty) scratch folder for WebM frames.
    fn scratch(&self) -> Result<PathBuf, String> {
        std::fs::create_dir(&self.frames).map_err(|e| {
            format!(
                "could not make a scratch folder {}: {e}",
                self.frames.display()
            )
        })?;
        Ok(self.frames.clone())
    }

    /// Moves the finished part file into place without replacing anything that appeared meanwhile.
    fn finish(&mut self) -> Result<PathBuf, String> {
        if self.path.exists() {
            return Err(format!(
                "{} appeared while exporting; try again",
                self.path.display()
            ));
        }
        std::fs::rename(&self.part, &self.path)
            .map_err(|e| format!("could not finish {}: {e}", self.path.display()))?;
        self.finished = true;
        Ok(self.path.clone())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        remove_leftover(&self.frames);
        if !self.finished {
            remove_leftover(&self.part);
        }
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// Removes a leftover file or folder; a link is removed as a link, never followed.
fn remove_leftover(path: &Path) {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            let _ = std::fs::remove_dir_all(path);
        }
        Ok(_) => {
            let _ = std::fs::remove_file(path);
        }
        Err(_) => {}
    }
}

/// Removes what dead exports left in `dir`: `.rsrewind-*.part` files and `.rsrewind-*.frames`
/// folders whose `.lock` is missing or held by nobody (and older than [`LOCK_GRACE`]). Only names
/// of that exact shape are touched, never anything else in the folder. Returns how many names
/// were removed.
pub fn sweep(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut bases: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(LEFTOVER_PREFIX) {
            continue;
        }
        for suffix in [".part", ".frames", ".lock"] {
            if let Some(base) = name.strip_suffix(suffix) {
                bases.insert(base.to_owned());
            }
        }
    }
    let mut removed = 0;
    for base in bases {
        let lock_path = dir.join(format!("{base}.lock"));
        let alive = match std::fs::OpenOptions::new().write(true).open(&lock_path) {
            Ok(lock) => {
                let young = std::fs::metadata(&lock_path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_none_or(|age| age < LOCK_GRACE);
                // Held by a running export, or so new its owner may not have locked it yet.
                lock.try_lock().is_err() || young
            }
            Err(_) => false,
        };
        if alive {
            continue;
        }
        for suffix in [".part", ".frames", ".lock"] {
            let path = dir.join(format!("{base}{suffix}"));
            if std::fs::symlink_metadata(&path).is_ok() {
                remove_leftover(&path);
                removed += 1;
            }
        }
    }
    if removed > 0 {
        tracing::debug!(removed, "removed leftovers of interrupted exports");
    }
    removed
}

/// An `ffmpeg` executable on PATH, if there is one (looked up when the window opens).
pub fn find_ffmpeg() -> Option<PathBuf> {
    find_on_path("ffmpeg", std::env::var_os("PATH"))
}

fn find_on_path(name: &str, path: Option<OsString>) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    std::env::split_paths(&path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(&exe))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Synthetic pictures by media path; a missing path behaves like a forgotten file.
    struct Fake(HashMap<String, Bgra>);

    impl Pictures for Fake {
        fn load(&self, frame: &Frame) -> Result<Bgra, String> {
            self.0.get(&frame.media_path).cloned().ok_or_else(|| {
                unreadable(
                    frame.at,
                    &QueryError::Io {
                        path: PathBuf::from(&frame.media_path),
                        source: std::io::ErrorKind::NotFound.into(),
                    },
                )
            })
        }
    }

    fn picture(w: u32, h: u32, seed: u8) -> Bgra {
        let pixels = (0..w * h)
            .flat_map(|i| [seed, (i % 251) as u8, seed.wrapping_mul(3), 255])
            .collect();
        Bgra {
            width: w,
            height: h,
            pixels,
        }
    }

    fn fixture(n: usize, step_ms: i64) -> (Fake, Vec<Frame>) {
        let mut map = HashMap::new();
        let mut frames = Vec::new();
        for i in 0..n {
            let path = format!("m{i}");
            map.insert(path.clone(), picture(64, 40, (i * 40) as u8));
            frames.push(Frame {
                source: None,
                media_path: path,
                at: Timestamp(1_790_000_000_000 + i as i64 * step_ms),
            });
        }
        (Fake(map), frames)
    }

    #[test]
    fn delays_follow_real_time_but_never_drag() {
        assert_eq!(
            delays_ms(&[0, 100, 1_100, 601_100]),
            [MIN_DELAY_MS, 1_000, MAX_DELAY_MS, MAX_DELAY_MS],
            "a 10-minute idle stretch shows for 1.5 s"
        );
        assert!(delays_ms(&[]).is_empty());
        assert_eq!(delays_ms(&[5]), [MAX_DELAY_MS]);
        // 300 slow moments would be 7.5 minutes: sped up to a minute, evenly, above the floor.
        let starts: Vec<i64> = (0..300).map(|i| i * 10_000).collect();
        let delays = delays_ms(&starts);
        let total: i64 = delays.iter().sum();
        assert!(
            total <= MAX_CLIP_MS && total > MAX_CLIP_MS - 1_000,
            "{total}"
        );
        assert!(delays.iter().all(|&d| d >= FLOOR_DELAY_MS));
    }

    #[test]
    fn clips_fit_and_are_guarded() {
        assert_eq!(fit(2560, 1600, 960), (960, 600));
        assert_eq!(fit(640, 400, 960), (640, 400), "never enlarged");
        assert_eq!(fit(1001, 333, 2000), (1000, 332), "even sizes");
        assert_eq!(fit(1, 1, 960), (2, 2));
        assert!(check_clip(1, (100, 100)).is_err());
        assert!(check_clip(MAX_CLIP_FRAMES + 1, (2, 2)).is_err());
        assert!(check_clip(300, (1280, 800)).is_ok());
        assert!(check_clip(300, (1920, 1200)).is_err());
    }

    #[test]
    fn a_png_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let p = picture(7, 5, 9);
        let rgba = p.to_rgba();
        let bytes = encode_png(&rgba, 7, 5)?;
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info()?;
        let mut out = vec![0; reader.output_buffer_size().ok_or("size")?];
        let info = reader.next_frame(&mut out)?;
        assert_eq!((info.width, info.height), (7, 5));
        assert_eq!(&out[..info.buffer_size()], &rgba[..]);

        let tmp = tempfile::tempdir()?;
        let (fake, frames) = fixture(1, 1);
        let path = save_png(&fake, &frames[0], tmp.path())?;
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
        let again = save_png(&fake, &frames[0], tmp.path())?;
        assert_ne!(path, again, "a second save never overwrites the first");
        Ok(())
    }

    #[test]
    fn a_gif_has_every_frame_with_real_timing() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let (fake, frames) = fixture(5, 700);
        let progress = Progress::default();
        let path = clip(
            &fake,
            &frames,
            ClipFormat::Gif,
            32,
            tmp.path(),
            None,
            &progress,
        )?;
        assert_eq!(progress.get(), (5, 5));
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder = options.read_info(std::fs::File::open(&path)?)?;
        assert_eq!((decoder.width(), decoder.height()), (32, 20));
        let mut delays = Vec::new();
        while let Some(frame) = decoder.read_next_frame()? {
            delays.push(frame.delay);
        }
        assert_eq!(delays, [70, 70, 70, 70, 150]);
        // Nothing but the clip is left behind.
        let names: Vec<_> = std::fs::read_dir(tmp.path())?
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(names.len(), 1);
        Ok(())
    }

    #[test]
    fn a_forgotten_picture_is_a_clear_error_and_leaves_nothing()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let (mut fake, frames) = fixture(4, 500);
        fake.0.remove("m2");
        let error = clip(
            &fake,
            &frames,
            ClipFormat::Gif,
            64,
            tmp.path(),
            None,
            &Progress::default(),
        )
        .err()
        .ok_or("expected an error")?;
        assert!(error.contains("is gone"), "{error}");
        assert_eq!(std::fs::read_dir(tmp.path())?.count(), 0, "no partial file");
        Ok(())
    }

    #[test]
    fn a_cancelled_export_leaves_nothing() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let (fake, frames) = fixture(4, 500);
        let progress = Progress::default();
        progress.cancel();
        let gif = clip(
            &fake,
            &frames,
            ClipFormat::Gif,
            64,
            tmp.path(),
            None,
            &progress,
        );
        assert_eq!(gif.err().as_deref(), Some(CANCELLED));
        if let Some(ffmpeg) = find_ffmpeg() {
            let webm = clip(
                &fake,
                &frames,
                ClipFormat::Webm,
                64,
                tmp.path(),
                Some(&ffmpeg),
                &progress,
            );
            assert_eq!(webm.err().as_deref(), Some(CANCELLED));
        }
        assert_eq!(std::fs::read_dir(tmp.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn without_ffmpeg_there_is_no_webm() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(find_on_path("ffmpeg", None), None);
        assert_eq!(find_on_path("ffmpeg", Some(OsString::new())), None);
        let empty = tempfile::tempdir()?;
        assert_eq!(
            find_on_path("ffmpeg", Some(empty.path().as_os_str().to_owned())),
            None
        );
        let (fake, frames) = fixture(3, 500);
        let error = clip(
            &fake,
            &frames,
            ClipFormat::Webm,
            64,
            empty.path(),
            None,
            &Progress::default(),
        )
        .err()
        .ok_or("expected an error")?;
        assert!(error.contains("ffmpeg"), "{error}");
        Ok(())
    }

    /// Runs only where an ffmpeg with libvpx-vp9 is installed (it is skipped, not failed, elsewhere).
    #[test]
    fn webm_through_ffmpeg_when_present() -> Result<(), Box<dyn std::error::Error>> {
        let Some(ffmpeg) = find_ffmpeg() else {
            skipped("no ffmpeg on PATH, the WebM encode was not exercised");
            return Ok(());
        };
        let tmp = tempfile::tempdir()?;
        let (fake, frames) = fixture(4, 400);
        let progress = Progress::default();
        let path = clip(
            &fake,
            &frames,
            ClipFormat::Webm,
            64,
            tmp.path(),
            Some(&ffmpeg),
            &progress,
        )?;
        let bytes = std::fs::read(&path)?;
        assert!(
            bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]),
            "an EBML (WebM) file"
        );
        assert_eq!(progress.get().0, progress.get().1);
        let left: Vec<_> = std::fs::read_dir(tmp.path())?
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(left.len(), 1, "the scratch folder is gone: {left:?}");
        // Four moments lasting 0.4 + 0.4 + 0.4 + 1.5 s, and no audio stream. The concat list
        // names the last picture twice (so its duration is honoured), which decodes as a fifth
        // frame at the very end.
        let Some(ffprobe) = find_on_path("ffprobe", std::env::var_os("PATH")) else {
            skipped("no ffprobe on PATH, the WebM's frames and duration were not checked");
            return Ok(());
        };
        let probe = Command::new(ffprobe)
            .args(["-v", "error", "-count_frames", "-show_entries"])
            .args(["stream=codec_type,codec_name,nb_read_frames:format=duration"])
            .args(["-of", "default=noprint_wrappers=1"])
            .arg(&path)
            .output()?;
        let text = String::from_utf8_lossy(&probe.stdout);
        assert!(text.contains("codec_name=vp9"), "{text}");
        assert!(!text.contains("codec_type=audio"), "{text}");
        assert!(text.contains("nb_read_frames=5"), "{text}");
        let duration: f64 = text
            .lines()
            .find_map(|l| l.strip_prefix("duration="))
            .ok_or("no duration")?
            .parse()?;
        assert!((duration - 2.7).abs() < 0.15, "{duration} s");
        Ok(())
    }

    /// Says a test was skipped where `cargo test` shows it even without `--nocapture` (the macros
    /// are captured; a direct write to stderr is not).
    fn skipped(why: &str) {
        let _ = writeln!(std::io::stderr(), "SKIPPED: {why}");
    }

    /// Cancels as it loads `at`: a cancel that lands in the middle of an export.
    struct CancelAt<'a> {
        inner: Fake,
        progress: &'a Progress,
        at: &'static str,
    }

    impl Pictures for CancelAt<'_> {
        fn load(&self, frame: &Frame) -> Result<Bgra, String> {
            if frame.media_path == self.at {
                self.progress.cancel();
            }
            self.inner.load(frame)
        }
    }

    #[test]
    fn a_gif_cancelled_mid_encode_leaves_nothing() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let (fake, frames) = fixture(6, 500);
        let progress = Progress::default();
        let cancelling = CancelAt {
            inner: fake,
            progress: &progress,
            at: "m1",
        };
        let result = clip(
            &cancelling,
            &frames,
            ClipFormat::Gif,
            64,
            tmp.path(),
            None,
            &progress,
        );
        assert_eq!(result.err().as_deref(), Some(CANCELLED));
        // Cancelled while loading the second picture: the frame in hand is finished, no more.
        let (done, total) = progress.get();
        assert!(done < total, "{done} of {total}");
        assert_eq!(
            std::fs::read_dir(tmp.path())?.count(),
            0,
            "no part, lock or clip"
        );
        Ok(())
    }

    /// A stand-in `ffmpeg` that records its pid and sleeps: cancelling while it runs must kill it
    /// and remove the part file, the scratch frames and the lock.
    #[cfg(unix)]
    #[test]
    fn a_webm_cancelled_while_ffmpeg_runs_kills_it_and_leaves_nothing()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let tools = tempfile::tempdir()?;
        let pid_file = tools.path().join("pid");
        let fake_ffmpeg = tools.path().join("ffmpeg");
        std::fs::write(
            &fake_ffmpeg,
            format!(
                "#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n",
                pid_file.display()
            ),
        )?;
        std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o755))?;

        let out = tempfile::tempdir()?;
        let (fake, frames) = fixture(3, 500);
        let progress = std::sync::Arc::new(Progress::default());
        let canceller = {
            let progress = progress.clone();
            let pid_file = pid_file.clone();
            std::thread::spawn(move || {
                for _ in 0..400 {
                    if std::fs::read_to_string(&pid_file).is_ok_and(|p| !p.trim().is_empty()) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                progress.cancel();
            })
        };
        let result = clip(
            &fake,
            &frames,
            ClipFormat::Webm,
            64,
            out.path(),
            Some(&fake_ffmpeg),
            &progress,
        );
        canceller.join().map_err(|_| "canceller panicked")?;
        assert_eq!(result.err().as_deref(), Some(CANCELLED));
        let pid = std::fs::read_to_string(&pid_file)?.trim().to_owned();
        let alive = Command::new("kill").args(["-0", &pid]).status()?.success();
        assert!(!alive, "ffmpeg (pid {pid}) is still running");
        let left: Vec<_> = std::fs::read_dir(out.path())?
            .filter_map(|e| e.ok())
            .collect();
        assert!(left.is_empty(), "{left:?}");
        Ok(())
    }

    #[test]
    fn leftovers_of_a_dead_export_are_swept_and_nothing_else()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path();
        // A crash mid-WebM: part, scratch frames, and a lock nobody holds (old enough).
        std::fs::write(dir.join(".rsrewind-20261011-1-3f.webm.part"), b"x")?;
        std::fs::create_dir(dir.join(".rsrewind-20261011-1-3f.webm.frames"))?;
        std::fs::write(
            dir.join(".rsrewind-20261011-1-3f.webm.frames/f00000.png"),
            b"x",
        )?;
        let old_lock = dir.join(".rsrewind-20261011-1-3f.webm.lock");
        std::fs::write(&old_lock, b"")?;
        std::fs::File::options()
            .write(true)
            .open(&old_lock)?
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(600))?;
        // A crash with no lock at all, and a scratch folder alone.
        std::fs::write(dir.join(".rsrewind-20261011-2.png.part"), b"x")?;
        std::fs::create_dir(dir.join(".rsrewind-20261011-4-2f.webm.frames"))?;
        // Not ours: kept.
        for keep in ["notes.txt", ".other.part", "rsrewind-20261011-1-3f.webm"] {
            std::fs::write(dir.join(keep), b"mine")?;
        }
        // A live export (locked) in this process: kept.
        let (live, _file) = reserve(dir, "rsrewind-20261011-3", "gif")?;

        assert_eq!(sweep(dir), 0, "reserve already swept them");
        let mut left: Vec<String> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                ".other.part",
                ".rsrewind-20261011-3.gif.lock",
                ".rsrewind-20261011-3.gif.part",
                "notes.txt",
                "rsrewind-20261011-1-3f.webm",
            ]
        );
        drop(live);
        // The live one cleans up after itself.
        assert!(!dir.join(".rsrewind-20261011-3.gif.lock").exists());
        assert!(!dir.join(".rsrewind-20261011-3.gif.part").exists());
        Ok(())
    }

    #[test]
    fn a_stale_scratch_folder_does_not_block_the_next_export()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let (fake, frames) = fixture(3, 500);
        let name = format!("{}.gif", stem(frames[0].at, 3));
        // What a crash during frame writing used to leave: scratch frames, no part file.
        std::fs::create_dir(tmp.path().join(format!(".{name}.frames")))?;
        let path = clip(
            &fake,
            &frames,
            ClipFormat::Gif,
            32,
            tmp.path(),
            None,
            &Progress::default(),
        )?;
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(name.as_str())
        );
        assert_eq!(std::fs::read_dir(tmp.path())?.count(), 1);
        Ok(())
    }
}
