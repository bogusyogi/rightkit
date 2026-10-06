# rightkit-hotkeys

Cross-platform shortcut policy with native macOS Carbon hotkeys, CoreGraphics event taps, and a
Windows low-level hook. Ordinary chords use `GlobalHotkeys`; fn gestures, push-to-talk, and the
middle button use `FnHotkeyTap`/`MiddleMouseTap`. All native handles stop and join on drop.

macOS event taps need Accessibility + Input Monitoring. `tests/macos_e2e.rs` checks those grants,
then drives private synthetic events through real Carbon/CoreGraphics paths; it never emits audio.

