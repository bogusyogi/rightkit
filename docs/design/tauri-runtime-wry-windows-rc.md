# tauri-runtime-wry Windows Rc/Arc crash: shared patch source

Issue: bogusyogi/rightkit#229 (first seen in HeardRight, bogusyogi/heardright#177; all Orthic Tauri apps on
Windows are exposed). Date: 2026-10-17.

## 1. Did upstream fix it? No

Checked on the newest published 2.12 (`tauri-runtime-wry` 2.12.2, git sha `59b63ab5`, from
`https://static.crates.io/crates/tauri-runtime-wry/tauri-runtime-wry-2.12.2.crate`; 2.12.0 and 2.12.1 are earlier
and no 2.13 exists) and the Tao it resolves (0.37.0 in the crate's own `Cargo.lock`; 0.37.1 is the newest 0.37).
Sources were extracted to a scratch directory and read only.

Evidence, `tauri-runtime-wry-2.12.2/src/lib.rs`:

- `:234-246` `#[derive(Clone)] pub struct Context<T>` still holds `main_thread: DispatcherMainThreadContext<T>`
  directly (line 239), unwrapped.
- `:248-249` `unsafe impl Send/Sync for Context<T>` still present, so `Context` is cloned and dropped on any
  thread.
- `:453-460` `#[derive(Debug, Clone)] pub struct DispatcherMainThreadContext<T>` holds
  `window_target: EventLoopWindowTarget<Message<T>>`, which is Tao's target (and, under the `tracing` feature,
  `ActiveTraceSpanStore(Rc<..>)` at `:410`).
- `:2525-2528` `#[derive(Debug, Clone)] WryHandle { context: Context<T> }` and `:2829-2833` `Wry::handle()` does
  `self.context.clone()`. `AppHandle` clones reach the same chain.
- `:2771-2777` (init) constructs `Context` with the target inline: no `Arc`. The only `Arc` nearby is the
  `windows: Arc<WindowsStore>` field, which carries the comment "changing this to an Rc will cause frequent app
  crashes" (`:457`), so upstream knows this class of bug and has not addressed the target.

Evidence, Tao Windows:

- `tao-0.37.1/src/platform_impl/windows/event_loop.rs:163-169` `#[derive(Clone)] struct EventLoopWindowTarget` has
  `pub(crate) runner_shared: EventLoopRunnerShared<T>`.
- `tao-0.37.1/src/platform_impl/windows/event_loop/runner.rs:27`
  `pub(crate) type EventLoopRunnerShared<T> = Rc<EventLoopRunner<T>>;` and `event_loop.rs:196` creates it with
  `Rc::new(..)`. Tao 0.35.3 (what HeardRight resolved) has the identical alias at the same `runner.rs:27`.

So 2.12.2 plus Tao 0.37.x still clones the `Rc`-holding target whenever a `Context` is cloned off the main thread,
exactly as in 2.11.4. Nothing in 2.12.x replaces HeardRight's patch. (This is a source reading, not a runtime
reproduction on 2.12.2; the vendored regression test below is the reproduction.)

Consequence for the issue's alternative ask ("confirm upstream fixed it so apps can drop their patches"): not
available. Apps must keep a patch, now from one shared source.

## 2. What RightKit ships

`vendor/tauri-runtime-wry/` is upstream 2.12.2 with HeardRight's patch rebased onto it, plus the regression test
`worker_handle_clone_drop_preserves_live_event_loop_context`. See `vendor/tauri-runtime-wry/PATCH.md` for the
change list, why, and how to rebase. The 2.12 rebase is slightly larger than HeardRight's 2.11.4 patch because
2.12's `make_event_handler` takes `Context` by value and partially moves fields out of `main_thread`; the patch
clones them out of the `Arc` on Windows and keeps an owning `Arc` in the handler closure.

It is not a workspace member: root `Cargo.toml` `exclude` lists `vendor/tauri-runtime-wry`, and
`.gitattributes` marks the directory `-text` so upstream bytes are kept. It is not published to crates.io (we
cannot publish a crate named `tauri-runtime-wry` under our account), so it is a git `[patch]` source.

## 3. How every app pins it

Because RightKit's repository is the git source, the pin is by tag. Tag format `trw-<upstream ver>-rk<n>`.
First tag (not yet created, see section 5): `trw-2.12.2-rk1`.

In the app's workspace root `Cargo.toml` (the one that owns `Cargo.lock`):

```toml
[patch.crates-io]
tauri-runtime-wry = { git = "https://github.com/bogusyogi/rightkit", tag = "trw-2.12.2-rk1" }
```

Local development against a RightKit checkout (do not commit):

```toml
[patch.crates-io]
tauri-runtime-wry = { path = "../rightkit/vendor/tauri-runtime-wry" }
```

Rules:

- The patch is for every OS build of the app; on non-Windows targets the patched code compiles to the upstream
  representation. Keep one `[patch]` entry for all targets.
- Cargo applies a `[patch]` only if the patched version satisfies the requirement `tauri` resolves. The vendored
  crate keeps version `2.12.2`, so it replaces any `tauri-runtime-wry ^2.12` (2.12.2 resolved today). If an app is
  held on `tauri ~2.11` it needs the older HeardRight copy until it moves; the symptom of a stale pin is Cargo
  warning "Patch ... was not used in the crate graph", which an app's CI should turn into a failure (grep
  `cargo tree -i tauri-runtime-wry` for the git source).
- If `tauri` later requires `tauri-runtime-wry 2.12.3` or newer, the patch no longer applies (same warning). Rebase
  per `PATCH.md`, tag `trw-2.12.3-rk1`, and bump the apps' tags in one pass.
- Remove the patch (and the vendor directory) when a released `tauri-runtime-wry` fixes Windows context
  ownership. Run the vendored regression test against the replacement first.
- After pinning, `cargo update -p tauri-runtime-wry` in the app refreshes `Cargo.lock` to the git source.
  Apps vendoring their own copy (HeardRight `tauri-app-next/vendor/tauri-runtime-wry`, 2.11.4) delete it and its
  `[patch]` entry when they adopt this one.

## 4. Upstream issue / PR draft for tauri-apps/tauri (not filed)

Do not file without Adrian's yes. Suggested issue title and body:

> **[bug] tauri-runtime-wry on Windows: `Context` is `Send + Sync` and cloned on worker threads, but contains Tao's `Rc` event-loop runner (non-atomic refcount race)**
>
> **Versions:** tauri-runtime-wry 2.11.4 and 2.12.2, tao 0.35.3 and 0.37.1, Windows 11.
>
> **Summary:** `Context<T>` (`crates/tauri-runtime-wry/src/lib.rs`) derives `Clone`, is `unsafe impl Send + Sync`,
> and owns a `DispatcherMainThreadContext<T>` whose `window_target: EventLoopWindowTarget<Message<T>>` is Tao's
> Windows target. On Windows that target holds `runner_shared: Rc<EventLoopRunner<T>>`
> (`tao/src/platform_impl/windows/event_loop/runner.rs`, `EventLoopRunnerShared<T> = Rc<..>`) and is itself
> `Clone`. `Wry::handle()`, `WryHandle::clone`, `AppHandle::clone` and their drops therefore update the non-atomic
> `Rc` strong count from arbitrary threads while the event-loop thread updates it too. Concurrent updates lose
> counts, which later frees the runner early or double-drops it. We observed an access violation in the `Rc` drop
> of the runner reached from a hover-poll worker dropping an `AppHandle`.
>
> **Reproduction:** build a `Wry` runtime without showing a window, then run 8 worker threads each doing 100,000
> `handle.clone(); drop(clone)` while the UI thread does 100,000 `drop(runtime.handle())`. Checking the target's
> `Rc` count (or running under Miri/loom-style tooling) shows the race; in a real app it manifests as a
> crash in the runner `Rc` drop. A test is attached to the PR.
>
> **Proposed fix:** on Windows hold `DispatcherMainThreadContext` in an `Arc` inside `Context` so clones and drops
> on any thread touch only an atomic count, and have the event-loop handler keep an owning `Arc` so the final drop
> of the Tao-owning payload happens on the event-loop thread. Non-Windows representation is unchanged. The
> underlying alternative is for Tao's Windows target not to be `Clone` through an `Rc` (for example `Arc` for
> `runner_shared`), which would be a larger change in tao; happy to discuss which side you prefer.
>
> **PR:** the `rightkit_windows_context_tests` regression test plus the `Arc` change from
> `vendor/tauri-runtime-wry` (RightKit `trw-2.12.2-rk1`), under MIT/Apache-2.0 like the rest of the crate. Related
> prior art in the same file: the comment "changing this to an Rc will cause frequent app crashes" on
> `DispatcherMainThreadContext::windows`.

## 5. Needs a Windows build or test to prove

Nothing here has been compiled: this was written edit-only on 2026-10-17. Proof still needed before the tag
`trw-2.12.2-rk1` is created:

1. Compile the crate on Windows with and without the `tracing` feature (the `make_event_handler` change has
   `cfg(feature = "tracing")` and `cfg(target_os = "windows")` branches), and once for a non-Windows target
   (CI on Linux/macOS) to confirm the non-Windows representation is unchanged. `Some(&self.main_thread)` in
   `Context::run_threaded` relies on deref coercion from `&Arc<..>` to `&..` on Windows (as in HeardRight's
   2.11.4 patch, which compiled).
2. Run the lib test `worker_handle_clone_drop_preserves_live_event_loop_context` (package `tauri-runtime-wry`,
   Windows only) from a scratch workspace that patches `tauri-runtime-wry` to this directory. It needs a real
   Windows event loop but shows no window. Confirm it also fails (or crashes) against unpatched 2.12.2 if
   feasible, so the test is proven to detect the bug.
3. Build and smoke-run a real Tauri 2.12 app on Windows with the git pin: window opens, `AppHandle` use from a
   worker thread, clean exit (this exercises the closure-owned `Arc` drop in `make_event_handler`, which is new
   relative to HeardRight's validated 2.11.4 patch and the part least covered by the regression test).
4. Confirm `cargo metadata` in the RightKit workspace is unaffected by the new `exclude` entry, and that an app
   resolving the git `tag` finds the package under `vendor/tauri-runtime-wry` (Cargo scans the repo for the
   package by name).
5. Then create and push tag `trw-2.12.2-rk1` (needs Adrian's go-ahead for the push).
