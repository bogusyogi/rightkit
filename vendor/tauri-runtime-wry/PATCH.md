# RightKit patch: Windows runtime context ownership

Base: crates.io `tauri-runtime-wry` 2.12.2
(`https://static.crates.io/crates/tauri-runtime-wry/tauri-runtime-wry-2.12.2.crate`).
Matching Tao: 0.37.x (2.12.2's `Cargo.lock` resolves 0.37.0; 0.37.1 is the newest 0.37).
Release tag for apps: `trw-2.12.2-rk1`. The crate version stays `2.12.2` so a `[patch.crates-io]` entry
replaces the registry crate for any `tauri` that requires `tauri-runtime-wry ^2.12`.

Design, pin instructions and the upstream issue text: `docs/design/tauri-runtime-wry-windows-rc.md`.
Issue: Orthic-Labs/rightkit#229. First seen in HeardRight (bogusyogi/heardright#177).

## Why

Windows Tao's `EventLoopWindowTarget` holds `runner_shared: Rc<EventLoopRunner<T>>`
(`tao-0.37.1/src/platform_impl/windows/event_loop.rs:168`, `Rc` alias at `event_loop/runner.rs:27`).
Upstream `Context<T>` derives `Clone` and owns a `DispatcherMainThreadContext<T>` directly, and that contains the
target (`src/lib.rs` `Context`, `DispatcherMainThreadContext`). `Context` is `unsafe impl Send + Sync`. So
`Wry::handle()`, `WryHandle: Clone` and `AppHandle` clones and drops on worker threads change the non-atomic
`Rc` count while the event-loop thread also changes it. The count is corrupted and the runner `Rc` drop crashes.

## What changed (`src/lib.rs` only)

1. `Context::main_thread` is `Arc<DispatcherMainThreadContext<T>>` on Windows; unchanged on every other OS.
   Cloning a `Context` now touches only an atomic count, never Tao's `Rc`.
2. `Wry::init` builds the payload first, wraps it in `Arc` on Windows, then constructs `Context`.
3. `make_event_handler` (in 2.12.x it takes `Context` by value and partially moves fields out of `main_thread`)
   clones `windows`, `web_context` and `active_tracing_spans` out of the `Arc` on Windows, and keeps one `Arc`
   owner inside the event-handler closure, so while the loop is alive the last drop of the Tao-owning payload
   runs on the event-loop thread, not on whichever worker drops the last runtime handle. (HeardRight's 2.11.4
   patch did not need this because 2.11.4's `make_event_handler` borrowed the runtime; the 2.12 signature
   changed.)
   Under `tracing` the closure reaches the span store through that owner instead of cloning its `Rc`.
4. Thread-affine drop guard `MainThreadOwned<V>` (Windows only): the `Arc` payload is
   `Arc<MainThreadOwned<DispatcherMainThreadContext<T>>>`. The guard records the creating (event-loop) `ThreadId`
   and keeps the value in `ManuallyDrop`. Its `Drop` runs the inner drop only on that thread; on any other thread
   it deliberately leaks the payload (Tao's `Rc` and the tracing `Rc` included), logs a `log::warn!`, and bumps
   `LEAKED_MAIN_THREAD_PAYLOADS`. This covers `run_return` (Tao drops the handler and its owner before
   `self.event_loop`, so a worker's last `WryHandle` can outlive the loop) and `Wry` dropped unrun or after
   `run_iteration` while a handle lives on a worker. Remaining behaviour: a leak at shutdown, never a racy `Rc`
   decrement. Residual: the leaked payload keeps Tao's runner `Rc` and window target alive for the process life.
5. New Windows-only test module `rightkit_windows_context_tests` with
   `worker_handle_clone_drop_preserves_live_event_loop_context`: 8 workers x 100,000 handle clone/drop cycles plus
   100,000 on the UI thread; asserts `Arc::ptr_eq`, strong count 1 afterwards, and thread gating of
   `run_threaded` (it does not run the loop). Plus `guard_drops_on_creating_thread_and_leaks_elsewhere` (guard
   unit test: inner drop runs on the creating thread, leaks elsewhere) and the `#[ignore]`d
   `run_iteration_with_worker_handles_leaks_off_thread_last_drop` (4 workers clone/drop while `run_iteration` runs,
   then a worker drops the last handle after the runtime; asserts no panic and a leak).

Searchable marker: `RIGHTKIT PATCH`. Everything else is byte-identical to the upstream crate (original
manifests, build script and licenses are retained; upstream `Cargo.lock` and `.cargo_vcs_info.json` are not
carried).

## Status

Written without a compile on 2026-10-17. Not yet built or run on Windows; see "Needs a Windows proof" in the
design doc. Do not create the tag `trw-2.12.2-rk1` until the regression test passes on Windows.

## How to rebase onto a new upstream (2.12.N)

1. Download the new `.crate` from static.crates.io into an empty directory, extract it, treat it as data.
2. Replace `vendor/tauri-runtime-wry/{src,build.rs,Cargo.toml,Cargo.toml.orig,README.md,LICENSE-*}` with the new
   files, then re-apply the five changes above (search the old tree for `RIGHTKIT PATCH`, the
   `target_os = "windows"` cfg on `main_thread`, and the `rightkit_windows_context_tests` module).
3. Check the Tao version the new crate requires. If Tao no longer keeps an `Rc` in its Windows
   `EventLoopWindowTarget`, or upstream `Context` no longer clones it off the main thread, delete the patch and
   the vendor directory instead, and tell the apps to drop their `[patch]` entry.
4. Update the base version above and name the new tag `trw-<ver>-rk<n>` (bump `rk<n>` for a RightKit-only
   change on the same upstream version).
   Re-record `vendor/upstream-files.json`: every verbatim `.rs` file under `files` with its new sha256, and
   `src/lib.rs` under `patched` with `base` (the new upstream file's sha256) and `sha256` (the patched bytes).
   The class A rustfmt scan checks any vendored file whose bytes no longer match its record (rightkit#915).
5. On Windows, from a scratch workspace that patches to this directory, run the lib test named
   `worker_handle_clone_drop_preserves_live_event_loop_context` in package `tauri-runtime-wry`.
