# rightkit-logs

GUI-free local logging and diagnostics for Right Suite Tauri apps.

The crate deliberately has no Tauri, async-runtime, network, or tracing dependency. Apps supply their log paths and content-free metadata; the crate owns the shared mechanics:

- one JSON object per line with stable `schema_version`, `ts_ms`, `level`, and `event` fields;
- synchronous, serialized append so crash-adjacent events are not intentionally buffered;
- size rotation to one `.1` predecessor;
- bounded reads from current and rotated JSONL without loading whole logs;
- one bundle-wide recent-event cap, followed by the most-recent-error anchor;
- recursive privacy redaction with non-removable secret/content defaults;
- malformed-row, missing-file, and truncation statistics instead of report failure;
- a final serialized-byte cap that removes whole oldest events, never slices JSON.

Nothing is uploaded. The host decides when a user copies or saves the returned bundle.

## Registry integration

After the Phase 2 publication gate, product repositories consume the exact
crates.io version:

```toml
rightkit-logs = "=0.1.0"
```

Do not use a Git dependency, sibling checkout, vendored copy, or filesystem
link. Until the publication/migration gate is complete, existing app-local
copies are temporary compatibility state, not the target architecture.

## Host adapter

Keep the app layer thin:

- expose `rightkit_logs_write`, `rightkit_logs_collect`, and
  `rightkit_logs_clear` Tauri commands for `@rightkit/logs`;

1. Construct `JsonlStore` with the app-data `events.jsonl` path and the chosen byte cap.
2. Emit `EventRecord`, or normalize legacy JSON objects before `append_value`.
3. Catch append errors and degrade to a non-panicking stderr/startup-log path.
4. Build an explicit content-free settings snapshot. Include counts for content-bearing collections, never their values.
5. Call `collect_diagnostics` with logical sources such as `app` and `engine`.
6. Serialize `DiagnosticsBundle` for the existing user-initiated support action.

HeardRight is the reference adapter and contract test. Its public filenames remain `events.jsonl`, `events.jsonl.1`, `engine-events.jsonl`, and `engine-events.jsonl.1`.

## Defaults

`CollectionPolicy::default()` uses:

- 500 events across all sources;
- 512 KiB scanned from each current/predecessor file;
- 512 KiB maximum serialized bundle;
- rotated-file continuity enabled;
- structural redaction of message, transcript/content, clipboard/window title, password/authorization, API-key, token, and secret-bearing keys.

Apps may add redaction keys/fragments. The built-in privacy keys cannot be removed.

## Verification

```powershell
cargo test -p rightkit-logs
cargo fmt --check
cargo clippy -p rightkit-logs --all-targets -- -D warnings
```

Licensed under either MIT or Apache-2.0, at your option.
