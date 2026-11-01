//! Sigils for tool/plugin mentions in plaintext, lifted from the Codex donor
//! (`codex_utils_plugins::mention_syntax` @ 444da310). The composer treats `$`
//! as a tool mention and `@` as a plugin/task mention prefix.

/// Default plaintext sigil for tools.
pub(crate) const TOOL_MENTION_SIGIL: char = '$';

/// Plugins use `@` in linked plaintext outside TUI.
pub(crate) const PLUGIN_TEXT_MENTION_SIGIL: char = '@';
