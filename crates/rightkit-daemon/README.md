# rightkit-daemon

Daemon host capability implementing the `daemon-api` v1 contract (`contracts/daemon-api.v1.schema.json` at the repository root, not inside this crate). `publish = false`.

## Provider slug rule

The `{provider}` segment of `/v1/oauth/{provider}/callback` and `/v1/webhooks/{provider}` must match `^[a-z0-9][a-z0-9-]{0,63}$` (lowercase letters, digits and hyphens; 1 to 64 characters; no leading hyphen). The route table enforces it at dispatch (`routes::is_provider_slug`): anything else is not a route and answers `404 not_found` before authentication or the OAuth flow. Use the same slug as the key in `DaemonConfig::webhook_secrets`; a key outside the rule can never be reached.

## Blocking entry point

`serve_blocking(daemon, std::net::TcpListener, tls, ready: mpsc::Sender<SocketAddr>, stop: StopHandle) -> io::Result<()>` runs the server on a runtime the crate owns (2 workers, blocking pool capped at 32), for consumers that do not depend on tokio. Bind a std listener, spawn a thread that calls it, wait on `ready` for the bound address, and call `StopHandle::stop()` (from any thread) to drain and return. Behaviour matches `serve`: `LISTENING`/`SHUTDOWN` events, 10 s connection drain; the runtime then shuts down within 5 s. Calling it from inside a tokio runtime, including a `spawn_blocking` thread, returns an error.

## Device client and test daemon

Feature `client` (off by default, no new dependencies) adds `rightkit_daemon::client`, a blocking client for the routes the daemon serves, so a consumer's end-to-end runner need not hand-roll signing, hashing or TLS:

- `ClientConfig::new(addr).root_pem(pem)?` then `Client::new(cfg)?`. TLS trusts only the roots you pass (the server name defaults to `localhost`; change it with `server_name`). With no root, `Client::new` returns `Error::NoRoots`: there is no system-root fallback.
- Keys are yours to store: `DeviceKey::generate_pkcs8()` returns bytes to persist, `DeviceKey::from_pkcs8` / `from_seed` rebuild the key.
- `client.enrol(code, key, label)?` returns a `DeviceSession` (`device_id()`, `scopes()`); `client.session(id, key)` rebuilds one from stored values.
- Signed calls: `send(method, path_and_query, headers, body)`, `update_job(id, patch, if_match, idempotency_key)`, and uploads: `upload_init` returns an `UploadSession` (serde-serializable; the `digest_key` is shown only at init, so persist it), `upload_status`, `upload_chunk`, `upload_complete`, and `upload_reader` / `upload_bytes` / `upload`. `upload_reader` asks `upload.status` which chunks the daemon holds, sends only the rest with `X-Chunk-Digest`, hashes the whole source with the session key and commits; call it again after any failure to resume.
- Errors: a daemon refusal is `Error::Refused(Refusal { status, code, body })` with `code: ErrorCode` for every code the server returns (`Unauthenticated`, `Forbidden`, `RevisionConflict`, `HashMismatch`, ...; unknown codes are `ErrorCode::Other`). TLS failure is `Error::Tls`, transport failure `Error::Io`.

The signature and digests come from `signing::sign_request`, `signing::verify_request`, `signing::chunk_digest` and `signing::object_hasher`, the same functions the daemon uses to verify.

Feature `testing` (implies `client`; pulls in `rcgen` and `tempfile`) adds `rightkit_daemon::testing`: `TestCertificate::generate()` (a fresh self-signed certificate per call, never a committed key) and `TestDaemon::start()` / `start_with(|cfg| ..)`, which run a daemon with `serve_blocking` on `127.0.0.1:0` and expose `addr`, `root_pem`, `enrolment_code`, `daemon`, `client()` and `stop_handle()`. Dropping the `TestDaemon` stops and joins the server. Enable both features in a consumer's dev-dependencies only.

`testing` also adds two negative-case requests to `DeviceSession`, absent without it: `send_unsigned(method, path_and_query, headers, body)` (the device id, no timestamp, nonce or signature) and `send_with_nonce(nonce, method, path_and_query, headers, body)` (signed correctly over a nonce you choose). The daemon refuses the first with `401 unauthenticated` and the second, when the nonce was already used by that device, with `401 unauthenticated` too; both come back as `Error::Refused` with `ErrorCode::Unauthenticated`, and the `AUTH_REJECTED` event's `reason` (`malformed_timestamp`, `replayed_nonce`) tells them apart.

## Drift guards

`tests/conformance.rs` pins the route table, fixtures, error codes and lifecycle events to the schema. The schema example, the valid fixture and `routes.rs` must list the same routes and statuses. Error codes the daemon cannot return are listed under `x-reservedErrors`, and lifecycle events it cannot emit under `x-reservedEvents`.

## State files: known limits

- **Upload records at start.** `Daemon::open` loads every `uploads/<id>.json`. A record that does not parse is renamed to `<id>.json.bad`; a record that cannot be read is left in place and read again at the next start. Either way the daemon opens, the session is unknown to it for this run, and the event `UPLOAD_RECORD_SKIPPED` (`file`, `reason`, `detail`) is logged. `state.json` is different: missing means defaults, any other error fails `open`.
- **The crate's own atomic write (`util::atomic_write`), not `rightkit-fs`.** It writes to a fixed temp name, `<name>.tmp` beside the target, so two writers of the same file at once would share one temp file; the daemon writes its state under its own mutex and is the only writer of its state directory. A temp file left by a crash is overwritten by the next write of that file.
- **The directory flush after the rename is best effort.** Its failure is ignored, so after a power loss a write that returned `Ok` can show the previous version of the file. On Windows there is no directory flush at all.

## Runtime budget

Generated from `runtime-budget.json`; edit that file, then run `node packages/release/runtime-budgets.mjs sync <crate-dir>`.

- Resident memory ceiling: 95.0 MiB.
- CPU ceiling (idle, steady state): 1%.
- Wake-ups ceiling: 1 per second.
- Disk operations ceiling: 1 operations per second.
- Required scale: 10,000 submissions or more.
- Required hosts: not declared in `runtime-budget.json`.
- Status exposure: `Daemon::status()` in `src/daemon.rs`; exposes resident memory, idle CPU, wake-ups, disk operations.
- Measured: not yet measured.
