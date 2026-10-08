# rightkit-qa

Rust QA harness for Right Suite applications. It owns declarative scenarios (`harness::Harness::scenario`), engine and app control (`control`, through `rightkit-control`), run locking and orphan sweeps, isolated app profiles and temp companions (`storage`), process handling, paid-provider mocks (`mock`), loudness, WAV, and PNG validators, and hashed evidence.

**Use it** in an app's end-to-end and UI tests, through `rightkit cargo test`, when the test must run the real engine or the installed app and record evidence.

**Do not use it** as a general test runner or as a substitute for unit tests. Scenarios that launch a real `.app` need a user login session (see the UI section below).

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `browser` | off | `rightkit-browser`, `tokio` | Chrome (CDP) targets for browser scenarios. Off by default because it pulls `tokio` and `chromiumoxide`. |

Other dependencies include `rightkit-control`, `rightkit-framed-sidecar`, `rightkit-service`, `rightkit-process`, and `png`. On Windows, `windows` is a target dependency.

## Platform support

- macOS: UI launch through `open`, and companions under `/private/tmp/rightkit-qa/`. The `cfg(target_os = "macos")` branches are in `src/audio.rs`, `src/control.rs`, and `src/storage.rs`.
- Windows and Unix: `cfg(windows)` and `cfg(unix)` branches are in `src/process.rs` and `src/suite.rs`. Companions use the system temp directory.

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`.

## Examples

`examples/control-test-app` and `examples/genright` are compiled examples. Run them through `rightkit cargo run -p rightkit-qa --example <name>`.

## Run storage

Run roots, harness evidence, captures & summaries remain under `<managed>/runs/<app>/<run-id>`. App homes, profiles, service/port files, live logs & app-provided output paths use an owned internal system-temp companion recorded as `appRoot` in `.rightkit-run.json`. On macOS, companions always use `/private/tmp/rightkit-qa/<app-prefix>-<random>` with short names so launchd can open app files & control socket paths fit `sockaddr_un`.

Finish copies app logs to managed `evidence/`, app-written evidence to `evidence/app/` & app captures to `captures/` before releasing ownership. Disposal removes temp companions; failed copies or cleanup preserve ownership metadata for recovery. Retention & orphan pruning validate companion bindings & protect live runs.

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
tools/rightkit/scripts/run-ui-tests.sh -p cutright-e2e --test ui -- --nocapture
```

On macOS, `control::launch` stages raw binaries & supplied `.app` bundles under `<appRoot>/apps/<launch-id>/`, then passes every harness-owned home/temp/profile path through `open --env NAME=VALUE`. `HOME`/`USERPROFILE` point to `appRoot`; data/profile keys point to `appRoot/data`; `TMPDIR`/`TMP`/`TEMP` point to `appRoot/tmp`. `RIGHTKIT_SUITE_ROOT` points to `appRoot/suite-<launch-id-prefix>` (including `runtime/<service>/service.json`); live stdout/stderr use `appRoot/app-<launch-id-prefix>.log`. The launcher cwd & harness scratch directories also use `appRoot`.

Caller-supplied `LaunchSpec::env` still rejects credential keys & allows only `RIGHTKIT_*` keys on macOS. Absolute caller paths must live inside `appRoot`; callers cannot replace suite/service identity or move isolation paths outside it. `${data_dir}` & `${evidence_dir}` scenario templates use internal companion paths; artifacts are copied into managed evidence before hashing so disposal cannot invalidate them. App-specific QA knobs should use `RIGHTKIT_<APP>_*` keys honoured only in debug QA builds.

Hidden launches: the app must not take focus itself. When `RIGHTKIT_QA_HIDDEN=1`, skip any `set_focus()`/`show()` on startup, and build with `activate_ignoring_other_apps(false)` on macOS (see `crates/rightkit-control/examples/control-test-app/src/main.rs`). Compare paths with `std::fs::canonicalize`: the QA data dir lives under `/var/folders`, which macOS resolves to `/private/var/folders`.

`Control::wait_for_text(Some("#status"), "Ready", timeout)` waits for visible text (`None` searches the whole body).

## Mocks and validators

- `mock::MockServer::start_with(|req, base| Reply)` serves dynamic, stateful and binary (`Reply::bytes`) responses for paid-provider contracts. `MockServer::start(routes)` keeps the static JSON routes; `MockServer::start_static(vec![(method, path, Reply)])` serves static binary replies.
- Every `200` reply to `GET`/`HEAD` honours a single HTTP `Range` (`bytes=a-b`, `a-`, suffix `-n`): `206` with `Content-Range`, `416` when unsatisfiable, the full `200` when `Range` is absent, malformed, multi-range or its `If-Range` does not match the reply's `ETag`. `Reply::ignore_range()` models a server without resume; `Reply::cut_after(n)` drops the connection after `n` body bytes.
- `loudness`: pure-Rust ITU-R BS.1770-4 integrated loudness, EBU loudness range, sample peak and true peak. It is checked against ffmpeg `ebur128` to within 0.2 LU.
- `media`: WAV decode and stats, PNG non-trivial (pure Rust), and decoded-frame non-uniformity. ffmpeg is only needed to decode compressed audio and video, and its binary path is always passed in.

The browser target reaches a local dev server only through `browser::allow_loopback_origin` / `BrowserTarget::launch_for_origins`. That allowance is scoped to one scheme, host and port.

Publication status is tracked in `INDEX.md`.
