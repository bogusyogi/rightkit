//! Telegram transport: the long-poll loop and the HTTP [`DaemonClient`] it drives.
//!
//! [`run_telegram_bot`] is spawned as a tokio task by the host server when a bot token is set.
//! It polls the Telegram Bot API for updates, hands each one to [`TelegramBot`] (the decision
//! layer in [`crate::channels::telegram`]), and sends the replies back to Telegram, all without
//! touching engine internals: it speaks only the local daemon HTTP API (`/sessions`,
//! `/remote/channels`, `/remote/bindings`). Where the chat-to-session bindings are kept is the
//! host's choice (`binding_store_path`). [`TelegramRuntimeStatus`] is what the host's status
//! route shows.

use crate::{
    DaemonClient, DaemonError, InlineButton, TelegramBot, TelegramConfig, TelegramReply,
    TelegramUpdate,
};
use async_trait::async_trait;
use rightkit_agent_engine_protocol::ProtocolEvent;
use rightkit_http::AsyncClient as Client;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::info;

include!("telegram_poll/status.rs");
include!("telegram_poll/client.rs");
include!("telegram_poll/bot_api.rs");
include!("telegram_poll/polling.rs");
