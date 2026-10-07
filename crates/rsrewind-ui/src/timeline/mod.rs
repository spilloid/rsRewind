//! The rewind viewport: a custom wgpu widget showing recorded moments as screenshot cards in a
//! 2.5D room (time runs into depth, one lane per source), and the filament strip beneath it.
//!
//! - [`model`]: layout, camera, culling and hit testing as plain, unit-tested math.
//! - [`cache`]: the byte-bounded LRU used for thumbnails (CPU) and textures (GPU).
//! - [`gpu`]: the wgpu pipeline and its texture cache.
//! - this file: the iced `shader::Program`s that turn pointer input into [`Event`]s and app state
//!   into draw lists.

pub mod cache;
pub mod gpu;
pub mod model;

use crate::style::Tokens;
use crate::thumb::Bgra;
use cache::ByteLru;
use gpu::{Cards, Quad};
use iced::event::Event as UiEvent;
use iced::mouse;
use iced::widget::shader::{self, Action};
use iced::{Color, Point, Rectangle};
use model::{Camera, Card, Filament, Placed};
use rsrewind_core::{SourceId, TimelineEntry, VisualStateId};
use std::collections::HashSet;
use std::sync::Arc;

/// A stored picture: which source, which visual state. Thumbnails and textures are cached by it.
pub type FrameKey = (Option<SourceId>, VisualStateId);

const ROOM_SLOT: u64 = 1;
const FILAMENT_SLOT: u64 = 2;

/// One recorded moment in the room.
#[derive(Debug, Clone)]
pub struct Moment {
    pub entry: TimelineEntry,
    /// Position among the lanes on screen (only the selected sources have one).
    pub lane: usize,
    /// The source's accent, stable whatever is filtered.
    pub accent: usize,
}

impl Moment {
    pub fn frame(&self) -> FrameKey {
        (self.entry.source, self.entry.visual_state_id)
    }

    pub fn card(&self) -> Card {
        Card {
            lane: self.lane,
            started_at: self.entry.started_at.0,
            ended_at: self.entry.ended_at.0,
        }
    }
}

/// What the viewport asks the application to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// Move the cursor to this moment (Unix ms).
    Scrub(f64),
    /// The pointer is over this moment (index into the moments), or none.
    Hover(Option<usize>),
    /// This moment was clicked.
    Select(usize),
}

/// Lays out moments the same way for drawing, hit testing and deciding which pictures to load.
pub fn place(
    moments: &[Moment],
    lanes: usize,
    camera: &Camera,
    width: f32,
    height: f32,
) -> Vec<Placed> {
    let cards: Vec<Card> = moments.iter().map(Moment::card).collect();
    camera.place(&cards, lanes, width, height)
}

/// The room: everything it needs to draw one frame, borrowed from the application.
pub struct Room<'a, F> {
    pub moments: &'a [Moment],
    pub lanes: usize,
    pub camera: Camera,
    /// Oldest and newest moments in history, bounds for scrubbing.
    pub first: f64,
    pub last: f64,
    /// Decoded thumbnails; cards without one are drawn as placeholders until it arrives.
    pub thumbs: &'a ByteLru<FrameKey, Arc<Bgra>>,
    pub selected: Option<FrameKey>,
    /// Pictures that match the current search.
    pub matched: &'a HashSet<FrameKey>,
    pub tokens: Tokens,
    pub on_event: F,
}

#[derive(Debug, Default)]
pub struct RoomState {
    drag: Option<Drag>,
    hovered: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct Drag {
    from: Point,
    camera: Camera,
    moved: bool,
}

impl<Message, F> shader::Program<Message> for Room<'_, F>
where
    F: Fn(Event) -> Message,
{
    type State = RoomState;
    type Primitive = Cards;

    fn update(
        &self,
        state: &mut RoomState,
        event: &UiEvent,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let UiEvent::Mouse(event) = event else {
            return None;
        };
        match event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                let at = cursor.position_in(bounds)?;
                state.drag = Some(Drag {
                    from: at,
                    camera: self.camera,
                    moved: false,
                });
                Some(Action::capture())
            }
            mouse::Event::CursorMoved { .. } => {
                if let Some(drag) = state.drag.as_mut() {
                    let at = cursor.position_from(bounds.position())?;
                    let (dx, dy) = (at.x - drag.from.x, at.y - drag.from.y);
                    if dx.abs() + dy.abs() > 4.0 {
                        drag.moved = true;
                    }
                    if !drag.moved {
                        return None;
                    }
                    // Down or right pulls older moments toward the camera.
                    let fraction = f64::from((dy + dx) / bounds.height.max(1.0));
                    let moved = drag.camera.scrubbed(fraction, self.first, self.last);
                    return Some(Action::publish((self.on_event)(Event::Scrub(moved.cursor))));
                }
                let hovered = cursor.position_in(bounds).and_then(|at| {
                    let placed = place(
                        self.moments,
                        self.lanes,
                        &self.camera,
                        bounds.width,
                        bounds.height,
                    );
                    model::hit_test(&placed, at.x, at.y).map(|p| p.index)
                });
                if hovered == state.hovered {
                    return None;
                }
                state.hovered = hovered;
                Some(Action::publish((self.on_event)(Event::Hover(hovered))))
            }
            mouse::Event::ButtonReleased(mouse::Button::Left) => {
                let drag = state.drag.take()?;
                if drag.moved {
                    return Some(Action::capture());
                }
                let at = cursor.position_in(bounds)?;
                let placed = place(
                    self.moments,
                    self.lanes,
                    &self.camera,
                    bounds.width,
                    bounds.height,
                );
                let hit = model::hit_test(&placed, at.x, at.y)?;
                Some(Action::publish((self.on_event)(Event::Select(hit.index))).and_capture())
            }
            mouse::Event::WheelScrolled { delta } => {
                cursor.position_in(bounds)?;
                let fraction = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => f64::from(-y) * 0.05,
                    mouse::ScrollDelta::Pixels { y, .. } => {
                        f64::from(-y / bounds.height.max(1.0)) * 0.5
                    }
                };
                let moved = self.camera.scrubbed(fraction, self.first, self.last);
                Some(Action::publish((self.on_event)(Event::Scrub(moved.cursor))).and_capture())
            }
            mouse::Event::CursorLeft => {
                state.hovered.take()?;
                Some(Action::publish((self.on_event)(Event::Hover(None))))
            }
            _ => None,
        }
    }

    fn draw(&self, state: &RoomState, _cursor: mouse::Cursor, bounds: Rectangle) -> Cards {
        let t = &self.tokens;
        let (w, h) = (bounds.width, bounds.height);
        let mut quads = Vec::new();
        let mut pictures = Vec::new();

        // The floor: a few receding lines, brightest at the camera.
        for i in 0..=8 {
            let z = i as f32 / 8.0;
            let y = self.camera.floor_y(self.lanes, w, h, z);
            let span = w * (0.98 - 0.5 * z);
            quads.push(Quad::solid(
                [(w - span) / 2.0, y, span, 1.0],
                t.line,
                0.0,
                0.9 - 0.75 * z,
            ));
        }

        for placed in place(self.moments, self.lanes, &self.camera, w, h) {
            let Some(moment) = self.moments.get(placed.index) else {
                continue;
            };
            let key = moment.frame();
            let rect = placed.rect;
            let picture = self.thumbs.peek(&key).cloned();
            let image = picture.as_ref().map(|p| {
                let inset = (3.0 * rect.w / 240.0).clamp(1.0, 4.0);
                let inner = model::Rect {
                    x: rect.x + inset,
                    y: rect.y + inset,
                    w: rect.w - 2.0 * inset,
                    h: rect.h - 2.0 * inset,
                };
                let fit = inner.fit(p.width as f32 / p.height.max(1) as f32);
                [fit.x, fit.y, fit.w, fit.h]
            });
            let lane = t.lane(moment.accent);
            let selected = self.selected == Some(key);
            let hovered = state.hovered == Some(placed.index);
            let matched = self.matched.contains(&key);
            let (border, border_width, glow) = if selected {
                (t.rewind_bright, 2.5, 14.0)
            } else if matched {
                (t.gold, 2.0, 0.0)
            } else if hovered {
                (lane, 2.0, 0.0)
            } else {
                (lane.scale_alpha(0.65), 1.2, 0.0)
            };
            quads.push(Quad {
                rect: [rect.x, rect.y, rect.w, rect.h],
                image,
                fill: if picture.is_some() {
                    Color::BLACK
                } else {
                    t.raised
                },
                border,
                radius: (8.0 * rect.w / 240.0).clamp(2.0, 10.0),
                border_width,
                opacity: placed.alpha,
                glow,
                texture: picture.as_ref().map(|_| key),
            });
            if let Some(picture) = picture {
                pictures.push((key, picture));
            }
        }
        Cards {
            slot: ROOM_SLOT,
            quads,
            pictures,
        }
    }

    fn mouse_interaction(
        &self,
        state: &RoomState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.drag.is_some_and(|d| d.moved) {
            mouse::Interaction::Grabbing
        } else if state.hovered.is_some() {
            mouse::Interaction::Pointer
        } else if cursor.is_over(bounds) {
            mouse::Interaction::Grab
        } else {
            mouse::Interaction::default()
        }
    }
}

/// The filament: all of history on one line, a mark per loaded moment, the cursor as a lit bead.
pub struct Strip<F> {
    pub filament: Filament,
    /// `(time, lane colour)` per loaded moment, oldest first.
    pub marks: Vec<(f64, Color)>,
    pub cursor: f64,
    pub tokens: Tokens,
    pub on_event: F,
}

#[derive(Debug, Default)]
pub struct StripState {
    dragging: bool,
}

impl<Message, F> shader::Program<Message> for Strip<F>
where
    F: Fn(Event) -> Message,
{
    type State = StripState;
    type Primitive = Cards;

    fn update(
        &self,
        state: &mut StripState,
        event: &UiEvent,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let UiEvent::Mouse(event) = event else {
            return None;
        };
        let scrub = |x: f32| {
            let t = self.filament.t_of(x, bounds.width);
            Action::publish((self.on_event)(Event::Scrub(t))).and_capture()
        };
        match event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                let at = cursor.position_in(bounds)?;
                state.dragging = true;
                Some(scrub(at.x))
            }
            mouse::Event::CursorMoved { .. } if state.dragging => {
                let at = cursor.position_from(bounds.position())?;
                Some(scrub(at.x))
            }
            mouse::Event::ButtonReleased(mouse::Button::Left) if state.dragging => {
                state.dragging = false;
                Some(Action::capture())
            }
            _ => None,
        }
    }

    fn draw(&self, _state: &StripState, _cursor: mouse::Cursor, bounds: Rectangle) -> Cards {
        let t = &self.tokens;
        let (w, h) = (bounds.width, bounds.height);
        let mid = h / 2.0;
        let inset = Filament::INSET;
        let mut quads = vec![Quad::solid(
            [inset, mid - 0.75, (w - 2.0 * inset).max(0.0), 1.5],
            t.line,
            0.75,
            1.0,
        )];
        // Marks: thin ticks, merged when closer than a pixel so a dense day stays cheap.
        let mut last_x = f32::NEG_INFINITY;
        for (time, color) in &self.marks {
            let x = self.filament.x_of(*time, w);
            if x - last_x < 1.0 {
                continue;
            }
            last_x = x;
            quads.push(Quad::solid(
                [x - 0.75, mid - 6.0, 1.5, 12.0],
                *color,
                0.75,
                0.85,
            ));
        }
        let x = self.filament.x_of(self.cursor, w);
        quads.push(Quad {
            rect: [x - 5.0, mid - 5.0, 10.0, 10.0],
            image: None,
            fill: t.rewind_bright,
            border: t.rewind,
            radius: 5.0,
            border_width: 0.0,
            opacity: 1.0,
            glow: 9.0,
            texture: None,
        });
        Cards {
            slot: FILAMENT_SLOT,
            quads,
            pictures: Vec::new(),
        }
    }

    fn mouse_interaction(
        &self,
        _state: &StripState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}
