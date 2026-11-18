use serde::{Deserialize, Serialize};

/// Pill control-surface states. The pill is an icon/timer surface plus short transient status
/// text for completed actions. `Error.label` is rendered inline in the error capsule.
///
/// `Voice` is a separate high-frequency amplitude update that drives the recording bars WITHOUT
/// changing the rendered mode (so it does not reset the timer).
///
/// Wire form (serde, internally tagged): `{ "kind": "<snake_case variant>", ..fields }`, for
/// example `{"kind":"voice","level":0.5}` or `{"kind":"status","label":"Saved"}`. The exact
/// JSON of every variant is pinned by the fixtures in `fixtures/pill_event/` and the round-trip
/// tests in `tests/pill_event_wire.rs`: webview renderers and the macOS bridge parse it, so a
/// change to a tag, a rename or a field name is a breaking change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PillEvent {
    Hide,
    /// Resting sliver, emitted at rest and at startup: the pill is an always-on overlay. Hovering
    /// the sliver is the start affordance.
    Idle,
    /// Immediate acknowledgement while the audio stream is resuming. A renderer may promote this
    /// to `Recording` only after the first mic samples.
    Starting,
    /// Recording: reactive bars and a timer. The renderer ticks the timer locally.
    Recording,
    /// Mic amplitude `0.0..=1.0` for the bars. Updates the bars only; does not change the mode.
    Voice {
        level: f32,
    },
    /// Processing animation. `cancellable = false` for final-only ASR after stop, so the
    /// renderer shows no controls on hover.
    Processing {
        cancellable: bool,
    },
    /// Legacy generic completion event. Renderers show the host's "done" status text; new
    /// producers should prefer `Status`.
    Success,
    /// Short transient non-interactive status, e.g. `Pasted`, `Sent`, `Saved`, `Copied`.
    Status {
        label: String,
    },
    /// Pulsing warning. `label` is rendered inline and exposed as its accessible name.
    Error {
        label: String,
    },
}
