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

/// ChatGPT Blue 100 (#A4CDFB), used for selection fills on light backgrounds.
#[cfg(test)]
pub(crate) const CHATGPT_BLUE_100: (u8, u8, u8) = (164, 205, 251);

/// ChatGPT Blue 200 (#63A8F8).
pub(crate) const CHATGPT_BLUE_200: (u8, u8, u8) = (99, 168, 248);

/// Shared accent for picker selection backgrounds and transcript foreground emphasis.
pub(crate) const UI_ACCENT: (u8, u8, u8) = CHATGPT_BLUE_200;

#[derive(Clone, Copy)]
#[cfg(test)]
pub(crate) enum StatusTone {
    Success,
    Attention,
    Failure,
}

#[cfg(test)]
fn status_style_for(
    tone: StatusTone,
    terminal_bg: Option<(u8, u8, u8)>,
    color_level: StdoutColorLevel,
) -> Style {
    let light = terminal_bg.is_some_and(is_light);
    let color = match (tone, color_level) {
        (_, StdoutColorLevel::Unknown) => Color::Reset,
        (StatusTone::Success, _) => Color::Green,
        (StatusTone::Failure, _) => Color::Red,
        // Yellow can disappear on light themes; use it only with a known dark background.
        (StatusTone::Attention, _) if light || terminal_bg.is_none() => Color::Reset,
        (StatusTone::Attention, _) => Color::Yellow,
    };
    Style::default().fg(color).bold()
}
// Decorative table rules should remain visible without competing with cell content.
const TABLE_SEPARATOR_FG_ALPHA: f32 = 0.20;

/// Returns a low-contrast rule style for separators within markdown tables.
pub(crate) fn table_separator_style() -> Style {
    table_separator_style_for(default_fg(), default_bg(), stdout_color_level())
}

/// Returns the shared accent style for active or selected TUI controls.
#[cfg(test)]
pub(crate) fn accent_style() -> Style {
    if matches!(
        effective_stdout_color_level(),
        StdoutColorLevel::TrueColor | StdoutColorLevel::Ansi256
    ) && let Some(mut style) =
        crate::transcript::highlight::foreground_style_for_scopes(&["codex.accent"])
    {
        if let Some(Color::Rgb(r, g, b)) = style.fg {
            style = style.fg(best_color((r, g, b)));
        }
        return style.bold();
    }
    accent_style_for(default_bg())
}

/// Returns the foreground accent without imposing bold or dim text modifiers.
pub(crate) fn accent_color() -> Color {
    accent_color_for(default_bg())
}

#[cfg(test)]
fn background_rgb(background: Option<Color>) -> Option<(u8, u8, u8)> {
    match background {
        Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
        Some(Color::Indexed(index)) if index >= 16 => {
            Some(crate::transcript::terminal_palette::XTERM_COLORS[usize::from(index)])
        }
        None | Some(Color::Reset) => default_bg(),
        _ => None,
    }
}

/// Keep theme-derived text readable on its painted surface, preserving terminal-owned ANSI colors.
#[cfg(test)]
pub(crate) fn readable_color_on(preferred: Color, background: Option<Color>) -> Color {
    let preferred = match preferred {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(index) if index >= 16 => {
            Some(crate::transcript::terminal_palette::XTERM_COLORS[usize::from(index)])
        }
        Color::Reset => default_fg(),
        _ => return preferred,
    };
    preferred.map_or(Color::Reset, |preferred| {
        contrast::foreground(
            preferred,
            background_rgb(background),
            effective_stdout_color_level(),
        )
    })
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

/// Returns the shared accent style for the provided terminal background.
#[cfg(test)]
pub(crate) fn accent_style_for(terminal_bg: Option<(u8, u8, u8)>) -> Style {
    Style::default().fg(accent_color_for(terminal_bg)).bold()
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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn status_colors_preserve_light_terminal_themes() {
        for level in [StdoutColorLevel::TrueColor, StdoutColorLevel::Ansi256] {
            for bg in [(255, 255, 255), (130, 130, 130), (220, 210, 180)] {
                for (tone, color) in [
                    (StatusTone::Success, Color::Green),
                    (StatusTone::Attention, Color::Reset),
                    (StatusTone::Failure, Color::Red),
                ] {
                    assert_eq!(
                        status_style_for(tone, Some(bg), level),
                        Style::default().fg(color).bold(),
                    );
                }
            }
            for bg in [(0, 0, 0), (0, 218, 0)] {
                assert_eq!(
                    status_style_for(StatusTone::Attention, Some(bg), level),
                    Style::default().fg(Color::Yellow).bold(),
                );
            }
            assert_eq!(
                status_style_for(StatusTone::Attention, /*terminal_bg*/ None, level),
                Style::default().fg(Color::Reset).bold(),
            );
        }
    }

    #[test]
    fn status_colors_preserve_ansi16_and_no_color_fallbacks() {
        for (tone, light, dark) in [
            (StatusTone::Success, Color::Green, Color::Green),
            (StatusTone::Attention, Color::Reset, Color::Yellow),
            (StatusTone::Failure, Color::Red, Color::Red),
        ] {
            for (bg, expected) in [((255, 255, 255), light), ((0, 0, 0), dark)] {
                assert_eq!(
                    status_style_for(tone, Some(bg), StdoutColorLevel::Ansi16),
                    Style::default().fg(expected).bold()
                );
                assert_eq!(
                    status_style_for(tone, Some(bg), StdoutColorLevel::Unknown),
                    Style::default().fg(Color::Reset).bold()
                );
            }
        }
    }

    #[test]
    fn theme_accents_respect_terminal_color_depth() {
        const CHILD: &str = "CODEX_ACCENT_COLOR_TEST_CHILD";
        let Ok(level) = std::env::var(CHILD) else {
            for level in ["0", "1", "2", "3"] {
                #[allow(clippy::disallowed_methods)]
                let mut child_command =
                    std::process::Command::new(std::env::current_exe().unwrap());
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt as _;
                    child_command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                }
                let output = child_command
                    .args([
                        "--exact",
                        "style::tests::theme_accents_respect_terminal_color_depth",
                    ])
                    .env(CHILD, level)
                    .env("FORCE_COLOR", level)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "level {level}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        };
        let theme =
            crate::transcript::highlight::resolve_theme_by_name("ada", /*codex_home*/ None)
                .unwrap();
        crate::transcript::highlight::set_syntax_theme(theme);
        let (expected_level, expected_foreground) = match level.as_str() {
            "3" => (StdoutColorLevel::TrueColor, rgb_color((95, 175, 255))),
            "2" => (
                StdoutColorLevel::Ansi256,
                crate::transcript::terminal_palette::indexed_color(/*index*/ 75),
            ),
            "1" => (StdoutColorLevel::Ansi16, Color::Reset),
            "0" => (StdoutColorLevel::Unknown, Color::Reset),
            _ => unreachable!(),
        };
        assert_eq!(effective_stdout_color_level(), expected_level);
        crate::transcript::terminal_palette::set_default_colors_from_startup_probe(
            /*colors*/ None,
        );
        // Preserve configured theme accents when the terminal can represent their palette.
        assert_eq!(
            accent_style(),
            Style::default().fg(expected_foreground).bold()
        );
        assert_eq!(
            readable_color_on(rgb_color((95, 175, 255)), /*background*/ None),
            expected_foreground,
        );
        // An explicit surface exercises the real child-process depth without the truecolor
        // override used by widget palette fixtures.
        assert_eq!(
            readable_color_on(rgb_color((95, 175, 255)), Some(rgb_color((24, 24, 24)))),
            expected_foreground,
        );
    }

    #[test]
    fn table_separator_blends_toward_dark_background() {
        let style = table_separator_style_for(
            Some((255, 255, 255)),
            Some((0, 0, 0)),
            StdoutColorLevel::TrueColor,
        );

        assert_eq!(style.fg, Some(rgb_color((51, 51, 51))));
    }

    #[test]
    fn table_separator_blends_toward_light_background() {
        let style = table_separator_style_for(
            Some((0, 0, 0)),
            Some((255, 255, 255)),
            StdoutColorLevel::TrueColor,
        );

        assert_eq!(style.fg, Some(rgb_color((204, 204, 204))));
    }

    #[test]
    fn table_separator_dims_when_palette_aware_color_is_unavailable() {
        let expected = Style::default().dim();

        assert_eq!(
            table_separator_style_for(
                Some((255, 255, 255)),
                Some((0, 0, 0)),
                StdoutColorLevel::Ansi16,
            ),
            expected
        );
        assert_eq!(
            table_separator_style_for(
                /*terminal_fg*/ None,
                Some((0, 0, 0)),
                StdoutColorLevel::TrueColor,
            ),
            expected
        );
    }
}
