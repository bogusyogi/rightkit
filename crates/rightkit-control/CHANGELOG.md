# Changelog

## Unreleased (after 0.2.0)

- Windows: a `devtools call timed out` error now says where the call stopped: never run by the UI thread (main thread blocked), or issued to WebView2 and then, after a `Browser.getVersion` probe, either "the browser process answers but the page did not complete it" (renderer busy or blocked, modal script dialog, paused debugger) or "the channel is unresponsive". The message still starts with `devtools call timed out`.
- Windows: opt-in tracing with `RIGHTKIT_CONTROL_TRACE=1` (stderr) or `=<file path>`: every devtools call with id, method, thread id and enqueue/dispatch/complete timing, and the webview's NavigationStarting, NavigationCompleted, ScriptDialogOpening, NewWindowRequested and ProcessFailed events. Off by default.
- Windows: stack capture on a devtools timeout (on with `RIGHTKIT_CONTROL_TRACE` or `RIGHTKIT_CONTROL_STACK_ON_TIMEOUT=1`). The V8 debugger is enabled when the webview is created, because `Debugger.enable` is not an interrupting command and would queue behind a busy script; on a timeout the harness sends `Debugger.pause` out of band, reads `Debugger.paused` through an event receiver, reports the top 15 frames (function, url, line, column) up to three times 300 ms apart in the timeout error and the trace, and resumes. No pause within 2 s is reported as renderer in native code or gone. Pauses nobody asked for (a `debugger;` statement) are logged and resumed at once. Frames are positions in the bundle the page loaded; a non-minified debug bundle keeps function names. macOS has no devtools protocol in WKWebView and is unchanged.

## 0.2.0

Minor bump: a default feature flipped (below). Consumers that relied on `wv2-0_38` being on by default must add it.

- **`wv2-0_38` is no longer a default feature.** `default` is now empty. Previously every Windows build pulled `windows` 0.61 and `webview2-com` 0.38 even without `tauri-plugin`, though only the `embedded` module (gated on `tauri-plugin`) uses them. The `wv2-0_38` and `wv2-0_39` features are unchanged.
- **`tauri-plugin` on Windows requires one `wv2-*` feature.** The existing `compile_error!` in `embedded/win_view.rs` enforces it and its message no longer calls `wv2-0_38` a default. Use `features = ["tauri-plugin", "wv2-0_38"]` (Tauri 2.11) or `["tauri-plugin", "wv2-0_39"]` (Tauri 2.12+). macOS builds are unaffected.
- The bundled `examples/control-test-app` now names `wv2-0_38` explicitly.
- Semver note: dropping a default is breaking for pre-1.0 users on Windows; the version stays 0.1.x only because `rightkit-qa` pins `rightkit-control = "0.1.8"`. Bump to 0.2.0 together with that requirement.
