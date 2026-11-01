#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_tools::context::{SubagentSpawnOptions, SubagentSpawner};
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use std::pin::Pin;
    use tempfile::tempdir;

    fn ctx(root: &std::path::Path) -> ToolContext {
        ToolContext::new("s1", root, PermissionMode::Bypass, WorkspaceTrust::Trusted)
    }

    fn ctx_with_shells(
        root: &std::path::Path,
        store: crate::coding_tools::background_shell::BgStore,
    ) -> ToolContext {
        let _ = crate::coding_tools::process_resource_authority::install_process_resource_profile(
            crate::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        ToolContext::new("s1", root, PermissionMode::Bypass, WorkspaceTrust::Trusted)
            .with_background_shells(store)
            .with_task_store(Arc::new(Mutex::new(HashMap::new())))
    }

    #[derive(Default)]
    struct RecordingSpawner {
        calls: Mutex<Vec<(String, String, String)>>,
    }

    impl SubagentSpawner for RecordingSpawner {
        fn spawn(
            &self,
            role: &str,
            task: &str,
            parent_session_id: &str,
            _options: SubagentSpawnOptions,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
            self.calls.lock().unwrap().push((
                role.to_string(),
                task.to_string(),
                parent_session_id.to_string(),
            ));
            Box::pin(async { Ok("subagent finished".to_string()) })
        }
    }

    #[tokio::test]
    async fn task_records_persist_between_tool_instances() {
        let dir = tempdir().unwrap();
        let created = TaskCreateTool
            .execute(
                json!({"title": "Investigate failing build"}),
                &ctx(dir.path()),
            )
            .await;
        assert!(!created.is_error, "{}", created.content);
        assert!(created.content.contains("Investigate failing build"));

        let listed = TaskListTool.execute(json!({}), &ctx(dir.path())).await;
        assert!(!listed.is_error, "{}", listed.content);
        assert!(listed.content.contains("Investigate failing build"));
    }

    #[tokio::test]
    async fn shell_task_output_polls_background_process() {
        let dir = tempdir().unwrap();
        let shell_store: crate::coding_tools::background_shell::BgStore = Arc::new(Mutex::new(HashMap::new()));
        let ctx = ctx_with_shells(dir.path(), shell_store);
        let command = if cfg!(windows) {
            "Write-Output task-hello".to_string()
        } else {
            "echo task-hello".to_string()
        };
        let created = TaskCreateTool
            .execute(json!({"title": "Run echo", "command": command}), &ctx)
            .await;
        assert!(!created.is_error, "{}", created.content);
        assert!(created.content.contains("shell_id:"));
        let id = created
            .content
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_string();

        for _ in 0..100 {
            let out = TaskOutputTool.execute(json!({"id": id}), &ctx).await;
            assert!(!out.is_error, "{}", out.content);
            if out.content.contains("task-hello") {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("background task output never appeared");
    }

    #[tokio::test]
    async fn shell_task_records_execution_target_on_background_shell() {
        let dir = tempdir().unwrap();
        let shell_store: crate::coding_tools::background_shell::BgStore = Arc::new(Mutex::new(HashMap::new()));
        let ctx = ctx_with_shells(dir.path(), shell_store.clone())
            .with_execution_backend("local")
            .with_execution_workspace_id("task-workspace")
            .with_execution_network_profile("local-host");
        let command = if cfg!(windows) {
            "Write-Output task-target".to_string()
        } else {
            "echo task-target".to_string()
        };

        let created = TaskCreateTool
            .execute(
                json!({"title": "Run target echo", "command": command}),
                &ctx,
            )
            .await;

        assert!(!created.is_error, "{}", created.content);
        let shell_id = created
            .content
            .lines()
            .find_map(|line| line.strip_prefix("shell_id: "))
            .expect("shell_id should be rendered")
            .to_string();
        let target = shell_store
            .lock()
            .unwrap()
            .get(&shell_id)
            .expect("shell should be live")
            .execution_target();
        assert_eq!(target.backend_id.as_deref(), Some("local"));
        assert_eq!(target.workspace_id.as_deref(), Some("task-workspace"));
        assert_eq!(target.network_profile.as_deref(), Some("local-host"));
    }

    #[tokio::test]
    async fn shell_task_rejects_nonlocal_execution_backend_before_host_spawn() {
        let dir = tempdir().unwrap();
        let marker = dir.path().join(concat!("task", "-nonlocal-ran.txt"));
        let shell_store: crate::coding_tools::background_shell::BgStore = Arc::new(Mutex::new(HashMap::new()));
        let ctx = ctx_with_shells(dir.path(), shell_store.clone())
            .with_execution_backend("missing-backend");
        let command = if cfg!(windows) {
            concat!("Set-Content -Path task", "-nonlocal-ran.txt -Value task-ran").to_string()
        } else {
            concat!("printf task-ran > task", "-nonlocal-ran.txt").to_string()
        };

        let created = TaskCreateTool
            .execute(
                json!({"title": "Run backend task", "command": command}),
                &ctx,
            )
            .await;

        assert!(created.is_error);
        assert!(
            created
                .content
                .contains("not routed through the selected non-local backend"),
            "{}",
            created.content
        );
        assert!(
            created.content.contains("shell(run_in_background=true)"),
            "{}",
            created.content
        );
        assert!(
            shell_store.lock().unwrap().is_empty(),
            "non-local task_create command must not spawn a host background shell"
        );
        assert!(
            !marker.exists(),
            "non-local task_create command must not execute on the host"
        );
    }

    #[tokio::test]
    async fn task_update_changes_state() {
        let dir = tempdir().unwrap();
        let created = TaskCreateTool
            .execute(json!({"title": "Review docs"}), &ctx(dir.path()))
            .await;
        let id = created
            .content
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_string();

        let updated = TaskUpdateTool
            .execute(json!({"id": id, "state": "done"}), &ctx(dir.path()))
            .await;
        assert!(!updated.is_error, "{}", updated.content);
        assert!(updated.content.contains("Done"));
    }

    #[tokio::test]
    async fn task_stop_does_not_mutate_plain_records() {
        let dir = tempdir().unwrap();
        let created = TaskCreateTool
            .execute(json!({"title": "Track manual follow-up"}), &ctx(dir.path()))
            .await;
        let id = created
            .content
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_string();

        let stopped = TaskStopTool
            .execute(json!({"id": id.clone()}), &ctx(dir.path()))
            .await;

        assert!(stopped.is_error);
        assert!(stopped
            .content
            .contains("only supports active shell-backed tasks"));
        let fetched = TaskGetTool
            .execute(json!({"id": id}), &ctx(dir.path()))
            .await;
        assert!(!fetched.is_error, "{}", fetched.content);
        assert!(fetched.content.contains("[record Open]"));
    }

    #[tokio::test]
    async fn task_stop_does_not_mutate_terminal_tasks() {
        let dir = tempdir().unwrap();
        let created = TaskCreateTool
            .execute(json!({"title": "Finished work"}), &ctx(dir.path()))
            .await;
        let id = created
            .content
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_string();
        let updated = TaskUpdateTool
            .execute(json!({"id": id.clone(), "state": "done"}), &ctx(dir.path()))
            .await;
        assert!(!updated.is_error, "{}", updated.content);

        let stopped = TaskStopTool
            .execute(json!({"id": id.clone()}), &ctx(dir.path()))
            .await;

        assert!(!stopped.is_error, "{}", stopped.content);
        assert!(stopped.content.contains("already Done"));
        let fetched = TaskGetTool
            .execute(json!({"id": id}), &ctx(dir.path()))
            .await;
        assert!(!fetched.is_error, "{}", fetched.content);
        assert!(fetched.content.contains("[record Done]"));
    }

    #[tokio::test]
    async fn task_stop_rejects_detached_shell_without_state_change() {
        let dir = tempdir().unwrap();
        let record = TaskRecord {
            node: TaskNode {
                id: concat!("task", "-detached-stop").into(),
                title: "Detached shell".into(),
                deps: Vec::new(),
                done_when: String::new(),
                done_when_predicates: Vec::new(),
                state: TaskState::InProgress,
                category: "verification".into(),
                estimated_duration: String::new(),
                assigned_role: String::new(),
                escalation_path: String::new(),
                open_questions: Vec::new(),
                created_at: "t1".into(),
                updated_at: "t2".into(),
            },
            kind: "shell".into(),
            command: Some("cargo test".into()),
            shell_id: Some("bg-missing".into()),
            output: "stored line\n".into(),
        };
        let mut records = HashMap::new();
        records.insert(record.node.id.clone(), record);
        save_records(&ctx(dir.path()), &records).unwrap();

        let stopped = TaskStopTool
            .execute(json!({"id": concat!("task", "-detached-stop")}), &ctx(dir.path()))
            .await;

        assert!(stopped.is_error);
        assert!(stopped.content.contains("not attached"));
        let fetched = TaskGetTool
            .execute(json!({"id": concat!("task", "-detached-stop")}), &ctx(dir.path()))
            .await;
        assert!(!fetched.is_error, "{}", fetched.content);
        assert!(fetched.content.contains("[shell InProgress]"));
    }

    #[tokio::test]
    async fn shell_task_output_returns_stored_output_when_shell_is_detached() {
        let dir = tempdir().unwrap();
        let record = TaskRecord {
            node: TaskNode {
                id: "task-detached".into(),
                title: "Detached shell".into(),
                deps: Vec::new(),
                done_when: "historical output visible".into(),
                done_when_predicates: Vec::new(),
                state: TaskState::InProgress,
                category: "verification".into(),
                estimated_duration: String::new(),
                assigned_role: "coder".into(),
                escalation_path: String::new(),
                open_questions: Vec::new(),
                created_at: "t1".into(),
                updated_at: "t2".into(),
            },
            kind: "shell".into(),
            command: Some("cargo test".into()),
            shell_id: Some("bg-missing".into()),
            output: "stored line\n".into(),
        };
        let mut records = HashMap::new();
        records.insert(record.node.id.clone(), record);
        save_records(&ctx(dir.path()), &records).unwrap();

        let out = TaskOutputTool
            .execute(json!({"id": "task-detached"}), &ctx(dir.path()))
            .await;

        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("stored line"));
        assert!(out.content.contains("[detached]"));
        assert!(out.content.contains("stored task output only"));
    }

    #[tokio::test]
    async fn local_agent_task_spawns_subagent_and_persists_result() {
        let dir = tempdir().unwrap();
        let spawner = Arc::new(RecordingSpawner::default());
        let ctx = ctx(dir.path()).with_spawner(spawner.clone());

        let created = TaskCreateTool
            .execute(
                json!({
                    "title": "Review parser",
                    "kind": "local_agent",
                    "assigned_role": "reviewer",
                    "task": "Inspect parser changes"
                }),
                &ctx,
            )
            .await;

        assert!(!created.is_error, "{}", created.content);
        assert!(created.content.contains("[local_agent Done]"));
        assert!(created.content.contains("subagent finished"));
        let calls = spawner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "reviewer");
        assert_eq!(calls[0].1, "Inspect parser changes");
        assert_eq!(calls[0].2, "s1");

        let listed = TaskListTool.execute(json!({}), &ctx).await;
        assert!(!listed.is_error, "{}", listed.content);
        assert!(listed.content.contains("[local_agent Done]"));
    }

    #[tokio::test]
    async fn local_agent_task_requires_spawner_context() {
        let dir = tempdir().unwrap();
        let created = TaskCreateTool
            .execute(
                json!({
                    "title": "Review parser",
                    "kind": "local_agent",
                    "assigned_role": "reviewer"
                }),
                &ctx(dir.path()),
            )
            .await;

        assert!(created.is_error);
        assert!(created.content.contains("requires sub-agent orchestration"));
    }
}
