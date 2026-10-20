# Pulse RightKit implementation packet

Status: plan only; future Pulse integration owner receives this file alone. No API is asserted published by this packet.

## Authority & checkout

- Normative source: `docs/plans/2026-10-20-rightkit-spec-adjudication.md`; K01–K16 semantics & P1–P6 deletion map in `docs/plans/2026-10-20-rightkit-m1-m22-package-spec-astra-r1.md`; row organization in `docs/plans/2026-10-20-rightkit-spec-lock/fable-r1.md`. Adjudication overrides conflicting originals. No design reopening.
- Signed spec identity: commit `5ce6e2c32843edd28b003162f31f50f6cce0d9b5`, SHA-256 `f01330aed8e63a74ee290e31ad1c4c728dd8b3f8414bea4bea17dd4855701a49`.
- RightKit read-time HEAD `5ce6e2c32843edd28b003162f31f50f6cce0d9b5` (signed spec commit). Historical spec inspection used `5a5892a9e4f07b17e985ee93ffe20a54ff78178c`; these are distinct facts. Pulse root `D:\Claude\pulse`, HEAD `08ada52deafc8f895658fbd7ce93825e5a142ee9`; working tree dirty separately. Refresh source before edits.
- Use primary checkout `main`, no branch/worktree. Preserve unrelated changes. Refresh source, resolve conflicts, run `D:\Claude\tools\setup-workspace.py` plus `workspace-doctor`, read overlay/AGENTS & gotchas before changes.
- Required input: `handoff/published-release-manifest.json` in this RightKit packet folder, produced by RK package executor; use schema in `01-packages.md` §6; verify selected immutable snapshot hash, spec identity, published package bytes, each AP row's `requiresCapabilities` satisfaction and derived `apps.pulse.tasks` readiness. PK rollup status is never an AP gate. Missing file blocks dependent adoption only. No path pins, local workaround, or downgrade.
- Exact published dependencies only (Cargo `=x.y.z`, npm exact, Swift exact tag). Reconcile `Cargo.toml`/`Cargo.lock`, `hub/src-tauri/Cargo.toml`, `hub/package.json`/`hub/pnpm-lock.yaml`, `mac/Package.swift`, `mac/Hub/Package.swift`, `swift/RightKitHub/Package.swift`, `swift/SendKit/Package.swift`, `swift/MeshLink/Package.swift`, `rightkit-mesh-ffi/Cargo.toml` only where active caller inventory requires them.
- Future integration owner owns all builds/tests/qualification. Children edit-only. Scratch under `D:\scratch` or `/Volumes/D/scratch`. No release/upload authorized.

## Outcome & non-negotiable behavior

Adopt common owners while retaining Pulse product actions, schema, protocols, UI, & native Swift host. Root migration is one coordinated all-root cutover: resolve one immutable root context, migrate legacy data, route every reader/writer/child, then remove superseded helpers. Preserve P-256/Secure Enclave identity & signing, exact action/request binding, nonce/replay rules, expiry/skew, independent PIN authority, and audit-before-success. Approval is a typed shared workflow, never a boolean. No Tauri rewrite of native Swift host.

## Ordered migration

1. Refresh/setup & enumerate callers. 2. Qualify exact packages & manifest. 3. Pin. 4. Wire actual production callers. 5. Verify parity/failures on Windows, macOS, iOS targets in product. 6. Delete replaced mechanics only alongside working replacement. 7. Search residue, rerun acceptance, record exact installed/native artifact. 8. Complete manifest row: owner, versions, call path, deleted functions, evidence, remainder.

## App acceptance tasks

- [ ] AP-PU-01 roots; depends PK07, PK01. Coordinate AP-PU-04 owned budget DB participant in all-root cutover. Inspect `core/src/store.rs` `migrated_app_root()` (~269), `state_root()` (~128). Search `dirs`, env root overrides, path construction across `core`, `gateway`, `hub`, `windows`, `engine`, `mac`, `swift`, `rightkit-mesh*`. Pass one immutable daemon root through settings, DB, credentials, models, logs, services, children. Delete helper only after all consumers reach new root & caller search is empty.
- [ ] AP-PU-02 files; depends PK01, PK07. Inventory temp writes, rename/fsync, lock helpers across Rust/Swift. Replace only generic publication/coordination; retain Pulse serialization, formats, and product data. Keep wrappers that delegate.
- [ ] AP-PU-03 engine supervision/protocol; depends PK02, PK06, PK07. `engine/src/resident.rs:48–55,697,780` imports `rightkit_daemon::{host_with_options,…}` and `rightkit_job_queue::{CommandSet,Dispatcher,DispatcherHandler}`, defines `method_table`, and constructs `Dispatcher`; no `stdio_supervised_child` or `resident_service` symbol is asserted. Map actual handlers and callers. `hub/src-tauri/src/attach.rs::{Gate,connect,attached,fallback}` coordinates epoch-gated attach, `ClientPool`, and fallback. M3/M4 replace shared lifecycle/protocol mechanics at these actual callers; do not add a new queue. PK03 is not a prerequisite unless separate DB transition usage is confirmed.
- [ ] AP-PU-04 DB/files; depends PK01, PK03, PK07 only for owned budget DB and root context. `core/src/budget.rs::Store::{open,open_at}` opens app-owned `budgets.sqlite3` through `rightkit_db` under optional `budget` feature (`core/Cargo.toml` default includes `budget`). Preserve its budget schema and migrations while adopting qualified lifecycle. `core/src/bridge/deliver_codex.rs::{open_state,read_state,read_state_by_id}` and later read-only query callers inspect external `~/.codex/state_<N>.sqlite`; preserve read-only/no-create/no-journal behavior, do not migrate, encrypt, rotate, or claim ownership. Credentials work stays in AP-PU-06 where device/license callers exist; no unobserved Pulse rotation flow.
- [ ] AP-PU-05 settings & host registration; depends PK04, PK05, PK07, PK09. Inventory setting models, commands, event subscriptions & frontend imports. Use Store/Document and caller-gated host composition. Renderer gets safe projections; secrets use typed write intent/native capture. Retain Pulse values/schema.
- [ ] AP-PU-06 licence/account/device/update auth; depends PK05, PK07, PK13. Search token/device/update/getInstallHeaders callers. Preserve identity and user workflow while moving native storage/verification/transport; no secret renderer bridge.
- [ ] AP-PU-07 approvals; depends PK07, PK15. Inspect `core/src/remote/approval_sig.rs` plus every sign/verify/nonce/PIN caller. Shared typed workflow; Pulse action catalogue stays product data. Test valid/altered/replayed/expired/wrong action, PIN authority, audit failure. No operation succeeds before durable audit. Preserve P-256/Secure Enclave profile.
- [ ] AP-PU-08 windows/overlay/preview/PDF/lock; depends PK08, PK16. Pinpoint native lock callers: `core/src/remote/unlock.rs::verify_pin` (~518), `windows/src/unlock_auth.rs`, `windows/src/remote_alerts.rs` (`Way::Pin`/`Way::Hello`, ~326–334,404), `mac/Notch/Sources/Agent/ChatUnlockPanel.swift` (LAContext, ~30,66,142,386,454), `ios/Sources/Owners/AppLock.swift` (`AppLock`, ~8). PK08.app-lock qualification must include a qualified OS-auth backend plus native parity; `PK03.protected-store` does not prove OS-auth. Require PK03 storage slice only if an actual credential-storage caller consumes it. Replace shared OS-auth mechanics only; retain Pulse PIN authority, action policy & independent remote approval semantics. No HeardRight lock consumer.
- [ ] AP-PU-09 UI/settings primitives; depends PK09. Replace repeated neutral settings/theme/platform chrome only; retain Pulse panels, native UI & domain controls.
- [ ] AP-PU-10 legal/onboarding; depends PK10. Inventory receipt & acceptance flows; preserve exact legal text/version and assent sequence.
- [ ] AP-PU-11 logs/crash; depends PK11. Map sinks, registration & capture. Reuse shell watchdog if applicable; crash adapter owns capture; no duplicate detector.
- [ ] AP-PU-12 HTTP/cookies; depends PK12. Map network callers and cookie use; preserve request/auth semantics; mark N/A only after caller evidence.
- [ ] AP-PU-13 update/release; depends PK13, PK17. Existing root scripts: `pnpm check`, `pnpm journeys`, `pnpm release:check`; inspect `scripts/gate.sh` before selecting cases. Route release ownership through RightKit after qualification; no publication.
- [ ] AP-PU-14 QA/mode; depends PK14, PK17. Inventory selectors and QA scripts. Isolate every root, fail closed, compile QA/dev controls out of release, maintain switch manifest.
- [ ] AP-PU-15 agent surfaces; depends PK15; hosted services additionally depend PK07 services/approvals. Distinguish hosted service from client toolbox. Include `engine/src/devicelink_host.rs::DeviceLinkHost` (~1762): extract generic pairing/session mechanics and register through shared service/approval owners. Shared admission/approval/audit; app method/action catalogue & domain operations remain Pulse data.
- [ ] AP-PU-16 Swift; depends PK16. Active pins include `mac/Hub/Package.swift:23` and `mac/Notch/project.yml:46–49`; refresh source pin (currently 0.8.1, `https://github.com/bogusyogi/rightkit`) to exact canonical published RightKitSwift tag/URL resolved by PK16. Inventory duplicate mechanics and callers in `swift/RightKitHub/Sources/RightKitHub/Settings/HubSettingsStore.swift::{init,get,set}`, `Platform/HubServicesEnvironment.swift`, `Platform/HubPlatformServices.swift`, `mac/Notch/Sources/Agent/EngineClient.swift::ResidentEngineClient::{current,detach,call}`, and updater sources under `mac/Notch/Sources/Updater`, and `ios/Sources/Owners/AppLock.swift`. Replace only generic store/window/engine/update/auth mechanics actually supplied; retain Pulse menus/views/action semantics, Secure Enclave key/signing identity and native host. No Tauri rewrite.
- [ ] AP-PU-17 M23; depends PK17. Inventory all env/argv/settings/frontend switches in `switches.json`, compile prohibited selectors out, retain release receipt.

## Exception-ledger reconciliation

- [ ] AP-PU-18 exception ledger; depends on completed AP migrations whose mechanics appear in ledger; no new package slice. Reconcile every `docs/rightkit-exceptions/*.md` entry against signed spec and AP replacement: retire stale entries in same change that deletes/replaces code; otherwise identify product data or a published owner with bounded sunset. Update/remove each stale ledger entry in same implementation change that retires its code, and record unresolved external scratch references as non-authoritative. In particular reconcile `docs/rightkit-exceptions/M5.md` raw `Command` entries against AP-PU-19 child-process replacement. Cross-app scratch references are not authority. No stale “kept” ruling overrides signed spec.

- [ ] AP-PU-19 child processes; depends PK02. Replace raw local launches at `core/src/worker.rs:261`, `core/src/context_cmd.rs:30`, `core/src/bridge/deliver_codex.rs:1259`, `core/src/main.rs:1574` with qualified `rightkit-child-process` `run`/`run_with_stdin`/`OwnedCommand` forms; preserve owned/detached containment, stdin/stdout behavior, cancellation, arguments and exit/error semantics. Delete only superseded construction/helpers after exact callers are rewired; inspect `docs/rightkit-exceptions/M5.md` entries and close them in AP-PU-18. Acceptance exercises each production operation and child failure class on its native host. Preserve worker tree ownership at `worker.rs`, interactive context behavior at `context_cmd.rs`, detached delivery semantics at `deliver_codex.rs`, and explicit uninstaller launch at `main.rs`; do not funnel all sites through one invented launch mode.

### M5 launch disposition

| Caller | Replacement evidence required | Preserve |
|---|---|---|
| `core/src/worker.rs::Worker` | owned child completion/cancel path | worker lifecycle and result handling |
| `core/src/context_cmd.rs` | explicit command + arguments through typed API | interactive context action and exit result |
| `core/src/bridge/deliver_codex.rs` | detached launch using supported published form | delivery request/payload and detached semantics |
| `core/src/main.rs` uninstaller | explicit `cmd.exe` program/arguments on Windows | product uninstall action and error reporting |

## M1–M23 applicability

Every row is evaluated; N/A needs source/caller and platform evidence in completion record.

| Row | Status | Evidence boundary |
|---|---|---|
| M1 window host | Required | Windows/Tauri hub & Swift host command/overlay callers |
| M2 UI shell | Required | `hub/src/main.tsx` shared legal/onboarding imports; inspect `hub/src/App.tsx` + `mac/Hub/Sources` chrome, retain domain UI |
| M3 engine supervision | Required | `engine/src/resident.rs:48–54` daemon + `Dispatcher`; `hub/src-tauri/src/attach.rs::{Gate,connect,attached,fallback}`; no additional queue |
| M4 window/CLI protocol | Required | `engine/src/resident.rs::method_table` (~697), `Dispatcher::new` (~780), `DispatcherHandler` (~55); `hub/src-tauri/src/attach.rs::{Gate,connect,attached,fallback}` epoch/ClientPool protocol; preserve wire contract |
| M5 child processes | Required | `core/src/worker.rs:261`, `core/src/context_cmd.rs:30`, `core/src/bridge/deliver_codex.rs:1259`, `core/src/main.rs:1574`; AP-PU-19 replaces these raw spawns with published child-process API, preserving ownership/detached semantics and command arguments |
| M6 roots | Required | `core/src/store.rs::state_root`, `default_directory`, hub `bridge_dir`; coordinated cutover |
| M7 services | Required | `hub/src-tauri/src/attach.rs` service attach + `core/src/remote/approval_sig.rs` approval |
| M8 filesystem | Required | `core/src/store.rs::state_root/default_directory`, metadata migration |
| M9 database | Required | `core/src/budget.rs::Store::{open,open_at}` owns `budgets.sqlite3`; `core/src/bridge/deliver_codex.rs::{open_state,read_state,read_state_by_id}` reads foreign Codex DB read-only |
| M10 settings | Required | `core/src/store.rs::hub_document_load/patch`, `hub/src-tauri/src/lib.rs::hub_document_load/patch` |
| M11 secrets | Required | `hub/src-tauri/src/rk.rs::open_device`, `license.rs::License` storage |
| M12 licence/device | Required | `hub/src-tauri/src/license.rs::License`, `rk.rs::device`; no Pro feature gate currently |
| M13 enforcement | Provisional | license.rs documents no gated feature; inventory native paid commands before claiming product enforcement |
| M14 legal | Required | `hub/src/Startup.tsx`, `hub/src/lib/legal.ts`, `hub/src/lib/onboardingSettings.ts` |
| M15 logs | Required | `hub/src-tauri/src/rk.rs::rightkit_logs_*`, handler registrations in `lib.rs` |
| M16 crash | Required | `hub/src-tauri/src/lib.rs` calls `pulse_core::crash::install` |
| M17 HTTP | Provisional | network/update callers exist; map each protocol to owner before adoption |
| M18 updates | Required | `hub/src-tauri/src/commands.rs::apps_updates_cached/apps_updates_refresh/app_update`; `engine/src/apps_windows_updates.rs` (included by `#[path]` for `engine::apps::updates`) |
| M19 release | Required | `package.json` release scripts and native release config |
| M20 QA | Required | `pnpm check`, `pnpm journeys`, `scripts/gate.sh` |
| M21 test mode | Required | isolate all roots, compile selectors out |
| M22 agents | Required | `hub/src-tauri/src/attach.rs` and `core/src/remote/approval_sig.rs`; map hosted/client callers separately |
| M23 switches | Required | enumerate actual env/argv/settings/frontend selectors before inventory |

## Source deletion/retention ledger

| Anchor | Planned action | Caller inventory |
|---|---|---|
| `core/src/store.rs:128` `state_root()`; `:269` `migrated_app_root()` (P1) | Replace root resolution/cutover, then delete only unused helper | `rg state_root|migrated_app_root` across all Pulse roots |
| `core/src/store.rs:166` read/patch storage (P2) | Shared Document/host mechanics; retain 16 MiB policy, BOM/read-only behavior as config | search every read/write/patch command |
| `engine/src/resident.rs:48–55,194,697,780,797` (P3) | Reconcile current `host_with_options` + `Dispatcher`/`DispatcherHandler` adoption; shared M3/M4 lifecycle/protocol owns generic composition | retain Pulse `method_table`, contract and product service identity; search `Dispatcher`, `host_with_options`, handler callsites; do not search for nonexistent `stdio_supervised_child`/`resident_service` |
| `core/src/budget.rs::Store::{open,open_at}` and budget migrations | Keep owned budget DB schema; adopt qualified DB lifecycle as applicable | verify `budget` feature enabled in each consumer; do not conflate with foreign Codex DB |
| `core/src/bridge/deliver_codex.rs::{open_state,read_state,read_state_by_id}` (lines 496–535) | Preserve external Codex DB read-only query adapter | retain busy timeout, no-create/no-journal behavior and read-only SQL; no migration/rotation/encryption |
| `core/src/remote/approval_sig.rs` (P4) | Replace common approval workflow; preserve signed profile | signature, nonce, PIN callers; tests must cover alteration, replay, expiry, wrong action and audit failure |
| `core/src/worker.rs:261`; `core/src/context_cmd.rs:30`; `core/src/bridge/deliver_codex.rs:1259`; `core/src/main.rs:1574` (M5) | Replace local process launch mechanics with exact published `rightkit-child-process` API; delete local spawn construction only after replacement works | retain app arguments, foreground/detached ownership, cancellation and exit handling; search all `Command::new`, `subprocess::spawn` and exception-ledger sites |
| `engine/src/devicelink_host.rs:1762` (P5) | Extract generic pairing/session mechanics only | enumerate device action/message callers |
| `mac/Hub/Package.swift:23` (P6) | Exact Swift dependency pin after published qualification | imports/product dependencies |
| `hub/src-tauri/src/lib.rs::hub_document_load`, `hub_document_patch` + `core/src/store.rs::hub_document_load`, `hub_document_patch` | Move generic persistence/revision/CAS host registration to qualified M10 owners; retain onboarding data and legacy import semantics | search Tauri invoke names and all frontend `hub_document_*` calls |
| `hub/src/Startup.tsx::Startup` + `hub/src/lib/legal.ts::legalStepConfig` | Keep shared `@rightkit/legal-ui`/`@rightkit/onboarding` composition; replace only local receipt creation/state mechanics when native receipt API is qualified | retain `LEGAL_VERSION`, `LEGAL_DOCUMENTS_HASH`, document set, permission feature sequence; trace `legalStepConfig`, `onboardingSettings` |
| `hub/src/lib/onboardingSettings.ts::onboardingSettings` + native hub document commands | Replace generic revision/persistence mechanics through shared adapter; keep migration from web storage and Pulse `onboardingDone` bridge | search imports and `onboardingSettings.patch/load` |
| `hub/src-tauri/src/rk.rs::rightkit_logs_write`, `rightkit_logs_collect`, `rightkit_logs_clear`, `collect_diagnostics`, `clear_diagnostic_logs`; `lib.rs` handler registration | Adopt qualified shared host registration; retain hub log retention and user setting semantics | search `@rightkit/logs` command names, native registration and diagnostics callers |
| `hub/src-tauri/src/lib.rs::pulse_core::crash::install` | Replace crash adapter only after shared install/capture path qualified | confirm panic hook and output directory caller scope |
| `hub/src-tauri/src/commands.rs::apps_updates_cached`, `apps_updates_refresh`, `app_update`; `engine/src/apps_windows_updates.rs` (included by `#[path]` for `engine::apps::updates`) | Move generic updater/auth/scheduling mechanics to shared owner; retain app discovery/update action data | search frontend invoke names and updater config/payload callers |
| `hub/src-tauri/src/license.rs::License::{open_or_unlicensed,stored,status,set_record}` + commands | Native storage/auth adapter only after PK05 publication; retain currently unprovisioned signer state and avoid asserting Pro feature | `lib.rs` handler list, `hub/src/lib/license.ts`, Startup optional license step |
| Product schemas, methods, UI, legacy names, policy | Retain as data/action composition | record owner symbol in final manifest |

No anchor authorizes deleting whole file. Search additional copies for same owner responsibility; add exact function-level units before closure.

## Verification plan

Use declared app scripts only: `pnpm check`, `pnpm journeys`, `pnpm release:check`; inspect gate script for actual command expansion. Use native Rust/Swift targets named by manifests; no invented flags. Add tests if missing: coordinated root switch interruption/reopen and reader coverage; approval signature alteration, replay, expiry, wrong action, separate PIN role, audit failure. Exact Windows/macOS installed artifact & iOS/macOS shipped Swift target required. Verify visible behavior, fail-closed errors, clean production call graph, no devtools/QA payload. Record M-row N/A evidence with searched paths, callers, platforms.

## Task-to-capability prerequisites

## Authoritative capability prerequisites

`requiresCapabilities` values use exact `PKnn.slice` IDs from `01-packages.md` §6; package names in task prose identify owner context only and do not gate an entire PK rollup. A conditional capability is required only when its stated active caller/platform is in scope; its absence removes only that slice, not the whole AP task. Mark a task N/A only with evidence no caller exists for that whole task. Unresolved caller applicability is not ready: resolve inventory before writing `requiresCapabilities`; never encode unresolved applicability as an empty list. Qualify an already-published exact version where it satisfies the same slice. `PK14.fixture-capture` is separately deferred; no AP waits for it. `PK17.activation` enforces policy separately and never establishes owner readiness. An evidence/closure task may require no new package slice while still waiting for app migration tasks it closes.

| Task | `requiresCapabilities` | Conditional capabilities (only on stated caller) |
|---|---|---|
| AP-PU-01 | `PK07.root-cutover`, `PK01.atomic-baseline` | `PK06.daemon-client` only if immutable root context crosses daemon clients; `PK08.app-lock` only if root-coupled lock caller exists; AP-PU-04 carries owned DB lifecycle slice |
| AP-PU-02 | `PK01.atomic-baseline` | `PK01.atomic-publication` for new retry/publish-dir flow; `PK01.write-locks` for adopted generic lock; `PK07.migration` if cutover uses this path |
| AP-PU-03 | `PK02.capture`, `PK06.job-queue`, `PK06.engine-host`, `PK06.daemon-client`, `PK07.services` | `PK02.configured-runner` for `run_command`; `PK02.streaming` for `run_streaming`; `PK02.breakaway` for explicit breakaway builder |
| AP-PU-04 | `PK03.sqlite-baseline` for the owned plain `budgets.sqlite3` store | `PK07.root-cutover` only if the owned budget DB folder is relocated; external Codex DB remains read-only and requires no DB migration slice |
| AP-PU-05 | `PK04.settings-baseline`, `PK04.host-services`, `PK09.primitives` | `PK04.write-security` for patch/write callers; `PK03.protected-store` + `PK03.helper-trust` for active typed secret/helper callers; `PK07.services` or `PK07.approvals` only for corresponding registered host calls |
| AP-PU-06 | `PK05.license-client` | `PK17.release` only for new JS license-client consumer; `PK05.device` / `PK05.account` for active device/account callers; `PK03.protected-store` for credential storage; `PK13.update` for active update-auth callers |
| AP-PU-07 | `PK07.approvals`, `PK15.applicability` | `PK07.services` for hosted approval service; `PK15.agent-tools` only if an agent toolbox is an active client caller |
| AP-PU-08 | `PK08.shell-modules`, `PK08.app-lock`, `PK16.swift-release`, `PK16.pulse-hub` | `PK03.protected-store` only if a real credential-storage caller consumes it; `PK08.surface` only for active setup/main SurfaceSpec switch caller; `PK08.overlay`, `PK08.preview`, `PK08.pdf` only for active corresponding callers |
| AP-PU-09 | `PK09.primitives` | — |
| AP-PU-10 | `PK10.receipts` | `PK10.license-step` if license onboarding is active; `PK10.notices` if shared legal notices are consumed |
| AP-PU-11 | `PK11.redaction` | `PK11.watchdog` for shell detector callers; `PK11.log-config` for app log configuration callers |
| AP-PU-12 | none until a network caller is confirmed | `PK12.redirect`, `PK12.cookie-export`, `PK12.downloader` only for matching active redirect/cookie/download callers; otherwise record evidence-backed N/A |
| AP-PU-13 | `PK13.update` | `PK17.recipes` for named release scripts; `PK17.adoption-evidence` for qualifying the adopted callpath |
| AP-PU-14 | `PK14.mode-roots`, `PK14.harness`, `PK17.scanner` | `PK14.qa-chrome` only for active browser QA target; never `PK14.fixture-capture` or `PK17.activation` |
| AP-PU-15 | `PK15.applicability`; hosted surfaces: `PK07.services`, `PK07.approvals` | `PK15.agent-tools` only for an active client-toolbox caller; hosted actions do not require agent-tools |
| AP-PU-16 | `PK16.swift-release`, `PK16.pulse-hub` | — |
| AP-PU-17 | `PK17.scanner` | `PK17.adoption-evidence` for final switch receipt; no `PK17.activation` |
| AP-PU-18 | none | closure waits only for other migration tasks implicated by ledger (AP-PU-01–17/19), never itself or overall packet signoff; no new package slice |
| AP-PU-19 | `PK02.capture` | — |

## Caller inventory before editing

Searches locate candidate paths; inspect each function and its callers before changing it. Save evidence only under `D:\scratch` when a retained artifact is necessary.

| Concern | Search roots/symbols | Closure evidence |
|---|---|---|
| roots | `core`, `gateway`, `hub`, `engine`, `windows`, `mac`, `swift`, `rightkit-mesh*`; `state_root`, `migrated_app_root`, `dirs`, data env vars | all data/engine roots use one immutable context |
| DB/secrets | `core`, `hub`, `mac`, `swift`; keychain, rotate, rekey, sqlite, database path | every participant & old-key recovery path mapped |
| child/service | all Rust/Swift; `Command`, `spawn`, `kill`, `restart`, socket, readiness | ownership class per process; no unrelated kill |
| approval | `core/src/remote`, `engine`, `gateway`, `hub`, native; signature, nonce, PIN, digest, replay | all signer/verifier/caller paths mapped |
| host boundary | Tauri handler, `invoke`, event/listener registration, Swift bridge dispatch | caller/origin checks and exposed DTOs |
| auth/update | TS, Rust, Swift; token, license, device, update, headers | no secret-bearing renderer path |
| switches | source/config; env reads, process env, CLI args, settings flags/features | complete inventory and release exclusion |

## Platform acceptance

| Host | Evidence target | Pass condition |
|---|---|---|
| Windows hub | installed native host + engine | root cutover preserves user data; approval failure blocks action |
| macOS native hub | Swift package/host | same root/trust outcomes; Secure Enclave identity remains bound |
| macOS Tauri hub, if shipped | installed host/updater | no token in renderer IPC; denied update leaves installation untouched |
| shipped iOS/native target | active Swift integration | no Tauri conversion; protocol/data/auth parity |
| QA build | bound isolated QA root | writes stay under app root; mismatched app/root refused |
| release artifact | exact installed build | QA selectors/test markers/forbidden DevTools absent |

## Completion record

| Field | Required evidence |
|---|---|
| App root + HEAD | exact path and commit read before edits |
| Package manifest | handoff manifest identity, package versions, integrity, RK commit |
| Platform qualification | target, host, build identity, installed path/hash |
| Production call paths | each adopted symbol and caller function/file |
| Deleted mechanics | exact function/file/symbol, not directory-only claim |
| Acceptance | scenario, declared command, result/evidence pointer |
| Residual code | function purpose and evidence, or owner gap |
| N/A rows | M ID, searched paths, no-caller result, platform scope |

## M19 release recipe caller closure

AP-PU-13 release-script callers conditionally require `PK17.recipes` under the capability map. Read `right-release.config.mjs`, `package.json::release:check` & `scripts/release/candidate.mjs::run` (local helper at line 8, invoked by argv CLI). Keep product candidate checks, release identities & declarative package lists. Replace any duplicate generic command execution, staging/signing/notarization/publication mechanics with qualified recipes; a wrapper already delegating to RightKit needs no second implementation or gratuitous deletion. Acceptance: native development build/staging plan preserves required payload, hashes, identifiers & sign order; build remains upload-free, upload remains build-free. Distribution is outside this app packet.
