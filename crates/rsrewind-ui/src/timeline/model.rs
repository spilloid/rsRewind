//! The 2.5D timeline as plain math: layout, camera, culling and hit testing.
//!
//! Time runs into the screen. The camera stands at a moment (`cursor`); a moment recorded before
//! it sits deeper in the room, smaller and dimmer, and one recorded just after it is drifting past
//! the camera and fading out. Each source has its own lane, side by side. Depth is
//! logarithmic in elapsed time, so the last minute is spread out and last week is compressed into
//! the back: recorded history is bursty, and a linear depth would show either one card or a wall.
//!
//! Nothing here knows about the GPU, iced or the query layer. The renderer turns [`Placed`] cards
//! into quads; the widget turns pointer input into calls on [`Camera`].

/// Width / height of a card. Screens are mostly 16:9 or 16:10; pictures are letterboxed inside.
pub const CARD_ASPECT: f32 = 16.0 / 10.0;
/// Most cards drawn at once (the nearest win). Bounds GPU work and texture uploads per frame.
pub const MAX_CARDS: usize = 180;
/// How strongly depth shrinks a card: scale at depth `z` is `1 / (1 + z * PERSPECTIVE)`.
const PERSPECTIVE: f32 = 2.6;
/// Moments newer than the cursor fade out over this much depth in front of the camera.
const Z_AHEAD: f32 = 0.12;
/// Vanishing point height, and where the front row of cards sits, as fractions of the height.
const HORIZON: f32 = 0.30;
const FRONT_Y: f32 = 0.66;
/// Time scale of the logarithmic depth: moments within about this long of the cursor are spaced
/// almost linearly.
pub const DEFAULT_KNEE_MS: f64 = 20_000.0;
/// Shortest and longest the room may reach back.
pub const MIN_DEPTH_MS: f64 = 60_000.0;
pub const MAX_DEPTH_MS: f64 = 90.0 * 24.0 * 3_600_000.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }

    pub fn intersects(&self, width: f32, height: f32) -> bool {
        self.x < width && self.x + self.w > 0.0 && self.y < height && self.y + self.h > 0.0
    }

    /// The largest rectangle of `aspect` (w / h) centred inside this one.
    pub fn fit(&self, aspect: f32) -> Rect {
        if !(aspect.is_finite() && aspect > 0.0) || self.h <= 0.0 {
            return *self;
        }
        if self.w / self.h > aspect {
            let w = self.h * aspect;
            Rect {
                x: self.x + (self.w - w) / 2.0,
                w,
                ..*self
            }
        } else {
            let h = self.w / aspect;
            Rect {
                y: self.y + (self.h - h) / 2.0,
                h,
                ..*self
            }
        }
    }
}

/// One moment to place: which lane, and the span it was on screen (Unix ms).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Card {
    pub lane: usize,
    pub started_at: i64,
    pub ended_at: i64,
}

/// A card that survived culling, in screen space (logical pixels, origin top-left).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    /// Index into the slice passed to [`Camera::place`].
    pub index: usize,
    pub rect: Rect,
    /// 0 at the camera, 1 at the back wall, negative in front (newer than the cursor).
    pub z: f32,
    /// Opacity, 0..=1.
    pub alpha: f32,
}

/// Where the camera stands and how far back it sees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// The moment at the front of the room (Unix ms; fractional while scrubbing).
    pub cursor: f64,
    /// How far back the back wall is, in ms.
    pub depth_ms: f64,
    pub knee_ms: f64,
}

impl Camera {
    pub fn new(cursor: f64, depth_ms: f64) -> Self {
        Self {
            cursor,
            depth_ms: depth_ms.clamp(MIN_DEPTH_MS, MAX_DEPTH_MS),
            knee_ms: DEFAULT_KNEE_MS,
        }
    }

    /// How long before the cursor the moments are that sit at depth `z` (0..=1).
    pub fn age_at(&self, z: f64) -> f64 {
        let z = z.clamp(0.0, 1.0);
        self.knee_ms * ((1.0 + self.depth_ms / self.knee_ms).powf(z) - 1.0)
    }

    /// Moments newer than the cursor stay visible for this long, fading as they pass.
    fn ahead_ms(&self) -> f64 {
        (self.depth_ms * 0.004).clamp(2_000.0, 60_000.0)
    }

    /// Depth of a moment on screen from `started_at` to `ended_at`, or `None` when it is behind
    /// the back wall or already past the camera. A moment still on screen at the cursor is at 0.
    pub fn depth(&self, started_at: i64, ended_at: i64) -> Option<f32> {
        let end = ended_at.max(started_at) as f64;
        let start = started_at as f64;
        if self.cursor >= start && self.cursor <= end {
            return Some(0.0);
        }
        if self.cursor > end {
            let age = self.cursor - end;
            if age > self.depth_ms {
                return None;
            }
            let z = (1.0 + age / self.knee_ms).ln() / (1.0 + self.depth_ms / self.knee_ms).ln();
            return Some(z as f32);
        }
        let ahead = start - self.cursor;
        let limit = self.ahead_ms();
        (ahead <= limit).then(|| -Z_AHEAD * (ahead / limit) as f32)
    }

    /// Lays out `cards` in `lanes` lanes on a `width` x `height` viewport: culled (too deep,
    /// passed, off screen, invisible), capped at [`MAX_CARDS`] keeping the nearest, and sorted
    /// back to front so drawing in order paints nearer cards over farther ones.
    pub fn place(&self, cards: &[Card], lanes: usize, width: f32, height: f32) -> Vec<Placed> {
        if width <= 0.0 || height <= 0.0 || lanes == 0 {
            return Vec::new();
        }
        let geometry = Geometry::new(lanes, width, height);
        let mut placed: Vec<Placed> = cards
            .iter()
            .enumerate()
            .filter(|(_, c)| c.lane < lanes)
            .filter_map(|(index, card)| {
                let z = self.depth(card.started_at, card.ended_at)?;
                let alpha = if z >= 0.0 {
                    1.0 - 0.82 * z
                } else {
                    1.0 + z / Z_AHEAD
                };
                let rect = geometry.card(card.lane, z);
                (alpha > 0.03 && rect.intersects(width, height)).then_some(Placed {
                    index,
                    rect,
                    z,
                    alpha,
                })
            })
            .collect();
        if placed.len() > MAX_CARDS {
            placed.sort_by(|a, b| a.z.abs().total_cmp(&b.z.abs()));
            placed.truncate(MAX_CARDS);
        }
        placed.sort_by(|a, b| {
            b.z.total_cmp(&a.z)
                .then_with(|| {
                    cards[b.index]
                        .started_at
                        .cmp(&cards[a.index].started_at)
                        .reverse()
                })
                .then_with(|| a.index.cmp(&b.index))
        });
        placed
    }

    /// Moves the cursor as if the room were dragged by `fraction` of its height: positive pulls
    /// older moments toward the camera (back in time), negative pushes them away. One full height
    /// moves by as much time as the whole room holds; small drags move by little, because depth is
    /// logarithmic. The result stays within `[first, last]`.
    pub fn scrubbed(&self, fraction: f64, first: f64, last: f64) -> Self {
        let f = fraction.abs();
        // Up to one room: logarithmic, like depth. Beyond (a long fling): a room per height.
        let step = if f <= 1.0 {
            self.age_at(f)
        } else {
            self.depth_ms * f
        };
        let cursor = if fraction >= 0.0 {
            self.cursor - step
        } else {
            self.cursor + step
        };
        Self {
            cursor: clamp_between(cursor, first, last),
            ..*self
        }
    }

    /// Zooms the depth by `factor` (> 1 sees further back).
    pub fn zoomed(&self, factor: f64) -> Self {
        if !(factor.is_finite() && factor > 0.0) {
            return *self;
        }
        Self {
            depth_ms: (self.depth_ms * factor).clamp(MIN_DEPTH_MS, MAX_DEPTH_MS),
            ..*self
        }
    }

    /// Screen height of the floor line at depth `z`, for drawing the floor and its time marks.
    pub fn floor_y(&self, lanes: usize, width: f32, height: f32, z: f32) -> f32 {
        let geometry = Geometry::new(lanes.max(1), width, height);
        let k = scale(z);
        let front = height * FRONT_Y + geometry.card_h / 2.0;
        height * HORIZON + (front - height * HORIZON) * k
    }

    /// Horizontal centre of `lane` at the camera (z = 0), for its label.
    pub fn lane_x(&self, lane: usize, lanes: usize, width: f32, height: f32) -> f32 {
        let geometry = Geometry::new(lanes.max(1), width, height);
        width / 2.0 + geometry.offset(lane)
    }
}

fn scale(z: f32) -> f32 {
    1.0 / (1.0 + z * PERSPECTIVE)
}

fn clamp_between(value: f64, a: f64, b: f64) -> f64 {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    value.clamp(lo, hi)
}

/// Lane and card sizes for one viewport.
struct Geometry {
    lanes: usize,
    width: f32,
    height: f32,
    pitch: f32,
    card_w: f32,
    card_h: f32,
}

impl Geometry {
    fn new(lanes: usize, width: f32, height: f32) -> Self {
        let pitch = (width * 0.92 / lanes as f32).min(width * 0.34);
        let card_w = (pitch * 0.9)
            .min(width * 0.46)
            .min(height * 0.5 * CARD_ASPECT);
        Self {
            lanes,
            width,
            height,
            pitch,
            card_w,
            card_h: card_w / CARD_ASPECT,
        }
    }

    /// Horizontal offset of a lane's centre from the viewport centre, at the camera.
    fn offset(&self, lane: usize) -> f32 {
        (lane as f32 - (self.lanes as f32 - 1.0) / 2.0) * self.pitch
    }

    fn card(&self, lane: usize, z: f32) -> Rect {
        let k = scale(z);
        let (w, h) = (self.card_w * k, self.card_h * k);
        let cx = self.width / 2.0 + self.offset(lane) * k;
        let horizon = self.height * HORIZON;
        let cy = horizon + (self.height * FRONT_Y - horizon) * k;
        Rect {
            x: cx - w / 2.0,
            y: cy - h / 2.0,
            w,
            h,
        }
    }
}

/// The front-most placed card under a point (`placed` as returned by [`Camera::place`]).
pub fn hit_test(placed: &[Placed], x: f32, y: f32) -> Option<&Placed> {
    placed.iter().rev().find(|p| p.rect.contains(x, y))
}

/// The nearest moment strictly after (`forward`) or before the cursor, from moment start times.
pub fn step(starts: &[i64], cursor: f64, forward: bool) -> Option<i64> {
    let candidates = starts.iter().copied();
    if forward {
        candidates.filter(|&t| t as f64 > cursor + 0.5).min()
    } else {
        candidates.filter(|&t| (t as f64) < cursor - 0.5).max()
    }
}

/// The filament under the room: all of history mapped onto one horizontal strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Filament {
    pub first: f64,
    pub last: f64,
}

impl Filament {
    /// Horizontal inset at each end, in logical pixels.
    pub const INSET: f32 = 14.0;

    pub fn new(first: i64, last: i64) -> Self {
        let (first, last) = (first.min(last) as f64, first.max(last) as f64);
        Self { first, last }
    }

    pub fn x_of(&self, t: f64, width: f32) -> f32 {
        let usable = (width - 2.0 * Self::INSET).max(1.0);
        let span = (self.last - self.first).max(1.0);
        let f = ((t - self.first) / span).clamp(0.0, 1.0);
        Self::INSET + usable * f as f32
    }

    pub fn t_of(&self, x: f32, width: f32) -> f64 {
        let usable = (width - 2.0 * Self::INSET).max(1.0);
        let f = ((x - Self::INSET) / usable).clamp(0.0, 1.0);
        self.first + (self.last - self.first) * f64::from(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f32 = 1200.0;
    const H: f32 = 700.0;
    const T: i64 = 1_790_000_000_000;

    fn card(lane: usize, start: i64) -> Card {
        Card {
            lane,
            started_at: start,
            ended_at: start + 1_000,
        }
    }

    fn camera() -> Camera {
        Camera::new(T as f64, 3_600_000.0)
    }

    #[test]
    fn depth_is_zero_on_screen_and_grows_with_age() {
        let cam = camera();
        assert_eq!(cam.depth(T - 500, T + 500), Some(0.0));
        let mut last = 0.0;
        for age in [1_000, 10_000, 60_000, 600_000, 3_000_000] {
            let z = cam.depth(T - age - 1, T - age).unwrap_or(f32::NAN);
            assert!(z > last && z <= 1.0, "age {age}: {z}");
            last = z;
        }
        assert_eq!(
            cam.depth(T - 3_700_000, T - 3_650_000),
            None,
            "behind the wall"
        );
        let ahead = cam.depth(T + 1_000, T + 2_000).unwrap_or(f32::NAN);
        assert!(ahead < 0.0 && ahead > -Z_AHEAD, "{ahead}");
        assert_eq!(
            cam.depth(T + 120_000, T + 121_000),
            None,
            "long past the camera"
        );
    }

    #[test]
    fn depth_is_logarithmic_so_the_recent_past_is_spread_out() {
        let cam = camera();
        let z = |age: i64| cam.depth(T - age, T - age).unwrap_or(f32::NAN);
        // The last minute is 1/60 of an hour-deep room but gets over a quarter of its depth.
        assert!(z(60_000) > 0.25, "{}", z(60_000));
        assert!(z(60_000) / 60_000.0 > (z(3_600_000) - z(60_000)) / 3_540_000.0 * 10.0);
        // age_at inverts depth.
        for age in [5_000.0, 120_000.0, 1_800_000.0] {
            let back = cam.age_at(f64::from(z(age as i64)));
            assert!((back - age).abs() / age < 1e-3, "{age} -> {back}");
        }
    }

    #[test]
    fn nearer_cards_are_bigger_and_drawn_last() {
        let cam = camera();
        let cards = [card(0, T - 600_000), card(0, T), card(0, T - 30_000)];
        let placed = cam.place(&cards, 1, W, H);
        assert_eq!(placed.len(), 3);
        let order: Vec<usize> = placed.iter().map(|p| p.index).collect();
        assert_eq!(order, [0, 2, 1], "back to front");
        assert!(placed[0].rect.w < placed[1].rect.w && placed[1].rect.w < placed[2].rect.w);
        assert!(placed[0].alpha < placed[2].alpha);
        assert!((placed[2].rect.w / placed[2].rect.h - CARD_ASPECT).abs() < 1e-3);
    }

    #[test]
    fn lanes_sit_side_by_side_without_overlapping_at_the_camera() {
        let cam = camera();
        for lanes in 1..=6 {
            let cards: Vec<Card> = (0..lanes).map(|l| card(l, T)).collect();
            let placed = cam.place(&cards, lanes, W, H);
            assert_eq!(placed.len(), lanes);
            let mut rects: Vec<Rect> = placed.iter().map(|p| p.rect).collect();
            rects.sort_by(|a, b| a.x.total_cmp(&b.x));
            for pair in rects.windows(2) {
                assert!(pair[0].x + pair[0].w <= pair[1].x, "{lanes} lanes overlap");
            }
            assert!(
                rects.iter().all(|r| r.x >= 0.0 && r.x + r.w <= W),
                "{lanes} lanes"
            );
            // Lane order is left to right.
            let xs: Vec<f32> = (0..lanes).map(|l| cam.lane_x(l, lanes, W, H)).collect();
            assert!(xs.windows(2).all(|p| p[0] < p[1]));
        }
    }

    #[test]
    fn culling_drops_what_cannot_be_seen_and_caps_the_count() {
        let cam = camera();
        let mut cards = vec![
            card(0, T - 10_000_000), // behind the wall
            card(0, T + 600_000),    // long past the camera
            card(5, T),              // lane that does not exist
        ];
        assert!(cam.place(&cards, 2, W, H).is_empty());
        // A burst: far more moments than can be drawn. The nearest survive.
        cards = (0..1_000)
            .map(|i| card(i % 2, T - i as i64 * 1_000))
            .collect();
        let placed = cam.place(&cards, 2, W, H);
        assert_eq!(placed.len(), MAX_CARDS);
        let deepest = placed.iter().map(|p| p.z).fold(0.0f32, f32::max);
        let dropped_nearest = (0..1_000)
            .filter(|i| !placed.iter().any(|p| p.index == *i))
            .filter_map(|i| cam.depth(cards[i].started_at, cards[i].ended_at))
            .fold(f32::INFINITY, f32::min);
        assert!(dropped_nearest >= deepest, "{dropped_nearest} < {deepest}");
        assert!(cam.place(&cards, 2, 0.0, H).is_empty());
    }

    #[test]
    fn hit_testing_picks_the_front_most_card() {
        let cam = camera();
        // Same lane: the deeper card is behind the nearer one and overlaps it.
        let cards = [card(0, T - 2_000), card(0, T)];
        let placed = cam.place(&cards, 1, W, H);
        let front = placed.iter().find(|p| p.index == 1).map(|p| p.rect);
        let front = front.unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        });
        let (cx, cy) = (front.x + front.w / 2.0, front.y + front.h / 2.0);
        assert_eq!(hit_test(&placed, cx, cy).map(|p| p.index), Some(1));
        assert_eq!(hit_test(&placed, 1.0, 1.0).map(|p| p.index), None);
    }

    #[test]
    fn scrubbing_moves_through_time_within_history() {
        let cam = camera();
        let (first, last) = (T as f64 - 7_200_000.0, T as f64);
        let back = cam.scrubbed(0.5, first, last);
        assert!(back.cursor < cam.cursor);
        // Within f64 resolution at a 2026 timestamp in ms (about 2e-4).
        assert!((cam.cursor - back.cursor - cam.age_at(0.5)).abs() < 1e-2);
        assert_eq!(
            cam.scrubbed(-0.5, first, last).cursor,
            last,
            "clamped at the newest"
        );
        assert_eq!(
            cam.scrubbed(10.0, first, last).cursor,
            first,
            "clamped at the oldest"
        );
        // Small drags move little: depth is logarithmic.
        assert!(cam.age_at(0.01) < 2_000.0);
        assert_eq!(cam.zoomed(1e9).depth_ms, MAX_DEPTH_MS);
        assert_eq!(cam.zoomed(1e-9).depth_ms, MIN_DEPTH_MS);
        assert_eq!(cam.zoomed(f64::NAN), cam);
    }

    #[test]
    fn stepping_finds_the_neighbouring_moments() {
        let starts = [10, 20, 30];
        assert_eq!(step(&starts, 20.0, true), Some(30));
        assert_eq!(step(&starts, 20.0, false), Some(10));
        assert_eq!(step(&starts, 30.0, true), None);
        assert_eq!(step(&starts, 25.0, false), Some(20));
    }

    #[test]
    fn the_filament_maps_time_to_x_and_back() {
        let f = Filament::new(1_000, 11_000);
        assert_eq!(f.x_of(1_000.0, 228.0), Filament::INSET);
        assert_eq!(f.x_of(11_000.0, 228.0), 228.0 - Filament::INSET);
        assert_eq!(f.x_of(-5.0, 228.0), Filament::INSET);
        let t = f.t_of(f.x_of(6_000.0, 500.0), 500.0);
        assert!((t - 6_000.0).abs() < 1.0);
        // A single moment does not divide by zero.
        let one = Filament::new(5, 5);
        assert!(one.x_of(5.0, 100.0).is_finite());
    }

    #[test]
    fn fit_letterboxes_inside_the_card() {
        let card = Rect {
            x: 0.0,
            y: 0.0,
            w: 160.0,
            h: 100.0,
        };
        let wide = card.fit(2.0);
        assert_eq!((wide.w, wide.h, wide.y), (160.0, 80.0, 10.0));
        let tall = card.fit(1.0);
        assert_eq!((tall.w, tall.h, tall.x), (100.0, 100.0, 30.0));
        assert_eq!(card.fit(0.0), card);
    }
}
