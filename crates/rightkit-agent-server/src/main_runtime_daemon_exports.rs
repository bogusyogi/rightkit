async fn daemon_export(session_id: &str, as_json: bool) -> i32 {
    let (base_url, token) = match crate::cli_presence::resolve_daemon_or_autostart().await {
        Some(p) => p,
        None => {
            eprintln!(
                "No running daemon found. Start one with `{app} serve`.",
                app = crate::host_identity::app()
            );
            return 1;
        }
    };

    let encoded_session_id = url_encode_component(session_id);
    let messages = match rightkit_http::http_client::daemon_get(
        &format!("{base_url}/sessions/{encoded_session_id}/transcript"),
        &token,
    )
    .await
    {
        Ok(resp) if rightkit_http::http_client::is_success(resp.status) => resp
            .json::<serde_json::Value>()
            .unwrap_or_else(|_| serde_json::json!([])),
        Ok(resp) if resp.status == 404 => {
            eprintln!("Session not found: {session_id}");
            return 1;
        }
        Ok(resp) => {
            eprintln!("Daemon returned {} for transcript", resp.status);
            return 1;
        }
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            return 1;
        }
    };

    let message_count = messages.as_array().map(Vec::len).unwrap_or(0);
    if as_json {
        let export = serde_json::json!({
            "sessionId": session_id,
            "messageCount": message_count,
            "messages": messages,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&export).unwrap_or_default()
        );
        return 0;
    }

    println!(
        "{}",
        render_cli_transcript_markdown(session_id, &messages, message_count)
    );
    0
}

async fn daemon_review_findings(session_id: &str, as_json: bool) -> i32 {
    let (base_url, token) = match crate::cli_presence::resolve_daemon_or_autostart().await {
        Some(p) => p,
        None => {
            eprintln!(
                "No running daemon found. Start one with `{app} serve`.",
                app = crate::host_identity::app()
            );
            return 1;
        }
    };
    let artifacts = match rightkit_http::http_client::daemon_get(
        &format!(
            "{base_url}/sessions/{}/runtime/artifacts",
            url_encode_component(session_id)
        ),
        &token,
    )
    .await
    {
        Ok(resp) if rightkit_http::http_client::is_success(resp.status) => resp
            .json::<serde_json::Value>()
            .unwrap_or_else(|_| serde_json::json!([])),
        Ok(resp) if resp.status == 404 => {
            eprintln!("Session not found: {session_id}");
            return 1;
        }
        Ok(resp) => {
            eprintln!("Daemon returned {} for review artifacts", resp.status);
            return 1;
        }
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            return 1;
        }
    };

    let mut findings = Vec::new();
    for artifact in artifacts.as_array().into_iter().flatten() {
        if !is_auditor_transcript_artifact(artifact) {
            continue;
        }
        let review_session_id = artifact
            .get("provenance")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let transcript = if review_session_id.is_empty() {
            serde_json::Value::Null
        } else {
            match rightkit_http::http_client::daemon_get(
                &format!(
                    "{base_url}/sessions/{}/transcript",
                    url_encode_component(review_session_id)
                ),
                &token,
            )
            .await
            {
                Ok(resp) if rightkit_http::http_client::is_success(resp.status) => resp
                    .json::<serde_json::Value>()
                    .unwrap_or(serde_json::Value::Null),
                _ => serde_json::Value::Null,
            }
        };
        findings.push(serde_json::json!({
            "artifactId": artifact.get("id").cloned().unwrap_or(serde_json::Value::Null),
            "artifactPath": artifact.get("path").cloned().unwrap_or(serde_json::Value::Null),
            "producer": artifact.get("producer").cloned().unwrap_or(serde_json::Value::Null),
            "reviewSessionId": review_session_id,
            "createdAt": artifact.get("createdAt").cloned().unwrap_or(serde_json::Value::Null),
            "contentHash": artifact.get("contentHash").cloned().unwrap_or(serde_json::Value::Null),
            "content": final_assistant_content(&transcript).unwrap_or_default(),
        }));
    }

    let body = review_findings_body(session_id, findings);
    if as_json {
        print_json_value(body);
    } else {
        println!("{}", render_review_findings_markdown(&body));
    }
    0
}

fn review_findings_body(session_id: &str, findings: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "schema_version": crate::host_identity::schema("cli.review_findings.v1"),
        "sessionId": session_id,
        "source": "auditor_subagent_transcripts",
        "findingCount": findings.len(),
        "findings": findings,
    })
}

fn render_review_findings_markdown(body: &serde_json::Value) -> String {
    let session_id = body["sessionId"].as_str().unwrap_or("");
    let findings = body["findings"].as_array().cloned().unwrap_or_default();
    if findings.is_empty() {
        return format!("**Review findings:** {session_id}\n\nNo auditor findings found.");
    }
    let mut out = format!(
        "**Review findings:** {session_id}\nSource: auditor subagent transcripts\nFindings: {}\n",
        findings.len()
    );
    for finding in findings {
        let producer = finding["producer"].as_str().unwrap_or("auditor");
        let review_session_id = finding["reviewSessionId"].as_str().unwrap_or("");
        let content = finding["content"].as_str().unwrap_or("").trim();
        out.push_str(&format!("\n## {producer} ({review_session_id})\n"));
        if content.is_empty() {
            out.push_str("_Transcript did not contain a final assistant finding._\n");
        } else {
            out.push_str(content);
            out.push('\n');
        }
    }
    out
}

fn render_cli_transcript_markdown(
    session_id: &str,
    messages: &serde_json::Value,
    message_count: usize,
) -> String {
    let mut out =
        format!("**Session export:** {session_id}\nFormat: markdown\nMessages: {message_count}\n");
    let Some(rows) = messages.as_array() else {
        return out;
    };
    for message in rows {
        let kind = message
            .get("$kind")
            .and_then(|v| v.as_str())
            .unwrap_or("message");
        let created_at = message
            .get("createdAt")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let title = match kind {
            "system" => "System",
            "user" => "User",
            "assistant" => "Assistant",
            "tool" => "Tool",
            "compact" => "Compact",
            _ => "Message",
        };
        let content = message
            .get("content")
            .or_else(|| message.get("summary"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        out.push_str(&format!(
            "\n## {title}\n_Time: {created_at}_\n\n{content}\n"
        ));
    }
    out
}

/// POST to the daemon and return the raw JSON response (no pretty-print).
async fn daemon_post_raw(path: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
    let (base_url, token) = crate::cli_presence::resolve_daemon_or_autostart()
        .await
        .ok_or_else(|| {
            format!(
                "No running daemon found. Start one with `{app} serve`.",
                app = crate::host_identity::app()
            )
        })?;
    let resp = rightkit_http::http_client::daemon_call(
        rightkit_http::Method::Post,
        &format!("{base_url}{path}"),
        &token,
        Some(&body),
    )
    .await
    .map_err(|e| format!("Failed to contact daemon: {e}"))?;
    if !rightkit_http::http_client::is_success(resp.status) {
        return Err(format!(
            "Daemon returned HTTP {}: {}",
            resp.status,
            resp.text()
        ));
    }
    let body = resp.body;
    if body.is_empty() {
        Ok(serde_json::Value::Null)
    } else {
        serde_json::from_slice(&body).map_err(|e| format!("Failed to parse daemon response: {e}"))
    }
}
// ── Serve ─────────────────────────────────────────────────────────────────────
