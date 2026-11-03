# rightkit-autostart

Launch at login, runtime-neutral (no Tauri). Split out of `rightkit-tauri-shell` (rightkit#956); the shell re-exports it as `rightkit_tauri_shell::autostart` and `::login_item`.

- `autostart`: `AutostartSpec`, `set_enabled`, `status`, `migrate_plugin_entry`, `migrate_launch_agent`, `BACKGROUND_ARG`.
- `login_item`: macOS `SMAppService` status and `open_login_items_settings`.

Not published (0.1.0, unreleased).
