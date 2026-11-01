//! Terminal-specific scrollback strategy selection (donor
//! `codex-rs/tui/src/tui/scrollback.rs` `ScrollbackStrategy`), adapted to the
//! the host app seam: the donor drives `custom_terminal::Terminal`, while
//! the host app's `Surface` owns geometry and `terminal::history` owns the wire
//! protocol. This module keeps only the *decision* logic; insertion and
//! viewport growth run through [`super::history`].

#[cfg(test)]
use std::io::{self, Write};

#[cfg(test)]
use super::capability::{TerminalInfo, TerminalName};
#[cfg(test)]
use super::history::{self, HistoryLineWrapPolicy, InsertMode};

/// How this terminal safely moves rows into native scrollback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) enum ScrollbackStrategy {
    /// Scroll regions above the viewport plus newline scrolling at the
    /// region boundary.
    Standard,
    /// Zellij emulates scroll regions without confining soft-wrap
    /// continuation rows; terminal-wrapped history must take the full-screen
    /// path.
    Zellij,
    /// Windows Terminal discards rows scrolled out of a partial DECSTBM
    /// region; scroll the whole screen with newlines.
    FullScreen,
}

#[cfg(test)]
impl ScrollbackStrategy {
    /// Donor `ScrollbackStrategy::detect`.
    #[cfg(test)]
    pub(crate) fn detect(terminal: &TerminalInfo) -> Self {
        if terminal.is_zellij() {
            Self::Zellij
        } else if terminal.name == TerminalName::WindowsTerminal
            || std::env::var_os("WT_SESSION").is_some()
        {
            Self::FullScreen
        } else {
            Self::Standard
        }
    }

    /// Donor `ScrollbackStrategy::history_insertion_mode`, mapped onto our
    /// `InsertMode` (`Standard` == `ScrollRegion`).
    #[cfg(test)]
    pub(crate) fn history_insertion_mode(self, wrap_policy: HistoryLineWrapPolicy) -> InsertMode {
        match self {
            Self::FullScreen => InsertMode::FullScreen,
            Self::Zellij if wrap_policy == HistoryLineWrapPolicy::Terminal => {
                InsertMode::FullScreen
            }
            Self::Standard | Self::Zellij => InsertMode::ScrollRegion,
        }
    }

    /// Donor `ScrollbackStrategy::grow_viewport`, adapted to the `impl Write`
    /// seam: `Standard` scrolls the region above `viewport_top` with newlines,
    /// `FullScreen` clears below the viewport top and newline-scrolls the
    /// whole screen, and `Zellij` uses CSI S over the region (the donor's
    /// `backend.scroll_region_up`, emitted as raw ANSI so no ratatui
    /// `scrolling-regions` feature is needed).
    #[cfg(test)]
    pub(crate) fn grow_viewport(
        self,
        writer: &mut impl Write,
        viewport_top: u16,
        screen_height: u16,
        scroll_by: u16,
        restore_cursor: (u16, u16),
    ) -> io::Result<()> {
        match self {
            Self::Zellij => {
                if scroll_by == 0 || viewport_top == 0 {
                    return Ok(());
                }
                // CSI S inside the region above the viewport: rows scrolled
                // off the region's top enter the multiplexer's scrollback.
                write!(writer, "\x1b[1;{viewport_top}r")?;
                write!(writer, "\x1b[{};{}H", viewport_top, 1)?;
                for _ in 0..scroll_by {
                    writer.write_all(b"\x1b[S")?;
                }
                writer.write_all(b"\x1b[r")?;
                write!(
                    writer,
                    "\x1b[{};{}H",
                    restore_cursor.1.saturating_add(1),
                    restore_cursor.0.saturating_add(1)
                )?;
                writer.flush()
            }
            Self::Standard | Self::FullScreen => history::grow_viewport(
                writer,
                match self {
                    Self::Standard => InsertMode::ScrollRegion,
                    _ => InsertMode::FullScreen,
                },
                viewport_top,
                screen_height,
                scroll_by,
                restore_cursor,
            ),
        }
    }
}

#[cfg(test)]
#[path = "scrollback_tests.rs"]
mod tests;
