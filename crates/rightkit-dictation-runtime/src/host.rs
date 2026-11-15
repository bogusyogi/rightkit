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
