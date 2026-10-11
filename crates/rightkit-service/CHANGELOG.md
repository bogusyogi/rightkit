# Changelog

## Unreleased

- Fix: a handler result larger than the host's `frame_limit` was dropped without a word, so the
  client waited for its whole deadline (120 s by default). The client now receives the typed,
  non-retryable error `ERROR_RESPONSE_TOO_LARGE` ("response_too_large") straight away and the
  connection stays usable. The host is told through the new `ServiceEvent::FrameTooLarge
  { conn, method, kind, limit }` (`kind` is "response" or "event"; an oversized emitted event is
  still not sent, and is now reported). Exhaustive matches on `ServiceEvent` need an arm.
- Additive: `CallContext::peer()` returns the `PeerIdentity` the OS verified for the connection
  (uid on Unix, user SID on Windows, pid on Linux and Windows) before any byte was read.
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

## 0.1.5

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
