# ViewRight — RightKit implementation owner brief

Status: future app-owner execution brief. No app code changed by this document.

## Authority & checkout

- Product root: `D:\Claude\viewright`; inspected HEAD: `b7e91c03642e0ddf89657adefe5a5eb2386f5aec`.
- Normative spec: RightKit commit `5ce6e2c32843edd28b003162f31f50f6cce0d9b5`, SHA-256 `f01330aed8e63a74ee290e31ad1c4c728dd8b3f8414bea4bea17dd4855701a49`.
- This brief follows `docs/plans/2026-10-20-rightkit-spec-adjudication.md`, overriding original spec text. Astra anchors V1–V8: `2026-10-20-rightkit-m1-m22-package-spec-astra-r1.md` lines 370–377; K01/K05/K08/K11/K13/K14/K16 obligations apply as crosswalked below.
- Before future edits, app owner must pull latest `main`, run `py -3.11 D:\Claude\tools\setup-workspace.py` when workspace setup is required, inspect current source & preserve unrelated edits. Use primary checkout on `main`; no branch/worktree. Parent package executor supplies `handoff/published-release-manifest.json` in this RightKit packet folder. It is absent now. Use `01-packages.md` §6 schema; verify selected immutable snapshot hash and spec identity. AP-VR-01 validates manifest identity first, then is re-entered for pins as each AP task obtains its exact `requiresCapabilities` receipts. No task waits for AP-VR-01 pinning to finish globally; each AP pins only packages it consumes. Existing published bytes may qualify through `qualify-existing`; do not republish unchanged releases. Cargo/npm use exact registry versions; Swift, where used, uses repository URL + exact qualified tag. No local source substitution.
- RightKit publishes packages separately. ViewRight consumes exact published packages only. No release/upload is authorized by this brief.
- App integration owner owns commands/checks after all edits. Child contributors are edit-only. Keep run output under `D:\scratch` or `/Volumes/D/scratch`.

## Current evidence

- Cargo pins in `src-tauri/Cargo.toml`: `rightkit-license = 0.2.3`, `rightkit-pdf = 0.3.2`; `objc2-pdf-kit = 0.3.2` is platform integration. JS pins in `package.json` include `@rightkit/license 0.2.1`, `@rightkit/app-update 0.4.0`, `@rightkit/logs 0.1.5`, `@rightkit/tauri 0.2.3`, `@rightkit/tauri-shell 0.1.1`, `@rightkit/pdf-ui 0.2.0`, `@rightkit/qa 0.5.1`, `@rightkit/qa-chrome 0.1.8`, `@rightkit/release 0.4.0`. Read exact current Cargo.lock & pnpm-lock.yaml resolutions before changing pins; pin only manifest-qualified published versions/integrity supplied by release manifest.
- Native command registration is centralized in `src-tauri/src/lib.rs`: preview register/revoke & inspector near lines 466–468; license commands lines 518–519; PDF save line 536. These are production call paths, not mere package imports.
- App-owned data root is in `src-tauri/src/app_paths.rs` (imports `rightkit_daemon::{AppRoot, Suite}` and resolves app storage); `Cargo.toml` pins `rightkit-daemon = 0.3.0`, with test-mode features. This root/config persistence is M6. User document save destinations in `documents.rs` are distinct caller-selected paths, not the app data root.
- Exact root/db symbols: `src-tauri/src/app_paths.rs::{resolve_storage, migrate_legacy, root_parent, data_dir, data_local_dir, cache_dir}`; `resolve_storage` migrates legacy root before any store, and startup `prepare` errors stop opening stores. Comments identify `settings.rs`, `logs.rs`, `pdf_bench.rs`, `webview2_env.rs` transitional root wrappers, with `webview2_env` named as a root-parent consumer. `src-tauri/src/persistence.rs::{Db::options, Db::migrations, Db::migrate_options, Db::recovery_policy, Db::open, Db::open_path, try_db_path}` owns existing metadata DB config/schema/recovery. Retain these app-specific schema/migration rules and `rightkit-db` adoption; database is plain SQLite metadata, not an encrypted credential store.
- Settings entrypoints: `src-tauri/src/commands/settings.rs::{settings_path, ...}`; renderer `src/settings/settingsBackend.ts`, `settings.tsx`, `SettingsPanel.tsx`, `publishedSections.tsx`. Legal/onboarding entrypoints: `src/state/legal.ts`, `src/generated/legal-contract.json`, plus `@rightkit/legal`, `@rightkit/legal-ui`, `@rightkit/onboarding` manifest pins. Update entrypoints: `src/state/updateLicense.ts`, `src/settings/sections/UpdateLicenseBlock.tsx`, `@rightkit/app-update` pin. Logs entrypoint: `src-tauri/src/logs.rs`.
- Persistence is applicable M9: `src-tauri/src/persistence.rs` uses `rightkit_db::rusqlite`, opens app metadata DB and stores recent files, pins, sessions & settings; `src-tauri/Cargo.toml` pins `rightkit-db = 0.5.0`. This is an existing DB consumer, not evidence for encrypted licence DB lifecycle.
- Bounded search evidence for M7/M22: inspected all `src-tauri/src` filenames and searched Rust source plus Cargo/package manifests for `rightkit_job_queue`, `EngineSupervisor`, `CommandSet`, `run_cli`, sidecar framing, `rightkit-daemon::Service`, agent-tools/agent packages. No product agent host/client or app local service surface appeared; `rightkit-daemon` is present for roots/mode. Treat M7/M22 as provisional N/A pending expanded owner inventory, not as conclusions from V-anchor absence.
- `src-tauri/src/documents.rs:171` defines `destination_write_lock`; `:511` defines generic `write_atomic`; PDF save already has separate candidate-byte flow. Keep digest/source fingerprint checks before encoding & immediately before replacement. Replace local keyed lock + atomic publication only after published K01 implementation is qualified. This is one-file save; do not introduce a multi-file transaction.
- `src-tauri/src/commands/license.rs` contains native lifecycle code; `src/state/updateLicense.ts` carries renderer token storage/refresh/header creation (Astra V2 anchor at line 190; `licenseHeaders` is at line 387 and `getInstallHeaders` call is at line 521). Remove raw licence/export/getInstallHeaders renderer bridge after safe native actions/DTO reach every caller.
- `src-tauri/src/lib.rs::report_fatal_startup` (around line 129) is called before Tauri startup and uses bounded `osascript` on macOS, `powershell.exe` on Windows and optional `zenity` on Linux, while preserving fatal text on stderr. This is a concrete M5 child-process consumer and must use a shared bounded/no-visible-console runner while preserving native alert behavior and timeout.
- `src-tauri/src/logs.rs::load_persisted_enabled` (line 32) opens app DB directly with `rightkit_db::rusqlite::{Connection, OpenFlags, OptionalExtension}` to load diagnostics opt-out/default. Route DB open/root ownership through shared qualified root/DB owners, preserve read-only query and fallback default; keep diagnostics opt-out. No encryption is applicable.
- `preview_protocol.rs` is an app-owned content-kind/CSP producer and token flow; `lib.rs` manages PreviewStore and scheme callback. Shared generic preview store/auth/budget/revocation/scheme mechanics belong K08; preserve content-kind/CSP policy, HTML producer & untrusted content boundary.
- `src-tauri/src/mac_pdf_view.rs` and `src-tauri/src/commands/pdf_adapter.rs` are Rust/macOS PDFKit and native adapter paths. Shared PDF view/find/print/window mechanics are K08; app supplies PDF request/presentation input. Keep no platform-only residue; any retained function needs a specific published-owner gap and caller evidence.
- `html_devtools.rs` and `ContentWebview` creation in `lib.rs` implement the one named permanent app exception: iframe-only content inspector. Never expose app-shell DevTools or privileged host commands to content.
- Scripts provide focused test entrypoints: `package.json` includes unit test script(s), `test:license-lifecycle`, `test:e2e`; `scripts/test-edge-cases.json` names native PDF bridge, save lifecycle, preview protocol & licensing cases. Inspect exact current scripts/lock before invocation. No tests run for this brief.

## Dependency map

`PK01` fs; `PK02` child-process; `PK03` DB/credentials lifecycle; `PK04` settings/host services; `PK05` native licensing/account/device/updateauth; `PK06` engine/protocol; `PK07` root/service/approval; `PK08` window/overlay/PDF/preview/app lock; `PK09` UI/settings; `PK10` legal/onboard; `PK11` logging/crash via shell detector; `PK12` HTTP/cookie; `PK13` updater/release; `PK14` QA/mode; `PK15` agent client/host; `PK16` Swift parity; `PK17` M23/enforcement. “Gate” means wait for publication/readiness evidence before consuming affected owner.

## Applicability: M1–M23

| Row | Applies | Evidence/decision |
|---|---|---|
| M1 Window host | Yes | Tauri host + macOS PDFKit; V5, lib.rs registrations. PK08/16. |
| M2 App frame | Yes | React app; trusted chrome/UI. PK09. |
| M3 Engine supervision | N/A, provisional | Bounded search of app Rust, Cargo.toml & package.json found no `rightkit-job-queue`, `EngineSupervisor`, sidecar or engine supervisor caller; Tauri `invoke` alone is not this row. Reconfirm full source/build inventory before closure. |
| M4 Window/CLI protocol | N/A, provisional | Generic Tauri command `invoke` and PDF resource protocol are not job-queue/sidecar framing. Bounded source/manifest search found no `CommandSet`, `serve`/`run_cli`, CLI frame parser or engine IPC caller; recheck generated/build surfaces. |
| M5 Other processes | Yes | `src-tauri/src/lib.rs::report_fatal_startup` launches osascript/PowerShell/zenity before Tauri startup; use PK02 bounded/no-visible-console runner, retaining stderr/native alerts and timeout. |
| M6 Data location | Yes | `app_paths.rs` app-managed AppRoot/Suite and `persistence.rs` app DB are distinct from user document destinations in `documents.rs`. PK01/03/07 as applicable. |
| M7 Local services | N/A, provisional | Bounded Rust/Cargo/npm search found no app local service/agent admission implementation; `rightkit-daemon` use is AppRoot/mode. Confirm expanded source/config inventory. |
| M8 File writes | Yes | `documents.rs` lock/atomic save. PK01. |
| M9 Database | Yes | `src-tauri/src/persistence.rs` SQLite metadata DB and `src-tauri/Cargo.toml` `rightkit-db` pin; assess DB lifecycle and settings adoption with package owner. |
| M10 Settings | Yes | App settings UI/config; preserve schema & actions. PK04/09. |
| M11 Secrets | Yes | Native license/update secret lifecycle. PK03/05. |
| M12 Licence/device | Yes | Native license commands & renderer update client. PK05. |
| M13 Pro entitlement | Yes | Native license authorizer and feature gates. PK05. |
| M14 Legal/onboarding | Yes | package.json pins legal/legal-ui/onboarding. PK10. |
| M15 Logging | Yes | `src-tauri/src/logs.rs`; V8. PK11. |
| M16 Crash | Yes, conditional | Reuse shell watchdog; require concrete app capture adapter/callers. PK11. |
| M17 HTTP | Yes | License refresh/update auth and document requests; inspect actual transport use. PK12. |
| M18 Updates | Yes | app-update + update-auth renderer/native paths. PK05/13. |
| M19 Release | Yes | Existing release config/scripts. PK13/17. |
| M20 QA | Yes | package QA dependencies & test inventory. PK14. |
| M21 Test mode | Yes | Apply only if app launches QA mode/test roots; trace feature/artifact reachability. PK14/17. |
| M22 Agent surface | N/A, provisional | Bounded search of app Rust/Cargo/npm manifests found no agent client/host/package or method handlers. Confirm generated/build surfaces and product inventory. |
| M23 Release switches | Yes | Every shipped app’s source/feature/payload scan is mandatory. PK17. |

## Ordered owner tasks

PK labels in task prose name shared owner/API context; only exact `PKnn.slice` entries in the capability map gate AP readiness. Manifest `requiresCapabilities` is authoritative. AP prerequisites are task ordering only, separate from capability readiness; package receipts do not gate AP-VR-01 preflight.

Each task stays open until exact published receipt covers named behavior, all production callers are migrated, deletion is complete, & acceptance evidence is captured. A task may retain app configuration/actions listed explicitly.

- [ ] **AP-VR-01 — qualify dependency release set.** Depends: package executor outputs `handoff/published-release-manifest.json`. Owner: app integration owner. Read `package.json`, `pnpm-lock.yaml`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`, native feature declarations. Validate selected snapshot/hash and spec identity before edits. Pin only exact packages consumed by an AP task whose required capability receipts are satisfied, then re-enter AP-VR-01 on later manifest events for newly ready adopters. Do not guess versions, use sibling paths or source substitutions; preserve unrelated dependency changes. Acceptance: each consumed lock entry matches published bytes/integrity & native feature targets; remaining tasks stay gated on their own capabilities; AP-VR-01 closes after the final applicable adopter is pinned/read back.
- [ ] **AP-VR-13 — app root & metadata DB context.** Depends: AP-VR-01; capability gates are listed in the authoritative map (`PK03.sqlite-baseline` for plain DB lifecycle), with `PK07.root-cutover` only if relocation applies. Owner: `src-tauri/src/app_paths.rs::{resolve_storage, migrate_legacy, root_parent, data_dir, data_local_dir, cache_dir}`, `src-tauri/src/persistence.rs::{Db::options, Db::migrations, Db::migrate_options, Db::recovery_policy, Db::open, Db::open_path, try_db_path}`, `src-tauri/src/logs.rs::load_persisted_enabled`, and callers including `src-tauri/src/commands/settings.rs`, `src-tauri/src/pdf_bench.rs`, `src-tauri/src/webview2_env.rs`. Move transitional wrapper/root-selection/migration and DB-open ownership only to qualified published owners; retain ViewRight app ID, root layout/configuration, DB schema, five migrations, `_migrations` version table, quick-check/quarantine policy, `recent_files`, pins, sessions, settings rows, diagnostics opt-out/default and `load_persisted_enabled` read-only SQL. Do not add encryption. Acceptance: isolated QA roots never touch user root; legacy migration copies/verifies before DB opens; migration failure blocks startup without fresh-root fallback; existing metadata rows/migrations survive; unsupported newer schema/transient busy/permission behavior stays precise; diagnostics opt-out and default survive; if relocation/root transition exists, contention returns Busy before effects and keeps old/new root identities through recovery.
- [ ] **AP-VR-02 — native licensing lifecycle.** Depends: AP-VR-01 + PK05 native qualification. Owner: `src-tauri/src/commands/license.rs`, callers/registration in `src-tauri/src/lib.rs`. Replace duplicated clock, credential storage, refresh/lifecycle orchestration with published `rightkit-license` native client actions. Keep app feature/update requirements, configured endpoints/profile & domain feature checks. Acceptance: activate/refresh/deactivate/import, unavailable credential store, malformed/expired/revoked token, rollback clock, cleanup interruption; tokens never leave native host.
- [ ] **AP-VR-03 — renderer license surface.** Depends: AP-VR-02, PK05 safe DTO/action API & PK13 UpdateAuth. Owner: `src/state/updateLicense.ts` plus all imports/callers and native command registration. Remove renderer token storage, refresh orchestration, raw secret DTO, `getInstallHeaders`/header export and direct auth-header construction. Replace with published fixed allowlist status/actions & native update authorization. Retain display fields and user-triggered action UX. Acceptance: renderer cannot read/export token or arbitrary headers; update succeeds through native provider; denied/unconfigured/busy statuses are non-authoritative.
- [ ] **AP-VR-04 — save locking/publication.** Depends: AP-VR-01 + PK01 qualified keyed locks & AtomicWriter/`commit_checked`. Owner: `src-tauri/src/documents.rs` `destination_write_lock`, `write_atomic`, save callers incl. PDF save in `src-tauri/src/commands/pdf_adapter.rs`. Remove only duplicate lock-map and raw atomic-write mechanics. Keep format encoders, candidate bytes, source digest checks, conflict behavior and product destination selection. Use one-file keyed lock and checked atomic commit; no FileTransaction. Acceptance: stale source before encoding, change immediately before replacement, concurrent same destination, Windows contention/ACL failure preserve prior bytes and surface conflict.
- [ ] **AP-VR-05 — PDF command host.** Depends: AP-VR-01 + PK08 published command registration/auth and current PDF package parity. Owner: `src-tauri/src/commands/pdf_adapter.rs`, command table in `src-tauri/src/lib.rs`, all TS invoke sites. Delete duplicate shared command-registration/auth/transport mechanics only when every production invoke uses published owner; keep ViewRight PDF request construction, serializer and product behavior. Acceptance: each command caller reaches typed registered action; hostile/untrusted content cannot invoke privileged actions; malformed/oversized input refused.
- [ ] **AP-VR-06 — PDFKit/macOS host.** Depends: AP-VR-05 + PK08 native PDF/window qualification; PK16 only if an actual Swift consumer is found. Owner: `src-tauri/src/mac_pdf_view.rs`, `objc2-pdf-kit` pin in `src-tauri/Cargo.toml`, view lifecycle/find/print callers. Replace generic view/find/print/window/lifetime mechanics with qualified shared Rust owner. App does not need conversion to Swift for M1/K08. Retain a local function only when a concrete published-owner gap is evidenced by source/API comparison and named native caller; platform specificity alone does not justify residue. Acceptance: native open/close/reopen, find, print, selection, save, teardown; Windows build has no mac-only reachability.
- [ ] **AP-VR-07 — preview protocol.** Depends: AP-VR-01 + PK08 PreviewStore/auth/scheme/budget/revocation qualification. Owner: `src-tauri/src/preview_protocol.rs`, `src-tauri/src/lib.rs` protocol callback/state setup, renderer register/revoke imports. Remove duplicate generic token store/auth/scheme/budget mechanics. Retain content-kind, CSP, sanitized HTML production & explicit untrusted-content policy. Acceptance: exact-token access, revocation cannot revive, per-kind CSP, method/path rejection, registration bounds, malicious iframe cannot access shell commands.
- [ ] **AP-VR-08 — preserve inspector exception.** Depends: AP-VR-07 caller-policy API. Owner: `src-tauri/src/html_devtools.rs`, `src-tauri/src/context_menu.rs::install_view_source_context_item` (314-line WebView2 COM plumbing), `src-tauri/src/lib.rs` ContentWebview configuration/registration, inspector callers. Move generic COM context-menu mechanics to qualified PK08 `webview2_menu`; retain only ViewRight's iframe-scoped Inspect element interaction, editable preview View source action/source jump and policy. Do not retain generic COM plumbing absent a specific published-owner gap with caller proof. Never enable app-wide DevTools or grant content frame command capabilities. Acceptance: inspect/source jump stays scoped to eligible iframe; shell window has no inspector; content frame calls to license/filesystem/PDF/native commands are denied on each platform.
- [ ] **AP-VR-09 — logs & crash adapter.** Depends: AP-VR-01 + PK11 and AP-VR-13 root/DB integration. Owner: `src-tauri/src/logs.rs::{load_persisted_enabled,...}`, `src-tauri/src/lib.rs` command registration, shell watchdog integration. Replace duplicate recording, collection, rotation/commands and detector loop; use existing shared shell detector once. Route `load_persisted_enabled` through shared app-root/DB ownership while preserving its read-only `rightkit_db` query, opt-out/default semantics and app event data. Keep diagnostic fields and minimal capture adapter. Acceptance: redaction, retention, bounded collection, shutdown and stalled UI capture; no second watchdog loop; no accidental opt-out reset or diagnostics enablement.
- [ ] **AP-VR-10 — trusted UI/settings & legal/onboard.** Depends: AP-VR-01 + PK04/09/10 qualified. Owner: `src/app/useGlobalShortcuts.ts::useGlobalShortcuts` and its caller `src/app/useAppCommands.ts::useGlobalShortcuts`, plus settings/legal/onboarding imports named above. Replace local global keydown dispatcher with `@rightkit/react-shell` `createShortcutRegistry` / PK09 shortcut owner; retain ViewRight shortcut mapping, product callbacks and context checks as configuration/actions. This is not a reason to delete domain actions or HTML reload/inspector policy. Replace duplicated settings primitives and legal/onboarding step mechanics only where current imports show adoption gaps; keep schema, defaults, legal copy and navigation choices. Acceptance: each existing shortcut fires once with same typing-target guards, active-document context and preventDefault behavior; registration/unregistration follows component lifecycle; current sections/actions, conflict, receipt and onboarding remain.
- [ ] **AP-VR-11 — process/HTTP/release/QA rows.** Depends: AP-VR-01 + applicable PK02/12/13/14/17 receipts. Owner: `src-tauri/src/lib.rs::report_fatal_startup` plus current transport/release/QA callsites. Replace direct shell process launches with PK02 bounded runner using no visible console where supported. Preserve pre-Tauri fatal stderr and native alert path for osascript/PowerShell/zenity, including two-minute bound; preserve alert text/exit behavior. M3/M4 remain N/A unless later inventory finds a real engine/sidecar caller. Acceptance: launch each supported host alert, timeout/hung child, missing zenity, stderr remains actionable, no console flash, HTTP refusal/redirect/body errors, QA isolation and release payload scan.
- [ ] **AP-VR-14 — exception-ledger reconciliation.** Owner: app integration owner, after AP-VR-02..11 and AP-VR-13 deletions land. Reconcile every existing `docs/rightkit-exceptions/M*.md` entry (20 files: M1–M15, M17–M20, M22) and matching `.rightkit/ownership-exceptions.json` record against source. For each entry record `retired by <AP/PK receipt>` when code is deleted, `product data/action` when it is not shared mechanics, or `retained with exact evidence + sunset/review condition`; remove stale “kept” claims and obsolete registry entries in same change as deletion. Specifically map `docs/rightkit-exceptions/M8.md::B1` `destination_write_lock` to AP-VR-04 / `PK01.write-locks`; preserve only product conflict check and caller behavior. Do not treat a ledger entry as authority to retain generic mechanics. Acceptance: every ledger/registry row has disposition, source/caller anchor, exact package receipt for retired mechanics or bounded evidence/sunset for retained code; no stale exception statement remains.
- [ ] **AP-VR-12 — final parity & residue.** Depends: all contributing tasks, including AP-VR-14. Owner: app integration owner. Expand residue searches from removed functions/imports across Rust, TS/TSX, Swift, build scripts, generated registries, Tauri command lists, lockfiles and release artifacts. Each deletion needs matching published owner receipt + caller parity evidence; retain justified app functions with named unique behavior. Run focused app checks only after edits complete on native Windows/macOS targets. Record exact commands/results; do not infer from compile alone.

## Authoritative AP capability prerequisites

Use only stable slices from `01-packages.md` §6. Capabilities are not full-PK gates. A conditional capability may be omitted only when source/configuration evidence shows that specific caller is absent; this does not make the AP task N/A when other work applies. An unresolved scan is `unknown`, not N/A. Task-level N/A is allowed only when no AP work applies. The prior AP column is task ordering, separate from capability readiness; `AP-VR-01` means successful manifest identity preflight only, while its pinning subtask is re-entered incrementally and closes after adopters. Every other applicable AP must complete before AP-VR-12 closes.

| AP task | `requiresCapabilities` | Conditional caller/platform disposition | Prior AP |
|---|---|---|---|
| AP-VR-01 | `[]` | Manifest identity and snapshot validation only; do not gate on owner receipts. | package executor creates manifest |
| AP-VR-13 | `PK01.atomic-baseline`, `PK03.sqlite-baseline`, `PK07.migration`, `PK14.mode-roots` | Add `PK07.root-cutover` only if actual app-root relocation/cutover is in scope. Plain SQLite metadata does not imply `PK03.encrypted-lifecycle` or credential `PK03.protected-store`. | AP-VR-01 |
| AP-VR-02 | `PK04.host-services`, `PK05.license-client`, `PK05.device`, `PK03.protected-store` | Protected-store slice applies to actual credential persistence; qualify shipped native host(s). | AP-VR-01 |
| AP-VR-03 | `PK04.host-services`, `PK05.license-client`, `PK13.update`, `PK17.release` | Native UpdateAuth required for actual renderer call path in `src/state/updateLicense.ts` and callers. For `@rightkit/license` 0.3.0 `./testing` adoption, PK17.release is required and exact version comes from its receipt. | AP-VR-02 |
| AP-VR-04 | `PK01.atomic-baseline`, `PK01.write-locks` | One-file save uses keyed lock + checked atomic commit; no `PK01.transactions` or multifile publication. | AP-VR-01 |
| AP-VR-05 | `PK08.shell-modules`, `PK08.pdf` | Native-target qualification for each registered PDF command caller. | AP-VR-01 |
| AP-VR-06 | `PK08.pdf` | Rust/macOS PDFKit consumer is `src-tauri/src/mac_pdf_view.rs`; qualify native Rust/PDFKit target. No `PK16` absent actual Swift consumer; no app conversion to Swift. | AP-VR-05 |
| AP-VR-07 | `PK08.preview` | Receipts cover actual Tauri preview scheme, auth, budget and revocation caller. | AP-VR-01 |
| AP-VR-08 | `PK08.shell-modules` | Generic WebView2 COM menu moves to `webview2_menu`; EX1 is only iframe inspector + View Source/source-jump policy. Add `PK08.surface` only if source/API comparison identifies a consumed SurfaceSpec caller. No app DevTools. | AP-VR-07 |
| AP-VR-09 | `PK11.redaction`, `PK11.log-config`, `PK11.watchdog`, `PK07.migration` | `PK07.migration` covers migrated root/DB open path; preserve `logs.rs::load_persisted_enabled` read-only SQL/opt-out/default. Capture adapter conditional on actual capture caller. | AP-VR-13 |
| AP-VR-10 | `PK04.host-services`, `PK09.primitives`, `PK10.receipts` | `PK10.license-step` only for a source-confirmed license onboarding step; `PK10.notices` only for actual release notice generation. Settings/legal/onboarding sources are named above. | AP-VR-01 |
| AP-VR-11 | `PK02.configured-runner`, `PK12.redirect`, `PK14.mode-roots`, `PK14.harness`, `PK14.qa-chrome`, `PK17.scanner`, `PK17.recipes` | `src-tauri/src/lib.rs::report_fatal_startup` configures Windows `powershell.exe` with `.env("VIEWRIGHT_FATAL_MESSAGE", message)`; published `run` lacks env configuration, so `run_command(OwnedCommand, …, RunOptions)` requires `PK02.configured-runner`. `PK12.redirect` only for confirmed redirect caller; `PK17.recipes` only for exact release recipe source. M3/M4 provisional N/A from bounded source/manifest scan; add PK06 only if engine/sidecar caller is found. | AP-VR-01 |
| AP-VR-14 | `PK17.adoption-evidence` | Reconcile all existing exception-ledger records after contributing deletions; this does not create an exemption for generic mechanics. | AP-VR-02..11, AP-VR-13 |
| AP-VR-12 | `PK17.adoption-evidence`, `PK17.handoff` | Close only after every other applicable AP, including AP-VR-14, is complete with source-backed N/A/unknown dispositions & residual evidence. No `PK17.activation` gate. | all other applicable AP-VR tasks |

## Final acceptance record

One row per retired function or cohesive retained adapter: `function/path | old callers | published owner + exact version/integrity | features | production call path after change | deleted functions | test/receipt evidence | exception rationale`.

Required user-visible scenarios: one-file saves preserve old content on conflict/error; native licensing & updates fail closed without secret export; PDF view/find/print/save work on native targets; preview content stays untrusted; iframe inspector works only in its frame; logs/capture remain bounded. QA/release checks prove no test/dev-control payload in production except the named inspector capability.

No product release/upload follows automatically. Completion requires app owner to read back exact pins, production callers, deletion search results, native evidence & readiness state against the published manifest.

## Verification targets for future owner

These are currently declared app commands/targets, not executed by this planning task. App owner selects focused subset after implementation, then required aggregate gates. Commands must run only after all contributing edits are complete. Rust commands use configured RustRight broker on this host; native-host actions remain brokered through authorized workspace workflow.

| Scope | Declared command/target | Evidence expected |
|---|---|---|
| Renderer/types | `pnpm exec tsc --noEmit` | safe DTO callsites typecheck; no raw header/credential API imports. |
| JS lint | `pnpm lint` | changed renderer imports & adapters comply. |
| Renderer tests | `pnpm test:edge` | unit project exercises caller state and denied/error paths. |
| Browser journey | `pnpm test:e2e` | observable preview/PDF/user workflow where browser suite supports it. |
| Native Rust | `rustright cargo test --manifest-path src-tauri/Cargo.toml --workspace` | save conflict, command ACL, preview token and native lifecycle behavior. |
| Native formatting | `rustright cargo fmt --manifest-path src-tauri/Cargo.toml --all --check` | changed Rust formatting. |
| Existing PDF bridge case | `src-tauri/src/commands/pdf_adapter.rs` tests/callers; `scripts/test-edge-cases.json` entries `src/documents/pdf/macPdfNativeBridge.test.ts` | renderer/native handoff and refusal parity. |
| Preview protocol case | `src-tauri/src/preview_protocol.rs::preview_protocol_serves_only_live_exact_tokens_with_kind_csp` | exact token, CSP, revocation and bounds. |
| Save lifecycle cases | `src/documents/pdf/usePdfSavePlan.lifecycle.test.tsx`; native save tests located in current source | conflict, dirty state and failure preservation. |
| License cases | existing `test:license-lifecycle` suite and `src/state/updateLicense.ts` tests | native authorization, no renderer secret reads/exports. |
| Command ACL | `node scripts/check-command-acl.mjs` | commands available only to permitted shell callers. |
| CSP guard | `node scripts/check-csp.mjs` | untrusted content isolation preserved. |
| Regression guards | `node scripts/check-regression-guards.mjs` | no forbidden DevTools or unsafe call path introduced. |
| QA mode | `scripts/test-runtime-config-e2e.test.mjs` | selected mode/root behavior and production separation. |
| Final JS aggregate | `pnpm verify:js` | app-level type/lint/tests/build/guards after completed changes. |
| Final Rust aggregate | `pnpm verify:rust` | workspace fmt/check/test/lint after completed changes; verify nested calls route through RustRight. |
| Native Windows | app owner uses configured Windows dev verification entrypoints | file lock, write ACL, Tauri command policy, PDF workflow. |
| Native macOS | app owner uses configured macOS dev verification entrypoints | PDFKit lifecycle/find/print, permissions and save parity. |

## Deletion receipt rules

For every removed symbol, record direct and indirect production callers before removal. Search both symbol and equivalent behavior after migration; renamed wrappers do not count as deletion.

Search `destination_write_lock`, lock-map identifiers, `write_atomic`, and all imports/callers of generic write helpers. Confirm only product encoders and destination selection remain locally.

Search renderer state for `license`, `licence`, `getInstallHeaders`, header constructors, refresh calls, token field names and raw command strings. Follow API import aliases and generated invoke bindings to their native registration.

Search command tables, `invoke(...)` names and `@tauri-apps/api/core` callsites to prove each privileged command has native identity/policy. Do not trust renderer-supplied labels/origins.

Search `PreviewStore`, token maps, register/revoke calls, scheme handlers, CSP builders and content-window setup. Delete only shared generic mechanics; content-kind policy and app HTML producers remain.

Search PDFKit types, window construction, find/print handlers and lifecycle callbacks separately. Remove duplicated mechanics when K08 covers them; any surviving function needs exact published-owner gap, caller and evidence. Platform-specific code alone is not an exception.

Search `html_devtools`, inspector registration, DevTools flags and browser devtools configuration across runtime code, build configs, package scripts and artifacts. The surviving exception must inspect only iframe content; it cannot enable app shell inspection.

Search `logs.rs`, shell watchdog registrations and stall callback names. Confirm exactly one detector loop and one set of shared log commands remain reachable.

No whole-file delete is acceptable where a file combines domain behavior and shared mechanics. Split ownership at function boundaries or retain the file with a precise residual map.

Do not delete a former path until its replacement callsites compile against the published owner and behavior-specific acceptance passes. Package import or manifest pin is not replacement evidence.

Capture relevant test output and exact native target/OS for every safety-sensitive case. A test name in inventory is a target, not proof that it passed.

If package receipt is missing a target feature or native parity, keep dependent implementation gated and report exact missing capability to RightKit package owner.

## Residue and closeout format

Expand duplicate search by behavior after initial matches: locking, temp file creation, rename/replacement, digest recheck, conflict translation, token persistence, refresh, request headers, preview map/revocation, CSP selection, PDF view lifetime, find/print dispatch, window creation, logging retention and watchdog timing.

For every positive residue, state whether it is configuration, schema/data, product action, adapter or duplicate mechanics. Configuration/data/actions may remain with concrete callers. An adapter may remain only when exact published-owner gap, function and caller evidence prove it necessary; platform specificity alone is insufficient.

For every API adopted, include source/tag and integrity from package executor manifest plus enabled feature list. Verify Cargo and pnpm lock entries resolve to same exact version.

Record command registration path and renderer import path together; an uncalled native registration or unused renderer import does not establish adoption.

At closeout, attach QA receipts by acceptance scenario, not just aggregate green output. Link any reused current receipt and state which behavior stayed unchanged.

Record known platform exclusions explicitly with evidence from target cfg/manifest and call reachability. Do not turn a platform gap into a blanket PDF or CloudKit exemption.

The app owner must inspect the final artifact inventory for forbidden QA/test/dev controls. The iframe inspector is separately classified as the explicit exception and must be content-scoped.

Close AP-VR-12 only when each applicable M row has either a qualified shared owner and caller evidence, or source-backed N/A/retained exception. Unresolved publication is a gate, not completion.

## Platform and feature decision record

| Platform/feature | Required evidence before closing |
|---|---|
| Windows Tauri host | Native command caller policy, atomic save conflict behavior, filesystem permissions and process/preview safety. |
| macOS Tauri host | PDFKit lifecycle/find/print, Keychain-backed licensing, native command policy and save publication. |
| Linux/local unsigned target, if still supported | Manifest target declaration, feature resolution, preview/document behavior and unsupported native capabilities stated precisely. |
| Web renderer tests | Trusted chrome and UI state; cannot establish native permission or keychain behavior. |
| PDF.js renderer | Existing PDF editor/save-plan path and serialization behavior; cannot certify PDFKit host. |
| Native PDFKit | Native open/save/print/find and teardown; cannot certify PDF.js renderer paths. Qualify this Rust consumer against K08 native owner; PK16 applies only if source search finds a Swift caller. |
| Preview iframe | Opaque/untrusted frame behavior, CSP by content kind and exact token revocation. |
| Inspector exception | Only designated content iframe; no general shell DevTools in any production artifact. |
| Release feature set | Feature closure and packed artifact show no QA/test/developer bypass code except inspector capability. |

Acceptance evidence must name both target and mode. A renderer-only test cannot close native K05/K08 behavior.

The shared generic PDF/window owner must be proven in package archive/tag and native qualification before deleting local wrappers.

Retain `objc2-pdf-kit` only if qualified published K08 API requires it and named source callers remain; delete duplicated window/find/print mechanics once covered. A native dependency or platform specificity alone is not a residue justification.

Keep `pdfjs-dist` and app-specific document serializers as product/editor dependencies unless caller evidence shows otherwise; the task removes shared mechanics, not rendering or file-format ownership. Retain an app-local PDFKit bridge only when a published K08 contract gap is evidenced with exact functions/callers; platform specificity alone is not an exception.

Do not replace ViewRight's document-specific source fingerprint with a generic file hash that has different semantics. Reconcile exact conflict contract with K01 receipt.

No K01 API may claim uncontrolled external-writer compare-and-swap if current platform rename cannot guarantee it. Preserve and state existing boundary.

Avoid treating all preview content as trusted because registration originates in native code; content itself remains untrusted and gets no host privileges.

The inspector exception does not extend to other windows, PDF viewers, live URL previews, QA windows or bundled app chrome.

Any change to accepted preview CSP/content policy is a changed requirement; stop and escalate rather than silently broadening policy during migration.

Any renderer action API that returns credential headers or bearer values violates this brief even if source type is branded or obfuscated.

The safe license DTO should expose only fields needed by existing UI: status, feature entitlement display, expiry/display dates and typed action outcomes; validate exact fields against published receipt.

Update request authorization belongs in native UpdateAuthProvider. UI may request an update action but cannot choose arbitrary host, header or credential.

When an owner API lacks a named operation, retain only the smallest adapter and add a package-owned capability request; do not create app-private generic shared-owner facsimiles.

## Completion evidence shape

Attach exact package receipt ID/hash for every package family used by an AP-VR task.

List manifest and lockfile lines showing selected versions and integrity; include Cargo feature graph for native dependencies.

List every `invoke` registration and callsite pair affected by a command migration.

List renderer imports/callers affected by a safe DTO/action migration.

List each removed local function and the qualified owner function that now supplies its behavior.

List each retained local function with a product/platform reason and concrete caller.

List acceptance scenario, target OS, command name, exit status and relevant assertion result.

List generated artifact scan result and any explicitly allowed production exception.

List unresolved package readiness item as a gate with dependent AP-VR task IDs.

Do not mark the task complete from a successful declaration check, import search or build alone.

## M19 release recipe caller closure

AP-VR-11 also depends on PK13/PK17 for applicable release recipes. Trace `right-release.config.mjs` & `package.json` release commands; existing commands already delegate to RightKit. Keep `scripts/check-release-truth.mjs::{requireIncludes,forbidIncludes,requireLfOnly}` product assertions where they check ViewRight facts. Inventory hook bodies for duplicate packaging/signing/staging; delete only replaced mechanics. No new release pipeline or removal of product assertions merely to increase deletion count. Acceptance: native development build/staging plan preserves required payload, hashes, identifiers & sign order; build remains upload-free, upload remains build-free. Distribution is outside this app packet.
