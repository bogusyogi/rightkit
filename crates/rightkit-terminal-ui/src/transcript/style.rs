//! Shared TUI colors and semantic styles, including the picker and transcript accent.
//! Informative accents meet minimum contrast on known backgrounds and supported palettes.

mod contrast;

use crate::transcript::color::blend;
use crate::transcript::color::is_light;
use crate::transcript::terminal_palette::StdoutColorLevel;
use crate::transcript::terminal_palette::best_color;
use crate::transcript::terminal_palette::default_bg;
use crate::transcript::terminal_palette::default_fg;
use crate::transcript::terminal_palette::effective_stdout_color_level;
use crate::transcript::terminal_palette::rgb_color;
use crate::transcript::terminal_palette::stdout_color_level;
use ratatui::style::Color;
use ratatui::style::Style;

const LIGHT_BG_ACCENT_RGB: (u8, u8, u8) = (28, 100, 200);

/// ChatGPT Blue 200 (#63A8F8).
pub(crate) const CHATGPT_BLUE_200: (u8, u8, u8) = (99, 168, 248);

/// Shared accent for picker selection backgrounds and transcript foreground emphasis.
pub(crate) const UI_ACCENT: (u8, u8, u8) = CHATGPT_BLUE_200;

// Decorative table rules should remain visible without competing with cell content.
const TABLE_SEPARATOR_FG_ALPHA: f32 = 0.20;

/// Returns a low-contrast rule style for separators within markdown tables.
pub(crate) fn table_separator_style() -> Style {
    table_separator_style_for(default_fg(), default_bg(), stdout_color_level())
}

/// Returns the foreground accent without imposing bold or dim text modifiers.
pub(crate) fn accent_color() -> Color {
    accent_color_for(default_bg())
}

/// Resolve emphasis on the shaded prompt surface, including its quantized background.
#[allow(dead_code, reason = "Used by later layers of the TUI refresh stack.")]
pub(crate) fn user_message_accent_color() -> Color {
    user_message_accent_color_for(default_bg(), effective_stdout_color_level())
}

#[allow(dead_code, reason = "Used by later layers of the TUI refresh stack.")]
fn user_message_accent_color_for(
    background: Option<(u8, u8, u8)>,
    level: StdoutColorLevel,
) -> Color {
    let preferred = if background.is_some_and(is_light) {
        LIGHT_BG_ACCENT_RGB
    } else {
        UI_ACCENT
    };
    let surface = background.and_then(|background| {
        match crate::transcript::terminal_palette::best_color_for_level(
            user_message_bg_rgb(background),
            level,
        ) {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            Color::Indexed(index) => {
                Some(crate::transcript::terminal_palette::XTERM_COLORS[usize::from(index)])
            }
            _ => None,
        }
    });
    contrast::foreground(preferred, surface, level)
}

fn accent_color_for(terminal_bg: Option<(u8, u8, u8)>) -> Color {
    let preferred = if terminal_bg.is_some_and(is_light) {
        LIGHT_BG_ACCENT_RGB
    } else {
        UI_ACCENT
    };
    contrast::foreground(preferred, terminal_bg, effective_stdout_color_level())
}

fn table_separator_style_for(
    terminal_fg: Option<(u8, u8, u8)>,
    terminal_bg: Option<(u8, u8, u8)>,
    color_level: StdoutColorLevel,
) -> Style {
    let (Some(fg), Some(bg)) = (terminal_fg, terminal_bg) else {
        return Style::default().dim();
    };
    let separator_rgb = blend(fg, bg, TABLE_SEPARATOR_FG_ALPHA);
    match color_level {
        StdoutColorLevel::TrueColor => Style::default().fg(rgb_color(separator_rgb)),
        StdoutColorLevel::Ansi256 => Style::default().fg(best_color(separator_rgb)),
        StdoutColorLevel::Ansi16 | StdoutColorLevel::Unknown => Style::default().dim(),
    }
}

pub(crate) fn user_message_bg_rgb(terminal_bg: (u8, u8, u8)) -> (u8, u8, u8) {
    let (top, alpha) = if is_light(terminal_bg) {
        ((0, 0, 0), 0.04)
    } else {
        ((255, 255, 255), 0.12)
    };
    blend(top, terminal_bg, alpha)
}
