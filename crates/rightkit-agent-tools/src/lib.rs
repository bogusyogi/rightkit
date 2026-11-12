//! rightkit-agent-tools: one typed tool interface for agents over the Right Suite.
//!
//! * [`Tool`] / [`ToolSpec`]: a name, a JSON schema and an effect class.
//! * [`Toolbox`]: registry plus the one gate every call passes (hard exposure
//!   rules, per-caller allowlist, argument schema, the caller's admission hook,
//!   journal).
//! * Backends: `<app>.*` over each app's in-app control service
//!   (`rightkit-qa`'s `control` server on `rightkit-daemon`, including the app's own typed
//!   commands), `desktop.*` over `rightkit-automation`, `browser.*` over
//!   `rightkit-browser`, `speech.*` over the shared speech service
//!   (`rightkit-speech-client`, the first client).
//! * `right-ctl <app> <command>`: the CLI over the same toolbox.
//! * `ToolboxProvider` (feature `mcp-provider`): serves a [`Toolbox`] over MCP as a
//!   `rightkit_mcp::ToolProvider`; every call still passes the toolbox's gate.
//! * [`applicability`]: the M22 applicability record (`client | host | both | absent` plus real
//!   entrypoints) an app declares in `rightkit-mandatory.json`.
//!
//! HeardRight's rules are built in and not configurable: no dictation, text
//! delivery, pasteboard or history tool can be registered or called ([`exposure`]).

pub mod applicability;
pub mod backends;
pub mod exposure;
mod journal;
#[cfg(feature = "mcp-provider")]
mod mcp_provider;
mod peer;
mod spec;
mod toolbox;

pub use applicability::{AgentRole, ApplicabilityError, ApplicabilityRecord, APPLICABILITY_KEY};
pub use backends::control::{
    control_tools, serve_commands, AppProfile, CommandPolicy, CLIENT_APP, CONTROL_METHODS,
};
pub use backends::http::HttpEndpoint;
pub use journal::JsonlJournal;
#[cfg(feature = "mcp-provider")]
pub use mcp_provider::ToolboxProvider;
pub use peer::CallContextExt;
pub use spec::*;
pub use toolbox::{
    DeniedBy, Journal, MemoryJournal, Metrics, ToolAdmission, ToolEvent, ToolRequest, Toolbox,
    Verdict,
};

use rightkit_daemon::Suite;
use std::sync::Arc;

/// The caller identity `right-ctl` uses (and the service-side allowlist key).
pub const CTL_CALLER: &str = CLIENT_APP;

/// A toolbox for the CLI caller: `<app>.*` tools for each named app (built-in
/// [`AppProfile::for_app`] profile), `speech.*` for an app whose profile asks for it
/// ([`AppProfile::with_speech`]),
/// and allowlist patterns granting `right-ctl` exactly what each profile allows.
pub fn control_toolbox(suite: &Suite, apps: &[&str]) -> Result<Toolbox, ToolError> {
    let mut tb = Toolbox::new();
    for app in apps {
        add_app(&mut tb, suite, AppProfile::for_app(app))?;
    }
    Ok(tb)
}

/// The speech role [`add_app`] presents to the speech service: the one `right-ctl` has always used.
/// An app embedding its own toolbox passes its own role to [`add_app_as`].
#[cfg(feature = "speech")]
pub const CTL_SPEECH_ROLE: rightkit_speech_client::HostRole =
    rightkit_speech_client::HostRole::CutRight;

/// Register one app profile and grant [`CTL_CALLER`] its patterns. A profile with
/// [`AppProfile::with_speech`] also gets `speech.*`, presenting [`CTL_SPEECH_ROLE`].
#[cfg(feature = "speech")]
pub fn add_app(tb: &mut Toolbox, suite: &Suite, profile: AppProfile) -> Result<(), ToolError> {
    add_app_as(tb, suite, profile, CTL_SPEECH_ROLE)
}

/// Register one app profile and grant [`CTL_CALLER`] its patterns.
#[cfg(not(feature = "speech"))]
pub fn add_app(tb: &mut Toolbox, suite: &Suite, profile: AppProfile) -> Result<(), ToolError> {
    add_app_control(tb, suite, profile)
}

/// [`add_app`], with the speech service told the caller is `role` (used only when the profile has
/// [`AppProfile::with_speech`]).
#[cfg(feature = "speech")]
pub fn add_app_as(
    tb: &mut Toolbox,
    suite: &Suite,
    profile: AppProfile,
    role: rightkit_speech_client::HostRole,
) -> Result<(), ToolError> {
    let speech = profile.speech;
    add_app_control(tb, suite, profile)?;
    if speech {
        tb.register_all(backends::speech::speech_tools(suite, role))?;
        tb.allow(CTL_CALLER, ["speech.transcribe", "speech.capabilities"]);
    }
    Ok(())
}

fn add_app_control(tb: &mut Toolbox, suite: &Suite, profile: AppProfile) -> Result<(), ToolError> {
    if profile.dictation_enforced() {
        tb.protect_dictation(profile.app.clone());
    }
    tb.register_all(control_tools(suite, &profile))?;
    tb.allow(CTL_CALLER, profile.allow_patterns());
    Ok(())
}

/// Convenience: share a toolbox.
pub fn shared(tb: Toolbox) -> Arc<Toolbox> {
    Arc::new(tb)
}
