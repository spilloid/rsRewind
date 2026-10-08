//! The application side of the full-window image viewer: opening it from the room or the detail
//! pane, decoding the full-resolution picture off the UI thread, stepping through one lane, and
//! drawing its header and hints. The view math lives in `crate::viewer::model`.
//!
//! Memory: the viewer holds at most one full-resolution picture (reduced to fit
//! [`FULL_MAX`] x [`FULL_MAX`] if larger). Stepping drops it before asking for the next one and
//! aborts a decode still in flight, so the decoder skips it; the GPU frees a full-resolution
//! texture on the frame it leaves the screen.

use super::{App, Message, format_time, source_name};
use crate::style::{MONO, Tokens};
use crate::thumb::Bgra;
use crate::timeline::FrameKey;
use crate::viewer::{self, Viewer, model::Zoom};
use crate::worker::{Answer, Ask, FrameAnswer};
use iced::widget::{button, column, container, row, shader, space, stack, text};
use iced::{Alignment, Element, Fill, Task, keyboard, task, window};
use rsrewind_core::{TimelineCursor, TimelineEntry, Timestamp, VisualDetail};
use rsrewind_query::SourceFilter;
use std::sync::Arc;

/// Full-resolution pictures larger than this (in either direction) are reduced to fit: a 4K
/// screen arrives untouched; an 8K one is halved instead of costing 130 MB of texture.
pub(super) const FULL_MAX: u32 = 4096;
/// Moments asked for when stepping, so repeated observations of the same picture can be skipped.
const STEP_LOOKAHEAD: u32 = 24;

pub(super) struct ViewerState {
    pub frame: FrameKey,
    pub at: Timestamp,
    /// Where this moment sits in the timeline order, for stepping.
    pub cursor: TimelineCursor,
    pub zoom: Zoom,
    pub full: Option<Arc<Bgra>>,
    /// Size of the previous picture, to keep the zoom when stepping between same-sized screens.
    pub previous_size: Option<(u32, u32)>,
    pub detail: Option<VisualDetail>,
    /// The viewer's drawing area (logical px), once laid out.
    pub size: Option<(f32, f32)>,
    pub seq: u64,
    pub decode: Option<task::Handle>,
    pub stepping: bool,
    pub notice: Option<&'static str>,
    pub failed: bool,
}

impl App {
    /// Opens (or switches the open viewer to) one moment.
    pub(super) fn open_viewer(
        &mut self,
        frame: FrameKey,
        at: Timestamp,
        cursor: TimelineCursor,
        media_path: String,
    ) -> Task<Message> {
        self.viewer_seq += 1;
        let seq = self.viewer_seq;
        let (zoom, previous_size, size) = match self.viewer.take() {
            Some(mut old) => {
                if let Some(handle) = old.decode.take() {
                    handle.abort();
                }
                let previous = old.full.as_ref().map(|p| (p.width, p.height));
                (old.zoom, previous, old.size)
            }
            None => (Zoom::Fit, None, None),
        };
        self.viewer = Some(ViewerState {
            frame,
            at,
            cursor,
            zoom,
            full: None,
            previous_size,
            detail: self
                .detail
                .as_ref()
                .filter(|d| (d.source, d.visual_state_id) == frame)
                .cloned(),
            size,
            seq,
            decode: None,
            stepping: false,
            notice: None,
            failed: false,
        });
        let mut tasks = vec![
            window::latest()
                .and_then(window::scale_factor)
                .map(Message::ScaleFactor),
            self.ask(
                Ask::Detail {
                    source: frame.0,
                    id: frame.1,
                },
                move |a| Message::ViewerDetail(seq, a),
            ),
        ];
        if let Some(frames) = &self.frames {
            let (decode, handle) = Task::perform(
                frames.request(frame, media_path, FULL_MAX, FULL_MAX),
                move |a| Message::ViewerPicture(seq, a),
            )
            .abortable();
            if let Some(v) = self.viewer.as_mut() {
                v.decode = Some(handle);
            }
            tasks.push(decode);
        }
        tracing::debug!(seq, "viewer opened");
        Task::batch(tasks)
    }

    /// Double-click on a card in the room.
    pub(super) fn open_from_room(&mut self, frame: FrameKey) -> Task<Message> {
        let Some(entry) = self
            .moments
            .iter()
            .find(|m| m.frame() == frame)
            .map(|m| m.entry.clone())
        else {
            return Task::none();
        };
        self.open_viewer(frame, entry.started_at, entry.cursor(), entry.media_path)
    }

    /// Double-click on the detail pane's picture.
    pub(super) fn open_selected(&mut self) -> Task<Message> {
        let Some(selection) = self.selection.clone() else {
            return Task::none();
        };
        let frame = selection.frame;
        let entry = self
            .moments
            .iter()
            .find(|m| m.frame() == frame)
            .map(|m| m.entry.clone());
        let media_path = entry
            .as_ref()
            .map(|e| e.media_path.clone())
            .or_else(|| {
                self.detail
                    .as_ref()
                    .filter(|d| (d.source, d.visual_state_id) == frame)
                    .map(|d| d.media_path.clone())
            })
            .or_else(|| {
                self.hits
                    .iter()
                    .find(|h| (h.source, h.visual_state_id) == frame)
                    .map(|h| h.media_path.clone())
            });
        let Some(media_path) = media_path else {
            return Task::none();
        };
        let cursor = entry.as_ref().map_or_else(
            || TimelineCursor::at_or_before(selection.at),
            TimelineEntry::cursor,
        );
        self.open_viewer(frame, selection.at, cursor, media_path)
    }

    /// Closes the viewer and leaves the room and detail pane on the moment it was showing.
    pub(super) fn close_viewer(&mut self) -> Task<Message> {
        let Some(mut viewer) = self.viewer.take() else {
            return Task::none();
        };
        if let Some(handle) = viewer.decode.take() {
            handle.abort();
        }
        tracing::debug!(seq = viewer.seq, "viewer closed");
        if self
            .selection
            .as_ref()
            .is_some_and(|s| s.frame == viewer.frame)
        {
            return Task::none();
        }
        // The viewer stepped: follow it.
        match self
            .moments
            .iter()
            .find(|m| m.frame() == viewer.frame)
            .map(|m| m.entry.clone())
        {
            Some(entry) => {
                self.cue = None;
                Task::batch([self.select(&entry), self.refresh_window(true)])
            }
            None => {
                self.cue_to(viewer.at.0 as f64);
                self.refresh_window(true)
            }
        }
    }

    pub(super) fn viewer_picture(&mut self, seq: u64, answer: FrameAnswer) -> Task<Message> {
        let Some(v) = self.viewer.as_mut().filter(|v| v.seq == seq) else {
            return Task::none();
        };
        v.decode = None;
        match answer {
            FrameAnswer::Ready(picture) => {
                let size = (picture.width, picture.height);
                if v.previous_size.is_some_and(|p| p != size) {
                    v.zoom = Zoom::Fit;
                }
                tracing::debug!(seq, width = size.0, height = size.1, "viewer picture ready");
                v.full = Some(picture);
            }
            FrameAnswer::Failed(reason) => {
                tracing::debug!(%reason, "viewer picture unavailable");
                v.failed = true;
            }
            FrameAnswer::Dropped => v.failed = true,
        }
        Task::none()
    }

    pub(super) fn viewer_detail(&mut self, seq: u64, answer: Answer) -> Task<Message> {
        if let Some(v) = self.viewer.as_mut().filter(|v| v.seq == seq)
            && let Answer::Detail(detail) = answer
        {
            v.detail = detail;
        }
        Task::none()
    }

    pub(super) fn viewer_event(&mut self, event: viewer::Event) -> Task<Message> {
        let Some(v) = self.viewer.as_mut() else {
            return Task::none();
        };
        match event {
            viewer::Event::Zoom(zoom) => v.zoom = zoom,
            viewer::Event::Resized(w, h) => v.size = Some((w, h)),
            viewer::Event::Close => return self.close_viewer(),
        }
        Task::none()
    }

    /// Asks for the neighbouring moment in the open picture's lane.
    pub(super) fn viewer_step(&mut self, forward: bool) -> Task<Message> {
        let Some(v) = self.viewer.as_mut() else {
            return Task::none();
        };
        if v.stepping {
            return Task::none();
        }
        v.stepping = true;
        v.notice = None;
        let (seq, from, source) = (v.seq, v.cursor, v.frame.0);
        self.ask(
            Ask::Step {
                filter: SourceFilter::only(source),
                from,
                forward,
                limit: STEP_LOOKAHEAD,
            },
            move |a| Message::ViewerStepped(seq, forward, a),
        )
    }

    pub(super) fn viewer_stepped(
        &mut self,
        seq: u64,
        forward: bool,
        answer: Answer,
    ) -> Task<Message> {
        let Some(v) = self.viewer.as_mut().filter(|v| v.seq == seq) else {
            return Task::none();
        };
        v.stepping = false;
        let Answer::Step(entries) = answer else {
            return Task::none();
        };
        // The same picture is often observed several times in a row; step to a different one.
        let current = v.frame;
        let Some(next) = entries
            .into_iter()
            .find(|e| (e.source, e.visual_state_id) != current)
        else {
            v.notice = Some(if forward {
                "This is the latest moment in this lane."
            } else {
                "This is the earliest moment in this lane."
            });
            return Task::none();
        };
        let frame = (next.source, next.visual_state_id);
        self.open_viewer(frame, next.started_at, next.cursor(), next.media_path)
    }

    /// Keys while the viewer is open. `true` if the key was the viewer's.
    pub(super) fn viewer_key(&mut self, key: keyboard::Key<&str>) -> Option<Task<Message>> {
        use keyboard::key::Named;
        let v = self.viewer.as_ref()?;
        let size = v.size.unwrap_or((1280.0, 800.0));
        let area = viewer::image_area(size.0, size.1);
        let centre = (area.0 / 2.0, area.1 / 2.0);
        let image = v.full.as_ref().map(|p| (p.width as f32, p.height as f32));
        let actual = self.actual_scale();
        let zoom = v.zoom;
        let set = |app: &mut App, z: Zoom| {
            if let Some(v) = app.viewer.as_mut() {
                v.zoom = z;
            }
            Task::none()
        };
        Some(match key {
            keyboard::Key::Named(Named::Escape) => self.close_viewer(),
            keyboard::Key::Named(Named::ArrowLeft) => self.viewer_step(false),
            keyboard::Key::Named(Named::ArrowRight) => self.viewer_step(true),
            keyboard::Key::Character("0") => set(self, Zoom::Fit),
            keyboard::Key::Character("1") => match image {
                Some(image) => set(
                    self,
                    viewer::model::actual_size(zoom, centre, image, area, actual),
                ),
                None => Task::none(),
            },
            keyboard::Key::Character("+" | "=") | keyboard::Key::Character("-") => {
                let factor = if matches!(key, keyboard::Key::Character("-")) {
                    0.8
                } else {
                    1.25
                };
                match image {
                    Some(image) => set(
                        self,
                        viewer::model::zoom_at(zoom, factor, centre, image, area, actual),
                    ),
                    None => Task::none(),
                }
            }
            // Everything else is swallowed while the viewer is open, so the room behind it does
            // not move.
            _ => Task::none(),
        })
    }

    /// Logical pixels per image pixel at 100 %.
    pub(super) fn actual_scale(&self) -> f32 {
        if self.scale_factor.is_finite() && self.scale_factor > 0.0 {
            1.0 / self.scale_factor
        } else {
            1.0
        }
    }

    /// The current search's matches on the open picture, as fractions of it.
    fn outlines(&self, v: &ViewerState) -> Vec<[f32; 4]> {
        let Some(detail) = v
            .detail
            .as_ref()
            .filter(|d| (d.source, d.visual_state_id) == v.frame)
        else {
            return Vec::new();
        };
        let captured = (detail.width as f32, detail.height as f32);
        viewer::model::matching_blocks(&detail.blocks, &self.query)
            .into_iter()
            .filter_map(|i| detail.blocks.get(i))
            .filter_map(|b| viewer::model::block_rect(b, captured, (1.0, 1.0)))
            .collect()
    }

    pub(super) fn viewer_view<'a>(&'a self, v: &'a ViewerState) -> Element<'a, Message> {
        // A lightbox: dark in both appearances, so the screenshot is what stands out.
        let t = Tokens::DARK;
        let outlines = self.outlines(v);
        let matches = outlines.len();
        let actual = self.actual_scale();
        let canvas = shader(Viewer {
            frame: v.frame,
            full: v.full.clone(),
            fallback: self.thumbs.peek(&v.frame).cloned(),
            zoom: v.zoom,
            actual,
            outlines,
            tokens: t,
            on_event: Message::Viewer,
        })
        .width(Fill)
        .height(Fill);

        let source = self
            .sources
            .iter()
            .find(|s| s.source == v.frame.0)
            .map_or_else(|| "unknown source".into(), source_name);
        let title = v.detail.as_ref().map_or_else(String::new, |d| {
            super::app_and_window(d.application.as_deref(), d.window_title.as_deref())
        });
        let percent = v.full.as_ref().map(|p| {
            let size = v.size.unwrap_or((1280.0, 800.0));
            let placement = viewer::model::place(
                v.zoom,
                (p.width as f32, p.height as f32),
                viewer::image_area(size.0, size.1),
            );
            viewer::model::percent(&placement, actual)
        });
        let mut right = row![].spacing(8).align_y(Alignment::Center);
        if matches > 0 {
            right = right.push(
                container(
                    text(format!(
                        "{matches} match{}",
                        if matches == 1 { "" } else { "es" }
                    ))
                    .size(12)
                    .font(MONO),
                )
                .padding([4, 10])
                .style(move |_| t.pill(t.gold)),
            );
        }
        let zoom_label = match (percent, v.zoom) {
            (Some(p), Zoom::Fit) => format!("fit · {p}%"),
            (Some(p), _) => format!("{p}%"),
            (None, _) => "…".into(),
        };
        right = right
            .push(
                container(text(zoom_label).size(12).font(MONO))
                    .padding([4, 10])
                    .style(move |_| t.pill(t.rewind)),
            )
            .push(
                button(text("Close").size(13))
                    .padding([5, 14])
                    .style(move |_, status| t.transport(status))
                    .on_press(Message::CloseViewer),
            );
        let header = row![
            column![
                row![
                    text(format_time(v.at.0)).size(15).font(MONO).color(t.text),
                    text(source).size(13).color(t.lane(self.accent(v.frame.0))),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
                text(title).size(13).color(t.text_2),
            ]
            .spacing(2),
            space::horizontal(),
            right,
        ]
        .align_y(Alignment::Center)
        .padding([8, 24])
        .height(viewer::HEADER);

        let status: String = if v.failed {
            "The full-resolution picture could not be read.".into()
        } else if let Some(notice) = v.notice {
            notice.into()
        } else if v.full.is_none() {
            "Loading full resolution…".into()
        } else {
            String::new()
        };
        let footer = row![
            text(status).size(12).color(t.text_2),
            space::horizontal(),
            text("Esc close  ·  ← → earlier / later in this lane  ·  scroll zoom, drag pan  ·  double-click or 1 / 0: 100% / fit")
                .size(12)
                .color(t.text_3),
        ]
        .align_y(Alignment::Center)
        .padding([8, 24])
        .height(viewer::FOOTER);

        // Bands behind the header and hints, so they stay readable over a zoomed-in picture.
        let band = move |_: &iced::Theme| container::Style {
            background: Some(viewer::backdrop(&t).scale_alpha(0.86).into()),
            ..container::Style::default()
        };
        stack![
            canvas,
            column![
                container(header).width(Fill).style(band),
                space::vertical(),
                container(footer).width(Fill).style(band),
            ]
            .width(Fill)
            .height(Fill),
        ]
        .width(Fill)
        .height(Fill)
        .into()
    }
}
