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

/// The file `<PREFIX>_TELEGRAM_BINDINGS` names when the host lets the environment move its state
/// stores (`host_options::state_store_paths_from_environment`; with that option off the variable
/// is never read), else `telegram-bindings.json` in the state root.
pub(crate) fn telegram_binding_store_path() -> PathBuf {
    if let Some(path) =
        crate::api_stores::state_store_override(crate::api_stores::StateStoreKey::TelegramBindings)
    {
        return path;
    }
    if let Some(root) = rightkit_agent_config::state_root() {
        return root.join("telegram-bindings.json");
    }
    PathBuf::from(rightkit_agent_config::state_dir_name()).join("telegram-bindings.json")
}
