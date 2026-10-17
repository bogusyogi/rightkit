# rightkit-qa

> **Migrating from `rightkit-qa-driver` (0.6.0).** The embedded QA control server moved here, behind the dev-only `control` feature. It never ships: it carries the same release marker (`RIGHTKIT-QA-CONTROL-SERVER-PRESENT`) and `right-release` refuses it in both its binary and its source layer. `rightkit-qa-driver` 0.5.0 is a deprecated shim, so a version bump alone compiles; do the change below to leave it.
>
> In the app's `Cargo.toml`, replace
> `rightkit-qa-driver = { version = "0.4", optional = true, default-features = false, features = ["qa-control", "wv2-0_38"] }`
> with
> `rightkit-qa = { version = "0.6", optional = true, default-features = false, features = ["control", "wv2-0_38"] }`
> and the feature line `qa-native = ["dep:rightkit-qa-driver", "rightkit-qa-driver/qa-control"]` with
> `qa-native = ["dep:rightkit-qa", "rightkit-qa/control"]`. Keep `optional = true` and keep `qa-native` out of `default`. An app that already lists `rightkit-qa` under `[dev-dependencies]` keeps that line; the optional `[dependencies]` entry is additional. Feature names `wv2-0_38` and `wv2-0_39` are unchanged.
>
> Import changes (the only paths apps use today):
>
> | Before | After |
> |---|---|
> | `rightkit_qa_driver::embedded::Control` | `rightkit_qa::embedded::Control` |
> | `rightkit_qa_driver::embedded::ENV_SERVICE` | `rightkit_qa::embedded::ENV_SERVICE` |
> | `rightkit_qa_driver::embedded::{QA_CONTROL_MARKER, METHODS, SECRET_SELECTORS, MAX_BATCH_STEPS, effect_kind, Mouse, Button, PlacementGate, ViewportCfg, ViewportOutcome, ViewportWindow, PLACEMENT_EVENT}` | the same names under `rightkit_qa::embedded::` |
> | `rightkit_qa_driver::admission`, `events`, `lease`, `lease_store` | `rightkit_automation::control::{admission, events, lease, lease_store}` |
> | `rightkit_qa_driver::mac` | `rightkit_automation::macos::background` (macOS) |
> | `rightkit_qa_driver::offscreen` (launch, report types) | `rightkit_automation::offscreen`; the Tauri helpers `start_for_window` and `start_from_env` stay in `rightkit_qa_driver::offscreen` (feature `tauri-plugin`) |
> | `rightkit_qa_driver::json`, `keys` | `rightkit_automation::json`, `rightkit_automation::keys::appkit` |
> | `rightkit_qa_driver::webdriver`, `cli` and the `rightkit-qa-driver` binary | no replacement yet; they stay in the shim |
>
> Registration code is otherwise unchanged: `rightkit_qa::embedded::Control::<tauri::Wry>::new().build_if_enabled()` inside `#[cfg(feature = "qa-native")]`. `rightkit-qa guard` accepts either shape. Tier 2 of `rightkit-automation` is `rightkit_qa::in_app::ServiceBridge` (feature `control`), registered with `Automation::add_in_app`.

Rust QA harness for Right Suite applications. It owns declarative scenarios (`harness::Harness::scenario`), engine and app control (`control`; the in-app server behind the dev-only `control` feature), run locking and orphan sweeps, isolated app profiles and temp companions (`storage`), process handling, paid-provider mocks (`mock`), loudness, WAV, and PNG validators, and hashed evidence.

**Use it** in an app's end-to-end and UI tests, through `rightkit cargo test`, when the test must run the real engine or the installed app and record evidence.

**Do not use it** as a general test runner or as a substitute for unit tests. Scenarios that launch a real app need an interactive user session: a macOS login session (see the macOS UI section below) or a Windows desktop session (see the Windows section).

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `browser` | off | `rightkit-browser`, `tokio` | Chrome (CDP) targets for browser scenarios. Off by default because it pulls `tokio` and `chromiumoxide`. |
| `control` | off | `tauri` 2, `rightkit-daemon/test-mode` | QA ONLY, NEVER SHIPPED. The embedded Tauri control server (`embedded`) and the tier-2 `InApp` client (`in_app::ServiceBridge`). Emits the release marker `RIGHTKIT-QA-CONTROL-SERVER-PRESENT`; `right-release` refuses any build that carries it. |
| `android` | off | nothing extra | `android` module: `adb`-backed Android journeys (install the test APK, `am start` with test-mode extras, logcat by pid into the run dir, control-server port forward, force-stop and uninstall at teardown). Test packages only (`TestPackage`); device chosen by serial. Needs the Android SDK platform-tools (`ANDROID_HOME` or `ANDROID_SDK_ROOT`) at run time. |
| `wv2-0_38`, `wv2-0_39` | off | `windows` and `webview2-com` 0.38 / 0.39 (Windows only) | WebView2 generation for Tauri 2.11 / 2.12+; enable exactly one with `control` on Windows. |

Other dependencies include `rightkit-sidecar`, `rightkit-daemon`, `rightkit-child-process`, `getrandom`, and `png`. `rightkit-automation` supplies the foreground-app reads and the shared input and lease types; on Windows, `windows-sys` is a target dependency. The app under test links `rightkit-qa` with `control` (optional, from `qa-native`), not the harness (see the Windows section).

## Platform support

- macOS: UI launch through `open`, and companions under `/private/tmp/rightkit-qa/`. The `cfg(target_os = "macos")` branches are in `src/audio.rs`, `src/control.rs`, `src/storage.rs`, `src/util.rs`, `src/workspace.rs`, and `src/bin/rightkit-qa.rs`.
- Windows: real Tauri app UI driving through the app's embedded control server, with `rightkit-qa-driver` linked into the app. See [Windows: driving a real Tauri app](#windows-driving-a-real-tauri-app). The `cfg(windows)` branches are in `src/control.rs`, `src/process.rs`, `src/suite.rs`, `src/storage.rs`, `src/workspace.rs`, `src/scenario.rs`.
- Unix: `cfg(unix)` branches are in `src/process.rs`, `src/storage.rs`, and `src/userdata.rs`. Companions use the system temp directory.

Published version: see `INDEX.md` at the repository root. Release notes are in `CHANGELOG.md`.

## Ownership: this crate versus `@rightkit/qa`

Both runners stay. Pick by what the test drives.

| Surface | Owner | Evidence in code |
|---|---|---|
| macOS native app (a real `.app` launched from the login session) | `rightkit-qa` (`control::launch`, via `open` and the in-app `rightkit-qa-driver` server) | `cfg(target_os = "macos")` in `src/control.rs`, `src/storage.rs`, `src/audio.rs` |
| Windows WebView2 (Tauri) app | `rightkit-qa` (`control::launch` against the app's embedded control server on a named pipe; foreground checks via `GetForegroundWindow`) | `cfg(windows)` in `src/control.rs`, `src/process.rs`, `src/suite.rs`; the Windows section below |
| Engine (CLI words, framed stdio sidecar) | `rightkit-qa` (`engine::EngineTarget`) | `src/engine.rs` |
| Browser page (any web app in Chrome for Testing) | `@rightkit/qa` and `@rightkit/qa-chrome` (`launchChromeForTesting`, `QaBrowserSession`, `QaPage`). This crate reaches Chrome only through the optional `browser` feature (`rightkit-browser`, CDP), for Rust tests that need a page. | `packages/qa/src/page.ts`, `packages/qa-chrome/src/index.ts`; `src/browser.rs` |
| Pane, frame and paint checks on a browser page | `@rightkit/qa` (`panes`, `frames`, `paint`) | `packages/qa/src/index.ts` exports |
| Run storage, evidence bundles, run lock, owned-process cleanup | Both, each in its own language and format; evidence from one is not read by the other | `src/evidence.rs`, `src/lock.rs`; `packages/qa/src/evidence.ts`, `ownership.ts` |

A test chooses its runner by its target.

## Examples

`examples/control-test-app` and `examples/genright` are compiled examples. Run them through `rightkit cargo run -p rightkit-qa --example <name>`.

`sidecar_command` picks its frame shape from the sidecar's hello. A `contract_version` field means the rightkit-job-queue contract (`request {id, method, params}` → `response {id, ok, result | error}`); an `rpc_version` field means GenRight's legacy frames (`method:{method:"command", params:{name,input}}`, answered by `status: ok|err`). A hello with neither uses the jobs contract, and a silent engine then gets an error that names the shape. Set `legacy_status_frames = true` on `sidecar_start` or on one `sidecar_command` to force the legacy frames, or `legacy_status_frames = false` on either to force the jobs contract.

## Run storage

Run roots, harness evidence, captures & summaries remain under `<managed>/runs/<app>/<run-id>`. App homes, profiles, service/port files, live logs & app-provided output paths use an owned internal system-temp companion recorded as `appRoot` in `.rightkit-run.json`. On macOS, companions always use `/private/tmp/rightkit-qa/<app-prefix>-<random>` with short names so launchd can open app files & control socket paths fit `sockaddr_un`.

**Home retention and location.** Marker-driven pruning only reaches homes whose run record still exists. `workspace::create` also sweeps `<tmp>/rightkit-qa/` (at most every 10 minutes; `RIGHTKIT_QA_HOME_SWEEP=0` skips): homes with no run record or a finished run, older than 24 h, are removed, then the oldest finished homes while the total exceeds 4 GB. Live runs, young homes and directories without a `.rightkit-app.json` binding are kept. Run `rightkit-qa homes` to see what would go and `rightkit-qa homes --apply` to remove it (`--dir`, `--max-gb`, `--young-hours`). `RIGHTKIT_QA_HOME_ROOT=<absolute dir>` puts homes under `<dir>/rightkit-qa/`; the default stays internal because a launchd-started app cannot open its log on an external volume (`src/control.rs`, `launch`). The per-app retention lock only guards the prune pass: a run start never waits for it.

Finish copies app logs to managed `evidence/`, app-written evidence to `evidence/app/` & app captures to `captures/` before releasing ownership. Disposal removes temp companions; failed copies or cleanup preserve ownership metadata for recovery. Retention & orphan pruning validate companion bindings & protect live runs.

## Run kinds, evidence location and retention

**Two run kinds.** A run is `functional` or `visual`.

* `functional` is the default. It checks clicks, state and text and takes **no screenshots and no screen recordings**. A scenario step that would capture (`ui_screenshot`, `browser_screenshot`, a `screenshot` or `record*`/`capture*`/`video*` method inside `ui_batch`) is **refused** with an error that names the step, the scenario and how to change the kind; nothing is skipped silently, so the evidence never changes behind the author's back. The failure screenshot the suite used to take when a UI scenario failed is a capture too: functional runs do not take it.
* `visual` is the audit-visual run that captures screens (`ui_screenshot`, `browser_screenshot`, the failure screenshot).

Choose the kind per scenario with `kind = "functional"|"visual"` at the top of the `.scenario.toml`, or for the run with `rightkit-qa run --kind functional|visual` (`SuiteOptions::kind`). Resolution: `--kind` wins; without it the run is `visual` when any selected scenario declares `kind = "visual"`, else `functional`. A scenario that declares nothing takes the run's kind; one that declares `functional` stays functional inside a visual run; one that declares `visual` inside an explicit `--kind functional` run fails with a clear error (the explicit request is not widened). The kind is printed in the run header, written to `results.json`, bound into the evidence identity (`identity.runKind`) and recorded in the run marker. Plain Rust tests: `Harness::with_run_kind` records the kind (default `functional`); the harness itself never captures anything. Direct API calls (`Control::screenshot_png`, `Control::batch`) are not gated: the rule belongs to the scenario runner.

**Where evidence lives.** Each run writes only into its own directory `<managed root>/runs/<app>/<run-id>/` (the managed root is `RIGHTKIT_MANAGED_ROOT` or the platform default, never the app checkout). The `evidence_dir` key of `rightkit-qa.toml` and `QaConfig::evidence_root`, which pointed into the app checkout (`<app>/.cache/rightkit-qa`) and were not used for suite or harness evidence, are removed in 0.5.0; an old `evidence_dir` key is ignored. The paths the crate still touches inside an app repo are read-only inputs and ordinary build output: `.cache/qa/settings.env`, `.cache/e2e/settings.env` and `rightkit-qa.settings.env` (settings files, see below), and `target/` from `--build`/`--build-ui`. Apps must git-ignore them. Add this to the app's `.gitignore`:

```gitignore
# rightkit-qa: local settings and build output, never committed
/.cache/
/target/
/rightkit-qa.settings.env
```

**The run marker.** Every run directory carries `.rightkit-run.json`: `app`, `runId`, `kind` (`functional|visual`), `createdAt` (start, ms), `completedAt` (end, ms), `outcome` (`passed|failed`; `skipped` for a harness run in which every scenario skipped), plus the lease fields the run lock uses. Markers written before 0.5.0 have no kind and no outcome.

**Retention.** `retention::prune_runs(managed_root, app, dry_run)` applies one rule to `<managed root>/runs`, and `rightkit-qa prune-runs [--app KEY] [--root DIR] [--dry-run]` is the same call for a schedule (no `--app` prunes every app; no `--root` uses the managed root default):

* only a directory that carries a valid marker naming its own app and run id is ever considered; nothing else is touched or deleted, whatever it holds;
* every failed run is kept, a run with no recorded outcome (still running, or an older marker) and an active run are kept;
* of the passing runs, the newest per app and kind is kept, the other passing runs are deleted;
* a `skipped` run holds no evidence and is deleted;
* nothing outside `<managed root>/runs` is touched, a `runs` directory that is a link (or resolves elsewhere) is refused, an app or run directory that is a link is skipped and never followed, and a link inside a run is removed as itself.

The suite (`run_suite`) and the Rust harness run `retention::prune_runs_after_job` right after each job ends; it never fails the job. The older count and byte-cap prune at run start (`storage::prune*`, the `sweep` command) stays as the disk-safety valve, but it now keeps failed runs by the count rule too: only the byte cap can still evict one, oldest first.

## QA runs never touch real user data

A test run may never start an app on the user's real data or on a location that can raise a system dialog. `userdata::Guard` is evaluated by `control::launch` before anything is created or started, and every engine launch (`EngineTarget::cli`, `run`, `start_sidecar`, and the scenario `cli` / `sidecar_start` steps) goes through `userdata::check_engine_launch`. There is no flag to turn the app guard off; the one engine opt-out is the refusal probe below.

* The workspace home must be a QA root this runner created: directly under a `rightkit-qa` directory inside a system temp root on the internal disk (`/private/tmp`, the per-user temp dir, `RUNNER_TEMP`, the Windows temp dir), carrying the `.rightkit-app.json` binding. The launch working directory and every absolute path in the launch environment must resolve, symlinks followed, inside that home.
* Refused anywhere: the user's Documents, Desktop, Downloads, Pictures, Music, Movies; iCloud Drive (`~/Library/Mobile Documents`, `~/Library/CloudStorage`), OneDrive, Dropbox, Google Drive; the user's real application-support / AppData / `.config` directory of the app under test; any `/Volumes/*` volume; any Windows UNC path. `RIGHTKIT_QA_HOME_ROOT` or `RUNNER_TEMP` pointing at one of these is refused too.
* A violation is a `userdata::UserDataViolation { path, rule, detail }` (`rule` is a `userdata::Rule`); `?` turns it into `rightkit_qa::Error`. Nothing is launched.
* Engines (no workspace): `data_dir` and environment values whose key contains `DATA`, `HOME`, `PROFILE` or `CONFIG` (or is `APPDATA`, `LOCALAPPDATA`, `TMPDIR`) must be absolute paths inside a system temp root and clear of the rules above.
* **An engine launch must name an isolated data directory** (breaking change, see CHANGELOG "Unreleased"). The directory guard above judges only a directory it is given, so a launch that named none used to pass and the engine then opened its own default, the user's real per-user folder. On Windows that default comes from the Known Folder API, which ignores `HOME`, `USERPROFILE` and `APPDATA` in the child environment, so redirecting them does not help. Now a launch is refused before the process starts, with `UserDataViolation { rule: NoIsolatedDataDir }` naming the target, unless the target declares how it takes its directory and the launch supplies one that passes the guard:
  * `EngineTarget::data_dir_env` (config `[engine] data_dir_env`): `CliOptions::data_dir` is delivered through this variable, or the variable is set in the launch environment; or
  * `EngineTarget::data_dir_arg` (config `[engine] data_dir_arg`, for example `--data-dir`): the launch arguments carry `--data-dir <dir>` or `--data-dir=<dir>`.
  A `CliOptions::data_dir` on a target that declares neither is not enough: the engine would never receive it. An empty value counts as absent. Every named directory is judged, so a good variable does not excuse a bad flag.
* **Switches to the real library.** `EngineTarget::forbid_args(&["--use-user-library"])` (config `[engine] forbidden_args`) lists flags the product has for "use the real library". A launch whose arguments carry one, as `flag` or `flag=value`, is refused with `Rule::ForbiddenArgument`. The list is empty by default; this crate names no product flag. App launches take no arguments, so the list does not apply to them.
* **Refusal probe (the only opt-out).** A test of the engine's OWN refusal to run without a folder cannot name one. `EngineTarget::allow_no_data_dir_for_refusal_probe()` allows that launch, only where a second barrier exists, and never for any other purpose. Use it on a dedicated clone of the target, not the one that runs real scenarios.
  * Windows: refused outright (`Rule::ProbeUnavailable`). The Known Folder API ignores the environment, so nothing stands between the engine and the real folder.
  * Unix: the child environment (the target's `env` plus `CliOptions::env`) must set `HOME` to a directory that passes the guard (a QA home), and `XDG_DATA_HOME` / `XDG_CONFIG_HOME` must too wherever they are set in that environment or inherited from the test process. Otherwise `Rule::ProbeUnavailable`. This stops engines that resolve their folder from `HOME` / XDG variables; it is not proof for an engine that asks the system for the user's home directory instead, so assert the engine's refusal itself.
  * The directory rules and the forbidden-argument list still apply to a probe.
* **Apps.** `control::launch` also refuses (rule `NoIsolatedDataDir`) a launch environment that lacks `RIGHTKIT_QA_DATA_DIR`; the workspace always sets it, so only a hand-built workspace is affected.

Migration for callers that launched an engine with no data dir: pass an isolated directory (`CliOptions { data_dir: Some(dir), .. }` with `data_dir_env` declared on the target, a directory under the QA workspace such as `ws.data_dir` or `ws.scenario_dir(name)`), or, for a refusal test on Unix, use the probe with `HOME` redirected into the workspace. `EngineTarget` gained fields, so struct literals need `..EngineTarget::new(binary)` or the new fields; `EngineTarget::new(binary)` plus `with_data_dir_env`, `with_data_dir_arg`, `forbid_args` build one.

**Seed state with generated fixtures.** That is the default and needs nothing. When a scenario needs existing state, `userdata::seed_from(ws, app, source, dest_rel, transform, scrub)` runs the caller's `transform(source, dest)` into `<data_dir>/<dest_rel>`. A `source` that is the real per-user data of `app` (or in any refused location) is refused unless `scrub` is `Some(ScrubDeclaration { declared_scrubbed: true, verify })` and `verify(dest)` returns true on the copy before launch ("zero user rows, else no launch"). A false result deletes the copy and fails the seed.

**Permission dialogs.** `StopReport::permission_dialog()` reports a macOS dialog agent (`UserNotificationCenter`, `CoreServicesUIAgent`, ...) seen in the sampled foreground names: positive evidence only. `Control::assert_no_permission_dialog()` is a **no-op today**: `rightkit-qa-driver` lists windows per pid, not by owner name, so there is no reliable live detector (TODO in `userdata::live_permission_dialog_check`).

### QA home hygiene

Each home carries `.rightkit-home.json` (owner pid, start time). A home is removed when its run ends, pass or fail (`Harness` drop, `suite::run`, `workspace::dispose`); `Harness::with_keep_failed_home(true)` or `RIGHTKIT_QA_KEEP_FAILED_HOME=1` keeps only the failed run's home and prints its path and size. At run start, before the run is created, `storage::preflight_home_root` removes homes whose owner is dead and that are older than 2 hours (`RIGHTKIT_QA_HOME_MAX_AGE_SECS`; a live pid is never touched), then refuses to start with a `storage::RootOverCap` error if the run's own product holds more than `RIGHTKIT_QA_ROOT_CAP_BYTES` (default 4 GB, `0` disables) or all homes together exceed `RIGHTKIT_QA_ROOT_CEILING_BYTES` (default 32 GB). See "QA root limits" below. `RIGHTKIT_QA_HOME_SWEEP=0` skips the sweep. `storage::clean(dry_run)` / `rightkit-qa clean [--dry-run] [--max-age-hours N]` run the sweep by hand. Deletion happens only inside the `rightkit-qa` root, never by a caller's path, and never follows a symlink out of it. A harness that is never dropped (a `static`) leaves its home to the next run's sweep.

## Rust integration tests

`harness::Harness::scenario(name, tier, requires, |sc| ...)` wraps a `#[test]` body with tier gating, requires-skip, a per-scenario evidence directory (`<evidence root>/<slug>/evidence.json`, hashed artifacts), an orphan-process check and a `summary.jsonl` skip ledger. A skipped scenario prints `[qa] SKIP name: reason`, because libtest reports it as `ok`.

## Settings under `rightkit cargo`

`rightkit cargo test` starts the test binary with a scrubbed environment and refuses nested managed builds. Do not pass knobs as environment variables. Use this pattern instead:

1. Declare tiers as cargo features of the test crate (`full = []`, `paid = []`), and pass them with `Harness::with_feature_tiers(&[("full", cfg!(feature = "full")), ("paid", cfg!(feature = "paid"))])`. Run them with `rightkit cargo test -p my-e2e --features full,paid`. The `fast` tier always runs.
2. Put knobs in a `KEY=VALUE` settings file and read them through `settings::Settings` (`get`, `require`, `get_bool`, `get_u64`, `get_f64`, `get_list`, `get_path`). `requires = ["env:KEY"]` is answered from the same file.
3. Build binaries under test first with a separate `rightkit cargo build -p <bin>`, then locate them next to the test executable's `deps/` directory.

Search order (the first existing file wins; files are not merged):

1. `RIGHTKIT_QA_SETTINGS` (a path; an error if it is set and the file is missing)
2. `<root>/.cache/qa/settings.env`
3. `<root>/.cache/e2e/settings.env`
4. `<root>/rightkit-qa.settings.env`

A non-empty process environment variable overrides the file for a single key. This only applies to direct, unmanaged settings. The harness reads `RIGHTKIT_QA_TIER` to override tier selection (`Harness::with_tier_key` renames it); legacy `RIGHTKIT_QA_EVIDENCE` is accepted by settings for compatibility but managed harness runs always write evidence under their managed run root.

## UI scenarios against a real app (macOS)

Scenarios that launch a real `.app` (`control::launch`) must run from the user's login session. Under `rightkit cargo test`, the test binary runs in the build broker's context, where `open` has no GUI session and exits 1. Build through the broker and run the binary yourself; `scripts/run-ui-tests.sh` does both and keeps your shell's environment:

```sh
rightkit/scripts/run-ui-tests.sh -p cutright-e2e --test ui -- --nocapture
```

On macOS, `control::launch` stages raw binaries & supplied `.app` bundles under `<appRoot>/apps/<launch-id>/`, then passes every harness-owned home/temp/profile path through `open --env NAME=VALUE`. `HOME`/`USERPROFILE` point to `appRoot`; data/profile keys point to `appRoot/data`; `TMPDIR`/`TMP`/`TEMP` point to `appRoot/tmp`. `RIGHTKIT_SUITE_ROOT` points to `appRoot/suite-<launch-id-prefix>` (including `runtime/<service>/service.json`); live stdout/stderr use `appRoot/app-<launch-id-prefix>.log`. The launcher cwd & harness scratch directories also use `appRoot`.

Caller-supplied `LaunchSpec::env` still rejects credential keys & allows only `RIGHTKIT_*` keys on macOS. Absolute caller paths must live inside `appRoot`; callers cannot replace suite/service identity or move isolation paths outside it. `${data_dir}` & `${evidence_dir}` scenario templates use internal companion paths; artifacts are copied into managed evidence before hashing so disposal cannot invalidate them. App-specific QA knobs should use `RIGHTKIT_<APP>_*` keys honoured only in debug QA builds.

Focus for keyboard and accessibility scenarios: a hidden window is never key, so `:focus`, `:focus-visible` and `document.hasFocus()` do not match. Opt in with `focus = true` on `ui_launch` (or `[ui] focus = true`, or `LaunchSpec { focus: true, .. }`); default is off. After the page is ready the harness calls the control method `focus` (also `Control::focus_window`). macOS: activates the app, makes the window key and the webview first responder, and leaves the window's frame and alpha alone, so an off-screen window stays off-screen; activation can change the frontmost app, so a focused launch does not claim `owned_never_frontmost` and `ui_stop` skips that proof. Windows: enables Chromium focus emulation (`Emulation.setFocusEmulationEnabled`) and moves WebView2 focus into the page; there is deliberately no `SetForegroundWindow`, so the user's foreground window is untouched. The launcher also sets `RIGHTKIT_QA_FOCUS=1`.

Hidden launches: the app must not take focus itself. When `RIGHTKIT_QA_HIDDEN=1`, skip any `set_focus()`/`show()` on startup, and build with `activate_ignoring_other_apps(false)` on macOS (see `crates/rightkit-qa/examples/control-test-app/src/main.rs`). Compare paths with `std::fs::canonicalize`: the QA data dir lives under `/var/folders`, which macOS resolves to `/private/var/folders`.

`Control::wait_for_text(Some("#status"), "Ready", timeout)` waits for visible text (`None` searches the whole body).

## Finding the dev build

`right-release dev` builds into the RightKit build broker's own target root, and the managed test runner scrubs the environment, so a journey cannot assume `target/debug/<app>`. `QaConfig::resolve_ui_binary()` (module `binary`) returns the first valid of: `RIGHTKIT_QA_APP_BINARY` / `RIGHTKIT_QA_UI_BINARY` / `ui.binary_env`; the pointer file `.rightkit-qa-app-binary` (or `ui.pointer_file`) under the app root, one path on its first non-empty line; the newest dev-profile executable (`release-iterate`, `debug`, `dev`) in the broker workspace `<managed root>/rightkit-build-control/ws/<key>/g/<gen>/<t|target>/[<triple>/]<profile>/` whose `identity.json` `canonicalRoot` is the app's cargo workspace (`ui.workspace_root`, default `src-tauri` then the app root); then `ui.binaries`. The result carries a `BinarySource` saying which won. Add the pointer file to the app's `.gitignore`. The broker's receipts name no artifact paths, so the target tree is what is read. A `release` directory is never accepted. Every candidate must also carry the control marker `RIGHTKIT-QA-CONTROL-SERVER-PRESENT` (Latin-1 or UTF-16LE): with one dev profile a plain dev build and the control build both write `target/<triple>/debug/<bin>`, so a binary without the marker is skipped (broker, app root) or an error (env, pointer). `--build-ui` copies the built executable (and a sibling `.pdb`) to `<app root>/.rightkit-qa-bin/<triple-or-host>/<bin>` (outside the broker target tree, which is garbage-collected) through a temp file and rename, and writes `.rightkit-qa-app-binary` under the app root pointing at it. **Git-ignore `.rightkit-qa-app-binary` and `.rightkit-qa-bin/` in the app** (the crate never edits an app's `.gitignore`).

### Naming the binary yourself

`SuiteOptions::ui_binary`, `--ui-binary` and `RIGHTKIT_QA_UI_BINARY` name the app executable and skip the resolver's checks (file name, `release` directory, QA marker): the caller said which file. They do not skip the installed-app refusal. A path under `/Applications`, `~/Applications`, `ProgramFiles`, `ProgramFiles(x86)`, `ProgramW6432` or `%LOCALAPPDATA%\Programs` is refused by `suite::explicit_ui_binary` before any scenario starts, unless installed-QA mode is on (`RIGHT_RELEASE_QA_STAGE=installed`, set by `right-release qa --stage installed`).

## Scenario steps for seeded state and accessibility

- `engine_seed` seeds engine state before the app starts, in a `target = "ui"` scenario. `commands` is an array of `cli` step tables (`command` or `args`, `input`, `env`, `allow_error`, `timeout_s`), run in order against the scenario's data dir, which `ui_launch` then hands the app. The step is refused once the app is running. A `ui` scenario that uses it needs the engine binary (`--engine`, `RIGHTKIT_QA_ENGINE`, `[engine].binary` or `--build`); one that does not is unchanged. Each command's transcript is an evidence log (`NN-seed-MM.log`).
- `ui_focus_ring` reports whether the focused element shows a visible indicator: `focused`, `visible`, `kind` (`outline`, `box-shadow`, `none`), `reason`, `focus_visible`, the element's `tag`, `id`, `role`, `name`, and the computed `outline` and `box_shadow`. `selector` adds `matches`; `screenshot = true` saves a capture as evidence (visual scenarios only; credential elements are masked by the control server). Assert with `[[step.expect]]` on `visible`.
- `ui_contrast` measures a selector's text colour against its effective background (the element's and its ancestors' `background-color` over a white canvas, the element's opacity applied) with the WCAG 2.x ratio. It fails below `min`, else the `level` threshold (`AA` default: 4.5, or 3.0 for large text; `AAA`: 7.0, or 4.5). Large text is read from the font size and weight unless `large` is set. `report_only = true` reports `ratio`, `passes`, `fg` and `bg` without failing. A background image is flagged (`background_image`) because it cannot be measured here.
- `ui_assert_ax` asserts over the `ax` nodes: `role` (case-insensitive), `name` (exact after whitespace collapse), `name_contains`, `named = true` (non-empty name), `disabled`, `min_count` (default 1) and `max_count`. A miss lists the roles and names that exist.

The same checks are available to Rust tests as `rightkit_qa::a11y` (`contrast_ratio`, `parse_css_color`, `focus_ring_verdict`, `AxQuery`, `ax_assert`).

## Tauri custom-protocol build with an external engine

A Tauri app that serves its UI over the custom protocol and starts a separate sidecar engine needs two builds, in this order. `examples/tauri-custom-protocol/` holds the config and a scenario.

1. Build the frontend into `build.frontendDist`.
2. `rightkit-qa run --build` builds the engine from `[engine].build`.
3. `rightkit-qa run --build-ui` builds the app from `[ui].build` with `--features qa-native,custom-protocol`. Without `custom-protocol` the debug build points at `build.devUrl` and a dev server that a hidden run does not have, and the window stays blank. `qa-native` adds the control server (never in a shipped build). The built executable is copied to `<app root>/.rightkit-qa-bin/<triple>/<bin>` and `.rightkit-qa-app-binary` points at it.
4. Give the app the engine's path on `ui_launch`: `env = { POSTRIGHT_ENGINE_PATH = "${engine}" }` (`${engine}` is the engine binary the run resolved). An app that finds its sidecar next to its own executable needs that copy made by its own QA build step.

`rightkit-qa run --build --build-ui` does steps 2 and 3 in one run, engine first. Or build both yourself and pass `--engine` and `--ui-binary`.

## Windows: driving a real Tauri app

The Windows leg drives a real Tauri app through the app's own in-app control server. Pulse's hub runs it on its Windows CI runner (`scripts/gate.sh` in the Pulse repository, test `hub/qa-e2e/tests/ui.rs`), so the pattern is in use, not only planned.

### App side: the `qa-native` feature

**The harness drives only builds compiled with `control`.** `rightkit-qa` is an internal development tool, not a user automation surface. The app-side half is the embedded server (`rightkit_qa::embedded`), which exists only in a binary built with the explicit `rightkit-qa/control` feature, which only an app's `qa-native` feature may enable. A normal build has no server, so this harness can neither launch-and-control it nor attach to it, and `right-release` refuses to seal or upload a release or update build that carries one. The only supported automation surface for users is the app's own command line or plugin. See `docs/CONTROL-SERVER.md`, "QA control never ships".

Pulse's hub uses this pattern (`hub/src-tauri/Cargo.toml`, `hub/src-tauri/src/lib.rs`):

- A feature, `qa-native = ["dep:rightkit-qa", "rightkit-qa/control"]`, with `rightkit-qa` optional and declared with `default-features = false`. It is off by default, and no default feature may reach it. Dev lanes enable it explicitly (`--features qa-native`); release and update lanes never do. A dev build may contain the server, including a release-derived profile such as `release-iterate`, so the registration needs no `debug_assertions` condition.
- `rightkit-qa` gets the `control` feature and exactly one `wv2-*` feature on Windows. Use `wv2-0_38` for Tauri 2.11 and `wv2-0_39` for Tauri 2.12+. The crate's `compile_error!` in `src/embedded/win_view.rs` rejects a build with neither. The `wv2-*` feature must match the `windows` and `webview2-com` generation the app's Tauri resolves, so WebView2 types cross the Tauri boundary. Pulse selects `wv2-0_39`.
- The plugin is registered only inside a `#[cfg(feature = "qa-native")]` block (the older `#[cfg(all(debug_assertions, feature = "qa-native"))]` form is still accepted). The gate may sit on the statement or on the function or block that contains it:

```rust
if let Some(plugin) = rightkit_qa::embedded::Control::<tauri::Wry>::new().build_if_enabled() {
    builder = builder.plugin(plugin);
}
```

**Contract check.** `rightkit-qa guard [--backend control|wdio]` (`src/guard.rs`) reads the app's `Cargo.toml` and Rust sources and fails on: a registration with no `qa-native` gate (`use` lines and comments are ignored); a `rightkit-qa` (or legacy `rightkit-qa-driver`) dependency that is not optional, or that enables `control` (`qa-control`) without being optional; a missing `qa-native` feature, or one that does not enable `dep:rightkit-qa` and `rightkit-qa/control` (or list `control` in the dependency's features); `qa-native` reachable from `default`, directly or through another feature; any other feature that enables the dependency; a missing runtime flag; and a release or build package script that enables `qa-native`. It also rejects rightkit-tauri-shell developer inspection in a shipped build: `dev-inspect`, `rightkit-tauri-shell/dev-inspect` or `dev-native` reachable from `default`, a non-optional `rightkit-tauri-shell` dependency that lists `dev-inspect`, and a release-named package script that enables `dev-inspect` or `dev-native`. Each message names the file and the change to make. The check cannot prove that a release build lacks the server; `right-release`'s marker scan does that.

- `build_if_enabled()` returns `None` unless the launcher sets `RIGHTKIT_CONTROL_SERVICE`, so a normal launch of a debug build does not listen. When enabled, the server is a `rightkit-daemon` host: a discovery record at `<suite>/runtime/<name>/service.json`, a current-user named pipe on Windows, no TCP listener and no bearer token. It fails closed when no absolute suite root exists.
- Input goes to the webview from inside the process (on Windows through WebView2's `Input.dispatch*`), not through OS input events.
- Apps still on `rightkit-qa-driver` (Pulse pins an older version) use the legacy feature `qa-control`; check the feature names against the version the app pins.

### Harness side

- Launch with `control::launch(&LaunchSpec { binary, mode: Mode::Hidden, env, startup_timeout, label, focus: false }, &ws, sc.tracker())`, then `Control::wait_eval(js, timeout)` until the app's shell is up (Pulse waits for `nav.rk-nav`). `Control::eval` and `wait_for_text` read the page. `wait_eval`/`wait_for_text` wait in the page in one round trip (an app on a driver older than 0.4 is polled every 25 ms instead), `Control::batch(&[(method, params)])` and the `ui_batch` scenario step send many steps in one round trip, `screenshot_png` receives the image inline, and `RIGHTKIT_QA_PACE_MS` or `Control::set_pace_ms` restores a gap between input events (default none). `examples/control-test-app/bench/` holds the fixed latency benchmark scenarios.
- Gate each scenario with `requires = ["platform:windows"]` or `platform:darwin`. A scenario whose requirement is not met prints its `[qa] SKIP` line.
- Hidden launch: `Mode::Hidden` sets `RIGHTKIT_QA_HIDDEN=1` and `RIGHTKIT_QA_BACKGROUND=1` in the child environment (`launch_env` in `src/control.rs`). Pulse runs the hidden launch on a Windows runner (its hub journey drives a hidden WebView2 through the embedded control server). On Windows the harness samples the foreground window's owning pid every 20 ms (`GetForegroundWindow` + `GetWindowThreadProcessId`) and `StopReport::owned_never_frontmost` is true only when no sample found the started pid in front and no foreground owner was unreadable; a moment with no foreground window counts as nothing owned in front. It checks the started pid only, not helper processes, and does not stop the app taking focus (that is the app's job, below). Pulse's hub skips `set_focus()` under the flag but still calls `show()` on its main window, so do not read "hidden" as "no window".
- Isolated data dir: `workspace::create(&sc.scratch("ws"), None, "<app-key>")` puts HOME, USERPROFILE, LOCALAPPDATA and WEBVIEW2_USER_DATA_FOLDER inside the workspace. On Windows those paths come back in `\\?\` verbatim form, but WebView2 and the Win32 settings code take plain drive paths only. Pulse rewrites `ws.home`, `ws.data_dir` and each `ws.env` value with the prefix removed (`plain_workspace` in `tests/ui.rs`). Expect the same in any app that hands these paths to WebView2 or Win32.
- App-specific knobs go through `LaunchSpec::env` under `RIGHTKIT_<APP>_*` keys. Pulse passes `RIGHTKIT_PULSE_QA_HOME`, which the debug QA build reads to set its HOME and USERPROFILE.
- `hub/qa-e2e` is its own cargo workspace (`[workspace]` in its `Cargo.toml`). rightkit-qa declares `windows-sys` 0.61.2 as a caret requirement (`Cargo.toml:47`, not an exact pin), and Pulse's comment says its Windows crate pins do not resolve next to the hub's `pulse-core`. Pulse pins `rightkit-qa` exactly in that manifest; it is held at `=0.2.12` until a Windows run on 0.2.16 passes.

### Running

- CI (Pulse `scripts/gate.sh`, `run_hub_qa`): build the debug app with `cargo build --features qa-native,custom-protocol` in `hub/src-tauri`, then run `cargo test --test ui -- --nocapture` in `hub/qa-e2e` with `RIGHTKIT_MANAGED_ROOT`, `PULSE_QA_SHOTS` and `RIGHTKIT_QA_EVIDENCE` set. The gate calls plain `cargo` because its runner has no `rightkit` CLI. That is the gate's stated exception, not the local rule.
- The gate does not accept a silent pass. A skipped scenario exits `ok` (see Rust integration tests), so the gate also requires a `"passed"` receipt in `evidence.json` and at least eight screenshots (so those scenarios are `kind = "visual"`; a functional run takes none).
- A hidden WebView2 app cannot be launched from a non-interactive session (SSH, a service, session 0 or a non-visible window station). The app process starts and the control server may answer, but the webview never produces a ready window, so `control::launch` ends with `app window never became ready` after `startup_timeout`. On Windows that error now says when the harness itself is in such a session. Start the run from the logged-in user's desktop session instead, for example a one-shot scheduled task created with `schtasks /create /it` (interactive token), run once, and deleted afterwards. Do not read the timeout as an app defect before checking the session.
- Local host: start the run from the user's interactive desktop session, not over SSH. Over SSH use `rightkit-qa-interactive` (next bullet).
- **From SSH (session 0): `rightkit-qa-interactive`.** `RIGHTKIT_QA_INTERACTIVE=1 rightkit-qa-interactive --timeout-secs 1800 --pass-env FOO -- <journey program> [args...]` runs the whole journey command (harness and app together, so window-ready checks, pid tracking and the control server share one session) in the logged-on user's session through a one-shot Task Scheduler task, streams its stdout and stderr and exits with its exit code. In an interactive session, or off Windows, it just runs the program. Without the opt-in a session-0 run fails with the clear session-0 error. `rightkit-qa-interactive status` prints the session facts and the decision.
  - The task is `\RightKit\QA\<uuid>`, `LogonType InteractiveToken`, `RunLevel LeastPrivilege`, for the current account, no triggers (started by `/run`), with a hard time limit. No password is stored or asked for. It runs only when the SSH account is the account logged on at the console; otherwise the run refuses with a clear error. The task and its temp files are removed on success, error, timeout and panic, and a still-running journey is killed by pid. A hard kill of the parent process cannot run that cleanup: delete a leftover with `schtasks /delete /tn "\RightKit\QA\<uuid>" /f` (the task's time limit bounds it meanwhile).
  - The task runs `rightkit-qa-interactive __child <spec.json>`, not a shell: the program and its arguments are JSON data spawned as argv, so a journey argument is never parsed by `cmd.exe` or a shell. A `.cmd` or `.bat` program is still started through `cmd.exe` by the OS, with Rust's argument escaping.
  - Scheduled tasks do not inherit the SSH environment. Forwarded: every `RIGHTKIT_*` variable, each `--pass-env NAME`, and each `--env K=V` (explicit wins). Everything else comes from the logged-on user's own environment.
  - Git Bash cannot execute `rightkit.cmd`. That shim is not generated in this repository; the POSIX `rightkit` sh shim belongs next to it in the tool that installs the CLI.

### Limits and gaps

- The harness drives webview windows only. Pulse observed that native-drawn surfaces are outside its reach: the Windows notch draws its panels on the desktop rather than in a webview, so the control server cannot drive them.
- Pressed-state checks on Windows are advisory. Pulse's `tests/ui.rs` reports a control with no pressed state as advisory, because whether a hidden WebView2 produces `:active` is unmeasured.
- Installed-package lane: no harness path yet launches the installed Windows app and binds its executable identity into evidence. Today's runs are real-app evidence from a debug build and must be labelled that way. Gap ledger row 56 (`docs/audits/2026-10-10-rightkit-gap-ledger.md`) is open; its decision is an installed-package lane in rightkit-qa 0.3.0 (launch into the interactive session, identity binding in evidence).

## Mocks and validators

- `mock::MockServer::start_with(|req, base| Reply)` serves dynamic, stateful and binary (`Reply::bytes`) responses for paid-provider contracts. `MockServer::start(routes)` keeps the static JSON routes; `MockServer::start_static(vec![(method, path, Reply)])` serves static binary replies.
- Every `200` reply to `GET`/`HEAD` honours a single HTTP `Range` (`bytes=a-b`, `a-`, suffix `-n`): `206` with `Content-Range`, `416` when unsatisfiable, the full `200` when `Range` is absent, malformed, multi-range or its `If-Range` does not match the reply's `ETag`. `Reply::ignore_range()` models a server without resume; `Reply::cut_after(n)` drops the connection after `n` body bytes.
- `loudness`: pure-Rust ITU-R BS.1770-4 integrated loudness, EBU loudness range, sample peak and true peak. It is checked against ffmpeg `ebur128` to within 0.2 LU.
- `media`: WAV decode and stats, PNG non-trivial (pure Rust), and decoded-frame non-uniformity. ffmpeg is only needed to decode compressed audio and video, and its binary path is always passed in.

The browser target reaches a local dev server only through `browser::allow_loopback_origin` / `BrowserTarget::launch_for_origins`. That allowance is scoped to one scheme, host and port.

Publication status is tracked in `INDEX.md`.

## Runtime budget

Generated from `runtime-budget.json`; edit that file, then run `node packages/release/runtime-budgets.mjs sync <crate-dir>`.

- Resident memory ceiling: 45.0 MiB.
- CPU ceiling (idle, steady state): 1%.
- Wake-ups ceiling: 1 per second.
- Disk operations ceiling: 1 operations per second.
- Required scale: 10,000 commands or more.
- Required hosts: macOS and Windows.
- Status exposure: `Sidecar::status()` in `src/engine.rs`; exposes resident memory, idle CPU, wake-ups, disk operations.
- Measured: not yet measured.

## QA root limits

Homes live under `<tmp>/rightkit-qa/` (`/private/tmp/rightkit-qa` on macOS). Two limits are checked when a run starts, after the dead-home sweep.

- **Per-product cap** (`RIGHTKIT_QA_ROOT_CAP_BYTES`, default 4 GB, `0` disables). A run for app X is refused only when X's own homes hold more than the cap. A home belongs to the product named by the `app` in its `.rightkit-app.json` binding, else by the label in its directory name (`<app cut to 16 characters>-<suffix>`). Other products' homes never count, so another app's leftovers cannot block a run.
- **Root ceiling** (`RIGHTKIT_QA_ROOT_CEILING_BYTES`, default 32 GB, `0` disables). Every run is refused when all homes together exceed it, because the disk is at risk. The refusal names the largest homes with their product. The cap is checked first.

A refusal says which product's homes it counted, the total, the largest homes, and that `rightkit-qa clean` removes the homes of dead runs. It is a `storage::RootOverCap` (`kind`, `app`, `total_bytes`, `home_count`, `limit_bytes`, `largest`); `?` turns it into `rightkit_qa::Error`.

**Setting limits in code.** A test run under `rightkit cargo` does not receive the environment variables, so set them on the harness: `Harness::new(..)?.with_root_cap_bytes(8_000_000_000).with_root_ceiling_bytes(0)` (`0` disables a limit), or `workspace::create_with_limits(.., storage::RootLimits::default().with_cap_bytes(..))`. A value set in code wins over the environment variable; a limit left unset falls back to the variable, then the default. The harness sweeps when it is built and checks its limits once, at the first scenario that runs (a skipped scenario does not check), so the setters take effect when chained after `new`. A refused scenario writes a failed evidence record, removes its own home and panics with the refusal.

**The sweep removes more.** `storage::clean_homes` (run at every start, and by `rightkit-qa clean`) removes a home that has no `.rightkit-home.json` owner marker (written before it existed, or missing a file) when all of these hold: it is a plain directory directly inside `rightkit-qa` (never a symlink, never outside it); its name is `<label>-<16 lowercase hex>` or `<label>-<6 letters or digits>` (the two runners' patterns); no pid its records name is alive (the `ownerPid` of the run record its binding points at, and the pid of a home marker that has no binding); and nothing in it, the directory included, was modified within `RIGHTKIT_QA_HOME_MAX_AGE_SECS` (default 2 hours). Not checked: which processes have the home as working directory or an open file (there is no process-table walk), and the run record's lease (a 24 hour timer, not evidence of a live process; it is what kept cancelled runs counted). An app that holds a home and writes nothing for two hours, or a recorded pid that is reused by an unrelated live process, is the residual risk: the first is removed, the second is kept. Homes with an owner marker keep the earlier rule (pid dead and run older than the window).
