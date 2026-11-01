//! Inspect the completed frame without adding test helpers to the terminal implementation.
//!
//! Lifted from the Codex donor `codex-rs/tui/src/custom_terminal_test_support.rs`
//! at revision `444da310e108da16aaeb18fd790b0ac464f08aca` (Apache-2.0).

use std::io;
use std::io::Write;

use ratatui::backend::Backend;
use ratatui::buffer::Buffer;

use super::Terminal;

#[allow(dead_code)] // consumed by the host-side frame adapter tests, not this module
pub(crate) fn last_rendered_buffer<B>(terminal: &Terminal<B>) -> &Buffer
where
    B: Backend<Error = io::Error> + Write,
{
    terminal.previous_buffer()
}
