# rightkit-qa

Rust QA harness for Right Suite applications. It owns declarative scenarios (`harness::Harness::scenario`), engine and app control (`control`, through `rightkit-control`), run locking and orphan sweeps, isolated app profiles and temp companions (`storage`), process handling, paid-provider mocks (`mock`), loudness, WAV, and PNG validators, and hashed evidence.

**Use it** in an app's end-to-end and UI tests, through `rightkit cargo test`, when the test must run the real engine or the installed app and record evidence.

**Do not use it** as a general test runner or as a substitute for unit tests. Scenarios that launch a real app need an interactive user session: a macOS login session (see the macOS UI section below) or a Windows desktop session (see the Windows section).

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `browser` | off | `rightkit-browser`, `tokio` | Chrome (CDP) targets for browser scenarios. Off by default because it pulls `tokio` and `chromiumoxide`. |

Other dependencies include `rightkit-sidecar`, `rightkit-daemon`, `rightkit-child-process`, `getrandom`, and `png`. `rightkit-control` is a macOS-only target dependency (foreground-app reads only); on Windows, `windows-sys` is a target dependency. On Windows the app under test links `rightkit-control`, not the harness (see the Windows section).

## Platform support

- macOS: UI launch through `open`, and companions under `/private/tmp/rightkit-qa/`. The `cfg(target_os = "macos")` branches are in `src/audio.rs`, `src/control.rs`, `src/storage.rs`, `src/util.rs`, `src/workspace.rs`, and `src/bin/rightkit-qa.rs`.
- Windows: real Tauri app UI driving through the app's embedded control server, with `rightkit-control` linked into the app. See [Windows: driving a real Tauri app](#windows-driving-a-real-tauri-app). The `cfg(windows)` branches are in `src/control.rs`, `src/process.rs`, `src/suite.rs`, `src/storage.rs`, `src/workspace.rs`, `src/scenario.rs`.
- Unix: `cfg(unix)` branches are in `src/process.rs`, `src/storage.rs`, and `src/userdata.rs`. Companions use the system temp directory.

Published version: see `INDEX.md` at the repository root. Release notes are in `CHANGELOG.md`.

## Ownership: this crate versus `@rightkit/qa`

Both runners stay. Pick by what the test drives.

| Surface | Owner | Evidence in code |
|---|---|---|
| macOS native app (a real `.app` launched from the login session) | `rightkit-qa` (`control::launch`, via `open` and the in-app `rightkit-control` server) | `cfg(target_os = "macos")` in `src/control.rs`, `src/storage.rs`, `src/audio.rs` |
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

**Permission dialogs.** `StopReport::permission_dialog()` reports a macOS dialog agent (`UserNotificationCenter`, `CoreServicesUIAgent`, ...) seen in the sampled foreground names: positive evidence only. `Control::assert_no_permission_dialog()` is a **no-op today**: `rightkit-control` lists windows per pid, not by owner name, so there is no reliable live detector (TODO in `userdata::live_permission_dialog_check`).

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

Hidden launches: the app must not take focus itself. When `RIGHTKIT_QA_HIDDEN=1`, skip any `set_focus()`/`show()` on startup, and build with `activate_ignoring_other_apps(false)` on macOS (see `crates/rightkit-control/examples/control-test-app/src/main.rs`). Compare paths with `std::fs::canonicalize`: the QA data dir lives under `/var/folders`, which macOS resolves to `/private/var/folders`.

`Control::wait_for_text(Some("#status"), "Ready", timeout)` waits for visible text (`None` searches the whole body).

## Windows: driving a real Tauri app

The Windows leg drives a real Tauri app through the app's own in-app control server. Pulse's hub runs it on its Windows CI runner (`scripts/gate.sh` in the Pulse repository, test `hub/qa-e2e/tests/ui.rs`), so the pattern is in use, not only planned.

### App side: the `qa-native` feature

**The harness drives only builds compiled with `qa-control`.** `rightkit-qa` is an internal development tool, not a user automation surface. The app-side half is `rightkit-control`'s embedded server, which exists only in a binary built with the explicit `rightkit-control/qa-control` feature, which only an app's `qa-native` feature may enable. A normal build has no server, so this harness can neither launch-and-control it nor attach to it, and `right-release` refuses to seal or upload a release or update build that carries one. The only supported automation surface for users is the app's own command line or plugin. See `crates/rightkit-control/README.md`, "QA control never ships".

Pulse's hub uses this pattern (`hub/src-tauri/Cargo.toml`, `hub/src-tauri/src/lib.rs`):

- A feature, `qa-native = ["dep:rightkit-control", "rightkit-control/qa-control"]`, with `rightkit-control` optional and declared with `default-features = false`. It is off by default, and no default feature may reach it. Dev lanes enable it explicitly (`--features qa-native`); release and update lanes never do. A dev build may contain the server, including a release-derived profile such as `release-iterate`, so the registration needs no `debug_assertions` condition.
- `rightkit-control` gets the `qa-control` feature (since 0.3.0; `tauri-plugin` alone no longer exposes the server) and exactly one `wv2-*` feature on Windows. Use `wv2-0_38` for Tauri 2.11 and `wv2-0_39` for Tauri 2.12+. The crate's `compile_error!` in `src/embedded/win_view.rs` rejects a build with neither. The `wv2-*` feature must match the `windows` and `webview2-com` generation the app's Tauri resolves, so WebView2 types cross the Tauri boundary. Pulse selects `wv2-0_39`.
- The plugin is registered only inside a `#[cfg(feature = "qa-native")]` block (the older `#[cfg(all(debug_assertions, feature = "qa-native"))]` form is still accepted). The gate may sit on the statement or on the function or block that contains it:

```rust
if let Some(plugin) = rightkit_control::embedded::Control::<tauri::Wry>::new().build_if_enabled() {
    builder = builder.plugin(plugin);
}
```

**Contract check.** `rightkit-qa guard [--backend control|wdio]` (`src/guard.rs`) reads the app's `Cargo.toml` and Rust sources and fails on: a registration with no `qa-native` gate (`use` lines and comments are ignored); a `rightkit-control` dependency that is not optional, or that enables `qa-control` without being optional; a missing `qa-native` feature, or one that does not enable `dep:rightkit-control` and `rightkit-control/qa-control` (or list `qa-control` in the dependency's features); `qa-native` reachable from `default`, directly or through another feature; any other feature that enables the dependency; a missing runtime flag; and a release or build package script that enables `qa-native`. It also rejects rightkit-shell developer inspection in a shipped build: `dev-inspect`, `rightkit-shell/dev-inspect` or `dev-native` reachable from `default`, a non-optional `rightkit-shell` dependency that lists `dev-inspect`, and a release-named package script that enables `dev-inspect` or `dev-native`. Each message names the file and the change to make. The check cannot prove that a release build lacks the server; `right-release`'s marker scan does that.

- `build_if_enabled()` returns `None` unless the launcher sets `RIGHTKIT_CONTROL_SERVICE`, so a normal launch of a debug build does not listen. When enabled, the server is a `rightkit-daemon` host: a discovery record at `<suite>/runtime/<name>/service.json`, a current-user named pipe on Windows, no TCP listener and no bearer token. It fails closed when no absolute suite root exists.
- Input goes to the webview from inside the process (on Windows through WebView2's `Input.dispatch*`), not through OS input events.
- Pulse pins `rightkit-control = "=0.1.11"`, while this repository's crate is 0.3.0. Check the feature names against the version the app pins.

### Harness side

- Launch with `control::launch(&LaunchSpec { binary, mode: Mode::Hidden, env, startup_timeout, label }, &ws, sc.tracker())`, then `Control::wait_eval(js, timeout)` until the app's shell is up (Pulse waits for `nav.rk-nav`). `Control::eval` and `wait_for_text` read the page.
- Gate each scenario with `requires = ["platform:windows"]` or `platform:darwin`. A scenario whose requirement is not met prints its `[qa] SKIP` line.
- Hidden launch: `Mode::Hidden` sets `RIGHTKIT_QA_HIDDEN=1` and `RIGHTKIT_QA_BACKGROUND=1` in the child environment (`launch_env` in `src/control.rs`). Pulse runs the hidden launch on a Windows runner (its hub journey drives a hidden WebView2 through the embedded control server). On Windows the harness samples the foreground window's owning pid every 20 ms (`GetForegroundWindow` + `GetWindowThreadProcessId`) and `StopReport::owned_never_frontmost` is true only when no sample found the started pid in front and no foreground owner was unreadable; a moment with no foreground window counts as nothing owned in front. It checks the started pid only, not helper processes, and does not stop the app taking focus (that is the app's job, below). Pulse's hub skips `set_focus()` under the flag but still calls `show()` on its main window, so do not read "hidden" as "no window".
- Isolated data dir: `workspace::create(&sc.scratch("ws"), None, "<app-key>")` puts HOME, USERPROFILE, LOCALAPPDATA and WEBVIEW2_USER_DATA_FOLDER inside the workspace. On Windows those paths come back in `\\?\` verbatim form, but WebView2 and the Win32 settings code take plain drive paths only. Pulse rewrites `ws.home`, `ws.data_dir` and each `ws.env` value with the prefix removed (`plain_workspace` in `tests/ui.rs`). Expect the same in any app that hands these paths to WebView2 or Win32.
- App-specific knobs go through `LaunchSpec::env` under `RIGHTKIT_<APP>_*` keys. Pulse passes `RIGHTKIT_PULSE_QA_HOME`, which the debug QA build reads to set its HOME and USERPROFILE.
- `hub/qa-e2e` is its own cargo workspace (`[workspace]` in its `Cargo.toml`). rightkit-qa declares `windows-sys` 0.61.2 as a caret requirement (`Cargo.toml:47`, not an exact pin), and Pulse's comment says its Windows crate pins do not resolve next to the hub's `pulse-core`. Pulse pins `rightkit-qa` exactly in that manifest; it is held at `=0.2.12` until a Windows run on 0.2.16 passes.

### Running

- CI (Pulse `scripts/gate.sh`, `run_hub_qa`): build the debug app with `cargo build --features qa-native,custom-protocol` in `hub/src-tauri`, then run `cargo test --test ui -- --nocapture` in `hub/qa-e2e` with `RIGHTKIT_MANAGED_ROOT`, `PULSE_QA_SHOTS` and `RIGHTKIT_QA_EVIDENCE` set. The gate calls plain `cargo` because its runner has no `rightkit` CLI. That is the gate's stated exception, not the local rule.
- The gate does not accept a silent pass. A skipped scenario exits `ok` (see Rust integration tests), so the gate also requires a `"passed"` receipt in `evidence.json` and at least eight screenshots.
- A hidden WebView2 app cannot be launched from a non-interactive session (SSH, a service, session 0 or a non-visible window station). The app process starts and the control server may answer, but the webview never produces a ready window, so `control::launch` ends with `app window never became ready` after `startup_timeout`. On Windows that error now says when the harness itself is in such a session. Start the run from the logged-in user's desktop session instead, for example a one-shot scheduled task created with `schtasks /create /it` (interactive token), run once, and deleted afterwards. Do not read the timeout as an app defect before checking the session.
- Local host: start the run from the user's interactive desktop session, not over SSH. An SSH-started run is the open gap below.

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
- Required hosts: not declared in `runtime-budget.json`.
- Status exposure: `Sidecar::status()` in `src/engine.rs`; exposes resident memory, idle CPU, wake-ups, disk operations.
- Measured: not yet measured.

## QA root limits

Homes live under `<tmp>/rightkit-qa/` (`/private/tmp/rightkit-qa` on macOS). Two limits are checked when a run starts, after the dead-home sweep.

- **Per-product cap** (`RIGHTKIT_QA_ROOT_CAP_BYTES`, default 4 GB, `0` disables). A run for app X is refused only when X's own homes hold more than the cap. A home belongs to the product named by the `app` in its `.rightkit-app.json` binding, else by the label in its directory name (`<app cut to 16 characters>-<suffix>`). Other products' homes never count, so another app's leftovers cannot block a run.
- **Root ceiling** (`RIGHTKIT_QA_ROOT_CEILING_BYTES`, default 32 GB, `0` disables). Every run is refused when all homes together exceed it, because the disk is at risk. The refusal names the largest homes with their product. The cap is checked first.

A refusal says which product's homes it counted, the total, the largest homes, and that `rightkit-qa clean` removes the homes of dead runs. It is a `storage::RootOverCap` (`kind`, `app`, `total_bytes`, `home_count`, `limit_bytes`, `largest`); `?` turns it into `rightkit_qa::Error`.

**Setting limits in code.** A test run under `rightkit cargo` does not receive the environment variables, so set them on the harness: `Harness::new(..)?.with_root_cap_bytes(8_000_000_000).with_root_ceiling_bytes(0)` (`0` disables a limit), or `workspace::create_with_limits(.., storage::RootLimits::default().with_cap_bytes(..))`. A value set in code wins over the environment variable; a limit left unset falls back to the variable, then the default. The harness sweeps when it is built and checks its limits once, at the first scenario that runs (a skipped scenario does not check), so the setters take effect when chained after `new`. A refused scenario writes a failed evidence record, removes its own home and panics with the refusal.

**The sweep removes more.** `storage::clean_homes` (run at every start, and by `rightkit-qa clean`) removes a home that has no `.rightkit-home.json` owner marker (written before it existed, or missing a file) when all of these hold: it is a plain directory directly inside `rightkit-qa` (never a symlink, never outside it); its name is `<label>-<16 lowercase hex>` or `<label>-<6 letters or digits>` (the two runners' patterns); no pid its records name is alive (the `ownerPid` of the run record its binding points at, and the pid of a home marker that has no binding); and nothing in it, the directory included, was modified within `RIGHTKIT_QA_HOME_MAX_AGE_SECS` (default 2 hours). Not checked: which processes have the home as working directory or an open file (there is no process-table walk), and the run record's lease (a 24 hour timer, not evidence of a live process; it is what kept cancelled runs counted). An app that holds a home and writes nothing for two hours, or a recorded pid that is reused by an unrelated live process, is the residual risk: the first is removed, the second is kept. Homes with an owner marker keep the earlier rule (pid dead and run older than the window).
