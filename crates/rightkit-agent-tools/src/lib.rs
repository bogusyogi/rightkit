//! rightkit-agent-tools: one typed tool interface for agents over the Right Suite.
//!
//! * [`Tool`] / [`ToolSpec`]: a name, a JSON schema and an effect class.
//! * [`Toolbox`]: registry plus the one gate every call passes (hard exposure
//!   rules, per-caller allowlist, argument schema, the caller's admission hook,
//!   journal).
//! * Backends: `<app>.*` over each app's in-app control service
//!   (`rightkit-control` on `rightkit-service`, including the app's own typed
//!   commands), `desktop.*` over `rightkit-automation`, `browser.*` over
//!   `rightkit-browser`, `speech.*` over the shared speech service
//!   (`rightkit-speech-client`, the first client).
//! * `right-ctl <app> <command>`: the CLI over the same toolbox.
//!
//! HeardRight's rules are built in and not configurable: no dictation, text
//! delivery, pasteboard or history tool can be registered or called ([`exposure`]).

pub mod backends;
pub mod exposure;
mod journal;
mod spec;
mod toolbox;

pub use backends::control::{control_tools, AppProfile, CommandPolicy, CLIENT_APP, CONTROL_METHODS};
pub use journal::JsonlJournal;
pub use spec::*;
pub use toolbox::{DeniedBy, Journal, MemoryJournal, Metrics, ToolAdmission, ToolEvent, ToolRequest, Toolbox, Verdict};

#[cfg(feature = "mcp")]
pub mod mcp;

use rightkit_service::Suite;
use std::sync::Arc;

/// The caller identity `right-ctl` uses (and the service-side allowlist key).
pub const CTL_CALLER: &str = CLIENT_APP;

/// A toolbox for the CLI caller: `<app>.*` tools for each named app (built-in
/// [`AppProfile::for_app`] profile), `speech.*` when HeardRight is among them,
/// and allowlist patterns granting `right-ctl` exactly what each profile allows.
pub fn control_toolbox(suite: &Suite, apps: &[&str]) -> Result<Toolbox, ToolError> {
    let mut tb = Toolbox::new();
    for app in apps {
        add_app(&mut tb, suite, AppProfile::for_app(app))?;
    }
    Ok(tb)
}

/// Register one app profile and grant [`CTL_CALLER`] its patterns.
pub fn add_app(tb: &mut Toolbox, suite: &Suite, profile: AppProfile) -> Result<(), ToolError> {
    tb.register_all(control_tools(suite, &profile))?;
    tb.allow(CTL_CALLER, profile.allow_patterns());
    #[cfg(feature = "speech")]
    if profile.app == "heardright" {
        tb.register_all(backends::speech::speech_tools(suite))?;
        tb.allow(CTL_CALLER, ["speech.transcribe", "speech.capabilities"]);
    }
    Ok(())
}

/// Convenience: share a toolbox.
pub fn shared(tb: Toolbox) -> Arc<Toolbox> {
    Arc::new(tb)
}
