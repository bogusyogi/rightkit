use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;

use crate::api::{transcript_for_session, DaemonState};

#[derive(Debug, Deserialize)]
pub(crate) struct SessionExportQuery {
    format: Option<String>,
}

pub(crate) async fn export_session_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Query(query): Query<SessionExportQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let format = query.format.as_deref().unwrap_or("markdown");
    if !matches!(format, "md" | "markdown" | "html") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let messages = transcript_for_session(&state, &id).await?;
    let content = if format == "html" {
        render_html(&id, &messages)
    } else {
        render_markdown(&id, &messages)
    };
    Ok(Json(serde_json::json!({
        "sessionId": id,
        "format": if format == "html" { "html" } else { "markdown" },
        "content": content,
    })))
}

fn visible_messages(
    messages: &[rightkit_agent_engine_protocol::TranscriptMessage],
) -> Vec<(&'static str, String, String)> {
    messages
        .iter()
        .filter_map(|message| match message {
            rightkit_agent_engine_protocol::TranscriptMessage::User {
                created_at,
                content,
                ..
            } => {
                let content = clean(content);
                (!content.is_empty() && !content.trim_start().starts_with("[view_image:"))
                    .then_some(("User", created_at.clone(), content))
            }
            rightkit_agent_engine_protocol::TranscriptMessage::Assistant {
                created_at,
                content,
                ..
            } => {
                let content = clean(content);
                (!content.is_empty()).then_some(("Assistant", created_at.clone(), content))
            }
            _ => None,
        })
        .collect()
}

fn clean(content: &str) -> String {
    let mut remaining = content;
    let mut out = String::new();
    loop {
        let Some(start) = remaining.find("<system-reminder>") else {
            out.push_str(remaining);
            break;
        };
        out.push_str(&remaining[..start]);
        let after = &remaining[start + "<system-reminder>".len()..];
        let Some(end) = after.find("</system-reminder>") else {
            break;
        };
        remaining = &after[end + "</system-reminder>".len()..];
    }
    out.lines()
        .filter(|line| !line.trim_start().starts_with("[view_image:"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn render_markdown(id: &str, messages: &[rightkit_agent_engine_protocol::TranscriptMessage]) -> String {
    let visible = visible_messages(messages);
    let mut out = format!(
        "# Session export: {id}\n\nMessages: {} visible of {}\n",
        visible.len(),
        messages.len()
    );
    for (role, created_at, content) in visible {
        out.push_str(&format!(
            "\n## {role}\n\n_Time: {created_at}_\n\n{content}\n"
        ));
    }
    out
}

fn render_html(id: &str, messages: &[rightkit_agent_engine_protocol::TranscriptMessage]) -> String {
    let mut out = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Session export: {}</title></head><body><main><h1>Session export: {}</h1>",
        escape_html(id),
        escape_html(id)
    );
    for (role, created_at, content) in visible_messages(messages) {
        out.push_str(&format!(
            "<article><h2>{role}</h2><time>{}</time><pre>{}</pre></article>",
            escape_html(&created_at),
            escape_html(&content)
        ));
    }
    out.push_str("</main></body></html>");
    out
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
