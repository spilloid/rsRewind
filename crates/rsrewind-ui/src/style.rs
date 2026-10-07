//! Design tokens from `docs/design/brief.md` ("Nostalgia at the edges, clarity at the center"):
//! satin-black canvas, warm ivory text, periwinkle for time and playback, mint for healthy, rose
//! for recording/danger, gold reserved for the studio (used here only to mark search matches),
//! violet/sky/mint/rose as lane accents. Light mode is warm archival paper, not white.

use iced::widget::{button, container, text_input};
use iced::{Background, Border, Color, Font, Shadow, Theme, theme};

/// Every colour the UI uses, for one appearance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    pub dark: bool,
    pub canvas: Color,
    pub surface: Color,
    pub raised: Color,
    pub line: Color,
    pub text: Color,
    pub text_2: Color,
    pub text_3: Color,
    pub rewind: Color,
    pub rewind_bright: Color,
    pub gold: Color,
    pub mint: Color,
    pub rose: Color,
    /// One accent per lane, cycled.
    pub lanes: [Color; 4],
}

const fn hex(rgb: u32) -> Color {
    Color::from_rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

impl Tokens {
    pub const DARK: Self = Self {
        dark: true,
        canvas: hex(0x09090B),
        surface: hex(0x121218),
        raised: hex(0x191920),
        line: hex(0x2A2A33),
        text: hex(0xF8F3EB),
        text_2: hex(0xC8C1B7),
        text_3: hex(0x9C968E),
        rewind: hex(0x8EA6FF),
        rewind_bright: hex(0xBDC9FF),
        gold: hex(0xE8BD72),
        mint: hex(0x74D4B5),
        rose: hex(0xEF98B1),
        lanes: [hex(0xA597FF), hex(0x7CC6EE), hex(0x74D4B5), hex(0xEF98B1)],
    };

    pub const LIGHT: Self = Self {
        dark: false,
        canvas: hex(0xF0EBE3),
        surface: hex(0xFBF7F0),
        raised: hex(0xE7E1D7),
        line: hex(0xD3CCC0),
        text: hex(0x171419),
        text_2: hex(0x645F63),
        text_3: hex(0x7E777A),
        rewind: hex(0x4F63C9),
        rewind_bright: hex(0x697FDB),
        gold: hex(0x9A6A12),
        mint: hex(0x1D8A62),
        rose: hex(0xB8435C),
        lanes: [hex(0x5A47D6), hex(0x1F7AA6), hex(0x1D8A62), hex(0xB8435C)],
    };

    pub fn for_mode(mode: theme::Mode) -> Self {
        match mode {
            theme::Mode::Light => Self::LIGHT,
            theme::Mode::Dark | theme::Mode::None => Self::DARK,
        }
    }

    pub fn lane(&self, lane: usize) -> Color {
        self.lanes[lane % self.lanes.len()]
    }

    pub fn theme(&self) -> Theme {
        Theme::custom(
            if self.dark {
                "rsRewind dark"
            } else {
                "rsRewind light"
            },
            theme::Palette {
                background: self.canvas,
                text: self.text,
                primary: self.rewind,
                success: self.mint,
                warning: self.gold,
                danger: self.rose,
            },
        )
    }

    pub fn panel(&self) -> container::Style {
        container::Style {
            text_color: Some(self.text),
            background: Some(Background::Color(self.surface)),
            border: Border {
                color: self.line,
                width: 1.0,
                radius: 10.0.into(),
            },
            shadow: Shadow::default(),
            snap: true,
        }
    }

    pub fn bar(&self) -> container::Style {
        container::Style {
            text_color: Some(self.text),
            background: Some(Background::Color(self.canvas)),
            border: Border::default(),
            shadow: Shadow::default(),
            snap: true,
        }
    }

    /// A pill-shaped metadata label.
    pub fn pill(&self, accent: Color) -> container::Style {
        container::Style {
            text_color: Some(accent),
            background: Some(Background::Color(accent.scale_alpha(0.12))),
            border: Border {
                color: accent.scale_alpha(0.45),
                width: 1.0,
                radius: 999.0.into(),
            },
            shadow: Shadow::default(),
            snap: true,
        }
    }

    pub fn search(&self, status: text_input::Status) -> text_input::Style {
        let focused = matches!(status, text_input::Status::Focused { .. });
        text_input::Style {
            background: Background::Color(self.raised),
            border: Border {
                color: if focused { self.rewind } else { self.line },
                width: if focused { 1.5 } else { 1.0 },
                radius: 999.0.into(),
            },
            icon: self.text_3,
            placeholder: self.text_3,
            value: self.text,
            selection: self.rewind.scale_alpha(0.35),
        }
    }

    /// A list row or filter chip; `selected` rows carry the periwinkle edge.
    pub fn row(&self, selected: bool, status: button::Status, accent: Color) -> button::Style {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(if selected {
                accent.scale_alpha(0.14)
            } else if hovered {
                self.raised
            } else {
                Color::TRANSPARENT
            })),
            text_color: self.text,
            border: Border {
                color: if selected {
                    accent.scale_alpha(0.7)
                } else {
                    Color::TRANSPARENT
                },
                width: 1.0,
                radius: 8.0.into(),
            },
            shadow: Shadow::default(),
            snap: true,
        }
    }

    pub fn transport(&self, status: button::Status) -> button::Style {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(if hovered {
                self.raised
            } else {
                self.surface
            })),
            text_color: if hovered {
                self.rewind_bright
            } else {
                self.text_2
            },
            border: Border {
                color: self.line,
                width: 1.0,
                radius: 999.0.into(),
            },
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

/// UI text: Segoe UI Variable on Windows (falls back to the system sans elsewhere).
pub const UI_FONT: Font = Font::with_name("Segoe UI Variable Text");
/// Timestamps, ids and paths.
pub const MONO: Font = Font::with_name("Cascadia Mono");
