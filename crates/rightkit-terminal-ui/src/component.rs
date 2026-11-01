//! The contract every UI piece implements, and the theme they resolve colour
//! through.
//!
//! Phase-2 modules (transcript, composer, tool rendering, navigation) depend on
//! this file and nothing else in the foundation. Changing anything here is a
//! breaking change for all of them.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::terminal::{Background, ColorDepth};

/// Widget rasterization shared by donor frames & Ratatui test frames.
/// Geometry & terminal I/O remain owned by the caller's terminal/surface.
pub trait RenderFrame {
    fn area(&self) -> Rect;
    fn buffer_mut(&mut self) -> &mut ratatui::buffer::Buffer;
    fn set_cursor(&mut self, position: ratatui::layout::Position);
}

/// Widget conveniences preserve object-safe component dispatch.
pub trait RenderFrameExt: RenderFrame {
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(&mut self, position: P) {
        self.set_cursor(position.into());
    }

    fn render_widget<W: ratatui::widgets::Widget>(&mut self, widget: W, area: Rect) {
        widget.render(area, self.buffer_mut());
    }

    fn render_stateful_widget<W: ratatui::widgets::StatefulWidget>(
        &mut self,
        widget: W,
        area: Rect,
        state: &mut W::State,
    ) {
        widget.render(area, self.buffer_mut(), state);
    }
}

impl<F: RenderFrame + ?Sized> RenderFrameExt for F {}

impl RenderFrame for Frame<'_> {
    fn area(&self) -> Rect {
        Frame::area(self)
    }

    fn buffer_mut(&mut self) -> &mut ratatui::buffer::Buffer {
        Frame::buffer_mut(self)
    }

    fn set_cursor(&mut self, position: ratatui::layout::Position) {
        Frame::set_cursor_position(self, position);
    }
}

/// Something that happened, delivered to components in focus order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A key was pressed, already disambiguated from escape sequences.
    ///
    /// A bare Escape and the first byte of an arrow key are the same byte, so
    /// telling them apart needs a timed read: a lone `\x1B` is only Escape once
    /// nothing follows it. Crossterm's parser does this, holding an incomplete
    /// sequence until it resolves.
    ///
    /// The live host feeds this from crossterm's buffered event parser
    /// (`tui_host::spawn_input_thread` uses `poll`/`read`, the same parser
    /// underneath `EventStream`) rather than reading tty bytes directly. The
    /// timing is what keeps an arrow sequence from becoming a random Escape
    /// over SSH or tmux.
    Key(crossterm::event::KeyEvent),
    /// A bracketed paste arrived, as one event rather than N keypresses.
    Paste(String),
    /// Terminal-native pointer input. Components may use this for selection,
    /// while text selection remains owned by terminal scrollback in inline
    /// mode.
    Mouse(crossterm::event::MouseEvent),
    /// Focus changed. This is state, not a synthetic key, so components can
    /// redraw focus affordances without mistaking it for user intent.
    Focus(bool),
    /// The terminal changed size.
    Resize { width: u16, height: u16 },
    /// The process resumed from suspension. Geometry must be re-queried; the
    /// last resize event cannot be trusted because resizes are not delivered
    /// while stopped.
    Resume,
}

/// What a component did with an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Handled. Stop propagating.
    Consumed,
    /// Not for me. Offer it to the next component.
    Ignored,
    /// Handled, and the display must be redrawn.
    Redraw,
}

impl Outcome {
    /// Whether this outcome should stop the event travelling further.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Consumed | Self::Redraw)
    }
}

/// A renderable, interactive piece of the UI.
pub trait Component {
    /// Draw into `area`. Called on every frame; render the current state in
    /// full rather than a delta, because frames may be dropped under load.
    fn render(&mut self, frame: &mut dyn crate::component::RenderFrame, area: Rect, theme: &Theme);

    /// React to an event. Default: ignore everything, render only.
    fn handle(&mut self, _event: &Event) -> Outcome {
        Outcome::Ignored
    }

    /// Rows this component wants, given the width and an upper bound.
    /// Default: whatever it is given.
    fn desired_height(&self, _width: u16, available: u16) -> u16 {
        available
    }
}

/// Semantic colour roles, resolved against the real terminal.
///
/// Components ask for meaning ("this failed"), never for a literal colour. The
/// theme decides what that looks like on a light background, a dark one, or a
/// 16-colour terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Ordinary text.
    Text,
    /// De-emphasised text: timestamps, hints, elided counts.
    Dim,
    /// A tool call or task in progress.
    Running,
    /// Completed successfully.
    Success,
    /// Failed.
    Failure,
    /// Needs the user's attention or input.
    Attention,
    /// Structural lines: borders, rules, separators.
    Border,
}

/// Resolves semantic roles to concrete colours for this terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    background: Background,
    depth: ColorDepth,
}

impl Theme {
    pub fn new(background: Background, depth: ColorDepth) -> Self {
        Self { background, depth }
    }

    pub fn background(&self) -> Background {
        self.background
    }

    pub fn depth(&self) -> ColorDepth {
        self.depth
    }

    /// The colour for a role on this terminal.
    ///
    /// Two independent adaptations happen here. Lightness: a green that reads
    /// well on black is washed out on white, so each role has a pair. Depth:
    /// the chosen 24-bit value is downsampled to 256 or 16 colours, or dropped
    /// entirely on a monochrome terminal.
    pub fn color(&self, role: Role) -> Color {
        if self.depth == ColorDepth::Monochrome {
            return Color::Reset;
        }

        let rgb = match (role, self.background) {
            // Let the terminal's own foreground win for ordinary text: it is by
            // definition readable against the user's background.
            (Role::Text, _) => return Color::Reset,

            // Contrast floor: >= #a8a6a0 for secondary text; nothing text-like
            // may fall below #8c8a85 (the host app TUI rev 2).
            (Role::Dim, Background::Dark) => (0xa8, 0xa6, 0xa0),
            (Role::Dim, Background::Light) => (0x6a, 0x6a, 0x6a),

            (Role::Running, Background::Dark) => (0x7f, 0xa8, 0xd8),
            (Role::Running, Background::Light) => (0x1e, 0x5a, 0x99),

            (Role::Success, Background::Dark) => (0x5f, 0xd7, 0x87),
            (Role::Success, Background::Light) => (0x0b, 0x74, 0x2e),

            (Role::Failure, Background::Dark) => (0xff, 0x6b, 0x6b),
            (Role::Failure, Background::Light) => (0xb3, 0x11, 0x1a),

            (Role::Attention, Background::Dark) => (0xf2, 0xc0, 0x5f),
            (Role::Attention, Background::Light) => (0x8a, 0x5d, 0x00),

            (Role::Border, Background::Dark) => (0x4a, 0x4a, 0x4a),
            (Role::Border, Background::Light) => (0xb0, 0xb0, 0xb0),
        };

        self.downsample(rgb)
    }

    /// The the host app brand accent (`#e53855`), used only for the wordmark's
    /// "Right". Not a semantic role: nothing but the lockup draws it.
    pub fn accent(&self) -> Color {
        if self.depth == ColorDepth::Monochrome {
            return Color::Reset;
        }
        self.downsample((0xe5, 0x38, 0x55))
    }

    /// The raised panel behind the user's sent messages (mockup `--panel`).
    /// `None` below 256 colours: a 16-colour palette has no neutral raised
    /// step, so the red rail alone marks the message there.
    pub fn panel_bg(&self) -> Option<Color> {
        if self.depth < ColorDepth::Ansi256 {
            return None;
        }
        Some(self.downsample(match self.background {
            Background::Dark => (0x1e, 0x1e, 0x1d),
            Background::Light => (0xee, 0xec, 0xe6),
        }))
    }

    fn downsample(&self, (r, g, b): (u8, u8, u8)) -> Color {
        match self.depth {
            ColorDepth::TrueColor => Color::Rgb(r, g, b),
            ColorDepth::Ansi256 => {
                Color::Indexed(crate::terminal::capability::rgb_to_xterm256(r, g, b))
            }
            ColorDepth::Ansi16 => {
                Color::Indexed(crate::terminal::capability::rgb_to_ansi16(r, g, b))
            }
            ColorDepth::Monochrome => Color::Reset,
        }
    }

    /// A glyph carrying the same meaning as the colour.
    ///
    /// Colour alone must never carry meaning: it is invisible on a monochrome
    /// terminal and ambiguous to a colour-blind reader. Every state that is
    /// coloured is also marked.
    pub fn glyph(&self, role: Role) -> &'static str {
        match role {
            Role::Running => "·",
            Role::Success => "✓",
            Role::Failure => "✗",
            Role::Attention => "!",
            Role::Text | Role::Dim | Role::Border => " ",
        }
    }
}

/// The seam onto the shared command registry.
///
/// The registry itself is owned elsewhere -- it is the single source of truth
/// for both the CLI and this TUI, so that `/model` and `session set --model`
/// resolve to one implementation. This trait is only the shape the TUI needs;
/// the owning crate implements it.
pub trait CommandSource {
    /// Resolve typed input to a command name, honouring aliases.
    /// `input` arrives without its leading slash.
    fn resolve(&self, input: &str) -> Option<String>;

    /// Names valid in the current context, for completion.
    fn completions(&self, prefix: &str) -> Vec<String>;

    /// Whether a command may run while a turn is in flight.
    fn valid_mid_turn(&self, name: &str) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_text_defers_to_the_terminal_foreground() {
        for bg in [Background::Dark, Background::Light] {
            let theme = Theme::new(bg, ColorDepth::TrueColor);
            assert_eq!(theme.color(Role::Text), Color::Reset);
        }
    }

    #[test]
    fn dim_text_never_falls_below_the_contrast_floor() {
        let theme = Theme::new(Background::Dark, ColorDepth::TrueColor);
        assert_eq!(theme.color(Role::Dim), Color::Rgb(0xa8, 0xa6, 0xa0));
        assert_eq!(theme.accent(), Color::Rgb(0xe5, 0x38, 0x55));
    }

    #[test]
    fn the_message_panel_exists_only_where_a_neutral_raised_step_does() {
        let dark = Theme::new(Background::Dark, ColorDepth::TrueColor);
        assert_eq!(dark.panel_bg(), Some(Color::Rgb(0x1e, 0x1e, 0x1d)));
        assert!(matches!(
            Theme::new(Background::Dark, ColorDepth::Ansi256).panel_bg(),
            Some(Color::Indexed(_))
        ));
        assert_eq!(
            Theme::new(Background::Dark, ColorDepth::Ansi16).panel_bg(),
            None
        );
        assert_eq!(
            Theme::new(Background::Dark, ColorDepth::Monochrome).panel_bg(),
            None
        );
    }

    #[test]
    fn success_and_failure_differ_on_both_backgrounds() {
        for bg in [Background::Dark, Background::Light] {
            let theme = Theme::new(bg, ColorDepth::TrueColor);
            assert_ne!(theme.color(Role::Success), theme.color(Role::Failure));
        }
    }

    #[test]
    fn the_same_role_differs_between_light_and_dark() {
        let dark = Theme::new(Background::Dark, ColorDepth::TrueColor);
        let light = Theme::new(Background::Light, ColorDepth::TrueColor);
        assert_ne!(dark.color(Role::Success), light.color(Role::Success));
        assert_ne!(dark.color(Role::Failure), light.color(Role::Failure));
    }

    #[test]
    fn colour_is_downsampled_to_the_terminals_depth() {
        let bg = Background::Dark;
        assert!(matches!(
            Theme::new(bg, ColorDepth::TrueColor).color(Role::Success),
            Color::Rgb(..)
        ));
        assert!(matches!(
            Theme::new(bg, ColorDepth::Ansi256).color(Role::Success),
            Color::Indexed(_)
        ));
        assert!(matches!(
            Theme::new(bg, ColorDepth::Ansi16).color(Role::Success),
            Color::Indexed(n) if n < 16
        ));
    }

    #[test]
    fn a_monochrome_terminal_gets_no_colour_at_all() {
        let theme = Theme::new(Background::Dark, ColorDepth::Monochrome);
        for role in [Role::Success, Role::Failure, Role::Running, Role::Dim] {
            assert_eq!(theme.color(role), Color::Reset);
        }
    }

    /// The accessibility rule, enforced: every state distinguished by colour is
    /// also distinguished by a glyph, so meaning survives without colour.
    #[test]
    fn state_roles_carry_a_distinct_glyph_as_well_as_a_colour() {
        let theme = Theme::new(Background::Dark, ColorDepth::TrueColor);
        let running = theme.glyph(Role::Running);
        let success = theme.glyph(Role::Success);
        let failure = theme.glyph(Role::Failure);
        assert_ne!(running, success);
        assert_ne!(success, failure);
        assert_ne!(running, failure);
    }

    #[test]
    fn outcomes_that_handled_an_event_stop_it_propagating() {
        assert!(Outcome::Consumed.is_terminal());
        assert!(Outcome::Redraw.is_terminal());
        assert!(!Outcome::Ignored.is_terminal());
    }
}

impl RenderFrame for crate::terminal::custom_terminal::Frame<'_> {
    fn area(&self) -> Rect {
        self.area()
    }
    fn buffer_mut(&mut self) -> &mut ratatui::buffer::Buffer {
        self.buffer_mut()
    }
    fn set_cursor(&mut self, position: ratatui::layout::Position) {
        self.set_cursor_position(position);
    }
}
