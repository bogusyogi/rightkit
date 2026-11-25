//! The renderer events that announce a model update prompt: one event carrying the offered model
//! and one asking the hub to open the download.
//!
//! Which route the hub opens, how an event reaches the renderer and how a failure is logged are the
//! application's: the event names, tab, model label and delivery arrive in
//! `ModelUpdatePromptEvents` and the `emit` and `log` arguments.

use serde_json::{json, Value};

/// What the application names in a model update prompt.
pub struct ModelUpdatePromptEvents<'a> {
    /// Event announcing the offered model.
    pub available_event: &'a str,
    /// Event asking the hub to navigate to the download.
    pub navigate_event: &'a str,
    /// Hub tab the navigate event opens.
    pub tab: &'a str,
    /// Backend id of the offered model (also the navigate event's `download_model`).
    pub backend: &'a str,
    /// Display label of the offered model.
    pub model_label: &'a str,
    /// Size of the download in bytes, when known (serialized as a number, or `null`).
    pub download_bytes: Option<u64>,
    /// Why the model is offered.
    pub reason: &'a str,
}

/// Emit the available event, then the navigate event. A failed emit is logged and the other still
/// goes out. `log` receives an event name and a message.
pub fn emit_model_update_prompt(
    events: &ModelUpdatePromptEvents<'_>,
    emit: impl Fn(&str, &Value) -> Result<(), String>,
    log: impl Fn(&str, &str),
) {
    let payload = json!({
        "backend": events.backend,
        "model_label": events.model_label,
        "download_bytes": events.download_bytes,
        "reason": events.reason,
    });
    if let Err(error) = emit(events.available_event, &payload) {
        log("model_update_prompt_emit_error", &error);
    }
    if let Err(error) = emit(
        events.navigate_event,
        &json!({ "tab": events.tab, "download_model": events.backend }),
    ) {
        log("model_update_prompt_navigate_error", &error);
    }
}
