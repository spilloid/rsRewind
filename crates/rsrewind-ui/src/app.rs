//! The rsRewind window: search bar, source filter, results, the rewind room, and the detail pane.
//!
//! Search is navigation into time (`docs/design/brief.md`): picking a result cues the room to the
//! moment the text was on screen, in its source's lane, and opens that moment in the detail pane.
//! Every query runs on the history worker; the update loop only sends questions and applies
//! answers, so the window stays responsive whatever SQLite is doing.

mod viewer;

use crate::export::{self, ClipFormat, Progress};
use crate::gaps;
use crate::nav::{self, JumpTarget, Motion, Shortcut};
use crate::pick::Picks;
use crate::recorder::{self, Control, Recorder};
use crate::style::{MONO, Tokens, UI_FONT};
use crate::thumb::Bgra;
use crate::timeline::cache::ByteLru;
use crate::timeline::model::{self, Camera, Filament};
use crate::timeline::{self, FrameKey, Moment, Room, Strip};
use crate::worker::{Answer, Ask, FrameAnswer, FramePool, HistoryWorker};
use iced::widget::{
    button, column, container, image, mouse_area, pin, responsive, rich_text, row, scrollable,
    shader, space, span, stack, text, text_input,
};
use iced::{
    Alignment, Color, ContentFit, Element, Fill, FillPortion, Font, Length, Subscription, Task,
    alignment, keyboard, theme, window,
};
use rsrewind_core::{
    DataDir, Gap, SearchHit, SearchQuery, SourceId, TimelineEntry, Timestamp, VisualDetail,
};
use rsrewind_query::{Keyframe, Mark, SourceFilter, SourceInfo, SourceKind, SourceProblem};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

/// Decoded card thumbnails kept in memory (the GPU keeps its own, also bounded).
const THUMB_BUDGET_BYTES: usize = 96 * 1024 * 1024;
/// Card thumbnails are reduced to fit this box.
const THUMB_W: u32 = 480;
const THUMB_H: u32 = 300;
/// The detail picture is reduced to fit this box.
const DETAIL_W: u32 = 2560;
const DETAIL_H: u32 = 1600;
/// Moments loaded around the cursor.
const WINDOW_LIMIT: u32 = 200;
/// Layout size used to decide which pictures to load before the real size is known.
const NOMINAL: (f32, f32) = (1280.0, 640.0);
const SEARCH_LIMIT: u32 = 100;
/// How far back gaps are looked up (from now). Older gaps are not shown yet.
const GAP_HORIZON_MS: i64 = 30 * 86_400_000;
/// The search box, for focusing it from the keyboard (Ctrl+F or `/`).
const SEARCH_ID: &str = "search";
/// The navigator panel's "Go to" and mark-label fields (`g`, Shift+M).
const GOTO_ID: &str = "goto";
const LABEL_ID: &str = "mark-label";
/// Mark labels drawn in the room at once (the nearest).
const ROOM_MARK_LABELS: usize = 6;

pub fn run(data: DataDir, appearance: crate::Appearance) -> iced::Result {
    iced::application(
        move || App::boot(data.clone(), appearance),
        App::update,
        App::view,
    )
    .title(App::title)
    .theme(App::theme)
    .subscription(App::subscription)
    .default_font(UI_FONT)
    .window(window::Settings {
        size: iced::Size::new(1280.0, 800.0),
        min_size: Some(iced::Size::new(960.0, 600.0)),
        maximized: true,
        position: window::Position::Centered,
        ..window::Settings::default()
    })
    .run()
}

#[derive(Debug, Clone)]
pub enum Message {
    Appearance(theme::Mode),
    Sources(Answer),
    Query(String),
    Searched(u64, Answer),
    Filter(SourceFilter),
    Window(u64, Answer),
    Gaps(u64, Answer),
    /// The recorder's state from `rsrewind status --json`.
    Recorder(Recorder),
    StartRecording,
    StartAtLogin,
    /// A front-door command finished: which one, and how.
    FrontDoor(&'static str, Result<(), String>),
    Thumb(FrameKey, FrameAnswer),
    Timeline(timeline::Event),
    Cue(usize),
    Cued(u64, Answer),
    Detail(u64, Answer),
    DetailPicture(u64, FrameKey, FrameAnswer),
    Step(bool),
    Latest,
    Oldest,
    /// The user's marks, re-read after every change.
    Marks(u64, Answer),
    /// Keyframes of the loaded window (for drawing).
    Keyframes(u64, Answer),
    /// The answer to a keyframe step; `true` extends the selection to it.
    SteppedKeyframe(u64, bool, Answer),
    /// The moments of a selection range.
    Ranged(u64, Answer),
    /// Previous/next keyframe or mark; `true` second: extend the selection to it.
    GoKeyframe(bool, bool),
    GoMark(bool, bool),
    JumpMark(i64),
    /// Mark the moment on stage with the label in the panel (empty: none).
    MarkNow,
    MarkLabel(String),
    /// A `rsrewind mark ...` command finished.
    MarkDone(&'static str, Result<(), String>),
    TogglePanel,
    GotoText(String),
    GotoSubmit,
    EditMark(Option<i64>),
    EditText(String),
    SaveEdit,
    AskDelete(Option<i64>),
    DeleteMark(i64),
    ClearPicks,
    /// Ask before an export (`Some`), or drop the question (`None`).
    AskExport(Option<ExportKind>),
    RunExport(ExportKind),
    ExportDone(u64, Result<Exported, String>),
    CancelExport,
    ClipSize(u32),
    Zoom(f64),
    Frame(Instant),
    Key(keyboard::Event),
    /// Double-click on the detail pane's picture.
    OpenSelected,
    Viewer(crate::viewer::Event),
    ViewerPicture(u64, FrameAnswer),
    ViewerDetail(u64, Answer),
    ViewerStepped(u64, bool, Answer),
    CloseViewer,
    ScaleFactor(f32),
}

/// What an export makes. Every kind leaves rsRewind's history (a file, or the clipboard).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    CopyImage,
    Png,
    Clip(ClipFormat),
}

impl ExportKind {
    fn label(self) -> &'static str {
        match self {
            Self::CopyImage => "Copying the picture",
            Self::Png => "Saving a PNG",
            Self::Clip(ClipFormat::Gif) => "Encoding a GIF",
            Self::Clip(ClipFormat::Webm) => "Encoding a WebM (ffmpeg)",
        }
    }
}

/// A finished export.
#[derive(Clone)]
pub enum Exported {
    Saved(std::path::PathBuf),
    /// Full-size RGBA for the clipboard.
    Image(Arc<(u32, u32, Vec<u8>)>),
}

impl std::fmt::Debug for Exported {
    // Never the pixels: they are screen content.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Saved(path) => f.debug_tuple("Saved").field(path).finish(),
            Self::Image(image) => write!(f, "Image({}x{})", image.0, image.1),
        }
    }
}

/// An export in progress.
struct Running {
    kind: ExportKind,
    seq: u64,
    progress: Arc<Progress>,
}

#[derive(Debug, Clone, PartialEq)]
struct Selection {
    frame: FrameKey,
    at: Timestamp,
}

pub struct App {
    tokens: Tokens,
    appearance: crate::Appearance,
    worker: Option<HistoryWorker>,
    frames: Option<FramePool>,
    fatal: Option<String>,

    sources: Vec<SourceInfo>,
    problems: Vec<SourceProblem>,
    sources_loaded: bool,
    /// Lane position of each source on screen; only the sources the filter admits.
    lanes: HashMap<Option<SourceId>, usize>,
    /// Accent of each source, by its place in the full list (stable across filters).
    accents: HashMap<Option<SourceId>, usize>,
    filter: SourceFilter,

    query: String,
    search_seq: u64,
    hits: Vec<SearchHit>,
    matched: HashSet<FrameKey>,
    search_error: Option<String>,
    selected_hit: Option<usize>,

    camera: Camera,
    first: f64,
    last: f64,
    /// The camera moving to a target (glide or cue; see `nav::Motion`).
    cue: Option<Motion>,
    window_seq: u64,
    window_center: Option<f64>,
    moments: Vec<Moment>,
    hovered: Option<usize>,
    /// Gaps over the recent history (see `load_gaps`), oldest first.
    gaps: Vec<Gap>,
    gaps_seq: u64,
    /// What the last step jumped over, and where it landed (shown while the camera stays there).
    skipped: Option<(Vec<Gap>, f64)>,
    /// Runs `rsrewind` subcommands for the front-door buttons and the status chip.
    control: Control,
    recorder: Recorder,
    /// A front-door command is running.
    starting: bool,
    /// Whether this window has already made sure the tray icon is up (once per window).
    tray_ensured: bool,
    /// One line about the last front-door command (an error, or "starts at login").
    front_note: Option<String>,

    thumbs: ByteLru<FrameKey, Arc<Bgra>>,
    pending: HashSet<FrameKey>,
    broken: HashSet<FrameKey>,

    selection: Option<Selection>,
    detail_seq: u64,
    detail: Option<VisualDetail>,
    detail_picture: Option<(FrameKey, image::Handle)>,
    cue_seq: u64,

    /// The user's marks, oldest first, and the panel that lists them.
    marks: Vec<Mark>,
    /// Their moments (Unix ms), for the room.
    mark_times: Vec<f64>,
    marks_seq: u64,
    panel_open: bool,
    mark_label: String,
    goto_text: String,
    editing: Option<(i64, String)>,
    confirm_delete: Option<i64>,
    /// A `rsrewind mark` command is running.
    marking: bool,
    /// Keyframes of the loaded window, and their pictures (for the cards' tabs).
    keyframes: Vec<Keyframe>,
    keyed: HashSet<FrameKey>,
    keyframes_seq: u64,
    step_seq: u64,
    /// One line about the last navigation or mark action.
    nav_note: Option<String>,
    /// The multi-frame selection, and its pictures (for drawing).
    picks: Picks,
    picked: HashSet<FrameKey>,
    range_seq: u64,
    /// The data folder (exports go to its `exports/`).
    data_root: std::path::PathBuf,
    /// An `ffmpeg` executable on PATH, for WebM (looked up once, at start).
    ffmpeg: Option<std::path::PathBuf>,
    clip_size: u32,
    confirm_export: Option<ExportKind>,
    exporting: Option<Running>,
    export_seq: u64,
    export_note: Option<String>,
    /// Created on first copy and kept: on X11 the copied picture lives as long as its owner.
    clipboard: Option<arboard::Clipboard>,

    /// The full-window image viewer, when open.
    viewer: Option<viewer::ViewerState>,
    viewer_seq: u64,
    /// The window's scale factor (physical / logical pixels), for the viewer's 100 %.
    scale_factor: f32,
}

impl App {
    fn boot(data: DataDir, appearance: crate::Appearance) -> (Self, Task<Message>) {
        let control = Control::new(data.root().to_path_buf());
        let data_root = data.root().to_path_buf();
        let (worker, frames, fatal) = match HistoryWorker::start(data) {
            Ok((worker, frames)) => (Some(worker), Some(frames), None),
            Err(error) => (None, None, Some(format!("could not start: {error}"))),
        };
        let now = Timestamp::now().0 as f64;
        let app = Self {
            tokens: match appearance {
                crate::Appearance::Light => Tokens::LIGHT,
                crate::Appearance::Dark | crate::Appearance::System => Tokens::DARK,
            },
            appearance,
            worker,
            frames,
            fatal,
            sources: Vec::new(),
            problems: Vec::new(),
            sources_loaded: false,
            lanes: HashMap::new(),
            accents: HashMap::new(),
            filter: SourceFilter::All,
            query: String::new(),
            search_seq: 0,
            hits: Vec::new(),
            matched: HashSet::new(),
            search_error: None,
            selected_hit: None,
            camera: Camera::new(now, 3_600_000.0),
            first: now,
            last: now,
            cue: None,
            window_seq: 0,
            window_center: None,
            moments: Vec::new(),
            hovered: None,
            gaps: Vec::new(),
            gaps_seq: 0,
            skipped: None,
            control,
            recorder: Recorder::Unknown,
            starting: false,
            tray_ensured: false,
            front_note: None,
            thumbs: ByteLru::new(),
            pending: HashSet::new(),
            broken: HashSet::new(),
            selection: None,
            detail_seq: 0,
            detail: None,
            detail_picture: None,
            cue_seq: 0,
            marks: Vec::new(),
            mark_times: Vec::new(),
            marks_seq: 0,
            panel_open: false,
            mark_label: String::new(),
            goto_text: String::new(),
            editing: None,
            confirm_delete: None,
            marking: false,
            keyframes: Vec::new(),
            keyed: HashSet::new(),
            keyframes_seq: 0,
            step_seq: 0,
            nav_note: None,
            picks: Picks::default(),
            picked: HashSet::new(),
            range_seq: 0,
            data_root,
            ffmpeg: export::find_ffmpeg(),
            clip_size: export::DEFAULT_CLIP_SIZE,
            confirm_export: None,
            exporting: None,
            export_seq: 0,
            export_note: None,
            clipboard: None,
            viewer: None,
            viewer_seq: 0,
            scale_factor: 1.0,
        };
        let sources = app.ask(Ask::Sources, Message::Sources);
        let sources = Task::batch([
            sources,
            app.ask(Ask::Marks, |a| Message::Marks(0, a)),
            Task::perform(
                app.control.status(std::time::Duration::ZERO),
                Message::Recorder,
            ),
        ]);
        let theme = if appearance == crate::Appearance::System {
            iced::system::theme().map(Message::Appearance)
        } else {
            Task::none()
        };
        (app, Task::batch([sources, theme]))
    }

    fn title(&self) -> String {
        "rsRewind".into()
    }

    fn theme(&self) -> iced::Theme {
        self.tokens.theme()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![keyboard::listen().map(Message::Key)];
        if self.appearance == crate::Appearance::System {
            subs.push(iced::system::theme_changes().map(Message::Appearance));
        }
        // Frames drive the camera's motion and redraw an export's progress.
        if self.cue.is_some() || self.exporting.is_some() {
            subs.push(window::frames().map(Message::Frame));
        }
        Subscription::batch(subs)
    }

    fn ask(&self, ask: Ask, done: impl Fn(Answer) -> Message + Send + 'static) -> Task<Message> {
        match &self.worker {
            Some(worker) => Task::perform(worker.ask(ask), done),
            None => Task::none(),
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Appearance(mode) => {
                if self.appearance == crate::Appearance::System {
                    self.tokens = Tokens::for_mode(mode);
                }
                Task::none()
            }
            Message::Sources(Answer::Sources { sources, problems }) => {
                self.sources = sources;
                self.problems = problems;
                self.sources_loaded = true;
                self.accents = self
                    .sources
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (s.source, i))
                    .collect();
                self.fit_bounds();
                self.camera.cursor = self.last;
                Task::batch([self.refresh_window(true), self.load_gaps()])
            }
            Message::Sources(Answer::Failed(error)) => {
                self.fatal = Some(error);
                Task::none()
            }
            Message::Sources(_) => Task::none(),
            Message::Query(query) => {
                self.query = query;
                self.search()
            }
            Message::Searched(seq, answer) => {
                if seq != self.search_seq {
                    return Task::none();
                }
                match answer {
                    Answer::Search(hits) => {
                        self.matched = hits.iter().map(|h| (h.source, h.visual_state_id)).collect();
                        self.hits = hits;
                        self.search_error = None;
                        self.selected_hit = None;
                    }
                    Answer::Failed(error) => self.search_error = Some(error),
                    _ => {}
                }
                Task::none()
            }
            Message::Filter(filter) => {
                self.filter = filter;
                self.fit_bounds();
                self.camera.cursor = self.camera.cursor.clamp(self.first, self.last);
                if self
                    .selection
                    .as_ref()
                    .is_some_and(|s| !filter.admits(s.frame.0))
                {
                    self.selection = None;
                    self.detail = None;
                    self.detail_picture = None;
                }
                Task::batch([self.refresh_window(true), self.search(), self.load_gaps()])
            }
            Message::Window(seq, answer) => {
                if seq != self.window_seq {
                    return Task::none();
                }
                let mut keyframes = Task::none();
                if let Answer::Window(entries) = answer {
                    self.set_moments(entries);
                    keyframes = self.load_keyframes();
                }
                // The first load (or a new filter) puts the moment at the cursor on the stage.
                let follow = if self.selection.is_none() {
                    self.follow_cursor()
                } else {
                    Task::none()
                };
                Task::batch([self.load_thumbs(), follow, keyframes])
            }
            Message::Gaps(seq, answer) => {
                if seq == self.gaps_seq
                    && let Answer::Gaps(gaps) = answer
                {
                    self.gaps = gaps;
                }
                Task::none()
            }
            Message::Recorder(state) => {
                let was = self.recorder;
                self.recorder = state;
                let mut next =
                    Task::perform(self.control.status(recorder::POLL), Message::Recorder);
                // A recorder that is running without its tray icon (started from the command line, or
                // at login before the icon was part of that) gets one when the window opens. `rsrewind
                // tray` exits at once if an icon is already up, so this is safe to ask once.
                if !self.tray_ensured && matches!(state, Recorder::Recording | Recorder::Paused) {
                    self.tray_ensured = true;
                    next = Task::batch([
                        next,
                        Task::perform(self.control.run(&["tray"]), |r| {
                            Message::FrontDoor("tray", r)
                        }),
                    ]);
                }
                // Waiting for the first moments: look for history again, so it appears on its own.
                if self.sources.is_empty() && state == Recorder::Recording
                    || (was != state && self.sources.is_empty())
                {
                    return Task::batch([next, self.ask(Ask::Sources, Message::Sources)]);
                }
                next
            }
            Message::StartRecording => {
                if self.starting {
                    return Task::none();
                }
                self.starting = true;
                self.front_note = None;
                Task::perform(self.control.run(&["start"]), |r| {
                    Message::FrontDoor("start", r)
                })
            }
            Message::StartAtLogin => {
                self.front_note = None;
                Task::perform(self.control.run(&["tray", "--autostart", "on"]), |r| {
                    Message::FrontDoor("login", r)
                })
            }
            Message::FrontDoor(what, result) => {
                if what == "start" {
                    self.starting = false;
                }
                match (what, result) {
                    // Recording: also bring up the tray icon (it exits at once if one is running).
                    ("start", Ok(())) => {
                        self.recorder = Recorder::Recording;
                        return Task::batch([
                            Task::perform(self.control.run(&["tray"]), |r| {
                                Message::FrontDoor("tray", r)
                            }),
                            self.ask(Ask::Sources, Message::Sources),
                        ]);
                    }
                    ("start", Err(e)) => {
                        self.front_note = Some(format!("Could not start recording: {e}"))
                    }
                    ("login", Ok(())) => {
                        self.front_note = Some("rsRewind will start when you log in.".into());
                    }
                    ("login", Err(e)) => {
                        self.front_note = Some(format!("Could not set that up: {e}"))
                    }
                    // The tray is a convenience; a failure there is not worth a message.
                    _ => {}
                }
                Task::none()
            }
            Message::Thumb(key, answer) => {
                self.pending.remove(&key);
                match answer {
                    FrameAnswer::Ready(picture) => {
                        let bytes = picture.bytes();
                        self.thumbs.insert(key, picture, bytes);
                        let visible = self.visible_frames();
                        let evicted = self
                            .thumbs
                            .evict_to(THUMB_BUDGET_BYTES, |k| visible.contains(k));
                        tracing::debug!(
                            evicted = evicted.len(),
                            cached_bytes = self.thumbs.bytes(),
                            "thumbnail cached"
                        );
                    }
                    FrameAnswer::Failed(reason) => {
                        // The reason names a file and an error, never screen content.
                        tracing::debug!(%reason, "picture unavailable");
                        self.broken.insert(key);
                    }
                    // Pushed out of the bounded queue by newer requests: ask again if it is
                    // still on screen.
                    FrameAnswer::Dropped if self.visible_frames().contains(&key) => {
                        return self.load_thumbs();
                    }
                    FrameAnswer::Dropped => {}
                }
                Task::none()
            }
            Message::Timeline(timeline::Event::Scrub(cursor)) => {
                self.cue = None;
                self.camera.cursor = cursor;
                Task::batch([
                    self.refresh_window(false),
                    self.load_thumbs(),
                    self.follow_cursor(),
                ])
            }
            Message::Timeline(timeline::Event::Hover(index)) => {
                self.hovered = index;
                Task::none()
            }
            Message::Timeline(timeline::Event::Open(frame)) => self.open_from_room(frame),
            Message::OpenSelected => self.open_selected(),
            Message::Viewer(event) => self.viewer_event(event),
            Message::ViewerPicture(seq, answer) => self.viewer_picture(seq, answer),
            Message::ViewerDetail(seq, answer) => self.viewer_detail(seq, answer),
            Message::ViewerStepped(seq, forward, answer) => {
                self.viewer_stepped(seq, forward, answer)
            }
            Message::CloseViewer => self.close_viewer(),
            Message::ScaleFactor(factor) => {
                self.scale_factor = factor;
                Task::none()
            }
            Message::Timeline(timeline::Event::Select {
                index,
                extend,
                toggle,
            }) => {
                let Some(moment) = self.moments.get(index) else {
                    return Task::none();
                };
                let entry = moment.entry.clone();
                if toggle {
                    self.picks.toggle(&entry);
                    self.picked = self.picks.frames();
                    return Task::none();
                }
                if extend {
                    return self.extend_to(entry.started_at);
                }
                self.picks.single(&entry);
                self.picked = self.picks.frames();
                self.selected_hit = None;
                self.select(&entry)
            }
            Message::Cue(index) => {
                let Some(hit) = self.hits.get(index).cloned() else {
                    return Task::none();
                };
                self.selected_hit = Some(index);
                // Cue to the moment the text was on screen (the earliest matching observation),
                // not to when the picture was first captured.
                let frame = (hit.source, hit.visual_state_id);
                self.selection = Some(Selection {
                    frame,
                    at: hit.timestamp,
                });
                self.cue_to(hit.timestamp.0 as f64);
                self.cue_seq += 1;
                let seq = self.cue_seq;
                Task::batch([
                    self.ask(
                        Ask::At {
                            at: hit.timestamp,
                            filter: SourceFilter::only(hit.source),
                        },
                        move |a| Message::Cued(seq, a),
                    ),
                    self.open_detail(frame, hit.media_path.clone()),
                    self.refresh_window(true),
                ])
            }
            Message::Cued(seq, answer) => {
                if seq != self.cue_seq {
                    return Task::none();
                }
                // The observation covering the hit's moment: aim the camera at its exact start.
                if let Answer::At(Some(entry)) = answer
                    && self
                        .selection
                        .as_ref()
                        .is_some_and(|s| s.frame == (entry.source, entry.visual_state_id))
                {
                    let target = entry.started_at.0 as f64;
                    if let Some(cue) = self.cue.as_mut() {
                        cue.to = target;
                    } else {
                        self.camera.cursor = target;
                    }
                }
                Task::none()
            }
            Message::Detail(seq, answer) => {
                if seq == self.detail_seq
                    && let Answer::Detail(detail) = answer
                {
                    self.detail = detail;
                }
                Task::none()
            }
            Message::DetailPicture(seq, key, answer) => {
                if seq == self.detail_seq
                    && let FrameAnswer::Ready(picture) = answer
                {
                    let handle =
                        image::Handle::from_rgba(picture.width, picture.height, picture.to_rgba());
                    self.detail_picture = Some((key, handle));
                }
                Task::none()
            }
            Message::Step(forward) => self.step(forward, false),
            Message::Oldest => {
                self.cue_to(self.first);
                self.refresh_window(true)
            }
            Message::Marks(seq, answer) => {
                if seq == self.marks_seq {
                    match answer {
                        Answer::Marks(marks) => {
                            self.mark_times = marks.iter().map(|m| m.at.0 as f64).collect();
                            self.marks = marks;
                        }
                        Answer::Failed(error) => self.nav_note = Some(error),
                        _ => {}
                    }
                }
                Task::none()
            }
            Message::Keyframes(seq, answer) => {
                if seq == self.keyframes_seq
                    && let Answer::Keyframes(keyframes) = answer
                {
                    self.keyed = keyframes
                        .iter()
                        .map(|k| (k.source, k.visual_state_id))
                        .collect();
                    self.keyframes = keyframes;
                }
                Task::none()
            }
            Message::GoKeyframe(forward, extend) => {
                self.step_seq += 1;
                let seq = self.step_seq;
                let from = Timestamp(self.target_cursor().round() as i64);
                self.ask(
                    Ask::StepKeyframe {
                        filter: self.filter,
                        from,
                        forward,
                    },
                    move |a| Message::SteppedKeyframe(seq, extend, a),
                )
            }
            Message::SteppedKeyframe(seq, extend, answer) => {
                if seq != self.step_seq {
                    return Task::none();
                }
                match answer {
                    Answer::StepKeyframe(Some(k)) => {
                        self.nav_note = Some(format!(
                            "Keyframe · {} · {}",
                            k.kind.describe(),
                            format_time(k.at.0)
                        ));
                        self.jump(k.at.0 as f64, extend)
                    }
                    Answer::StepKeyframe(None) => {
                        self.nav_note = Some("No more keyframes that way.".into());
                        Task::none()
                    }
                    Answer::Failed(error) => {
                        self.nav_note = Some(error);
                        Task::none()
                    }
                    _ => Task::none(),
                }
            }
            Message::GoMark(forward, extend) => {
                let times = self.marks.iter().map(|m| m.at.0);
                match nav::neighbour(times, self.target_cursor(), forward) {
                    Some(at) => {
                        self.nav_note = self.marks.iter().find(|m| m.at.0 == at).map(mark_title);
                        self.jump(at as f64, extend)
                    }
                    None => {
                        self.nav_note = Some(if self.marks.is_empty() {
                            "No marks yet: press M to mark the moment on stage.".into()
                        } else {
                            "No more marks that way.".into()
                        });
                        Task::none()
                    }
                }
            }
            Message::JumpMark(id) => {
                let Some(mark) = self.marks.iter().find(|m| m.id == id) else {
                    return Task::none();
                };
                let at = mark.at.0 as f64;
                self.nav_note = Some(mark_title(mark));
                self.jump(at, false)
            }
            Message::MarkNow => self.mark_now(),
            Message::MarkLabel(label) => {
                self.mark_label = label;
                Task::none()
            }
            Message::MarkDone(what, result) => {
                self.marking = false;
                match result {
                    Ok(()) => {
                        if what == "add" {
                            self.mark_label.clear();
                        }
                        self.editing = None;
                        self.confirm_delete = None;
                    }
                    Err(error) => {
                        self.nav_note = Some(format!("Could not {what} the mark: {error}"))
                    }
                }
                self.load_marks()
            }
            Message::TogglePanel => {
                self.panel_open = !self.panel_open;
                self.editing = None;
                self.confirm_delete = None;
                Task::none()
            }
            Message::GotoText(text) => {
                self.goto_text = text;
                Task::none()
            }
            Message::GotoSubmit => match nav::parse_jump(&self.goto_text, chrono::Local::now()) {
                Ok(JumpTarget::Latest) => self.update(Message::Latest),
                Ok(JumpTarget::Oldest) => self.update(Message::Oldest),
                Ok(JumpTarget::At(ms)) => {
                    let target = ms as f64;
                    self.nav_note = Some(if target < self.first || target > self.last {
                        format!(
                            "{} is outside the history shown; went to the nearest end.",
                            format_time(ms)
                        )
                    } else {
                        format!("Went to {}", format_time(ms))
                    });
                    self.jump(target, false)
                }
                Err(error) => {
                    self.nav_note = Some(error);
                    Task::none()
                }
            },
            Message::EditMark(id) => {
                self.confirm_delete = None;
                self.editing = id.and_then(|id| {
                    self.marks
                        .iter()
                        .find(|m| m.id == id)
                        .map(|m| (id, m.label.clone()))
                });
                Task::none()
            }
            Message::EditText(text) => {
                if let Some((_, label)) = self.editing.as_mut() {
                    *label = text;
                }
                Task::none()
            }
            Message::SaveEdit => {
                let Some((id, label)) = self.editing.clone() else {
                    return Task::none();
                };
                self.run_mark(
                    "rename",
                    vec![
                        "mark".into(),
                        "rename".into(),
                        id.to_string(),
                        format!("--label={label}"),
                    ],
                )
            }
            Message::AskDelete(id) => {
                self.editing = None;
                self.confirm_delete = id;
                Task::none()
            }
            Message::DeleteMark(id) => self.run_mark(
                "remove",
                vec!["mark".into(), "remove".into(), id.to_string()],
            ),
            Message::Ranged(seq, answer) => {
                if seq != self.range_seq {
                    return Task::none();
                }
                match answer {
                    Answer::Between { entries, truncated } => {
                        self.picks.apply_range(entries, truncated);
                        self.picked = self.picks.frames();
                        if truncated {
                            self.nav_note = Some(format!(
                                "Selected the first {} moments of that range (the most at once).",
                                crate::worker::MAX_RANGE
                            ));
                        }
                    }
                    Answer::Failed(error) => self.nav_note = Some(error),
                    _ => {}
                }
                Task::none()
            }
            Message::ClearPicks => {
                self.picks.clear();
                self.picked.clear();
                Task::none()
            }
            Message::AskExport(kind) => {
                self.confirm_export = kind;
                if kind.is_some() {
                    self.panel_open = true;
                }
                Task::none()
            }
            Message::RunExport(kind) => self.run_export(kind),
            Message::ExportDone(seq, result) => {
                if self.exporting.as_ref().is_none_or(|r| r.seq != seq) {
                    return Task::none();
                }
                self.exporting = None;
                self.export_note = Some(match result {
                    Ok(Exported::Saved(path)) => format!("Saved {}", path.display()),
                    Ok(Exported::Image(image)) => self.put_on_clipboard(&image),
                    Err(error) if error == export::CANCELLED => "Export cancelled.".into(),
                    Err(error) => format!("Export failed: {error}"),
                });
                Task::none()
            }
            Message::CancelExport => {
                if let Some(running) = &self.exporting {
                    running.progress.cancel();
                }
                Task::none()
            }
            Message::ClipSize(size) => {
                self.clip_size = size;
                Task::none()
            }
            Message::Latest => {
                self.cue_to(self.last);
                self.refresh_window(true)
            }
            Message::Zoom(factor) => {
                self.camera = self.camera.zoomed(factor);
                Task::batch([self.refresh_window(true), self.load_thumbs()])
            }
            Message::Frame(now) => {
                if let Some(motion) = self.cue {
                    let (cursor, done) = motion.at(now);
                    self.camera.cursor = cursor;
                    if done {
                        self.cue = None;
                        return Task::batch([self.load_thumbs(), self.follow_cursor()]);
                    }
                }
                Task::none()
            }
            Message::Key(keyboard::Event::KeyPressed { key, .. }) if self.viewer.is_some() => {
                self.viewer_key(key.as_ref()).unwrap_or_else(Task::none)
            }
            // Ctrl+F or `/` jumps to the search box (a `/` typed into the box is the box's own).
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. })
                if is_find_key(key.as_ref(), modifiers) =>
            {
                iced::widget::operation::focus(SEARCH_ID)
            }
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                match nav::shortcut(key.as_ref(), modifiers) {
                    Some(shortcut) => self.shortcut(shortcut),
                    None => Task::none(),
                }
            }
            Message::Key(_) => Task::none(),
        }
    }

    fn accent(&self, source: Option<SourceId>) -> usize {
        self.accents.get(&source).copied().unwrap_or(0)
    }

    /// Lanes and scrub bounds for the sources the filter admits.
    fn fit_bounds(&mut self) {
        self.lanes = self
            .sources
            .iter()
            .filter(|s| self.filter.admits(s.source))
            .enumerate()
            .map(|(lane, s)| (s.source, lane))
            .collect();
        let admitted = self.sources.iter().filter(|s| self.filter.admits(s.source));
        let first = admitted.clone().filter_map(|s| s.first).min();
        let last = admitted.filter_map(|s| s.last).max();
        if let (Some(first), Some(last)) = (first, last) {
            self.first = first.0 as f64;
            self.last = last.0 as f64;
        }
    }

    /// Gaps over the most recent [`GAP_HORIZON_MS`] of the admitted history, up to now (an imported
    /// machine's gaps stop at the last thing it sent). Reloaded with the source list or filter.
    fn load_gaps(&mut self) -> Task<Message> {
        if !self.sources_loaded {
            return Task::none();
        }
        self.gaps_seq += 1;
        let seq = self.gaps_seq;
        let to = Timestamp::now();
        let from = Timestamp((self.first as i64).max(to.0 - GAP_HORIZON_MS));
        self.ask(
            Ask::Gaps {
                filter: self.filter,
                from,
                to,
            },
            move |a| Message::Gaps(seq, a),
        )
    }

    /// Lines explaining the gaps the camera is in, or the ones the last step jumped over.
    fn gap_notes(&self) -> Vec<(String, bool)> {
        let cursor = self.cue.map_or(self.camera.cursor, |c| c.to);
        let jumped = self
            .skipped
            .as_ref()
            .filter(|(_, landed)| (landed - cursor).abs() < 1.0)
            .map(|(gaps, _)| gaps.iter().collect::<Vec<_>>());
        let (gaps, verb) = match jumped {
            Some(gaps) => (gaps, "Skipped: "),
            None => (gaps::at(&self.gaps, self.filter, cursor as i64), ""),
        };
        gaps.into_iter()
            .map(|g| {
                let who = if self.lanes.len() > 1 {
                    format!("{} · ", self.lane_label(g.source))
                } else {
                    String::new()
                };
                (
                    format!(
                        "{who}{verb}{} ({} to {})",
                        gaps::sentence(g),
                        format_time(g.from.0),
                        format_time(g.to.0)
                    ),
                    gaps::is_warning(g.reason),
                )
            })
            .collect()
    }

    /// Moves the camera to `target`: a glide from where it is when the target is within the
    /// room's depth, otherwise a cue that starts a little after the moment and settles onto it,
    /// instead of flying through everything in between (`nav::Motion::plan`).
    fn cue_to(&mut self, target: f64) {
        let target = target.clamp(self.first, self.last);
        let motion = Motion::plan(
            self.camera.cursor,
            target,
            self.camera.depth_ms,
            self.camera.knee_ms,
            self.last,
            Instant::now(),
        );
        self.camera.cursor = motion.from;
        self.cue = Some(motion);
    }

    /// Where the camera is going (or is): repeated steps chain from the target, not the
    /// animation's current frame.
    fn target_cursor(&self) -> f64 {
        self.cue.map_or(self.camera.cursor, |c| c.to)
    }

    /// One moment back or forward; `extend` grows the selection to it.
    fn step(&mut self, forward: bool, extend: bool) -> Task<Message> {
        let starts: Vec<i64> = self.moments.iter().map(|m| m.entry.started_at.0).collect();
        let Some(target) = model::step(&starts, self.target_cursor(), forward) else {
            return Task::none();
        };
        let entry = self
            .moments
            .iter()
            .filter(|m| m.entry.started_at.0 == target)
            .max_by_key(|m| m.entry.cursor())
            .map(|m| m.entry.clone());
        let jumped: Vec<Gap> =
            gaps::crossed(&self.gaps, self.filter, self.target_cursor() as i64, target)
                .into_iter()
                .cloned()
                .collect();
        self.skipped = (!jumped.is_empty()).then_some((jumped, target as f64));
        self.cue_to(target as f64);
        let moved = match entry {
            Some(entry) => Task::batch([self.select(&entry), self.refresh_window(false)]),
            None => self.refresh_window(false),
        };
        if extend {
            return Task::batch([moved, self.extend_to(Timestamp(target))]);
        }
        moved
    }

    /// Goes to a time (a mark, a keyframe, "Go to"); `extend` grows the selection to it.
    fn jump(&mut self, target: f64, extend: bool) -> Task<Message> {
        self.skipped = None;
        self.cue_to(target);
        let moved = self.refresh_window(false);
        if extend {
            return Task::batch([moved, self.extend_to(Timestamp(target as i64))]);
        }
        moved
    }

    /// Selects every moment from the anchor (or the moment on stage) to `target`.
    fn extend_to(&mut self, target: Timestamp) -> Task<Message> {
        if let Some(stage) = self.stage_entry() {
            self.picks.anchor_at(&stage);
        }
        let fallback = self.selection.as_ref().map(|s| s.at);
        let Some((from, to)) = self.picks.range_to(target, fallback) else {
            return Task::none();
        };
        self.range_seq += 1;
        let seq = self.range_seq;
        self.ask(
            Ask::Between {
                filter: self.filter,
                from,
                to,
            },
            move |a| Message::Ranged(seq, a),
        )
    }

    /// The loaded moment the stage shows, if it is loaded.
    fn stage_entry(&self) -> Option<TimelineEntry> {
        let frame = self.selection.as_ref()?.frame;
        self.moments
            .iter()
            .find(|m| m.frame() == frame)
            .map(|m| m.entry.clone())
    }

    fn shortcut(&mut self, shortcut: Shortcut) -> Task<Message> {
        match shortcut {
            Shortcut::Moment { forward, extend } => self.step(forward, extend),
            Shortcut::Keyframe { forward, extend } => {
                self.update(Message::GoKeyframe(forward, extend))
            }
            Shortcut::Mark { forward, extend } => self.update(Message::GoMark(forward, extend)),
            Shortcut::Oldest => self.update(Message::Oldest),
            Shortcut::Latest => self.update(Message::Latest),
            Shortcut::MarkNow => {
                self.mark_label.clear();
                self.mark_now()
            }
            Shortcut::MarkWithLabel => {
                self.panel_open = true;
                iced::widget::operation::focus(LABEL_ID)
            }
            Shortcut::GoTo => {
                self.panel_open = true;
                iced::widget::operation::focus(GOTO_ID)
            }
            Shortcut::Escape => {
                if self.panel_open {
                    self.panel_open = false;
                    self.confirm_export = None;
                } else if !self.picks.is_empty() {
                    self.picks.clear();
                    self.picked.clear();
                }
                Task::none()
            }
            Shortcut::SelectAll => {
                let entries: Vec<TimelineEntry> =
                    self.moments.iter().map(|m| m.entry.clone()).collect();
                self.picks.all(&entries);
                self.picked = self.picks.frames();
                Task::none()
            }
            Shortcut::CopyImage => self.copy_image(),
        }
    }

    /// Copies the picked picture (or the one on stage) to the clipboard.
    fn copy_image(&mut self) -> Task<Message> {
        self.run_export(ExportKind::CopyImage)
    }

    /// The one picture single-frame actions use: the only picked moment, else the one on stage.
    fn single_frame(&self) -> Option<export::Frame> {
        if self.picks.len() == 1
            && let Some(entry) = self.picks.entries().next()
        {
            return Some(export::Frame {
                source: entry.source,
                media_path: entry.media_path.clone(),
                at: entry.started_at,
            });
        }
        let selection = self.selection.as_ref()?;
        if let Some(entry) = self.stage_entry() {
            return Some(export::Frame {
                source: entry.source,
                media_path: entry.media_path,
                at: selection.at,
            });
        }
        let detail = self
            .detail
            .as_ref()
            .filter(|d| (d.source, d.visual_state_id) == selection.frame)?;
        Some(export::Frame {
            source: detail.source,
            media_path: detail.media_path.clone(),
            at: selection.at,
        })
    }

    /// Starts an export on its own thread (never the UI thread; the recorder is another process).
    fn run_export(&mut self, kind: ExportKind) -> Task<Message> {
        self.confirm_export = None;
        if self.exporting.is_some() {
            self.export_note = Some("An export is already running.".into());
            return Task::none();
        }
        let Some(media) = self.frames.as_ref().and_then(|f| f.media()) else {
            self.export_note = Some("History is not open yet.".into());
            return Task::none();
        };
        let dir = export::exports_dir(&self.data_root);
        let progress = Arc::new(Progress::default());
        let job_progress = progress.clone();
        let work: Box<dyn FnOnce() -> Result<Exported, String> + Send> = match kind {
            ExportKind::CopyImage | ExportKind::Png => {
                let Some(frame) = self.single_frame() else {
                    self.export_note = Some("Put a moment on stage or pick one first.".into());
                    return Task::none();
                };
                if kind == ExportKind::Png {
                    Box::new(move || export::save_png(&media, &frame, &dir).map(Exported::Saved))
                } else {
                    Box::new(move || {
                        export::picture_rgba(&media, &frame)
                            .map(|image| Exported::Image(Arc::new(image)))
                    })
                }
            }
            ExportKind::Clip(format) => {
                let frames: Vec<export::Frame> = self
                    .picks
                    .entries()
                    .map(|e| export::Frame {
                        source: e.source,
                        media_path: e.media_path.clone(),
                        at: e.started_at,
                    })
                    .collect();
                let ffmpeg = self.ffmpeg.clone();
                let size = self.clip_size;
                Box::new(move || {
                    export::clip(
                        &media,
                        &frames,
                        format,
                        size,
                        &dir,
                        ffmpeg.as_deref(),
                        &job_progress,
                    )
                    .map(Exported::Saved)
                })
            }
        };
        self.export_seq += 1;
        let seq = self.export_seq;
        self.export_note = None;
        self.exporting = Some(Running {
            kind,
            seq,
            progress,
        });
        Task::perform(
            recorder::off_thread(Err("the export could not start".to_string()), work),
            move |r| Message::ExportDone(seq, r),
        )
    }

    /// Hands a decoded picture to the system clipboard; the sentence says how it went.
    fn put_on_clipboard(&mut self, image: &(u32, u32, Vec<u8>)) -> String {
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(clipboard) => self.clipboard = Some(clipboard),
                Err(error) => return format!("Could not reach the clipboard: {error}"),
            }
        }
        let Some(clipboard) = self.clipboard.as_mut() else {
            return "Could not reach the clipboard.".into();
        };
        let data = arboard::ImageData {
            width: image.0 as usize,
            height: image.1 as usize,
            bytes: std::borrow::Cow::Borrowed(&image.2),
        };
        match clipboard.set_image(data) {
            Ok(()) => format!(
                "Copied a {}x{} picture. It is now outside rsRewind's history.",
                image.0, image.1
            ),
            Err(error) => format!("Could not copy the picture: {error}"),
        }
    }

    fn load_marks(&mut self) -> Task<Message> {
        self.marks_seq += 1;
        let seq = self.marks_seq;
        self.ask(Ask::Marks, move |a| Message::Marks(seq, a))
    }

    /// Keyframes over the loaded moments, for the strip and the cards' tabs.
    fn load_keyframes(&mut self) -> Task<Message> {
        let (Some(first), Some(last)) = (self.moments.first(), self.moments.last()) else {
            self.keyframes.clear();
            self.keyed.clear();
            return Task::none();
        };
        self.keyframes_seq += 1;
        let seq = self.keyframes_seq;
        self.ask(
            Ask::Keyframes {
                filter: self.filter,
                from: first.entry.started_at,
                to: last.entry.started_at,
            },
            move |a| Message::Keyframes(seq, a),
        )
    }

    /// Marks the moment on stage (or, with nothing on stage, the cursor) with the panel's label,
    /// through `rsrewind mark add` (the window may not write history itself).
    fn mark_now(&mut self) -> Task<Message> {
        if self.marking {
            return Task::none();
        }
        let at = self
            .selection
            .as_ref()
            .map_or(self.target_cursor().round() as i64, |s| s.at.0);
        self.nav_note = Some(format!("Marked {}", format_time(at)));
        let label = self.mark_label.trim().to_owned();
        self.run_mark(
            "add",
            vec![
                "mark".into(),
                "add".into(),
                "--at-ms".into(),
                at.to_string(),
                format!("--label={label}"),
            ],
        )
    }

    fn run_mark(&mut self, what: &'static str, args: Vec<String>) -> Task<Message> {
        self.marking = true;
        Task::perform(self.control.run_args(args), move |r| {
            Message::MarkDone(what, r)
        })
    }

    fn search(&mut self) -> Task<Message> {
        self.search_seq += 1;
        let seq = self.search_seq;
        if self.query.trim().is_empty() {
            self.hits.clear();
            self.matched.clear();
            self.search_error = None;
            self.selected_hit = None;
            return Task::none();
        }
        self.ask(
            Ask::Search {
                query: SearchQuery {
                    text: self.query.clone(),
                    limit: SEARCH_LIMIT,
                    ..SearchQuery::default()
                },
                filter: self.filter,
            },
            move |a| Message::Searched(seq, a),
        )
    }

    /// Loads the moments around the cursor when it has moved far enough from the last load.
    fn refresh_window(&mut self, force: bool) -> Task<Message> {
        if !self.sources_loaded {
            return Task::none();
        }
        let target = self.cue.map_or(self.camera.cursor, |c| c.to);
        let near = self
            .window_center
            .is_some_and(|c| (c - target).abs() < self.camera.age_at(0.08).max(1_000.0));
        if near && !force {
            return Task::none();
        }
        self.window_center = Some(target);
        self.window_seq += 1;
        let seq = self.window_seq;
        // A little ahead of the cursor, so moments drifting past the camera are loaded too.
        let at = Timestamp((target + 120_000.0) as i64);
        self.ask(
            Ask::Window {
                filter: self.filter,
                at: Some(at),
                limit: WINDOW_LIMIT,
            },
            move |a| Message::Window(seq, a),
        )
    }

    fn set_moments(&mut self, entries: Vec<TimelineEntry>) {
        let mut entries = entries;
        entries.sort_by_key(TimelineEntry::cursor);
        self.moments = entries
            .into_iter()
            .filter_map(|entry| {
                let lane = *self.lanes.get(&entry.source)?;
                let accent = self.accent(entry.source);
                Some(Moment {
                    entry,
                    lane,
                    accent,
                })
            })
            .collect();
        self.hovered = None;
    }

    fn visible_frames(&self) -> HashSet<FrameKey> {
        let camera = self.cue.map_or(self.camera, |c| Camera {
            cursor: c.to,
            ..self.camera
        });
        timeline::place(
            &self.moments,
            self.lanes.len(),
            &camera,
            NOMINAL.0,
            NOMINAL.1,
        )
        .iter()
        .filter_map(|p| self.moments.get(p.index).map(Moment::frame))
        .collect()
    }

    /// Requests thumbnails for visible moments that are neither cached, pending nor broken.
    fn load_thumbs(&mut self) -> Task<Message> {
        let Some(frames) = self.frames.clone() else {
            return Task::none();
        };
        let camera = self.cue.map_or(self.camera, |c| Camera {
            cursor: c.to,
            ..self.camera
        });
        let mut placed = timeline::place(
            &self.moments,
            self.lanes.len(),
            &camera,
            NOMINAL.0,
            NOMINAL.1,
        );
        // Nearest first, so the front of the room fills in first.
        placed.sort_by(|a, b| a.z.abs().total_cmp(&b.z.abs()));
        let mut tasks = Vec::new();
        for p in placed {
            let Some(moment) = self.moments.get(p.index) else {
                continue;
            };
            let key = moment.frame();
            if self.thumbs.contains(&key)
                || self.pending.contains(&key)
                || self.broken.contains(&key)
            {
                continue;
            }
            self.pending.insert(key);
            let request = frames.request(key, moment.entry.media_path.clone(), THUMB_W, THUMB_H);
            tasks.push(Task::perform(request, move |a| Message::Thumb(key, a)));
        }
        Task::batch(tasks)
    }

    /// The stage shows the moment at the cursor: when that moment changes (scrubbing, a cue settling,
    /// new moments loading), it becomes the selection and its picture is loaded. Nothing happens while
    /// the moment stays the same, so a continuous scrub does not queue a decode per pixel.
    fn follow_cursor(&mut self) -> Task<Message> {
        let cursor = self.cue.map_or(self.camera.cursor, |c| c.to);
        let Some(index) = timeline::moment_at(&self.moments, cursor) else {
            return Task::none();
        };
        let entry = self.moments[index].entry.clone();
        let frame = (entry.source, entry.visual_state_id);
        if self.selection.as_ref().is_some_and(|s| s.frame == frame) {
            return Task::none();
        }
        self.selection = Some(Selection {
            frame,
            at: entry.started_at,
        });
        self.open_detail(frame, entry.media_path)
    }

    fn select(&mut self, entry: &TimelineEntry) -> Task<Message> {
        let frame = (entry.source, entry.visual_state_id);
        self.selection = Some(Selection {
            frame,
            at: entry.started_at,
        });
        if self.cue.is_none() {
            self.cue_to(entry.started_at.0 as f64);
        }
        self.open_detail(frame, entry.media_path.clone())
    }

    fn open_detail(&mut self, frame: FrameKey, media_path: String) -> Task<Message> {
        self.detail_seq += 1;
        let seq = self.detail_seq;
        if self
            .detail
            .as_ref()
            .is_some_and(|d| (d.source, d.visual_state_id) != frame)
        {
            self.detail = None;
        }
        if self
            .detail_picture
            .as_ref()
            .is_some_and(|(k, _)| *k != frame)
        {
            self.detail_picture = None;
        }
        let detail = self.ask(
            Ask::Detail {
                source: frame.0,
                id: frame.1,
            },
            move |a| Message::Detail(seq, a),
        );
        let picture = match &self.frames {
            Some(frames) => Task::perform(
                frames.request(frame, media_path, DETAIL_W, DETAIL_H),
                move |a| Message::DetailPicture(seq, frame, a),
            ),
            None => Task::none(),
        };
        Task::batch([detail, picture])
    }

    fn lane_label(&self, source: Option<SourceId>) -> String {
        self.sources
            .iter()
            .find(|s| s.source == source)
            .map_or_else(|| "unknown source".into(), source_name)
    }

    // ----- view -------------------------------------------------------------------------------

    pub fn view(&self) -> Element<'_, Message> {
        let t = self.tokens;
        // The viewer takes the whole window: nothing behind it is laid out, drawn, or focused.
        if let Some(v) = &self.viewer {
            return container(self.viewer_view(v))
                .style(move |_| t.bar())
                .width(Fill)
                .height(Fill)
                .into();
        }
        let body: Element<'_, Message> = if let Some(error) = &self.fatal {
            empty_state(t, "Could not open history.", error)
        } else if self.sources_loaded && self.sources.is_empty() {
            self.front_door()
        } else {
            row![
                self.sidebar(),
                column![
                    self.stage(),
                    container(self.room()).height(FillPortion(2)),
                    self.filament(),
                    self.transport()
                ]
                .spacing(8)
                .width(FillPortion(5)),
                column![self.results(), self.detail_pane()]
                    .spacing(10)
                    .width(FillPortion(3))
                    .max_width(460),
            ]
            .spacing(12)
            .padding([0, 14])
            .height(Fill)
            .into()
        };
        let body: Element<'_, Message> = if self.panel_open {
            stack![
                body,
                container(self.panel())
                    .align_right(Fill)
                    .padding([0, 18])
                    .height(Fill)
            ]
            .into()
        } else {
            body
        };
        container(column![self.top_bar(), body].spacing(10).padding([10, 0]))
            .style(move |_| t.bar())
            .width(Fill)
            .height(Fill)
            .into()
    }

    fn top_bar(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let mark = row![
            text("rs").size(22).color(t.text_3),
            text("Rewind").size(22).color(t.text),
        ];
        let search = text_input("Search anything you remember…", &self.query)
            .id(SEARCH_ID)
            .on_input(Message::Query)
            .padding([9, 16])
            .size(15)
            .style(move |_, status| t.search(status))
            .width(Fill);
        let status = self.navigator();
        let recording = self.recorder;
        let chip_color = match recording {
            Recorder::Recording => t.rose,
            Recorder::Error => t.gold,
            _ => t.text_3,
        };
        let chip_text = text(if recording.can_start() {
            format!("{} · Start", recording.label())
        } else {
            recording.label().to_string()
        })
        .size(12)
        .font(MONO);
        let chip: Element<'_, Message> = if recording.can_start() && !self.starting {
            button(chip_text)
                .padding([4, 10])
                .style(move |_, status| t.transport(status))
                .on_press(Message::StartRecording)
                .into()
        } else {
            container(chip_text)
                .padding([4, 10])
                .style(move |_| t.pill(chip_color))
                .into()
        };
        row![mark, search, chip, status]
            .spacing(16)
            .padding([0, 18])
            .align_y(Alignment::Center)
            .into()
    }

    /// The top-right navigator: keyframe and mark stepping around the marks list, "+ Mark", and the
    /// selection's size when there is one.
    fn navigator(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let ready = self.sources_loaded && !self.sources.is_empty();
        let nav = |label: String, message: Message| {
            let b = button(
                text(label)
                    .size(12)
                    .font(MONO)
                    .wrapping(text::Wrapping::None),
            )
            .padding([4, 8])
            .style(move |_, status| t.transport(status));
            if ready { b.on_press(message) } else { b }
        };
        let marks = button(
            text(format!(
                "Marks · {}{}",
                self.marks.len(),
                if self.panel_open { " ▴" } else { "" }
            ))
            .size(12)
            .font(MONO)
            .wrapping(text::Wrapping::None),
        )
        .padding([4, 10])
        .style(move |_, status| t.row(self.panel_open, status, t.gold))
        .on_press(Message::TogglePanel);
        let mut group = row![
            nav("« key".into(), Message::GoKeyframe(false, false)),
            nav("‹ mark".into(), Message::GoMark(false, false)),
            marks,
            nav("mark ›".into(), Message::GoMark(true, false)),
            nav("key »".into(), Message::GoKeyframe(true, false)),
            nav(
                if self.marking {
                    "…".into()
                } else {
                    "+ Mark".into()
                },
                Message::MarkNow
            ),
        ]
        .spacing(4)
        .align_y(Alignment::Center);
        if let Some((from, to)) = self.picks.span() {
            group = group
                .push(
                    button(
                        text(format!(
                            "{} picked · {}",
                            self.picks.len(),
                            gaps::duration(to.0 - from.0)
                        ))
                        .size(12)
                        .font(MONO)
                        .wrapping(text::Wrapping::None),
                    )
                    .padding([4, 10])
                    .style(move |_, status| t.row(true, status, t.mint))
                    .on_press(Message::TogglePanel),
                )
                .push(nav("✕".into(), Message::ClearPicks));
        }
        if let Some(running) = &self.exporting {
            let (done, total) = running.progress.get();
            group = group.push(
                text(format!("{} {done}/{total}", running.kind.label()))
                    .size(12)
                    .font(MONO)
                    .color(t.text_2)
                    .wrapping(text::Wrapping::None),
            );
        }
        container(group)
            .padding([2, 4])
            .style(move |_| t.pill(t.gold))
            .into()
    }

    /// The marks panel: what history holds, "Go to", a labelled mark, and every mark (jump, rename,
    /// delete). Opened from the navigator, `g` or Shift+M; Esc closes it.
    fn panel(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let moments: u64 = self.sources.iter().map(|s| s.observations).sum();
        let small = |label: &'static str, message: Message| {
            button(text(label).size(12))
                .padding([3, 8])
                .style(move |_, status| t.transport(status))
                .on_press(message)
        };
        let mut col = column![
            row![
                text(format!(
                    "{} source{} · {} moments",
                    self.sources.len(),
                    if self.sources.len() == 1 { "" } else { "s" },
                    moments
                ))
                .size(12)
                .font(MONO)
                .color(t.text_3),
                space::horizontal(),
                small("Close", Message::TogglePanel),
            ]
            .align_y(Alignment::Center),
            heading(t, "Go to"),
            row![
                text_input(
                    "14:30, yesterday 9:00, 2026-10-08 14:00, 15m…",
                    &self.goto_text
                )
                .id(GOTO_ID)
                .on_input(Message::GotoText)
                .on_submit(Message::GotoSubmit)
                .padding([6, 10])
                .size(13)
                .style(move |_, status| t.search(status)),
                small("Go", Message::GotoSubmit),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
            heading(t, "Mark the moment on stage"),
            row![
                text_input("Label (optional)", &self.mark_label)
                    .id(LABEL_ID)
                    .on_input(Message::MarkLabel)
                    .on_submit(Message::MarkNow)
                    .padding([6, 10])
                    .size(13)
                    .style(move |_, status| t.search(status)),
                small("Mark", Message::MarkNow),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        ]
        .spacing(8);
        if let Some(note) = &self.nav_note {
            col = col.push(text(note.clone()).size(12).color(t.text_2));
        }
        col = col.push(self.export_section());
        col = col.push(heading(t, &format!("Marks · {}", self.marks.len())));
        let mut list = column![].spacing(2);
        if self.marks.is_empty() {
            list = list.push(
                text("No marks yet. Press M to mark the moment on stage; [ and ] step between marks.")
                    .size(12)
                    .color(t.text_3),
            );
        }
        // Newest first: the marks made today are the ones looked for.
        for mark in self.marks.iter().rev() {
            let id = mark.id;
            let line: Element<'_, Message> = match (&self.editing, self.confirm_delete) {
                (Some((editing, label)), _) if *editing == id => row![
                    text_input("Label", label)
                        .on_input(Message::EditText)
                        .on_submit(Message::SaveEdit)
                        .padding([4, 8])
                        .size(12)
                        .style(move |_, status| t.search(status)),
                    small("Save", Message::SaveEdit),
                    small("Cancel", Message::EditMark(None)),
                ]
                .spacing(4)
                .align_y(Alignment::Center)
                .into(),
                (_, Some(confirm)) if confirm == id => row![
                    text("Delete this mark?").size(12).color(t.rose),
                    space::horizontal(),
                    small("Delete", Message::DeleteMark(id)),
                    small("Keep", Message::AskDelete(None)),
                ]
                .spacing(4)
                .align_y(Alignment::Center)
                .into(),
                _ => row![
                    button(
                        column![
                            text(format_time(mark.at.0))
                                .size(11)
                                .font(MONO)
                                .color(t.text_2),
                            text(if mark.label.is_empty() {
                                "(no label)".to_owned()
                            } else {
                                mark.label.clone()
                            })
                            .size(13)
                            .color(if mark.label.is_empty() {
                                t.text_3
                            } else {
                                t.text
                            }),
                        ]
                        .spacing(1),
                    )
                    .width(Fill)
                    .padding([5, 8])
                    .style(move |_, status| t.row(false, status, t.gold))
                    .on_press(Message::JumpMark(id)),
                    small("Rename", Message::EditMark(Some(id))),
                    small("Delete", Message::AskDelete(Some(id))),
                ]
                .spacing(4)
                .align_y(Alignment::Center)
                .into(),
            };
            list = list.push(line);
        }
        col = col.push(scrollable(list).height(Fill)).push(
            text(
                "M mark · [ ] marks · , . keyframes · ← → moments
Shift+any of these extends the selection · G go to · Esc close",
            )
            .size(11)
            .color(t.text_3),
        );
        container(col)
            .padding(12)
            .width(Length::Fixed(380.0))
            .max_height(560.0)
            .style(move |_| t.panel())
            .into()
    }

    /// Exports: the single picture (copy, PNG) and, with two or more picked, a silent clip.
    fn export_section(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let idle = self.exporting.is_none();
        let action = |label: &'static str, message: Option<Message>| {
            let b = button(text(label).size(12))
                .padding([3, 8])
                .style(move |_, status| t.transport(status));
            match message {
                Some(m) if idle => b.on_press(m),
                _ => b,
            }
        };
        let single = self.single_frame().is_some();
        let clip = self.picks.len() >= 2;
        let title = match self.picks.span() {
            Some((from, to)) if self.picks.len() > 1 => format!(
                "Export · {} picked · {} to {}",
                self.picks.len(),
                format_clock(from.0),
                format_clock(to.0)
            ),
            _ => "Export · the moment on stage".to_owned(),
        };
        let webm = action(
            "WebM",
            (clip && self.ffmpeg.is_some())
                .then_some(Message::AskExport(Some(ExportKind::Clip(ClipFormat::Webm)))),
        );
        let webm: Element<'_, Message> = if self.ffmpeg.is_some() {
            webm.into()
        } else {
            iced::widget::tooltip(
                webm,
                container(
                    text("WebM needs ffmpeg on PATH; rsRewind does not bundle a video encoder.")
                        .size(12),
                )
                .padding(6)
                .style(move |_| t.panel()),
                iced::widget::tooltip::Position::Bottom,
            )
            .into()
        };
        let mut sizes = row![text("Clip size").size(11).color(t.text_3)]
            .spacing(4)
            .align_y(Alignment::Center);
        for size in export::CLIP_SIZES {
            let selected = self.clip_size == size;
            sizes = sizes.push(
                button(text(size.to_string()).size(11).font(MONO))
                    .padding([2, 6])
                    .style(move |_, status| t.row(selected, status, t.mint))
                    .on_press(Message::ClipSize(size)),
            );
        }
        let mut col = column![
            heading(t, &title),
            row![
                action(
                    "Copy image",
                    single.then_some(Message::RunExport(ExportKind::CopyImage))
                ),
                action(
                    "Save PNG",
                    single.then_some(Message::AskExport(Some(ExportKind::Png)))
                ),
                action(
                    "GIF",
                    clip.then_some(Message::AskExport(Some(ExportKind::Clip(ClipFormat::Gif))))
                ),
                webm,
            ]
            .spacing(4)
            .align_y(Alignment::Center),
            sizes,
        ]
        .spacing(6);
        if self.picks.len() > 1 && single {
            col = col.push(
                text("Copy image and Save PNG use the moment on stage.")
                    .size(11)
                    .color(t.text_3),
            );
        }
        if !clip {
            col = col.push(
                text("Click a card, then Shift-click (or Shift+[ ] , .) to pick a stretch for a clip.")
                    .size(11)
                    .color(t.text_3),
            );
        }
        if let Some(kind) = self.confirm_export {
            col = col.push(
                column![
                    text(format!(
                        "This writes a new file to {}. Exported files are copies outside \
                         rsRewind's history: forget and retention will not delete them. Clips are \
                         silent (rsRewind records no audio).",
                        export::exports_dir(&self.data_root).display()
                    ))
                    .size(12)
                    .color(t.gold),
                    row![
                        action("Export", Some(Message::RunExport(kind))),
                        action("Cancel", Some(Message::AskExport(None))),
                    ]
                    .spacing(4),
                ]
                .spacing(4),
            );
        }
        if let Some(running) = &self.exporting {
            let (done, total) = running.progress.get();
            col = col.push(
                row![
                    text(format!("{} · {done} of {total}", running.kind.label()))
                        .size(12)
                        .color(t.text_2),
                    space::horizontal(),
                    button(text("Cancel").size(12))
                        .padding([3, 8])
                        .style(move |_, status| t.transport(status))
                        .on_press(Message::CancelExport),
                ]
                .align_y(Alignment::Center),
            );
        }
        if let Some(note) = &self.export_note {
            col = col.push(text(note.clone()).size(12).color(t.text_2));
        }
        col.into()
    }

    /// The empty window: what is happening, and the buttons that get recording going.
    fn front_door(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let (title, body) = match self.recorder {
            Recorder::Recording => (
                "Recording.",
                "Your first moments appear here within a minute. Everything stays on this computer.",
            ),
            Recorder::Paused => (
                "Recording is paused.",
                "Resume from the tray icon, and your history starts here.",
            ),
            Recorder::NotRunning | Recorder::Error => (
                "Nothing on tape yet.",
                "Start recording and rsRewind keeps a private, searchable history of this screen. Nothing leaves this computer, and you can pause or forget at any time from the tray icon.",
            ),
            Recorder::Unknown => (
                "Nothing on tape yet.",
                "Checking whether rsRewind is recording…",
            ),
        };
        let mut col = column![
            text(title).size(22).color(t.text),
            text(body)
                .size(14)
                .color(t.text_2)
                .width(Length::Fixed(520.0))
                .align_x(Alignment::Center),
        ]
        .spacing(10)
        .align_x(Alignment::Center);
        if self.recorder.can_start() {
            let start = button(
                text(if self.starting {
                    "Starting…"
                } else {
                    "Start recording"
                })
                .size(15),
            )
            .padding([10, 22])
            .style(move |_, status| t.transport(status));
            let start = if self.starting {
                start
            } else {
                start.on_press(Message::StartRecording)
            };
            let login = button(text("Start rsRewind when I log in").size(13))
                .padding([6, 14])
                .style(move |_, status| t.transport(status))
                .on_press(Message::StartAtLogin);
            col = col.push(space().height(6)).push(start).push(login);
        }
        if let Some(note) = &self.front_note {
            col = col.push(text(note.clone()).size(13).color(t.text_3));
        }
        container(col).center(Fill).into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let mut list = column![heading(t, "Sources")].spacing(4);
        let chip = |label: String, filter: SourceFilter, accent: Color, detail: String| {
            let selected = self.filter == filter;
            button(
                row![
                    container(space().width(8).height(8)).style(move |_| container::Style {
                        background: Some(accent.into()),
                        border: iced::Border {
                            radius: 4.0.into(),
                            ..iced::Border::default()
                        },
                        ..container::Style::default()
                    }),
                    column![
                        text(label).size(14).color(t.text),
                        text(detail).size(11).color(t.text_3).font(MONO),
                    ]
                    .spacing(1),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .width(Fill)
            .padding([7, 10])
            .style(move |_, status| t.row(selected, status, t.rewind))
            .on_press(Message::Filter(filter))
        };
        let total: u64 = self.sources.iter().map(|s| s.observations).sum();
        list = list.push(chip(
            "All sources".into(),
            SourceFilter::All,
            t.rewind,
            format!("{total} moments"),
        ));
        for (lane, source) in self.sources.iter().enumerate() {
            let kind = match source.kind {
                SourceKind::Local => "recorded here".to_owned(),
                SourceKind::Replica => source
                    .source
                    .map_or_else(String::new, |id| format!("probe {}", id.short())),
            };
            list = list.push(chip(
                source_name(source),
                SourceFilter::only(source.source),
                t.lane(lane),
                format!("{} · {}", kind, source.observations),
            ));
        }
        if !self.problems.is_empty() {
            list = list.push(space().height(10)).push(heading(t, "Skipped"));
            for problem in &self.problems {
                list = list.push(
                    text(format!(
                        "{}: {}",
                        problem
                            .data_dir
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        problem.reason
                    ))
                    .size(11)
                    .color(t.rose),
                );
            }
        }
        container(scrollable(list.padding(10)))
            .style(move |_| t.panel())
            .width(Length::Fixed(220.0))
            .height(Fill)
            .into()
    }

    fn room(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let lanes = self.lanes.len().max(1);
        let camera = self.camera;
        let room = responsive(move |size| {
            let viewport = shader(Room {
                moments: &self.moments,
                lanes,
                camera,
                first: self.first,
                last: self.last,
                thumbs: &self.thumbs,
                selected: self.selection.as_ref().map(|s| s.frame),
                matched: &self.matched,
                picked: &self.picked,
                keyframes: &self.keyed,
                marks: &self.mark_times,
                tokens: t,
                on_event: Message::Timeline,
            })
            .width(Fill)
            .height(Fill);

            let mut layers = stack![viewport].width(Fill).height(Fill);
            // Lane names under the front row.
            let label_y = camera.floor_y(lanes, size.width, size.height, 0.0) + 8.0;
            for source in &self.sources {
                let Some(&lane) = self.lanes.get(&source.source) else {
                    continue;
                };
                let x = camera.lane_x(lane, lanes, size.width, size.height);
                let name = source_name(source);
                // As wide as the gap to the next lane, so neighbouring names never overlap.
                let pitch = if lanes > 1 {
                    camera.lane_x(1, lanes, size.width, size.height)
                        - camera.lane_x(0, lanes, size.width, size.height)
                } else {
                    size.width
                };
                let w = (pitch - 8.0).clamp(40.0, 220.0);
                layers = layers.push(
                    pin(text(name)
                        .size(12)
                        .font(MONO)
                        .color(t.lane(self.accent(source.source)))
                        .width(w)
                        .align_x(alignment::Horizontal::Center))
                    .x(x - w / 2.0)
                    .y(label_y),
                );
            }
            // How far back each depth is.
            for z in [0.25f32, 0.5, 0.75, 1.0] {
                let y = camera.floor_y(lanes, size.width, size.height, z) - 16.0;
                layers = layers.push(
                    pin(text(format!("−{}", age_label(camera.age_at(f64::from(z)))))
                        .size(11)
                        .font(MONO)
                        .color(t.text_3))
                    .x(14.0)
                    .y(y),
                );
            }
            layers = layers.push(
                pin(column![
                    text(format_time(camera.cursor as i64))
                        .size(15)
                        .font(MONO)
                        .color(t.text),
                    text("now ← deeper is earlier").size(11).color(t.text_3),
                ]
                .spacing(2))
                .x(16.0)
                .y(12.0),
            );
            // The nearest marks' labels, at the right end of their floor line.
            let mut labelled: Vec<(f32, &Mark)> = self
                .marks
                .iter()
                .filter_map(|m| {
                    let z = camera.depth(m.at.0, m.at.0).filter(|z| *z >= 0.0)?;
                    Some((z, m))
                })
                .collect();
            labelled.sort_by(|a, b| a.0.total_cmp(&b.0));
            // Nearest first; a label that would overlap one already placed is left out.
            let mut taken: Vec<f32> = Vec::new();
            for (z, mark) in labelled {
                if taken.len() >= ROOM_MARK_LABELS {
                    break;
                }
                let y = camera.floor_y(lanes, size.width, size.height, z) - 17.0;
                if taken.iter().any(|t| (t - y).abs() < 14.0) {
                    continue;
                }
                taken.push(y);
                let label = if mark.label.is_empty() {
                    format!("◆ {}", format_clock(mark.at.0))
                } else {
                    format!("◆ {}", mark.label)
                };
                layers = layers.push(
                    pin(text(label)
                        .size(11)
                        .color(t.gold)
                        .width(220.0)
                        .wrapping(text::Wrapping::None)
                        .align_x(alignment::Horizontal::Right))
                    .x((size.width - 236.0).max(0.0))
                    .y(y),
                );
            }
            let notes = self.gap_notes();
            if !notes.is_empty() {
                let lines = notes.into_iter().map(|(line, warning)| {
                    text(line)
                        .size(12)
                        .color(if warning { t.rose } else { t.text_2 })
                        .into()
                });
                layers = layers.push(
                    pin(container(column(lines).spacing(2))
                        .padding([6, 10])
                        .style(move |_| t.panel()))
                    .x(16.0)
                    .y(56.0),
                );
            }
            if let Some(index) = self.hovered
                && let Some(moment) = self.moments.get(index)
            {
                layers = layers.push(
                    pin(container(
                        column![
                            text(format!(
                                "{} · {}",
                                self.lane_label(moment.entry.source),
                                format_time(moment.entry.started_at.0)
                            ))
                            .size(12)
                            .font(MONO)
                            .color(t.text_2),
                            text(app_and_window(
                                moment.entry.application.as_deref(),
                                moment.entry.window_title.as_deref()
                            ))
                            .size(13)
                            .color(t.text),
                        ]
                        .spacing(2),
                    )
                    .padding([6, 10])
                    .style(move |_| t.panel()))
                    .x((size.width - 420.0).max(16.0))
                    .y(12.0),
                );
            }
            if self.moments.is_empty() && self.sources_loaded {
                layers = layers.push(
                    container(text("Nothing recorded around this moment.").color(t.text_3))
                        .center(Fill),
                );
            }
            layers.into()
        });
        container(room)
            .style(move |_| t.panel())
            .width(Fill)
            .height(Fill)
            .padding(1)
            .into()
    }

    fn filament(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let filament = Filament::new(self.first as i64, self.last as i64);
        let marks: Vec<(f64, Color)> = self
            .moments
            .iter()
            .map(|m| (m.entry.started_at.0 as f64, t.lane(m.accent)))
            .collect();
        let gaps = self
            .gaps
            .iter()
            .filter(|g| self.filter.admits(g.source))
            .map(|g| {
                let color = if gaps::is_warning(g.reason) {
                    t.rose
                } else {
                    t.text_3
                };
                (g.from.0 as f64, g.to.0 as f64, color)
            })
            .collect();
        let strip = shader(Strip {
            filament,
            gaps,
            marks,
            stops: self.keyframes.iter().map(|k| k.at.0 as f64).collect(),
            flags: self.marks.iter().map(|m| m.at.0 as f64).collect(),
            picked: self.picks.span().map(|(a, b)| (a.0 as f64, b.0 as f64)),
            cursor: self.camera.cursor,
            tokens: t,
            on_event: Message::Timeline,
        })
        .width(Fill)
        .height(Length::Fixed(30.0));
        container(strip)
            .style(move |_| t.panel())
            .width(Fill)
            .into()
    }

    fn transport(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let control = |label: &'static str, message: Message| {
            button(text(label).size(13))
                .padding([5, 14])
                .style(move |_, status| t.transport(status))
                .on_press(message)
        };
        row![
            // Plain arrows: Windows' font fallback draws the triangles as emoji tiles.
            control("←  Earlier", Message::Step(false)),
            control("Later  →", Message::Step(true)),
            control("Latest", Message::Latest),
            space::horizontal(),
            text(format!("depth {}", age_label(self.camera.depth_ms)))
                .wrapping(text::Wrapping::None)
                .size(12)
                .font(MONO)
                .color(t.text_3),
            control("−", Message::Zoom(0.5)),
            control("+", Message::Zoom(2.0)),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
    }

    fn results(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let mut list = column![].spacing(4);
        if let Some(error) = &self.search_error {
            list = list.push(text(error.clone()).size(12).color(t.rose));
        } else if self.query.trim().is_empty() {
            list = list.push(
                text("Search recognized text. Pick a result to cue the room to that moment.")
                    .size(13)
                    .color(t.text_3),
            );
        } else if self.hits.is_empty() {
            list = list.push(
                column![
                    text("No moment found.").size(15).color(t.text),
                    text("Try another phrase, application, or time range.")
                        .size(12)
                        .color(t.text_3),
                ]
                .spacing(4),
            );
        }
        for (index, hit) in self.hits.iter().enumerate() {
            let lane = self.accent(hit.source);
            let selected = self.selected_hit == Some(index);
            let header = row![
                text(format_time(hit.timestamp.0))
                    .size(11)
                    .font(MONO)
                    .color(t.text_2),
                space::horizontal(),
                text(self.lane_label(hit.source))
                    .size(11)
                    .font(MONO)
                    .color(t.lane(lane)),
            ];
            let entry = column![
                header,
                text(app_and_window(
                    hit.application.as_deref(),
                    hit.window_title.as_deref()
                ))
                .size(13)
                .color(t.text),
                snippet(t, &hit.snippet),
            ]
            .spacing(3);
            list = list.push(
                button(entry)
                    .width(Fill)
                    .padding([8, 10])
                    .style(move |_, status| t.row(selected, status, t.rewind))
                    .on_press(Message::Cue(index)),
            );
        }
        let title = if self.hits.is_empty() {
            "Indexed moments".to_owned()
        } else {
            format!("Indexed moments · {}", self.hits.len())
        };
        container(column![heading(t, &title), scrollable(list).height(Fill)].spacing(6))
            .padding(10)
            .style(move |_| t.panel())
            .width(Fill)
            .height(FillPortion(2))
            .into()
    }

    /// The current moment's picture, as large as the window allows: what the room is pointing at.
    /// Double-click opens it full window.
    fn stage(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let body: Element<'_, Message> = match (&self.selection, &self.detail_picture) {
            (Some(selection), Some((key, handle))) if *key == selection.frame => mouse_area(
                image(handle.clone())
                    .content_fit(ContentFit::Contain)
                    .width(Fill)
                    .height(Fill),
            )
            .on_double_click(Message::OpenSelected)
            .interaction(iced::mouse::Interaction::ZoomIn)
            .into(),
            (Some(_), _) => container(text("Loading picture…").size(13).color(t.text_3))
                .center(Fill)
                .into(),
            (None, _) => container(
                text("Scrub the strip or step with ← → to put a moment here.")
                    .size(13)
                    .color(t.text_3),
            )
            .center(Fill)
            .into(),
        };
        container(body)
            .padding(6)
            .style(move |_| t.panel())
            .width(Fill)
            .height(FillPortion(5))
            .into()
    }

    fn detail_pane(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let Some(selection) = &self.selection else {
            return container(
                text("Scrub, step or search: the moment on screen is described here.")
                    .size(13)
                    .color(t.text_3),
            )
            .padding(14)
            .style(move |_| t.panel())
            .width(Fill)
            .height(FillPortion(3))
            .into();
        };
        let mut info = column![
            text(format_time(selection.at.0))
                .size(14)
                .font(MONO)
                .color(t.text),
            text(self.lane_label(selection.frame.0))
                .size(12)
                .color(t.lane(self.accent(selection.frame.0))),
        ]
        .spacing(4);
        if let Some(detail) = &self.detail {
            info = info.push(
                text(app_and_window(
                    detail.application.as_deref(),
                    detail.window_title.as_deref(),
                ))
                .size(13)
                .color(t.text),
            );
            let ocr: Element<'_, Message> = if detail.ocr_text.is_empty() {
                text(match detail.ocr_status.as_str() {
                    "pending" => "Text not recognized yet.",
                    "failed" => "Text recognition failed for this moment.",
                    "skipped" => "Text recognition was skipped for this moment.",
                    _ => "No text on this screen.",
                })
                .size(12)
                .color(t.text_3)
                .into()
            } else {
                text(detail.ocr_text.clone())
                    .size(12)
                    .color(t.text_2)
                    .width(Fill)
                    .into()
            };
            info = info
                .push(heading(t, "Recognized text"))
                .push(scrollable(ocr).height(Fill));
        }
        container(info.height(Fill))
            .padding(12)
            .style(move |_| t.panel())
            .width(Fill)
            .height(FillPortion(3))
            .into()
    }
}

fn heading<'a>(t: Tokens, label: &str) -> Element<'a, Message> {
    text(label.to_uppercase())
        .size(11)
        .color(t.text_3)
        .font(MONO)
        .into()
}

fn empty_state<'a>(t: Tokens, title: &str, body: &str) -> Element<'a, Message> {
    container(
        column![
            text(title.to_owned()).size(22).color(t.text),
            text(body.to_owned()).size(14).color(t.text_2),
        ]
        .spacing(8)
        .align_x(Alignment::Center),
    )
    .center(Fill)
    .into()
}

/// The snippet with its `[match]` markers rendered as emphasis.
fn snippet<'a>(t: Tokens, raw: &str) -> Element<'a, Message> {
    let spans: Vec<text::Span<'a, (), Font>> = split_matches(raw)
        .into_iter()
        .map(|(part, hit)| {
            let s = span(part).size(12.0);
            if hit {
                s.color(t.gold)
            } else {
                s.color(t.text_2)
            }
        })
        .collect();
    rich_text(spans).into()
}

/// Ctrl+F (Cmd+F on macOS) or a bare `/`.
fn is_find_key(key: keyboard::Key<&str>, modifiers: keyboard::Modifiers) -> bool {
    match key {
        keyboard::Key::Character("f" | "F") => modifiers.command(),
        keyboard::Key::Character("/") => !modifiers.command() && !modifiers.alt(),
        _ => false,
    }
}

/// `"a [b] c"` -> `[("a ", false), ("b", true), (" c", false)]`.
pub(crate) fn split_matches(raw: &str) -> Vec<(String, bool)> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut inside = false;
    for ch in raw.chars() {
        match (ch, inside) {
            ('[', false) | (']', true) => {
                if !current.is_empty() {
                    parts.push((std::mem::take(&mut current), inside));
                }
                inside = !inside;
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        parts.push((current, inside));
    }
    parts
}

fn source_name(source: &SourceInfo) -> String {
    match (&source.kind, &source.label) {
        (SourceKind::Local, _) => "This machine".into(),
        (SourceKind::Replica, Some(label)) => label.clone(),
        (SourceKind::Replica, None) => source.source.map_or_else(
            || "unnamed probe".into(),
            |id| format!("probe {}", id.short()),
        ),
    }
}

fn app_and_window(app: Option<&str>, window: Option<&str>) -> String {
    let app = app.map(|a| {
        a.strip_suffix(".exe")
            .or_else(|| a.strip_suffix(".EXE"))
            .unwrap_or(a)
    });
    // Most window titles already end with the application's name ("Inbox — Mail"); say it once.
    if let (Some(a), Some(w)) = (app, window)
        && !a.is_empty()
        && w.to_lowercase().contains(&a.to_lowercase())
    {
        return w.to_owned();
    }
    match (app, window) {
        (Some(a), Some(w)) if !w.is_empty() => format!("{a} — {w}"),
        (Some(a), _) => a.to_owned(),
        (None, Some(w)) => w.to_owned(),
        (None, None) => "Unknown application".into(),
    }
}

fn format_clock(ms: i64) -> String {
    Timestamp(ms).to_local().format("%H:%M:%S").to_string()
}

/// "Mark · standup · Sat Oct 11 · 09:30:00".
fn mark_title(mark: &Mark) -> String {
    if mark.label.is_empty() {
        format!("Mark · {}", format_time(mark.at.0))
    } else {
        format!("Mark · {} · {}", mark.label, format_time(mark.at.0))
    }
}

fn format_time(ms: i64) -> String {
    Timestamp(ms)
        .to_local()
        .format("%a %b %-d · %H:%M:%S")
        .to_string()
}

/// `90_000.0` -> `"1.5 min"`.
pub(crate) fn age_label(ms: f64) -> String {
    let s = ms / 1000.0;
    let (value, unit) = if s < 90.0 {
        (s, "s")
    } else if s < 90.0 * 60.0 {
        (s / 60.0, "min")
    } else if s < 36.0 * 3600.0 {
        (s / 3600.0, "h")
    } else {
        (s / 86_400.0, "d")
    };
    if value < 10.0 && unit != "s" {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.0} {unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_split_on_match_markers() {
        assert_eq!(
            split_matches("order 8812 [konica] toner"),
            vec![
                ("order 8812 ".to_owned(), false),
                ("konica".to_owned(), true),
                (" toner".to_owned(), false)
            ]
        );
        assert_eq!(
            split_matches("[a][b]"),
            vec![("a".to_owned(), true), ("b".to_owned(), true)]
        );
        assert!(split_matches("").is_empty());
    }

    #[test]
    fn the_application_is_named_once() {
        assert_eq!(
            app_and_window(Some("Huddle.exe"), Some("#facilities — Huddle")),
            "#facilities — Huddle"
        );
        assert_eq!(
            app_and_window(Some("EXCEL.EXE"), Some("Budget.xlsx")),
            "EXCEL — Budget.xlsx"
        );
        assert_eq!(app_and_window(Some("pwsh.exe"), None), "pwsh");
        assert_eq!(app_and_window(None, None), "Unknown application");
    }

    #[test]
    fn find_shortcuts() {
        use keyboard::{Key, Modifiers};
        assert!(is_find_key(Key::Character("f"), Modifiers::COMMAND));
        assert!(!is_find_key(Key::Character("f"), Modifiers::empty()));
        assert!(is_find_key(Key::Character("/"), Modifiers::empty()));
        assert!(!is_find_key(Key::Character("/"), Modifiers::COMMAND));
        assert!(!is_find_key(Key::Character("g"), Modifiers::COMMAND));
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age_label(12_000.0), "12 s");
        assert_eq!(age_label(300_000.0), "5.0 min");
        assert_eq!(age_label(3_600_000.0 * 5.0), "5.0 h");
        assert_eq!(age_label(86_400_000.0 * 30.0), "30 d");
    }
}
