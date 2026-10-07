//! The rsRewind window: search bar, source filter, results, the rewind room, and the detail pane.
//!
//! Search is navigation into time (`docs/design/brief.md`): picking a result cues the room to the
//! moment the text was on screen, in its source's lane, and opens that moment in the detail pane.
//! Every query runs on the history worker; the update loop only sends questions and applies
//! answers, so the window stays responsive whatever SQLite is doing.

use crate::style::{MONO, Tokens, UI_FONT};
use crate::thumb::Bgra;
use crate::timeline::cache::ByteLru;
use crate::timeline::model::{self, Camera, Filament};
use crate::timeline::{self, FrameKey, Moment, Room, Strip};
use crate::worker::{Answer, Ask, FrameAnswer, FramePool, HistoryWorker};
use iced::widget::{
    button, column, container, image, pin, responsive, rich_text, row, scrollable, shader, space,
    span, stack, text, text_input,
};
use iced::{
    Alignment, Color, ContentFit, Element, Fill, FillPortion, Font, Length, Subscription, Task,
    alignment, keyboard, theme, window,
};
use rsrewind_core::{
    DataDir, SearchHit, SearchQuery, SourceId, TimelineEntry, Timestamp, VisualDetail,
};
use rsrewind_query::{SourceFilter, SourceInfo, SourceKind, SourceProblem};
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
/// The short settle when cueing to a moment (the brief: ~100-180 ms, no fake scrubbing).
const CUE_MS: f32 = 180.0;
const SEARCH_LIMIT: u32 = 100;

pub fn run(data: DataDir) -> iced::Result {
    iced::application(move || App::boot(data.clone()), App::update, App::view)
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
    Thumb(FrameKey, FrameAnswer),
    Timeline(timeline::Event),
    Cue(usize),
    Cued(u64, Answer),
    Detail(u64, Answer),
    DetailPicture(u64, FrameKey, FrameAnswer),
    Step(bool),
    Latest,
    Zoom(f64),
    Frame(Instant),
    Key(keyboard::Event),
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Cue {
    from: f64,
    to: f64,
    started: Instant,
}

#[derive(Debug, Clone, PartialEq)]
struct Selection {
    frame: FrameKey,
    at: Timestamp,
}

pub struct App {
    tokens: Tokens,
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
    cue: Option<Cue>,
    window_seq: u64,
    window_center: Option<f64>,
    moments: Vec<Moment>,
    hovered: Option<usize>,

    thumbs: ByteLru<FrameKey, Arc<Bgra>>,
    pending: HashSet<FrameKey>,
    broken: HashSet<FrameKey>,

    selection: Option<Selection>,
    detail_seq: u64,
    detail: Option<VisualDetail>,
    detail_picture: Option<(FrameKey, image::Handle)>,
    cue_seq: u64,
}

impl App {
    fn boot(data: DataDir) -> (Self, Task<Message>) {
        let (worker, frames, fatal) = match HistoryWorker::start(data) {
            Ok((worker, frames)) => (Some(worker), Some(frames), None),
            Err(error) => (None, None, Some(format!("could not start: {error}"))),
        };
        let now = Timestamp::now().0 as f64;
        let app = Self {
            tokens: Tokens::DARK,
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
            thumbs: ByteLru::new(),
            pending: HashSet::new(),
            broken: HashSet::new(),
            selection: None,
            detail_seq: 0,
            detail: None,
            detail_picture: None,
            cue_seq: 0,
        };
        let sources = app.ask(Ask::Sources, Message::Sources);
        (
            app,
            Task::batch([sources, iced::system::theme().map(Message::Appearance)]),
        )
    }

    fn title(&self) -> String {
        "rsRewind".into()
    }

    fn theme(&self) -> iced::Theme {
        self.tokens.theme()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            keyboard::listen().map(Message::Key),
            iced::system::theme_changes().map(Message::Appearance),
        ];
        if self.cue.is_some() {
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
                self.tokens = Tokens::for_mode(mode);
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
                self.refresh_window(true)
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
                Task::batch([self.refresh_window(true), self.search()])
            }
            Message::Window(seq, answer) => {
                if seq != self.window_seq {
                    return Task::none();
                }
                if let Answer::Window(entries) = answer {
                    self.set_moments(entries);
                }
                self.load_thumbs()
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
                Task::batch([self.refresh_window(false), self.load_thumbs()])
            }
            Message::Timeline(timeline::Event::Hover(index)) => {
                self.hovered = index;
                Task::none()
            }
            Message::Timeline(timeline::Event::Select(index)) => {
                let Some(moment) = self.moments.get(index) else {
                    return Task::none();
                };
                let entry = moment.entry.clone();
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
            Message::Step(forward) => {
                let starts: Vec<i64> = self.moments.iter().map(|m| m.entry.started_at.0).collect();
                let Some(target) = model::step(&starts, self.camera.cursor, forward) else {
                    return Task::none();
                };
                let entry = self
                    .moments
                    .iter()
                    .filter(|m| m.entry.started_at.0 == target)
                    .max_by_key(|m| m.entry.cursor())
                    .map(|m| m.entry.clone());
                self.cue_to(target as f64);
                match entry {
                    Some(entry) => Task::batch([self.select(&entry), self.refresh_window(false)]),
                    None => self.refresh_window(false),
                }
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
                if let Some(cue) = self.cue {
                    let t = (now.saturating_duration_since(cue.started).as_secs_f32() * 1000.0
                        / CUE_MS)
                        .clamp(0.0, 1.0);
                    let eased = 1.0 - (1.0 - t).powi(3);
                    self.camera.cursor = cue.from + (cue.to - cue.from) * f64::from(eased);
                    if t >= 1.0 {
                        self.camera.cursor = cue.to;
                        self.cue = None;
                        return self.load_thumbs();
                    }
                }
                Task::none()
            }
            Message::Key(keyboard::Event::KeyPressed { key, .. }) => match key.as_ref() {
                keyboard::Key::Named(keyboard::key::Named::ArrowLeft)
                | keyboard::Key::Named(keyboard::key::Named::ArrowDown) => {
                    self.update(Message::Step(false))
                }
                keyboard::Key::Named(keyboard::key::Named::ArrowRight)
                | keyboard::Key::Named(keyboard::key::Named::ArrowUp) => {
                    self.update(Message::Step(true))
                }
                keyboard::Key::Named(keyboard::key::Named::End) => self.update(Message::Latest),
                _ => Task::none(),
            },
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

    /// Jumps to `target` with a short settle: the camera starts a little after the moment and
    /// glides back onto it, instead of flying through everything in between.
    fn cue_to(&mut self, target: f64) {
        let target = target.clamp(self.first, self.last);
        let from = (target + self.camera.age_at(0.12)).min(self.last.max(target));
        self.camera.cursor = from;
        self.cue = Some(Cue {
            from,
            to: target,
            started: Instant::now(),
        });
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
        let body: Element<'_, Message> = if let Some(error) = &self.fatal {
            empty_state(t, "Could not open history.", error)
        } else if self.sources_loaded && self.sources.is_empty() {
            empty_state(
                t,
                "Nothing on tape yet.",
                "rsRewind will start building your local history once recording begins.",
            )
        } else {
            row![
                self.sidebar(),
                column![self.room(), self.filament(), self.transport()]
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
            .on_input(Message::Query)
            .padding([9, 16])
            .size(15)
            .style(move |_, status| t.search(status))
            .width(Fill);
        let moments: u64 = self.sources.iter().map(|s| s.observations).sum();
        let status = container(
            text(format!(
                "{} source{} · {} moments",
                self.sources.len(),
                if self.sources.len() == 1 { "" } else { "s" },
                moments
            ))
            .size(12)
            .font(MONO),
        )
        .padding([4, 10])
        .style(move |_| t.pill(t.mint));
        row![mark, search, status]
            .spacing(16)
            .padding([0, 18])
            .align_y(Alignment::Center)
            .into()
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
        let strip = shader(Strip {
            filament,
            marks,
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
            control("◀  Earlier", Message::Step(false)),
            control("Later  ▶", Message::Step(true)),
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

    fn detail_pane(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let Some(selection) = &self.selection else {
            return container(
                text("Select a moment in the room or a search result to inspect it.")
                    .size(13)
                    .color(t.text_3),
            )
            .padding(14)
            .style(move |_| t.panel())
            .width(Fill)
            .height(FillPortion(3))
            .into();
        };
        let picture: Element<'_, Message> = match &self.detail_picture {
            Some((key, handle)) if *key == selection.frame => image(handle.clone())
                .content_fit(ContentFit::Contain)
                .width(Fill)
                .height(Length::Fixed(200.0))
                .into(),
            _ => container(text("Loading picture…").size(12).color(t.text_3))
                .center_x(Fill)
                .height(Length::Fixed(200.0))
                .into(),
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
                    .into()
            };
            info = info
                .push(heading(t, "Recognized text"))
                .push(scrollable(ocr).height(Fill));
        }
        container(column![picture, info.height(Fill)].spacing(10))
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
    let app = app.map(|a| a.strip_suffix(".exe").unwrap_or(a));
    match (app, window) {
        (Some(a), Some(w)) if !w.is_empty() => format!("{a} — {w}"),
        (Some(a), _) => a.to_owned(),
        (None, Some(w)) => w.to_owned(),
        (None, None) => "Unknown application".into(),
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
    fn ages_read_naturally() {
        assert_eq!(age_label(12_000.0), "12 s");
        assert_eq!(age_label(300_000.0), "5.0 min");
        assert_eq!(age_label(3_600_000.0 * 5.0), "5.0 h");
        assert_eq!(age_label(86_400_000.0 * 30.0), "30 d");
    }
}
