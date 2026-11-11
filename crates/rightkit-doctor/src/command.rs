//! The `doctor` command for `rightkit-engine-host` registries.
//!
//! An app implements [`DoctorHost`] (its identity, capabilities and checks) and wraps its
//! command handler with [`wrap`]; the registry gets the [`spec`] through [`register`].

use std::sync::Arc;

use rightkit_engine_host::{CommandRegistry, CommandSpec, EngineError, Handler};
use serde_json::{Map, Value};

use crate::report::{EngineReport, Identity};
use crate::row::Row;

/// The command name.
pub const NAME: &str = "doctor";

/// What the app supplies to the doctor.
pub trait DoctorHost: Send + Sync {
    /// Who the engine is.
    fn identity(&self) -> Identity;

    /// Capabilities beyond the built-in `doctor`, as `(name, version)`; usually every other
    /// command the engine serves.
    fn capabilities(&self) -> Vec<(String, u32)> {
        Vec::new()
    }

    /// The live checks. `input` is the command's JSON input (`{}` when none).
    fn checks(&self, input: &Value) -> Vec<Row>;

    /// The data folder in use, if the engine has one.
    fn data_dir(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// The resource folder; defaults to the running executable's, by the engine rule
    /// ([`rightkit_engine_host::env::resource_dir`]).
    fn resource_dir(&self) -> Option<std::path::PathBuf> {
        std::env::current_exe()
            .ok()
            .and_then(|exe| rightkit_engine_host::env::resource_dir(std::env::consts::OS, &exe))
    }

    /// The app's own top-level sections of the report.
    fn extra(&self, _input: &Value) -> Map<String, Value> {
        Map::new()
    }
}

/// The registry entry: a quick read-only command.
pub fn spec() -> CommandSpec {
    CommandSpec::new(
        NAME,
        "Report engine readiness: identity, capabilities and live checks",
    )
    .quick()
}

/// Register [`spec`] on `registry`.
pub fn register(registry: CommandRegistry) -> CommandRegistry {
    registry.register(spec())
}

/// Run the doctor and return its report.
pub fn run(host: &dyn DoctorHost, input: &Value, dry_run: bool) -> Value {
    let identity = host.identity();
    let mut capabilities = vec![(NAME.to_string(), 1)];
    capabilities.extend(
        host.capabilities()
            .into_iter()
            .filter(|(name, _)| name != NAME),
    );
    let data_dir = host.data_dir();
    let resource_dir = host.resource_dir();
    EngineReport {
        identity: &identity,
        capabilities,
        rows: host.checks(input),
        data_dir: data_dir.as_deref(),
        resource_dir: resource_dir.as_deref(),
        dry_run,
        extra: host.extra(input),
    }
    .build()
}

/// A handler that answers [`NAME`] from `host` and forwards every other command to `inner`.
pub fn wrap(host: Arc<dyn DoctorHost>, inner: Handler) -> Handler {
    Arc::new(
        move |name: &str, input: &Value, dry_run: bool| -> Result<Value, EngineError> {
            if name == NAME {
                Ok(run(host.as_ref(), input, dry_run))
            } else {
                inner(name, input, dry_run)
            }
        },
    )
}
