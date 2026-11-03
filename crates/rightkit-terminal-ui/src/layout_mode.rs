//! S:I7 (layout half) — responsive/plain/reduced-motion rendering mode.
//!
//! `terminal/capability.rs` already handles color-depth fallback (24-bit →
//! 256 → 16 → monochrome). What is missing, per the reconciled spec's I7
//! row, is a *layout*-level mode: `--plain` asks for a linear, undecorated
//! rendering (no spinners, no animated glyphs, one line per event) and
//! reduced motion asks the same of anything that would otherwise animate,
//! independent of color support. This module is that mode selection --
//! deliberately small and decision-only, so `tui_host.rs` (which owns the
//! actual render loop) can match on it without this crate reaching into
//! rendering decisions that belong to the host.

/// How much motion/decoration the renderer should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    /// Full interactive rendering: animated spinners, live-updating rows,
    /// color and glyphs per `component::Theme`.
    Normal,
    /// `--plain`: linear output, one line per settled event, no animation,
    /// no redraw-in-place. Intended for non-TTY output and CI logs as much
    /// as for a user who asked for it directly.
    Plain,
    /// Interactive rendering minus motion: no animated spinners or
    /// transitions, but still redraws in place and still uses color/glyphs.
    /// Distinct from `Plain`, which also gives up the interactive layout.
    ReducedMotion,
}

impl LayoutMode {
    /// Resolve from explicit request plus environment signals, in priority
    /// order: an explicit `--plain` flag wins outright; otherwise an
    /// explicit `--reduced-motion` flag; otherwise the `NO_COLOR`/
    /// `TERM=dumb` environment convention implies reduced motion (never
    /// full plain -- an env convention should not silently drop the whole
    /// interactive layout, only its animation).
    pub fn resolve(
        plain_flag: bool,
        reduced_motion_flag: bool,
        no_color_env: bool,
        term_dumb: bool,
    ) -> Self {
        if plain_flag {
            LayoutMode::Plain
        } else if reduced_motion_flag || no_color_env || term_dumb {
            LayoutMode::ReducedMotion
        } else {
            LayoutMode::Normal
        }
    }

    /// Whether the renderer should animate anything (spinners, blinking
    /// cursors beyond the terminal's own, transition effects).
    pub fn animates(self) -> bool {
        matches!(self, LayoutMode::Normal)
    }

    /// Whether output should be a flat, append-only stream (true "plain")
    /// rather than an in-place-redrawing interactive layout.
    pub fn is_linear(self) -> bool {
        matches!(self, LayoutMode::Plain)
    }
}
