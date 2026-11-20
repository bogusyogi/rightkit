//! When to offer a pending model replacement: once per launch, never while the host is busy, and
//! without starting the download (the user's own action does that).
//!
//! The mechanism: a once-per-launch gate, a bounded wait for the host to go idle on a named
//! worker thread, the one-shot "download requested" flag the app's UI reads on mount
//! ([`crate::retired_model_cleanup::request_replacement_download`]), and the telemetry vocabulary.
//! The host supplies what "idle" means, and how the prompt is shown (its window, its wording, its
//! renderer events): none of that lives here.
//!
//! Telemetry (event `model_update_prompt`): `deferred_busy_timeout`, `shown`, `dismissed`; plus
//! `model_update_prompt_spawn_error`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::retired_model_cleanup::{
    replacement_pending, request_replacement_download, take_replacement_download_request,
};

const EVENT: &str = "model_update_prompt";
const SPAWN_ERROR_EVENT: &str = "model_update_prompt_spawn_error";

/// The once-per-launch gate. Keep one in a `static`.
pub struct PromptGate(AtomicBool);

impl PromptGate {
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }
}

impl Default for PromptGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Block until `is_idle()` holds, up to `limit`, polling every `poll`. Returns false on timeout.
pub fn wait_for_idle(is_idle: impl Fn() -> bool, limit: Duration, poll: Duration) -> bool {
    let started = Instant::now();
    loop {
        if is_idle() {
            return true;
        }
        if started.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(poll);
    }
}

/// Offer the replacement once per launch.
///
/// Does nothing when no replacement is pending or the gate was already taken. Otherwise a worker
/// thread named `model-update-prompt` waits up to `idle_wait` for `is_idle()`; on timeout it logs
/// `deferred_busy_timeout` and re-arms the gate (the prompt re-arms on the next launch attempt); on
/// idle it sets the one-shot download-request flag, logs `shown` and runs `show`. A thread that
/// cannot be spawned logs `model_update_prompt_spawn_error` and re-arms the gate.
pub fn maybe_prompt_replacement(
    gate: &'static PromptGate,
    is_idle: impl Fn() -> bool + Send + 'static,
    idle_wait: Duration,
    poll: Duration,
    show: impl FnOnce() + Send + 'static,
    sink: fn(&str, &str),
) {
    if !replacement_pending() || gate.0.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("model-update-prompt".into())
        .spawn(move || {
            if !wait_for_idle(is_idle, idle_wait, poll) {
                sink(EVENT, "deferred_busy_timeout");
                gate.0.store(false, Ordering::SeqCst);
                return;
            }
            // One-shot flag: a window created later reads it on mount
            // (`take_replacement_download_request`) and opens the same prompt.
            request_replacement_download();
            sink(EVENT, "shown");
            show();
        });
    if let Err(error) = spawned {
        sink(SPAWN_ERROR_EVENT, &error.to_string());
        gate.0.store(false, Ordering::SeqCst);
    }
}

/// "Later": nothing downloads now; the prompt re-arms on the next launch.
pub fn dismiss_model_update(sink: fn(&str, &str)) {
    let _ = take_replacement_download_request();
    sink(EVENT, "dismissed");
}
