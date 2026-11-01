//! the host app's terminal client.
//!
//! # Shape
//!
//! The conversation renders **inline**: finalized lines are pushed into the
//! terminal's own scrollback, and only a small live viewport at the bottom is
//! diffed and redrawn. Native scrolling, selection, search, and copy therefore
//! work as they do in any shell, and a streamed token repaints a few rows
//! rather than a transcript. The alternate screen is reserved for named modals
//! -- pickers, diff viewers, approval dialogs -- and always given back.
//!
//! # Layers
//!
//! - [`terminal`] owns the host: what it can do ([`terminal::capability`]),
//!   whether it is light or dark, and how to get lines into scrollback
//!   ([`terminal::history`]).
//! - [`scheduler`] decides *when* to draw. Two lanes: keystrokes echo
//!   immediately, model output coalesces.
//! - [`app`] owns viewport geometry and screen-mode transitions.
//! - [`component`] is the contract UI pieces implement, and the theme they
//!   resolve colour through.
//!
//! # Invariants
//!
//! Presentation only. No engine, provider, registry, or storage type is
//! constructed here; session operations belong to the daemon client. Commands
//! come from the shared registry via [`component::CommandSource`] so that a
//! slash command and its CLI subcommand are one implementation, never two.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

pub mod app;
pub mod bots_panel;
pub mod browser_panel;
pub mod ci_band;
pub mod command_effects;
pub mod component;
pub mod composer;
pub mod diff_review_pane;
pub mod identity_strip;
pub mod interject;
pub mod layout_mode;
pub mod nav;
pub mod observability_pane;
pub mod pty_attach;
pub mod render;
pub mod running_panel;
pub mod scheduler;
pub mod terminal;
pub mod todo_overlay;
pub mod transcript;
pub mod user_shell;

pub use app::{ModalKind, ScreenMode, Surface};
pub use component::{Component, Event, Outcome, Role, Theme};
pub use scheduler::{FrameRequester, MIN_FRAME_INTERVAL};
pub use terminal::{
    Background, Capabilities, ColorDepth, ImageProtocol, TerminalProfile, Viewport,
};

// Re-exported so the host binary (`engine/host-app/src/tui_host.rs`,
// R6) can drive the real terminal and build ratatui frames without the bin
// crate taking its own direct dependency on these two crates. The host needs
// `crossterm::event::EventStream` (the timed-read escape/arrow-key
// disambiguation described on `component::Event::Key`) and `ratatui::Terminal`
// to actually paint; nothing in this crate's own API surface exposes either.
// Re-exporting the crate reference here does not change what either crate
// depends on -- it is the same `ratatui`/`crossterm` this crate already pins
// in `Cargo.toml`, just visible one hop further out.
pub use crossterm;
pub use ratatui;

// The render-class registration seam.
//
// `render::registry::classify` resolves a tool name to a [`render::RenderClass`]
// from a static list of legacy names that almost never match this engine's real
// production tool names (`read_file`, `shell`, `browser_fetch`, ...), so without
// registration every production call would fall back to `Generic`. The fix the
// audit preferred -- render-class metadata on tool descriptors in the tools
// crate -- would invert the presentation/engine dependency; instead the host
// binary (which already depends on both crates) registers an intentional class
// per production tool name at boot via [`register_render_classes`], and
// `classify` consults those registrations before its static fallback.
//
// Why these functions live at the crate root rather than in
// `render::registry`: that module is private behind `render/mod.rs`'s closed
// re-export list, so items added there are unreachable from outside the crate.
// The seam needs a public path the binary can name; the crate root is the
// narrowest one that does not widen `render/mod.rs`'s export surface.
//
// Unknown names still fall back to `Generic` deliberately: registration only
// ever refines the classification of a named tool, never widens what an
// unrecognized one may draw.

/// Process-wide intentional tool-name -> render-class registrations.
///
/// A poisoned lock (a writer panicked mid-update) degrades to "no
/// registrations" -- `classify` then falls back to its static table and
/// `Generic`. It never panics and never blocks a renderer on formatting.
fn render_class_registrations() -> &'static RwLock<HashMap<String, crate::render::RenderClass>> {
    static REGISTRATIONS: OnceLock<RwLock<HashMap<String, crate::render::RenderClass>>> =
        OnceLock::new();
    REGISTRATIONS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register the intentional render class for one production tool name.
///
/// Names are matched case-insensitively by `classify`; registering the same
/// name twice keeps the last class. See the module-level comment above for
/// why this seam exists and who calls it.
pub fn register_render_class(tool_name: &str, class: crate::render::RenderClass) {
    if let Ok(mut registrations) = render_class_registrations().write() {
        registrations.insert(tool_name.to_ascii_lowercase(), class);
    }
}

/// Bulk-register intentional render classes for production tool names.
///
/// Called once at host boot with the binary's production-name mapping table;
/// see `engine/host-app/src/tui_host.rs`.
pub fn register_render_classes<I, S>(entries: I)
where
    I: IntoIterator<Item = (S, crate::render::RenderClass)>,
    S: AsRef<str>,
{
    for (name, class) in entries {
        register_render_class(name.as_ref(), class);
    }
}

/// The registered class for `tool_name`, if any. Crate-internal lookup used
/// by `render::registry::classify` before its static fallback.
pub(crate) fn lookup_registered_render_class(
    tool_name: &str,
) -> Option<crate::render::RenderClass> {
    let registrations = render_class_registrations().read().ok()?;
    registrations.get(&tool_name.to_ascii_lowercase()).copied()
}

// Inline images are deliberately absent.
//
// The previous stub emitted a Kitty graphics escape gated on a caller-supplied
// boolean, with no protocol detection, no multiplexer rejection, no chunking of
// payloads over the 4096-byte limit, no dimensions, and no image lifecycle. On
// any terminal that does not speak the protocol it painted raw escape bytes
// into the user's scrollback, which they cannot clear.
//
// Emitting nothing is strictly better than emitting that. Image support returns
// with real detection (`terminal::capability::ImageProtocol` already resolves
// Kitty vs iTerm2 vs none, and already refuses under a multiplexer), chunking,
// and a textual placeholder wherever inline rendering is unavailable.
