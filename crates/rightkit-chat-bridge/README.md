# rightkit-chat-bridge

Chat between AI chats: envelopes, linked computers and the device link, native delivery into running Claude and Codex chats (hooks, desktop), receipts, the chat skill and hook installer. The host app supplies its brand and state folder.

Moved into RightKit from Pulse (pulse-core chat/ and claude_chats.rs) on 2026-11-01.

## What the host app supplies

* `rightkit_app_host::install(...)` once at start: the state folder, the account home and the published settings (`hubLog` is read from them).
* `rightkit_chat_bridge::chat::brand::set_brand(Brand { ... })` once at start: the display name, the command agents run, install paths, wire identifiers and the text of the chat skill (`skill_markdown`; the bridge never ships a skill of its own). The default brand keeps the identifiers existing installs already use.

The Pulse-era test-only variables are now `RIGHTKIT_CHAT_SSH` and `RIGHTKIT_CODEX_BIN` (feature `test-overrides`).
