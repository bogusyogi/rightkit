//! `send_message` — write a message payload to the messages drop-dir for the daemon to pick up.
//!
//! S:F10 (continued) — `followup_task`, the third leg of Codex's
//! `multi_agents_v2` typed child contract (spawn/wait/**send_message**/
//! interrupt/**followup_task**). Codex's `followup_task` steers an
//! already-running child with a new `InterAgentMessage` rather than
//! spawning a fresh transcript; [`FollowupSubagentTool`] below carries the
//! same "steer, don't respawn" contract through the host app's existing
//! `MessageDispatcher` channel (`send_message`'s own live-dispatch path),
//! since a followup is, at the transport level, exactly a directed message
//! to the child's session id. What it adds over plain `send_message` is
//! the typed `agentPath` addressing from `spawn_subagent::AgentPath` and a
//! `channel = "followup_task"` marker so a live supervisor can distinguish
//! steering messages from ordinary notifications once one is wired.

use crate::context::{MessageDispatch, ToolContext};
use crate::spawn_subagent::AgentPath;
use crate::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct SendMessageTool;

#[async_trait::async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        #[cfg(feature = "host")]
        {
            "Send a message to a specific session or broadcast to all. Writes the message to the \
             messages drop-dir ($HOST_MESSAGES_DIR or ~/.host/messages) as a JSON file \
             for the daemon to forward. Returns confirmation with the target."
        }
        // The mobile build has no env override, so its description names none.
        #[cfg(not(feature = "host"))]
        {
            "Send a message to a specific session or broadcast to all. Writes the message to the messages drop-dir (~/.rightkit/messages) as a JSON file for the daemon to forward. Returns confirmation with the target."
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["message"],
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "Target session ID. Omit to broadcast to all sessions."
                },
                "message": {
                    "type": "string",
                    "description": "The message text to send."
                },
                "channel": {
                    "type": "string",
                    "description": "Message channel (default: 'notification')."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let message = match args["message"].as_str() {
            Some(m) => m,
            None => return ToolResult::error("send_message: 'message' is required"),
        };
        let session_id = args["session_id"].as_str();
        let channel = args["channel"].as_str().unwrap_or("notification");

        if let Some(dispatcher) = &ctx.message_dispatcher {
            let dispatch = MessageDispatch {
                from_session_id: ctx.session_id.clone(),
                target_session_id: session_id.map(|s| s.to_string()),
                channel: channel.to_string(),
                message: message.to_string(),
            };
            return match dispatcher.dispatch(dispatch).await {
                Ok(summary) => ToolResult::ok(format!("Message dispatched: {summary}")),
                Err(e) => ToolResult::error(format!("send_message: live dispatch failed: {e}")),
            };
        }

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);

        // `RIGHTKIT_MESSAGES_DIR` relocates the message directory in `host`
        // builds only; the mobile profile always uses the state root.
        let default_dir = || match rightkit_agent_config::state_root() {
            Some(root) => root.join("messages").to_string_lossy().into_owned(),
            None => "/tmp/.rightkit/messages".to_string(),
        };
        #[cfg(feature = "host")]
        let dir = std::env::var("RIGHTKIT_MESSAGES_DIR").unwrap_or_else(|_| default_dir());
        #[cfg(not(feature = "host"))]
        let dir = default_dir();

        if let Err(e) = std::fs::create_dir_all(&dir) {
            return ToolResult::error(format!("send_message: failed to create messages dir: {e}"));
        }

        let filename = match session_id {
            Some(sid) => format!("{}/{}-{}.json", dir, ts, sid),
            None => format!("{}/{}-broadcast.json", dir, ts),
        };

        let payload = json!({
            "ts": ts,
            "session_id": session_id,
            "channel": channel,
            "message": message,
        });

        if let Err(e) = std::fs::write(&filename, payload.to_string()) {
            return ToolResult::error(format!("send_message: failed to write message file: {e}"));
        }

        let target = session_id.unwrap_or("all sessions (broadcast)");
        ToolResult::ok(format!("Message sent to: {target}"))
    }
}

/// `followup_task` — steer an already-running subagent with a new message
/// instead of spawning a fresh child. Reuses the live `MessageDispatcher`
/// path `send_message` already has; adds typed `agentPath` addressing and
/// a distinguishable `followup_task` channel.
pub struct FollowupSubagentTool;

#[async_trait::async_trait]
impl Tool for FollowupSubagentTool {
    fn name(&self) -> &str {
        "followup_subagent"
    }

    fn description(&self) -> &str {
        "Steer an already-running subagent with a new instruction instead of spawning a fresh \
         child. Addressed by agent id or `parent/child` agent path; delivered through the same \
         live message channel as send_message."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["agentPath", "message"],
            "properties": {
                "agentPath": {
                    "type": "string",
                    "description": "Agent id or `parent/child` agent path of the subagent to steer."
                },
                "message": {
                    "type": "string",
                    "description": "The follow-up instruction text."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let raw_path = match args.get("agentPath").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("followup_subagent: 'agentPath' is required"),
        };
        let message = match args.get("message").and_then(|v| v.as_str()) {
            Some(m) => m,
            None => return ToolResult::error("followup_subagent: 'message' is required"),
        };
        let target = match AgentPath::parse(raw_path) {
            Ok(t) => t,
            Err(e) => return ToolResult::error(format!("followup_subagent: {e}")),
        };

        if let Some(dispatcher) = &ctx.message_dispatcher {
            let dispatch = MessageDispatch {
                from_session_id: ctx.session_id.clone(),
                // Preserve the donor's canonical AgentPath through the host
                // boundary. The daemon resolves parent/child lineage before
                // delivering, so nested follow-ups cannot escape their
                // originating parent subtree.
                target_session_id: Some(target.canonical()),
                channel: "followup_task".to_string(),
                message: message.to_string(),
            };
            return match dispatcher.dispatch(dispatch).await {
                Ok(summary) => ToolResult::ok(format!("Follow-up delivered: {summary}")),
                Err(e) => {
                    ToolResult::error(format!("followup_subagent: live dispatch failed: {e}"))
                }
            };
        }
        ToolResult::error(
            "followup_subagent: no live MessageDispatcher is wired on this ToolContext; a \
             follow-up cannot steer a running child through the filesystem drop-dir fallback \
             (that path only notifies, it cannot resume an in-flight turn)",
        )
    }
}
