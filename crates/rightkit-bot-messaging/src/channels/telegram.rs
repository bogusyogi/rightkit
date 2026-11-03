//! Telegram bot adapter — message routing, command handling, approval inline keyboards.
//!
//! The real long-poll loop lives in the binary; this module is the pure decision layer:
//! given a Telegram update and config, decide what daemon calls to make and what reply
//! to send back. Fully testable against `StubDaemon`.

use crate::auth::AuthGate;
use crate::daemon::{event_seq, DaemonClient, DaemonError};
use rightkit_agent_engine_protocol::{ProtocolEvent, TranscriptMessage};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Configuration for the Telegram adapter.
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub allowed_users: Vec<u64>,
    /// The session a chat is bound to. In the real bot this is per-chat; here it's a default.
    pub default_session: String,
}

/// A normalized inbound Telegram update.
#[derive(Debug, Clone)]
pub enum TelegramUpdate {
    /// A text message from a user.
    Message {
        user_id: u64,
        chat_id: i64,
        text: String,
    },
    /// A tapped inline button (callback query), e.g. an approval response.
    Callback {
        user_id: u64,
        chat_id: i64,
        data: String,
    },
}

impl TelegramUpdate {
    pub fn user_id(&self) -> u64 {
        match self {
            TelegramUpdate::Message { user_id, .. } | TelegramUpdate::Callback { user_id, .. } => {
                *user_id
            }
        }
    }

    pub fn chat_id(&self) -> i64 {
        match self {
            TelegramUpdate::Message { chat_id, .. } | TelegramUpdate::Callback { chat_id, .. } => {
                *chat_id
            }
        }
    }
}

/// A recognized bot command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotCommand {
    Status,
    Stop,
    Session(Option<String>),
    /// Plain prose to forward to the session as a user message.
    Say(String),
}

impl BotCommand {
    pub fn parse(text: &str) -> BotCommand {
        let trimmed = text.trim();
        match trimmed {
            "/status" => BotCommand::Status,
            "/stop" => BotCommand::Stop,
            "/session" => BotCommand::Session(None),
            other if other.starts_with("/session ") => {
                let id = other.trim_start_matches("/session").trim();
                if id.is_empty() {
                    BotCommand::Session(None)
                } else {
                    BotCommand::Session(Some(id.to_string()))
                }
            }
            other => BotCommand::Say(other.to_string()),
        }
    }
}

/// An inline keyboard button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineButton {
    pub label: String,
    /// Callback data, parsed back on tap.
    pub data: String,
}

/// What the bot should send back to Telegram in response to an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramReply {
    pub chat_id: i64,
    pub text: String,
    /// Optional inline keyboard (e.g. approval Allow/Deny).
    pub buttons: Vec<InlineButton>,
}

impl TelegramReply {
    pub fn text(chat_id: i64, text: impl Into<String>) -> Self {
        Self {
            chat_id,
            text: text.into(),
            buttons: vec![],
        }
    }

    pub fn with_buttons(mut self, buttons: Vec<InlineButton>) -> Self {
        self.buttons = buttons;
        self
    }
}

/// Durable per-chat Telegram session bindings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TelegramSessionBindings {
    /// Telegram chat ids are string keys so the store remains JSON-object friendly.
    #[serde(default)]
    pub chats: HashMap<String, String>,
}

pub fn load_telegram_session_bindings(path: &Path) -> Result<TelegramSessionBindings, String> {
    if !path.exists() {
        return Ok(TelegramSessionBindings::default());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read Telegram bindings {}: {e}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|e| format!("failed to parse Telegram bindings {}: {e}", path.display()))
}

pub fn save_telegram_session_bindings(
    path: &Path,
    bindings: &TelegramSessionBindings,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "failed to create Telegram binding directory {}: {e}",
                parent.display()
            )
        })?;
    }
    let raw = serde_json::to_string_pretty(bindings)
        .map_err(|e| format!("failed to encode Telegram bindings: {e}"))?;
    std::fs::write(path, raw)
        .map_err(|e| format!("failed to write Telegram bindings {}: {e}", path.display()))
}

fn sessions_from_bindings(bindings: TelegramSessionBindings) -> HashMap<i64, String> {
    bindings
        .chats
        .into_iter()
        .filter_map(|(chat_id, session_id)| chat_id.parse::<i64>().ok().map(|id| (id, session_id)))
        .collect()
}

fn bindings_from_sessions(sessions: &HashMap<i64, String>) -> TelegramSessionBindings {
    TelegramSessionBindings {
        chats: sessions
            .iter()
            .map(|(chat_id, session_id)| (chat_id.to_string(), session_id.clone()))
            .collect(),
    }
}

/// The Telegram bot decision engine.
pub struct TelegramBot<D: DaemonClient> {
    daemon: D,
    auth: AuthGate,
    session: String,
    chat_sessions: Mutex<HashMap<i64, String>>,
    binding_store_path: Option<PathBuf>,
}

impl<D: DaemonClient> TelegramBot<D> {
    pub fn new(daemon: D, config: &TelegramConfig) -> Self {
        Self::with_sessions(daemon, config, HashMap::new(), None)
    }

    pub fn new_with_binding_store(
        daemon: D,
        config: &TelegramConfig,
        binding_store_path: impl Into<PathBuf>,
    ) -> Self {
        let binding_store_path = binding_store_path.into();
        let sessions = load_telegram_session_bindings(&binding_store_path)
            .map(sessions_from_bindings)
            .unwrap_or_default();
        Self::with_sessions(daemon, config, sessions, Some(binding_store_path))
    }

    fn with_sessions(
        daemon: D,
        config: &TelegramConfig,
        sessions: HashMap<i64, String>,
        binding_store_path: Option<PathBuf>,
    ) -> Self {
        Self {
            daemon,
            auth: AuthGate::new(config.allowed_users.clone()),
            session: config.default_session.clone(),
            chat_sessions: Mutex::new(sessions),
            binding_store_path,
        }
    }

    pub fn session_for_chat(&self, chat_id: i64) -> String {
        self.chat_sessions
            .lock()
            .unwrap()
            .get(&chat_id)
            .cloned()
            .unwrap_or_else(|| self.session.clone())
    }

    fn bind_chat_session(&self, chat_id: i64, session_id: String) {
        let snapshot = {
            let mut sessions = self.chat_sessions.lock().unwrap();
            sessions.insert(chat_id, session_id);
            bindings_from_sessions(&sessions)
        };

        if let Some(path) = &self.binding_store_path {
            if let Err(e) = save_telegram_session_bindings(path, &snapshot) {
                eprintln!("[telegram] failed to persist session bindings: {e}");
            }
        }
    }

    /// Handle one inbound update, performing daemon calls and returning a reply.
    pub async fn handle(&self, update: TelegramUpdate) -> Result<TelegramReply, DaemonError> {
        match update {
            TelegramUpdate::Message {
                user_id,
                chat_id,
                text,
            } => {
                if !self.auth.check(user_id).is_allowed() {
                    return Ok(TelegramReply::text(chat_id, "⛔ Not authorized."));
                }
                match BotCommand::parse(&text) {
                    BotCommand::Status => {
                        let session_id = self.session_for_chat(chat_id);
                        let latest = self
                            .latest_assistant_text(&session_id)
                            .await
                            .unwrap_or_default();
                        let body = if latest.is_empty() {
                            format!("✅ Session `{session_id}` idle.")
                        } else {
                            format!("✅ Session `{session_id}` last: {latest}")
                        };
                        Ok(TelegramReply::text(chat_id, body))
                    }
                    BotCommand::Stop => {
                        // Stop maps to interrupting the run — modeled as a control message.
                        let session_id = self.session_for_chat(chat_id);
                        let target_key = chat_id.to_string();
                        if let Err(e) = self
                            .daemon
                            .record_binding("telegram", &target_key, &session_id)
                            .await
                        {
                            eprintln!("[telegram] SQL binding dual-write failed: {e}");
                        }
                        self.daemon
                            .submit_channel_message("telegram", &target_key, &session_id, "/stop")
                            .await?;
                        Ok(TelegramReply::text(chat_id, "🛑 Stop requested."))
                    }
                    BotCommand::Session(Some(session_id)) => {
                        // Failure policy (intentional, do not change without
                        // updating the dual-write failure test in this file):
                        // 1. JSON binding is the source of truth — it is
                        //    committed via `bind_chat_session` (synchronous,
                        //    in-memory + file write) and is required for the
                        //    bot to route subsequent messages to the session.
                        // 2. SQL binding is a best-effort dual-write used by
                        //    `/remote/bindings` and the status endpoints for
                        //    query / dashboard purposes.
                        // 3. If SQL fails, the binding is still active via the
                        //    JSON store and the user-facing reply is the same.
                        //    The error is logged (eprintln) and can be surfaced
                        //    in the runtime diagnostics panel; a follow-up
                        //    reconciler can replay the JSON bindings into SQL.
                        self.bind_chat_session(chat_id, session_id.clone());
                        if let Err(e) = self
                            .daemon
                            .record_binding("telegram", &chat_id.to_string(), &session_id)
                            .await
                        {
                            eprintln!("[telegram] SQL binding dual-write failed: {e}");
                        }
                        Ok(TelegramReply::text(
                            chat_id,
                            format!("✅ Bound this chat to session `{session_id}`."),
                        ))
                    }
                    BotCommand::Session(None) => {
                        let session_id = self.session_for_chat(chat_id);
                        Ok(TelegramReply::text(
                            chat_id,
                            format!("Current session: `{session_id}`"),
                        ))
                    }
                    BotCommand::Say(msg) => {
                        let session_id = self.session_for_chat(chat_id);
                        let target_key = chat_id.to_string();
                        if let Err(e) = self
                            .daemon
                            .record_binding("telegram", &target_key, &session_id)
                            .await
                        {
                            eprintln!("[telegram] SQL binding dual-write failed: {e}");
                        }
                        self.daemon
                            .submit_channel_message("telegram", &target_key, &session_id, &msg)
                            .await?;
                        Ok(TelegramReply::text(chat_id, "📨 Sent."))
                    }
                }
            }
            TelegramUpdate::Callback {
                user_id,
                chat_id,
                data,
            } => {
                if !self.auth.check(user_id).is_allowed() {
                    return Ok(TelegramReply::text(chat_id, "⛔ Not authorized."));
                }
                self.handle_callback(chat_id, &data).await
            }
        }
    }

    /// Convert daemon events newer than `after_seq` into proactive Telegram replies.
    pub async fn event_replies_after(
        &self,
        chat_id: i64,
        after_seq: i64,
    ) -> Result<(i64, Vec<TelegramReply>), DaemonError> {
        let session_id = self.session_for_chat(chat_id);
        let mut events = self.daemon.events_after(&session_id, after_seq).await?;
        events.sort_by_key(event_seq);

        let mut next_seq = after_seq;
        let mut replies = Vec::new();
        for event in &events {
            next_seq = next_seq.max(event_seq(event));
            match event {
                ProtocolEvent::ToolApprovalRequested { .. } => {
                    if let Some(reply) = self.approval_prompt(chat_id, event) {
                        replies.push(reply);
                    }
                }
                ProtocolEvent::AssistantMessageCompleted {
                    message:
                        TranscriptMessage::Assistant {
                            content,
                            tool_calls,
                            ..
                        },
                    ..
                } if !content.trim().is_empty() && tool_calls.is_empty() => {
                    replies.push(TelegramReply::text(chat_id, format!("✅ {content}")));
                }
                ProtocolEvent::RunInterrupted { reason, .. } => {
                    let detail = reason
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or("requested");
                    replies.push(TelegramReply::text(
                        chat_id,
                        format!("🛑 Run interrupted: {detail}"),
                    ));
                }
                ProtocolEvent::RunFailed { error, hint, .. } => {
                    let mut text = format!("❌ Run failed: {error}");
                    if !hint.trim().is_empty() {
                        text.push('\n');
                        text.push_str(hint);
                    }
                    replies.push(TelegramReply::text(chat_id, text));
                }
                _ => {}
            }
        }

        Ok((next_seq, replies))
    }

    /// Handle an approval button tap. Callback data format: `approve:<tool_call_id>` or
    /// `deny:<tool_call_id>`.
    async fn handle_callback(
        &self,
        chat_id: i64,
        data: &str,
    ) -> Result<TelegramReply, DaemonError> {
        if let Some(tc) = data.strip_prefix("approve:") {
            let session_id = self.session_for_chat(chat_id);
            self.daemon.resolve_approval(&session_id, tc, true).await?;
            Ok(TelegramReply::text(chat_id, "✅ Approved."))
        } else if let Some(tc) = data.strip_prefix("deny:") {
            let session_id = self.session_for_chat(chat_id);
            self.daemon.resolve_approval(&session_id, tc, false).await?;
            Ok(TelegramReply::text(chat_id, "❌ Denied."))
        } else {
            Ok(TelegramReply::text(chat_id, "Unknown action."))
        }
    }

    /// Build the approval prompt + inline keyboard for a `ToolApprovalRequested` event.
    pub fn approval_prompt(&self, chat_id: i64, event: &ProtocolEvent) -> Option<TelegramReply> {
        if let ProtocolEvent::ToolApprovalRequested {
            call, risk_label, ..
        } = event
        {
            let text = format!("⚠️ Approve tool `{}`? (risk: {})", call.name, risk_label);
            let buttons = vec![
                InlineButton {
                    label: "Allow".into(),
                    data: format!("approve:{}", call.id),
                },
                InlineButton {
                    label: "Deny".into(),
                    data: format!("deny:{}", call.id),
                },
            ];
            Some(TelegramReply::text(chat_id, text).with_buttons(buttons))
        } else {
            None
        }
    }

    /// Assemble streamed assistant text from the daemon's recent events.
    async fn latest_assistant_text(&self, session_id: &str) -> Result<String, DaemonError> {
        let events = self.daemon.events_after(session_id, 0).await?;
        let mut buf = String::new();
        let mut ordered = events;
        ordered.sort_by_key(event_seq);
        for e in &ordered {
            if let ProtocolEvent::AssistantDelta {
                text, is_reasoning, ..
            } = e
            {
                if !is_reasoning {
                    buf.push_str(text);
                }
            }
        }
        Ok(buf)
    }
}
