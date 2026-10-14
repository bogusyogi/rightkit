# Changelog


## 0.2.1

- Requires `rightkit-oauth` 0.5.0 (was 0.4.0), so a fresh resolution no longer reaches a release that names renamed-away crates.
## 0.2.0

Renamed from rightkit-service; 0.1.x remains published under the old name.

Breaking minor, published once. It folds the unreleased 0.1.5 entries (the source was 0.1.5 while crates.io has 0.1.4) and the unreleased changes below into one release; nothing was published as 0.1.5.

### Breaking

- `CallContext::peer()` now returns `Option<&PeerIdentity>` (`None` for a caller that is not a local app). A local caller always has `Some`; migrate with `cx.peer().cloned()` or `if let Some(p) = cx.peer()`.
- Release builds read no override switch (see the folded entry below) and `HostOptions::write_timeout` defaults to 120 s (see 0.1.5 below). Both are behaviour changes for existing hosts.

### Fixed
- `mode`: a test root given as a Windows verbatim path (`\\?\…\..\…`) is refused as not absolute; `..` is matched as text as well as as a component, since verbatim paths keep it literal.

### Added (M7 lane D: outbound link and durable tasks)

- **`rk-link/1`, the outbound link** (feature `link` for the device client, `remote` for the host). A machine with no inbound port opens TLS to the host and upgrades `GET /v1/link` (`Upgrade: rk-link/1`), signed with the device key like every other device request (signature, nonce, replay window; scope `link`). The connection then carries newline-delimited JSON frames in both directions: `hello{device, resume_seq}`, `welcome`, `task{id, seq, epoch, method, params, lease_ttl}`, `ack`, `result{id, epoch, ok|error}`, `settled`, `request`/`response` (a machine calling upward through the same device contract as `POST /v1/call/{method}`), `ping`, `close`. Each frame is `<tag> <json>`: the tag is the device's Ed25519 signature (machine to host) or an HMAC under a per-connection session key issued in the upgrade response (host to machine), bound to direction, connection id and exact payload bytes. Frames carry a sequence number that must be exactly the previous plus one, so a replayed, duplicated, reordered or dropped frame ends the connection. TLS only, pinned roots only, no plaintext fallback. 20 s heartbeat each way, dead after 60 s. One blocked reader per link, no polling thread.
- **Durable task leases with epoch fencing** (`Daemon::enqueue_task`, `task`, `tasks`, `tasks_sweep`, `TaskSpec`, `TaskInfo`, `TaskStatus`; state in `state.json`). `queued -> leased(device, epoch, expires) -> done | failed`. One owner per task at a time. The epoch grows by one on every lease grant. A result is accepted only from the device's current connection and the current epoch: a stale epoch is `lease_lost`, a superseded connection is `superseded`, the same `(task, epoch)` result twice is `duplicate`. A disconnect releases nothing: the device reconnects and gets every leased task it has not acked again, with the same epoch, whatever its `resume_seq` says (a re-lease reuses `seq`; the device journal dedups by `(id, epoch)`). A lease lapses only when its owner sends no frame for `lease_ttl`; the sweep that observes the lapse re-queues the task in the same step, so it re-queues exactly once, and the next grant (epoch + 1) fences the old holder. Two connections of one device: the newer supersedes, the older is closed with `superseded`. Unpinned tasks go to the linked device holding the fewest leases. Events: `LINK_UP`, `LINK_DOWN{reason}`, `LINK_FRAME_REJECTED{reason}`, `TASK_QUEUED`, `TASK_LEASED`, `TASK_DONE`, `TASK_FAILED`, `TASK_LEASE_LOST{reason}`; `DaemonMetrics` gains `links`, `tasks_queued`, `tasks_leased`.
- **Blocking client** `rightkit_daemon::link::client` (`LinkClient::start`, `LinkConfig`, `LinkHandle`, `TaskHandler`, `LinkTask`): std sockets and rustls only, so a `link`-only build has no tokio, axum or hyper (`tests/link_features.rs` pins this against the manifest and the sources). Reconnects with capped exponential backoff (1 s to 60 s, jitter) and a fresh signature; writes each task to a journal (fsync) before it is acknowledged; keeps each result until the host settles it and resends it after a reconnect; runs journalled unfinished tasks again after a restart (at-least-once; handlers key on `id` and `epoch`). `LinkHandle::call` is a machine-to-host request over the link.
- The HTTP listener now serves with `with_upgrades` and drains connections with its own stop signal and the connection permits instead of hyper-util's `GracefulShutdown` (which does not cover upgradable connections); links are closed first at shutdown. `GET /v1/link` is handled before the daemon-api route table, which, with `ROUTES` and the 1.1.0 fixtures, is unchanged.
- Tests: `tests/link.rs` (host logic in process with a manual clock, and the real client over TLS: kill the connection mid-task and the same device resumes, a result finished while down is delivered after reconnect, stale epoch refused, supersede, lease expiry re-queues once under concurrent sweeps, replay and forged frames refused, plaintext refused), `tests/link_features.rs` (runs in both `test-mode` states, no gate).

### Added

- **Dual transport glue (M7 lane C).** `Service::builder().methods(table).handler(h).local(&suite, cfg, opts).remote(daemon_cfg).start()` hosts one `Handler` and one `MethodTable` on the local transport and, under `remote`, exposes each `transport: both` method as `POST /v1/call/{method}` for signed devices (`remote::DeviceCalls`, `Daemon::with_calls`, route id `device.call`, class `device_call`). The device becomes `Caller::Device{id, scopes}`; the body is the params, the answer is the result or `{"error":{"code","message","retryable"}}` with the status table's status. A method that is not `both`, or unknown, is `forbidden`. The existing v1 routes are untouched. daemon-api schema 1.1.0 (additive, see `rightkit-contracts`) describes the class.
- `MethodTable::try_method` and `TableError`: a method id that is empty or holds `*`, `?`, `[`, `]`, `{`, `}`, whitespace, a control character, `/`, `\\`, `%` or `#` is refused, and so is a device-reachable policy with no scope (`and_remote([])`, `remote([])`). `MethodTable::method` panics on either at construction. An empty scope set admits no device (it used to read as any device). `MethodTable::authorize_device_call`.

- `rightkit_daemon::contract`, the transport-free request contract: `Caller` (`LocalApp{app, peer}` | `Device{id, scopes}`), `MethodTable` and `MethodPolicy` (one policy per method id: local apps, device scopes, `Transport::{Local, Remote, Both}`), `MethodTable::allowlist()` (the local allowlist derived from the table) and `MethodTable::authorize(&Caller, method)`, and `status_for_code`/`status_for_error` (error code -> HTTP-style status table). `ServiceConfig::with_methods(&MethodTable)` takes the allowlist from a table. `CallContext::origin() -> &Caller`; `CallContext::app()` returns the device id for a device caller.
- Error codes `ERROR_UNAUTHENTICATED` (`unauthenticated`), `ERROR_FORBIDDEN` (`forbidden`), `ERROR_REVISION_CONFLICT` (`revision_conflict`).
- `Handler`, `CallContext` and `Emit` now live in `contract` and are still exported at the crate root (the old paths keep compiling). The local server adapts to the contract with no wire or behaviour change: the same `request_not_allowed` answer, the same hello `methods`, `Allowlist` unchanged.
- Tests: `tests/contract.rs` (method table drives the allowlist end to end, table/allowlist agreement, device scopes, status table), run in both feature states.

### Unreleased entries folded in

- Release builds read no override switch at all (D4 ratified, Opus residual; **behaviour change** for builds without `test-mode`): `Suite::resolve()` returns `Suite::per_user_default()` (account database / known-folder table) and `Suite::resolve_ignoring_env_overrides(_)` is a no-op alias of it; `RIGHTKIT_SUITE_ROOT`, `RIGHT_SUITE_DIR`, `ORTHIC_SHARED_DIR`, `HOME`, `XDG_DATA_HOME` and `LOCALAPPDATA` are read only under `#[cfg(feature = "test-mode")]`, so the release binary does not contain the code or the names. macOS app discovery takes the user's `~/Applications` from `getpwuid_r`, not `HOME`. `USERNAME` in the Windows pipe name stays: it is a label, not a selector; the pipe DACL and same-user check decide access. Tests run in both feature states (`cargo test -p rightkit-daemon` and `--features test-mode`). Not compiled or run locally; verification is the CI run.

### 0.1.5 entries folded in

- Fix: `Client::call` returned `request_timeout` after the full deadline when the host's reply was larger than the client's 16 MiB frame limit (the host's `frame_limit` may be larger): the oversized frame was drained and then ignored while the client kept waiting for it. It now returns the non-retryable `response_too_large` at once, and the connection stays usable. Not compiled or run locally; verification is the CI run. `tests/write_bound.rs` `a_reply_over_the_clients_frame_limit_is_a_typed_error_not_a_deadline`; the keep-up test now moves 8 MiB (it asked for 32 MiB through a 16 MiB client, which is what timed out in CI).
- Fix: the host bounds each frame write to a client (M3/M4 design: service call bounds). A client that stopped reading used to fill the transport buffer and block the host's response (or event) write forever, holding that connection's thread; the host now closes the connection when one frame's write+flush exceeds `HostOptions::write_timeout` (default `DEFAULT_WRITE_TIMEOUT`, 120 s, the same as the client's default deadline). The client sees `connection_closed`; the handler's `connection_closed` hook runs. The bound is per frame, so a slow reader that keeps making progress is not cut off. Additive: `HostOptions::write_timeout(Option<Duration>)` (`None` = unbounded, zero raised to 1 ms), `HostOptions::get_write_timeout`, `DEFAULT_WRITE_TIMEOUT`. **Behaviour change:** hosts that never set the option now have the 120 s bound (before: none); set `None` to keep the old behaviour. No new error code and no new `ServiceEvent` variant (the stalled peer cannot receive a reply, and a new variant would break exhaustive matches). Not compiled or run locally; verification is the CI run. Tests: `tests/write_bound.rs` (stalled reader cut off; prompt reader unaffected; `None` stays open; defaults), Unix only.
- Fix: the per-request deadline of `Client::call_events` (and so `call`) now starts before the wait for the connection, not after it. A call queued behind a request still in flight used to wait for the holder with no bound and only then arm its deadline; it now times out at its own deadline (`request_timeout`) without aborting the connection the holder is using. Additive: `Client::call_events_until(method, params, deadline: Option<Instant>, on_event)` takes an absolute monotonic deadline fixed by the caller. Not compiled or run locally; verification is the CI run. Test: `tests/e2e.rs` `queued_call_deadline_includes_the_mutex_wait`. The server-side write that can block on a stalled peer is unchanged.

- Add `rightkit_daemon::mode` (D4, ratified: a release build reads no override switch at all; additive): the one runtime decision between a release run and a test run. `decide(&AppId, &dyn ModeInput)` returns `Mode::Release` and reads nothing without the new off-by-default `test-mode` cargo feature; with it, a test run needs an activation selector and a root that is absolute, a real directory (not a symlink), disjoint from the real per-user suite root and the user's content folders, private to the user (Unix) and bound to the app by the runner's `.rightkit-app.json`, else a typed `Refusal`. `roots`/`roots_with` give the suite and app directories (account database in release, inside `TestRoot` in a test run); `test_selector!` is the only sanctioned selector reader and drops its name in builds without `test-mode`; `init_or_exit` is the process-wide wrapper (exit status 2). `Suite::resolve_ignoring_env_overrides` is deprecated in favour of it and otherwise unchanged. Tests: `tests/mode.rs` (N1 default build; N2-N5 with `--features test-mode`). Not compiled or run locally; verification is the CI run, which must also run `cargo test -p rightkit-daemon --features test-mode --test mode`.
- Fix: a `service.json` or `takeover.json` that cannot be read (Windows `ERROR_ACCESS_DENIED` or `ERROR_SHARING_VIOLATION` while another process replaces it, a permission error, an I/O error) is no longer read as "absent". Only `NotFound` means absent. Not compiled or run locally; verification is the CI run. No version bump, nothing retried or slept here, and the writer-side retry in `write_atomic` is unchanged.
  - Additive: `try_read_record`, `try_read_takeover`, `try_read_live_record` (`io::Result<Option<_>>`: `Ok(None)` absent or unparseable, `Err` unreadable), `try_adopt_or_spawn` and `try_plan_host` (`io::Result` of the existing enums). `read_record`, `read_takeover`, `read_live_record`, `adopt_or_spawn` and `plan_host` keep their signatures and still answer as if absent for an unreadable file (their return types cannot say "unknown"); prefer the `try_` forms.
  - `Client::connect*`: an unreadable record is still the retryable `service_unavailable`, with the message `could not read service.json for <service> (try again): <error>` instead of `no service.json for <service>`.
  - Host election (`host`, `host_with_options`): an unreadable `service.json` no longer counts as no incumbent, so the elected host no longer removes it and binds beside a live host. It waits as for a host between lock and publication; if the record is still unreadable at `takeover_timeout`, `host` returns `HostError::Io` with the read error instead of `HostError::TakeoverTimeout`.
  - Takeover monitor: a `takeover.json` that cannot be read is not marked as judged, so the next once-a-second pass reads it again; before, the host never looked at that file state again and could miss the request.
  - `wait_for_claim_winner` keeps waiting through an unreadable record.
  - Unchanged on purpose: releasing a host removes its own `service.json` only after reading it, and `clear_takeover_if_owner` removes `takeover.json` only after reading it; if the read fails the file is left in place.
  - Tests: `tests/unreadable_record.rs`, `record::tests`, and two in `server::monitor_tests` (a directory at the path stands in for a read error that is not `NotFound`).
- Windows: `Client::connect_expecting` verifies the host instead of refusing with `Unverified`. After the same-user check it opens the pipe's server process and keeps the handle (so the pid cannot be reused), reads the creation time, the image path (`QueryFullProcessImageNameW`) and the creation time again, checks the process still runs and the pipe still reports the same pid, then compares the canonical path without regard to case, or the file identity (volume serial and file index), with the expected executables. `HostProcess::sid` carries the host's user SID. A signature check goes in `with_verifier`.
- Windows: `Suite::per_user_default()` (and `ignore_env_overrides`) returns `<local app data>\Orthic Labs\shared` from `SHGetKnownFolderPath(FOLDERID_LocalAppData)`, never from `%LOCALAPPDATA%`.

- Additive: verify the HOST from the client. `Client::connect_expecting(&ClientConfig, &HostExpectation)
  -> Result<Client, ConnectError>` asks the kernel who is on the listening end of the connected socket
  and refuses any program that is not an expected executable, before the `hello` or any request is
  written (the only thing the other side sees of a refusal is a connection that closes without a
  byte). The host must run as the client's uid and its executable (macOS `proc_pidpath`, Linux
  `/proc/<pid>/exe`) must equal, after canonicalisation, or be the same file (device and inode) as,
  one of the paths named. The pid is proven stable across the read (start time, plus the audit
  token on macOS, before and after; the pid re-read from the socket). Windows and other Unix
  refuse with `HostIdentityReason::Unverified` (a documented fail-closed stub) rather than connect
  unverified. `Client::connect` and `connect_with` are unchanged and documented as
  unauthenticated-host.
- New types: `HostExpectation` (`executable`, `any_of`, `or_executable`, `sibling_executable`,
  `bundle_executable`, `relative_to_current_exe`, `with_verifier`), `HostProcess`,
  `HostIdentityError { expected, found_pid, found_executable, reason }`, `HostIdentityReason`,
  `ConnectError { Service, HostIdentity }`, `ClientConfig`. New `Client::verified_host`,
  `Client::connect_config`, `Client::call_retrying_expecting`, `open_stream_expecting`, and the
  error codes `ERROR_HOST_IDENTITY` ("host_identity", not retryable) and `ERROR_SUITE_UNRESOLVED`.
  The macOS code-signature team identifier check is a documented hook (`with_verifier` receives the
  host's audit token); this crate links no Security.framework.
- Additive: environment hardening. `ClientConfig::ignore_env_overrides(true)` takes the suite root
  only from the new `Suite::per_user_default()` (the home directory from `getpwuid_r`, never `HOME`,
  `XDG_DATA_HOME`, `LOCALAPPDATA`, `RIGHTKIT_SUITE_ROOT`, `RIGHT_SUITE_DIR` or `ORTHIC_SHARED_DIR`),
  and refuses an explicit suite that came from `Suite::resolve()` (`Suite::env_derived()`). Hosts
  read no variable themselves; `Suite::resolve_ignoring_env_overrides(bool)` is the same switch for
  them. Release builds of an app and of its engine set it; QA builds do not. Windows fails closed
  until `SHGetKnownFolderPath` is wired in. `Suite::resolve()` is unchanged.
- Additive example helpers `examples/svc_fakehost.rs` (an impostor that counts the bytes it
  receives) and `examples/svc_verify_client.rs`; tests in `tests/verify_host.rs`.

- Fix: a handler result larger than the host's `frame_limit` was dropped without a word, so the
  client waited for its whole deadline (120 s by default). The client now receives the typed,
  non-retryable error `ERROR_RESPONSE_TOO_LARGE` ("response_too_large") straight away and the
  connection stays usable. The host is told through the new `ServiceEvent::FrameTooLarge
  { conn, method, kind, limit }` (`kind` is "response" or "event"; an oversized emitted event is
  still not sent, and is now reported). Exhaustive matches on `ServiceEvent` need an arm.
- Additive: `CallContext::peer()` returns the `PeerIdentity` the OS verified for the connection
  (uid on Unix, user SID on Windows, pid on Linux, macOS and Windows) before any byte was read.
  The macOS pid is new: it is read with the `LOCAL_PEERPID` socket option.
  `CallContext::app()` is documented as what it is: a label the client declares. The label is a
  claim; the peer identity is the fact.
- Fix (gap ledger row 167): a legal 48-byte service name can now claim its endpoint on macOS and
  Linux. The Unix socket fallback used when the in-suite path is over 100 bytes was
  `<tmp>/rk-<tag>-<service>.sock`, about 65 bytes for a 48-byte name, so under macOS's ~49-byte
  `$TMPDIR` it exceeded `sun_path` (104 on macOS, 108 on Linux) and bind failed. It is now the
  bounded `<tmp>/rk-<tag>-<fnv1a64(service) as 16 hex>.sock` (33 bytes whatever the name length).
  The hash is FNV-1a 64, stable across Rust versions and processes; the exact string is pinned
  by a test.
- Additive: `try_endpoint_for` returns `EndpointError { path, len, limit }` when even the short
  fallback cannot fit (absurdly long temp dir); `HostError` gains a variant `Endpoint(EndpointError)`
  (exhaustive matches on `HostError` need an arm). `endpoint_for` keeps its signature and returns
  the path that did not fit on failure. New `MAX_UNIX_SOCKET_PATH` (103).
- Upgrade compatibility (one release): a host records its endpoint in its discovery record and
  clients connect to the recorded endpoint, so an old host is found whatever its name. For callers
  that look up by name, `endpoint_candidates` / `find_live_endpoint` try the current endpoint, then
  the OLD long fallback name, connect-only and never bound, and only for names where that old path
  fitted the socket limit (otherwise no old host could have bound it, so there is nothing to find).
  The old-name lookup is removed in the next release.
- Windows: pipe names are `\\.\pipe\rightkit-<service>-<user>-<tag>`, at most 96 characters for a
  48-byte service name and 20-character user; the limit is 256. No change.

- Additive: `valid_service_name(&str) -> bool` and `MAX_SERVICE_NAME_LEN` (48) are now public, so callers
  can check a derived name against the exact rule `host` enforces (non-empty, <= 48 bytes, ASCII
  `[a-z0-9-]`). The rule itself is unchanged.

Peer authentication hardening (gap ledger #59). Public API is additive, with two caveats:
`ServiceEvent` gains a variant (`PeerRejected`) and `InstalledApp` gains a field
(`completeness`); exhaustive matches and struct literals in callers need updating.

- Unix: new `verify_peer_is_current_user(&UnixStream) -> Result<PeerIdentity, PeerError>`.
  `getpeereid` on macOS and the BSDs, `SO_PEERCRED` on Linux/Android; the peer's effective uid
  must equal `geteuid`. Any other Unix returns `PeerError::Unsupported`; it never returns `Ok`
  on a platform that cannot check. Before this release the check existed only on macOS and
  Linux accepted every peer, relying on the 0700 directory and 0600 mode alone.
- The host now verifies the peer on every accept (Unix and Windows) before reading any byte.
  A mismatch or unidentifiable peer is closed and reported as the new
  `ServiceEvent::PeerRejected { reason }`. A peer that vanishes before it can be identified
  (a probe, the shutdown wake-up) is closed silently.
- Clients verify the server too: Unix clients check the server socket owner, closing the
  hole where a squatter at a fallback `/tmp` endpoint would receive `hello`. A mismatch is a
  `PermissionDenied` connect error.
- Windows: new `verify_pipe_peer_is_current_user(RawHandle)` (server side,
  `GetNamedPipeClientProcessId` + process token user SID vs the current user SID) and
  `verify_pipe_server_is_current_user(RawHandle)` (client side, `GetNamedPipeServerProcessId`).
  The client now opens pipes with `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` (the server
  can identify, never impersonate) and verifies the server before any byte is exchanged.
- Windows: new `owner_only_security_attributes()` returning `OwnerOnlySecurityAttributes`
  (SDDL `D:P(A;;GA;;;<current user SID>)` built from the real token SID), usable for named
  pipes, mutexes and events; the pipe listener now uses it. Lifetime is documented on the type.
- New `HostOptions` and `host_with_options`; `HostOptions::max_pipe_instances(u32)` caps
  Windows pipe instances (default `DEFAULT_MAX_PIPE_INSTANCES` = 16, clamped to 1..=254, never
  unlimited; previously `PIPE_UNLIMITED_INSTANCES`). `host()` is unchanged and uses the default.
  Ignored on Unix.
- New `DiscoverOptions { include_incomplete }`, `discover_macos_with` and
  `discover_apps_in_with`. With `include_incomplete`, an app whose bundle is installed but
  whose engine is missing is returned with `completeness: Completeness::EngineMissing`
  (a complete install in any root still wins). Default `false` reproduces the old results;
  `InstalledApp::completeness` is `Complete` for everything the old functions returned.
  Windows discovery reports only complete installs by default.
- `discover_windows_with(entries, apps, engine_exists, dir_exists, &DiscoverOptions)`: `include_incomplete` now applies on Windows too. An entry whose install directory exists but whose engine is missing is returned as `Completeness::EngineMissing` when requested; a complete install of the same app always wins. `discover_windows` is unchanged. `discover_apps_in_with` passes the options through on Windows. The Windows branch is `cfg(windows)` and is unverified on this host; the `discover_windows_with` logic is covered by a platform-neutral unit test.
- **Idle wake-ups (runtime budget).** The host's takeover monitor no longer sleeps 25 ms and reads
  `takeover.json` on every pass (7.4 to 11.3 wake-ups per second measured while idle). It blocks on a
  condition variable with a one-second timeout and wakes early on three signals: the host releasing
  (shutdown, drop or a completed handoff stops the monitor at once, with no sleep left to outlive it),
  `write_takeover` / `publish_takeover` or a would-be host writing a takeover request in the same
  process (looks immediately), and the timeout (stats `takeover.json`: modification time and length;
  the file is read only when that changed, and a request already judged not to oblige is not judged
  again until it changes). **Behaviour change:** a takeover requested by a different process is now
  noticed up to about one second later than before (it was noticed within about 25 ms); the requester's
  `takeover_timeout` (default 60 s) is unaffected. A host also looks once as soon as its monitor starts.
  In-flight requests are handed off exactly as before, and the handoff grace wait now ends early if the
  host is released meanwhile. Public API unchanged. Tests: the monitor stops from a 10-minute wait on
  release, finds a request written without notification by its file check, finds an in-process one
  without waiting for its interval, does not re-read an unchanged file, and wakes at most three times in
  a quiet two seconds; the decision (`obliging_rank`) is tested for rank, own process, dead requester,
  staleness and role-only peers. The `higher_rank_takes_over_with_a_retryable_handoff` end-to-end test
  now runs a 5 s in-flight job so the handoff still lands inside it. Not yet re-measured: the recorded
  budget evidence predates this change.

## 0.1.4

- Spawn-claim locking uses std `File::try_lock` instead of a direct `libc::flock`; the direct `libc`
  dependency is removed (the remaining Unix FFI in `transport.rs` was already hand-declared).
- `write_atomic` (service records and takeover requests) now writes through
  `rightkit_fs::atomic_write_private`: unique temp file, `fsync` of file and directory, owner-only.
  The Windows transient "access denied" rename retry is kept. Adds the `rightkit-fs` dependency
  (`fs` feature only).
