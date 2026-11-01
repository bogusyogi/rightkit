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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_specs_contain_exactly_the_six_allowed_providers() {
        let ids: Vec<&str> = BUILT_IN_SPECS.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["claude", "codex", "grok", "cursor", "cline", "commandcode"],
            "built-in registry ceiling is exactly the six CLIs"
        );
    }

    #[test]
    fn no_built_in_spec_mentions_an_unlisted_provider() {
        for spec in BUILT_IN_SPECS.iter() {
            assert_ne!(
                spec.id.as_str().to_lowercase(),
                "unlisted-provider",
                "an unlisted provider must not appear in the built-in registry"
            );
            assert!(
                !spec
                    .display_name
                    .to_lowercase()
                    .contains("unlisted-provider"),
                "an unlisted provider must not appear in any display name"
            );
        }
    }

    #[test]
    fn spec_for_returns_none_for_unknown_provider() {
        let bogus = CliProviderId::new("not-a-real-cli");
        assert!(spec_for(&bogus).is_none());
    }

    #[test]
    fn commandcode_is_the_only_compatibility_isolation_target() {
        for spec in BUILT_IN_SPECS.iter() {
            if spec.id.as_str() == "commandcode" {
                assert_eq!(
                    spec.isolation_mode,
                    HarnessIsolationMode::NativeReadOnlyCompatibility
                );
            } else {
                assert_eq!(
                    spec.isolation_mode,
                    HarnessIsolationMode::ExclusiveHostGateway
                );
            }
        }
    }

    /// PR 6 acceptance: `seed_registry` installs both Claude and
    /// Codex adapters **and** the matching transports, so the
    /// executor can resolve either half of the open contract from
    /// the moment the daemon comes up.
    #[test]
    fn seed_registry_wires_claude_and_codex_adapters_and_transports() {
        let registry = crate::driver_host::cli_runtime::registry::CliAdapterRegistry::new();
        seed_registry(&registry).expect("seed must succeed");

        // Adapters.
        let claude_id = CliProviderId::new("claude");
        let codex_id = CliProviderId::new("codex");
        assert!(
            registry.adapter(&claude_id).is_some(),
            "Claude adapter must be registered after seed"
        );
        assert!(
            registry.adapter(&codex_id).is_some(),
            "Codex adapter must be registered after seed"
        );

        // Transports.
        assert!(
            registry
                .transport(CliTransportKind::ClaudeStreamJson)
                .is_some(),
            "Claude stream-JSON transport must be registered after seed"
        );
        assert!(
            registry
                .transport(CliTransportKind::CodexAppServer)
                .is_some(),
            "Codex app-server transport must be registered after seed"
        );

        // ACP now backs the qualified Grok, Cline, and Cursor adapters.
        assert!(registry.transport(CliTransportKind::Acp).is_some());
        assert!(registry
            .transport(CliTransportKind::ResumableNdjson)
            .is_some());
    }

    /// `seed_registry` is idempotent: calling it twice replaces
    /// the prior registrations and succeeds.
    #[test]
    fn seed_registry_is_idempotent() {
        let registry = crate::driver_host::cli_runtime::registry::CliAdapterRegistry::new();
        seed_registry(&registry).expect("first seed must succeed");
        seed_registry(&registry).expect("second seed must succeed");
        assert!(registry.adapter(&CliProviderId::new("claude")).is_some());
    }

    /// PR 8 acceptance: `seed_registry` registers the Grok adapter
    /// behind the open `CliProviderAdapter` contract so the
    /// executor can resolve a Grok provider the moment the daemon
    /// comes up. Grok shares the ACP transport with Cline and
    /// Cursor, so the transport registration is PR 7's job, not
    /// PR 8's. The adapter's harness requirements honor every
    /// Grok-specific restriction from §12.4 of the architecture
    /// plan: native subagent, native shell, native browser, and
    /// native memory writes are all forbidden in addition to the
    /// required managed-profile / single-MCP / scoped-lease /
    /// driver-sandbox block.
    #[test]
    fn seed_registry_wires_grok_adapter() {
        let registry = crate::driver_host::cli_runtime::registry::CliAdapterRegistry::new();
        seed_registry(&registry).expect("seed must succeed");

        let grok_id = CliProviderId::new("grok");
        let adapter = registry
            .adapter(&grok_id)
            .expect("Grok adapter must be registered after seed");
        assert_eq!(adapter.id().as_str(), "grok");
        assert_eq!(adapter.display_name(), "Grok");
        assert_eq!(
            adapter.transport_kind(),
            crate::driver_host::cli_runtime::ids::CliTransportKind::Acp
        );

        // Resume strategy is process-restart-with-resume-flag per
        // PR 8 acceptance. The registry seeded the trait object and
        // did not require the resume strategy at registration time,
        // but we re-assert it here as a regression check so future
        // refactors that re-route through the strategy surface do
        // not silently downgrade Grok.
        assert_eq!(
            crate::driver_host::cli_runtime::providers::grok::GrokAdapter::RESUME_STRATEGY,
            crate::driver_host::cli_runtime::ids::ResumeStrategy::ProcessRestartWithResumeFlag
        );

        // Harness requirements reflect every Grok-specific
        // restriction; the registry refuses to register an
        // inconsistent block, so this is also a regression check.
        let reqs = adapter.harness_requirements();
        assert!(reqs.requires_managed_profile);
        assert!(reqs.requires_one_mcp_server);
        assert!(reqs.requires_scoped_lease_token);
        assert!(reqs.requires_driver_sandbox);
        assert!(reqs.forbids_native_subagent);
        assert!(reqs.forbids_native_shell);
        assert!(reqs.forbids_native_browser);
        assert!(reqs.forbids_native_memory_writes);

        // Capabilities target `ExclusiveCodeRightGateway` per §12.4;
        // child delegation is denied so the Grok → Claude route
        // (PR 5) is the only delegation path.
        let caps = adapter.capabilities();
        assert_eq!(
            caps.isolation_mode,
            HarnessIsolationMode::ExclusiveHostGateway
        );
        assert!(!caps.supports_child_delegation);
        assert!(caps.supports_session_resume);
        assert!(caps.supports_native_account_login);
    }

    /// PR 9 acceptance: `seed_registry` registers the Cline adapter
    /// behind the open `CliProviderAdapter` contract so the
    /// executor can resolve a Cline provider the moment the daemon
    /// comes up. Cline shares the ACP transport with Grok and
    /// Cursor, so the transport registration is PR 7's job, not
    /// PR 9's.
    #[test]
    fn seed_registry_wires_cline_adapter() {
        let registry = crate::driver_host::cli_runtime::registry::CliAdapterRegistry::new();
        seed_registry(&registry).expect("seed must succeed");

        let cline_id = CliProviderId::new("cline");
        let adapter = registry
            .adapter(&cline_id)
            .expect("Cline adapter must be registered after seed");
        assert_eq!(adapter.id().as_str(), "cline");
        assert_eq!(
            adapter.transport_kind(),
            crate::driver_host::cli_runtime::ids::CliTransportKind::Acp
        );

        // Harness requirements reflect every Cline-specific
        // restriction; the registry refuses to register an
        // inconsistent block, so this is also a regression check.
        let reqs = adapter.harness_requirements();
        assert!(reqs.forbids_native_subagent);
        assert!(reqs.forbids_native_shell);
        assert!(reqs.forbids_native_browser);
        assert!(reqs.forbids_native_memory_writes);
    }

    /// PR 10 acceptance: `seed_registry` registers the Cursor
    /// adapter behind the open `CliProviderAdapter` contract so
    /// the executor can resolve a Cursor provider the moment the
    /// daemon comes up. Cursor shares the ACP transport with Grok
    /// and Cline, so the transport registration is PR 7's job,
    /// not PR 10's. The adapter's harness requirements honor
    /// every Cursor-specific restriction from §14.6 of the
    /// architecture plan: native subagent, native shell, native
    /// browser, and native memory writes are all forbidden, and
    /// the adapter's resume strategy is
    /// `ProtocolLoadById` per §14.7. Live ACP capability
    /// qualification is verified by the runner (the cursor-only
    /// MCP attach and session load probes); unsupported versions
    /// surface as `VersionUnsupported` with a repair message.
    #[test]
    fn seed_registry_wires_cursor_adapter() {
        let registry = crate::driver_host::cli_runtime::registry::CliAdapterRegistry::new();
        seed_registry(&registry).expect("seed must succeed");

        let cursor_id = CliProviderId::new("cursor");
        let adapter = registry
            .adapter(&cursor_id)
            .expect("Cursor adapter must be registered after seed");
        assert_eq!(adapter.id().as_str(), "cursor");
        assert_eq!(
            adapter.display_name(),
            crate::driver_host::cli_runtime::providers::cursor::DISPLAY_NAME
        );
        assert_eq!(
            adapter.transport_kind(),
            crate::driver_host::cli_runtime::ids::CliTransportKind::Acp
        );

        // Resume strategy is `ProtocolLoadById` per §14.7. The
        // architecture plan names the resume strategy as a hard
        // ceiling; drift here breaks the "same chat ID resumes"
        // guarantee that downstream consumers depend on.
        assert_eq!(
            crate::driver_host::cli_runtime::providers::cursor::CURSOR_RESUME_STRATEGY,
            crate::driver_host::cli_runtime::ids::ResumeStrategy::ProtocolLoadById
        );

        // Capabilities target `ExclusiveCodeRightGateway` per
        // §14.6; child delegation is denied so the only delegation
        // path is the open the host app MCP delegation surface.
        let caps = adapter.capabilities();
        assert_eq!(
            caps.isolation_mode,
            HarnessIsolationMode::ExclusiveHostGateway
        );
        assert!(!caps.supports_child_delegation);
        assert!(caps.supports_session_resume);
        assert!(caps.supports_native_account_login);

        // Harness requirements reflect every Cursor-specific
        // restriction from §14.6. The registry refuses to register
        // an inconsistent block, so this is also a regression
        // check.
        let reqs = adapter.harness_requirements();
        assert!(reqs.requires_managed_profile);
        assert!(reqs.requires_one_mcp_server);
        assert!(reqs.requires_scoped_lease_token);
        assert!(reqs.requires_driver_sandbox);
        assert!(reqs.forbids_native_subagent);
        assert!(reqs.forbids_native_shell);
        assert!(reqs.forbids_native_browser);
        assert!(reqs.forbids_native_memory_writes);

        // Qualification suite includes the two Cursor-only live-
        // capability probes (MCP attach and session/load) plus the
        // default positive and negative suites. The runner enforces
        // every probe; a Cursor binary that advertises `mcpServers`
        // but silently drops them fails the MCP attach probe and
        // the provider surfaces as `Unqualified`.
        let suite = adapter.qualification_suite();
        let ids: std::collections::BTreeSet<&str> = suite.iter().map(|p| p.id.as_str()).collect();
        assert!(
            ids.contains("pos.cursor_acp_mcp_attach"),
            "qualification suite must include pos.cursor_acp_mcp_attach"
        );
        assert!(
            ids.contains("pos.cursor_acp_session_load"),
            "qualification suite must include pos.cursor_acp_session_load"
        );
        assert!(
            ids.contains("neg.direct_native_file_write"),
            "qualification suite must include neg.direct_native_file_write"
        );
        assert!(
            ids.contains("neg.direct_native_shell"),
            "qualification suite must include neg.direct_native_shell"
        );
    }

    /// PR 11 acceptance: `seed_registry` registers the Command
    /// Code adapter **and** the ResumableNdjson transport it pairs
    /// with, so the executor can resolve either half of the open
    /// contract the moment the daemon comes up. The Command Code
    /// adapter is default-off (its activation gate returns
    /// `false`) so the registry's readiness gate refuses to launch
    /// it; this test only asserts that the trait objects are
    /// present so UI listings and dispatch resolution work.
    #[test]
    fn seed_registry_wires_commandcode_adapter_and_resumable_ndjson_transport() {
        let registry = crate::driver_host::cli_runtime::registry::CliAdapterRegistry::new();
        seed_registry(&registry).expect("seed must succeed");

        // Adapter.
        let commandcode_id = CliProviderId::new("commandcode");
        let adapter = registry
            .adapter(&commandcode_id)
            .expect("Command Code adapter must be registered after seed");
        assert_eq!(adapter.id().as_str(), "commandcode");
        assert_eq!(
            adapter.display_name(),
            crate::driver_host::cli_runtime::providers::commandcode::DISPLAY_NAME
        );
        assert_eq!(
            adapter.transport_kind(),
            crate::driver_host::cli_runtime::ids::CliTransportKind::ResumableNdjson
        );

        // Capabilities target `NativeReadOnlyCompatibility` per
        // the scope lock; native subagent / delegation is denied.
        let caps = adapter.capabilities();
        assert_eq!(
            caps.isolation_mode,
            HarnessIsolationMode::NativeReadOnlyCompatibility
        );
        assert!(!caps.supports_child_delegation);

        // Harness requirements reflect every Command-Code-specific
        // restriction; the registry refuses to register an
        // inconsistent block, so this is also a regression check.
        let reqs = adapter.harness_requirements();
        assert!(reqs.requires_managed_profile);
        assert!(reqs.requires_one_mcp_server);
        assert!(reqs.requires_scoped_lease_token);
        assert!(reqs.requires_driver_sandbox);
        assert!(reqs.forbids_native_subagent);
        assert!(reqs.forbids_native_shell);
        assert!(reqs.forbids_native_browser);
        assert!(reqs.forbids_native_memory_writes);

        // Activation gate is closed by default until activation
        // acceptance passes.
        assert!(
            !crate::driver_host::cli_runtime::providers::commandcode::CommandCodeAdapter::is_activation_passed(),
            "Command Code activation gate must stay closed (default-off)"
        );

        // Transport.
        let transport = registry
            .transport(CliTransportKind::ResumableNdjson)
            .expect("ResumableNdjson transport must be registered after seed");
        assert_eq!(transport.kind(), CliTransportKind::ResumableNdjson);
    }
}
