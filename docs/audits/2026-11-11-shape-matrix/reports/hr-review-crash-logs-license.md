# HeardRight vs RightKit: crash, logs, licence

Read-only comparison. Nothing was built or run; every claim marked **verified** was read in the source, **suspicion** was inferred, **not verified** was not read.

Trees:

- LIVE = `<scratch>/survey/heardright-live` (0.1.213, commit 12b7d8ce)
- MAIN = `<scratch>/survey/heardright-main` (0.1.231, unreleased)
- RK = `/home/user/rightkit`

App paths below are relative to `tauri-app-next/` in LIVE or MAIN unless they start with `RK/`.

## Verdicts

| Package | Verdict | Reason |
|---|---|---|
| `rightkit-crash` | Right shape with listed gaps | It reproduces the live panic report, native sample/minidump, dump helper, retention and hang capture, and main already runs on it. It does not, and should not, hold the 5.3k lines of usage-telemetry upload that the inventory maps to it; that pipeline was retired on main. Small capture differences remain (1 s sample, hang capture cooldown). |
| `rightkit-logs` + `@rightkit/logs` | Right shape | Live already used both. Main moved the file writer onto `JsonlStore` with the same 10 MiB, one-predecessor rotation, and added renderer crash capture. HeardRight keeps its own queue and content redaction on top, which the package allows. |
| `rightkit-license` + `@rightkit/license` | Right shape with listed gaps | The native client was built from HeardRight's own licence code and reads the live keychain items in place; lifecycle, clock, lease and device-binding rules match. Main still carries about 600 lines the pinned version could not cover (trial platform, removal drain, account and link commands), and main's pin (=0.2.8) lacks a 0.2.9 fix for a Pro refusal during renew. |

## 1. rightkit-crash

### 1.1 What the live app does

Inventory: 28 files, 8,567 lines mapped to `rightkit-crash`. None of it was RightKit at the live commit; the only RightKit use in this area was `rightkit-logs =0.1.0` (`heardright-telemetry/Cargo.toml:12`). By content, the 8.5k lines are three different things:

- **Crash capture, about 0.8k lines.**
  - Panic hook: `src-tauri/src/crash_report.rs:22-69`. Writes a plain-text `crash/crash_<unix>.log` (version, thread, location, message, backtrace), redacts quoted values in the message (`src-tauri/src/panic_redact.rs`), takes a native dump, logs a `panic` event, then chains the previous hook. Build profile is `panic = "abort"` (`Cargo.toml:45`).
  - Native dump: `heardright-telemetry/src/crash_dump.rs`. macOS `/usr/bin/sample <pid> 2 -file` (1 s for `ui_stall`), 6 s timeout, written straight into the crash folder unscrubbed (`:258-303`). Windows out-of-process `heardright-dump-helper.exe`, 12 s timeout, `.partial` then rename (`:114-242`). Retention 5 dumps, 10 logs, 50 MiB (`:22-24`, `:358-365`).
  - Dump helper: `heardright-dump-helper/src/main.rs`, `MiniDumpNormal | MiniDumpWithThreadInfo` (`:66`).
  - Callers: panic, 5 s main-thread hang (`src-tauri/src/hang_watchdog.rs:33,124`), macOS 200 ms UI stall with sampling only when `HEARDRIGHT_SAMPLE_UI_STALLS=1` (`src-tauri/src/ui_stall_watchdog.rs:43,117`), sidecar exit (`heardright-app-runtime/src/engine_supervisor/threads.rs:243`).
- **Diagnostics export and send, about 0.8k lines.** `src-tauri/src/commands/diagnostics.rs`. ZIP of log files, `diagnostics-summary.json` from `rightkit_logs::collect_diagnostics`, and every file in the crash folder including `.dmp` and unscrubbed samples (`:438-462`, `:544`). `send_diagnostics` uploads that same ZIP to `https://heardright.app/v1/diagnostics` with a persistent install id header (`:663-710`). Feedback: `heardright-telemetry/src/telemetry_upload/feedback.rs` posts to `/v1/feedback` with the persistent install id.
- **Usage telemetry, about 5.3k lines.** Local event log (`telemetry.rs`, `telemetry_writer.rs`), outbox (`telemetry_outbox.rs`, 1,065 lines), and an uploader to `https://heardright.app/v1/telemetry/ingest` (`telemetry_upload/mod.rs:47`, `schedule.rs:79-131`, `:286-330`). Gate: Developer Mode or the `telemetry_usage` setting (`telemetry_upload/payload.rs:5-8`); onboarding calibration failures upload without that opt-in (`schedule.rs:193-256`); AI polish payloads are queued when `telemetry_usage` is on (`payload.rs:45-56`). Started at launch (`src-tauri/src/app_run.rs:773`).

Note: `legal/PRODUCT_SCHEDULE.md:65` in LIVE says "the released app has no telemetry uploader". The live code above contradicts that (verified by reading; whether the endpoint accepted events is not verified).

### 1.2 What RightKit provides

`RK/crates/rightkit-crash` (source 0.3.8; INDEX.md records 0.3.6 as published; main pins =0.3.6). Public API in `src/lib.rs:40-67`:

- `install_panic_hook`, `install_process_hook(ProcessKind, CrashConfig)`, `record_report`, `record_helper_exit`, `CaughtPanicScope` (`src/hook.rs`).
- `CrashReport`: redacted JSON, message capped at 2,000 chars, backtrace at 48 KiB (`src/report.rs:5-88`).
- `capture_native`, `capture_native_for_pid`, `set_dump_helper`, `NativeCapture` (`src/native.rs`); `dump_helper::run` (`src/dump_helper.rs`).
- `HangWatchdog`, `HangOptions`, `Heartbeat` (`src/hang.rs`).
- `prune`, `Retention` (`src/retention.rs`).
- `enqueue_pending`, `list_pending` into the consented `rightkit-logs` outbox (`src/pending.rs`).
- Features: `support-bundle` (ZIP, `admit_crash_artifacts`), `window-errors` (`record_window_error`), `diagnostics` (`init_diagnostics`).
- Platforms: macOS sample, Windows minidump, Android exit history (README says not run on a device), others report-only.

It has no feedback call, no diagnostics upload transport and no usage-telemetry pipeline; the README says upload goes through `rightkit-logs`.

### 1.3 Behaviour differences

| # | Live | RightKit / main | Status |
|---|---|---|---|
| C1 | Report is text `crash_<unix>.log` (`crash_report.rs:51-60`) | JSON `crash_<kind>_<stamp>_<pid>_<seq>.json`, owner-only, atomic (`RK hook.rs:568`) | Verified. Old `.log` files on an upgraded install are no longer admitted to the export (`MAIN commands/diagnostics.rs:465-470`); `prune` still removes them by the `crash_` prefix (`RK retention.rs:30`). |
| C2 | Message: quoted values redacted only | `redact_quoted` plus pattern scrub on message, backtrace, thread, location, plus `redact_values` (`RK report.rs:57-88`) | Verified. Stricter. |
| C3 | macOS sample 2 s for panic and hang (`crash_dump.rs:262,276`) | 1 s for every kind (`RK native.rs:359`) | Verified. Shorter evidence window; no option to change it. |
| C4 | Sample written unscrubbed into the crash folder (`crash_dump.rs:271-278`) | Written to a private temp dir, scrubbed, then published; withheld marker on failure (`RK native.rs:183-340`) | Verified by reading the function list and README; the scrub internals were not read line by line. |
| C5 | Windows dump flags and 12 s timeout | Same flags (`RK dump_helper.rs:166`), same timeout (`RK native.rs:455`) | Verified. |
| C6 | Retention 5 / 10 / 50 MiB | Same defaults (`RK retention.rs:13-21`); main passes them explicitly (`MAIN heardright-telemetry/src/crash_dump.rs:22-26`) | Verified. |
| C7 | Hang: 5 s threshold, one dump per episode, no cooldown (`hang_watchdog.rs:33-56`) | 5 s set by main (`MAIN app_run.rs:153-155`), but native capture repeats at most once per 10 minutes per loop (`RK hang.rs:27`); main does not override it | Verified constants; effect on a second hang within 10 minutes is a suspicion (report written, no dump). |
| C8 | UI-stall sampling available by env var | Main passes `capture: None` (`MAIN app_run.rs:139`) | Verified. Developer-only option lost. |
| C9 | Hook chains the previous hook, so the panic text reaches stderr | Release default does not chain; one fixed line (`RK hook.rs:104,327-352`) | Verified. Deliberate (rightkit#513). |
| C10 | Export and upload include every crash file, minidumps included (`commands/diagnostics.rs:438-462,664`) | Export admits scrubbed reports only; native artifacts only when the user opts in, and never in an upload (`MAIN commands/diagnostics.rs:444-517,690,721`) | Verified. |
| C11 | Persistent `telemetry-install-id` on feedback and diagnostics (`telemetry_upload/environment.rs:108-124`) | Fresh random id per send (`MAIN heardright-telemetry/src/feedback.rs:16-45`); old id file deleted (`legacy_telemetry_cleanup.rs`) | Verified. |
| C12 | Usage telemetry uploader and outbox | Not in `rightkit-crash`; removed from main | Verified. See 1.5. |
| C13 | No renderer error capture | `record_window_error` + `installWindowCrashCapture` | Verified. Gain. |
| C14 | No engine-process hook through this path (`heardright-runtime/src/crash_capture.rs` was its own code, target `rightkit-dictation`) | Engine uses `install_process_hook(ProcessKind::Engine, ..)` (`MAIN heardright-runtime/src/crash_capture.rs:43`) | Verified. |

### 1.4 What main calls today

Main calls RightKit for all crash capture; the remaining app code is glue (inventory: 16 files, 2,396 lines, most of it the diagnostics export and the local event log).

- Panic hook: `src-tauri/src/crash_report.rs:25` (`install_process_hook`), then `process::abort()` because the profile is now `panic = "unwind"` (`Cargo.toml:52`).
- Renderer errors: `src-tauri/src/crash_report.rs:99-101`, `src/main.tsx:33`.
- Native capture and prune: `heardright-telemetry/src/crash_dump.rs:43,59,76`.
- Hang: `src-tauri/src/app_run.rs:150-196` (`HangWatchdog::start`, `pump_via`).
- Dump helper: `heardright-dump-helper/src/main.rs:7` (`dump_helper::run`).
- Export: `commands/diagnostics.rs:428` (`create_zip`), `:465-470` (`list_pending`, `CrashReport`).
- Not used by main: `enqueue_pending`, `build_support_bundle`, `record_helper_exit`, `init_diagnostics` (grep, verified). Main re-implements the bundle admission rules by hand (`commands/diagnostics.rs:431-517`); RK source 0.3.8 now exports `admit_crash_artifacts` for exactly that, unpublished.

### 1.5 Changes after the live build

Deliberate:

- Usage telemetry upload, outbox and persistent install id removed; cleanup of the leftover files (`MAIN heardright-telemetry/src/legacy_telemetry_cleanup.rs:1-4`: "retired remote-telemetry pipeline").
- Native crash artifacts out of uploads and opt-in for local export (C10).
- `panic = "unwind"` with an explicit abort after the report (`MAIN src-tauri/src/crash_report.rs:1-10`).
- Panic text no longer on stderr (C9). Renderer error capture added (C13).

Possible regressions (none proven by a run):

- C3, 1 s sample instead of 2 s.
- C7, hang capture cooldown not set.
- C1, pre-upgrade `.log` reports dropped from exports.

### 1.6 Inventory note

`rightkit-dictation-telemetry` in `RK/crates/` is a moved copy of main's `heardright-telemetry` (README: moved 2026-11-01; every source file differs from main's today). The inventory still maps the same files to `rightkit-crash`. The feedback call and local event log belong with that crate, not `rightkit-crash`.

## 2. rightkit-logs and @rightkit/logs

### 2.1 What the live app does

Inventory lists no file under a logs target; the app-side code sits in the crash rows.

- Writer: `heardright-telemetry/src/telemetry_writer.rs`. One background thread, bounded queue of 256 (`:14`), events over 64 KiB replaced by a marker (`:13`), its own `OpenEventLog` appender with rotation at 10 MiB to a single `.1` (`:12`, `:52-97`). Queue overflow is recorded as `telemetry_queue_overflow`.
- Gate and redaction: local logging only when the diagnostics setting is on (`telemetry.rs:35-58,218-229`); every event through `heardright_core::redact_diagnostic_event` (`telemetry.rs:234`).
- Files: `events.jsonl`, `engine-events.jsonl`, `engine-command-events.jsonl`, `onboarding-calibration.jsonl` (`telemetry.rs:278-294`).
- Already RightKit at the live commit: `JsonlStore::clear` (`telemetry_writer.rs:234`), `collect_diagnostics`, `LogSource`, `CollectionPolicy`, `RedactionPolicy`, `BundleMetadata`, `DiagnosticsBundle` (`src-tauri/src/commands/diagnostics.rs:338-400,712-734`), and the three commands `rightkit_logs_write/collect/clear` (`:311-336`) that `@rightkit/logs` `createLogsClient` calls (`src/lib/logs.ts`).

### 2.2 What RightKit provides

- `RK/crates/rightkit-logs` (source 0.3.10; INDEX.md records 0.3.5 published): `JsonlStore` (size rotation, optional generations, owner-only, cross-process lock), `AsyncWriter`, `RotatingTextWriter`, `capture_stderr`, `DetachedLog`, `collect_diagnostics`, `RedactionPolicy`, `scrub_text`/`scrub_value`, `KnownSecrets`, `ConsentStore`, `Outbox`, `Uploader`, `system::{identity, metrics}`, `identity`, and behind features `install_tracing` and `install_stderr_tracing` (`src/lib.rs:36-81`).
- `RK/packages/logs` (0.1.6): `createLogsClient`, `scrubLogText`, `installWindowCrashCapture`, `scrubWindowCrashText`, `createDiagnosticsRecorder` (`src/index.ts`, `src/crash.ts`, `src/recorder.ts`).

### 2.3 Behaviour differences

| # | Live | RightKit / main | Status |
|---|---|---|---|
| L1 | Own appender keeps the file handle open | `JsonlStore::append_value` opens the file per append (`RK store.rs:297-341`) | Verified. Same thread, so no caller-visible change; cost not measured. |
| L2 | Rotate at >= 10 MiB to `.1`, remove old `.1` | Same rule (`RK rotate.rs:74-90`) | Verified. |
| L3 | Rotation failure: append error, sink dropped | Event kept in the current file up to twice the limit, then refused (`RK store.rs:343-356`, README) | Verified from code and README. |
| L4 | Batch cap of 500 written by hand (`commands/diagnostics.rs:313-321`) | `cap_batch(.., DEFAULT_BATCH_CAP)` (`MAIN commands/diagnostics.rs:310`) | Verified. Same number. |
| L5 | Own `system_identity.rs`, `system_metrics.rs` (642 lines) | Re-exported from `rightkit_logs::system` (`MAIN heardright-telemetry/src/lib.rs:6`) | Verified re-export; field-level parity not verified. |
| L6 | Engine stderr tracing set up by hand (`heardright-runtime/src/entry.rs:284-302`) | `rightkit_logs::install_stderr_tracing` (`MAIN heardright-runtime/src/entry.rs:347`) | Verified. |
| L7 | No upload consent store | `ConsentStore`, `Outbox`, `Uploader` exist; HeardRight does not use them | Verified. Nothing lost; upload was retired. |

Nothing the live app does in logging is missing from the package. HeardRight still keeps its own queue (`AsyncWriter` is not adopted) and its own content redaction in `heardright_core`.

### 2.4 What main calls today

Both. Main pins `rightkit-logs =0.3.7` and `@rightkit/logs 0.1.6`.

- RightKit: `JsonlStore::new` + `append_value` (`heardright-telemetry/src/telemetry_writer.rs:270-278`), also in `heardright-text-pipeline/src/l3_cleanup/transport.rs`, `heardright-runtime/src/worker/probe_decisions.rs`, `heardright-command-host/src/command_dispatch/entry.rs`; `scrub_text`/`scrub_value` (`heardright-app-runtime/src/sidecar_log.rs`); `identity::random_uuid` (`heardright-telemetry/src/feedback.rs:17`); the diagnostics collection as in live.
- Own code: the queue and maintenance commands in `telemetry_writer.rs` (304 lines), event shaping and the settings gate in `telemetry.rs` (321 lines).
- JS: `src/lib/logs.ts` is identical in LIVE and MAIN (diff is empty); main adds `installWindowCrashCapture` (`src/main.tsx:33`) and `scrubWindowCrashText` (`src/components/renderCrash.tsx:16`).

### 2.5 Changes after the live build

All look deliberate: appender replaced by `JsonlStore`; path resolution moved onto the writer thread (`AppendEvents`, heardright#335); system identity and metrics moved to the package. No regression found.

One open point: INDEX.md records 0.3.5 as the published `rightkit-logs`, while main pins =0.3.7. Whether 0.3.7 is on crates.io was not verified.

## 3. rightkit-license and @rightkit/license

### 3.1 What the live app does

Inventory: 18 files, 6,084 lines (11 desktop Rust files, about 3.9k lines; 7 iOS Swift files mapped to `RightKitLicense`, not reviewed here). Already RightKit at the live commit: only `rightkit_license::verify_token` and `VerifyError` from =0.1.3 (`src-tauri/src/license/lifecycle.rs:127,161,172`, `license/keychain.rs:198`, `license/lease.rs:457`), and `@rightkit/license 0.1.7` `createLicenseClient` with renderer fetch and a keyring-backed storage adapter (`src/lib/license/core.ts:240-272`).

- **Activation.** New activations take a device lease: `POST /v1/pro/heardright/devices/lease` with key, device id hash, platform (`license/lease.rs:30,517-582,636`). Legacy key activation went through the JS SDK.
- **Lease envelope.** Ed25519 signature over the canonical envelope checked before anything is stored (`license/lease.rs:260-292`), then the nested token is verified (`:309-358`).
- **Refresh.** Lease records renew at `/lease/renew`; legacy records use `/v1/licenses/refresh` (`license/lease.rs:367-376`). A 410 or a 404 on renew revokes locally (`:602`).
- **Lifecycle.** `free`, `migration_required`, `invalid`, `clock_repair_required`, `active_legacy`, `active_trial`, `expired_trial`, `active_lifetime_provisional`, `verification_required`, `active_lifetime` (`license/lifecycle.rs:76-309`). Hard expiry, no grace.
- **Offline behaviour.** Pro holds until the signed token's `exp`. `lease_grace_seconds` and `lease_expires_at_unix` are stored but never read (grep over `src-tauri/src`: only written at `license/lease.rs:348-349`).
- **Clock.** Keychain ledger `app.heardright.next.license-clock` / `highest-seen-unix`, 300 s rollback tolerance, 60 s write interval (`license_clock.rs:1,46`, `license/clock.rs:19-21`). A final lifetime licence survives a rollback at the high-water mark (`license/lifecycle.rs:121-147`).
- **Device binding.** Random id in keychain `app.heardright.next.device` / `device-id`; raw id for legacy records, SHA-256 hash for lease records (`license/device.rs:19-20,113,152`).
- **Storage.** Bare JSON record in keychain `app.heardright.next.license` / `activation` (`license/keychain.rs:32-33`, record shape `license_gate.rs:6-52`).
- **Removal.** Durable queue in keychain `app.heardright.next.license-removals`, retried later (`license/removal.rs:18-21,135-206`).
- **Other.** Windows link codes (`license/lease.rs:690-782`), macOS Pro bridge over iCloud Keychain (`pro_bridge.rs`), account session by e-mailed link, device list, account deletion (`account_session.rs:42-50,227-590`), trial request from the renderer with `{app, email}`, copy says 14 days (`src/lib/license/core.ts:380-391`).
- **Gate.** `has_pro()` verifies on every call; on a read error it falls back to an in-process "verified final lifetime" flag (`license/lifecycle.rs:347-375`).

### 3.2 What RightKit provides

`RK/crates/rightkit-license` (source 0.2.9; INDEX.md records 0.2.5 published; main pins =0.2.8):

- Default build: `verify_token` and variants, `Entitlement`, `LicenseRequirement`, `Lifecycle`, `evaluate_lifecycle`, `LifecyclePolicy`, `AntiRollbackClock`, `ClockStore`, `FileClockStore`, `refresh_and_reanchor`, `verify_lease_envelope`, `NativeEngineEntitlements`, `RevokedIds` (`src/lib.rs`, `src/lifecycle.rs`, `src/clock.rs`, `src/lease.rs`, `src/native_engine.rs`).
- Feature `client`: `LicenseClient` (activate, `lease_activate`, refresh/renew, deactivate, `deactivate_with_pending_release`, `deactivate_with_queue`, `drain_pending_releases`, `request_trial`, `request_trial_for`, `import_legacy`, `bridge_publish/exchange`, `link_request/redeem`), `AccountSessionClient`, `LicenseInvalidation`, engine snapshot (`src/client/ops.rs:763-1422`, `src/client/account.rs`, `src/client/bridge.rs`, `src/client/release_queue.rs`).
- Every HeardRight endpoint is in the generated path table (`src/suite_api_generated.rs:30-48`).
- `RK/packages/license` (source 0.3.1, 0.3.0 published): `createLicenseClient` over native commands (`status`, `activate`, `refresh`, `deactivate`, `requestTrial`, and in 0.3.1 `requestLink`, `redeemLink`, `retryPendingReleases`), `createAccountClient` (0.3.1), fixed message tables. No renderer fetch or storage since 0.3.0.
- Platforms: Windows and macOS through the OS credential store. Linux is disabled in main (`MAIN src-tauri/src/host_services.rs:349-363`).

### 3.3 Behaviour differences

| # | Topic | Live | RightKit | Status |
|---|---|---|---|---|
| K1 | Lifecycle table | `license/lifecycle.rs:226-309` | `RK lifecycle.rs:259-303`; `legacy_kind` maps `Expired` to `invalid` and `Grace` to `active_legacy` (`RK lifecycle.rs:101-108`) | Verified. Same classes. The claim-shape checks (`issued <= iat <= exp`, live `:231-256`) sit behind `has_valid_lifecycle`, which was not read. |
| K2 | Grace after expiry | None | `grace_secs`, default 0; main sets 0 (`MAIN host_services.rs:62-67`) | Verified. Same result. |
| K3 | Lifetime under clock rollback | Survives (`license/lifecycle.rs:121-147`) | `lifetime_survives_rollback` (`RK lifecycle.rs:599`), set true by main | Verified. |
| K4 | Clock constants | 300 s, 60 s | 300 s, 60 s (`RK clock.rs:33,37`) | Verified. |
| K5 | Clock storage | Keychain item | Main keeps the same item through `with_credential_clock` (`MAIN host_services.rs:288-291`) | Verified. |
| K6 | Record storage | Bare JSON | Revisioned envelope in the same item; bare record upgraded in place (`RK client/ops.rs:629-700`, `client/envelope.rs:258-270` reads HeardRight's flat lease fields) | Verified that the code exists; the upgrade was not run. |
| K7 | Device binding | Raw id for legacy, hash for lease | Same rule (`RK client/reader.rs:198-207`) | Verified. |
| K8 | Lease expiry | Stored, never enforced | The status view turns `Expired` once `now >= lease.expires_at + grace_seconds` (`RK client/ops.rs:546-553`). `native_engine.rs` has no lease check (grep). | Verified. So the UI can show expired while the native Pro gate still grants on the token's `exp`. Whether a token ever outlives its lease is a server fact, not verified. |
| K9 | Lease generation | Stored, not compared | An older generation is refused (`RK client/ops.rs:1357-1361`) | Verified. Stricter. |
| K10 | Renew answered 404 | Revokes (`license/lease.rs:602`) | 0.2.8 revokes; 0.2.9 keeps the record and treats it as stale (`RK CHANGELOG.md` 0.2.9, rightkit#1557) | Verified in source and changelog. Main's pin behaves as live does. |
| K11 | Pro check during a renew | No lock; reads the record | Up to 0.2.8 the native read was refused as busy while a network call held the lock; fixed in 0.2.9 (`RK CHANGELOG.md` 0.2.9) | Verified in changelog. Main pins =0.2.8, so the refusal is expected there; not reproduced. |
| K12 | Removal queue | App-owned queue and drain | `PendingReleaseQueue`, `deactivate_with_queue`, `drain_pending_releases` since 0.2.8 (`RK client/release_queue.rs:133-155`) | Verified. Main does not use them yet (see 3.4). |
| K13 | Trial request | Renderer fetch, `{app, email}` | `request_trial(email)`; `request_trial_for(email, platform)` since 0.2.8 (`RK client/ops.rs:1165-1210`) | Verified. |
| K14 | Account session, device list, deletion | App commands | `AccountSessionClient` native; shell commands and `createAccountClient` only in unpublished source (`RK packages/license/CHANGELOG.md` 0.3.1) | Verified. |
| K15 | Link codes, Pro bridge wire | App code | `link_request/redeem`, `bridge_publish/exchange`; shell link commands in source (`RK crates/rightkit-tauri-shell/src/host/license.rs:446-461`) | Verified. The iCloud Keychain helper stays app-side. |
| K16 | Renderer role | JS SDK fetched and held the record through commands | Renderer gets fixed views only; main trusts only the `hub` window (`MAIN host_services.rs:57`) | Verified. |
| K17 | Read-error fallback | In-process final-lifetime flag | Prior final-lifetime report reused only for `NativeSourceError::Unavailable` (`RK native_engine.rs:243-269`) | Verified by grep and README; logic not read line by line. |

### 3.4 What main calls today

Both, with RightKit as the owner. Deleted from the app: `license/{lease,lifecycle,keychain,clock}.rs`, `license_clock.rs`, `license_gate.rs` (per `MAIN docs/rightkit-exceptions/M12.md`). Desktop inventory drops from about 3.9k to about 1.6k lines.

RightKit call sites:

- Owner built once: `src-tauri/src/host_services.rs:263-346` (`LicenseClientConfig::new`, `LicenseClient::new`, `import_legacy`, `AccountSessionClient::new`, `NativeEngineEntitlements::new`).
- Commands: `rightkit_tauri_shell::host::license::rightkit_license_{status,activate,refresh,deactivate,request_trial}` (`src-tauri/src/app_run.rs:1321-1325`).
- Policy overrides in `HeardRightLicenseOps` (`host_services.rs:218-254`): activate is `lease_activate`; deactivate is `deactivate_with_pending_release`.
- Gate: `license::has_pro()` to `NativeEngineEntitlements::current()` (`src-tauri/src/license/entitlement.rs:27-29`, `host_services.rs:133-162`).
- Pro bridge: `src-tauri/src/pro_bridge.rs:299,361-362`. Account: `src-tauri/src/account_session.rs:16`.
- Renderer: `createLicenseClient` (`src/lib/license/core.ts:31,153-218`).

Still app code (M12.md L1 to L6):

- Trial with platform, own HTTP call (`src-tauri/src/license/trial.rs:16-76`), although `request_trial_for` is in the pinned 0.2.8.
- Removal queue storage and drain (`src-tauri/src/license/removal.rs`, 209 lines), although the queue seam is in 0.2.8.
- Account and link commands (`account_session.rs`, `license/link.rs`), because the published shell and JS package lack them.
- Update-check credentials (`license/entitlement.rs`), engine Pro flag push, iCloud bridge helper.

### 3.5 Changes after the live build

Deliberate:

- Licence authority moved out of the renderer into the native client; `@rightkit/license` 0.1.7 to 0.3.0 is a breaking API change the app has absorbed.
- Trial request moved native and now sends the platform; length changed from 14 to 30 days (`MAIN src-tauri/src/license/trial.rs:3`, dated 2026-10-31).
- Hub-only caller policy; no force-Pro switch (M12.md L5).

Regressions, found and their state:

- Bare stored record refused by the native gate, so paid commands failed closed after the client adoption. Fixed in main by the launch-time `import_legacy` with retry (`MAIN host_services.rs:296-316`); RK 0.2.6 also upgrades the record automatically.
- Pro read as Free for about a second after a licence write (rightkit#1320). Main uses the fix, `ProcessAnchor::process` (`MAIN host_services.rs:335`).
- K11, Pro refused during a launch-time renew. Fix is in 0.2.9, which main does not pin. Open.

### 3.6 Gaps to close before HeardRight ships on it

1. Move main to `rightkit-license` 0.2.9 (K10, K11). INDEX.md lists 0.2.5 as published, so 0.2.8 and 0.2.9 publication status needs checking.
2. Decide K8: either the native gate also enforces lease expiry plus grace, or the view stops doing so. Today the two can disagree.
3. Replace `license/trial.rs` with `request_trial_for` and `license/removal.rs` with `deactivate_with_queue` and `drain_pending_releases`, keeping the existing keychain queue as the store.
4. Publish the shell account and link commands and `@rightkit/license` 0.3.1, then drop `account_session.rs` and `license/link.rs`.
5. iOS (`RightKitLicense`, 7 Swift files) was not reviewed.

## Not verified

- Nothing was compiled or run; no installed build was exercised.
- Server behaviour (token `exp` against lease expiry, trial length by platform, whether the telemetry endpoint accepted events).
- crates.io and npm publication state beyond what INDEX.md records.
- `has_valid_lifecycle`, the macOS sample scrub internals, the bare-record upgrade path and `refresh_legacy_inner` against `refresh_and_reanchor` were not read line by line.
- iOS Swift licence code and `RightKitSwift`.
