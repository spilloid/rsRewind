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
    let (path, part) = destination(dir, &stem(frame.at, 1), "png")?;
    std::fs::write(&part, png).map_err(|e| format!("could not write {}: {e}", part.display()))?;
    finish(&part, &path)?;
    Ok(path)
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
    let (path, part) = destination(dir, &stem(frames[0].at, frames.len()), format.extension())?;
    let result = match format {
        ClipFormat::Gif => write_gif(pictures, frames, first, canvas, &delays, &part, progress),
        ClipFormat::Webm => match ffmpeg {
            Some(ffmpeg) => write_webm(
                pictures, frames, first, canvas, &delays, &part, ffmpeg, progress,
            ),
            None => Err("WebM needs ffmpeg".into()),
        },
    };
    match result {
        Ok(()) => {
            finish(&part, &path)?;
            tracing::debug!(
                frames = frames.len(),
                width = canvas.0,
                height = canvas.1,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "clip exported"
            );
            Ok(path)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&part);
            Err(error)
        }
    }
}

fn write_gif(
    pictures: &impl Pictures,
    frames: &[Frame],
    first: Bgra,
    canvas: (u32, u32),
    delays: &[i64],
    part: &Path,
    progress: &Progress,
) -> Result<(), String> {
    let (w, h) = (
        u16::try_from(canvas.0).map_err(|_| "too wide for a GIF")?,
        u16::try_from(canvas.1).map_err(|_| "too tall for a GIF")?,
    );
    let file = std::fs::File::create(part)
        .map_err(|e| format!("could not write {}: {e}", part.display()))?;
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
    ffmpeg: &Path,
    progress: &Progress,
) -> Result<(), String> {
    let scratch = Scratch::new(part)?;
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
        std::fs::write(
            scratch.0.join(&name),
            encode_png(&rgba, canvas.0, canvas.1)?,
        )
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
    let list_path = scratch.0.join("frames.ffconcat");
    std::fs::write(&list_path, list).map_err(|e| format!("could not write the frame list: {e}"))?;
    let log_path = scratch.0.join("ffmpeg.log");
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

/// A scratch folder next to the output, removed when dropped (success, error or cancel).
struct Scratch(PathBuf);

impl Scratch {
    fn new(part: &Path) -> Result<Self, String> {
        let dir = part.with_extension("frames");
        std::fs::create_dir(&dir)
            .map_err(|e| format!("could not make a scratch folder {}: {e}", dir.display()))?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

/// A new file name in `dir` that does not exist yet, and the hidden part file written first.
fn destination(dir: &Path, stem: &str, ext: &str) -> Result<(PathBuf, PathBuf), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    for n in 1..1_000 {
        let name = if n == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}-{n}.{ext}")
        };
        let path = dir.join(&name);
        let part = dir.join(format!(".{name}.part"));
        if !path.exists() && !part.exists() {
            return Ok((path, part));
        }
    }
    Err(format!(
        "too many exports named {stem} in {}",
        dir.display()
    ))
}

/// Moves a finished part file into place without replacing anything that appeared meanwhile.
fn finish(part: &Path, path: &Path) -> Result<(), String> {
    if path.exists() {
        let _ = std::fs::remove_file(part);
        return Err(format!(
            "{} appeared while exporting; try again",
            path.display()
        ));
    }
    std::fs::rename(part, path).map_err(|e| format!("could not finish {}: {e}", path.display()))
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
            eprintln!("no ffmpeg on PATH: skipping the WebM encode");
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
        Ok(())
    }
}
