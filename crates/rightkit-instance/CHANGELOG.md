# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-daemon` 0.5 -> 0.8. 0.1.1 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.1 in place (#1572). No source change in this crate since 0.1.1.

## 0.1.1
- Docs: intra-doc links rustdoc could not resolve (feature-only, private or out-of-scope items) are plain code, so the doc build (`-D warnings`) passes.

## 0.1.0 (unreleased)

- First release (rightkit#956): split out of `rightkit-tauri-shell` 0.7.3 with no behaviour change. The code is the shell's `instance`, `single_instance`, `legacy_hold`, `legacy_instance` modules moved unchanged, so apps without Tauri (an egui host, a C-ABI companion, a menu-bar helper, an overlay app) can use it. `rightkit-tauri-shell` depends on this crate and re-exports it at the old path. Source-level move only: not compiled in this pass.
