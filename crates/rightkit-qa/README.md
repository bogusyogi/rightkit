# rightkit-qa

Rust QA harness for Right Suite applications. It provides declarative scenarios, engine and app control, run locking, isolated profiles, process handling, and hashed evidence.

The `browser` feature enables Chrome targets through `rightkit-browser`.

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

A non-empty process environment variable overrides the file for a single key. This only applies to direct, unmanaged runs. The harness reads `RIGHTKIT_QA_TIER` to override the tier selection (`Harness::with_tier_key` renames it) and `RIGHTKIT_QA_EVIDENCE` to set the evidence root.

## UI scenarios against a real app (macOS)

Scenarios that launch a real `.app` (`control::launch`) must run from the user's login session. Under `rightkit cargo test`, the test binary runs in the build broker's context, where `open` has no GUI session and exits 1. Build through the broker and run the binary yourself; `scripts/run-ui-tests.sh` does both and keeps your shell's environment:

```sh
tools/rightkit/scripts/run-ui-tests.sh -p cutright-e2e --test ui -- --nocapture
```

On macOS, `control::launch` passes only `RIGHTKIT_*` keys to the app, because `open --env` exposes values in argv. It drops the workspace's `XDG_*`, `WEBVIEW2_*` and `<APP>_DATA_DIR` keys, so the app must derive its isolated data dir from `RIGHTKIT_QA_DATA_DIR`. Caller-supplied `LaunchSpec::env` is still checked strictly. For an app-specific QA knob, use a `RIGHTKIT_<APP>_*` key (for example `RIGHTKIT_COCKPIT_QA_HOME`) and honour it only in debug QA builds.

Hidden launches: the app must not take focus itself. When `RIGHTKIT_QA_HIDDEN=1`, skip any `set_focus()`/`show()` on startup, and build with `activate_ignoring_other_apps(false)` on macOS (see `rightkit-control/examples/control-test-app/src/main.rs`). Compare paths with `std::fs::canonicalize`: the QA data dir lives under `/var/folders`, which macOS resolves to `/private/var/folders`.

`Control::wait_for_text(Some("#status"), "Ready", timeout)` waits for visible text (`None` searches the whole body).

## Mocks and validators

- `mock::MockServer::start_with(|req, base| Reply)` serves dynamic, stateful and binary (`Reply::bytes`) responses for paid-provider contracts. `MockServer::start(routes)` keeps the static JSON routes; `MockServer::start_static(vec![(method, path, Reply)])` serves static binary replies.
- Every `200` reply to `GET`/`HEAD` honours a single HTTP `Range` (`bytes=a-b`, `a-`, suffix `-n`): `206` with `Content-Range`, `416` when unsatisfiable, the full `200` when `Range` is absent, malformed, multi-range or its `If-Range` does not match the reply's `ETag`. `Reply::ignore_range()` models a server without resume; `Reply::cut_after(n)` drops the connection after `n` body bytes.
- `loudness`: pure-Rust ITU-R BS.1770-4 integrated loudness, EBU loudness range, sample peak and true peak. It is checked against ffmpeg `ebur128` to within 0.2 LU.
- `media`: WAV decode and stats, PNG non-trivial (pure Rust), and decoded-frame non-uniformity. ffmpeg is only needed to decode compressed audio and video, and its binary path is always passed in.

The browser target reaches a local dev server only through `browser::allow_loopback_origin` / `BrowserTarget::launch_for_origins`. That allowance is scoped to one scheme, host and port.

Publish through the RightKit release workflow.
