//! Resolve informative foregrounds against their actual background before palette reduction.
//! Missing background samples preserve the requested palette color; unknown color capabilities
//! and user-defined ANSI palettes retain the terminal's default foreground.

use crate::transcript::color::blend;
use crate::transcript::color::perceptual_distance;
use crate::transcript::terminal_palette::StdoutColorLevel;
use crate::transcript::terminal_palette::XTERM_COLORS;
use crate::transcript::terminal_palette::best_color_for_level;
use crate::transcript::terminal_palette::indexed_color;
use crate::transcript::terminal_palette::rgb_color;
use ratatui::style::Color;
use std::cell::RefCell;
use std::collections::HashMap;

const MIN_TEXT_CONTRAST: f64 = 4.5;
const MAX_CACHED_FOREGROUNDS: usize = 512;

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct ForegroundKey {
    preferred: (u8, u8, u8),
    background: Option<(u8, u8, u8)>,
    level: StdoutColorLevel,
}

thread_local! {
    // Diff spans reuse a small palette. Bound storage even when themes contain many colors.
    // Resolved backgrounds and capabilities are part of the key, so probe/theme changes cannot
    // reuse a result for the previous surface.
    static FOREGROUNDS: RefCell<HashMap<ForegroundKey, Color>> = RefCell::default();
}

pub(super) fn foreground(
    preferred: (u8, u8, u8),
    background: Option<(u8, u8, u8)>,
    level: StdoutColorLevel,
) -> Color {
    FOREGROUNDS.with(|cache| {
        let key = ForegroundKey {
            preferred,
            background,
            level,
        };
        let mut cache = cache.borrow_mut();
        if let Some(color) = cache.get(&key) {
            return *color;
        }
        let color = match (background, level) {
            (None, _) => best_color_for_level(preferred, level),
            (Some(background), StdoutColorLevel::TrueColor) => {
                if ratio(preferred, background) >= MIN_TEXT_CONTRAST {
                    rgb_color(preferred)
                } else {
                    let black = (0, 0, 0);
                    let white = (255, 255, 255);
                    let endpoint = if ratio(black, background) >= ratio(white, background) {
                        black
                    } else {
                        white
                    };
                    // Bounded search preserves as much of the requested hue as the surface allows.
                    (1..=255)
                        .map(|step| blend(endpoint, preferred, step as f32 / 255.0))
                        .find(|candidate| ratio(*candidate, background) >= MIN_TEXT_CONTRAST)
                        .map_or_else(|| rgb_color(endpoint), rgb_color)
                }
            }
            (Some(background), StdoutColorLevel::Ansi256) => XTERM_COLORS
                .iter()
                .enumerate()
                .skip(/*n*/ 16)
                .filter(|(_, color)| ratio(**color, background) >= MIN_TEXT_CONTRAST)
                .min_by(|(_, a), (_, b)| {
                    perceptual_distance(**a, preferred)
                        .total_cmp(&perceptual_distance(**b, preferred))
                })
                .map_or(Color::Reset, |(index, _)| indexed_color(index as u8)),
            (Some(_), StdoutColorLevel::Ansi16 | StdoutColorLevel::Unknown) => Color::Reset,
        };
        if cache.len() >= MAX_CACHED_FOREGROUNDS {
            cache.clear();
        }
        cache.insert(key, color);
        color
    })
}

/// WCAG 2.x contrast ratio of two terminal colours (`rightkit_theme::color`).
fn ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    rightkit_theme::color::contrast_ratio(rgb(a), rgb(b))
}

fn rgb((r, g, b): (u8, u8, u8)) -> rightkit_theme::color::Rgb {
    rightkit_theme::color::Rgb {
        r: f64::from(r),
        g: f64::from(g),
        b: f64::from(b),
    }
}
