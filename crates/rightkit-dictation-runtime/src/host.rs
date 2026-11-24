//! Names the engine process puts on the wire, on stderr and in its messages, and the options a
//! host that shipped other behaviour turns on. Everything is configured once per process, before
//! `entry::run`; each setter returns `false` when a value was already installed (the
//! first one stays). The defaults are what the crate always did.

use std::sync::OnceLock;

/// The engine's names and messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineIdentity {
    /// Engine name in the `hello` envelope, the ready log line, the fatal IPC message and the
    /// diagnostics JSON. Default `rightkit-engine`.
    pub engine_name: &'static str,
    /// Build identity (`<name>/<version>`) in the `hello` envelope, the engine info and the speech
    /// service response. Default `rightkit-engine/<crate version>`.
    pub engine_version: &'static str,
    /// Product name in "Bundled ONNX runtime is missing; repair the `<name>` installation."
    /// Default `app`.
    pub product_name: &'static str,
    /// Owner in "(`<owner>` microphone permission may still be denied in Windows Settings)".
    /// Default `the app's`.
    pub microphone_permission_owner: &'static str,
    /// Prefix of the one-line JSON wake diagnostics written to stderr. Default
    /// `RIGHTKIT_WAKE_DIAGNOSTICS_JSON=`.
    pub wake_diagnostics_prefix: &'static str,
    /// Name of an environment variable whose presence turns on calibration diagnostics in the
    /// `calibrate` one-shot, as the `--collect-diagnostics` argument does. Default none.
    pub calibration_diagnostics_env: Option<&'static str>,
}

impl EngineIdentity {
    /// The names of earlier releases of this crate.
    pub const DEFAULT: Self = Self {
        engine_name: "rightkit-engine",
        engine_version: concat!("rightkit-engine/", env!("CARGO_PKG_VERSION")),
        product_name: "app",
        microphone_permission_owner: "the app's",
        wake_diagnostics_prefix: "RIGHTKIT_WAKE_DIAGNOSTICS_JSON=",
        calibration_diagnostics_env: None,
    };
}

static IDENTITY: OnceLock<EngineIdentity> = OnceLock::new();

/// Install the engine identity once. Returns `false` when one was already installed.
pub fn configure_identity(identity: EngineIdentity) -> bool {
    IDENTITY.set(identity).is_ok()
}

/// The engine identity in force.
pub fn identity() -> &'static EngineIdentity {
    IDENTITY.get().unwrap_or(&EngineIdentity::DEFAULT)
}

/// The installed command line (`transcribe`, `help`, `--version`): its names and messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliIdentity {
    /// Program name in the usage text (`<program> transcribe ...`).
    pub program: &'static str,
    /// Product name in the usage text and messages (the speech service the file is
    /// sent to, "switch `<product>` to Parakeet", "open `<product>` once ...").
    pub product: &'static str,
    /// Prefix of the stderr lines (`<prefix>: <message>`).
    pub message_prefix: &'static str,
}

static CLI: OnceLock<CliIdentity> = OnceLock::new();

/// Turn on the installed command line and name it. Without this call the engine has no command
/// line and every argument belongs to its other roles. Returns `false` when already installed.
pub fn configure_cli(identity: CliIdentity) -> bool {
    CLI.set(identity).is_ok()
}

/// The command line identity, when the host turned the command line on.
pub fn cli_identity() -> Option<&'static CliIdentity> {
    CLI.get()
}

/// Names in the speech service's log lines, for a host whose log consumers expect other names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceLogNames {
    /// `event` of the warning for a peer the OS would not identify. Default
    /// `service_peer_unidentified`.
    pub unidentified_peer_event: &'static str,
    /// Put the `reason` field right after `event` in that warning (default: after the peer's
    /// `pid`, `image` and `kind`).
    pub unidentified_peer_reason_first: bool,
    /// Name the refusal in the `service_peer_untrusted` warning `reason` (default: `error`).
    pub untrusted_peer_field_is_reason: bool,
}

impl ServiceLogNames {
    /// The names of earlier releases of this crate.
    pub const DEFAULT: Self = Self {
        unidentified_peer_event: "service_peer_unidentified",
        unidentified_peer_reason_first: false,
        untrusted_peer_field_is_reason: false,
    };
}

static SERVICE_LOG_NAMES: OnceLock<ServiceLogNames> = OnceLock::new();

/// Install the speech service's log names once. Returns `false` when already installed.
pub fn configure_service_log_names(names: ServiceLogNames) -> bool {
    SERVICE_LOG_NAMES.set(names).is_ok()
}

/// The speech service's log names in force.
pub fn service_log_names() -> &'static ServiceLogNames {
    SERVICE_LOG_NAMES.get().unwrap_or(&ServiceLogNames::DEFAULT)
}

static PULSE_HANDOFF: OnceLock<rightkit_dictation_command_host::pulse_handoff::HandoffIdentity> =
    OnceLock::new();

/// Turn on the hand-off to Pulse (`docs/contracts/voice.md`, decision 5): a recording that began from the
/// wake word and that no command matched is sent to the Pulse command line as a task instead of being typed.
/// Call once per process, before `entry::run`; returns `false` when already installed. A host that makes no
/// call keeps today's behaviour: such a recording is dictated.
pub fn configure_pulse_handoff(
    identity: rightkit_dictation_command_host::pulse_handoff::HandoffIdentity,
) -> bool {
    PULSE_HANDOFF.set(identity).is_ok()
}

/// The hand-off identity, when the host turned the hand-off on.
pub fn pulse_handoff(
) -> Option<&'static rightkit_dictation_command_host::pulse_handoff::HandoffIdentity> {
    PULSE_HANDOFF.get()
}
