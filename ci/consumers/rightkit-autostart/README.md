# rightkit-autostart 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-autostart` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`. Extra pinned-by-range test dependencies: `serde_json`.

Renders and parses login-entry text only: default options, macOS program arguments and LaunchAgent
plist, XDG desktop entry, Windows Run command, legacy tauri-plugin-autostart names, the Task Manager
StartupApproved flag, SMAppService status mapping and the pure decision rules. `status` is a read
against a config home that does not exist. `set_enabled`, the migrations, `register`, `unregister`
and `set_background_io` change login-item state and are never called.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
