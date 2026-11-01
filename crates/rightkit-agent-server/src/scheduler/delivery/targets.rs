pub(super) async fn deliver_schedule_result(
    state: &DaemonState,
    root: &Path,
    entry: &ScheduleEntry,
    session_id: &str,
    result_status: &str,
    source_artifact: Option<&Path>,
    fallback_content: &str,
) -> DeliveryOutcome {
    if entry.delivery.is_empty() {
        return DeliveryOutcome::default();
    }

    let mut delivered = 0usize;
    let mut errors = Vec::new();
    for target in &entry.delivery {
        match target.kind.as_str() {
            "file_artifact" => {
                match deliver_file_artifact(root, entry, target, source_artifact, fallback_content)
                {
                    Ok(()) => delivered += 1,
                    Err(e) => errors.push(format!("file_artifact: {e}")),
                }
            }
            "session_message" => match deliver_session_message(
                state,
                session_id,
                target,
                source_artifact,
                fallback_content,
            )
            .await
            {
                Ok(()) => delivered += 1,
                Err(e) => errors.push(format!("session_message: {e}")),
            },
            "webhook" | "ntfy" => match deliver_webhook_result(
                state,
                root,
                entry,
                session_id,
                result_status,
                target,
                source_artifact,
                fallback_content,
            )
            .await
            {
                Ok(()) => delivered += 1,
                Err(e) => errors.push(format!("{}: {e}", target.kind)),
            },
            "telegram_chat" => match deliver_telegram_chat_result(
                entry,
                session_id,
                result_status,
                target,
                source_artifact,
                fallback_content,
            )
            .await
            {
                Ok(()) => delivered += 1,
                Err(e) => errors.push(format!("telegram_chat: {e}")),
            },
            "desktop_notification" | "desktopNotification" => match deliver_desktop_notification(
                root,
                entry,
                session_id,
                result_status,
                source_artifact,
                fallback_content,
            ) {
                Ok(()) => delivered += 1,
                Err(e) => errors.push(format!("desktop_notification: {e}")),
            },
            other => errors.push(format!("unsupported delivery target '{other}'")),
        }
    }

    let status = if errors.is_empty() {
        "delivered"
    } else if delivered > 0 {
        "partial"
    } else {
        "failed"
    };
    DeliveryOutcome {
        status: Some(status.to_string()),
        errors,
    }
}

async fn deliver_session_message(
    state: &DaemonState,
    session_id: &str,
    target: &DeliveryTarget,
    source_artifact: Option<&Path>,
    fallback_content: &str,
) -> Result<(), String> {
    let message = delivery_message_content(source_artifact, fallback_content);
    rightkit_agent_tools::coding_tools::MessageDispatcher::dispatch(
        state,
        rightkit_agent_tools::coding_tools::MessageDispatch {
            from_session_id: session_id.to_string(),
            target_session_id: target
                .target
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            channel: "schedule".to_string(),
            message,
        },
    )
    .await
    .map(|_| ())
}

fn deliver_desktop_notification(
    root: &Path,
    entry: &ScheduleEntry,
    session_id: &str,
    result_status: &str,
    source_artifact: Option<&Path>,
    fallback_content: &str,
) -> Result<(), String> {
    crate::api_notifications::append_desktop_notification(
        root,
        "schedule",
        Some(&entry.id),
        Some(session_id),
        format!("Schedule {} {}", entry.id, result_status),
        desktop_notification_body(source_artifact, fallback_content),
    )
    .map(|_| ())
}

fn desktop_notification_body(source_artifact: Option<&Path>, fallback_content: &str) -> String {
    let artifact = source_artifact
        .map(|path| format!("Artifact: {}\n\n", path.display()))
        .unwrap_or_default();
    format!(
        "{}{}",
        artifact,
        delivery_message_content(source_artifact, fallback_content)
    )
}
