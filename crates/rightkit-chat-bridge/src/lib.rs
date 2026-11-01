//! Chat between AI chats: deliver messages into running Claude, Codex and hook-harness chats on
//! this computer and, over ssh links or the device link, on other computers.
//!
//! * [`chat`]: the envelope (`from`, `to`, `body`), linked computers, inboxes and receipts, the
//!   roster of chats, native delivery into Claude and Codex chats (hooks, desktop typing), the
//!   hub's heartbeat and control requests, the cross-user boundary, and the installer for the chat
//!   skill and the hooks. `chat::brand` holds everything the host app names: its command, its
//!   display name, install paths and wire identifiers.
//! * [`claude_chats`]: the Claude Desktop Code chats that were running, so they can open again
//!   after an account switch.
//!
//! The host app installs a [`chat::brand::Brand`] and a `rightkit_app_host::AppHost` once at
//! start; until then the bridge answers with neutral names and no state folder.

pub mod chat;
pub mod claude_chats;
