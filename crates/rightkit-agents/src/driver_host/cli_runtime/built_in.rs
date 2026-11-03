//! Seed list of the six allowed CLI providers.
//!
//! PR 1 of the architecture plan. PR 1 only registers descriptors
//! for the six providers so the registry can be queried for them;
//! the actual [`CliProviderAdapter`] trait objects for Claude and
//! Codex are migrated from the existing bridge in PR 6, and the
//! new providers (Grok, Cursor, Cline, Command Code) are wired in
//! by PRs 8–11.
//!
//! [`seed_registry`] installs the Claude and Codex adapters
//! **and** their matching transports (PR 6), the Cline adapter
//! (PR 9), the Grok adapter (PR 8), the Cursor adapter (PR 10),
//! and the Command Code adapter plus its matching ResumableNdjson
//! transport (PR 11). The Command Code adapter is default-off —
//! the registry's readiness gate refuses to launch it until the
//! activation acceptance criteria pass — but the trait objects
//! are registered so dispatch resolution works. Grok and Cursor
//! share the ACP transport with Cline; the transport registration
//! is PR 7's job.
//!
//! [`seed_registry`] installs both halves of the open contract
//! for every wired provider so the daemon can resolve them the
//! moment it comes up.

use std::sync::{Arc, LazyLock};

use serde::{Deserialize, Serialize};

use crate::driver_host::cli_runtime::adapter::CliTransport;
use crate::driver_host::cli_runtime::ids::{CliProviderId, CliTransportKind, HarnessIsolationMode};

/// A static descriptor for one built-in provider. Carries the
/// provider identity, the transport it uses, and the display label
/// shown in the picker. Real adapter construction (which produces a
/// `Box<dyn CliProviderAdapter>`) lands with the per-provider PRs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltInProviderSpec {
    pub id: CliProviderId,
    pub display_name: &'static str,
    pub transport: CliTransportKind,
    pub isolation_mode: HarnessIsolationMode,
}

/// The canonical list. The order is the picker default order and
/// the implementation sequence (see the architecture plan,
/// section 16). Adding a provider means appending an entry here
/// **and** shipping the matching adapter in its own PR.
pub static BUILT_IN_SPECS: LazyLock<Vec<BuiltInProviderSpec>> = LazyLock::new(|| {
    vec![
        BuiltInProviderSpec {
            id: CliProviderId::new("claude"),
            display_name: "Claude Code",
            transport: CliTransportKind::ClaudeStreamJson,
            isolation_mode: HarnessIsolationMode::ExclusiveHostGateway,
        },
        BuiltInProviderSpec {
            id: CliProviderId::new("codex"),
            display_name: "Codex",
            transport: CliTransportKind::CodexAppServer,
            isolation_mode: HarnessIsolationMode::ExclusiveHostGateway,
        },
        BuiltInProviderSpec {
            id: CliProviderId::new("grok"),
            display_name: "Grok Build",
            transport: CliTransportKind::Acp,
            isolation_mode: HarnessIsolationMode::ExclusiveHostGateway,
        },
        BuiltInProviderSpec {
            id: CliProviderId::new("cursor"),
            display_name: "Cursor",
            transport: CliTransportKind::Acp,
            isolation_mode: HarnessIsolationMode::ExclusiveHostGateway,
        },
        BuiltInProviderSpec {
            id: CliProviderId::new("cline"),
            display_name: "Cline",
            transport: CliTransportKind::Acp,
            isolation_mode: HarnessIsolationMode::ExclusiveHostGateway,
        },
        BuiltInProviderSpec {
            id: CliProviderId::new("commandcode"),
            display_name: "Command Code",
            transport: CliTransportKind::ResumableNdjson,
            isolation_mode: HarnessIsolationMode::NativeReadOnlyCompatibility,
        },
    ]
});

/// Iterate the canonical six in order. Returns an iterator over
/// borrowed `BuiltInProviderSpec` entries.
pub fn iter() -> impl Iterator<Item = &'static BuiltInProviderSpec> {
    BUILT_IN_SPECS.iter()
}

/// Look up a spec by id. Returns `None` for any provider that is
/// not in the built-in table. Use this rather than scanning
/// [`BUILT_IN_SPECS`] from caller code.
pub fn spec_for(id: &CliProviderId) -> Option<&'static BuiltInProviderSpec> {
    BUILT_IN_SPECS.iter().find(|spec| &spec.id == id)
}

/// Seed a registry with the built-in descriptors and the
/// built-in adapter / transport pairs that PR 6 wires. PR 6
/// registers the Claude and Codex adapters and their matching
/// transports; PR 9 adds the Cline adapter; PR 11 adds the
/// Command Code adapter **and** the ResumableNdjson transport it
/// pairs with. The remaining two providers (Grok, Cursor) share
/// the ACP transport that lands with PR 7.
///
/// `register` errors propagate immediately; the daemon must not
/// come up with a partial seed list. The function is idempotent
/// against the provider ids — calling it twice replaces prior
/// registrations, which is what we want for daemon hot-reload.
pub fn seed_registry(
    registry: &crate::driver_host::cli_runtime::registry::CliAdapterRegistry,
) -> Result<(), SeedError> {
    for spec in BUILT_IN_SPECS.iter() {
        if spec.id.as_str().is_empty() {
            return Err(SeedError::EmptyProviderId);
        }
        if spec.display_name.is_empty() {
            return Err(SeedError::EmptyDisplayName(spec.id.clone()));
        }
        let _ = registry.adapter(&spec.id);
    }

    // PR 6: install Claude + Codex adapters and the matching
    // transports. The adapter construction is unconditional; the
    // readiness gate (PR 3) is what actually blocks a launch
    // before attestation succeeds.
    let claude: Arc<dyn crate::driver_host::cli_runtime::adapter::CliProviderAdapter> =
        Arc::new(crate::driver_host::cli_runtime::providers::claude::ClaudeAdapter::new());
    registry
        .register_adapter(claude)
        .map_err(seed_error_from_registry)?;

    let codex: Arc<dyn crate::driver_host::cli_runtime::adapter::CliProviderAdapter> =
        Arc::new(crate::driver_host::cli_runtime::providers::codex::CodexAdapter::new());
    registry
        .register_adapter(codex)
        .map_err(seed_error_from_registry)?;

    let claude_transport: Arc<dyn CliTransport> = Arc::new(
        crate::driver_host::cli_runtime::transports::claude_stream_json::ClaudeStreamJsonTransport::new(),
    );
    registry
        .register_transport(claude_transport)
        .map_err(seed_error_from_registry)?;

    let codex_transport: Arc<dyn CliTransport> = Arc::new(
        crate::driver_host::cli_runtime::transports::codex_app_server::CodexAppServerTransport::new(
        ),
    );
    registry
        .register_transport(codex_transport)
        .map_err(seed_error_from_registry)?;

    let acp_transport: Arc<dyn CliTransport> =
        Arc::new(crate::driver_host::cli_runtime::transports::acp::AcpTransport::new());
    registry
        .register_transport(acp_transport)
        .map_err(seed_error_from_registry)?;

    // PR 9: install the Cline adapter so the registry can resolve
    // the Cline provider the moment the daemon comes up. The Cline
    // transport is shared with Grok and Cursor.
    let cline: Arc<dyn crate::driver_host::cli_runtime::adapter::CliProviderAdapter> =
        Arc::new(crate::driver_host::cli_runtime::providers::cline::ClineAdapter::new());
    registry
        .register_adapter(cline)
        .map_err(seed_error_from_registry)?;

    // PR 8: install the Grok adapter so the registry can resolve
    // the Grok provider the moment the daemon comes up. The Grok
    // transport (ACP) lands with PR 7; the adapter is the open-
    // contract half that ships now. Grok shares the ACP transport
    // with Cline and Cursor, so no transport is registered here.
    // The Grok adapter enforces strict profile isolation
    // (`--profile-dir <managed_home>`), process-restart resume via
    // `--resume <vendor_session_id>`, and disables every provider-
    // native orchestration surface (leader, team, native tools,
    // skills) so the only capability surface the model can reach is
    // the the host app MCP server.
    let grok: Arc<dyn crate::driver_host::cli_runtime::adapter::CliProviderAdapter> =
        Arc::new(crate::driver_host::cli_runtime::providers::grok::GrokAdapter::new());
    registry
        .register_adapter(grok)
        .map_err(seed_error_from_registry)?;

    // PR 10: install the Cursor adapter so the registry can resolve
    // the Cursor provider the moment the daemon comes up. The Cursor
    // adapter drives the ACP transport (`agent acp`); browser-account
    // authentication only, never `--api-key` / `CURSOR_API_KEY`.
    // The qualification runner verifies live ACP capabilities (MCP
    // attach and `session/load`) before the registry allows a
    // launch; unsupported Cursor versions surface as
    // `CliProviderEligibility::VersionUnsupported` with a repair
    // message. Cursor shares the ACP transport with Grok and
    // Cline, so no transport is registered here.
    let cursor: Arc<dyn crate::driver_host::cli_runtime::adapter::CliProviderAdapter> =
        Arc::new(crate::driver_host::cli_runtime::providers::cursor::CursorAdapter::new());
    registry
        .register_adapter(cursor)
        .map_err(seed_error_from_registry)?;

    // PR 11: install the Command Code adapter **and** its matching
    // ResumableNdjson transport. The adapter is default-off until
    // its activation gate (`is_activation_passed`) flips; the
    // readiness gate refuses to launch it regardless. We install
    // both halves of the open contract so the dispatch layer can
    // resolve the provider (e.g. for UI listings) without re-
    // deriving the provider id.
    let commandcode: Arc<dyn crate::driver_host::cli_runtime::adapter::CliProviderAdapter> =
        Arc::new(
            crate::driver_host::cli_runtime::providers::commandcode::CommandCodeAdapter::new(),
        );
    registry
        .register_adapter(commandcode)
        .map_err(seed_error_from_registry)?;

    let commandcode_transport: Arc<dyn CliTransport> = Arc::new(
        crate::driver_host::cli_runtime::transports::resumable_ndjson::ResumableNdjsonTransport::new(Box::new(
            crate::driver_host::cli_runtime::transports::resumable_ndjson::CommandCodeDialect::new(),
        )),
    );
    registry
        .register_transport(commandcode_transport)
        .map_err(seed_error_from_registry)?;

    Ok(())
}

/// Translate a [`crate::driver_host::cli_runtime::registry::RegistryError`] into
/// the seed error type so callers do not have to know both.
fn seed_error_from_registry(
    error: crate::driver_host::cli_runtime::registry::RegistryError,
) -> SeedError {
    match error {
        crate::driver_host::cli_runtime::registry::RegistryError::InconsistentRequirements(msg) => {
            SeedError::InconsistentRequirements(msg)
        }
        crate::driver_host::cli_runtime::registry::RegistryError::LockPoisoned => {
            SeedError::RegistryLockPoisoned
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedError {
    EmptyProviderId,
    EmptyDisplayName(CliProviderId),
    InconsistentRequirements(String),
    RegistryLockPoisoned,
}

impl std::fmt::Display for SeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SeedError::EmptyProviderId => f.write_str("a built-in spec has an empty provider id"),
            SeedError::EmptyDisplayName(id) => {
                write!(f, "built-in spec {id} has an empty display name")
            }
            SeedError::InconsistentRequirements(msg) => {
                write!(f, "built-in adapter has inconsistent requirements: {msg}")
            }
            SeedError::RegistryLockPoisoned => f.write_str("registry lock poisoned"),
        }
    }
}

impl std::error::Error for SeedError {}
