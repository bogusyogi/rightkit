//! The engine binary framework: one registry of named commands served two ways, as a headless
//! CLI ([`cli`]) and as the resident stdio sidecar a shell supervises ([`sidecar`]), with the wire
//! types they share ([`wire`]), the contract and TypeScript export ([`registry`]) and the
//! isolated-data-folder rule ([`data_folder`]), the process environment an engine's commands read
//! ([`mod@env`]) and installed-app discovery for an `apps` command ([`apps`]).
//!
//! ```no_run
//! use std::sync::Arc;
//! use rightkit_engine_host::{cli, registry::{CommandRegistry, CommandSpec}, sidecar, EngineSpec};
//!
//! let spec = EngineSpec::new("demo-engine", "1.0.0").with_env_prefix("DEMO");
//! let registry = CommandRegistry::new(Arc::new(|name: &str, input: &serde_json::Value, _dry_run: bool| {
//!     Ok::<_, rightkit_engine_host::EngineError>(serde_json::json!({ "name": name, "input": input }))
//! }))
//! .register(CommandSpec::new("job.status", "Status of a job").quick().positional(["job_id"]));
//! let args: Vec<String> = std::env::args().skip(1).collect();
//! if args.is_empty() || args[0] == "--stdio" {
//!     let _ = sidecar::run(sidecar::dispatcher(&spec, registry, Default::default()));
//! } else {
//!     std::process::exit(cli::run(&spec, &registry, args));
//! }
//! ```

pub mod apps;
pub mod caller;
pub mod cli;
pub mod data_folder;
pub mod doctor;
pub mod env;
#[cfg(feature = "project-cmd")]
pub mod project;
#[cfg(feature = "project-cmd")]
pub mod project_cmd;
pub mod registry;
pub mod sidecar;
pub mod wire;

pub use registry::{CommandRegistry, CommandSpec, Handler};
pub use wire::{EngineError, EngineEvent, EngineRequest};

/// Who the engine is: name, version, contract version, usage text and environment prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSpec {
    /// Engine binary name (`demo-engine`).
    pub name: String,
    /// Engine version.
    pub version: String,
    /// Version of the CLI/sidecar contract; breaking changes to commands or their JSON bump it.
    pub contract_version: u32,
    /// Usage text printed by `--help` and after a usage error.
    pub usage: String,
    /// Prefix of the engine's environment variables (`<PREFIX>_APP_DATA`, `<PREFIX>_QA`).
    pub env_prefix: String,
}

impl EngineSpec {
    /// A spec with contract version 1, a one-line usage and the `RIGHTKIT` prefix.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            usage: format!("usage: {name} [--stdio] | {name} <command words> [--input <json>|-]"),
            name,
            version: version.into(),
            contract_version: 1,
            env_prefix: "RIGHTKIT".into(),
        }
    }

    /// Replace the usage text.
    pub fn with_usage(mut self, usage: impl Into<String>) -> Self {
        self.usage = usage.into();
        self
    }

    /// Replace the environment prefix.
    pub fn with_env_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.env_prefix = prefix.into();
        self
    }

    /// Replace the contract version.
    pub fn with_contract_version(mut self, version: u32) -> Self {
        self.contract_version = version;
        self
    }

    /// `<PREFIX>_<suffix>`.
    pub fn env_name(&self, suffix: &str) -> String {
        format!("{}_{suffix}", self.env_prefix)
    }
}
