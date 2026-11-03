//! Telegram long-poll loop and HTTP `DaemonClient` implementation.
//!
//! `run_telegram_bot` is spawned as a tokio task from `serve()` when
//! `config.remote.telegram_token` is set. It polls the Telegram Bot API for
//! updates, delegates decision logic to `TelegramBot` (from `coderight-remote`),
//! and sends replies back to Telegram - all without ever interacting with the
//! engine internals directly (only via the local daemon HTTP API).

use async_trait::async_trait;
use rightkit_agent_engine_protocol::ProtocolEvent;
use rightkit_bot_messaging::{
    DaemonClient, DaemonError, InlineButton, TelegramBot, TelegramConfig, TelegramReply,
    TelegramUpdate,
};
use rightkit_http::AsyncClient as Client;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::info;

include!("telegram/status.rs");
include!("telegram/client.rs");
include!("telegram/bot_api.rs");
include!("telegram/polling.rs");
