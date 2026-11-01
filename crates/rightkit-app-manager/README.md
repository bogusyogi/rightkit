# rightkit-app-manager

Installed-app management for desktop apps.

- `apps`: app inventory rows shared by every platform.
- `app_manager` (macOS/Unix): inventory with sizes and icons, Sparkle and Homebrew updates, uninstall that moves the app and its leftovers to the Trash (optionally through a privileged helper).
- `apps_windows` (Windows): installed programs, AppX packages, icons, running apps, usage and winget/Store updates.
- `process_control` (Unix): quit and force-quit by app bundle, refusing system processes and the host app itself.

The host app calls `set_host_identity`, `set_state_directory` and `set_privileged_helper_enabled` once at start.

Moved into RightKit from Pulse's shared core on 2026-11-01.
