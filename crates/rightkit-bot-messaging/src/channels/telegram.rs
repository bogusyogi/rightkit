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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::StubDaemon;
    use rightkit_agent_engine_protocol::ToolCall;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn config(users: Vec<u64>) -> TelegramConfig {
        TelegramConfig {
            bot_token: "t".into(),
            allowed_users: users,
            default_session: "s1".into(),
        }
    }

    fn temp_binding_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("rightkit-telegram-{label}-{nanos}.json"))
    }

    #[test]
    fn command_parse() {
        assert_eq!(BotCommand::parse("/status"), BotCommand::Status);
        assert_eq!(BotCommand::parse("/stop"), BotCommand::Stop);
        assert_eq!(BotCommand::parse("/session"), BotCommand::Session(None));
        assert_eq!(
            BotCommand::parse("/session sprint-7"),
            BotCommand::Session(Some("sprint-7".into()))
        );
        assert_eq!(BotCommand::parse("hello"), BotCommand::Say("hello".into()));
    }

    #[tokio::test]
    async fn message_forwarded_to_daemon() {
        let bot = TelegramBot::new(StubDaemon::new(), &config(vec![1]));
        let reply = bot
            .handle(TelegramUpdate::Message {
                user_id: 1,
                chat_id: 10,
                text: "fix the bug".into(),
            })
            .await
            .unwrap();
        assert_eq!(reply.text, "📨 Sent.");
        assert_eq!(bot.daemon.submitted_count(), 1);
    }

    #[tokio::test]
    async fn unauthorized_user_rejected() {
        let bot = TelegramBot::new(StubDaemon::new(), &config(vec![1]));
        let reply = bot
            .handle(TelegramUpdate::Message {
                user_id: 999,
                chat_id: 10,
                text: "hi".into(),
            })
            .await
            .unwrap();
        assert!(reply.text.contains("Not authorized"));
        assert_eq!(bot.daemon.submitted_count(), 0);
    }

    #[tokio::test]
    async fn approve_callback_resolves_approval() {
        let bot = TelegramBot::new(StubDaemon::new(), &config(vec![1]));
        let reply = bot
            .handle(TelegramUpdate::Callback {
                user_id: 1,
                chat_id: 10,
                data: "approve:tc-42".into(),
            })
            .await
            .unwrap();
        assert!(reply.text.contains("Approved"));
        assert_eq!(
            bot.daemon.last_approval(),
            Some(("s1".into(), "tc-42".into(), true))
        );
    }

    #[tokio::test]
    async fn deny_callback_resolves_negative() {
        let bot = TelegramBot::new(StubDaemon::new(), &config(vec![1]));
        bot.handle(TelegramUpdate::Callback {
            user_id: 1,
            chat_id: 10,
            data: "deny:tc-7".into(),
        })
        .await
        .unwrap();
        assert_eq!(
            bot.daemon.last_approval(),
            Some(("s1".into(), "tc-7".into(), false))
        );
    }

    #[tokio::test]
    async fn unauthorized_callback_blocked() {
        let bot = TelegramBot::new(StubDaemon::new(), &config(vec![1]));
        bot.handle(TelegramUpdate::Callback {
            user_id: 2,
            chat_id: 10,
            data: "approve:x".into(),
        })
        .await
        .unwrap();
        assert!(bot.daemon.last_approval().is_none());
    }

    #[test]
    fn approval_prompt_builds_keyboard() {
        let bot = TelegramBot::new(StubDaemon::new(), &config(vec![1]));
        let ev = ProtocolEvent::ToolApprovalRequested {
            session_id: "s1".into(),
            seq: 5,
            created_at: "t".into(),
            call: ToolCall {
                id: "tc1".into(),
                name: "shell".into(),
                arguments_json: "{}".into(),
            },
            risk_label: "high".into(),
            proposed_change: None,
            policy_source: None,
            middleware_decisions: vec![],
        };
        let reply = bot.approval_prompt(10, &ev).unwrap();
        assert_eq!(reply.buttons.len(), 2);
        assert_eq!(reply.buttons[0].data, "approve:tc1");
        assert_eq!(reply.buttons[1].data, "deny:tc1");
        assert!(reply.text.contains("shell"));
    }

    #[test]
    fn approval_prompt_ignores_other_events() {
        let bot = TelegramBot::new(StubDaemon::new(), &config(vec![1]));
        let ev = ProtocolEvent::RunInterrupted {
            session_id: "s1".into(),
            seq: 1,
            created_at: "t".into(),
            reason: None,
        };
        assert!(bot.approval_prompt(10, &ev).is_none());
    }

    #[tokio::test]
    async fn status_assembles_assistant_text() {
        let daemon = StubDaemon::new().with_events(vec![
            ProtocolEvent::AssistantDelta {
                session_id: "s1".into(),
                seq: 1,
                created_at: "t".into(),
                text: "Hello ".into(),
                is_reasoning: false,
            },
            ProtocolEvent::AssistantDelta {
                session_id: "s1".into(),
                seq: 2,
                created_at: "t".into(),
                text: "world".into(),
                is_reasoning: false,
            },
        ]);
        let bot = TelegramBot::new(daemon, &config(vec![1]));
        let reply = bot
            .handle(TelegramUpdate::Message {
                user_id: 1,
                chat_id: 10,
                text: "/status".into(),
            })
            .await
            .unwrap();
        assert!(reply.text.contains("Hello world"));
    }

    #[tokio::test]
    async fn stop_command_sends_control() {
        let bot = TelegramBot::new(StubDaemon::new(), &config(vec![1]));
        let reply = bot
            .handle(TelegramUpdate::Message {
                user_id: 1,
                chat_id: 10,
                text: "/stop".into(),
            })
            .await
            .unwrap();
        assert!(reply.text.contains("Stop requested"));
        assert_eq!(
            bot.daemon.channel_submitted.lock().unwrap().last().cloned(),
            Some(("telegram".into(), "10".into(), "s1".into(), "/stop".into()))
        );
    }

    #[tokio::test]
    async fn session_command_binds_chat_for_subsequent_messages() {
        let bot = TelegramBot::new(StubDaemon::new().with_session("s2"), &config(vec![1]));

        let reply = bot
            .handle(TelegramUpdate::Message {
                user_id: 1,
                chat_id: 10,
                text: "/session s2".into(),
            })
            .await
            .unwrap();
        assert!(reply.text.contains("s2"));
        assert_eq!(bot.session_for_chat(10), "s2");

        bot.handle(TelegramUpdate::Message {
            user_id: 1,
            chat_id: 10,
            text: "continue".into(),
        })
        .await
        .unwrap();

        assert_eq!(
            bot.daemon.submitted.lock().unwrap().last().cloned(),
            Some(("s2".into(), "continue".into()))
        );
    }

    #[tokio::test]
    async fn session_command_persists_and_reloads_chat_binding() {
        let path = temp_binding_path("binding");
        let bot = TelegramBot::new_with_binding_store(
            StubDaemon::new().with_session("s2"),
            &config(vec![1]),
            path.clone(),
        );

        bot.handle(TelegramUpdate::Message {
            user_id: 1,
            chat_id: 10,
            text: "/session s2".into(),
        })
        .await
        .unwrap();

        let persisted = load_telegram_session_bindings(&path).unwrap();
        assert_eq!(persisted.chats.get("10"), Some(&"s2".to_string()));

        let reloaded = TelegramBot::new_with_binding_store(
            StubDaemon::new().with_session("s2"),
            &config(vec![1]),
            path.clone(),
        );
        assert_eq!(reloaded.session_for_chat(10), "s2");

        reloaded
            .handle(TelegramUpdate::Message {
                user_id: 1,
                chat_id: 10,
                text: "continue".into(),
            })
            .await
            .unwrap();

        assert_eq!(
            reloaded.daemon.submitted.lock().unwrap().last().cloned(),
            Some(("s2".into(), "continue".into()))
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn approvals_resolve_against_bound_chat_session() {
        let bot = TelegramBot::new(StubDaemon::new().with_session("s2"), &config(vec![1]));
        bot.handle(TelegramUpdate::Message {
            user_id: 1,
            chat_id: 10,
            text: "/session s2".into(),
        })
        .await
        .unwrap();

        bot.handle(TelegramUpdate::Callback {
            user_id: 1,
            chat_id: 10,
            data: "approve:tc-9".into(),
        })
        .await
        .unwrap();

        assert_eq!(
            bot.daemon.last_approval(),
            Some(("s2".into(), "tc-9".into(), true))
        );
    }

    #[tokio::test]
    async fn event_replies_after_builds_approval_prompt_and_advances_seq() {
        let daemon = StubDaemon::new().with_events(vec![ProtocolEvent::ToolApprovalRequested {
            session_id: "s1".into(),
            seq: 7,
            created_at: "t".into(),
            call: ToolCall {
                id: "tc1".into(),
                name: "shell".into(),
                arguments_json: "{}".into(),
            },
            risk_label: "high".into(),
            proposed_change: None,
            policy_source: None,
            middleware_decisions: vec![],
        }]);
        let bot = TelegramBot::new(daemon, &config(vec![1]));

        let (seq, replies) = bot.event_replies_after(10, 0).await.unwrap();

        assert_eq!(seq, 7);
        assert_eq!(replies.len(), 1);
        assert!(replies[0].text.contains("Approve tool"));
        assert_eq!(replies[0].buttons[0].data, "approve:tc1");
    }

    #[tokio::test]
    async fn event_replies_after_reports_completion_failures_and_interruptions() {
        let daemon = StubDaemon::new().with_events(vec![
            ProtocolEvent::RunFailed {
                session_id: "s1".into(),
                seq: 3,
                created_at: "t".into(),
                error: "provider timed out".into(),
                kind: "timeout".into(),
                hint: "Try again.".into(),
            },
            ProtocolEvent::AssistantMessageCompleted {
                session_id: "s1".into(),
                seq: 5,
                created_at: "t".into(),
                message: TranscriptMessage::Assistant {
                    id: "a1".into(),
                    created_at: "t".into(),
                    content: "Done.".into(),
                    reasoning_content: None,
                    tool_calls: vec![],
                    pinned: false,
                },
            },
            ProtocolEvent::RunInterrupted {
                session_id: "s1".into(),
                seq: 6,
                created_at: "t".into(),
                reason: Some("operator".into()),
            },
        ]);
        let bot = TelegramBot::new(daemon, &config(vec![1]));

        let (seq, replies) = bot.event_replies_after(10, 0).await.unwrap();

        assert_eq!(seq, 6);
        assert_eq!(replies.len(), 3);
        assert!(replies[0].text.contains("provider timed out"));
        assert!(replies[1].text.contains("Done."));
        assert!(replies[2].text.contains("operator"));
    }

    #[tokio::test]
    async fn session_command_dual_writes_sql_binding() {
        let bot = TelegramBot::new(StubDaemon::new().with_session("s2"), &config(vec![1]));

        bot.handle(TelegramUpdate::Message {
            user_id: 1,
            chat_id: 42,
            text: "/session s2".into(),
        })
        .await
        .unwrap();

        let bindings = bot.daemon.bindings.lock().unwrap();
        assert_eq!(
            bindings.len(),
            1,
            "expected SQL dual-write, got: {bindings:?}"
        );
        assert_eq!(bindings[0].0, "telegram");
        assert_eq!(bindings[0].1, "42");
        assert_eq!(bindings[0].2, "s2");
    }

    #[tokio::test]
    async fn session_command_json_binding_persists_when_sql_dual_write_fails() {
        // SQL is a best-effort dual-write; the JSON binding must survive a
        // daemon SQL failure so the bot can still route messages.
        let path = temp_binding_path("dual-write-fail");
        let daemon = StubDaemon::new()
            .with_session("s2")
            .with_binding_error(DaemonError::Http("simulated SQL outage".into()));
        let bot = TelegramBot::new_with_binding_store(daemon, &config(vec![1]), path.clone());

        let reply = bot
            .handle(TelegramUpdate::Message {
                user_id: 1,
                chat_id: 7,
                text: "/session s2".into(),
            })
            .await
            .unwrap();
        // User-visible reply is the same — failure is best-effort.
        assert!(reply.text.contains("s2"));
        // JSON binding is persisted and survives the SQL failure.
        let persisted = load_telegram_session_bindings(&path).unwrap();
        assert_eq!(persisted.chats.get("7"), Some(&"s2".to_string()));
        // The bot's in-memory view is also active.
        assert_eq!(bot.session_for_chat(7), "s2");
        // And a follow-up message still routes to s2.
        bot.handle(TelegramUpdate::Message {
            user_id: 1,
            chat_id: 7,
            text: "continue".into(),
        })
        .await
        .unwrap();
        let submitted = bot.daemon.submitted.lock().unwrap();
        assert_eq!(
            submitted.last().cloned(),
            Some(("s2".into(), "continue".into()))
        );

        let _ = std::fs::remove_file(path);
    }
}
