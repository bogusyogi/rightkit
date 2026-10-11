# Changelog

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

## 0.1.4

- Spawn-claim locking uses std `File::try_lock` instead of a direct `libc::flock`; the direct `libc`
  dependency is removed (the remaining Unix FFI in `transport.rs` was already hand-declared).
- `write_atomic` (service records and takeover requests) now writes through
  `rightkit_fs::atomic_write_private`: unique temp file, `fsync` of file and directory, owner-only.
  The Windows transient "access denied" rename retry is kept. Adds the `rightkit-fs` dependency
  (`fs` feature only).
