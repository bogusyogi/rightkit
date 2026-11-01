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
pub mod provider_extension;
pub mod runtime;

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
pub use runtime::{
    check_plugin_active_for_session, new_shared_activations,
    run_plugin_entry_with_policy_and_adapter, PluginInvocation, PluginInvocationResult,
    PluginRegistrationScope, SessionPluginActivations, SharedSessionActivations,
};
#[cfg(test)]
pub use runtime::{run_plugin_entry, run_plugin_entry_with_policy};

#[cfg(test)]
mod runtime_tests {
    use super::*;

    #[test]
    fn plugin_registration_scope_is_stable_and_distinct() {
        let alpha = PluginRegistrationScope::new("alpha");
        let same_alpha = PluginRegistrationScope::new("alpha");
        let beta = PluginRegistrationScope::new("beta");
        assert_eq!(alpha, same_alpha);
        assert_ne!(alpha, beta);
        assert_eq!(alpha.as_str(), "plugin:alpha");
    }

    #[tokio::test]
    async fn subprocess_plugin_entry_receives_json_and_returns_json_result() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("plugin.py");
        std::fs::write(
            &script,
            r#"
import json, sys
payload = json.load(sys.stdin)
print(json.dumps({
    "content": f"{payload['kind']}:{payload['name']}:{payload['args']['message']}:{payload['context']['workspaceRoot']}",
    "isError": False
}))
"#,
        )
        .unwrap();

        let result = run_plugin_entry(
            "plugin.py",
            dir.path(),
            PluginInvocation {
                kind: "tool".into(),
                name: "fixture_tool".into(),
                args: serde_json::json!({ "message": "hello" }),
                session_id: "s1".into(),
                workspace_root: dir.path().to_path_buf(),
            },
        )
        .await
        .unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("tool:fixture_tool:hello:"));
    }

    #[tokio::test]
    async fn subprocess_plugin_entry_nonzero_exit_is_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("plugin.py");
        std::fs::write(
            &script,
            r#"
import sys
print("bad plugin", file=sys.stderr)
sys.exit(7)
"#,
        )
        .unwrap();

        let result = run_plugin_entry(
            "plugin.py",
            dir.path(),
            PluginInvocation {
                kind: "tool".into(),
                name: "fixture_tool".into(),
                args: serde_json::json!({}),
                session_id: "s1".into(),
                workspace_root: dir.path().to_path_buf(),
            },
        )
        .await
        .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("bad plugin"));
    }

    #[tokio::test]
    async fn subprocess_plugin_entry_uses_restricted_default() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("plugin.py");
        std::fs::write(&script, "print('{}')").unwrap();

        let err = run_plugin_entry(
            &script,
            dir.path(),
            PluginInvocation {
                kind: "tool".into(),
                name: "fixture_tool".into(),
                args: serde_json::json!({}),
                session_id: "s1".into(),
                workspace_root: dir.path().to_path_buf(),
            },
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("relative to the plugin root"));
    }

    #[tokio::test]
    async fn subprocess_plugin_policy_rejects_absolute_entry_by_default() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("plugin.py");
        std::fs::write(&script, "print('{}')").unwrap();

        let err = run_plugin_entry_with_policy(
            &script,
            dir.path(),
            PluginInvocation {
                kind: "tool".into(),
                name: "fixture_tool".into(),
                args: serde_json::json!({}),
                session_id: "s1".into(),
                workspace_root: dir.path().to_path_buf(),
            },
            PluginSandboxPolicy::default(),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("relative to the plugin root"));
    }

    #[tokio::test]
    async fn subprocess_plugin_policy_rejects_entry_outside_root() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin_root = dir.path().join("plugin");
        std::fs::create_dir_all(&plugin_root).unwrap();
        std::fs::write(dir.path().join("outside.py"), "print('{}')").unwrap();

        let err = run_plugin_entry_with_policy(
            "../outside.py",
            &plugin_root,
            PluginInvocation {
                kind: "tool".into(),
                name: "fixture_tool".into(),
                args: serde_json::json!({}),
                session_id: "s1".into(),
                workspace_root: dir.path().to_path_buf(),
            },
            PluginSandboxPolicy::default(),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("outside the plugin root"));
    }

    #[tokio::test]
    async fn subprocess_plugin_policy_can_scrub_environment() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("plugin.py");
        std::fs::write(
            &script,
            r#"
import json, os
print(json.dumps({
    "content": "|".join([
        os.environ.get("RIGHTKIT_PLUGIN_ENV_INHERITED", ""),
        os.environ.get("RIGHTKIT_PLUGIN_NETWORK", ""),
        os.environ.get("RIGHTKIT_PLUGIN_TEST_SECRET", ""),
        os.environ.get("HTTP_PROXY", ""),
        os.environ.get("http_proxy", ""),
        os.environ.get("RIGHTKIT_PLUGIN_ROOT", ""),
        os.environ.get("RIGHTKIT_WORKSPACE_ROOT", "")
    ])
}))
"#,
        )
        .unwrap();

        std::env::set_var("RIGHTKIT_PLUGIN_TEST_SECRET", "do-not-leak");
        std::env::set_var("HTTP_PROXY", "http://proxy.invalid:8080");
        std::env::set_var("http_proxy", "http://proxy.invalid:8080");

        let result = run_plugin_entry_with_policy(
            "plugin.py",
            dir.path(),
            PluginInvocation {
                kind: "tool".into(),
                name: "fixture_tool".into(),
                args: serde_json::json!({}),
                session_id: "s1".into(),
                workspace_root: dir.path().to_path_buf(),
            },
            PluginSandboxPolicy {
                inherit_env: false,
                allowed_env: vec!["PATH".into(), "Path".into(), "SYSTEMROOT".into()],
                denied_env: vec!["RIGHTKIT_PLUGIN_TEST_SECRET".into()],
                strip_network_env: true,
                ..PluginSandboxPolicy::default()
            },
        )
        .await
        .unwrap();

        std::env::remove_var("RIGHTKIT_PLUGIN_TEST_SECRET");
        std::env::remove_var("HTTP_PROXY");
        std::env::remove_var("http_proxy");

        assert!(!result.is_error);
        assert!(result.content.starts_with("false|disabled|||"));
        assert!(result.content.contains(&dir.path().display().to_string()));
    }

    #[tokio::test]
    async fn metadata_only_isolation_refuses_subprocess_execution() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("plugin.py");
        std::fs::write(&script, "print('{}')").unwrap();

        let err = run_plugin_entry_with_policy(
            "plugin.py",
            dir.path(),
            PluginInvocation {
                kind: "tool".into(),
                name: "fixture_tool".into(),
                args: serde_json::json!({}),
                session_id: "s1".into(),
                workspace_root: dir.path().to_path_buf(),
            },
            PluginSandboxPolicy {
                isolation: PluginIsolationMode::MetadataOnly,
                ..PluginSandboxPolicy::default()
            },
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("metadataOnly"));
    }

    #[tokio::test]
    async fn restricted_subprocess_forces_env_scrub_and_relative_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("plugin.py");
        std::fs::write(
            &script,
            r#"
import json, os
print(json.dumps({
    "content": "|".join([
        os.environ.get("RIGHTKIT_PLUGIN_ENV_INHERITED", ""),
        os.environ.get("RIGHTKIT_PLUGIN_NETWORK", ""),
        os.environ.get("RIGHTKIT_PLUGIN_TEST_SECRET", ""),
        os.environ.get("HTTP_PROXY", "")
    ])
}))
"#,
        )
        .unwrap();
        std::env::set_var("RIGHTKIT_PLUGIN_TEST_SECRET", "do-not-leak");
        std::env::set_var("HTTP_PROXY", "http://proxy.invalid:8080");

        let absolute_err = run_plugin_entry_with_policy(
            &script,
            dir.path(),
            PluginInvocation {
                kind: "tool".into(),
                name: "fixture_tool".into(),
                args: serde_json::json!({}),
                session_id: "s1".into(),
                workspace_root: dir.path().to_path_buf(),
            },
            PluginSandboxPolicy {
                isolation: PluginIsolationMode::RestrictedSubprocess,
                allow_absolute_entry: true,
                inherit_env: true,
                strip_network_env: false,
                timeout_ms: 30_000,
                ..PluginSandboxPolicy::default()
            },
        )
        .await
        .unwrap_err();
        assert!(absolute_err
            .to_string()
            .contains("relative to the plugin root"));

        let result = run_plugin_entry_with_policy(
            "plugin.py",
            dir.path(),
            PluginInvocation {
                kind: "tool".into(),
                name: "fixture_tool".into(),
                args: serde_json::json!({}),
                session_id: "s1".into(),
                workspace_root: dir.path().to_path_buf(),
            },
            PluginSandboxPolicy {
                isolation: PluginIsolationMode::RestrictedSubprocess,
                allow_absolute_entry: true,
                inherit_env: true,
                strip_network_env: false,
                timeout_ms: 30_000,
                ..PluginSandboxPolicy::default()
            },
        )
        .await
        .unwrap();

        std::env::remove_var("RIGHTKIT_PLUGIN_TEST_SECRET");
        std::env::remove_var("HTTP_PROXY");

        assert!(!result.is_error);
        assert_eq!(result.content, "false|disabled||");
    }
}
