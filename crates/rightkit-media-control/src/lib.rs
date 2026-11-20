//! Minimal session-owned media pause/resume while a recording runs.
//!
//! The whole policy:
//! - Recording start: for every player observed playing, send explicit
//!   PAUSE and remember (session id, player identity). Nothing playing,
//!   nothing done.
//! - Recording stop/cancel/failure: for every player this session paused,
//!   send explicit PLAY only if that same player is still observed and is
//!   not playing again. Then forget it. A failed or empty observation never
//!   permits a command.
//! - Never toggle. Never mute/duck. No metadata beyond identity.
//!
//! One shared, platform-independent lifecycle (`Lifecycle<B>`) drives a tiny
//! per-platform `Backend`: `observe`, `pause`, `play`. All commands for a
//! platform run on ONE worker thread fed by a FIFO channel, so a stop that
//! arrives while a slow start is still probing is processed after it and
//! restores what that start paused. The recording path only ever enqueues.

mod backend;
mod worker;

#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "macos")]
pub mod mac;

pub use backend::{classify_post_observation, Backend, CommandOutcome, Observation, Observed};
pub use worker::{begin_for_session, install_backend, restore_any, restore_for_session};

#[cfg(target_os = "macos")]
pub use mac::{MacBackend, MacPlayer};

use std::sync::OnceLock;

/// Receives every diagnostic event this crate emits: an event name and a short detail string that
/// never contains media metadata beyond a player identity.
pub type EventSink = fn(event: &str, detail: &str);

static EVENT_SINK: OnceLock<EventSink> = OnceLock::new();

/// Install the host's event sink once, before the first recording. Without one, events are dropped.
/// Returns false when a sink was already installed.
pub fn set_event_sink(sink: EventSink) -> bool {
    EVENT_SINK.set(sink).is_ok()
}

pub(crate) fn event(event: &str, detail: &str) {
    if let Some(sink) = EVENT_SINK.get() {
        sink(event, detail);
    }
}
