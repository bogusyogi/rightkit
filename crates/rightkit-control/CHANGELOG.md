# Changelog

## 0.2.0

Minor bump: a default feature flipped (below). Consumers that relied on `wv2-0_38` being on by default must add it.

- **`wv2-0_38` is no longer a default feature.** `default` is now empty. Previously every Windows build pulled `windows` 0.61 and `webview2-com` 0.38 even without `tauri-plugin`, though only the `embedded` module (gated on `tauri-plugin`) uses them. The `wv2-0_38` and `wv2-0_39` features are unchanged.
- **`tauri-plugin` on Windows requires one `wv2-*` feature.** The existing `compile_error!` in `embedded/win_view.rs` enforces it and its message no longer calls `wv2-0_38` a default. Use `features = ["tauri-plugin", "wv2-0_38"]` (Tauri 2.11) or `["tauri-plugin", "wv2-0_39"]` (Tauri 2.12+). macOS builds are unaffected.
- The bundled `examples/control-test-app` now names `wv2-0_38` explicitly.
- Semver note: dropping a default is breaking for pre-1.0 users on Windows; the version stays 0.1.x only because `rightkit-qa` pins `rightkit-control = "0.1.8"`. Bump to 0.2.0 together with that requirement.
