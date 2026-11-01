//! Link hover owns the mouse shape only while the TUI captures pointer input.
//! Remember motion across repaints, but discard coordinates across focus and geometry changes.
//! Cleanup is independent of the host so panic, suspend, and editor handoffs also restore the shape.
//! Kitty releases its override; Ghostty retains its existing text-cursor fallback.
//!
//! Lifted from donor `codex-rs/tui/src/tui/link_pointer.rs` at
//! `444da310e108da16aaeb18fd790b0ac464f08aca` (Apache-2.0). Adapted seams:
//! [`LinkHover::observe`] consumes our `component::Event` instead of the
//! donor's `TuiEvent`, and the donor's `impl Tui::set_link_pointer` (which
//! gates on terminal name and forwards to the alternate-screen singleton) is
//! not ported -- wiring it belongs to the host/input owner (see the result
//! report).

use std::io::Write;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

use crate::component::Event;
use crate::terminal::capability::TerminalName;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;

/// Last-seen pointer position while capturing, cleared on any event that
/// invalidates screen coordinates.
#[derive(Default)]
pub(crate) struct LinkHover {
    pub(crate) mouse: Option<MouseEvent>,
}

impl LinkHover {
    pub(crate) fn observe(&mut self, event: &Event) {
        match event {
            Event::Mouse(mouse) => {
                self.mouse = (!matches!(
                    mouse.kind,
                    MouseEventKind::Down(_) | MouseEventKind::Drag(_)
                ))
                .then_some(*mouse);
            }
            Event::Key(key) => {
                if let Some(mouse) = &mut self.mouse {
                    mouse.modifiers = key.modifiers;
                }
            }
            Event::Focus(false) | Event::Resize { .. } | Event::Resume => self.mouse = None,
            _ => {}
        }
    }
}

// 0: untouched/restored, 1: captured arrow, 2: link hand, 3: uncertain after a write failure.
// Keep cleanup armed until flush succeeds.
#[derive(Default)]
pub(super) struct LinkPointer(AtomicU8);

impl LinkPointer {
    pub(super) const fn new() -> Self {
        Self(AtomicU8::new(/*v*/ 0))
    }

    pub(super) fn update(&self, writer: &mut impl Write, over_link: bool) -> std::io::Result<()> {
        let previous = self.0.load(Ordering::Relaxed);
        let next = if over_link { 2 } else { 1 };
        if previous == next || (previous == 0 && !over_link) {
            return Ok(());
        }
        self.0.store(next, Ordering::Relaxed);
        let result = writer
            .write_all(if over_link {
                b"\x1b]22;pointer\x1b\\"
            } else {
                b"\x1b]22;default\x1b\\"
            })
            .and_then(|()| writer.flush());
        if result.is_err() {
            self.0.store(/*val*/ 3, Ordering::Relaxed);
        }
        result
    }

    pub(super) fn restore(
        &self,
        writer: &mut impl Write,
        terminal: TerminalName,
    ) -> std::io::Result<()> {
        if self.0.load(Ordering::Relaxed) != 0 {
            // In Kitty, a named shape persists across mouse capture and screen changes.
            // An empty OSC 22 releases that override back to the terminal's default policy.
            writer.write_all(if terminal == TerminalName::Kitty {
                b"\x1b]22;\x1b\\"
            } else {
                b"\x1b]22;text\x1b\\"
            })?;
            writer.flush()?;
            self.0.store(/*val*/ 0, Ordering::Relaxed);
        }
        Ok(())
    }
}

/// OSC 22 names differ between terminals. Restrict CSS names to known
/// implementations; multiplexers may consume OSC 22 and do not promise
/// pointer-shape passthrough (donor `Tui::set_link_pointer` gate).
pub(crate) fn link_pointer_supported() -> bool {
    let info = crate::terminal::capability::terminal_info();
    info.multiplexer.is_none() && matches!(info.name, TerminalName::Ghostty | TerminalName::Kitty)
}

#[cfg(test)]
#[path = "link_pointer_tests.rs"]
mod tests;
