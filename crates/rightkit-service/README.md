# rightkit-service

Local service discovery and authentication for Right Suite apps. It owns the convention for `service.json` discovery, a current-user-only transport (Unix socket on macOS and Unix, named pipe on Windows), per-app request allowlists, rank-based takeover, and the framed request, response, and event protocol built on `rightkit-framed-sidecar`.

**Use it** when two or more local processes must share one resident service on a machine, or when a local service must accept calls only from known apps running as the current user.

**Do not use it** for network services. The transport is local only: no TCP, no tokens.

## Features

None. The crate has no Cargo features.

## Platform support

- macOS and the BSDs: socket in a 0700 directory, mode 0600, peer uid checked with `getpeereid` on every accept and by every client.
- Linux and Android: same, with `SO_PEERCRED` (generic ABI; the mips, powerpc and sparc families report `Unsupported` rather than guess).
- Other Unix: the peer check reports `PeerError::Unsupported`, so the host refuses every peer. It never assumes the peer is you.
- Windows: named pipe with a DACL that allows only the current user SID, remote clients rejected, first-instance flag set, at most `HostOptions::max_pipe_instances` instances, client token SID checked on every accept, `SECURITY_IDENTIFICATION` clients that check the server SID.
- Verifying the host (`Client::connect_expecting`): macOS, Linux and Windows. Other Unix refuses with the typed `HostIdentityReason::Unverified` until implemented; it never connects unverified. See "Verifying the host".
- Platform gates: `cfg(windows)`, `cfg(unix)`, `cfg(target_os = "macos")`, and `cfg(target_os = "linux")` in `src/discovery.rs`, `src/host_identity.rs`, `src/record.rs`, `src/suite.rs`, and `src/transport.rs`.

Published version: see `INDEX.md` at the repository root. Release notes: see `CHANGELOG.md`.

## Example

The compiled examples live in `examples/` (`svc_host.rs`, `svc_call.rs`, `svc_claim.rs`, `svc_mute.rs`, `svc_race.rs`, `svc_fakehost.rs`, `svc_verify_client.rs`). Run them through `rightkit cargo run -p rightkit-service --example <name>`.

## Convention

The one discovery and authentication convention for local Right Suite services: the shared speech service, the in-app `rightkit-control` server, `right-ctl`, and any future one.

| Concern | Convention |
|---|---|
| Discovery | `<suite>/runtime/<service>/service.json` (atomic, owner-only). Stale when its endpoint does not answer. |
| Transport | macOS/Unix: socket in a 0700 dir, mode 0600, peer uid checked (`getpeereid`). Windows: named pipe, DACL = current user SID only, remote clients rejected, first-instance flag. No TCP, no tokens. |
| Frames | `rightkit-framed-sidecar` JSON lines: `hello`, `request`, `event`, `response`; errors carry `retryable`. |
| Allowlist | Per calling app: which methods it may call. Unknown app rejected at `hello`. `ping` always allowed. |
| Ranking | Lower rank wins. A higher-ranked host writes `takeover.json`; the incumbent stops admitting work, lets in-flight requests reach a boundary (they return the retryable `host_handoff`), releases, and the requester binds. Clients re-read discovery and resubmit (`Client::call_retrying`). The incumbent looks for a request from another process once a second (a stat of `takeover.json`, no faster polling), so a takeover starts up to about a second after the request is written; a request written in the incumbent's own process wakes it at once. |
| Roots | `Suite::resolve()` fails closed when no absolute app-data root exists; `Suite::per_run_temp()` is the explicit alternative. Never the working directory. Default `<app data>/Orthic Labs/shared` (the root HeardRight, ScrapeRight and CutRight already use); `RIGHTKIT_SUITE_ROOT`, `RIGHT_SUITE_DIR`, `ORTHIC_SHARED_DIR` override. Those variables and `HOME` are chosen by whoever launches the process: release builds use `Suite::per_user_default()` / `ClientConfig::ignore_env_overrides(true)` instead (see "Verifying the host"). |
| Host identity | `Client::connect` checks that the host runs as you, not which program it is. `Client::connect_expecting` also requires the host's executable to be one you name, before any byte is written. |

Why a crate of its own and not a module of `rightkit-framed-sidecar`: the sidecar crate is a published, dependency-light, `unsafe`-free framing library. The service needs OS FFI (`getpeereid`, named pipes, SDDL), threads and a filesystem convention. Keeping them apart lets stdio-only consumers stay minimal; the service depends on the sidecar for its frames.

Tests are end-to-end: real host processes, real clients over the real socket. The `examples/` directory has `svc_host.rs` (host), `svc_fakehost.rs`, `svc_call.rs` and `svc_claim.rs` (clients), `svc_verify_client.rs`, `svc_mute.rs`, and `svc_race.rs`. `RIGHTKIT_SUITE_ROOT` (aliases `RIGHT_SUITE_DIR`, `ORTHIC_SHARED_DIR`) isolates a run from the user's installed apps.

## Verifying the host

The transport proves the program that answered runs as you. It does not prove which program it is. A client that reads discovery from a file and a path that any same-user program can write, and that sends its `hello` and requests (and any secret in them, such as a pairing key) to whatever answers, can be pointed at an impostor.

### How discovery works, and who can influence each input

`Client::connect(suite, service, app)`:

1. `suite` is whatever the caller built. `Suite::resolve()` takes it from the environment: `RIGHTKIT_SUITE_ROOT`, then `RIGHT_SUITE_DIR`, then `ORTHIC_SHARED_DIR`, then the per-user default derived from `HOME` (macOS), `XDG_DATA_HOME` or `HOME` (Linux), `LOCALAPPDATA` (Windows).
2. It reads `<suite>/runtime/<service>/service.json` with no owner, mode or liveness check, and rejects a different `protocol_major`.
3. It connects to the record's `endpoint` exactly as written (a Unix socket path, a pipe name). It does not recompute it.
4. After the connect, and before any byte: Unix compares the peer's uid with its own (`getpeereid`/`SO_PEERCRED`); Windows compares the pipe server process's token SID with its own.
5. It writes `hello`, then requests.

| Input | Who controls it |
|---|---|
| `RIGHTKIT_SUITE_ROOT`, `RIGHT_SUITE_DIR`, `ORTHIC_SHARED_DIR`, `HOME`, `XDG_DATA_HOME`, `LOCALAPPDATA` | whoever launches the client |
| `service.json` (so the `endpoint`, and a `pid` nobody reads) | any process of the same user (0600 in a 0700 directory keeps other users out, not the same user) |
| the socket path, when no host holds it | any process of the same user can bind it |
| `TMPDIR`, `USERNAME` | feed endpoint naming (`try_endpoint_for`, and the name-based `endpoint_candidates`/`find_live_endpoint`); `Client` connects to the recorded endpoint, so they do not steer it |

Step 4 is the only check, and it says "same user". A same-user impostor at the end of a redirected root, a forged record or a squatted socket passes it and receives the hello.

### The API

```rust
use rightkit_service::{Client, ClientConfig, ConnectError, HostExpectation};

// An engine shipped inside the app (MailRight.app/Contents/Resources/bin/engine) names its
// app without a hard-coded path: <bundle>/Contents/MacOS/mailright.
let expect = HostExpectation::bundle_executable("mailright").expect("inside the app bundle");
let config = ClientConfig::new("mailright-control", "engine")
    .ignore_env_overrides(!cfg!(feature = "qa")); // release builds refuse environment redirection
match Client::connect_expecting(&config, &expect) {
    Ok(client) => { /* only now send the pairing key */ }
    Err(ConnectError::HostIdentity(e)) => { /* e.found_pid, e.found_executable, e.expected, e.reason */ }
    Err(ConnectError::Service(e)) => { /* no record, unreachable, timeout, hello refused; e.retryable */ }
    Err(_) => {}
}
```

* `HostExpectation::executable(path)`, `any_of(paths)`, `or_executable(path)`: named paths. An expectation with no path matches nothing.
* `sibling_executable(name)` (next to the running executable), `bundle_executable(name)` (`<App>.app/Contents/MacOS/<name>` of the bundle that contains the running executable), `relative_to_current_exe(levels_up, path)`: derive the host path from where the running executable lives. These fail with an `io::Error` when the layout does not fit; they never guess.
* `with_verifier(|host: &HostProcess| ...)`: an extra check that runs after the executable matched, before any byte is written; `Err(reason)` refuses the host as `HostIdentityReason::Refused`.
* `Client::verified_host()`: the `HostProcess` (pid, uid, executable, macOS audit token) a verified client was connected to. `None` for `Client::connect`.
* `Client::call_retrying_expecting`, `open_stream_expecting`: the retrying and raw-stream entry points with the same check.
* `Client::connect`, `connect_with`, `call_retrying`, `open_stream` are unchanged and remain unauthenticated-host. Their documentation says so.

A refusal is `ConnectError::HostIdentity(HostIdentityError { expected, found_pid, found_executable, reason })`, distinct from `ConnectError::Service`. It is not retryable. Collapsed with `into_service_error()` it is `ServiceError` code `host_identity`.

### Order: nothing is sent before the check

`connect_expecting` resolves the suite, reads the record, calls `connect()` on the socket, asks the kernel who is on the other end, and only then wraps the stream and writes `hello`. The kernel-level `connect` is the only thing that has happened on the wire when a host is refused: the impostor sees a connection that closes without a byte, and the client's stream is dropped on every error path. The tests assert it with a real impostor process that counts the bytes it receives (zero).

### What each platform verifies

| | Process | Executable | The pid is proven to name one process |
|---|---|---|---|
| macOS | uid by `getpeereid`; pid by `LOCAL_PEERPID` (the process that last owned the accepted socket, normally the acceptor) | `proc_pidpath(pid)` | start time (`proc_pidinfo` `PROC_PIDTBSDINFO`) and the `LOCAL_PEERTOKEN` audit token (it carries a pid version) read before and after the path, and the pid re-read from the socket: all equal |
| Linux | uid and pid by `SO_PEERCRED` (the process that called `listen()`) | `readlink /proc/<pid>/exe`; file identity from `stat /proc/<pid>/exe` | `/proc/<pid>/stat` start time before and after the path, and the pid re-read from the socket |
| Windows | user SID of the pipe's server process (`GetNamedPipeServerProcessId`, process token) | `QueryFullProcessImageNameW` on a process handle; file identity from `GetFileInformationByHandle` (volume serial and file index) | the process handle is held throughout (the pid cannot be reused while it is open); creation time (`GetProcessTimes`) before and after the path, still running (`WaitForSingleObject`), and the pid re-read from the pipe |
| other Unix | not implemented: `Unverified` | | |

The executable matches when the canonicalised found path equals a canonicalised expected path, or when both name the same file (device and inode). The second rule is needed on macOS: volumes are case-insensitive and `realpath` keeps the spelling it was given, so `/applications/x` and `/Applications/x` differ as strings and are one file. A hard link to the genuine binary is the genuine binary. An expected path that does not exist matches nothing.

No Security.framework call is made by this crate. The macOS code-signature team identifier check is the caller's: `with_verifier` receives `HostProcess::audit_token`, the race-free handle for `SecCodeCopyGuestWithAttributes` with `kSecGuestAttributeAudit`, then `SecCodeCopySigningInformation` for `kSecCodeInfoTeamIdentifier`, compared with the client's own (`SecCodeCopySelf`).

### Environment hardening

`ClientConfig::ignore_env_overrides(true)` makes the suite root come only from `Suite::per_user_default()`: the home directory from the account database (`getpwuid_r(geteuid())`), never `HOME`, and none of `RIGHTKIT_SUITE_ROOT`, `RIGHT_SUITE_DIR`, `ORTHIC_SHARED_DIR`, `XDG_DATA_HOME`, `LOCALAPPDATA`. A suite handed in from `Suite::resolve()` (see `Suite::env_derived()`) is refused under the switch, so the switch cannot be defeated by passing the environment in. On Windows the root is `<local app data>\Orthic Labs\shared` with the folder from `SHGetKnownFolderPath(FOLDERID_LocalAppData)`, which reads the profile's registry, never `%LOCALAPPDATA%`.

* Release builds of an app and of its command-line engine set `ignore_env_overrides(true)`. QA builds and isolated test runs do not, because they depend on `RIGHTKIT_SUITE_ROOT`.
* Hosts read no environment variable themselves: `host()` takes the `Suite` the app built. The same switch for a host is `Suite::resolve_ignoring_env_overrides(true)` (or `Suite::per_user_default()`), and a host and its hardened clients must resolve the same way. On Linux that means a user who moved `XDG_DATA_HOME` is hosted at `~/.local/share` by a hardened host, not at their `XDG_DATA_HOME`.
* The switch and the expectation are independent. With the switch off, a redirected client still refuses the impostor it is redirected to, because it is not the app.

### What this closes, and what it does not

**Closes:** redirection of a client by the environment (the switch); impersonation of the host by another program, through a redirected root, a forged `service.json`, a socket bound at the recorded path, a stale record whose socket another program now holds, or a copy of the app binary in another directory (the expectation).

**Does not close:**

* A same-user attacker who can replace the app binary, or write anything in its install directory, or attach a debugger or inject code into the real host or into the client. The attacker is then the host, or the client. On macOS a signed hardened-runtime app resists injection; an unsigned build does not. The team-identifier hook narrows binary replacement for signed apps.
* The sibling and bundle helpers inherit the trust of the running client's location. An attacker who can start a copy of the client from a directory they control also controls the "sibling" it names, and can capture what that client sends. A client that sends secrets should name an absolute install path or add a signature check with `with_verifier`.
* Identity is checked once, at connect. A host that later hands its connected socket to another process is not noticed.
* Root or Administrator.
* A host that is not the process that created or accepted the socket (a forking host): macOS reports the acceptor and Linux the process that called `listen()`. The identity may then be a parent or worker, and an accept landing between the two pid reads on macOS refuses the host as `HostChanged`.
* A host that replaced its binary on disk while running (an update): on Linux `/proc/<pid>/exe` then names `... (deleted)`, which does not match the new path, so the old host is refused until it restarts. On macOS the old inode is not reachable by path. The refusal is typed, never an accept.
* A Linux host that is non-dumpable (setuid, or `prctl(PR_SET_DUMPABLE, 0)`): `/proc/<pid>/exe` is unreadable to its own user and the host is refused as `Unreadable`. A pid namespace that hides the host's pid is refused as `PidUnavailable`.

### Windows notes

`HostProcess::sid` is the host's user SID and `uid` is `None`. Paths are compared after canonicalisation without regard to case; `HostProcess::executable` and `HostIdentityError::found_executable` are given without the `\\?\` prefix. A code-signature check is not built in: do it in `HostExpectation::with_verifier` (for example `WinVerifyTrust` on `HostProcess::executable`), which runs while the process handle is still held. A host running elevated, or as a protected process, may refuse `OpenProcess` to a non-elevated client and is then refused as `Unreadable`.

## Threat model

**Defended against:** another local user (or a service running as one) connecting to the endpoint to call the service; another local user squatting on the endpoint name or path so a client sends its `hello` and requests to an impostor; a remote client (Windows pipes reject remote clients); a server that tries to impersonate the calling user through the pipe (clients open with `SECURITY_IDENTIFICATION`); connection floods growing the host without bound (Windows: `max_pipe_instances`).

**Layers.** (1) Filesystem or DACL: 0700 directory and 0600 socket on Unix, a protected DACL granting only the current user SID on Windows (`owner_only_security_attributes`, also for mutexes and events). (2) Independent kernel identity check of the peer on every accept and every connect: `verify_peer_is_current_user` (Unix) or `verify_pipe_peer_is_current_user` / `verify_pipe_server_is_current_user` (Windows). The check runs before any byte is read or written, fails closed (an unidentifiable peer is rejected, an unsupported platform rejects everyone), and a rejection is reported to the host as `ServiceEvent::PeerRejected`.

**Not defended against:** a client that uses `Client::connect` talking to a same-user impostor (see "Verifying the host" for `Client::connect_expecting`); code already running as the current user. It passes every check by definition; the per-app allowlist is a key for routing, not a boundary against it. Root or Administrator. A process that hijacks the pid between the kernel recording it and the token lookup on Windows (the pipe DACL is the primary boundary there and the SID check is defense in depth). Local denial of service by the same user. A Unix peer is identified by effective uid; a setuid binary a different user runs appears as its owner.

**Platform note.** Cross-user behaviour cannot be exercised by the unit tests (it needs a second account). The decision function is tested with a foreign uid, the listener's rejection path (peer closed unread, reported as `Rejected`) is tested with an injected verifier, and an ignored test accepts a socket served by another account (`RIGHTKIT_SERVICE_FOREIGN_SOCKET`).

## Compatibility

Compatibility with HeardRight's speech service (see `rightkit-speech-client`): `service.json`/`takeover.json` tolerate peers that publish only `role` (rank via `ServiceConfig::known_roles`; an unresolvable rank is never displaced and never obliges a handoff), `engine_version` mirrors `version`, `ServiceConfig::endpoint` accepts a legacy endpoint name, `open_stream` gives raw line streams over the same user-only transport, and `pid_alive` is hardened (no process-group signals, Windows exit-code check).

## Runtime budget

Generated from `runtime-budget.json`; edit that file, then run `node packages/release/runtime-budgets.mjs sync <crate-dir>`.

- Resident memory ceiling: 22.0 MiB.
- CPU ceiling (idle, steady state): 1%.
- Wake-ups ceiling: undeclared.
- Disk operations ceiling: 1 operations per second.
- Required scale: 64 connections or more.
- Required hosts: not declared in `runtime-budget.json`.
- Status exposure: `Hosted::status()` in `src/server.rs`; exposes resident memory, idle CPU, wake-ups, disk operations.
- Measured: not yet measured.
