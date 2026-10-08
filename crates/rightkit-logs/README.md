# rightkit-logs

GUI-free local logging, diagnostics, and consented telemetry for Right Suite apps.

The crate has no Tauri, async-runtime or tracing dependency. Apps supply log paths, content-free metadata, consent and an endpoint. The crate owns the shared mechanics:

- one JSON object per line, with stable `schema_version`, `ts_ms`, `level` and `event` fields;
- synchronous serialised append, and a bounded non-blocking async writer (`AsyncWriter`);
- size rotation to one `.1` predecessor;
- bounded reads from current and rotated JSONL, without loading whole logs;
- one bundle-wide recent-event cap, followed by the most-recent-error anchor;
- recursive privacy redaction, with non-removable secret and content defaults;
- malformed-row, missing-file and truncation statistics, instead of a failed report;
- a final serialised-byte cap that removes whole oldest events and never slices JSON;
- a consent-gated durable outbox, with key and free-text redaction before anything reaches disk;
- HTTPS upload, with loopback HTTP reserved for local test servers, persisted back-off, and a pluggable `Transport`.

The host decides when to grant consent, enqueue events, flush, revoke consent, or let a user copy and save a returned bundle. Revocation purges queued and quarantined bytes.

Catalog capability: `structured-logging`. Published to crates.io (see [INDEX.md](../../INDEX.md)). Licensed under MIT or Apache-2.0, at your option.

## When to use it

- Use it for an app's local JSONL event log and its diagnostics bundle.
- Use it for consented telemetry through the outbox.

Do not use it as a general tracing framework. The crate has no `tracing` integration.

## Cargo features

| Feature | Default | Pulls in | Enables |
|---|---|---|---|
| `http` | yes | `ureq` 3.4 (`rustls`) | The HTTPS transport for the consented outbox uploader. Without it the crate has no network dependency, and callers supply their own `Transport`. |
| `testing` | no | none | `testing::MockServer`, a loopback mock ingest server for app end-to-end tests. |

Consumers that bring their own transport use `default-features = false`.

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

`append` returns an error. `append_best_effort` never fails the caller. The test checks that `EventRecord` serialises with the fields flattened.

## Defaults

`CollectionPolicy::default()`:

- 500 events across all sources (`max_events`);
- 512 KiB scanned from each current and predecessor file;
- 512 KiB maximum serialised bundle;
- rotated-file continuity enabled;
- structural redaction of message, transcript and content, clipboard and window titles, password and authorisation, API keys, tokens and secret-bearing keys.

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

The `@rightkit/logs` package expects app commands named `rightkit_logs_write`, `rightkit_logs_collect` and `rightkit_logs_clear`. Those commands live in the app layer, not in this crate. HeardRight is the reference adapter and contract test. Its public filenames remain `events.jsonl`, `events.jsonl.1`, `engine-events.jsonl` and `engine-events.jsonl.1`.

## Platform support

Pure Rust with `cfg` gates for macOS and Windows helpers (`system`). The `http` feature uses `rustls`, so no system TLS library is needed.

## Verification

```text
rightkit cargo test -p rightkit-logs --target <native-target>
rightkit cargo fmt --check --manifest-path crates/rightkit-logs/Cargo.toml
```

Tests: `tests/contract.rs` (event shape, collection and redaction) and `tests/outbox_e2e.rs` (consented outbox end to end).

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). No CHANGELOG.md in this crate.
