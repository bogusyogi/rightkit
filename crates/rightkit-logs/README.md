# rightkit-logs

GUI-free local logging, diagnostics, and consented telemetry for Right Suite apps.

The default build has no Tauri, async-runtime or tracing dependency. Apps supply log paths, content-free metadata, consent and an endpoint. The crate owns the shared mechanics:

- one JSON object per line, with stable `schema_version`, `ts_ms`, `level` and `event` fields;
- synchronous serialised append, and a bounded non-blocking async writer (`AsyncWriter`);
- size rotation to one `.1` predecessor;
- bounded reads from current and rotated JSONL, without loading whole logs;
- one bundle-wide recent-event cap, followed by the most-recent-error anchor;
- recursive privacy redaction, with non-removable secret and content defaults;
- malformed-row and dropped-event counts, and a warning for a missing file, instead of a failed report;
- a final serialised-byte cap that removes whole oldest events and never slices JSON;
- a consent-gated durable outbox, with key and free-text redaction before anything reaches disk;
- HTTPS upload, with loopback HTTP reserved for local test servers, persisted back-off, and a pluggable `Transport`;
- optional daily-rolling JSONL files for `tracing` (feature `tracing`), collected by the same diagnostics path.

The host decides when to grant consent, enqueue events, flush, revoke consent, or let a user copy and save a returned bundle. Revocation purges queued and quarantined bytes.

A consent file that is absent, unreadable or corrupt reads as `Consent::Unset`, so nothing uploads. `ConsentStore::last_read_error()` (or `try_status()`) tells an unreadable or corrupt file from one the user never wrote. `identity::install_id` creates a new id only when the file is absent or holds no valid id; any other read error is returned and the stored id is left alone.

Catalog capability: `structured-logging`. Published to crates.io (see [INDEX.md](../../INDEX.md)). Licensed under MIT or Apache-2.0, at your option.

## When to use it

- Use it for an app's local JSONL event log and its diagnostics bundle.
- Use it for consented telemetry through the outbox.

Do not use it as a general tracing framework. The `tracing` feature only writes the app's daily JSONL file and its diagnostics; it does not replace `tracing` for other sinks.

## Cargo features

| Feature | Default | Pulls in | Enables |
|---|---|---|---|
| `http` | yes | `ureq` 3.4 (`rustls`) | The HTTPS transport for the consented outbox uploader. Without it the crate has no network dependency, and callers supply their own `Transport`. |
| `testing` | no | none | `testing::MockServer`, a loopback mock ingest server for app end-to-end tests. |
| `tracing` | no | `tracing` 0.1 (`std`), `tracing-subscriber` 0.3 (`std`, `registry`, `fmt`, `json`, `env-filter`) | Daily-rolling JSONL file logs: `install_tracing`, `tracing_layer`, `tracing_layer_checked`, `TracingFileConfig`, `TracingGuard`, `tracing_log_sources` and `collect_tracing_diagnostics`. |

Consumers that bring their own transport use `default-features = false`. `default-features = false` builds the event store, collection and consent code with no network dependency.

## Tracing file logs

With `features = ["tracing"]`, the writer creates `<dir>/<file_prefix>.YYYY-MM-DD.jsonl` for each UTC day:

- `install_tracing(config)` installs a global subscriber and returns a `TracingGuard`. Keep the guard alive. Dropping it writes accepted lines and joins the writer thread, within the shutdown bounds below.
- `tracing_layer::<S>(config)` returns a boxed layer and guard for apps that compose their own subscriber, such as one that also writes stderr. It never fails the caller; setup problems go to stderr.
- Opening a day's file prunes the directory to the newest `max_files` daily files (default 3), today's included, oldest first. Only names of the exact form `<prefix>.YYYY-MM-DD.jsonl` are touched.
- Lines pass through a bounded queue of `TracingFileConfig::queue_lines` lines (default 128_000, the `tracing-appender` limit; `0` behaves as `1`). When the queue is full, the line is dropped and counted; the caller never blocks. Read the running total with `TracingGuard::dropped_events()`. Check it at shutdown or in diagnostics to see whether bursts lost lines. Dropping the guard waits at most `shutdown_send_timeout` (default 100 ms) to queue its stop request and `shutdown_wait` (default 1 s) for the writer to flush; on timeout it detaches the writer and counts the unwritten lines, so a stalled disk cannot hang shutdown.
- `tracing_layer_checked::<S>(config)` is the fallible form of `tracing_layer`. It returns `io::Result` for an invalid prefix or filter, an unusable directory or file open, or a worker that cannot start, instead of writing to stderr.
- With `json: true` and `flatten_event: true`, event fields are top-level keys. `env_filter: None` reads `RUST_LOG`, then falls back to `info`.
- `tracing_log_sources(config, "app")` lists the newest `max_files` daily files as sources named `app-0`, `app-1`, and so on, oldest first. `collect_tracing_diagnostics` runs those sources through the normal bounded, redacted collection policy.

```rust
use rightkit_logs::{install_tracing, TracingFileConfig};

let _guard = install_tracing(TracingFileConfig::new(logs_dir, "scraperight"))?;
tracing::info!(detail = "ready", "app started");
```

Days use UTC, so the file for an event is chosen by its UTC date.

## Usage

Taken from `tests/contract.rs`:

```rust
use rightkit_logs::{EventRecord, JsonlStore, Level};
use serde_json::json;

let store = JsonlStore::new(app_data.join("events.jsonl"), 512 * 1024);
let event = EventRecord::new(now_ms, Level::Info, "app_started")
    .with_field("message", json!("ready"))
    .with_field("cold_start", json!(true));
store.append(&event)?;
```

`append` returns an error. `append_best_effort` never fails the caller and never prints: it counts the failure, and `JsonlStore::status()` returns the counts and the last message. A failed rotation still appends to the current file up to twice the rotation size, then refuses appends (both counted). The test checks that `EventRecord` serialises with the fields flattened.

## Defaults

`CollectionPolicy::default()`:

- 500 events across all sources (`max_events`);
- 512 KiB scanned from each current and predecessor file;
- 512 KiB maximum serialised bundle;
- rotated-file continuity enabled;
- structural redaction of the built-in exact keys `api_key`, `authorization`, `body`, `clipboard`, `completion`, `content`, `cookie`, `last_transcript`, `message`, `password`, `prompt`, `recognized_text`, `refresh_token`, `transcript` and `window_title`, and of any key containing `secret`, `password`, `api_key` or `token` (`src/redact.rs:6-23`).

Apps may add redaction keys and fragments. The built-in privacy keys cannot be removed.

## Registry integration

After the publication gate, product repositories consume an exact crates.io version from `INDEX.md`:

```toml
rightkit-logs = "=<exact published version from INDEX.md>"
```

Do not use a Git dependency, sibling checkout, vendored copy or filesystem link. Until the publication and migration gate is complete, app-local copies are temporary compatibility state, not the target architecture.

## Host adapter

Keep the app layer thin:

1. Construct `JsonlStore` with the app-data `events.jsonl` path and the chosen byte cap.
2. Emit `EventRecord`, or normalise legacy JSON objects before `append_value`.
3. Catch append errors and fall back to a non-panicking stderr or startup-log path.
4. Build an explicit content-free settings snapshot. Include counts for content-bearing collections, never their values.
5. Call `collect_diagnostics` with logical sources such as `app` and `engine`.
6. Serialise `DiagnosticsBundle` for the existing user-initiated support action.

The `@rightkit/logs` package expects app commands named `rightkit_logs_write`, `rightkit_logs_collect` and `rightkit_logs_clear`. Those commands live in the app layer, not in this crate. HeardRight is the reference adapter and contract test. This crate's tests use the filenames `events.jsonl` and `engine-events.jsonl` (`tests/contract.rs:231`). The HeardRight adapter is outside this repository, so this README does not verify it.

## Platform support

Pure Rust with `cfg` gates for macOS and Windows helpers (`system`). The `http` feature uses `rustls`, so no system TLS library is needed.

## Verification

```text
rightkit cargo test -p rightkit-logs --target <native-target>
rightkit cargo fmt --check --manifest-path crates/rightkit-logs/Cargo.toml
```

Tests: `tests/contract.rs` (event shape, collection and redaction) and `tests/outbox_e2e.rs` (consented outbox end to end). With `--features tracing`, `tests/tracing.rs` covers the layer, JSON shape, guard flush, retention and collection, and `tests/tracing_install.rs` covers the global install.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Release notes are in [CHANGELOG.md](CHANGELOG.md).

## Runtime budget

Generated from `runtime-budget.json`; edit that file, then run `node packages/release/runtime-budgets.mjs sync <crate-dir>`.

- Resident memory ceiling: 60.0 MiB.
- CPU ceiling (idle, steady state): 1%.
- Wake-ups ceiling: 1 per second.
- Disk operations ceiling: 1 operations per second.
- Required scale: 200,000 lines or more.
- Required hosts: macOS and Windows.
- Status exposure: `TracingGuard::status()` in `src/tracing_file.rs`; exposes resident memory, idle CPU, wake-ups, disk operations.
- Measured: not yet measured.
