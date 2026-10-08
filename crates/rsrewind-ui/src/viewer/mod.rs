//! The full-window image viewer: one recorded screen at full resolution over a dark backdrop,
//! with zoom toward the pointer, drag to pan, double-click for 100 % / fit, click-away to close,
//! and the current search's matches outlined in gold.
//!
//! - [`model`]: the view math (fit, zoom, pan, toggle, OCR mapping, match finding), unit-tested.
//! - this file: the iced `shader::Program` that turns pointer input into [`Event`]s and draws the
//!   picture with the timeline's GPU pipeline (full-resolution textures are freed as soon as they
//!   leave the screen; see `timeline::gpu`).

pub mod model;

use crate::style::Tokens;
use crate::thumb::Bgra;
use crate::timeline::gpu::{Cards, Quad, TexKey};
use crate::timeline::{FrameKey, is_double_click};
use iced::event::Event as UiEvent;
use iced::widget::shader::{self, Action};
use iced::{Color, Point, Rectangle, keyboard, mouse};
use model::{Placement, Zoom};
use std::sync::Arc;
use std::time::Instant;

const SLOT: u64 = 3;
/// Inset of the image from the viewer's edges at fit, so the backdrop frames it.
pub const MARGIN: f32 = 24.0;
/// Room for the header and the hint line at fit.
pub const HEADER: f32 = 56.0;
pub const FOOTER: f32 = 36.0;

/// What the viewer asks the application to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// Show the picture this way from now on.
    Zoom(Zoom),
    /// The viewer's drawing area (for keyboard zoom, which has no pointer to aim at).
    Resized(f32, f32),
    /// Close the viewer (click on the backdrop).
    Close,
}

/// The viewer's backdrop: near-black in both appearances (a light frame around a screenshot
/// makes the screenshot look washed out), a touch warmer in light mode.
pub fn backdrop(t: &Tokens) -> Color {
    if t.dark {
        Color::from_rgb8(5, 5, 7)
    } else {
        Color::from_rgb8(24, 22, 26)
    }
}

/// The area the image is laid out in, inside the viewer's bounds.
pub fn image_area(width: f32, height: f32) -> (f32, f32) {
    (
        (width - 2.0 * MARGIN).max(1.0),
        (height - HEADER - FOOTER - 2.0 * MARGIN).max(1.0),
    )
}

fn area_origin() -> (f32, f32) {
    (MARGIN, HEADER + MARGIN)
}

/// One frame of the viewer.
pub struct Viewer<F> {
    pub frame: FrameKey,
    /// The full-resolution picture, once decoded.
    pub full: Option<Arc<Bgra>>,
    /// The card thumbnail, shown (at fit) until the full picture arrives.
    pub fallback: Option<Arc<Bgra>>,
    pub zoom: Zoom,
    /// Scale for 100 %: logical pixels per image pixel (1 / window scale factor).
    pub actual: f32,
    /// Search matches as fractions of the picture (`[x, y, w, h]`, 0..1).
    pub outlines: Vec<[f32; 4]>,
    pub tokens: Tokens,
    pub on_event: F,
}

impl<F> Viewer<F> {
    /// The picture to draw and whether zooming applies to it (only the full one zooms).
    fn picture(&self) -> Option<(&Arc<Bgra>, bool)> {
        self.full
            .as_ref()
            .map(|p| (p, true))
            .or_else(|| self.fallback.as_ref().map(|p| (p, false)))
    }

    fn placement(&self, bounds: Rectangle) -> Option<(Placement, bool)> {
        let (picture, zoomable) = self.picture()?;
        let image = (picture.width as f32, picture.height as f32);
        let zoom = if zoomable { self.zoom } else { Zoom::Fit };
        let mut p = model::place(zoom, image, image_area(bounds.width, bounds.height));
        let (ox, oy) = area_origin();
        p.rect.x += ox;
        p.rect.y += oy;
        Some((p, zoomable))
    }

    fn full_size(&self) -> Option<(f32, f32)> {
        self.full
            .as_ref()
            .map(|p| (p.width as f32, p.height as f32))
    }
}

#[derive(Debug, Default)]
pub struct State {
    drag: Option<Drag>,
    last_click: Option<(Instant, Point)>,
    size: Option<(f32, f32)>,
    modifiers: keyboard::Modifiers,
}

#[derive(Debug, Clone, Copy)]
struct Drag {
    last: Point,
    moved: bool,
}

impl<Message, F> shader::Program<Message> for Viewer<F>
where
    F: Fn(Event) -> Message,
{
    type State = State;
    type Primitive = Cards;

    fn update(
        &self,
        state: &mut State,
        event: &UiEvent,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let size = (bounds.width, bounds.height);
        if state.size != Some(size) {
            state.size = Some(size);
            return Some(Action::publish((self.on_event)(Event::Resized(
                size.0, size.1,
            ))));
        }
        let area = image_area(bounds.width, bounds.height);
        let (ox, oy) = area_origin();
        let publish = |e: Event| Some(Action::publish((self.on_event)(e)).and_capture());
        match event {
            UiEvent::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                state.modifiers = *m;
                None
            }
            UiEvent::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let at = cursor.position_in(bounds)?;
                let image = self.full_size()?;
                let at = (at.x - ox, at.y - oy);
                let zoom = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => {
                        model::zoom_at(self.zoom, 1.25f32.powf(*y), at, image, area, self.actual)
                    }
                    // Touchpads: pinch arrives with Ctrl held; plain two-finger scroll pans.
                    mouse::ScrollDelta::Pixels { y, .. } if state.modifiers.control() => {
                        model::zoom_at(self.zoom, (y / 200.0).exp(), at, image, area, self.actual)
                    }
                    mouse::ScrollDelta::Pixels { x, y } => {
                        model::pan(self.zoom, *x, *y, image, area, self.actual)
                    }
                };
                publish(Event::Zoom(zoom))
            }
            UiEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let at = cursor.position_in(bounds)?;
                state.drag = Some(Drag {
                    last: at,
                    moved: false,
                });
                Some(Action::capture())
            }
            UiEvent::Mouse(mouse::Event::CursorMoved { .. }) => {
                let drag = state.drag.as_mut()?;
                let at = cursor.position_from(bounds.position())?;
                let (dx, dy) = (at.x - drag.last.x, at.y - drag.last.y);
                if !drag.moved && dx.abs() + dy.abs() < 3.0 {
                    return None;
                }
                drag.moved = true;
                drag.last = at;
                let image = self.full_size()?;
                let zoom = model::pan(self.zoom, dx, dy, image, area, self.actual);
                (zoom != self.zoom).then(|| Action::publish((self.on_event)(Event::Zoom(zoom))))
            }
            UiEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let drag = state.drag.take()?;
                if drag.moved {
                    state.last_click = None;
                    return Some(Action::capture());
                }
                let at = cursor.position_in(bounds)?;
                let now = Instant::now();
                let on_image = self
                    .placement(bounds)
                    .is_some_and(|(p, _)| p.rect.contains(at.x, at.y));
                if !on_image {
                    // Click-away: the backdrop closes the viewer.
                    state.last_click = None;
                    return publish(Event::Close);
                }
                if is_double_click(state.last_click, now, at) {
                    state.last_click = None;
                    let image = self.full_size()?;
                    let zoom =
                        model::toggle(self.zoom, (at.x - ox, at.y - oy), image, area, self.actual);
                    return publish(Event::Zoom(zoom));
                }
                state.last_click = Some((now, at));
                Some(Action::capture())
            }
            _ => None,
        }
    }

    fn draw(&self, _state: &State, _cursor: mouse::Cursor, bounds: Rectangle) -> Cards {
        let t = &self.tokens;
        let mut quads = vec![Quad::solid(
            [0.0, 0.0, bounds.width, bounds.height],
            backdrop(t),
            0.0,
            1.0,
        )];
        let mut pictures = Vec::new();
        if let Some((placement, zoomable)) = self.placement(bounds)
            && let Some((picture, _)) = self.picture()
        {
            let key = if zoomable {
                TexKey::full(self.frame)
            } else {
                TexKey::thumb(self.frame)
            };
            let r = placement.rect;
            quads.push(Quad {
                rect: [r.x, r.y, r.w, r.h],
                image: Some([r.x, r.y, r.w, r.h]),
                fill: Color::BLACK,
                border: t.line,
                radius: 0.0,
                border_width: 0.0,
                opacity: 1.0,
                glow: 0.0,
                texture: Some(key),
            });
            pictures.push((key, picture.clone()));
            let (iw, ih) = (picture.width as f32, picture.height as f32);
            for [x, y, w, h] in &self.outlines {
                let o = placement.to_screen(x * iw, y * ih, w * iw, h * ih);
                // A little padding so the outline sits around the word, not on it.
                let pad = 3.0;
                quads.push(Quad {
                    rect: [o.x - pad, o.y - pad, o.w + 2.0 * pad, o.h + 2.0 * pad],
                    image: None,
                    fill: Color::TRANSPARENT,
                    border: t.gold,
                    radius: 3.0,
                    border_width: 2.0,
                    opacity: 1.0,
                    glow: 6.0,
                    texture: None,
                });
            }
        }
        Cards {
            slot: SLOT,
            quads,
            pictures,
        }
    }

    fn mouse_interaction(
        &self,
        state: &State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let Some(at) = cursor.position_in(bounds) else {
            return mouse::Interaction::default();
        };
        let over_image = self
            .placement(bounds)
            .is_some_and(|(p, _)| p.rect.contains(at.x, at.y));
        let pannable = matches!(self.zoom, Zoom::Scale { .. }) && self.full.is_some();
        if state.drag.is_some_and(|d| d.moved) {
            mouse::Interaction::Grabbing
        } else if over_image && pannable {
            mouse::Interaction::Grab
        } else if over_image {
            mouse::Interaction::ZoomIn
        } else {
            mouse::Interaction::default()
        }
    }
}
