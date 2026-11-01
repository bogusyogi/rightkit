# rightkit-voice-commands

Voice command recognition: typed command actions, spoken-command catalog and matching, app-launch
and shortcut queries, external link allowlist check.

Moved into RightKit from HeardRight (`heardright_core`: `command`, `command_recognition`,
`external_url`) on 2026-11-01.

- `CommandAction`: the typed action a recognizer hands the host dispatcher (key sequences, app
  launch, macOS Shortcuts, mouse, power with confirmation, last-paste transforms, special ops).
- `command_recognition::recognize_command`: is this transcript a standalone command, and which?
  Direct phrases, spoken chords (`"control shift t"`), `open <app>` and `shortcut <name>` grammars,
  with fuzzy matching. `command_catalog` lists the discoverable commands for a help panel.
- `is_allowed_external_url(url, allowed)`: exact-match allowlist the host passes for links its
  webview may open.

Pure Rust, no OS side effects; dispatch stays in the host.
