//! The Telegram transport is `rightkit-bot-messaging`'s (`telegram_poll`); the daemon picks
//! where the chat-to-session bindings are kept.

use std::path::PathBuf;

pub use rightkit_bot_messaging::telegram_poll::{
    new_telegram_runtime_status_store, TelegramChatCursorStatus, TelegramRuntimeStatus,
    TelegramRuntimeStatusStore,
};

/// Run the Telegram long-poll loop with the daemon's binding store.
pub async fn run_telegram_bot(
    bot_token: String,
    allowed_users: Vec<u64>,
    port: u16,
    daemon_token: String,
    runtime_status: TelegramRuntimeStatusStore,
) {
    rightkit_bot_messaging::telegram_poll::run_telegram_bot(
        bot_token,
        allowed_users,
        port,
        daemon_token,
        runtime_status,
        telegram_binding_store_path(),
    )
    .await;
}

/// `RIGHTKIT_TELEGRAM_BINDINGS`, else `telegram-bindings.json` in the state root.
pub(crate) fn telegram_binding_store_path() -> PathBuf {
    if let Ok(path) = std::env::var("RIGHTKIT_TELEGRAM_BINDINGS") {
        return PathBuf::from(path);
    }
    if let Some(root) = rightkit_agent_config::state_root() {
        return root.join("telegram-bindings.json");
    }
    PathBuf::from(".coderight").join("telegram-bindings.json")
}
