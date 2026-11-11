# HeardRight vs RightKit: process, runtime, models, media, device

Read-only comparison. Nothing was built or run. Each claim is marked **verified** (read in source), **suspicion** (inferred) or **not verified** (not read).

Trees:

- LIVE = `<scratch>/survey/heardright-live` (0.1.213, commit 12b7d8ce)
- MAIN = `<scratch>/survey/heardright-main` (0.1.231, unreleased)
- RK = `/home/user/rightkit`

App paths are relative to `tauri-app-next/` in LIVE or MAIN unless they start with `RK/`.

**Limit that applies to every section.** RK source is ahead of every pin except `rightkit-device`, and the pinned crate sources are not on this machine. All RK line references are to RK source at head. Where a claim depends on the pinned version it says so.

| Package | MAIN pin (Cargo.lock, crates.io) | RK source |
|---|---|---|
| rightkit-job-queue | 0.5.1 | 0.5.4 |
| rightkit-child-process | 0.5.7 | 0.5.10 |
| rightkit-sidecar | 0.3.0 | 0.3.2 |
| rightkit-daemon | 0.5.1 | 0.5.4 |
| rightkit-models | 0.5.3 | 0.5.9 |
| rightkit-onnx | 0.2.9 | 0.2.10 |
| rightkit-ffmpeg | 0.2.13 | 0.3.1 |
| rightkit-audio | 0.3.1 | 0.3.4 |
| rightkit-device | 0.4.2 | 0.4.2 |

`RK/INDEX.md` lists older "Published" versions for several of these (daemon 0.4.0, models 0.5.1, onnx 0.2.7, ffmpeg 0.2.10, device 0.4.0) while MAIN's `Cargo.lock` resolves the pins from crates.io. One of the two is stale; which is not verified.

## Verdicts

| Package | Verdict | Reason |
|---|---|---|
| `rightkit-job-queue` | Right shape with listed gaps | MAIN's engine supervisor really runs on `EngineSupervisor` and keeps LIVE's timeouts and restart rules. Only the supervisor and launch frame are used; the persistent job store is not, and LIVE never had a persistent queue to reproduce. |
| `rightkit-child-process` | Right shape | MAIN has no raw `Command::new` left in product code (LIVE had 40). Windows job object, no-window and bounded reap match. One open risk: macOS parent-death is now a forked watchdog instead of the engine's own poll. |
| `rightkit-sidecar` | Right shape | Framing is fully on the package. The wire changed from a bare `EngineFrame` line to a hello/request envelope; shell and engine ship together, and a mismatched pair fails closed at hello. |
| `rightkit-daemon` | Right shape with listed gaps | Used only by the speech service, which did not exist in LIVE. MAIN carries 391 lines of peer signature checking that RK source now has, plus one SID query RK still does not export. Four crates declare the dependency and never call it. |
| `rightkit-models` | Right shape with listed gaps | Models LIVE downloaded are found in place and keep working; nothing is re-downloaded on upgrade. Gaps: MAIN duplicates the Ultra spec and the grant call that RK has, Whisper is not on the RK route, and every launch now hashes the whole model. |
| `rightkit-onnx` | Right shape with listed gaps | Runtime binding and the environment are on the package. MAIN still builds sessions itself and carries a verbatim copy of RK's CPU-thread budget code; RK's own session builder differs on CoreML failure handling. |
| `rightkit-ffmpeg` | Right shape with listed gaps | Decode and probe are on the package and the decode arguments match. MAIN now refuses any ffmpeg that is not the RK-catalogued build and has no PATH fallback, so the bundle must ship exactly that build. |
| `rightkit-audio` | Right shape | `capture` is a line-for-line port of LIVE's `heardright_capture`, and MAIN deleted its own copy. MAIN keeps a small route wrapper because RK's route helper cannot read HeardRight's saved-microphone setting. |
| `rightkit-device` | Right shape | LIVE already used the same `DeviceService` (through `rightkit-tauri`). Same keychain service and account, same id format, so the device id is stable. |

## The largest duplicate: RightKit already holds whole copies of three HeardRight crates

Verified by `diff -w` per file. MAIN depends on none of these three; of the `rightkit-dictation-*` family it pins only `rightkit-dictation-core =0.2.3` (`heardright_core/Cargo.toml:29`).

| RK crate (source, never published per `RK/INDEX.md`) | MAIN crate | Sameness |
|---|---|---|
| `RK/crates/rightkit-dictation-models` 0.1.2, 11 files, 2,980 lines | `heardright-model-store/src` | All 11 files exist on both sides; 140 differing lines in total. `retired_model_cleanup.rs` is identical (0/461). MAIN-only: `verify_receipt.rs`. |
| `RK/crates/rightkit-dictation-app-runtime` 0.1.2, 2,489 lines | `heardright-app-runtime/src` (`engine_supervisor*`, `engine_process.rs`, `sidecar_log.rs`) | 177 differing lines; `supervisor.rs` 54/1075. RK-only: `host.rs`. |
| `RK/crates/rightkit-dictation-runtime` 0.1.0, 14,676 lines | `heardright-runtime/src` | 40 common files, 949 differing lines of 14,676. `speech_service/conn.rs` identical (0/653). MAIN-only: `cli.rs`, `ipc/l3_live.rs`, `perf_levers.rs`. |

`RK/crates/rightkit-dictation-inference` (0.1.0, 16,741 lines) also exists; it was not diffed against `heardright-inference` (not verified).

This is the real answer to "no duplicate code" for this area. Every MAIN file named in the per-package lists below also has a twin in one of these crates. Which side is the newer one was not determined; the differences are small in both directions.

## 1. rightkit-job-queue

### 1.1 Live

- No persistent job store. File transcription is one sidecar request (`heardright-runtime/src/ipc/server.rs`, `Request::TranscribeFile`). Model downloads are blocking calls in `heardright-model-store`. No import queue was found (searched `file_queue`, `import_queue`, `FileQueue`, `ImportQueue`).
- Engine supervision is HeardRight's own code, 2,282 lines: `heardright-app-runtime/src/engine_supervisor/{supervisor,lifecycle,threads,protocol,env,paths}.rs`.
  - Ready wait 360 s / 90 s (`supervisor.rs:36,38`), write ack 2 s (`:43`), kill then reap bounded to 3 s (`lifecycle.rs:38`).
  - Circuit breaker: 3 abandonments inside 30 s (`lifecycle.rs:46,51`).
  - Restart only on the next `start_dictation`.
  - Sidecar crash writes a dump (`threads.rs:243`).

### 1.2 RightKit

`RK/crates/rightkit-job-queue` (19,982 lines): `JobStore`, `Runner`, `Dispatcher`, `EngineSupervisor`, `EngineClient`, `LaunchFrame` / `read_launch_frame`, `InlineCancelRegistry`. Features `store`, `protocol`, `contract`, `doctor`, `supervisor`, `service`. Contract version 1, default frame limit 16 MiB, maximum 256 MiB (`src/wire.rs:5-11`).

### 1.3 Differences

| # | Live | Main / RightKit | Status |
|---|---|---|---|
| J1 | Own supervisor | `rightkit_job_queue::EngineSupervisor` configured in `MAIN heardright-app-runtime/src/engine_supervisor/supervisor.rs:410-441` | Verified |
| J2 | Ready 360/90 s; abandon cap 3 in 30 s | Same numbers: hello timeout `:41,43`, cap and cooldown `:65-66`. Added: 3 restarts per 60 s, backoff 0.5 s to 30 s (`:60-63`) | Verified |
| J3 | Restart on next dictation start | `RestartMode::OnDemand`; shell passes `autorestart = false` (`:325-341`) | Verified |
| J4 | `kill()` at once, wait up to 3 s (`LIVE supervisor.rs:768-800`) | `with_shutdown_grace(3 s)`; RK documents the bound as about twice the grace (`RK src/supervisor.rs:1313`) | Verified that the setting differs; the exact RK shutdown sequence was not read. Quit can take up to about 6 s instead of 3 s (suspicion). |
| J5 | Write ack 2 s | Send timeout 5 s (`MAIN supervisor.rs:51`) | Verified |
| J6 | Backend, models dir, language passed as env (`LIVE supervisor.rs:307-311`) | Launch frame on stdin before hello carries backend and model directory (`MAIN supervisor.rs:210-238`, read at `heardright-runtime/src/entry.rs:360`); the engine rejects a directory outside its models roots (`entry.rs:367-390`) | Verified |
| J7 | BYOK keys passed as env vars (`LIVE supervisor.rs:386-392`) | Keys sent over stdio after start (`MAIN supervisor.rs:602-660`); env scrubbed, and RK refuses an unscrubbed command (`require_scrubbed_env`, `:437`) | Verified |
| J8 | Crash dump on unexpected exit | `CrashDumpObserver` (`MAIN supervisor.rs:303-316`) | Verified |
| J9 | No contract check | `require_current_contract_version` + `with_mandatory_contract_check` (`:435-436`) | Verified |

### 1.4 What main calls

Only `supervisor` (app-runtime) and `protocol` plus `read_launch_frame` (runtime). Call sites: `heardright-app-runtime/src/engine_supervisor/supervisor.rs:20`, `wire.rs:13`, `routing.rs:11`; `heardright-runtime/src/ipc/server.rs:39`; `heardright-runtime/src/entry.rs:360`.

The job store is not used anywhere. The speech service keeps its own in-memory queue (`heardright-runtime/src/speech_service/conn.rs:397-421`, `Jobs` with a `VecDeque`); it is lost on engine exit.

### 1.5 Duplicates

- `heardright-app-runtime/src/engine_supervisor/*` duplicates `RK/crates/rightkit-dictation-app-runtime/src/engine_supervisor/*` (see the table above).
- Nothing in MAIN re-implements `EngineSupervisor` itself; LIVE's `lifecycle.rs` and `threads.rs` are gone.

### 1.6 Post-live changes

- Deliberate: launch frame, keys off the environment, contract check, restart budget.
- Possible regression: slower worst-case quit (J4).

### 1.7 Verdict

**Right shape with listed gaps.** The supervisor is a faithful, stricter replacement. The package's main feature (persistent jobs) is unused; if long-form transcription is meant to survive an engine crash, that is new work, not a migration.

## 2. rightkit-child-process and rightkit-sidecar

### 2.1 Live

- Raw `std::process::Command` in 20 product files, 40 call sites (largest: `heardright-command-host/src/command_dispatch/power.rs` 10, `src-tauri/src/commands/updates.rs` 5).
- Windows containment: own job object with kill-on-close (`heardright-app-runtime/src/engine_supervisor/lifecycle.rs:54-98`) and `CREATE_NO_WINDOW` (`supervisor.rs:46,435`).
- macOS parent death: the engine polls `getppid()` every second against `HR_PARENT_PID` and exits (`heardright-runtime/src/entry.rs:241-265`).
- Helpers: `src-tauri/src/helper_process.rs` (397 lines) with its own spawn, poll and kill-on-timeout loop (`:244-290`).
- Wire: one bare `EngineFrame` JSON per line both ways; the engine's first line is an `EngineInfo` event (`heardright-runtime/src/ipc/server.rs:44-62`; shell side `engine_supervisor/threads.rs:250-303`).

### 2.2 RightKit

- `rightkit-child-process` (15,105 lines): `OwnedCommand` / `OwnedChild`, Windows suspended-create into a job object, Unix process group, `kill_on_parent_exit`, `run_command_capture`, `open_url`, `reveal`, restart and health trackers, worker journal.
- `rightkit-sidecar` (2,416 lines): bounded JSON-lines read and write, `wire::Frame` (`Hello`, `Ready`, `Request`, `Response`, `Event`; `RK crates/rightkit-sidecar/src/wire.rs:81-103`).

### 2.3 Differences

| # | Live | Main / RightKit | Status |
|---|---|---|---|
| P1 | 40 raw spawns | 0 raw `Command::new` in MAIN product code; 22 `OwnedCommand` uses, 8 `open_url`, `run`, `run_command_capture`, `reveal`, `open_folder` | Verified by search |
| P2 | Own Windows job object | RK job object (`RK crates/rightkit-child-process/src/windows_job.rs`, `spawn_windows.rs`); `CREATE_NO_WINDOW` at `spawn_windows.rs:466` | Verified that the code exists; flag conditions not read |
| P3 | Engine polls its parent pid (macOS) | `owned.kill_on_parent_exit(true)` (`MAIN supervisor.rs:244`). On macOS RK double-forks the calling process into a watchdog that waits in kqueue, then sends SIGTERM and, 2 s later, SIGKILL to the child's group (`RK crates/rightkit-child-process/src/process_unix.rs:224-290`, doc at `src/owned.rs:502-513`) | Verified by reading. **Suspicion:** this forks the multi-threaded Tauri shell without exec, once per tied child, and leaves an extra process per engine. RK states only async-signal-safe calls run after the fork. Not exercised here. `HR_PARENT_PID` is gone from the engine in MAIN (only `menubar_helper.rs:88` still passes it). |
| P4 | Helper timeout loop in app | `rightkit_child_process::run_command_capture` (`MAIN src-tauri/src/helper_process.rs:240-252`) | Verified |
| P5 | Bare `EngineFrame` lines, first frame `EngineInfo` | RK envelope: optional launch frame in, `hello` out, then `request` / `response` / `event` with `EngineFrame` as the body (`MAIN heardright-runtime/src/ipc/server.rs:9-40`, `engine_supervisor/wire.rs`) | Verified. **Upgrade:** a 0.1.213 engine and a MAIN shell cannot talk; the hello check fails and is reported as a terminal start failure (`InitialFailure::Terminal`, `MAIN supervisor.rs:434`). Both binaries ship in one bundle, so this matters only if an update leaves a stale engine binary in place (suspicion; updater not read). |
| P6 | No frame cap | 256 MiB cap on engine frames (`MAIN supervisor.rs:430`) | Verified |
| P7 | Whisper prepare passes the model dir as an argument (`LIVE heardright-model-store/src/whisper_model.rs:303-306`) | No argument; the engine derives the directory (`MAIN whisper_model.rs:303-309`) | Verified |

### 2.4 What main calls

- child-process: `src-tauri/src/{helper_process,update_relaunch,tray,menubar_helper,windows_wezterm,cloud_sync_cloudkit}.rs`, `src-tauri/src/commands/{diagnostics,system,microphone,catalog,files}.rs`, `src-tauri/src/media_control/mac.rs`, `src-tauri/src/onboarding_calibration/engine.rs`, `heardright-runtime/src/entry.rs`, `heardright-command-host/src/{app_launch,pulse_handoff}.rs`, `heardright-command-host/src/command_dispatch/{keys,power}.rs`, `heardright_platform/src/macos/window_query.rs`, `heardright-model-store/src/whisper_model.rs`, `heardright-inference/src/whisper_win.rs`.
- sidecar: `heardright-runtime/src/ipc/server.rs:31-40`, `heardright-runtime/src/speech_service/conn.rs:11`, `src-tauri/src/pro_bridge.rs:266`.
- Package only; no parallel own spawn code remains.

### 2.5 Duplicates

- None for spawning or framing.
- `src-tauri/src/helper_process.rs:34-175` (bundle path resolution, containment check, codesign probe) is app code with no RK equivalent found; it stays.

### 2.6 Post-live changes

- Deliberate: everything above.
- To watch: P3 (fork-based watchdog on macOS) and P5 (stale engine after update).

### 2.7 Verdict

**Right shape** for both. Adoption is complete and the Windows behaviour is the same mechanism. The macOS parent-death change is the one item that needs a real run on a signed build before release.

## 3. rightkit-daemon

### 3.1 Live

Nothing. There is no `speech_service` directory, no listener and no suite-root code in LIVE. The engine entry is 5 lines (`heardright-engine/src/main.rs`).

### 3.2 RightKit

`RK/crates/rightkit-daemon` (33,755 lines): `Listener` (unix socket in a 0700 directory, or a named pipe limited to the current user's SID), `PeerIdentity`, `Suite::resolve` (per-user suite root), `write_atomic`, client and host conventions. RK source at 0.5.3 and later also has `peer_signature::verify_peer` with `SigningIdentity::{MacTeamId, WindowsCommonName}` (`src/peer_signature.rs:141`, changelog entry for rightkit#1319 under 0.5.3).

### 3.3 Differences

| # | Item | Status |
|---|---|---|
| D1 | The speech service (1,730 lines, `MAIN heardright-runtime/src/speech_service/`) is a post-live feature: other Orthic apps send file transcription to the engine over a local endpoint. LIVE has nothing to compare it to. | Verified |
| D2 | The owner's note says `main.rs` and `heardright-engine/src/main.rs` "keep their own suite-root guard". In this MAIN tree neither file has one: the engine `main.rs` is 12 lines calling `heardright_runtime::entry::run()`, and `src-tauri/src/main.rs:1-60` has no suite code. The note is stale, or the guard moved (the 31 Oct audit lists it as debug-only). | Verified for the two files; where a guard might now live was not searched further |
| D3 | The suite root itself comes from `rightkit_daemon::Suite::resolve()` through `rightkit-models` (`RK crates/rightkit-models/src/suite/paths.rs:105`), default `<data dir>/Orthic Labs/shared` (`:163-187`). No environment variable redirects it in RK source 0.5.7 and later. | Verified in source; pinned 0.5.3 not verified |

### 3.4 What main calls

Only `heardright-runtime`: `speech_service/transport.rs:10` (`Listener`, `Accepted`, `DEFAULT_MAX_PIPE_INSTANCES`), `mod.rs:142`, `peer_trust.rs:18` (`PeerIdentity`), `files.rs:18` (`write_atomic`).

`rightkit-daemon` is also declared in `src-tauri/Cargo.toml:43`, `heardright-engine/Cargo.toml:48`, `heardright-app-runtime/Cargo.toml:11` and `heardright_platform/Cargo.toml:10`. No `.rs` file in those four crates names `rightkit_daemon` (verified by search). The comment calls it "test-only environment switches".

### 3.5 Duplicates

- `heardright-runtime/src/speech_service/peer_trust.rs` (391 lines): team id `6KLGD3LLKF` and Authenticode signer checks. Its header says it exists only until rightkit#1319 ships. RK source has that API from 0.5.3; MAIN pins 0.5.1. Delete after moving the pin.
- `heardright-runtime/src/speech_service/transport.rs:80-110`: current-user SID query. RK's `current_user_sid_string` is still `pub(crate)` (`RK crates/rightkit-daemon/src/peer.rs:635`), so this one stays until RK exports it.
- The whole `speech_service/` directory has a twin in `RK/crates/rightkit-dictation-runtime/src/speech_service/` (41 differing lines of 1,731).

### 3.6 Post-live changes

All deliberate (new feature). No regression against LIVE is possible here.

### 3.7 Verdict

**Right shape with listed gaps.** The listener is the right owner. Gaps: move the pin to pick up `verify_peer` and delete `peer_trust.rs`; RK must export the SID string; remove the four unused declarations or say what they are for.

## 4. rightkit-models

### 4.1 Live

`heardright-model-store/src` (2,971 lines, all own code, HTTP by `ureq`):

- Location: `<app data>/models/<subdir>` only (`model_paths.rs:29-37`).
  - macOS TDT: `parakeet-tdt-v3`; Unified: `parakeet-unified-en-0.6b` (`:49-58`).
  - Windows TDT: `parakeet_tdt_v3_static1500_qint8_20260722`, with two older fallbacks and activation markers (`:12-24`, `:74-92`).
- Download: `resumable_download.rs` (508 lines), `archive_install.rs` (244), `model_download_grant.rs` (170), `model_fetch.rs` (213), `whisper_model.rs` (443).
- Readiness is a file-presence check (`local_asr_ready`, `model_paths.rs:111`); no hashing at launch.

### 4.2 RightKit

`RK/crates/rightkit-models` (15,178 lines): `download`, `http`, `hash`, hardened `archive`, and `suite`:

- Shared store at `<suite root>/models/<id>` with per-app references, leases, migration journal and retired-entry pruning.
- `suite::catalog`: `parakeet_spec()`, `parakeet_ultra_spec()` (`src/suite/catalog.rs:798`, added in 0.5.1 per the changelog), Whisper specs.
- `suite::grant` (`acquire_model_grant`, `try_acquire_grant_pinned`, rate-limit parsing).
- `suite::whisper` (`resolve_whisper_model`, `ensure_whisper_model`; ggml only, CoreML bundle "not installable yet", `src/suite/whisper.rs:1-34`).

### 4.3 Differences and upgrade from 0.1.213

| # | Question | Finding | Status |
|---|---|---|---|
| M1 | Is the app data models folder the same? | Yes: `install_base()` is still `app_data_file("models")` (`MAIN model_paths.rs`, unchanged lines). | Verified. That the app data root itself is unchanged was not verified (`app_paths.rs` moved crates). |
| M2 | macOS: is `models/parakeet-tdt-v3` from LIVE found? | Yes. `macos_tdt_subdir()` asks the shared store for Ultra first; with none, `macos_tdt_subdir_for_root` returns `parakeet-tdt-v3` when it is complete (`MAIN model_paths.rs:141-150`, `:176-186`). The readiness rule for that id is unchanged (`:225`). | Verified |
| M3 | Windows: is `parakeet_tdt_v3_static1500_qint8_20260722` found? | Yes. It is now `WINDOWS_TDT_PREVIOUS_SUBDIR` and first in `RETIRED_TDT_MODELS` (`MAIN model_paths.rs:21`, `:48-52`); `windows_tdt_subdir_for_root` returns the first complete retired model unless Ultra is complete and activated (`:158-173`). | Verified |
| M4 | Is anything re-downloaded on upgrade? | Not automatically. The old model keeps serving. MAIN's current model is a different model, Parakeet Ultra (`ultra_model.rs:26-27`, archives 317 MB Windows / 391 MB macOS, `:42-43`); it is fetched only through the replacement prompt or the Model step (`src-tauri/src/commands/models.rs:139-200`). `ensure_shared_tdt_active` treats a retired copy as usable (`:39-52`). | Verified |
| M5 | Is the LIVE model deleted? | Only after Ultra has been verified and loaded in the same process (`commands/models.rs:126-136`, `retired_model_cleanup.rs:395-405`). The launch-time reclaim thread runs because 0.1.231 is past `LEGACY_COMPAT_THROUGH = "0.1.223"` (`shared.rs:17`, `src-tauri/src/app_run.rs:1128`), but it is still gated on verified Ultra. | Verified |
| M6 | Where does Ultra live? | Shared store, `<data dir>/Orthic Labs/shared/models/<id>` (`RK suite/paths.rs:163-192`), not app data. An activation record `shared-model-activation.json` in app data selects it (`MAIN shared.rs:34`, `:249-262`). | Verified |
| M7 | Launch cost | LIVE: presence check. MAIN: `store::ensure_ready` hashes every pinned file on every launch, 363 MB (Windows) or 433 MB (macOS), under the shell's runtime lock (`verify_receipt.rs:1-13`, `policy()` always `Full` at `:36-38`; call at `shared.rs:551`). | Verified. Applies once Ultra is installed. Likely a visible launch delay (suspicion; not measured). |
| M8 | Download transport | `ureq` replaced by `rightkit_models::download` and `http` (`MAIN resumable_download.rs:5-11`); archive install by `suite::archive` (`archive_install.rs`, 36 lines). | Verified |
| M9 | Whisper | Same directory and marker as LIVE; only the hashing, spawn and HTTP client calls changed (48 differing lines). Not on RK's `suite::whisper`. | Verified |
| M10 | Mac users on the retired Unified backend | The directory name is unchanged (`model_paths.rs:112`); the setting flips to TDT only after Ultra loads (`src-tauri/src/settings.rs:462-469`). | Verified for those lines; the full Unified path was not traced |

### 4.4 What main calls

Both. Package: `rightkit_models::suite` (re-exported as `shared::store`, `shared.rs:7`), `download`, `http`, `hash`, `suite::archive`. Call sites: `heardright-model-store/src/{shared,ultra_model,resumable_download,archive_install,model_fetch}.rs`; `src-tauri/src/commands/models.rs:92,153-200`; `heardright-runtime/src/entry.rs:333-338,377`; `heardright-runtime/src/speech_service/{mod,lease}.rs` (leases).

Own code alongside it: `model_paths.rs` (410), `shared.rs` (601), `ultra_model.rs` (368), `whisper_model.rs` (441), `model_download_grant.rs` (80), `retired_model_cleanup.rs` (461), `ane_cache_prune.rs` (164), `verify_receipt.rs` (120), `model_fetch.rs` (171).

### 4.5 Duplicates

- `ultra_model.rs` (368 lines). Same ids, routes, archive checksums and per-file pins as `RK suite/catalog.rs:395-800` (archive hashes `0ad7f5a2…` and `23610964…` at `RK catalog.rs:576,705`; first file pins `0cfca69c…`, `ed68cea2…` match). The file's own comment says the RK catalog "still describes Parakeet TDT v3 in 0.4.x"; that is out of date, `parakeet_ultra_spec` is in RK from 0.5.1.
  - **Not a drop-in:** RK's spec has `legacy_dirs: &[]` and `retired_legacy_paths: &[]` (`RK catalog.rs:789-790`); HeardRight's lists the app-private Ultra directory and old `asr/…` paths (`MAIN ultra_model.rs:259-298`). Switching loses migration of an Ultra copy placed in app data by 0.1.222 to 0.1.231 builds. LIVE never had Ultra, so released users are not affected.
- `model_download_grant.rs` (80 lines) duplicates `RK suite/grant.rs` (`ModelGrantRequest`, `acquire_model_grant`). RK also parses `Retry-After`, which MAIN's copy does not.
- `resumable_download.rs`, `archive_install.rs`, `model_fetch.rs:167`: already thin wrappers; delete when callers use the RK names.
- `shared.rs:140-174` `strip_foreign_speech_refs`: its comment says RK 0.5.2 already ignores foreign speech refs; RK source also has `store::retire_speech_models` for this.
- `whisper_model.rs`: overlaps `RK suite/whisper.rs` for the ggml file only. RK cannot install the macOS CoreML bundle, so this file cannot be deleted yet.
- The whole crate has a twin in `RK/crates/rightkit-dictation-models` (140 differing lines).

### 4.6 Post-live changes

- Deliberate: new model (Ultra), shared store, keep-old-until-verified, grant-gated archive.
- Looks like a regression: M7, full hash on every launch. `verify_receipt.rs` contains the fix (stat receipt) but it is switched off in every build.

### 4.7 Verdict

**Right shape with listed gaps.** The store reproduces what LIVE did and adds safe replacement; upgrade from 0.1.213 keeps the installed model working in place. Gaps: Ultra spec and grant are duplicated, RK's Ultra spec lacks HeardRight's legacy directories, RK cannot install CoreML Whisper, and the per-launch hash needs a decision.

## 5. rightkit-onnx

The inventory's 24 files (43,900 lines) for this target are the vendored iOS ONNX Runtime headers under `ios-native/Vendor/`. They are a third-party SDK, not HeardRight code. The real code is below.

### 5.1 Live

- `ort =2.0.0-rc.13`, load-dynamic, DirectML on Windows and CoreML on macOS (`heardright-inference/Cargo.toml:37,57`, `heardright-onnx-asr/Cargo.toml:21,28`).
- Runtime library choice by setting `ORT_DYLIB_PATH`: `src-tauri/src/platform_setup.rs:109-149` and `heardright-runtime/src/entry.rs:309-369`. An inherited `ORT_DYLIB_PATH` wins; a missing bundled library only logs a warning.
- Sessions: `heardright-onnx-asr/src/session.rs` (159 lines). DirectML registered with `error_on_failure` (`:140`), CoreML without it (`:146`), optimisation level 1 (`:98`).

### 5.2 RightKit

`RK/crates/rightkit-onnx` (4,507 lines): re-exports `ort` (`=2.0.0-rc.15` in source, `Cargo.toml:68-92`), `ndarray`, `half`; `configure_runtime*`, `RuntimeCandidate`, `clear_inherited_runtime_for_release`, `init_environment*`, `ort_environment`, `GlobalPool`, `cpu_thread_budget`, `count_core_records`, `SessionOptions` / `ExecutionProvider` / `BuiltSession`, `probe_providers`, bundle manifest checks.

### 5.3 Differences

| # | Live | Main / RightKit | Status |
|---|---|---|---|
| O1 | `ort` rc.13 | `ort` through `rightkit_onnx::ort`; rc.15 in RK source | Verified for source; the version inside pinned 0.2.9 not verified. Whether the bundled `onnxruntime` library changed was not checked. |
| O2 | Inherited `ORT_DYLIB_PATH` honoured | Cleared first thing in both processes (`MAIN src-tauri/src/main.rs:22`, `heardright-runtime/src/entry.rs:26`); the engine binds with `force_replace` (`entry.rs:473-479`) | Verified |
| O3 | Missing bundled runtime: warning, continue | Engine refuses to start: "Bundled ONNX runtime is missing; repair the HeardRight installation." (`MAIN entry.rs:483-489`) | Verified |
| O4 | Debug-only dev DLL path candidates | Removed; explicit override only with RK `dev-overrides`, which no HeardRight build enables (`MAIN entry.rs:402-413`) | Verified |
| O5 | CoreML registered without `error_on_failure` | MAIN keeps that (`heardright-onnx-asr/src/session.rs:302`). RK's own `SessionOptions` registers CoreML with `error_on_failure` (`RK src/session.rs:407`) | Verified. If HeardRight moved to RK's builder, a CoreML registration failure would become an error instead of a silent CPU run. |
| O6 | Level 1 on DirectML | MAIN adds the DirectML fusion guard and an optimiser bisect (`session.rs:172-200`, `:316-420`); RK has the same guard (`RK src/session.rs:370-372`) | Verified |

### 5.4 What main calls

Package for runtime binding, environment and the `ort` types: `src-tauri/src/platform_setup.rs:50-60`, `src-tauri/src/main.rs:22`, `heardright-runtime/src/entry.rs:26,402-489`, `heardright-onnx-asr/src/{environment,session,model}.rs`, `heardright-inference/src/vad.rs:391-474`, `heardright_core/src/kws_ffi.rs`, `heardright-android/src/lib.rs`.

Own code for building sessions: `heardright-onnx-asr/src/session.rs` (425 lines) calls `ort::session::Session::builder` directly and does not use RK `SessionOptions`.

### 5.5 Duplicates

- `heardright-onnx-asr/src/environment.rs:16-66`: `cpu_threads`, `physical_cores`, `count_core_records` are the same code as RK `cpu_thread_budget`, `physical_cores`, `count_core_records` (`RK src/environment.rs:180-238`; RK's comment calls it "HeardRight's measured budget"). Delete and call RK.
- `heardright-onnx-asr/src/session.rs:82-310`: overlaps RK `SessionOptions` (device id, DirectML fusion guard, CPU-for-stage). Not deletable until O5 is settled and RK takes the bisect switches or they are dropped.
- Runtime candidate lists are written twice in MAIN (`platform_setup.rs:44-58` and `entry.rs:431-466`); RK has `installed_runtime_candidate` (`RK src/lib.rs:516`).

### 5.6 Post-live changes

- Deliberate: O2, O3, O4 (release builds no longer steerable by environment; fail closed).
- Unknown: O1, an `ort` binding bump under a load-dynamic runtime.

### 5.7 Verdict

**Right shape with listed gaps.** Runtime selection is properly owned by the package and stricter than LIVE. Session construction and the thread budget still exist on both sides.

## 6. rightkit-ffmpeg

### 6.1 Live

`heardright-inference/src/file_transcribe.rs` (185 lines):

- Decode: `ffmpeg -y -hide_banner -loglevel error -i <in> -ac 1 -ar 16000 -c:a pcm_s16le <temp.wav>`, then read the WAV (`:22-57`).
- Duration: AVFoundation probe first, else parse `ffmpeg -i` stderr (`:59-95`).
- Binary: `HR_FFMPEG_PATH`, then `runtime/` beside the executable, then the bundle's `Resources/runtime/`, then bare `ffmpeg` on PATH (`:108-136`). No integrity check.

### 6.2 RightKit

`RK/crates/rightkit-ffmpeg` (9,033 lines at 0.3.1): `ArtifactCatalog`, `resolve_ffmpeg`, `verify_ffmpeg` (hash plus `-version`), `VerifiedFfmpeg` / `DevFfmpeg`, `decode::decode_pcm_s16`, `probe::probe_media_opt_with`, `runner`, `CancellationToken`.

### 6.3 Differences

| # | Live | Main / RightKit | Status |
|---|---|---|---|
| F1 | Temp WAV on disk | PCM read from ffmpeg's stdout, 16 kHz mono s16 (`MAIN file_transcribe.rs:80-100`) | Verified. Sample parity is claimed by `tests/ffmpeg_decode_parity.rs`; that test was not read. |
| F2 | Any ffmpeg runs | The binary must be the RK-catalogued build for this OS and architecture, or the call fails with "audio decoder failed its integrity check… Reinstall HeardRight" (`MAIN file_transcribe.rs:159-218`). The unverified path needs RK `dev-overrides`, off in every HeardRight build. | Verified. **Suspicion:** if the release bundle ships a different ffmpeg build than the catalogue row, file transcription is dead on arrival. The bundle scripts were not read. |
| F3 | PATH fallback | None in release: "Bundled media decoder is missing; repair the HeardRight installation." (`MAIN file_transcribe.rs:220-270`) | Verified |
| F4 | `HR_FFMPEG_PATH` env from the shell | The engine resolves from its own location; the shell passes nothing (`MAIN heardright-model-store/src/model_paths.rs:370-396`) | Verified |
| F5 | No timeout | Still none (`timeout: None`, `:90`); the cancel token is created and never triggered | Verified |
| F6 | Pin | MAIN pins 0.2.13; RK source is 0.3.1. Only source was read. | The 0.2.13 API was not verified. |

### 6.4 What main calls

Package only for decode and probe: `heardright-inference/src/file_transcribe.rs:10-15`. `av_decode.rs` (170 lines, AVFoundation) is unchanged from LIVE and kept for the duration probe and a test harness.

### 6.5 Duplicates

- ffmpeg path resolution exists three times in MAIN: `heardright-inference/src/file_transcribe.rs:230-270` (`resolve_ffmpeg`), `heardright-model-store/src/model_paths.rs:370-396` (`bundled_ffmpeg_path_from`), `heardright-app-runtime/src/engine_supervisor/paths.rs`. RK's `ResolutionRequest { bundled_paths, … }` is the intended owner. Collapse to one list passed to RK.

### 6.6 Post-live changes

- Deliberate: F1, F2, F3 (integrity, no PATH).
- Risk rather than regression: F2 depends on packaging.

### 6.7 Verdict

**Right shape with listed gaps.** Same decode, stricter binary trust. Before release, confirm the bundled ffmpeg hash is the catalogue row, and decide whether to re-pin from 0.2.13.

## 7. rightkit-audio

### 7.1 Live

`heardright_capture/` (own crate, 1,834 lines with tests): cpal 0.18.2 stream, lock-free ring, adaptive mono downmix, rubato 5.0.0 resampler, device listing by stable platform id, metrics and typed stream errors. Files: `src/capture/{session,downmix,staging}.rs`, `src/{device,resample,metrics}.rs`.

### 7.2 RightKit

`RK/crates/rightkit-audio/src/capture/` (1,644 lines). Its module header names HeardRight's `heardright_capture` as the donor. Also `wav`, `playback`, `chain`, `ducking`, `measure`, `mic_align`, and `capture/route.rs` (route signature helpers).

### 7.3 Differences

| # | Finding | Status |
|---|---|---|
| A1 | `session.rs`: identical apart from imports, formatting and a removed debug assertion. | Verified by `diff -w` |
| A2 | `device.rs`, `resample.rs`, `downmix.rs`, `staging.rs`: same logic; only tests removed and visibility widened. | Verified by `diff -w` |
| A3 | `metrics.rs`: one added Android-only branch. | Verified |
| A4 | The diff is against RK source 0.3.4. The changelog entries for 0.3.2 to 0.3.4 touch tests, playback, WAV prefix reads and lint, not capture. | Verified from the changelog; pinned 0.3.1 source not read |
| A5 | "Conditioning": `heardright_core::audio_conditioning` is re-exported from `rightkit-dictation-core`, not from `rightkit-audio`. Not compared here. | Not verified |

### 7.4 What main calls

Package only. `heardright_capture` is gone from MAIN. Call sites: `heardright-runtime/src/worker/worker_api.rs:23`, `worker/capture_recovery.rs:22`, `owner_diagnostics.rs:183` (`wav::write_wav_mono_i16`), `src-tauri/src/onboarding_mic.rs:46-142`, `onboarding_calibration.rs:188-194`, `onboarding_calibration/capture.rs:7`, `commands/devices.rs:26`, `commands/microphone/windows.rs:104-134`, `heardright-inference/src/whisper_win.rs:383`.

### 7.5 Duplicates

- `heardright-runtime/src/worker/capture_recovery.rs:13-147` (`CaptureRouteSignature`, `resolve_capture_route`, `current_capture_route`, `capture_route_changed`, `open_capture_paused`, `capture_open_error`) has the same names and struct as `RK capture/route.rs`.
  - **Not a drop-in:** RK matches a saved microphone by exact platform id (`RK route.rs:29-42`); HeardRight resolves its saved setting (`__default__`, `default`, or `device:{json}`) through `resolve_mic_selection` (`MAIN capture_recovery.rs:25-45`, function in `RK crates/rightkit-dictation-core/src/mic_selection.rs:18`). The wrapper can go once the caller converts the setting to a platform id first.
  - Whether `route.rs` exists in pinned 0.3.1 is not verified.

### 7.6 Post-live changes

None in capture behaviour. The move is a rename of ownership.

### 7.7 Verdict

**Right shape.** This is HeardRight's own capture code under a new name, already fully adopted. One small wrapper remains for a real reason.

## 8. rightkit-device

### 8.1 Live

`src-tauri/src/license/device.rs` (158 lines). Already RightKit: `rightkit_tauri::{DeviceService, SecureStore, StorageOperation}` from `rightkit-tauri =0.1.1` (`src-tauri/Cargo.toml:41`). The task brief's statement that none of these were RightKit in LIVE does not hold for this one.

- Store: `keyring_core::Entry::new("app.heardright.next.device", "device-id")` (`:8-9`, `device_entry`).
- An empty or whitespace-only stored value counts as absent and a new id is created.
- Licence binding: raw id, or its SHA-256 hex when the licence has a lease (`device_id_for_license`).

### 8.2 RightKit

`RK/crates/rightkit-device` 0.4.2 (1,021 lines): `DeviceService::get_or_create` (in-process mutex, optional cross-process lock file, `set_if_absent`, adopt the winner on a race; `src/lib.rs:178-222`), UUID-v4-shaped id from 16 random bytes (`:224-243`), `credentials::CredentialStore` over `rightkit-credentials`, `device_id_hash`, licence vault and authorisation services.

### 8.3 Differences

| # | Live | Main / RightKit | Status |
|---|---|---|---|
| V1 | Service `app.heardright.next.device`, account `device-id` | Same constants (`MAIN src-tauri/src/license/device.rs:17-18`) | Verified |
| V2 | `keyring_core` entry | `rightkit_device::credentials::CredentialStore` over `secret_store::credential_store` with the "KeyringCompat target" (`MAIN device.rs:52-60`, `secret_store.rs:98-105`) | Verified that MAIN says it addresses the same OS item. The target-name mapping inside `rightkit-credentials` was not read by me; the sibling report `hr-review-crash-logs-license.md` records the live keychain items as read in place. |
| V3 | Existing id returned as stored | Existing id returned before any generation (`RK lib.rs:209-211`) | Verified. An id created by 0.1.213 is reused unchanged. |
| V4 | Empty stored value: treated as absent, replaced | Empty or control-character value: error, never replaced (`RK credentials.rs:36-48`) | Verified. Fail-closed edge case only. |
| V5 | Hash for leased licences | Same rule in `DeviceIds::device_id_hash` (`MAIN src-tauri/src/native_license_adapter.rs:43-46`) | Verified |
| V6 | `secure_device_id_get_or_create` Tauri command exposed to the renderer | Removed; the raw id stays native (`MAIN device.rs:83-110`) | Verified |
| V7 | macOS and Windows | Linux returns "not supported" (`MAIN device.rs:24-39`) | Verified; Linux was not a LIVE target |

### 8.4 What main calls

Package only: `src-tauri/src/license/device.rs:13,21,71-73`, `src-tauri/src/native_license_adapter.rs:16-24`.

### 8.5 Duplicates

- `native_license_adapter.rs:43-46` re-implements `rightkit_device::device_id_hash` (`RK lib.rs:590`) in one line. Trivial.

### 8.6 Post-live changes

Deliberate: V6 (renderer no longer sees the id), V4.

### 8.7 Verdict

**Right shape.** Same service, same item names, same id; LIVE was already on the earlier home of this code. The only unread link is the credential target mapping.

## Top gaps, in order

1. RightKit holds near-identical copies of `heardright-model-store`, `heardright-app-runtime` and `heardright-runtime` (`rightkit-dictation-models`, `-app-runtime`, `-runtime`; about 20,000 lines) that MAIN does not depend on. One side has to go.
2. `rightkit-models`: full SHA-256 of the 363 to 433 MB model on every launch (M7).
3. `rightkit-ffmpeg`: the bundled ffmpeg must be the catalogued build or file transcription fails (F2); packaging not verified.
4. `rightkit-child-process`: macOS parent-death now forks the shell into a watchdog (P3); needs a run on a signed build.
5. `rightkit-models`: `ultra_model.rs` and `model_download_grant.rs` duplicate RK; RK's Ultra spec lacks HeardRight's legacy directories.
6. `rightkit-daemon`: `peer_trust.rs` (391 lines) is superseded by RK 0.5.3+ `verify_peer`; the pin is 0.5.1.
7. `rightkit-onnx`: thread-budget code duplicated verbatim; session builder on both sides with a CoreML failure-handling difference (O5).
