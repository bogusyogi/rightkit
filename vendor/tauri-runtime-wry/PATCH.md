# RightKit patch: Windows runtime context ownership

Base: crates.io `tauri-runtime-wry` 2.12.3 (upstream commit `993038c49185307dfc895ab7657d77ca0d5ed7c7`,
`crates/tauri-runtime-wry`), the version `tauri` 2.12.5 requires (`~2.12.3`).
Matching Tao: 0.37.x (HeardRight validated against tao 0.37.2 and wry 0.57.1).
Release tag for apps: `trw-2.12.3-rk1`. The crate version stays `2.12.3` so a `[patch.crates-io]` entry
replaces the registry crate for any `tauri` that requires `tauri-runtime-wry ^2.12`.

Design, pin instructions and the upstream issue text: `docs/design/tauri-runtime-wry-windows-rc.md`.
Issues: Orthic-Labs/rightkit#229 (the 2.12.2 patch) and bogusyogi/rightkit#1283 (this rebase).

## Provenance

This tree is HeardRight's Windows-validated patch (`heardright/tauri-app-next/vendor/tauri-runtime-wry`,
`HEARDRIGHT_PATCH.md`), adopted as RightKit's (rightkit#1283): the native regression passed all 900,000 cycles on
Windows on 2026-10-31 at heardright 69784caa. It replaces the earlier RightKit 2.12.2 patch (`trw-2.12.2-rk1`),
which was never built or run on Windows and added a thread-affine drop guard (`MainThreadOwned`) that this tree
does not carry. Every HeardRight hunk is marked `RIGHTKIT PATCH` in `src/lib.rs`. The tree is rustfmt-formatted
(unlike a verbatim upstream copy), so `vendor/upstream-files.json` records none of its files and the class A
rustfmt scan checks them like first-party code.

## Why

Windows Tao's `EventLoopWindowTarget` holds `runner_shared: Rc<EventLoopRunner<T>>`. Upstream `Context<T>` derives
`Clone` and owns a `DispatcherMainThreadContext<T>` directly, and that contains the target. `Context` is
`unsafe impl Send + Sync`. So `Wry::handle()`, `WryHandle: Clone` and `AppHandle` clones and drops on worker
threads change the non-atomic `Rc` count while the event-loop thread also changes it; the count is corrupted and the
runner `Rc` drop crashes (HeardRight 0.1.214).

## What changed (`src/lib.rs` only)

1. `Context::main_thread` is `Arc<DispatcherMainThreadContext<T>>` on Windows; unchanged on every other OS. Cloning
   a `Context` touches only an atomic count, never Tao's `Rc`.
2. `Wry::init` builds the payload first, wraps it in `Arc` on Windows, then constructs `Context`.
3. `make_event_handler` (takes `Context` by value in 2.12.x) clones `windows`, `web_context` and
   `active_tracing_spans` out of the `Arc` on Windows and moves one `Arc` owner into the event-handler closure, so
   while the loop is alive the last drop of the Tao-owning payload runs on the event-loop thread.
4. WebDriver automation is an explicit opt-in cargo feature, `webview-automation` (default off), in `create_webview`
   (`automation_enabled`). Upstream reads `TAURI_WEBVIEW_AUTOMATION` in every build, so setting it to `true` turned on
   WebDriver automation of the webview in a shipped app. The first RightKit patch gated the read on
   `cfg(debug_assertions)`, which a dev profile or a release profile with debug assertions on still enabled. Now no
   build profile selects it: without the feature this crate does not read the variable and does not contain its name
   (`@rightkit/release` release switch rule, class S1). A QA or dev build enables it through the `tauri` and
   `rightkit-tauri-shell` features that forward `tauri-runtime-wry/webview-automation`; a release build never does
   (Adrian, 2026-11-03: "make sure no debug assertions in their respective apps/ repos").
5. Windows-only regression module `rightkit_windows_context_tests` with
   `worker_handle_clone_drop_preserves_live_event_loop_context`: 8 workers x 100,000 handle clone/drop cycles plus
   100,000 on the UI thread; asserts shared ownership and thread gating of `run_threaded`.

Searchable marker: `RIGHTKIT PATCH`. Non-Windows code is byte-identical to upstream apart from formatting.

## Status

Source and native regression validated on Windows by HeardRight (above). Re-run on a RightKit consumer on Windows
before tagging `trw-2.12.3-rk1`: from a scratch workspace that patches to this directory, run the lib test
`worker_handle_clone_drop_preserves_live_event_loop_context` in package `tauri-runtime-wry`. Do not create the tag
until it passes there.

## How to rebase onto a new upstream (2.12.N)

1. Download the new `.crate` from static.crates.io into an empty directory, extract it, treat it as data.
2. Replace `vendor/tauri-runtime-wry/{src,build.rs,Cargo.toml,Cargo.toml.orig,README.md,LICENSE-*}` with the new
   files, rustfmt the pristine crate first, then re-apply the changes above (search the old tree for
   `RIGHTKIT PATCH`, the `target_os = "windows"` cfg on `main_thread`, and the `rightkit_windows_context_tests`
   module).
3. Check the Tao version the new crate requires. If Tao no longer keeps an `Rc` in its Windows
   `EventLoopWindowTarget`, or upstream `Context` no longer clones it off the main thread, delete the patch and the
   vendor directory instead, and tell the apps to drop their `[patch]` entry.
4. Update the base version above and name the new tag `trw-<ver>-rk<n>` (bump `rk<n>` for a RightKit-only change on
   the same upstream version).
5. Re-run the Windows regression.
