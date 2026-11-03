# Changelog

## 0.1.0 (unreleased)

- First release (rightkit#956): split out of `rightkit-tauri-shell` 0.7.3 with no behaviour change. The code is the shell's `instance`, `single_instance`, `legacy_hold`, `legacy_instance` modules moved unchanged, so apps without Tauri (an egui host, a C-ABI companion, a menu-bar helper, an overlay app) can use it. `rightkit-tauri-shell` depends on this crate and re-exports it at the old path. Source-level move only: not compiled in this pass.
