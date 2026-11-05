//! host_plugins — local-folder plugin loader and runtime (no marketplace).
//!
//! This crate provides a plugin system with:
//! - Manifest loading and serialization (JSON)
//! - Runtime plugin discovery and state management (enable/disable)
//! - Atomic local install (stage, swap, no retained previous version)

pub mod error;
pub mod execution_trust;
pub mod loader;
pub mod manifest;
pub mod marketplace;
pub mod provider_extension;
pub mod runtime;
pub mod scope;

pub use error::PluginError;
pub use execution_trust::{
    classify_plugin_policy, extension_trust_catalog, ExtensionIsolationGuarantee,
    ExtensionTrustClass, ExtensionTrustDiagnostic,
};
pub use loader::{validate_plugin_id, InstalledPlugin, PluginLoader};
pub use manifest::{
    PluginCommandMetadata, PluginDataAccess, PluginIsolationMode, PluginManifest,
    PluginMiddlewareCondition, PluginMiddlewareContribution, PluginPromptContribution,
    PluginSandboxPolicy, PluginToolMetadata, PluginUiContribution, PluginUiSlot,
};
pub use marketplace::{CatalogSort, Marketplace, MarketplaceEntry};
pub use runtime::{
    check_plugin_active_for_session, new_shared_activations,
    run_plugin_entry_with_policy_and_adapter, PluginInvocation, PluginInvocationResult,
    PluginRegistrationScope, SessionPluginActivations, SharedSessionActivations,
};
pub use scope::{PluginScope, ScopedPlugins};
