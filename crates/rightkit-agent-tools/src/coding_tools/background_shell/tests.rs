#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use tempfile::tempdir;

    fn ctx_with_store(root: &Path, store: BgStore) -> ToolContext {
        let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
            crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        ToolContext::new("s1", root, PermissionMode::Bypass, WorkspaceTrust::Trusted)
            .with_background_shells(store)
    }

    async fn wait_until_done(store: &BgStore, id: &str) -> BgStatus {
        for _ in 0..500 {
            let st = store.lock().unwrap().get(id).map(|s| s.status());
            match st {
                Some(BgStatus::Running) => {}
                Some(done) => return done,
                None => panic!("missing background shell id {id}"),
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        store
            .lock()
            .unwrap()
            .get(id)
            .map(|s| s.status())
            .unwrap_or_else(|| panic!("missing background shell id {id}"))
    }

    #[test]
    fn background_admission_denial_precedes_publication() {
        let mut limits = rightkit_agent_engine_runtime_resources::ResourceLimits::default();
        limits.daemon_background = 0;
        let governor = rightkit_agent_engine_runtime_resources::ResourceGovernor::new(limits);
        let scope = rightkit_agent_engine_runtime_resources::ServiceScope::Workspace("/denied".into());
        let result = crate::coding_tools::process_resource_authority::reserve_with_governor(
            &governor,
            crate::coding_tools::process_resource_authority::ResourceKind::BackgroundProcess,
            scope.clone(),
            false,
        );
        assert!(result.is_err());
        assert_eq!(
            governor.usage(
                crate::coding_tools::process_resource_authority::ResourceKind::BackgroundProcess,
                &scope
            ),
            0
        );
        assert_eq!(
            governor.usage(
                crate::coding_tools::process_resource_authority::ResourceKind::ExternalExecution,
                &scope
            ),
            0
        );
    }

    #[test]
    fn production_background_path_denial_precedes_registry_and_spawn() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let mut limits = rightkit_agent_engine_runtime_resources::ResourceLimits::default();
        limits.daemon_background = 0;
        let governor = Arc::new(rightkit_agent_engine_runtime_resources::ResourceGovernor::new(limits));
        let scope = rightkit_agent_engine_runtime_resources::ServiceScope::Workspace(
            dir.path().to_string_lossy().into_owned(),
        );
        let result = super::spawn_background_program_with_kind_and_target_using(
            "rightkit-agent-denial-side-effect".into(),
            Vec::new(),
            "denied".into(),
            dir.path(),
            &store,
            None,
            BgExecutionTarget::default(),
            false,
            "backgroundShell",
            || {
                crate::coding_tools::process_resource_authority::reserve_with_governor(
                    &governor,
                    crate::coding_tools::process_resource_authority::ResourceKind::BackgroundProcess,
                    scope.clone(),
                    false,
                )
                .map_err(crate::coding_tools::process_resource_authority::ProcessAdmissionError::Rejected)
            },
        );
        assert!(matches!(
            result,
            Err(crate::coding_tools::process_resource_authority::ProcessAdmissionError::Rejected(_))
        ));
        assert!(store.lock().unwrap().is_empty());
    }

    #[test]
    fn background_admission_is_held_until_settlement_then_released() {
        let mut limits = rightkit_agent_engine_runtime_resources::ResourceLimits::default();
        limits.daemon_background = 1;
        let governor = rightkit_agent_engine_runtime_resources::ResourceGovernor::new(limits);
        let scope = rightkit_agent_engine_runtime_resources::ServiceScope::Workspace("/held".into());
        let admission = crate::coding_tools::process_resource_authority::reserve_with_governor(
            &governor,
            crate::coding_tools::process_resource_authority::ResourceKind::BackgroundProcess,
            scope.clone(),
            false,
        )
        .expect("first background child admitted");
        assert!(crate::coding_tools::process_resource_authority::reserve_with_governor(
            &governor,
            crate::coding_tools::process_resource_authority::ResourceKind::BackgroundProcess,
            scope.clone(),
            false,
        )
        .is_err());
        drop(admission);
        assert!(crate::coding_tools::process_resource_authority::reserve_with_governor(
            &governor,
            crate::coding_tools::process_resource_authority::ResourceKind::BackgroundProcess,
            scope,
            false,
        )
        .is_ok());
    }

    #[tokio::test]
    async fn production_background_child_holds_capacity_until_exit() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let mut limits = rightkit_agent_engine_runtime_resources::ResourceLimits::default();
        limits.daemon_background = 1;
        let governor = Arc::new(rightkit_agent_engine_runtime_resources::ResourceGovernor::new(limits));
        let scope = rightkit_agent_engine_runtime_resources::ServiceScope::Workspace(
            dir.path().to_string_lossy().into_owned(),
        );
        let (program, flag) = crate::coding_tools::shell::platform_shell();
        let child_scope = scope.clone();
        let id = super::spawn_background_program_with_kind_and_target_using(
            program.into(),
            vec![flag.into(), "echo settled".into()],
            "echo settled".into(),
            dir.path(),
            &store,
            None,
            BgExecutionTarget::default(),
            false,
            "backgroundShell",
            || {
                crate::coding_tools::process_resource_authority::reserve_with_governor(
                    &governor,
                    crate::coding_tools::process_resource_authority::ResourceKind::BackgroundProcess,
                    child_scope,
                    false,
                )
                .map_err(crate::coding_tools::process_resource_authority::ProcessAdmissionError::Rejected)
            },
        )
        .expect("production background path admitted child");
        assert!(crate::coding_tools::process_resource_authority::reserve_with_governor(
            &governor,
            crate::coding_tools::process_resource_authority::ResourceKind::BackgroundProcess,
            scope.clone(),
            false,
        )
        .is_err());
        assert!(!matches!(
            wait_until_done(&store, &id).await,
            BgStatus::Running
        ));
        assert!(crate::coding_tools::process_resource_authority::reserve_with_governor(
            &governor,
            crate::coding_tools::process_resource_authority::ResourceKind::BackgroundProcess,
            scope,
            false,
        )
        .is_ok());
    }

    #[tokio::test]
    async fn production_background_spawn_failure_releases_capacity_for_re_admission() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let mut limits = rightkit_agent_engine_runtime_resources::ResourceLimits::default();
        limits.daemon_background = 1;
        let governor = Arc::new(rightkit_agent_engine_runtime_resources::ResourceGovernor::new(limits));
        let scope = rightkit_agent_engine_runtime_resources::ServiceScope::Workspace(
            dir.path().to_string_lossy().into_owned(),
        );
        let child_scope = scope.clone();
        let id = super::spawn_background_program_with_kind_and_target_using(
            "rightkit-agent-definitely-missing-executable".into(),
            Vec::new(),
            "spawn failure".into(),
            dir.path(),
            &store,
            None,
            BgExecutionTarget::default(),
            false,
            "backgroundShell",
            || {
                crate::coding_tools::process_resource_authority::reserve_with_governor(
                    &governor,
                    crate::coding_tools::process_resource_authority::ResourceKind::BackgroundProcess,
                    child_scope,
                    false,
                )
                .map_err(crate::coding_tools::process_resource_authority::ProcessAdmissionError::Rejected)
            },
        )
        .expect("admission must precede attempted spawn");
        assert!(matches!(
            wait_until_done(&store, &id).await,
            BgStatus::SpawnFailed(_)
        ));
        assert!(crate::coding_tools::process_resource_authority::reserve_with_governor(
            &governor,
            crate::coding_tools::process_resource_authority::ResourceKind::BackgroundProcess,
            scope,
            false,
        )
        .is_ok());
    }

    #[tokio::test]
    async fn background_echo_captures_output() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
            crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let id =
            spawn_background("echo hello-bg".into(), dir.path(), &store, None).expect("admission");
        let st = wait_until_done(&store, &id).await;
        assert!(!matches!(st, BgStatus::Running), "status: {st:?}");
        let r = GetShellOutputTool
            .execute(
                json!({ "shell_id": id }),
                &ctx_with_store(dir.path(), store.clone()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("hello-bg"), "got: {}", r.content);
        assert!(r.content.contains("exited"));
    }

    #[tokio::test]
    async fn background_shell_records_selected_execution_target() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
            crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let id = spawn_background_with_target(
            "echo target-bg".into(),
            dir.path(),
            &store,
            None,
            BgExecutionTarget {
                backend_id: Some("local".into()),
                workspace_id: Some("ws-target".into()),
                network_profile: Some("local-host".into()),
            },
        )
        .expect("admission");

        let target = store
            .lock()
            .unwrap()
            .get(&id)
            .expect("background shell should be registered")
            .execution_target();
        assert_eq!(target.backend_id.as_deref(), Some("local"));
        assert_eq!(target.workspace_id.as_deref(), Some("ws-target"));
        assert_eq!(target.network_profile.as_deref(), Some("local-host"));

        let _ = wait_until_done(&store, &id).await;
    }

    #[tokio::test]
    async fn background_program_records_display_command_and_target() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
            crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let (program, flag) = crate::coding_tools::shell::platform_shell();
        let id = spawn_background_program_with_target(
            program.to_string(),
            vec![flag.to_string(), "echo structured-bg".to_string()],
            "displayed command".to_string(),
            dir.path(),
            &store,
            None,
            BgExecutionTarget {
                backend_id: Some("local".into()),
                workspace_id: Some("ws-local".into()),
                network_profile: Some("local-host".into()),
            },
            true,
        )
        .expect("admission");

        let _ = wait_until_done(&store, &id).await;
        let shells = store.lock().unwrap();
        let shell = shells.get(&id).expect("background process should exist");
        assert_eq!(shell.command, "displayed command");
        let target = shell.execution_target();
        assert_eq!(target.backend_id.as_deref(), Some("local"));
        assert_eq!(target.workspace_id.as_deref(), Some("ws-local"));
        assert_eq!(target.network_profile.as_deref(), Some("local-host"));
    }

    #[tokio::test]
    async fn incremental_output_is_not_repeated() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
            crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let id = spawn_background("echo one".into(), dir.path(), &store, None).expect("admission");
        let st = wait_until_done(&store, &id).await;
        assert!(!matches!(st, BgStatus::Running), "status: {st:?}");
        let c = ctx_with_store(dir.path(), store.clone());
        let first = GetShellOutputTool
            .execute(json!({ "shell_id": id }), &c)
            .await;
        assert!(first.content.contains("one"));
        let second = GetShellOutputTool
            .execute(json!({ "shell_id": id }), &c)
            .await;
        assert!(!second.content.contains("one"), "output should not repeat");
        assert!(second.content.contains("no new output"));
    }

    #[tokio::test]
    async fn kill_stops_a_long_process() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let cmd = if cfg!(windows) {
            "Start-Sleep -Seconds 30".to_string()
        } else {
            "sleep 30".to_string()
        };
        let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
            crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let id = spawn_background(cmd, dir.path(), &store, None).expect("admission");
        let c = ctx_with_store(dir.path(), store.clone());
        let k = KillShellTool.execute(json!({ "shell_id": id }), &c).await;
        assert!(!k.is_error, "{}", k.content);
        let _ = wait_until_done(&store, &id).await;
        let st = store.lock().unwrap().get(&id).map(|s| s.status());
        assert!(matches!(st, Some(BgStatus::Killed)), "status: {st:?}");
    }

    #[tokio::test]
    async fn cancellation_token_stops_a_long_background_process() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let cmd = if cfg!(windows) {
            "Start-Sleep -Seconds 30".to_string()
        } else {
            "sleep 30".to_string()
        };
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
            crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let id = spawn_background(cmd, dir.path(), &store, Some(cancel_rx)).expect("admission");

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        cancel_tx.send(true).unwrap();
        let _ = wait_until_done(&store, &id).await;

        let st = store.lock().unwrap().get(&id).map(|s| s.status());
        assert!(matches!(st, Some(BgStatus::Killed)), "status: {st:?}");
    }

    #[test]
    fn exit_status_is_typed_and_absent_while_running() {
        use rightkit_agent_engine_protocol::ShellExitStatus;
        assert_eq!(exit_status_for(&BgStatus::Running), None);
        assert_eq!(exit_status_for(&BgStatus::Unknown), None);
        assert_eq!(
            exit_status_for(&BgStatus::Exited(3)),
            Some(ShellExitStatus::Exited { code: 3 })
        );
        assert_eq!(
            exit_status_for(&BgStatus::Killed),
            Some(ShellExitStatus::KilledByCancel)
        );
        assert_eq!(
            exit_status_for(&BgStatus::SpawnFailed("nope".into())),
            Some(ShellExitStatus::SpawnFailed {
                reason: "nope".into()
            })
        );
        #[cfg(unix)]
        assert_eq!(
            exit_status_for(&BgStatus::Exited(-1)),
            Some(ShellExitStatus::Signaled { signal: None })
        );
    }

    #[tokio::test]
    async fn exit_status_reports_code_only_after_reap_and_get_shell_output_still_works() {
        use rightkit_agent_engine_protocol::ShellExitStatus;
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let cmd = "exit 7";
        let id = spawn_background(cmd.into(), dir.path(), &store, None).expect("admission");
        let shell = store.lock().unwrap().get(&id).cloned().unwrap();
        // Whenever a typed status is visible, the run status is terminal: they
        // are one source, so a visible exit implies the child was reaped.
        for _ in 0..500 {
            if let Some(exit) = shell.exit_status() {
                assert!(!matches!(shell.status(), BgStatus::Running));
                assert_eq!(exit, ShellExitStatus::Exited { code: 7 });
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            shell.exit_status(),
            Some(ShellExitStatus::Exited { code: 7 })
        );
        let c = ctx_with_store(dir.path(), store.clone());
        let polled = GetShellOutputTool
            .execute(json!({ "shell_id": id }), &c)
            .await;
        assert!(!polled.is_error, "{}", polled.content);
        assert!(
            polled.content.contains("exited (code 7)"),
            "{}",
            polled.content
        );
    }

    #[tokio::test]
    async fn killed_background_shell_reports_killed_by_cancel() {
        use rightkit_agent_engine_protocol::ShellExitStatus;
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let cmd = if cfg!(windows) {
            "Start-Sleep -Seconds 30".to_string()
        } else {
            "sleep 30".to_string()
        };
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let id = spawn_background(cmd, dir.path(), &store, Some(cancel_rx)).expect("admission");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        cancel_tx.send(true).unwrap();
        let _ = wait_until_done(&store, &id).await;
        let shell = store.lock().unwrap().get(&id).cloned().unwrap();
        assert_eq!(shell.exit_status(), Some(ShellExitStatus::KilledByCancel));
    }

    /// Killing a background shell must take its grandchildren with it. Before
    /// process-group ownership this passed the status check while leaving the
    /// inner `sleep` running as an orphan.
    #[cfg(unix)]
    #[tokio::test]
    async fn kill_reaps_the_whole_process_tree() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let pid_file = dir.path().join("grandchild.pid");
        let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
            crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let id = spawn_background(
            format!("sleep 30 & echo $! > {}; wait", pid_file.display()),
            dir.path(),
            &store,
            None,
        )
        .expect("admission");

        let mut grandchild = None;
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = text.trim().parse::<i32>() {
                    grandchild = Some(pid);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let grandchild = grandchild.expect("grandchild pid was never recorded");
        // Guard against a gate that proves nothing: the pid must be live first.
        assert_eq!(
            unsafe { libc::kill(grandchild, 0) },
            0,
            "grandchild not running"
        );

        store.lock().unwrap().get(&id).unwrap().kill();
        let _ = wait_until_done(&store, &id).await;

        let mut alive = true;
        for _ in 0..40 {
            if unsafe { libc::kill(grandchild, 0) } != 0 {
                alive = false;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(!alive, "grandchild {grandchild} survived the kill");
    }

    #[tokio::test]
    async fn get_shell_output_since_offset_rereads_and_reports_offsets_and_exit() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let id =
            spawn_background("echo abcdef".into(), dir.path(), &store, None).expect("admission");
        let _ = wait_until_done(&store, &id).await;
        let c = ctx_with_store(dir.path(), store.clone());
        let first = GetShellOutputTool
            .execute(json!({ "shell_id": id }), &c)
            .await;
        assert!(!first.is_error, "{}", first.content);
        assert!(first.content.contains("abcdef"), "{}", first.content);
        assert!(first.content.contains("offset=0"), "{}", first.content);
        assert!(first.content.contains("more=false"), "{}", first.content);
        assert!(
            first.content.contains("\"state\":\"exited\""),
            "{}",
            first.content
        );
        // An explicit `since` re-reads from that offset, independent of the cursor.
        let again = GetShellOutputTool
            .execute(json!({ "shell_id": id, "since": 2 }), &c)
            .await;
        assert!(!again.is_error, "{}", again.content);
        assert!(again.content.contains("offset=2"), "{}", again.content);
        assert!(again.content.contains("cdef"), "{}", again.content);
        assert!(!again.content.contains("abcdef"), "{}", again.content);
    }

    #[tokio::test]
    async fn get_shell_output_max_bytes_bounds_each_read_and_continues() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let id = spawn_background("echo abcdefghij".into(), dir.path(), &store, None)
            .expect("admission");
        let _ = wait_until_done(&store, &id).await;
        let c = ctx_with_store(dir.path(), store.clone());
        let first = GetShellOutputTool
            .execute(json!({ "shell_id": id, "max_bytes": 4 }), &c)
            .await;
        assert!(first.content.contains("abcd"), "{}", first.content);
        assert!(!first.content.contains("abcde"), "{}", first.content);
        assert!(first.content.contains("next_offset=4"), "{}", first.content);
        assert!(first.content.contains("more=true"), "{}", first.content);
        // The cursor advanced only by what was returned: the rest follows.
        let second = GetShellOutputTool
            .execute(json!({ "shell_id": id, "max_bytes": 4 }), &c)
            .await;
        assert!(second.content.contains("offset=4"), "{}", second.content);
        assert!(second.content.contains("efgh"), "{}", second.content);
    }

    #[tokio::test]
    async fn get_shell_output_rejects_bad_arguments_and_offsets_past_the_end() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let id = spawn_background("echo hi".into(), dir.path(), &store, None).expect("admission");
        let _ = wait_until_done(&store, &id).await;
        let c = ctx_with_store(dir.path(), store.clone());
        let past = GetShellOutputTool
            .execute(json!({ "shell_id": id, "since": 100000 }), &c)
            .await;
        assert!(
            past.is_error && past.content.contains("beyond the end"),
            "{}",
            past.content
        );
        let zero = GetShellOutputTool
            .execute(json!({ "shell_id": id, "max_bytes": 0 }), &c)
            .await;
        assert!(zero.is_error, "{}", zero.content);
        let bad = GetShellOutputTool
            .execute(json!({ "shell_id": id, "since": "x" }), &c)
            .await;
        assert!(bad.is_error, "{}", bad.content);
        let missing = GetShellOutputTool.execute(json!({}), &c).await;
        assert!(missing.is_error, "{}", missing.content);
    }

    #[tokio::test]
    async fn kill_shell_waits_for_reap_and_returns_killed_by_cancel() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let cmd = if cfg!(windows) {
            "Start-Sleep -Seconds 30".to_string()
        } else {
            "sleep 30".to_string()
        };
        let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
            crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let id = spawn_background(cmd, dir.path(), &store, None).expect("admission");
        let c = ctx_with_store(dir.path(), store.clone());
        let k = KillShellTool.execute(json!({ "shell_id": id }), &c).await;
        assert!(!k.is_error, "{}", k.content);
        assert!(k.content.contains("killedByCancel"), "{}", k.content);
        // The result is only returned after reap: status is already terminal.
        let shell = store.lock().unwrap().get(&id).cloned().unwrap();
        assert!(!matches!(shell.status(), BgStatus::Running));
        // A second kill reports the finished state instead of failing.
        let again = KillShellTool.execute(json!({ "shell_id": id }), &c).await;
        assert!(!again.is_error, "{}", again.content);
        assert!(
            again.content.contains("Already finished"),
            "{}",
            again.content
        );
    }

    #[tokio::test]
    async fn kill_shell_unknown_id_and_missing_argument_are_clear_errors() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let c = ctx_with_store(dir.path(), store);
        let unknown = KillShellTool
            .execute(json!({ "shell_id": "nope" }), &c)
            .await;
        assert!(unknown.is_error);
        assert!(
            unknown
                .content
                .contains("No background shell with id 'nope'"),
            "{}",
            unknown.content
        );
        let missing = KillShellTool.execute(json!({}), &c).await;
        assert!(
            missing.is_error && missing.content.contains("shell_id"),
            "{}",
            missing.content
        );
        let r = GetShellOutputTool
            .execute(json!({ "shell_id": "nope" }), &c)
            .await;
        assert!(
            r.is_error && r.content.contains("No background shell with id 'nope'"),
            "{}",
            r.content
        );
    }

    /// Windows counterpart of `kill_reaps_the_whole_process_tree`: the job
    /// object must take the grandchild down with the shell.
    #[cfg(windows)]
    #[tokio::test]
    async fn kill_reaps_the_whole_process_tree_on_windows() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let pid_file = dir.path().join("grandchild.pid");
        let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
            crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let script = format!(
            "$p = Start-Process -PassThru -NoNewWindow powershell -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 30'; \
             Set-Content -Path '{}' -Value $p.Id; Wait-Process -Id $p.Id",
            pid_file.display()
        );
        let id = spawn_background(script, dir.path(), &store, None).expect("admission");
        let mut grandchild = None;
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    grandchild = Some(pid);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let grandchild = grandchild.expect("grandchild pid was never recorded");
        let is_alive = |pid: u32| {
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            out.contains(&pid.to_string())
        };
        assert!(is_alive(grandchild), "grandchild not running");
        let c = ctx_with_store(dir.path(), store.clone());
        let k = KillShellTool.execute(json!({ "shell_id": id }), &c).await;
        assert!(!k.is_error, "{}", k.content);
        let mut alive = true;
        for _ in 0..40 {
            if !is_alive(grandchild) {
                alive = false;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(!alive, "grandchild {grandchild} survived the kill");
    }

    #[tokio::test]
    async fn unknown_id_errors() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let r = GetShellOutputTool
            .execute(
                json!({ "shell_id": "nope" }),
                &ctx_with_store(dir.path(), store),
            )
            .await;
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn background_shell_process_inventory_supports_list_get_and_reattach() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
            crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let id =
            spawn_background("echo inventory".into(), dir.path(), &store, None).expect("admission");
        let _ = wait_until_done(&store, &id).await;

        let first_ctx = ctx_with_store(dir.path(), store.clone());
        let listed = ProcessTool
            .execute(json!({"operation":"list"}), &first_ctx)
            .await;
        assert!(!listed.is_error, "{}", listed.content);
        assert!(listed.content.contains(&id));

        // A new context simulates a session reconnect. The daemon-owned store
        // remains authoritative and the process handle can be reattached.
        let reconnect_ctx = ctx_with_store(dir.path(), store);
        let reattached = ProcessTool
            .execute(
                json!({"operation":"reattach", "process_id":id}),
                &reconnect_ctx,
            )
            .await;
        assert!(!reattached.is_error, "{}", reattached.content);
        assert!(reattached.content.contains("reattached"));
    }

    #[tokio::test]
    async fn process_reattach_fences_clean_lease_and_rejects_dirty_lease() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let shell = Arc::new(BgShell {
            command: "true".into(),
            process_kind: "test".into(),
            execution_target: BgExecutionTarget::default(),
            pid: Mutex::new(None),
            started_at_ns: 0,
            output: Mutex::new(BoundedOutput::default()),
            cursor: Mutex::new(0),
            status: Mutex::new(BgStatus::Exited(0)),
            kill_tx: Mutex::new(None),
            stdin: tokio::sync::Mutex::new(None),
            channel: Mutex::new(rightkit_sandbox::execution::ExecutionChannel::Pipe),
            control_tx: None,
            persistence: ProcessPersistence::Service,
            lease: tokio::sync::Mutex::new(ProcessLease {
                epoch: 7,
                next_input_sequence: 0,
                last_acked_input_sequence: 0,
                attached: false,
            }),
            human: Default::default(),
        });
        let id = "reattach-regression".to_string();
        store.lock().unwrap().insert(id.clone(), shell.clone());
        let ctx = ctx_with_store(dir.path(), store);

        let clean = ProcessTool
            .execute(json!({"operation":"reattach", "process_id":id}), &ctx)
            .await;
        assert!(!clean.is_error, "{}", clean.content);
        assert_eq!(shell.lease_snapshot_now().epoch, 8);
        assert!(shell.lease_snapshot_now().attached);

        {
            let mut lease = shell.lease.lock().await;
            lease.next_input_sequence = 4;
            lease.last_acked_input_sequence = 2;
            lease.attached = false;
        }
        let dirty = ProcessTool
            .execute(json!({"operation":"reattach", "process_id":id}), &ctx)
            .await;
        assert!(
            dirty.is_error,
            "dirty lease must require explicit reconciliation"
        );
        let snapshot = shell.lease_snapshot().await;
        assert_eq!(
            snapshot.epoch, 8,
            "rejected reattach must not fence stale lease"
        );
        assert_eq!(snapshot.unacknowledged_input, Some((2, 4)));
        assert!(!snapshot.attached);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn background_shell_process_input_and_stop_are_typed() {
        let dir = tempdir().unwrap();
        let store: BgStore = Arc::new(Mutex::new(HashMap::new()));
        let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
            crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let id = spawn_background(
            "read line; echo got:$line; sleep 30".into(),
            dir.path(),
            &store,
            None,
        )
        .expect("admission");
        let ctx = ctx_with_store(dir.path(), store.clone());
        let input = ProcessTool
            .execute(
                json!({"operation":"input", "process_id":id, "input":"hello\n", "lease_epoch":1, "input_sequence":0}),
                &ctx,
            )
            .await;
        assert!(!input.is_error, "{}", input.content);
        for _ in 0..100 {
            if store
                .lock()
                .unwrap()
                .get(&id)
                .unwrap()
                .output_snapshot()
                .contains("got:hello")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stopped = ProcessTool
            .execute(json!({"operation":"stop", "process_id":id}), &ctx)
            .await;
        assert!(!stopped.is_error, "{}", stopped.content);
        assert!(matches!(
            wait_until_done(&store, &id).await,
            BgStatus::Killed
        ));
    }

    #[test]
    fn background_shell_output_snapshot_is_bounded() {
        let shell = BgShell {
            command: "bounded".into(),
            process_kind: "backgroundShell".into(),
            execution_target: BgExecutionTarget::default(),
            pid: Mutex::new(None),
            started_at_ns: 0,
            output: Mutex::new(BoundedOutput::default()),
            cursor: Mutex::new(0),
            status: Mutex::new(BgStatus::Exited(0)),
            kill_tx: Mutex::new(None),
            stdin: tokio::sync::Mutex::new(None),
            channel: Mutex::new(rightkit_sandbox::execution::ExecutionChannel::Pipe),
            control_tx: None,
            persistence: ProcessPersistence::Inline,
            lease: tokio::sync::Mutex::new(ProcessLease {
                epoch: 1,
                next_input_sequence: 0,
                last_acked_input_sequence: 0,
                attached: true,
            }),
            human: Default::default(),
        };
        shell.append(&vec![b'x'; rightkit_agent_engine_runtime_resources::RETAINED_BYTES * 4]);
        let snapshot = shell.output_snapshot();
        assert!(snapshot.len() <= rightkit_agent_engine_runtime_resources::RETAINED_BYTES + 64);
        assert!(snapshot.contains("output truncated"));
    }

    fn shared_pty_shell() -> Arc<BgShell> {
        Arc::new(BgShell {
            command: "shared".into(),
            process_kind: "test".into(),
            execution_target: BgExecutionTarget::default(),
            pid: Mutex::new(None),
            started_at_ns: 0,
            output: Mutex::new(BoundedOutput::default()),
            cursor: Mutex::new(0),
            // Not running: a write that really reaches the process errors with
            // "execution process is not running", a queued one does not.
            status: Mutex::new(BgStatus::Exited(0)),
            kill_tx: Mutex::new(None),
            stdin: tokio::sync::Mutex::new(None),
            channel: Mutex::new(rightkit_sandbox::execution::ExecutionChannel::Pipe),
            control_tx: None,
            persistence: ProcessPersistence::Inline,
            lease: tokio::sync::Mutex::new(ProcessLease {
                epoch: 1,
                next_input_sequence: 0,
                last_acked_input_sequence: 0,
                attached: true,
            }),
            human: Default::default(),
        })
    }

    #[tokio::test]
    async fn agent_input_goes_straight_through_when_no_human_is_attached() {
        let shell = shared_pty_shell();
        let result = shell.write_agent_input_with_lease(b"ls\r", 1, 0).await;
        assert_eq!(
            result.unwrap_err(),
            "execution process is not running",
            "detached: the write must be attempted immediately, not queued"
        );
        assert_eq!(shell.queued_agent_bytes(), 0);
        // An attached but idle human does not block the agent either.
        shell.set_human_attached(true);
        let result = shell.write_agent_input_with_lease(b"ls\r", 1, 0).await;
        assert_eq!(result.unwrap_err(), "execution process is not running");
    }

    #[tokio::test]
    async fn agent_input_is_queued_while_the_human_types_then_delivered_after_idle() {
        let shell = shared_pty_shell();
        shell.set_human_attached(true);
        shell.note_human_input();
        let outcome = shell
            .write_agent_input_with_lease(b"rm x\r", 1, 0)
            .await
            .expect("queued, not an error");
        match outcome {
            AgentInputOutcome::Queued {
                lease,
                queued_bytes,
            } => {
                assert_eq!(queued_bytes, 5);
                assert_eq!(lease.next_input_sequence, 1, "sequence is reserved");
            }
            other => panic!("expected Queued, got {other:?}"),
        }
        assert_eq!(shell.queued_agent_bytes(), 5);
        // The human keeps typing: still held.
        tokio::time::sleep(HUMAN_IDLE / 3).await;
        shell.note_human_input();
        tokio::time::sleep(HUMAN_IDLE / 2).await;
        assert_eq!(
            shell.queued_agent_bytes(),
            5,
            "human typed again; keep holding"
        );
        // Idle long enough: the drain task takes the queue (the dead process
        // then drops it, but it was attempted rather than held forever).
        tokio::time::sleep(HUMAN_IDLE + std::time::Duration::from_millis(400)).await;
        assert_eq!(
            shell.queued_agent_bytes(),
            0,
            "delivered after the human went idle"
        );
    }

    #[tokio::test]
    async fn agent_input_is_refused_beyond_the_queue_bound() {
        let shell = shared_pty_shell();
        shell.set_human_attached(true);
        shell.note_human_input();
        let big = vec![b'x'; MAX_AGENT_QUEUE_BYTES];
        shell
            .write_agent_input_with_lease(&big, 1, 0)
            .await
            .expect("fills the queue");
        let refused = shell
            .write_agent_input_with_lease(b"y", 1, 1)
            .await
            .unwrap_err();
        assert!(refused.contains("input refused"), "{refused}");
        assert!(refused.contains("nothing was sent"), "{refused}");
        assert_eq!(shell.queued_agent_bytes(), MAX_AGENT_QUEUE_BYTES);
    }

    #[test]
    fn human_idle_window_is_measured_from_the_last_human_input() {
        let mut human = HumanInput::default();
        let t0 = std::time::Instant::now();
        assert!(human.wait_until_idle(t0).is_zero(), "no human attached");
        human.attached = true;
        assert!(
            human.wait_until_idle(t0).is_zero(),
            "attached but never typed"
        );
        human.last_input = Some(t0);
        assert_eq!(human.wait_until_idle(t0), HUMAN_IDLE);
        assert!(human.wait_until_idle(t0 + HUMAN_IDLE).is_zero());
        human.attached = false;
        assert!(
            human.wait_until_idle(t0).is_zero(),
            "detached releases at once"
        );
    }

    #[test]
    fn background_shell_stop_declares_process_control() {
        assert!(!KillShellTool.is_read_only());
        assert_eq!(
            KillShellTool.capability_class(),
            Some(rightkit_agent_policy::CapabilityClass::ProcessControl)
        );
        assert_eq!(
            ProcessTool.capability_class_for(&json!({"operation":"stop"})),
            Some(rightkit_agent_policy::CapabilityClass::ProcessControl)
        );
        assert_eq!(
            ProcessTool.capability_class_for(&json!({"operation":"list"})),
            Some(rightkit_agent_policy::CapabilityClass::ReadFile)
        );
    }
}
