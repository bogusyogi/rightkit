#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    fn ctx(root: &std::path::Path) -> ToolContext {
        let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
            crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        ToolContext::new("s1", root, PermissionMode::Bypass, WorkspaceTrust::Trusted)
    }

    fn sandbox_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    struct SandboxEnvGuard(Option<std::ffi::OsString>);

    impl SandboxEnvGuard {
        fn set(value: Option<&str>) -> Self {
            let previous = std::env::var_os("RIGHTKIT_SANDBOX_TIER");
            match value {
                Some(value) => std::env::set_var("RIGHTKIT_SANDBOX_TIER", value),
                None => std::env::remove_var("RIGHTKIT_SANDBOX_TIER"),
            }
            Self(previous)
        }
    }

    impl Drop for SandboxEnvGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("RIGHTKIT_SANDBOX_TIER", value),
                None => std::env::remove_var("RIGHTKIT_SANDBOX_TIER"),
            }
        }
    }

    #[tokio::test]
    async fn shell_runs_echo() {
        let dir = tempdir().unwrap();
        let tool = ShellTool;
        let result = tool
            .execute(json!({"command": "echo hello"}), &ctx(dir.path()))
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("hello"));
    }

    #[tokio::test]
    async fn external_provenance_cannot_bypass_durable_write_gate_through_shell() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("AGENTS.md");
        let command = if cfg!(windows) {
            "Set-Content -LiteralPath AGENTS.md -Value poisoned"
        } else {
            "printf poisoned > AGENTS.md"
        };
        let external =
            ctx(dir.path()).with_content_provenance(rightkit_agent_policy::ContentProvenance::new(
                rightkit_agent_policy::ContentOrigin::WebDerived,
                "browser_fetch:https://untrusted.invalid",
            ));

        assert_eq!(
            ShellTool.durable_write_kind(&json!({"command": command})),
            Some(rightkit_agent_policy::DurableWriteKind::ShellCommand)
        );
        let result = ShellTool
            .execute(json!({"command": command}), &external)
            .await;

        assert!(result.is_error, "{}", result.content);
        assert!(result.content.contains("explicit durable-write approval"));
        assert!(!target.exists(), "unapproved shell command mutated disk");
    }

    #[tokio::test]
    async fn approved_external_shell_call_reaches_normal_execution_policy() {
        let dir = tempdir().unwrap();
        let external = ctx(dir.path())
            .with_content_provenance(rightkit_agent_policy::ContentProvenance::new(
                rightkit_agent_policy::ContentOrigin::McpDerived,
                "mcp:test",
            ))
            .with_durable_write_approval(true);
        let result = ShellTool
            .execute(json!({"command": "echo approved"}), &external)
            .await;

        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("approved"));
    }

    #[tokio::test]
    async fn shell_populates_process_id_for_foreground_execution() {
        let dir = tempdir().unwrap();
        let tool = ShellTool;
        let result = tool
            .execute(json!({"command": "echo pid-test"}), &ctx(dir.path()))
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(
            result.process_id.is_some(),
            "foreground shell execution should populate process_id, got None"
        );
        assert!(
            result.process_id.as_ref().unwrap().parse::<u32>().is_ok(),
            "process_id should be a valid OS PID string"
        );
    }

    #[tokio::test]
    async fn foreground_shell_records_completed_execution_target() {
        let dir = tempdir().unwrap();
        let store = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let ctx = ctx(dir.path())
            .with_background_shells(store.clone())
            .with_execution_backend("local")
            .with_execution_workspace_id("foreground-workspace")
            .with_execution_network_profile("local-host");
        let tool = ShellTool;
        let result = tool
            .execute(json!({"command": "echo foreground-target"}), &ctx)
            .await;

        assert!(!result.is_error, "{}", result.content);
        let shells = store.lock().unwrap();
        let row = shells
            .values()
            .next()
            .expect("foreground shell should record a completed process row");
        assert_eq!(row.process_kind(), "foregroundShell");
        assert_eq!(
            row.status(),
            crate::coding_tools::background_shell::BgStatus::Exited(0)
        );
        assert!(row.output_snapshot().contains("foreground-target"));
        let target = row.execution_target();
        assert_eq!(target.backend_id.as_deref(), Some("local"));
        assert_eq!(target.workspace_id.as_deref(), Some("foreground-workspace"));
        assert_eq!(target.network_profile.as_deref(), Some("local-host"));
    }

    #[tokio::test]
    async fn foreground_shell_preserves_nonzero_exit_status() {
        let dir = tempdir().unwrap();
        let tool = ShellTool;
        let command = if cfg!(windows) {
            "exit 7"
        } else {
            "sh -c 'exit 7'"
        };
        let result = tool
            .execute(json!({"command": command}), &ctx(dir.path()))
            .await;

        assert!(result.is_error, "non-zero shell exit should be an error");
        assert!(
            result.content.contains("[exit 7]"),
            "unexpected shell result: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn shell_rejects_restricted_network_profile_before_host_spawn() {
        let dir = tempdir().unwrap();
        let marker = dir.path().join("restricted-network-shell-ran.txt");
        let command = if cfg!(windows) {
            "cmd /C echo restricted-network-shell-ran>restricted-network-shell-ran.txt"
        } else {
            "printf restricted-network-shell-ran > restricted-network-shell-ran.txt"
        };
        let ctx = ctx(dir.path())
            .with_execution_backend("local")
            .with_execution_network_profile("egress-restricted");
        let tool = ShellTool;
        let result = tool.execute(json!({"command": command}), &ctx).await;

        assert!(result.is_error);
        assert!(
            result
                .content
                .contains("shell networkProfile 'egress-restricted'"),
            "{}",
            result.content
        );
        assert!(
            !marker.exists(),
            "restricted network shell must not execute on the host"
        );
    }

    #[tokio::test]
    async fn background_shell_records_execution_target_from_context() {
        let dir = tempdir().unwrap();
        let store = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let ctx = ctx(dir.path())
            .with_background_shells(store.clone())
            .with_execution_backend("local")
            .with_execution_workspace_id("shell-workspace")
            .with_execution_network_profile("local-host");
        let tool = ShellTool;

        let result = tool
            .execute(
                json!({"command": "echo target-context", "run_in_background": true}),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "{}", result.content);
        let target = store
            .lock()
            .unwrap()
            .values()
            .next()
            .expect("background shell should be registered")
            .execution_target();
        assert_eq!(target.backend_id.as_deref(), Some("local"));
        assert_eq!(target.workspace_id.as_deref(), Some("shell-workspace"));
        assert_eq!(target.network_profile.as_deref(), Some("local-host"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn powershell_runs_command_on_windows() {
        let dir = tempdir().unwrap();
        let tool = PowerShellTool;
        let result = tool
            .execute(json!({"command": "Write-Output hello"}), &ctx(dir.path()))
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("hello"));
    }

    /// A confined shell must actually run IN the session workspace: a relative
    /// file read only resolves when the child's PowerShell provider location
    /// lands on `ctx.workspace_root`, a marker written through the shell must
    /// land inside the workspace, and the reported cwd (a path alias the
    /// container can traverse) must resolve to that root. The workspace name
    /// carries a space and a second workspace holds a same-named sentinel so
    /// a wrong-directory execution cannot pass.
    #[cfg(windows)]
    #[tokio::test]
    async fn sandboxed_shell_observes_workspace_root_as_cwd() {
        // The workspace lives under the user profile: the same subtree that
        // real sessions use and whose ancestors deny the container token an
        // attribute probe, which is what the cwd alias exists to handle.
        let profile = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = tempfile::tempdir_in(profile).unwrap();
        let workspace = dir.path().join("work space");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("sentinel.txt"), "sentinel-A").unwrap();
        let other = dir.path().join("other space");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("sentinel.txt"), "sentinel-WRONG").unwrap();

        let tool = ShellTool;
        let result = tool
            .execute(
                json!({"command": "type sentinel.txt; \"cwd=\" + (Get-Location).Path; New-Item -Path .\\shell-cwd.txt -ItemType File -Force | Out-Null"}),
                &ctx(&workspace),
            )
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(
            result.content.contains("sentinel-A"),
            "relative read must resolve inside the workspace: {}",
            result.content
        );
        assert!(
            !result.content.contains("sentinel-WRONG"),
            "shell observed a different workspace: {}",
            result.content
        );
        // A marker created through the shell lands in the real workspace —
        // the functional definition of "cwd is the selected workspace".
        assert!(
            workspace.join("shell-cwd.txt").exists(),
            "shell-created file must land inside the workspace: {}",
            result.content
        );
        // The reported cwd may be a junction alias (the container cannot
        // attribute-probe profile ancestors); it must resolve to the same
        // directory.
        let reported = result
            .content
            .lines()
            .find_map(|line| line.strip_prefix("cwd="))
            .unwrap_or("")
            .trim()
            .to_string();
        let canonical_workspace = std::fs::canonicalize(&workspace).unwrap();
        let resolved =
            std::fs::canonicalize(&reported).unwrap_or_else(|_| PathBuf::from(&reported));
        assert_eq!(
            resolved, canonical_workspace,
            "reported cwd must resolve to the session workspace: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn shell_blocked_by_permission() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(
            "s1",
            dir.path(),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );
        let tool = ShellTool;
        let result = tool.execute(json!({"command": "echo hello"}), &ctx).await;
        assert!(result.is_error);
        assert!(
            result.content.contains("Permission denied")
                || result.content.contains("requires approval")
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn shell_autosafe_allows_routine_compound_command() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(
            "s1",
            dir.path(),
            PermissionMode::AutoSafe,
            WorkspaceTrust::Trusted,
        );
        let result = ShellTool
            .execute(json!({"command": "printf hello | wc -c"}), &ctx)
            .await;

        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains('5'), "{}", result.content);
    }

    #[tokio::test]
    async fn shell_autosafe_checks_nonleading_segment_before_spawn() {
        let dir = tempdir().unwrap();
        let sentinel = dir.path().join("sentinel");
        std::fs::create_dir(&sentinel).unwrap();
        let ctx = ToolContext::new(
            "s1",
            dir.path(),
            PermissionMode::AutoSafe,
            WorkspaceTrust::Trusted,
        );
        let result = ShellTool
            .execute(json!({"command": "true && rm -rf sentinel"}), &ctx)
            .await;

        assert!(result.is_error);
        assert!(
            result.content.contains("requires approval"),
            "{}",
            result.content
        );
        assert!(sentinel.exists(), "command must be rejected before spawn");
    }

    #[tokio::test]
    async fn powershell_autosafe_fails_closed_before_spawn() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(
            "s1",
            dir.path(),
            PermissionMode::AutoSafe,
            WorkspaceTrust::Trusted,
        );
        let result = PowerShellTool
            .execute(
                json!({"command": "Get-ChildItem; Remove-Item -Recurse target"}),
                &ctx,
            )
            .await;

        assert!(result.is_error);
        assert!(
            result.content.contains("powershell-fail-closed"),
            "{}",
            result.content
        );
    }

    #[tokio::test]
    async fn shell_timeout_returns_error() {
        let dir = tempdir().unwrap();
        let tool = ShellTool;
        let result = tool
            .execute(
                json!({"command": "sleep 10", "timeout_ms": 100}),
                &ctx(dir.path()),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("timed out"));
    }

    #[tokio::test]
    async fn shell_cancellation_token_kills_foreground_process() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let command = if cfg!(windows) {
            "Start-Sleep -Seconds 30"
        } else {
            "sleep 30"
        };
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(async move {
            let tool = ShellTool;
            let ctx = ctx(&root).with_cancel_rx(cancel_rx);
            tool.execute(json!({"command": command, "timeout_ms": 30000}), &ctx)
                .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        cancel_tx.send(true).unwrap();
        let result = handle.await.unwrap();

        assert!(result.is_error);
        assert!(
            result.content.contains("cancelled by operator"),
            "{}",
            result.content
        );
    }

    #[test]
    fn repo_carried_workspace_setting_cannot_disable_strict_default() {
        let _lock = sandbox_env_lock();
        let _env = SandboxEnvGuard::set(None);
        let dir = tempdir().unwrap();
        let settings_dir = dir.path().join(".rightkit");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(
            settings_dir.join("sandbox-settings.json"),
            r#"{"tier":"none","updatedAt":"2026-06-10T00:00:00Z"}"#,
        )
        .unwrap();

        assert!(!sandbox_disabled_for_workspace(dir.path()));
        assert_eq!(
            sandbox_tier_for_workspace(dir.path()),
            crate::coding_tools::sandbox::SandboxTier::Strict
        );
    }

    #[test]
    fn sandbox_tier_reads_strict_from_workspace_settings() {
        let _lock = sandbox_env_lock();
        let _env = SandboxEnvGuard::set(None);
        let dir = tempdir().unwrap();
        let settings_dir = dir.path().join(".rightkit");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(
            settings_dir.join("sandbox-settings.json"),
            r#"{"tier":"strict"}"#,
        )
        .unwrap();

        assert_eq!(
            sandbox_tier_for_workspace(dir.path()),
            crate::coding_tools::sandbox::SandboxTier::Strict
        );
        assert!(!sandbox_disabled_for_workspace(dir.path()));
    }

    #[test]
    fn persisted_ui_tier_and_execution_tier_agree() {
        let _lock = sandbox_env_lock();
        let _env = SandboxEnvGuard::set(None);
        let dir = tempdir().unwrap();
        let settings_dir = dir.path().join(".rightkit");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(
            settings_dir.join("sandbox-settings.json"),
            r#"{"tier":"landlock-seccomp"}"#,
        )
        .unwrap();

        let policy = sandbox_policy_for_workspace(dir.path());
        assert_eq!(
            policy.effective,
            crate::coding_tools::sandbox::SandboxTier::Strict
        );
        assert_eq!(policy.effective.persisted_value(), "landlock-seccomp");
    }

    #[test]
    fn sandbox_tier_defaults_to_strict() {
        let _lock = sandbox_env_lock();
        let _env = SandboxEnvGuard::set(None);
        let dir = tempdir().unwrap();
        assert_eq!(
            sandbox_tier_for_workspace(dir.path()),
            crate::coding_tools::sandbox::SandboxTier::Strict
        );
    }

    #[test]
    fn operator_strict_floor_overrides_workspace_none() {
        let _lock = sandbox_env_lock();
        let _env = SandboxEnvGuard::set(Some("strict"));
        let dir = tempdir().unwrap();
        let settings_dir = dir.path().join(".rightkit");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(
            settings_dir.join("sandbox-settings.json"),
            r#"{"tier":"none"}"#,
        )
        .unwrap();

        let policy = sandbox_policy_for_workspace(dir.path());
        assert_eq!(
            policy.operator,
            Some(crate::coding_tools::sandbox::SandboxTier::Strict)
        );
        assert_eq!(
            policy.workspace,
            Some(crate::coding_tools::sandbox::SandboxTier::Disabled)
        );
        assert_eq!(
            policy.effective,
            crate::coding_tools::sandbox::SandboxTier::Strict
        );
        assert!(!sandbox_disabled_for_workspace(dir.path()));
    }

    #[test]
    fn strict_tier_config_enables_seccomp_and_network_deny() {
        let cfg = local_sandbox_config_for_tier(
            Path::new("/ws"),
            crate::coding_tools::sandbox::SandboxTier::Strict,
        );
        assert!(cfg.seccomp_enabled);
        assert!(cfg.network_denied);
    }

    #[test]
    fn workspace_write_tier_config_is_filesystem_only() {
        let cfg = local_sandbox_config_for_tier(
            Path::new("/ws"),
            crate::coding_tools::sandbox::SandboxTier::WorkspaceWrite,
        );
        assert!(!cfg.seccomp_enabled);
        assert!(!cfg.network_denied);
        assert!(cfg
            .allowed_write_paths
            .iter()
            .any(|p| p == Path::new("/ws")));
    }

    #[test]
    fn selected_shell_uses_powershell_flavor() {
        let (program, flag) = selected_shell(ShellFlavor::PowerShell);
        assert_eq!(flag, "-Command");
        if cfg!(windows) {
            assert_eq!(program, "powershell");
        } else {
            assert_eq!(program, "pwsh");
        }
    }

    fn only_spill_path(base: &std::path::Path) -> PathBuf {
        let dir = base
            .join(crate::coding_tools::output_compaction::session_store_dir_name("s1"))
            .join("shell-output");
        std::fs::read_dir(dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    }

    #[test]
    fn spill_above_cap_reports_truncation_and_sizes_never_full() {
        let dir = tempdir().unwrap();
        let big = "x".repeat(SHELL_OUTPUT_SPILL_MAX_BYTES + 10);
        let ShellOutputSpill::Stored { handle } = spill_shell_output_into(dir.path(), "s1", &big)
        else {
            panic!("expected a stored recovery copy");
        };
        assert!(handle.contains("truncated"));
        assert!(handle.contains(&format!("captured {} bytes", big.len())));
        assert!(handle.contains(&format!("stored {} bytes", SHELL_OUTPUT_SPILL_MAX_BYTES)));
        assert!(!handle.to_ascii_lowercase().contains("full"));
    }

    #[test]
    fn spill_under_cap_reports_full_output() {
        let dir = tempdir().unwrap();
        let text = "line\n".repeat(10);
        let ShellOutputSpill::Stored { handle } = spill_shell_output_into(dir.path(), "s1", &text)
        else {
            panic!("expected a stored recovery copy");
        };
        assert!(handle.contains("full output"));
        assert!(handle.contains(&format!("({} bytes captured and stored)", text.len())));
        assert!(!handle.contains("truncated"));
    }

    #[test]
    fn read_after_eviction_is_typed_unavailable() {
        let dir = tempdir().unwrap();
        let ShellOutputSpill::Stored { .. } = spill_shell_output_into(dir.path(), "s1", "hello")
        else {
            panic!("expected a stored recovery copy");
        };
        let path = only_spill_path(dir.path());
        assert_eq!(read_shell_output_recovery(&path).unwrap(), "hello");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            read_shell_output_recovery(&path),
            Err(ShellOutputUnavailable::Evicted)
        );
    }

    #[test]
    fn read_of_unknown_copy_is_typed_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("no-such-dir").join("x.txt");
        assert_eq!(
            read_shell_output_recovery(&path),
            Err(ShellOutputUnavailable::Missing)
        );
    }

    #[test]
    fn write_failure_is_typed_unavailable() {
        let dir = tempdir().unwrap();
        // A regular file where the session directory must be created.
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        assert_eq!(
            spill_shell_output_into(&blocker, "s1", "data"),
            ShellOutputSpill::Unavailable(ShellOutputUnavailable::WriteFailed)
        );
    }

    fn counting_notifier() -> (
        rightkit_agent_engine_protocol::ToolOutputNotifier,
        std::sync::Arc<Mutex<Vec<String>>>,
    ) {
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        (
            rightkit_agent_engine_protocol::ToolOutputNotifier::new(move |_, text| {
                sink.lock().unwrap().push(text.to_string());
            }),
            seen,
        )
    }

    #[tokio::test]
    async fn stream_drain_caps_retained_bytes_and_marks_truncation() {
        use tokio::io::AsyncWriteExt;
        let (mut tx, rx) = tokio::io::duplex(64 * 1024);
        let drain = StreamDrain::spawn_capped(
            Some(rx),
            rightkit_agent_engine_protocol::ToolOutputStream::Stdout,
            Default::default(),
            1000,
        );
        let writer = tokio::spawn(async move {
            tx.write_all(&vec![b'a'; 10_000]).await.unwrap();
        });
        let out = drain.finish().await;
        writer.await.unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.starts_with(&"a".repeat(1000)));
        assert!(!text.starts_with(&"a".repeat(1001)));
        assert!(text.contains("output truncated: 9000 bytes"), "{text}");
        assert!(out.len() < 1500);
    }

    #[tokio::test]
    async fn stream_drain_coalesces_live_deltas_without_losing_bytes() {
        use tokio::io::AsyncWriteExt;
        let (notifier, seen) = counting_notifier();
        let (mut tx, rx) = tokio::io::duplex(1024 * 1024);
        let drain = StreamDrain::spawn(
            Some(rx),
            rightkit_agent_engine_protocol::ToolOutputStream::Stdout,
            notifier,
        );
        let mut expected = String::new();
        for i in 0..2000 {
            let line = format!("line-{i:05}\n");
            expected.push_str(&line);
            tx.write_all(line.as_bytes()).await.unwrap();
        }
        drop(tx);
        drain.finish().await;
        let deltas = seen.lock().unwrap().clone();
        assert_eq!(deltas.concat(), expected);
        assert!(
            deltas.len() < 200,
            "{} deltas for 2000 writes",
            deltas.len()
        );
    }

    #[tokio::test]
    async fn stream_drain_flushes_stalled_tail_on_deadline() {
        use tokio::io::AsyncWriteExt;
        let (notifier, seen) = counting_notifier();
        let (mut tx, rx) = tokio::io::duplex(1024);
        let drain = StreamDrain::spawn(
            Some(rx),
            rightkit_agent_engine_protocol::ToolOutputStream::Stdout,
            notifier,
        );
        tx.write_all(b"hi").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(seen.lock().unwrap().concat(), "hi");
        drop(tx);
        drain.finish().await;
    }

    #[tokio::test]
    async fn shell_cancel_signalled_before_start_never_spawns() {
        let dir = tempdir().unwrap();
        let marker = dir.path().join("ran.txt");
        let command = if cfg!(windows) {
            format!("Set-Content -Path '{}' -Value x", marker.display())
        } else {
            format!("touch '{}'", marker.display())
        };
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(true);
        let ctx = ctx(dir.path()).with_cancel_rx(cancel_rx);
        let result = ShellTool
            .execute(json!({"command": command, "timeout_ms": 5000}), &ctx)
            .await;
        assert!(result.is_error);
        assert!(
            result.content.contains("cancelled by operator"),
            "{}",
            result.content
        );
        assert!(!marker.exists());
    }
}
