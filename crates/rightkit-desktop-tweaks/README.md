# rightkit-desktop-tweaks

Desktop tweaks for Windows: Mac-style editing shortcuts. The left Alt acts as Ctrl for A, C, V, X and Z (Alt+Shift+Z as Ctrl+Y) and, when chosen, S, F, T and W; with the guards on the shortcuts stand down in terminals and full-screen programs, and Alt+C / Alt+V there use the terminal's own copy and paste chord. The layer is one handler on the shared hook hub of `rightkit-hotkeys` (`rightkit_hotkeys::windows::hub`), switched with `keys::apply(Shortcuts { .. })`. `keys::send_input` and `keys::paste` inject keys tagged so no hub handler sees them. Windows only; empty elsewhere. The application installs `services::Services` for logging.

Other global keys are not here: the hook, the modifier state, `reinstall` and `physically_down` are the hub's (`rightkit-hotkeys`); an application's own chords (screenshots, a clipboard burst, snippet keywords, a mouse wheel menu) are handlers it registers on the hub; the launcher's type-ahead is `rightkit_launcher_win::type_ahead`.

The Mac tweaks (Finder cut and paste, path field, window closer, dock-click minimise, window maximiser) are the planned Swift module `RightKitDesktopTweaks`; Pulse's Finder sync extension stays an app extension.

Moved into RightKit from `pulse-windows` (Pulse) `keys.rs` on 2026-11-01; reduced to the shortcut layer in 0.2.0 (2026-11-11).
