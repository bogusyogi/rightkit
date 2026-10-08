# rightkit-hotkeys

Cross-platform shortcut policy with native macOS Carbon hotkeys, CoreGraphics event taps, and a
Windows low-level hook. Ordinary chords use `GlobalHotkeys`; fn gestures, push-to-talk, and the
middle button use `FnHotkeyTap`/`MiddleMouseTap`. All native handles stop and join on drop.

macOS event taps need Accessibility + Input Monitoring. `tests/macos_e2e.rs` checks those grants,
then drives private synthetic events through real Carbon/CoreGraphics paths; it never emits audio.

## Windows

- Two mechanisms, both in `src/windows/mod.rs` and compiled only under `cfg(windows)`:
  - `GlobalHotkeys` runs a thread with a message loop and calls `RegisterHotKey(NULL, id, mods | MOD_NOREPEAT, vk)`. `WM_HOTKEY` is handled on that thread. Ctrl, Alt and Shift map directly, and Meta, Super and Win all map to `MOD_WIN`. Error 1409 becomes `RegisterError::Conflict`. Dropping unregisters every id and joins the thread.
  - `LowLevelKeyboardHook` installs one process-wide `WH_KEYBOARD_LL` hook on its own thread. A second install returns `HookError::AlreadyInstalled`. The handler runs inside the hook procedure and returns `Pass` or `Swallow` (LRESULT 1). `KeyEvent.injected` reports `LLKHF_INJECTED`. The module comment says the handler must stay fast and hand real work to another thread.
- There are no Windows `FnHotkeyTap` or `MiddleMouseTap` types. Those live in `src/macos/` only, so fn, push-to-talk and middle-button support is macOS-only at HEAD.
- Dependency: `windows` 0.61 under `cfg(windows)`, with features `Win32_Foundation`, `Win32_System_LibraryLoader`, `Win32_System_Threading`, `Win32_UI_Input_KeyboardAndMouse` and `Win32_UI_WindowsAndMessaging`. Consumers enable no Windows features.
- Not verified: the module header says it has not been built on the macOS host and must be checked on Windows before an app adopts it. Nothing here was compiled for Windows.
- Pending: `tasks/handoffs/2026-10-08/inventory/hotkeys-windows-sys.patch` migrates the crate to `windows-sys` 0.61.2 and rewrites the hook and hotkey code to match. It is not applied at HEAD.

