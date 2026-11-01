#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::{ContentOrigin, ContentProvenance, PermissionMode, WorkspaceTrust};

    #[test]
    fn only_schedule_mutations_declare_a_durable_write() {
        let tool = ScheduleCronTool::new();
        assert_eq!(
            tool.durable_write_kind(&json!({"operation": "register"})),
            Some(rightkit_agent_policy::DurableWriteKind::Schedule)
        );
        assert_eq!(
            tool.durable_write_kind(&json!({"operation": "cancel"})),
            Some(rightkit_agent_policy::DurableWriteKind::Schedule)
        );
        assert_eq!(tool.durable_write_kind(&json!({"operation": "list"})), None);
    }

    #[tokio::test]
    async fn external_schedule_mutation_needs_per_call_approval_but_list_does_not() {
        let tool = ScheduleCronTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            "test",
            temp_dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_content_provenance(ContentProvenance::new(
            ContentOrigin::WebDerived,
            "browser_fetch",
        ));

        let rejected = tool
            .execute(
                json!({"operation": "register", "cron_expr": "0 9 * * *", "prompt": "persist this"}),
                &ctx,
            )
            .await;
        assert!(rejected.is_error, "{}", rejected.content);
        assert!(rejected.content.contains("requires explicit approval"));
        assert!(!schedules_path(&ctx).exists());

        let listed = tool.execute(json!({"operation": "list"}), &ctx).await;
        assert!(!listed.is_error, "{}", listed.content);
    }

    #[tokio::test]
    async fn test_register_schedule() {
        let tool = ScheduleCronTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            "test",
            temp_dir.path(),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let result = tool
            .execute(
                json!({"operation": "register", "cron_expr": "*/5 * * * *", "prompt": "test prompt"}),
                &ctx,
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("schedule_"));
    }

    #[tokio::test]
    async fn test_invalid_cron() {
        let tool = ScheduleCronTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            "test",
            temp_dir.path(),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let result = tool
            .execute(
                json!({"operation": "register", "cron_expr": "* * * *", "prompt": "test"}),
                &ctx,
            )
            .await;

        assert!(result.is_error);
    }

    #[tokio::test]
    async fn test_list_schedules() {
        let tool = ScheduleCronTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            "test",
            temp_dir.path(),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let _ = tool
            .execute(
                json!({"operation": "register", "cron_expr": "0 9 * * *", "prompt": "morning"}),
                &ctx,
            )
            .await;

        let result = tool.execute(json!({"operation": "list"}), &ctx).await;

        assert!(!result.is_error);
        assert!(result.content.contains("morning"));
    }

    #[tokio::test]
    async fn test_schedules_persist_in_workspace_between_tool_instances() {
        let temp_dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            "test",
            temp_dir.path(),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let first = ScheduleCronTool::new();
        let registered = first
            .execute(
                json!({"operation": "register", "cron_expr": "0 9 * * *", "prompt": "morning"}),
                &ctx,
            )
            .await;
        assert!(!registered.is_error);

        let second = ScheduleCronTool::new();
        let listed = second.execute(json!({"operation": "list"}), &ctx).await;

        assert!(!listed.is_error);
        assert!(listed.content.contains("morning"));
        assert!(listed.content.contains("0 9 * * *"));
    }

    #[tokio::test]
    async fn scheduled_channel_requires_schedule_admin_for_register() {
        let tool = ScheduleCronTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            "schedule-test",
            temp_dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_mcp_channel("schedule")
        .with_toolset("full");

        let result = tool
            .execute(
                json!({"operation": "register", "cron_expr": "0 9 * * *", "prompt": "morning"}),
                &ctx,
            )
            .await;

        assert!(result.is_error);
        assert!(
            result.content.contains("schedule-admin"),
            "{}",
            result.content
        );
        assert!(
            !schedules_path(&ctx).exists(),
            "non-admin scheduled session must not create schedules.json"
        );
    }

    #[tokio::test]
    async fn scheduled_channel_requires_schedule_admin_for_cancel() {
        let tool = ScheduleCronTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let admin_ctx = ToolContext::new(
            "schedule-test",
            temp_dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_mcp_channel("schedule")
        .with_toolset(SCHEDULE_ADMIN_TOOLSET);
        let registered = tool
            .execute(
                json!({"operation": "register", "cron_expr": "0 9 * * *", "prompt": "morning"}),
                &admin_ctx,
            )
            .await;
        assert!(!registered.is_error, "{}", registered.content);
        let raw_before = std::fs::read_to_string(schedules_path(&admin_ctx)).unwrap();
        let schedule_id = raw_before
            .split('"')
            .find(|part| part.starts_with("schedule_"))
            .expect("schedule id")
            .to_string();

        let non_admin_ctx = ToolContext::new(
            "schedule-test",
            temp_dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_mcp_channel("schedule")
        .with_toolset("full");
        let result = tool
            .execute(
                json!({"operation": "cancel", "id": schedule_id}),
                &non_admin_ctx,
            )
            .await;

        assert!(result.is_error);
        assert!(
            result.content.contains("schedule-admin"),
            "{}",
            result.content
        );
        let raw_after = std::fs::read_to_string(schedules_path(&non_admin_ctx)).unwrap();
        assert_eq!(raw_after, raw_before);
    }

    #[tokio::test]
    async fn scheduled_channel_allows_schedule_admin_register() {
        let tool = ScheduleCronTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            "schedule-test",
            temp_dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_mcp_channel("schedule")
        .with_toolset(SCHEDULE_ADMIN_TOOLSET);

        let result = tool
            .execute(
                json!({"operation": "register", "cron_expr": "0 9 * * *", "prompt": "morning"}),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "{}", result.content);
        let raw = std::fs::read_to_string(schedules_path(&ctx)).unwrap();
        assert!(raw.contains("morning"), "{raw}");
    }

    #[test]
    fn normalize_time_zone_accepts_common_iana_names() {
        assert_eq!(
            normalize_time_zone("america/new_york").unwrap(),
            "America/New_York"
        );
        assert_eq!(
            normalize_time_zone("Asia/Calcutta").unwrap(),
            "Asia/Kolkata"
        );
        assert_eq!(normalize_time_zone("UTC+5:30").unwrap(), "UTC+05:30");
    }

    #[test]
    fn iana_time_zone_offsets_respect_dst_rules() {
        let january_noon_utc = epoch_minute_from_civil(2026, 1, 15, 12, 0);
        let july_noon_utc = epoch_minute_from_civil(2026, 7, 15, 12, 0);

        assert_eq!(
            time_zone_offset_minutes_at("America/New_York", january_noon_utc).unwrap(),
            -5 * 60
        );
        assert_eq!(
            time_zone_offset_minutes_at("America/New_York", july_noon_utc).unwrap(),
            -4 * 60
        );
        assert_eq!(
            time_zone_offset_minutes_at("Europe/London", january_noon_utc).unwrap(),
            0
        );
        assert_eq!(
            time_zone_offset_minutes_at("Europe/London", july_noon_utc).unwrap(),
            60
        );
    }

    #[tokio::test]
    async fn register_schedule_persists_iana_time_zone() {
        let tool = ScheduleCronTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            "test",
            temp_dir.path(),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let result = tool
            .execute(
                json!({
                    "operation": "register",
                    "cron_expr": "0 9 * * *",
                    "prompt": "morning",
                    "time_zone": "US/Eastern"
                }),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "{}", result.content);
        let raw = std::fs::read_to_string(schedules_path(&ctx)).unwrap();
        assert!(raw.contains("America/New_York"), "{raw}");
    }

    #[tokio::test]
    async fn register_schedule_persists_job_metadata() {
        let tool = ScheduleCronTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        // Bypass mode: this test asserts allowPrivateNetwork persistence, not the
        // permission gate itself (see the dedicated allow_private_network_* tests
        // below) — Plan/Ask modes would reject the Network-class approval needed
        // for allowPrivateNetwork: true and never reach the persisted-entry assert.
        let ctx = ToolContext::new(
            "test",
            temp_dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        );

        let result = tool
            .execute(
                json!({
                    "operation": "register",
                    "cron_expr": "0 9 * * *",
                    "prompt": "morning",
                    "skills": ["review", "qa"],
                    "workdir": temp_dir.path().to_string_lossy(),
                    "toolset": "review",
                    "execution_backend": "local",
                    "delivery": [{
                        "kind": "session_message",
                        "target": "s1",
                        "allowPrivateNetwork": true
                    }],
                    "mode": "agent"
                }),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "{}", result.content);
        let raw = std::fs::read_to_string(schedules_path(&ctx)).unwrap();
        let schedules: HashMap<String, ScheduleEntry> = serde_json::from_str(&raw).unwrap();
        let entry = schedules.values().next().unwrap();
        assert_eq!(entry.skills, vec!["review", "qa"]);
        assert_eq!(entry.toolset.as_deref(), Some("review"));
        assert_eq!(entry.execution_backend.as_deref(), Some("local"));
        assert_eq!(entry.delivery[0].kind, "session_message");
        assert!(entry.delivery[0].allow_private_network);
        assert_eq!(entry.mode, "agent");
        assert!(!entry.paused);
    }

    #[tokio::test]
    async fn register_schedule_rejects_allow_private_network_without_approval() {
        // Ask mode: a caller-supplied allowPrivateNetwork=true must not silently
        // bypass the net_guard SSRF check — it needs an explicit approval decision.
        let tool = ScheduleCronTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            "test",
            temp_dir.path(),
            PermissionMode::Ask,
            WorkspaceTrust::Trusted,
        );

        let result = tool
            .execute(
                json!({
                    "operation": "register",
                    "cron_expr": "0 9 * * *",
                    "prompt": "morning",
                    "delivery": [{
                        "kind": "webhook",
                        "target": "http://internal.example/hook",
                        "allowPrivateNetwork": true
                    }]
                }),
                &ctx,
            )
            .await;

        assert!(result.is_error, "{}", result.content);
        assert!(
            result.content.contains("allowPrivateNetwork") || result.content.contains("approval"),
            "{}",
            result.content
        );
        assert!(
            !schedules_path(&ctx).exists(),
            "no schedule should be persisted when approval is denied"
        );
    }

    #[tokio::test]
    async fn register_schedule_allows_allow_private_network_with_bypass_approval() {
        // Bypass mode is the explicit approval channel (mirrors write_file/shell's
        // own destructive-escalation contract) — allowPrivateNetwork=true proceeds.
        let tool = ScheduleCronTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            "test",
            temp_dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        );

        let result = tool
            .execute(
                json!({
                    "operation": "register",
                    "cron_expr": "0 9 * * *",
                    "prompt": "morning",
                    "delivery": [{
                        "kind": "webhook",
                        "target": "http://internal.example/hook",
                        "allowPrivateNetwork": true
                    }]
                }),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "{}", result.content);
        let raw = std::fs::read_to_string(schedules_path(&ctx)).unwrap();
        let schedules: HashMap<String, ScheduleEntry> = serde_json::from_str(&raw).unwrap();
        let entry = schedules.values().next().unwrap();
        assert!(entry.delivery[0].allow_private_network);
    }

    #[tokio::test]
    async fn register_schedule_rejects_relative_workdir() {
        let tool = ScheduleCronTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            "test",
            temp_dir.path(),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let result = tool
            .execute(
                json!({
                    "operation": "register",
                    "cron_expr": "0 9 * * *",
                    "prompt": "morning",
                    "workdir": "relative/path"
                }),
                &ctx,
            )
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("workdir must be an absolute path"));
    }

    #[tokio::test]
    async fn register_schedule_rejects_absolute_workdir_outside_workspace() {
        let tool = ScheduleCronTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let outside = temp_dir
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("outside-schedule-workdir");
        std::fs::create_dir_all(&outside).unwrap();
        let ctx = ToolContext::new(
            "test",
            temp_dir.path(),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let result = tool
            .execute(
                json!({
                    "operation": "register",
                    "cron_expr": "0 9 * * *",
                    "prompt": "morning",
                    "workdir": outside.to_string_lossy()
                }),
                &ctx,
            )
            .await;

        assert!(result.is_error);
        assert!(result
            .content
            .contains("workdir must stay inside the workspace"));

        let _ = std::fs::remove_dir_all(outside);
    }
}
