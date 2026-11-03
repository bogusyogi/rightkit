/// Fixed, authenticated read projection of `membrane status --json` (Pulse's Membrane CLI).
/// This exposes one typed operation rather than forwarding arbitrary commands.
async fn get_context_status(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    let degraded_reason = state.context_degraded_reason().map(str::to_string);
    let backend = state.membrane.clone();
    match tokio::task::spawn_blocking(move || backend.status()).await {
        Ok(Ok(info)) if info.mode == "off" => Json(serde_json::json!({
            "schemaVersion": "coderight.context-status.v1",
            "state": "unavailable",
            "reason": degraded_reason.as_deref().unwrap_or(rightkit_memory::membrane::OFF_REASON),
            "contextDegraded": degraded_reason.is_some(),
            "degradedReason": degraded_reason,
        })),
        Ok(Ok(info)) => Json(serde_json::json!({
            "schemaVersion": "coderight.context-status.v1",
            "state": info.mode,
            "source": "membrane_cli_status",
            "contextDegraded": degraded_reason.is_some(),
            "degradedReason": degraded_reason,
            "snapshot": info.raw
        })),
        Ok(Err(error)) => Json(serde_json::json!({
            "schemaVersion": "coderight.context-status.v1",
            "state": "unavailable",
            "reason": error.to_string(),
            "contextDegraded": degraded_reason.is_some(),
            "degradedReason": degraded_reason,
        })),
        Err(_) => Json(serde_json::json!({
            "schemaVersion": "coderight.context-status.v1",
            "state": "unavailable",
            "reason": "context_status_worker_failed",
            "contextDegraded": degraded_reason.is_some(),
            "degradedReason": degraded_reason,
        })),
    }
}
