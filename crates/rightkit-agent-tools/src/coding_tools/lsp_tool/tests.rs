#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_tools::process_resource_authority::{
        install_process_resource_profile, GovernedProcessLaunchAdapter,
    };
    use rightkit_agent_engine_runtime_resources::{ResourceKind, ResourceProfile, ServiceScope};
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use std::sync::Arc;
    use tempfile::tempdir;

    fn ctx(root: &Path) -> ToolContext {
        ToolContext::new("s1", root, PermissionMode::Bypass, WorkspaceTrust::Trusted)
    }

    #[test]
    fn diagnostic_line_format_is_stable() {
        assert_eq!(
            format_diagnostic_line("file:///x/bad.rs", 3u32, 5u32, 1, "boom"),
            "file:///x/bad.rs:3:5 severity=1 boom"
        );
    }

    #[tokio::test]
    async fn local_goto_definition_finds_rust_function() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(
            &file,
            "pub fn compute(x: i32) -> i32 { x + 1 }\n\nfn main() {\n    let y = compute(2);\n}\n",
        )
        .unwrap();

        let result = LspTool
            .execute(
                json!({
                    "operation": "goto_definition",
                    "path": "lib.rs",
                    "line": 4,
                    "character": 13
                }),
                &ctx(dir.path()),
            )
            .await;

        assert!(!result.is_error, "{}", result.content);
        assert!(
            result.content.contains("Definitions for 'compute'"),
            "{}",
            result.content
        );
        assert!(result.content.contains("lib.rs:1"), "{}", result.content);
        assert!(
            result.content.contains("pub fn compute"),
            "{}",
            result.content
        );
    }

    #[test]
    fn workspace_lsp_config_roundtrips_and_status_prefers_config() {
        let dir = tempdir().unwrap();
        let executable = dir.path().join("custom-rust-analyzer.exe");
        std::fs::write(&executable, "test executable identity").unwrap();
        let config = LspWorkspaceConfig {
            servers: vec![LspWorkspaceServer {
                language: "rust".to_string(),
                extensions: vec!["rs".to_string()],
                server_path: executable.display().to_string(),
                server_args: vec!["--stdio".to_string()],
            }],
        };

        write_lsp_workspace_config(dir.path(), &config).unwrap();

        let loaded = load_lsp_workspace_config(dir.path());
        assert_eq!(loaded, config);
        let status = lsp_status(dir.path());
        let rust = status
            .servers
            .iter()
            .find(|server| server.language == "rust")
            .unwrap();
        assert_eq!(
            rust.configured_path.as_deref(),
            Some(executable.to_str().unwrap())
        );
        assert_eq!(
            rust.effective_path.as_deref(),
            Some(executable.to_str().unwrap())
        );
        assert!(rust.available);
        assert_eq!(rust.source, "workspace-config");
        assert!(status.fallback_enabled);

        let target = dir.path().join("lib.rs");
        let resolved =
            server_config_from_args_or_path(&json!({}), &target, &ctx(dir.path())).unwrap();
        assert_eq!(resolved.server_path, executable.display().to_string());
    }

    #[test]
    fn configured_missing_server_is_not_reported_available() {
        let dir = tempdir().unwrap();
        write_lsp_workspace_config(
            dir.path(),
            &LspWorkspaceConfig {
                servers: vec![LspWorkspaceServer {
                    language: "python".to_string(),
                    extensions: vec!["py".to_string()],
                    server_path: "rightkit-agent-deliberately-missing-pyright-server".to_string(),
                    server_args: vec!["--stdio".to_string()],
                }],
            },
        )
        .unwrap();
        let status = lsp_status(dir.path());
        let python = status
            .servers
            .iter()
            .find(|row| row.language == "python")
            .unwrap();
        assert_eq!(python.source, "workspace-config");
        assert!(!python.available);
        assert!(python.effective_path.is_none());
        assert!(server_config_from_args_or_path(
            &json!({}),
            &dir.path().join("x.py"),
            &ctx(dir.path())
        )
        .is_none());
    }

    #[test]
    fn relative_configured_server_is_resolved_against_workspace() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("tools")).unwrap();
        let executable = dir.path().join("tools/server.exe");
        std::fs::write(&executable, "test executable identity").unwrap();
        assert_eq!(
            resolve_server_executable(Path::new("tools/server.exe"), dir.path()),
            Some(executable)
        );
        assert_eq!(default_server_args(Path::new("x.py")), vec!["--stdio"]);
        assert_eq!(default_server_args(Path::new("x.rs")), Vec::<String>::new());
    }

    #[tokio::test]
    async fn references_require_a_real_language_server() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.unknown"),
            "fn compute() {}\nfn main() {\n    compute();\n}\n",
        )
        .unwrap();

        let result = LspTool
            .execute(
                json!({
                    "operation": "references",
                    "path": "lib.unknown",
                    "line": 3,
                    "character": 6
                }),
                &ctx(dir.path()),
            )
            .await;

        assert!(result.is_error, "{}", result.content);
        assert!(result
            .content
            .contains("References require a configured or discovered language server"));
    }

    #[tokio::test]
    async fn local_scan_skips_symlinked_directories() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn compute(x: i32) -> i32 { x + 1 }\n",
        )
        .unwrap();

        // A symlinked directory that points back at the workspace root would
        // make an unbounded recursive walk cycle forever; the scan must skip
        // symlinks rather than follow them. Skip silently on platforms/CI
        // configurations that refuse to create a symlink (e.g. unprivileged
        // Windows) — the assertion below is about not hanging, not about
        // this specific reproduction succeeding everywhere.
        #[cfg(unix)]
        let linked = {
            let link = dir.path().join("cycle");
            std::os::unix::fs::symlink(dir.path(), &link)
                .is_ok()
                .then_some(link)
        };
        #[cfg(windows)]
        let linked = {
            let link = dir.path().join("cycle");
            std::os::windows::fs::symlink_dir(dir.path(), &link)
                .is_ok()
                .then_some(link)
        };

        let result = LspTool
            .execute(
                json!({
                    "operation": "goto_definition",
                    "path": "lib.rs",
                    "line": 1,
                    "character": 10
                }),
                &ctx(dir.path()),
            )
            .await;

        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("lib.rs:1"), "{}", result.content);
        let _ = linked;
    }

    #[tokio::test]
    async fn local_fallback_refuses_oversized_target_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("huge.rs");
        // Sparse-write a file that reports as larger than the fallback bound
        // without actually allocating that much disk.
        let f = std::fs::File::create(&file).unwrap();
        f.set_len(3 * 1024 * 1024).unwrap();

        let result = LspTool
            .execute(
                json!({
                    "operation": "goto_definition",
                    "path": "huge.rs",
                    "line": 1,
                    "character": 0
                }),
                &ctx(dir.path()),
            )
            .await;

        assert!(result.is_error);
        assert!(
            result.content.contains("refuses files over"),
            "{}",
            result.content
        );
    }

    #[tokio::test]
    async fn status_reports_fallback() {
        let dir = tempdir().unwrap();
        let result = LspTool
            .execute(json!({"operation": "status"}), &ctx(dir.path()))
            .await;

        assert!(!result.is_error);
        assert!(result
            .content
            .contains("fallback: local symbol index enabled"));
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "explicitly launches one bounded LSP fixture process"]
    async fn expanded_real_lsp_producer_reports_publish_diagnostics_and_releases() {
        use std::time::{Duration, Instant};

        let root = tempdir().unwrap();
        let source = root.path().join("main.rs");
        std::fs::write(&source, "fn main() {}\n").unwrap();

        // The fixture is a Cargo-managed example built by the caller through
        // RightKit. Tests consume only that existing executable; they never
        // compile source or bypass the governed process-launch path.
        let test_executable = std::env::current_exe().unwrap();
        let built_fixture = test_executable
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples")
            .join("krn010_lsp_fixture.exe");
        assert!(
            built_fixture.is_file(),
            "LSP fixture executable must exist: {}",
            built_fixture.display()
        );
        let fixture = root.path().join("krn010_lsp_fixture.exe");
        std::fs::copy(&built_fixture, &fixture).expect("stage LSP fixture inside workspace");
        let context = rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: root.path().to_string_lossy().into_owned(),
            sandbox_profile: Some("workspace-write".to_string()),
            sandbox_required: true,
            network_required: false,
        };
        let governor = install_process_resource_profile(ResourceProfile::Expanded).unwrap_or_else(
            |existing| panic!("resource profile must be Expanded, found {existing:?}"),
        );
        let adapter = Arc::new(GovernedProcessLaunchAdapter::new(
            crate::coding_tools::shell::NativeCommandSandboxAdapter,
            governor.clone(),
            ResourceKind::Lsp,
            true,
        ));
        let config = rightkit_mcp::agent_mcp::LspServerConfig {
            language_id: "rust".into(),
            server_path: fixture.to_string_lossy().into_owned(),
            server_args: Vec::new(),
            workspace_root: format!(
                "file:///{}",
                root.path().to_string_lossy().replace('\\', "/")
            ),
        };
        let mut bridge = rightkit_mcp::agent_mcp::LspMcpBridge::new(config)
            .with_process_launcher(adapter, context);
        tokio::time::timeout(Duration::from_secs(15), bridge.initialize())
            .await
            .expect("real LSP fixture initialization must be bounded")
            .expect("real LSP fixture must initialize");
        let uri = format!("file:///{}", source.to_string_lossy().replace('\\', "/"));
        let diagnostics = tokio::time::timeout(Duration::from_secs(15), bridge.diagnostics(&uri))
            .await
            .expect("real LSP diagnostics must be bounded")
            .expect("real LSP fixture must publish diagnostics");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].uri, uri);
        assert_eq!(diagnostics[0].message, "KRN010 fixture diagnostic");
        let scope = ServiceScope::Workspace(root.path().to_string_lossy().into_owned());
        assert_eq!(governor.usage(ResourceKind::ExternalExecution, &scope), 1);
        assert_eq!(governor.usage(ResourceKind::Lsp, &scope), 1);
        bridge.shutdown().await;
        drop(bridge);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline
            && (governor.usage(ResourceKind::ExternalExecution, &scope) != 0
                || governor.usage(ResourceKind::Lsp, &scope) != 0)
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(governor.usage(ResourceKind::ExternalExecution, &scope), 0);
        assert_eq!(governor.usage(ResourceKind::Lsp, &scope), 0);
    }
}
