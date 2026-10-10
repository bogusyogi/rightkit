# RightKit Rust crate audit, group 4

Read-only static audit (Read/Grep/`wc -l` only; no cargo run). Paths are relative to `/Volumes/D/claude/rightkit/crates/` unless stated. Public-API counts are declared `pub fn/struct/enum/trait/const/type` items found by grep (items declared in `mod`s that may not be re-exported), shown as `top-level items / indented (method) items`. "Dependents" = other workspace `Cargo.toml`/`package.json` naming the crate.

Verdict legend: needed, unused, feature-gate-it, replace-with, heavy.

## Summary matrix

| Crate | Version | Publish | src LOC | needed | unused | gate | replace | heavy |
|---|---|---|---|---|---|---|---|---|
| rightkit-sandbox | 0.3.3 | yes | 6046 | 3 | 0 | 1 | 0 | 0 |
| rightkit-scenario | 0.0.0 | publish=false | 3436 | 4 | 0 | 0 | 0 | 0 |
| rightkit-search | 0.2.0 | yes | 1234 | 3 | 0 | 0 | 0 | 0 |
| rightkit-secrets | 0.1.2 | yes | 1853 | 10 | 0 | 0 | 1 | 0 |
| rightkit-service | 0.1.3 | yes | 2908 | 5 | 0 | 0 | 1 | 0 |
| rightkit-settings | 0.1.0 | yes | 664 | 4 | 0 | 0 | 0 | 0 |
| rightkit-shell | 0.1.4 | yes | 6891 | 16 | 0 | 0 | 0 | 1 |
| rightkit-speech-client | 0.3.2 | yes | 1809 | 6 | 0 | 0 | 0 | 0 |
| rightkit-sync | 0.1.0 | yes | 3179 | 2 | 0 | 0 | 0 | 0 |
| rightkit-tauri | 0.1.2 | yes | 532 | 2 | 0 | 0 | 0 | 0 |
| rightkit-transcript | 0.1.0 | publish=true | 977 | 5 | 0 | 0 | 0 | 0 |
| rightkit-tts | 0.1.0 | publish=true | 1839 | 7 | 0 | 1 | 1 | 0 |
| rightkit-vad | 0.0.2 | publish=true | 509 | 2 | 0 | 1 | 0 | 0 |
| rightkit-watch | 0.3.1 | yes | 3673 | 4 | 0 | 1 | 0 | 0 |
| desktop-app-template src-tauri (rightapp) | 0.1.0 | publish=false | 357 (+16 build.rs) | 9 | 1 | 0 | 0 | 0 |
| desktop-app-template engine (rightapp-engine) | 0.1.0 | publish=false | 109 | 2 | 1 | 0 | 0 | 0 |
| shell example-app | 0.1.0 | publish=false | 151 | 6 | 1 (dev) | 0 | 0 | 0 |

Note: rows count dependency table entries (incl. dev/build/target-cfg). thiserror is classed "needed (marginal)" everywhere it is used.

## cargo-machete hints, verified

| Hint | Verdict | Evidence |
|---|---|---|
| rightkit-shell: webview2_com_038, windows_061 | FALSE POSITIVE | Renamed optional packages consumed only through the alias module `pub use ::{webview2_com_038 as webview2_com, windows_061 as windows}` at `rightkit-shell/src/lib.rs:48` (and 039/062 at :50); used crate-wide as `crate::winapi::windows::...` (e.g. `clipboard.rs:170-176`, `harden.rs:166-171`, `idle.rs:209-237`). |
| shell-example-app: rightkit-service | TRUE POSITIVE | Listed only under `[dev-dependencies]` (`rightkit-shell/example-app/Cargo.toml`); zero `rightkit_service` references in `src/` or `tests/e2e.rs`. |
| shell-example-app: tauri-build | FALSE POSITIVE | `example-app/build.rs:2` calls `tauri_build::build()`. |
| desktop-app-template engine: serde | TRUE POSITIVE | `crates/engine/src/main.rs` and `tests/cli_contract.rs` use only `serde_json`; no `serde::`/derive anywhere. (`rightapp-protocol` has its own serde and does use it.) |
| rightapp (src-tauri): serde | TRUE POSITIVE | `src-tauri/src/*.rs` and `tests/supervisor.rs` use only `serde_json`; no derive/`serde::`. |
| rightapp (src-tauri): tauri-build | FALSE POSITIVE | `src-tauri/build.rs:16` calls `tauri_build::build()` (and `tauri_build::is_dev()` at :3). |

Additional finding (template version pins, `desktop-app-template/src-tauri/Cargo.toml:21-23`): pins `rightkit-process = "=0.2.1"`, `rightkit-tauri = "=0.1.1"`, `rightkit-logs = "=0.2.0"`. `packages/release/rightkit-versions.json` has `cargo` rightkit-process 0.1.0 / rightkit-tauri 0.1.0 (lines 270, 272) and `stagedCargo` 0.3.3 / 0.1.2 (lines 302, 315). The two template pins match neither section; the header comment says `pnpm check:template` enforces equality (not run here; unverified).

---

## rightkit-sandbox

**Profile.** OS sandbox (Seatbelt / Landlock+seccomp / AppContainer), PTY lease, process-tree kill, reserve-before-spawn resource authority. v0.3.3, publishable. Features: none. Dependents: none in workspace (no Cargo.toml or package.json references it; only ownership catalog/policy JSON).

| Module | LOC |
|---|---|
| windows_appcontainer.rs | 1870 |
| windows_ancestors.rs (+ windows_ancestor_tests.rs 775, `#[path]` included at appcontainer:18, ancestors:937) | 938 |
| spawn.rs | 452 |
| pty.rs | 402 |
| linux.rs | 357 |
| bin/rightkit-sandbox-exec.rs | 231 |
| denial.rs | 232 |
| process_tree.rs | 228 |
| resources.rs | 161 |
| seatbelt.rs | 160 |
| policy.rs | 136 |
| error.rs / lib.rs / windows_job.rs | 51 / 48 / 5 |

Public API: 38 top-level items (18 struct, 9 enum, 9 fn, 2 const) / 54 methods (+1 nested struct). Windows-only surface: `AppContainerAncestorLease`, sweeps (`lib.rs:37-41`).

| Dependency | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-process | 0.3.3 path | no | `spawn.rs:7,40`, `windows_job.rs:5` (re-exports `WindowsJob`) | needed | Owned spawn + Job Object reuse. |
| portable-pty | 0.9 | no | `pty.rs:13,147,325,368` (4 hits, one file) | feature-gate-it | Pulls a wide tree (nix, filedescriptor, serial, winapi...) for one 402-line module. Sandbox-only callers pay for it; make a `pty` feature. |
| libc [unix] | 0.2 | no | 47 hits: `linux.rs` 19, `process_tree.rs` 24, `seatbelt.rs` 4 | needed | Landlock/seccomp syscalls and proc enumeration. |
| windows [windows] | 0.62; Foundation, System_Threading, System_JobObjects, Security | no | `windows_appcontainer.rs:36-45` (core, HANDLE, CreateProcessW family). Most of the file and all of `windows_ancestors.rs` hand-declare `extern "system"` (appcontainer:154,194,208,213; ancestors:11-37) | needed | Trim: `Win32_System_JobObjects` is not referenced in sandbox (job lives in rightkit-process); `Win32_Security` is only needed because `CreateProcessW` signature is gated on it. |

**Overlap / split.**
- vs `rightkit-process`: `process_tree.rs` (descendants/kill_tree/is_alive with ps-style enumeration, `process_tree.rs:81-190`) is a second tree-kill implementation. `spawn.rs:349` and `pty.rs:389` call it while the owned path uses `OwnedChild::terminate_tree` (`rightkit-process/src/owned.rs:448-560`). Candidate: move bounded descendant enumeration into rightkit-process and have sandbox call one API. Job Object is already shared correctly (`windows_job.rs:5`).
- Split candidates: (1) `pty.rs` + `portable-pty` (independent lease-fencing PTY, 402 LOC) as `rightkit-pty` or a feature; (2) `windows_appcontainer.rs`+`windows_ancestors.rs` (2808 LOC, 46% of the crate, compiles only on Windows) as a platform half, e.g. `rightkit-sandbox-windows`; (3) `resources.rs` (`ResourceAuthority`) is policy unrelated to confinement.
- Zero in-workspace consumers: confirm product use before investing in the split.

---

## rightkit-scenario

**Profile.** RK8 scenario interpreter, durable state machine, append-only fsynced journal, leases/fencing tokens, remote worker wire. v0.0.0, `publish = false`. Features: none. Dependents: none (referenced only from ownership/policy JSON).

| Module | LOC |
|---|---|
| wire.rs | 758 |
| worker.rs | 719 |
| controller.rs | 602 |
| machine.rs | 444 |
| journal.rs | 179 |
| driver.rs | 149 |
| transport.rs | 143 |
| net.rs | 119 |
| protocol.rs | 120 |
| error.rs / clock.rs / lib.rs | 96 / 56 / 51 |

Public API: 78 top-level (27 struct, 25 fn, 11 const, 10 enum, 3 trait, 2 type) / 41 methods.

| Dependency | Spec | Optional | Used where | Verdict | Reason |
|---|---|---|---|---|---|
| blake3 | 1.8.7 | no | 15 hits: `controller.rs` 2, `wire.rs` 9, `worker.rs` 4 (keyed MAC, `derive_key` at `wire.rs:119`) | needed | Frame MAC + bundle hash. Note: default blake3 compiles C/asm via cc; `features=["pure"]` is an option. Workspace pins 3 different blake3 req strings (daemon "1", record "1.8", scenario "1.8.7"). |
| getrandom | 0.3 | no | `wire.rs` 1 hit (nonce) | needed | CSPRNG nonces. |
| serde | 1.0.228 derive | no | derives in driver, journal, protocol, transport, wire | needed | Journal + wire. |
| serde_json | 1.0.150 | no | 12 hits across driver, journal, machine, protocol, wire, worker | needed | JSON-lines journal and wire. |

**Overlap / split.** Two independent responsibilities: (a) local state machine (`machine`, `journal`, `driver`, `clock`, `error`, ~1000 LOC) and (b) remote worker wire (`wire`, `controller`, `worker`, `net`, `protocol`, `transport`, ~2400 LOC, 70%). (b) re-implements length-prefixed framing, device-key enrolment and signed/MAC'd requests that also exist in `rightkit-daemon` (device enrolment, signed requests, blake3-keyed uploads; `rightkit-daemon/Cargo.toml:6,28`) and framing primitives in `rightkit-framed-sidecar`. Recommend split into `rightkit-scenario` (core) + `rightkit-scenario-wire`, and reconcile device enrolment/MAC with rightkit-daemon rather than a second scheme (`wire.rs:1-21` notes no encryption; `net.rs:1-9` blocks non-loopback).

---

## rightkit-search

**Profile.** Product-neutral search query language: lexer, AST, operator values, SQL/FTS5 predicate compiler, snippets, in-memory filename index. v0.2.0, publishable. Features: `default=["sqlite"]`, `sqlite` (dep:rusqlite), `filename-index` (no deps). Dependents: none (policy/versions JSON only).

| Module | LOC |
|---|---|
| ast.rs | 283 |
| sql.rs | 280 |
| filename.rs (feature `filename-index`) | 214 |
| terms.rs | 174 |
| snippet.rs | 90 |
| values.rs | 86 |
| lexer.rs | 85 |
| lib.rs | 22 |

Public API: 37 top-level (12 struct, 10 fn, 9 enum, 6 const) / 27 methods.

| Dependency | Spec | Optional | Used where | Verdict | Reason |
|---|---|---|---|---|---|
| rusqlite | 0.40.1, default-features=false | yes (`sqlite`, default on) | `sql.rs:17-19` only: `impl rusqlite::ToSql for Param` | needed (already gated) | One trait impl. Consider making `sqlite` non-default so non-SQLite consumers do not link libsqlite3-sys; rightkit-db/embed already carry their own rusqlite. |
| rusqlite [dev] | 0.40.1 bundled | no | `tests/sqlite_e2e.rs` | needed | E2E against real FTS5. |
| tempfile [dev] | 3 | no | `tests/sqlite_e2e.rs:37`, `tests/filename_index.rs:32` | needed | Fixtures. |

**Overlap / split.** `filename.rs` ("Cockpit-derived" substring/ext/size/kind index) overlaps `rightkit-fsindex` (whole-disk fuzzy name search, `rightkit-fsindex/src/lib.rs:1`); decide which owns filename search. Otherwise the crate is cohesive (query language + SQL compile). Zero consumers in the workspace.

---

## rightkit-secrets

**Profile.** Per-app OS credential storage (Keychain / Credential Manager), passive presence, non-interactive mode, DB-key provisioning, migration, Apple synced-credentials helper client, CLI binary. v0.1.2, publishable. Features: `settings-probe` (dep:rightkit-settings); no defaults. Dependents: rightkit-oauth (optional `secrets` feature, `rightkit-oauth/Cargo.toml:17,40`), rightkit-record (hard dependency, `Cargo.toml:27`, path-only with no version).

| Module | LOC |
|---|---|
| macos.rs | 394 |
| store.rs | 321 |
| migrate.rs | 223 |
| windows.rs | 205 |
| apple_helper.rs | 159 |
| backend.rs | 152 |
| keyring_compat.rs | 107 |
| bin/rightkit-secrets.rs | 90 |
| namespace.rs | 79 |
| error.rs / secret.rs / lib.rs | 47 / 42 / 34 |

Public API: 30 top-level (14 struct, 7 enum, 4 fn, 3 type, 1 trait, 1 const) / 55 methods.

| Dependency | Spec | Optional | Used where | Verdict | Reason |
|---|---|---|---|---|---|
| serde | 1.0.228 derive | no | `migrate.rs` 4, `apple_helper.rs` 2, `store.rs:91`, bin 6 | needed | Serialize derives on reports/diagnostics. |
| serde_json | 1.0.150 | no | `apple_helper.rs` 2, bin 6, `migrate.rs` 1 | needed | Helper protocol + CLI output. |
| thiserror | 2 | no | `error.rs:5` (10 `#[error]`) | needed (marginal) | Single enum; hand `Display` would drop a proc-macro dep. |
| getrandom | 0.3 | no | `store.rs:293`, `macos.rs:327` | needed | Key generation. |
| base64 | 0.22 | no | `store.rs:5,294` (one `STANDARD.encode`) | replace-with (inline) | One call site; there are already two hand-rolled base64s in this group (`rightkit-shell/src/b64.rs`, `rightkit-sync/src/helper/wire.rs:16-31`); extract one tiny shared helper. |
| zeroize | 1.8 | no | `secret.rs`, `store.rs`, `keyring_compat.rs`, `apple_helper.rs`, `windows.rs` | needed | Secret hygiene. |
| rightkit-settings | 0.1.0 path | yes (`settings-probe`) | `store.rs` 3 hits | needed (gated) | Presence-only adapter. |
| windows-sys [windows] | 0.61; Foundation, Security_Credentials, Security_Cryptography, System_Threading | no | `windows.rs:11-20` | needed | All four features referenced. Note: sandbox/service use `windows` 0.62, shell uses 0.61/0.62; secrets alone uses windows-sys 0.61 (version spread). |
| keyring-core [windows dev] | 1 | no | `tests/windows_keyring_compat.rs` | needed | Compat test. |
| windows-native-keyring-store [windows dev] | 1.1.0 | no | same test | needed | Compat test. |
| tempfile [dev] | 3 | no | `tests/e2e.rs:42,57` | needed | Fixtures. |

**Overlap / split.** `apple_helper.rs` (spawn helper, one JSON line in/out, `Command::new` at :69) and `rightkit-sync/src/helper/process.rs:94` are two ad-hoc stdio-JSON helper clients; both could sit on `rightkit-process` + `rightkit-framed-sidecar`. `settings::SecretRef/Presence/SecretProbe` (`rightkit-settings/src/secret.rs`) vs `Store::exists` is a deliberately thin adapter (fine). `rightkit-tauri::SecureStore<T>` (get/set/delete) is a third secret-storage trait. The CLI bin ships inside the library package; acceptable, but the macOS FFI (394 LOC) and Windows backend are platform halves of one cohesive crate (no split recommended).

---

## rightkit-service

**Profile.** Discovery/auth convention for local services: `service.json`, ranking and takeover, user-only Unix socket / named pipe, per-app allowlist, installed-app discovery, spawn claims. v0.1.3, publishable. Features: none. Dependents: rightkit-shell, rightkit-shell example-app (dev), rightkit-automation, rightkit-control (optional `tauri-plugin`), rightkit-jobs, rightkit-speech-client, rightkit-agent-tools, rightkit-qa, `spikes/rk2-macos/svc-call`.

| Module | LOC |
|---|---|
| discovery.rs | 743 |
| server.rs | 648 |
| transport.rs | 643 |
| client.rs | 343 |
| record.rs | 207 |
| suite.rs | 192 |
| error.rs / lib.rs | 69 / 63 |

Public API: 68 top-level (31 fn, 14 struct, 14 const, 6 enum, 2 type, 1 trait) / ~53 methods.

| Dependency | Spec | Optional | Used where | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-framed-sidecar | 0.1.0 | no | `client.rs`, `server.rs` (1 each) | needed | Bounded JSON-lines framing. |
| serde | 1.0.228 derive | no | record 11, server 3, client/discovery/error 1 each | needed | Record/wire types. |
| serde_json | 1.0.150 | no | record 3, server 3, client/error 1 each | needed | Wire/record JSON. |
| libc [unix] | 0.2 | no | 3 hits, all `discovery.rs:316-322` (`flock`) | replace-with (std) | Stable `File::try_lock` (settings already uses `std::fs::TryLockError`, `rightkit-settings/src/store.rs:5`) removes the dep. The rest of the Unix FFI (`transport.rs:72-79` geteuid/recv/getpeereid, `:582` kill) is hand-declared `extern "C"` anyway, so libc is not even used consistently. Pick one. |
| windows [windows] | 0.62; Foundation, Security, Security_Authorization, Storage_FileSystem, System_IO, System_Pipes, System_Threading | no | `transport.rs:191-210,278,345,595-596` | needed | All features used. But `discovery.rs:468-480` hand-declares kernel32 `OpenProcess/GetProcessTimes` though `windows` is in scope. |
| tempfile [dev] | 3 | no | tests/examples | needed | Fixtures. |

**Overlap / split.**
- `discovery.rs` mixes three unrelated concerns: (1) installed-app discovery (plist/registry parsing, hardcoded `SUITE_APPS` list, `discovery.rs:1-250`), (2) spawn-claim/adopt-or-spawn (`:252-415`), (3) rank/takeover planning (`:527-660`). (1) is brand-specific knowledge (`KNOWN_APPS`, deprecated export at `lib.rs:42-44`) inside a "brand-neutral" crate; candidate for a `rightkit-service-apps` module/crate or move to the speech/agent layer.
- vs `rightkit-speech-client`: speech-client re-types the service record/takeover/plan with HeardRight `HostRole` and re-declares paths and endpoint naming: `speech-client/src/protocol.rs:146-197` (`ServiceRecord`, `TakeoverRequest`, `service_dir`, `service_record_path`, `takeover_request_path`, `endpoint_for` with its own FNV-1a `suite_root_tag`) vs `service/src/record.rs`/`transport.rs:36-57` (`record_path`, `takeover_path`, `endpoint_for`, own `fnv`). Runtime logic is delegated (`speech-client/src/lib.rs:119-227`), so remaining duplication is path/endpoint derivation and wire-compat record types. Keep the HeardRight endpoint scheme (`orthic-labs-speech-...` vs `rightkit-{service}-...`) as a documented compat shim or have service expose a `LegacyNaming` hook so it is not copy-pasted.
- `pid_alive` (`transport.rs:576-612`) and `process_start_ms` (`discovery.rs:416-500`) duplicate process liveness / creation-time logic in `rightkit-process/src/process_handle.rs` (`creation_time*`, `try_wait`).
- `write_atomic` (`record.rs:122-142` + `replace_with_retry`) duplicates `rightkit-fs::atomic_write_private`.
- `Suite::resolve` (`suite.rs:80-103`) is the fail-closed app-data root; `rightkit-tts::models::data_root` (`models.rs:40-55`) is a second, non-fail-closed root (falls back to `temp_dir()`).

---

## rightkit-settings

**Profile.** Versioned per-app settings persistence: migrations, typed defaults, atomic writes, corruption quarantine, change events. v0.1.0, publishable. Features: none. Dependents: rightkit-secrets (optional `settings-probe`), `packages/settings-ui/package.json` (npm description names it; no Cargo edge).

| Module | LOC |
|---|---|
| store.rs | 519 |
| atomic.rs | 104 |
| secret.rs | 32 |
| lib.rs | 9 |

Public API: 11 top-level (6 struct, 2 enum, 2 fn, 1 trait) / 11 methods.

| Dependency | Spec | Optional | Used where | Verdict | Reason |
|---|---|---|---|---|---|
| serde | 1.0.228 derive | no | `store.rs:2`, `secret.rs:1` | needed | Typed settings. |
| serde_json | 1.0.150 | no | `store.rs` throughout | needed | Envelope/value merge. |
| thiserror | 2 | no | `store.rs:17` (7 `#[error]`) | needed (marginal) | One enum. |
| tempfile [dev] | 3 | no | `tests/settings.rs` | needed | Fixtures. |

**Overlap / split.** Direct overlap with `rightkit-fs`: `atomic.rs` (temp file + fsync + `ReplaceFileW` FFI at `atomic.rs:58-104`) re-implements `rightkit-fs/src/atomic.rs:51-224` (`atomic_write`, `atomic_write_with`, `sync_dir`), and `store.rs:5` uses std `File::try_lock` for its cross-process lock where rightkit-fs has `lock`. Recommend `rightkit-fs = { default-features=false, features=["fs","lock"] }` and delete `atomic.rs`. Also overlaps `rightkit-shell/src/window_state.rs` (own versioned store with `migrate` and tmp+rename at `:111-127`): shell should persist via rightkit-settings (or both on rightkit-fs). No split needed.

---

## rightkit-shell

**Profile.** Shared Tauri desktop shell: webview hardening, window state, menu, tray, single instance, deep links, autostart, rich clipboard, watchdog, engine sidecar wiring. v0.1.4, publishable. Features: `default=["engine","wv2-0_38"]`; `wv2-0_38` (windows 0.61 + webview2-com 0.38), `wv2-0_39` (0.62/0.39), `engine` (rightkit-jobs + rightkit-process), `devtools` (tauri/devtools). Dependents: `rightkit-shell/example-app`, `packages/shell/package.json` (npm side). No Rust crate in `crates/*` or the template depends on it (the template builds its own shell: `desktop-app-template/src-tauri/Cargo.toml` has no rightkit-shell).

| Module | LOC |
|---|---|
| lib.rs | 676 |
| notify.rs | 617 |
| commands.rs | 520 |
| broker.rs | 398 |
| presence.rs | 393 |
| clipboard.rs | 366 |
| window_state.rs | 323 |
| windows.rs | 294 |
| idle.rs | 273 |
| menu.rs | 257 |
| open_url.rs | 334 |
| harden.rs | 225 |
| tray.rs | 221 |
| autostart.rs | 222 |
| stream.rs / lifecycle.rs / recovery.rs | 216 / 216 / 207 |
| instance.rs / nav.rs / deep_link.rs | 183 / 172 / 165 |
| engine.rs / frameless.rs / error.rs | 147 / 134 / 131 |
| watchdog.rs / opener.rs / b64.rs | 99 / 58 / 44 |

Public API: 201 top-level (105 fn, 53 struct, 26 const, 11 enum, 5 type, 1 trait) / ~141 methods (incl. 5 async).

| Dependency | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| tauri | 2; tray-icon, image-png | no | pervasive | needed | The point of the crate. Default features (wry, compression, common-controls-v6) stay on; fine for a host crate. |
| rightkit-service | 0.1.0 | no | `instance.rs:13`, `lib.rs:113,452` (3) | needed | Single-instance via service host. |
| rightkit-jobs | 0.2.2 | yes (`engine`, DEFAULT ON) | `engine.rs:12` (1) | heavy | Default feature drags rightkit-jobs, which depends on rightkit-db (rusqlite), time, uuid, rightkit-fs (`rightkit-jobs/Cargo.toml`) into every shell consumer for one `EngineClient` use. Make `engine` non-default (example-app already opts in with `features=["engine"]`). |
| rightkit-process | 0.3.0 | yes (`engine`) | `engine.rs:13` (1) | needed (gated) | OwnedCommand. |
| serde | 1.0.228 derive | no | 49 hits in 14 files | needed | Config/spec types. |
| serde_json | 1.0.150 | no | 20 hits in 12 files | needed | IPC payloads. |
| objc2 [macos] | 0.6 | no | 10 hits (clipboard, harden, notify, presence) | needed | AppKit FFI. |
| objc2-foundation [macos] | 0.3, 10 features | no | clipboard, frameless, harden, notify, presence (1 each) | needed | Verified type use: NSArray, NSData, NSString, NSURL, NSBundle, NSDictionary, NSError, NSPoint, NSNumber. Features NSValue/NSGeometry/NSObject are consistent with those types; not individually proven. |
| objc2-user-notifications [macos] | 0.3 | no | `notify.rs` 1 | needed | UN center. |
| block2 [macos] | 0.6 | no | `notify.rs` 1 | needed | Completion blocks. |
| objc2-app-kit [macos] | 0.3 | no | clipboard, frameless, windows (4) | needed | NSPasteboard, NSWindow. |
| tauri-winrt-notification [windows] | 0.7 | no | `notify.rs` 1 | needed | Toasts. |
| windows_061 [windows] | pkg windows 0.61, 6 features | yes (`wv2-0_38`, default) | via `lib.rs:48`; `clipboard.rs:170-176` | needed | machete false positive. |
| webview2_com_038 [windows] | pkg webview2-com 0.38 | yes (`wv2-0_38`) | via `lib.rs:48`; `harden.rs:166-171`, `idle.rs:209-237` | needed | machete false positive. |
| windows_062 / webview2_com_039 [windows] | 0.62 / 0.39 | yes (`wv2-0_39`) | via `lib.rs:50` | needed (alt generation) | Alternate to above; tracks Tauri 2.12. |
| tempfile [dev] | 3 | no | `tests/policy.rs:274` | needed | Fixtures. |

**Overlap / split.**
- vs `rightkit-tauri`: not duplicate code. `rightkit-tauri` has no tauri dependency (README states "no direct tauri dependency"; it is license/device/paid-command/log-bridge traits over `rightkit-license`). The actual Tauri host adapters all live in rightkit-shell. Naming is misleading; fold `rightkit-tauri` into `rightkit-license` (feature `host`) or rename (`rightkit-license-host`). rightkit-shell does not use it and the template uses `rightkit-tauri::DeviceService` directly (`src-tauri/src/lib.rs:9`).
- Persistence overlap: `window_state.rs:111-127` (versioned JSON + migrate + tmp/rename) and `recovery.rs:162-166` (rename dance) vs `rightkit-settings` and `rightkit-fs::atomic_write`; `b64.rs` (44 LOC hand-rolled base64) vs sync/secrets (see secrets).
- Split candidates (all in a 6.9k LOC crate with 25 modules): (1) macOS notifications/presence/clipboard/frameless native halves vs Windows halves; (2) `broker.rs` (PathBroker, no Tauri dependency in its logic) and `stream.rs`/`nav.rs` policy types could be a Tauri-free `rightkit-shell-policy`; (3) `engine.rs` (sidecar wiring) is already feature-gated and is the natural carve-out. Compile-time win comes from (3) first.
- Template gap: `desktop-app-template` does not consume `rightkit-shell`, so the "shared shell" is not exercised by the scaffold.

### shell example-app (`rightkit-shell/example-app`, standalone workspace, publish=false)

| Dependency | Spec | Where | Verdict | Reason |
|---|---|---|---|---|
| tauri | 2 | main.rs | needed | App. |
| serde_json | 1.0.150 | main.rs:13,136-137 | needed | IPC. |
| rightkit-shell | path, `engine` | main.rs:11-12 | needed | Subject under test. |
| rightkit-control | path, `tauri-plugin` | main.rs:9; also duplicated in `[dev-dependencies]` | needed (dup entry) | Dev-dependency copy is redundant; normal deps are visible to tests. |
| rightkit-process | 0.3.0 path | main.rs:10 | needed | OwnedCommand. |
| rightkit-framed-sidecar | path | main.rs:25 | needed | JSON-line engine fake. |
| tauri-build [build] | 2 | build.rs:2 | needed | machete false positive. |
| rightkit-qa [dev] | path | tests/e2e.rs:12-14 | needed | QA harness. |
| rightkit-service [dev] | path | no references | unused | Remove (machete true positive). |

---

## rightkit-speech-client

**Profile.** Client/contract for the shared speech service (one resident engine per machine) built on rightkit-service; wire-compatible with HeardRight v1. v0.3.2, publishable. Features: none. Dependents: rightkit-agent-tools (optional `speech`), rightkit-asr (dev-dependency only).

| Module | LOC |
|---|---|
| lib.rs | 946 |
| protocol.rs | 863 |

Public API: 67 top-level (21 const, 20 fn, 16 struct, 9 enum, 1 type) / 64 methods.

| Dependency | Spec | Optional | Used where | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-service | 0.1.3 | no | `lib.rs` 13 hits, `protocol.rs` 1 | needed | Discovery/takeover/transport. |
| rightkit-framed-sidecar | 0.1.0 | no | `lib.rs:24` (`read_json_line`) | needed | Bounded read. |
| serde | 1.0.228 derive | no | `protocol.rs` 14, `lib.rs` 4 | needed | Wire types. |
| serde_json | 1.0.150 | no | `lib.rs` 4, `protocol.rs` 3 | needed | Wire JSON. |
| tempfile [dev] | 3 | no | `tests/contract.rs:72`, `e2e.rs`, `windows_pipe.rs` | needed | Fixtures. |
| windows [windows dev] | 0.62, 4 features | no | `tests/windows_pipe.rs:14` | needed | Raw pipe server in test. |

**Overlap / split.** Discovery/takeover is correctly delegated to rightkit-service (`lib.rs:119-227` are thin role-vocabulary wrappers). Residual duplication: `protocol.rs:146-197` path helpers and `endpoint_for` + FNV tag (`:183-210`) mirror `service` (`record_path`, `takeover_path`, `endpoint_for`, `fnv`); `lib.rs:141-227` re-declares `HostPlan`, `plan_host`, `should_request_takeover`, `takeover_obliges_yield`, `read_takeover`, `request_takeover`, `clear_takeover_if_ours`, `pid_alive`, `read_live_record` as wrappers (about 130 LOC of mapping). Candidate: make these generic over a `Role` trait in rightkit-service so speech-client only supplies the `HostRole` table. `TimedWord` (`protocol.rs:213`, i64 ms) is a third distinct TimedWord (see transcript). No split; merge-candidate only with rightkit-asr's client side.

---

## rightkit-sync

**Profile.** Brand-neutral cloud sync engine: change tokens, tombstones, LWW conflicts, resumable batches, offline queue, backoff; plus helper-process backend (`rightkit-sync-helper/1`) and fake helper binary. v0.1.0, publishable. Features: none. Dependents: none in Rust; the Swift package `swift/RightKitCloudKitHelper` implements the same wire protocol (`Package.swift:4`, `Session.swift:1`, `main.swift:1`).

| Module | LOC |
|---|---|
| engine.rs | 1184 |
| bin/rightkit-sync-fake-helper.rs | 354 |
| helper/mod.rs | 337 |
| helper/process.rs | 251 |
| helper/wire.rs | 231 |
| memory.rs | 221 |
| traits.rs | 213 |
| record.rs | 132 |
| ledger.rs | 99 |
| storage.rs | 53 |
| error.rs / backoff.rs / lib.rs | 51 / 32 / 21 |

Public API: 53 top-level (29 struct, 9 trait, 6 fn, 6 enum, 2 type, 1 const) / 68 methods (+4 const).

| Dependency | Spec | Optional | Used where | Verdict | Reason |
|---|---|---|---|---|---|
| serde | 1.0.228 derive | no | wire 15, fake-helper 10, helper/mod 5, ledger 4, record/traits 1 | needed | Ledger + wire. |
| serde_json | 1.0.150 | no | fake-helper 7, helper/mod 5, ledger 2, process 2, wire 2, traits 1 | needed | JSON. |

**Overlap / split.**
- Two responsibilities: core engine/ledger/traits/memory (about 2000 LOC, no I/O) vs helper-process transport (`helper/*` 819 + fake helper 354, about 1170 LOC). Split into `rightkit-sync` + `rightkit-sync-helper` (the fake helper is test infrastructure that ships as a `[[bin]]` in the published crate via `include=src/**`).
- vs `rightkit-process` / `rightkit-framed-sidecar`: `helper/process.rs:94-160` hand-rolls spawn (no `OwnedCommand`, so no tree ownership), a bounded line reader (`:119-142`, vs `read_json_line`), and a stderr ring buffer (`:149-156`, vs `rightkit-process/src/stderr_tail.rs`). Same pattern in `rightkit-secrets/src/apple_helper.rs`.
- vs `rightkit-fs`: `storage.rs:30-37` `FileStorage::save` is `write` + `rename` with no fsync and a fixed `.tmp` name, although the module doc says "Durable ledger bytes". Use `rightkit-fs::atomic_write_private`.
- `helper/wire.rs:16-31` hand-rolled base64 (third copy in this group).
- No consumers: confirm a Rust consumer exists before publishing.

---

## rightkit-tauri

**Profile.** Framework-agnostic native host services for Tauri license/device adapters: secure-store mutation ordering, device-id get-or-create, authorization against signed entitlements, paid-command guard, log/diagnostic host bridges. v0.1.2, publishable. Features: none. Dependents: `desktop-app-template/src-tauri` (pinned `=0.1.1`); no `crates/*` dependent.

| Module | LOC |
|---|---|
| lib.rs | 532 |

Public API: 19 top-level (8 struct, 5 trait, 5 enum, 1 fn) / 16 methods.

| Dependency | Spec | Optional | Used where | Verdict | Reason |
|---|---|---|---|---|---|
| getrandom | 0.3 | no | `lib.rs:141` (`getrandom::fill` for UUIDv4) | needed | Device id. |
| rightkit-license | 0.1.3 path | no | `lib.rs:8-9` | needed | `verify_token`, `Entitlement`. |

**Overlap / split.** Misnamed: no `tauri` dependency (README: "no direct tauri dependency"). It is the host half of `rightkit-license` (`rightkit-license` description: "The Rust (security) half of @rightkit/license"). Candidate merge: `rightkit-license` feature `host`, or rename. `SecureStore<T>` overlaps `rightkit-secrets::Backend/Store`; `LogHost/LogService/DiagnosticHost/DiagnosticService` (`lib.rs:361-424`) are thin wrappers that the template bypasses by calling `rightkit-logs` directly (`src-tauri/src/diagnostics.rs:4`). Single-module crate; no split.

---

## rightkit-transcript

**Profile.** Product-neutral word-timed transcript contract, cue segmentation, SRT/VTT/JSON import/export. v0.1.0, `publish=true`. Features: `asr-roundtrip` (dep:rightkit-asr, test-only), no defaults. Dependents: rightkit-tts (optional `system`), rightkit-align.

| Module | LOC |
|---|---|
| segment.rs | 354 |
| contract.rs | 188 |
| parse.rs | 160 |
| export.rs | 117 |
| words.rs | 79 |
| align.rs | 49 |
| lib.rs | 30 |

Public API: 31 top-level (13 fn, 8 struct, 6 enum, 4 const) / 9 methods.

| Dependency | Spec | Optional | Used where | Verdict | Reason |
|---|---|---|---|---|---|
| serde | 1.0.228 derive | no | contract 13, export 5, parse 3, segment 2, words 1 | needed | Contract types. |
| serde_json | 1.0.150 | no | export 5, parse 3, contract 2 | needed | Timing JSON. |
| thiserror | 2 | no | `contract.rs:78`, `parse.rs:7` | needed (marginal) | Two enums. |
| rightkit-asr | 0.0.2 path | yes (`asr-roundtrip`) | `tests/asr_roundtrip.rs` only | needed (test-only) | A published crate carries an optional edge to an unpublished-tier crate solely for one test; move that test into rightkit-asr (as a dev-dependency there) to keep the transcript manifest clean. |
| hound [dev] | 3.5 | no | `tests/asr_roundtrip.rs` only (feature-gated test) | needed | Dev deps are built for all tests though only the gated one uses it; negligible. |

**Overlap / split.** Speech pipeline: three separate `TimedWord` types (`transcript/src/words.rs:12` u64 ms; `speech-client/src/protocol.rs:213` i64 ms; `rightkit-asr/src/longform.rs:89`), and `rightkit-asr/src/longform.rs:418-527` (`group_cues`, `render_subtitles`, `render_timing_json`) re-implements what `transcript::build_cues`/`to_srt`/`to_timing_json` already own. `tts::system` and `rightkit-align` already consume transcript correctly. Recommend asr and speech-client convert to `rightkit_transcript::TimedWord/Word` at their boundaries. Cohesive; no split.

---

## rightkit-tts

**Profile.** Shared local TTS: OS voices (say/SAPI) with word-timed manifest, Qwen3-TTS (QTS) worker client, optional vendored engine worker. v0.1.0, `publish=true`. Features: `default=["system","client"]`; `system` (rightkit-transcript), `client` (rightkit-ort no-default, rightkit-process, sha2, serde, serde_json), `engine` (client + qts + libc), `metal`, `vulkan`. Dependents: none in workspace Rust (only ownership/policy JSON).

| Module | LOC |
|---|---|
| client.rs | 657 |
| system.rs | 383 |
| worker.rs | 228 |
| models.rs | 215 |
| chunk.rs | 178 |
| wav.rs | 73 |
| protocol.rs | 53 |
| lib.rs / bin worker | 36 / 16 |

Public API: 51 top-level (28 fn, 12 struct, 8 const, 3 enum) / 8 methods.

| Dependency | Spec | Optional | Used where | Verdict | Reason |
|---|---|---|---|---|---|
| serde | 1.0.228 derive | yes (`client`) | `protocol.rs` 4, `models.rs` 1 | needed | Worker protocol. |
| serde_json | 1.0.150 | yes (`client`) | `client.rs` 2, `models.rs` 2, `worker.rs` 2 | needed | Worker protocol. |
| sha2 | 0.10 | yes (`client`) | `models.rs:8` | replace-with | `models.rs` is a manifest verifier with a `.verified.json` cache (`:1-6`); `rightkit-fs` already has a `digest` module (sha2 0.11) used by `rightkit-models`. Using it removes sha2 0.10 and the duplicate verifier (workspace is split sha2 0.10 vs 0.11). |
| thiserror | 2 | no (unconditional) | `system.rs:115` only (4 `#[error]`) | feature-gate-it | With `default-features=false, features=["client"]` the proc-macro is compiled for nothing. Make optional under `system`. |
| rightkit-ort | 0.2.2 path, default-features=false | yes (`client`) | `models.rs:159,171`, `worker.rs:76` (path/discovery only) | needed | Correctly avoids the `ort` crate. |
| rightkit-process | 0.3.0 | yes (`client`) | `client.rs:10,201` | needed | Owned worker spawn. |
| rightkit-transcript | 0.1.0 | yes (`system`) | `system.rs:4,246` | needed | Word-timed manifest. |
| qts (pkg rightkit-qts) | 0.2.0 | yes (`engine`) | `worker.rs:7`, `client.rs`/`models.rs` comments | needed | Engine worker. |
| libc [unix] | 0.2 | yes (`engine`) | `worker.rs:35` (`_exit`) | needed | Skip ggml teardown crash. |

**Overlap / split.**
- `system` (OS voice + transcript manifest) and `client`/`engine` (QTS) share no code except `wav.rs`; two independent responsibilities already separated by features. Splitting `rightkit-tts-system` vs `rightkit-tts-qts` would give separate version/publish lines; low priority since features isolate deps.
- `models.rs` vs `rightkit-models`+`rightkit-fs`: manifest/SHA-256 verification and model dir (`models.rs:1-72`) is duplicate responsibility; `data_root()` (`:40-55`) invents a third data root (`$HOME/Orthic Labs/RightKit`, falls back to `temp_dir()`) distinct from `rightkit-service::Suite::resolve` (fail-closed, platform dirs). Intra-crate: `chunk::split_text` (`chunk.rs:22`) vs `system::chunk_sentences` (`system.rs:18`) are two chunkers.
- Speech pipeline consistency: tts is the only crate that depends on transcript for output; vad/asr/speech-client do not.

---

## rightkit-vad

**Profile.** Voice activity detection: Silero scorer (caller-supplied model), hysteresis/padding/chunking policy, streaming frame carry. v0.0.2, `publish=true`. Features: `default=["rightkit-ort/tracing"]`, `download-binaries`, `tls-native`, `copy-dylibs` (forward to rightkit-ort), `load-dynamic = []` (empty). Dependents: rightkit-asr (`default-features=false`, `rightkit-asr/Cargo.toml:40`).

| Module | LOC |
|---|---|
| spans.rs | 229 |
| silero.rs | 120 |
| stream.rs | 100 |
| lib.rs | 60 |

Public API: 17 top-level (7 struct, 4 fn, 3 const, 1 enum, 1 trait, 1 type) / 11 methods.

| Dependency | Spec | Optional | Used where | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-ort | 0.2.2 path, default features (`session`) | no | `silero.rs:2-3,91` only | feature-gate-it | Only `Silero` needs ONNX. `spans.rs` (hysteresis/chunking) and `stream.rs` are pure. asr uses `rightkit_vad::Span` only (`rightkit-asr/src/chunked.rs:9`), and because the dep is unconditional with rightkit-ort defaults, `default-features=false` on vad does not remove `ort`. Gate `Silero` behind `silero` (default on). |
| hound [dev] | 3.5 | no | `tests/e2e_silero.rs:70` | needed | WAV fixture. |
| rightkit-media [dev] | 0.2.3 path | no | `tests/e2e_silero.rs:17` | needed | NativeCatalog check. |

**Dead feature:** `load-dynamic = []` (`Cargo.toml` features) is empty; rightkit-ort already always enables ort `load-dynamic`. Remove or document.

**Overlap / split.** Pure policy (`spans`, `stream`) vs ONNX scorer (`silero`) is the natural split via a feature. `rightkit-audio`/`rightkit-asr` own audio capture/decoding; vad correctly owns none. Speech pipeline: asr -> vad (Span) is the only edge.

---

## rightkit-watch

**Profile.** Filesystem watching: ref-counted async subscribers (CodeRight donor), thread-based debounced file-set/directory watchers (ViewRight donor), typed recursive streams, rename-storm coalescing; all on `notify`. v0.3.1, publishable. Features: none. Dependents: none in workspace.

| Module | LOC |
|---|---|
| options.rs | 1375 |
| subscribe.rs (async) | 903 |
| typed.rs | 572 |
| sync.rs | 552 |
| coalesce.rs | 242 |
| lib.rs | 29 |

Public API: 28 top-level (19 struct, 4 enum, 4 const, 1 trait) / 41 methods (3 async).

| Dependency | Spec | Optional | Used where | Verdict | Reason |
|---|---|---|---|---|---|
| notify | 8.2.0 | no | options 17, subscribe 13, typed 11, sync 9, coalesce 2, lib 1 | needed | Backend. |
| tokio | 1.53.0; rt, sync, time, macros | no | `subscribe.rs` only (7 hits: `time::sleep_until`, `sync::{mpsc,Notify,Mutex}`, `runtime::Handle`, `select!`) | feature-gate-it | `sync.rs`/`typed.rs`/`options.rs`/`coalesce.rs` are explicitly runtime-free (`lib.rs:7-9`). Gate `subscribe` behind an `async` (tokio) feature; non-async consumers shouldn't compile tokio. |
| tracing | 0.1 | no | `options.rs:556,563`, `sync.rs:391-392`, `subscribe.rs:32,606,619,678,685` (about 8 `warn!`) | needed | Light; keep. |
| tempfile [dev] | 3 | no | tests | needed | Fixtures. |
| tokio [dev] | rt-multi-thread, macros, time, sync | no | `tests/e2e.rs:261,295,334` | needed | `#[tokio::test]`. |

**Overlap / split.** Four event vocabularies in one crate: `WatchEvent` (typed.rs:48), `WatchEventRecord`/`WatchEventKind` (options.rs:20,65), `FileWatcherEvent` (subscribe.rs:36), `FileChange/FileChangeKind` (sync.rs:49,57). `options.rs` (1375 LOC) is the largest module and mostly adapts one vocabulary to another. Two independent halves: async subscriber (tokio, `subscribe.rs`) vs sync watchers (`sync.rs`, `typed.rs`, `coalesce.rs`); split or feature-gate on the runtime boundary. vs `rightkit-fsindex`: its macOS FSEvents change feed (`rightkit-fsindex/src/backend/macos.rs`) is a second change-feed implementation outside notify; decide whether whole-disk indexing should consume rightkit-watch events or stays separate for scale. `rightkit-sync` and `rightkit-http` do not overlap.

---

## desktop-app-template/src-tauri (package `rightapp`, publish=false) — consumer

LOC: lib.rs 86, engine_supervisor.rs 197, diagnostics.rs 69, main.rs 5, build.rs 16. Dependents: n/a.

| Dependency | Spec | Where | Verdict | Reason |
|---|---|---|---|---|
| rightapp-protocol | path | `engine_supervisor.rs:15` | needed | Framed protocol. |
| tauri | 2, `protocol-asset` | `lib.rs:11,68-70` | needed | Host. |
| tauri-plugin-os / -process / -updater | 2 | `lib.rs:68-70` | needed | One `.plugin(...)` each. |
| serde | 1 derive | none (only `serde_json` used in `src/*.rs`, `tests/supervisor.rs`) | unused | Remove. |
| serde_json | 1 | `lib.rs:10`, `diagnostics.rs:8`, `engine_supervisor.rs:17` | needed | Values. |
| dirs | 6 | `lib.rs:20` | needed | App data dir. |
| rightkit-logs | =0.2.0 | `diagnostics.rs:4` | needed | Standard diagnostics. |
| rightkit-process | =0.2.1 | `engine_supervisor.rs:16` | needed (stale pin) | Registry has 0.3.3 staged; pin matches neither feed section. |
| rightkit-tauri | =0.1.1 | `lib.rs:9` | needed (stale pin) | Staged 0.1.2. |
| tauri-build [build] | 2 | `build.rs:3,16` | needed | machete false positive. |
| serde_json [build] | 1 | `build.rs:5-12` | needed | TAURI_CONFIG tweak. |

## desktop-app-template/crates/engine (package `rightapp-engine`, publish=false) — consumer

LOC: main.rs 109; tests/cli_contract.rs.

| Dependency | Spec | Where | Verdict | Reason |
|---|---|---|---|---|
| rightapp-protocol | path | `main.rs:10` | needed | Framed protocol. |
| serde | 1 derive | none | unused | Remove (machete true positive). |
| serde_json | 1 | `main.rs:11,93` | needed | Values. |

Overlap: the template's `engine_supervisor.rs` (spawn, framed RPC, restart) is the same pattern as `rightkit-shell/src/engine.rs` + `rightkit-jobs::EngineClient`; the template uses rightkit-process directly and skips rightkit-shell. The template's `diagnostics.rs` duplicates the `LogService` bridge in `rightkit-tauri`.

---

## Cross-crate overlap matrix (requested focus areas)

| Pair | Finding |
|---|---|
| service vs speech-client | Runtime delegation is correct; duplicated path/endpoint/FNV naming and re-typed record/takeover/plan wrappers remain (see above). Service's `discovery.rs` also holds brand-specific installed-app discovery (`KNOWN_APPS`). |
| shell vs tauri | Not overlapping code; `rightkit-tauri` has no Tauri dependency and is really `rightkit-license` host glue. Shell holds all Tauri adapters. Rename or merge `rightkit-tauri`. Shell and the template do not use each other. |
| sandbox vs process | Two tree-kill implementations (`sandbox/process_tree.rs` vs `process/owned.rs` + `process_unix.rs`); Job Object shared properly. |
| settings vs fs | settings re-implements atomic write + `ReplaceFileW` FFI + file lock that rightkit-fs owns. Also service `write_atomic`, shell `window_state.rs:123-127`/`recovery.rs:162-166`, sync `storage.rs:30-37` (no fsync) are further hand-rolled atomic writes. |
| transcript / tts / vad / asr | Three `TimedWord` definitions; asr `longform.rs:418-527` re-implements transcript cue/SRT/timing-JSON; tts duplicates models/manifest verification (`models.rs`) of rightkit-models/rightkit-fs and uses a non-fail-closed data root. vad pulls ONNX unconditionally for pure policy code. |
| sync vs fs vs http | sync has no http dependency (backend is the Swift CloudKit helper over stdio JSON); overlap is with fs (non-durable `FileStorage`) and with process/framed-sidecar (hand-rolled helper spawn, line reader, stderr tail). Same helper pattern in `secrets/apple_helper.rs`. |
| search vs fsindex | `search::filename` vs `rightkit-fsindex` filename index. |
| watch vs fsindex | Separate change feeds (notify vs FSEvents). |
| scenario vs daemon/framed-sidecar | scenario wire re-implements framing/enrolment/MAC. |
| base64 (x3) | `shell/b64.rs`, `sync/helper/wire.rs:16-31` hand-rolled; `secrets` uses the base64 crate for one call. |
