# MailRight — RightKit implementation owner brief

Status: future app-owner execution brief. No app code changed by this document.

## Authority & checkout

- Product root: `D:\Claude\mailright`; inspected HEAD: `4d835646e79f64ddfd52afd1a7d4c378b19dc04b`.
- Normative spec: RightKit commit `5ce6e2c32843edd28b003162f31f50f6cce0d9b5`, SHA-256 `f01330aed8e63a74ee290e31ad1c4c728dd8b3f8414bea4bea17dd4855701a49`.
- Follow `docs/plans/2026-10-20-rightkit-spec-adjudication.md` over originals. Astra anchors M1–M6: `2026-10-20-rightkit-m1-m22-package-spec-astra-r1.md` lines 378–383. K01/K03/K04/K05/K06/K07/K09/K13/K14/K15/K17 apply as crosswalked below.
- Before future edits, owner pulls latest `main`, runs `py -3.11 D:\Claude\tools\setup-workspace.py` when workspace setup is required, inspects current source and preserves unrelated changes. Work in primary checkout on `main`; no branch/worktree. Package executor will produce `handoff/published-release-manifest.json` in this RightKit packet folder; absent now. Use `01-packages.md` §6 schema; verify selected immutable snapshot hash and spec identity. AP-MR-01 validates manifest identity first, then is re-entered for pins as each AP task obtains its exact `requiresCapabilities` receipts. No task waits for AP-MR-01 pinning to finish globally; each AP pins only packages it consumes. Existing published bytes may qualify through `qualify-existing`; do not republish unchanged releases. Cargo/npm use exact registry versions; Swift, where used, uses repository URL + exact qualified tag. No local source substitution.
- Consume exact published packages only. This brief authorizes no app release/upload.
- App integration owner runs commands/checks once all edits are complete; child contributors are edit-only. Keep run output under `D:\scratch` or `/Volumes/D/scratch`.

## Current evidence

- Product HEAD above. `package.json` direct RightKit pins include `@rightkit/app-update 0.4.0`, `@rightkit/license 0.2.1`, `@rightkit/logs 0.1.5`, `@rightkit/legal 0.6.1`, `@rightkit/legal-ui 0.4.0`, `@rightkit/onboarding 0.6.1`, `@rightkit/pdf-ui 0.2.0`, `@rightkit/platform-ui 0.4.0`, `@rightkit/react-shell 0.7.1`, `@rightkit/settings-ui 0.6.1`, `@rightkit/tauri 0.2.3`, `@rightkit/tauri-shell 0.1.1`, `@rightkit/theme 0.3.1`, plus QA/release dev dependencies. Rust pins are in `src-tauri/Cargo.toml`; inspect `Cargo.lock` and `pnpm-lock.yaml` at execution before changes. Pin only qualified versions supplied by manifest.
- `src-tauri/src/db/data_relocation.rs` is Astra M1 target, restore/relocate choreography. `src-tauri/src/db/accounts_labels.rs` contains `SqlSettings`; this is already adopted and must remain the domain SQL backend.
- `src-tauri/src/core_ipc/shell_mode.rs` calls `rightkit_daemon::supervisor::supervise_adopted` (line 104). It owns/adopts a specific child process; shared API accepts `PrivateChild`. This does not prove arbitrary resident daemon lifecycle solved; do not delete domain launch/adoption code on that claim.
- `src-tauri/src/agent/gate.rs` plus `gate_effects.rs`, `gate_forward.rs`, audit modules implement approval decisions. The gate requires durable audit before returning success; pending decisions are distinct state. Keep account/action trust and specific send/forward/delete/attach semantics. Do not collapse to `approved: bool` or send success before decision/audit settlement.
- `src-tauri/src/commands/license.rs` exposes native commands; `src-tauri/src/pro_gate.rs` stores/authorizes entitlement. Remove renderer raw licence record get/set/delete only after safe native actions and fixed DTO are published and all callers migrate. Preserve revocation/domain policy.
- `tauri_run.rs` registers QA under build/mode conditions. Keep compile-gated registration and isolation; test selectors must not leak to release artifacts.
- `package.json` has `verify`, `verify:contract`, `verify:js`, `verify:rust`, `test:frontend`, focused legal tests, QA browser/journey scripts. `verify:rust` includes fmt/check/test/lint; no commands were run for this brief. Use configured RustRight broker for Cargo on this host, and inspect aggregate routing before execution. Native-host actions remain brokered by workspace workflow.
- Exact UI anchors: `src/views/SettingsView.tsx`, `settingsSections.tsx`, `SettingsLicenseSection.tsx` and `Settings*Section.tsx` import `@rightkit/settings-ui`; `src/components/LegalGate.tsx` wraps app in `main.tsx`; `LegalAcceptance.tsx` imports `@rightkit/onboarding`, `@rightkit/legal-ui`, `@rightkit/settings-ui`; `src/lib/legal.ts` supplies product contract. Update/licence UI is `SettingsLicenseSection.tsx` + `src/lib/license.ts`; package pins include `@rightkit/app-update` and `@rightkit/license`. Inspect these named callers as executable AP-MR-06/07 targets, not future discovery-only scope.

## Dependency map

`PK01` fs; `PK02` child-process; `PK03` DB/credentials lifecycle; `PK04` settings/host services; `PK05` native licensing/account/device/updateauth; `PK06` engine/protocol; `PK07` root/service/approval; `PK08` window/overlay/PDF/preview/app lock; `PK09` UI/settings; `PK10` legal/onboard; `PK11` logging/crash via shell detector; `PK12` HTTP/cookie; `PK13` updater/release; `PK14` QA/mode; `PK15` agent client/host; `PK16` Swift parity; `PK17` M23/enforcement. “Gate” means wait for applicable published readiness evidence before consumption.

## Applicability: M1–M23

| Row | Applies | Evidence/decision |
|---|---|---|
| M1 Window host | Yes | Tauri shell/native windows, shell_mode; PK08. |
| M2 App frame | Yes | React chrome; preserve product mail screens. PK09. |
| M3 Engine supervision | Yes | core_ipc shell_mode uses supervise_adopted. PK02/06. |
| M4 Window/CLI protocol | Yes | core IPC and daemon wire. PK06. |
| M5 Other processes | Yes | `src-tauri/src/startup_error.rs::platform::show` launches `osascript` on macOS to show pre-window startup failure; use PK02 configured runner while preserving native-QA suppression and stderr fallback. |
| M6 Data location | Yes | `data_relocation.rs`, DB/root transition. PK01/03/07. |
| M7 Local services | Yes | hosted agent admission/profile. PK07/15. |
| M8 File writes | Yes | relocation & attachments; isolate ordinary writes from DB transaction. PK01. |
| M9 Database | Yes | encrypted DB, restore/relocate, pool & companion lifecycle. PK03. |
| M10 Settings | Yes | `SqlSettings` backend. Preserve existing adoption; PK04. |
| M11 Secrets | Yes | account OAuth & licence credential storage. PK03/05. |
| M12 Licence/device | Yes | `pro_gate.rs`, native license commands. PK05. |
| M13 Pro entitlement | Yes | native entitlement authorizer. PK05. |
| M14 Legal/onboarding | Yes | legal/onboarding package pins and app flows. PK10. |
| M15 Logging | Yes | logs package & native app logging. PK11. |
| M16 Crash | Yes, conditional | shell detector reuse; app capture adapter if present. PK11. |
| M17 HTTP | Yes | Gmail/account/update transport. PK12. |
| M18 Updates | Yes | app-update + native update authorization. PK05/13. |
| M19 Release | Yes | release scripts/config. PK13/17. |
| M20 QA | Yes | QA packages & tauri_run registration. PK14. |
| M21 Test mode | Yes | registration/isolation only where feature/config applies; trace built artifact. PK14/17. |
| M22 Agent surface | Yes | Mail profile, account-scoped approvals and hosted methods. PK07/15. |
| M23 Release switches | Yes | source/feature/payload scan for every production build. PK17. |

## Ordered owner tasks

PK labels in task prose name shared owner/API context; only exact `PKnn.slice` entries in the capability map gate AP readiness. Manifest `requiresCapabilities` is authoritative. AP prerequisites are task ordering only, separate from capability readiness; package receipts do not gate AP-MR-01 preflight.

- [ ] **AP-MR-01 — qualify dependency release set.** Depends: package executor manifest. Owner: app integration owner. Read `package.json`, `pnpm-lock.yaml`, `src-tauri/Cargo.toml`, `Cargo.lock`, Swift/package manifests if present. Validate selected snapshot/hash and spec identity before edits. Pin only exact packages consumed by an AP task whose required capability receipts are satisfied; re-enter AP-MR-01 on later manifest events for newly ready adopters. Do not guess versions or source substitutions; preserve unrelated dependency edits. Acceptance: each consumed lock entry matches published bytes/integrity & native feature targets; missing releases gate only tasks that consume them; AP-MR-01 closes after the final applicable adopter is pinned/read back.
- [ ] **AP-MR-02 — encrypted store transition lifecycle.** Depends: AP-MR-01 + PK03 (`EncryptedStore`/`StoreTransition`) and PK01/07 root/companion transaction readiness. Owner: `src-tauri/src/db/data_relocation.rs`, DB open/restore/relocate callers, root path configuration, companion file descriptors. Replace local transition lock/journal/copy/verification/cutover/recovery choreography with shared full cross-process transition guard spanning all supported openers, active pools/readers/writers, restore/relocate and credential state. Preserve root old/new identity guard across relocation & interruption recovery; do not release after timeout while backend still mutates. Keep account/message tables, schema/migrations, validation callbacks and declarative companion list. Acceptance: two-process contention, active-reader Busy before mutation, kill at every durable phase, partial copy, wrong destination key, old/new root substitution, recovery retains original until new root opens, no silent empty DB.
- [ ] **AP-MR-03 — retain SQL settings adoption.** Depends: AP-MR-01; capability gate is `PK04.settings-baseline` via `qualify-existing`. Owner: `src-tauri/src/db/accounts_labels.rs::SqlSettings`, settings callsites/UI. Keep SQL backend and its table/schema/transaction context. Remove only duplicate generic get/save wrappers, command maps, polling/default merge mechanics proven superseded. Acceptance: settings transaction rollback, external revision/conflict behavior and existing keys/defaults preserved. No second file settings store.
- [ ] **AP-MR-04 — preserve adopted daemon supervision.** Depends: AP-MR-01 + PK06; add PK02 only if source inventory confirms a child-process caller. Owner: `src-tauri/src/core_ipc/shell_mode.rs`, shell startup/shutdown callers and daemon registration. Keep existing `supervise_adopted` for its `PrivateChild`; do not assert arbitrary resident lifecycle is solved. Move repeated shared handshake/presence/stop/restart mechanics only where published owner accepts same owned-child contract. Retain MailRight core executable, method set, protocol profile and domain resource binding. Acceptance: adopt correct child identity, lost-child detection, bounded stop/restart, no accidental killing/replacing unrelated process.
- [ ] **AP-MR-05 — approval/pending/audit workflow.** Depends: AP-MR-01 + PK07/15 shared pending-decision, audit-before-success reporting and settlement contracts. Owner: `src-tauri/src/agent/gate.rs`, `src-tauri/src/agent/gate_effects.rs`, `src-tauri/src/agent/gate_forward.rs`, `src-tauri/src/agent/audit.rs` and its callers, command/service registrations, renderer approval queue. Replace duplicated generic admission, pending-decision storage, audit formatting/redaction and timeout/settlement mechanics only after shared API fits. Keep Mail profile, account-scoped trust, allowed actions, exact resource facts, effect handlers and durable outcome states. Do not change Mail protocol or collapse authorization to a Boolean. Source order matters: `gate.rs::handle` executes `perform_draft`/`perform_forward`/`perform_mutate`/`perform_attachment` at lines 155–166, then writes audit at 182; `gate_effects.rs::perform_mutate` calls Gmail before applying local state and returns a reconciliation note on local failure. Audit-before-success means a successful response requires audit commit; it does not mean every audit is pre-effect or audit failure proves no effect. Acceptance distinguishes refusal before effect, known effect applied with durable settlement, audit failure after effect (outcome unreported/indeterminate to caller; never claim no effect), provider partial application and local pending-sync. Never retry a send blindly after an uncertain result. For pending approval creation, prove persisted pending row before returning its identifier; user decision audit/settlement must match actual resulting effect. Test account mismatch, expiry, denial, timeout, safe retry only when effect is proven absent, send/forward/delete/attach effect outcomes and exactly-once durable audit where required.
- [ ] **AP-MR-06 — native licence & secret actions.** Depends: AP-MR-01 + PK05. Owner: `src-tauri/src/commands/license.rs`, `src-tauri/src/pro_gate.rs`, renderer callers/registrations. Replace raw renderer record get/set/delete and duplicated storage/refresh/lease lifecycle with native typed actions + fixed allowlist DTO. Keep revoked entitlement IDs, MailRight feature policy, account/domain authorization and update requirements. Acceptance: no token/key export to renderer, forged renderer DTO ignored, unavailable key store fails closed, revocation and lease expiry remove authority, user status is display only.
- [ ] **AP-MR-07 — settings/UI/legal/onboarding.** Depends: AP-MR-01 + PK04/09/10. Owner: `src/views/SettingsView.tsx`, `src/views/SettingsAdvancedSection.tsx:60`, `src/views/SettingsAgentsAudit.tsx:23`, `src/components/AttachmentPreviewModal.tsx:127`, `src/views/SettingsLicenseSection.tsx`, `src/components/LegalGate.tsx`, `src/components/LegalAcceptance.tsx`. Replace generic table primitives with qualified PK09 shared table; retain product columns/content/actions, sorting/loading/empty states and accessibility semantics (`aria-label`, `scope`, keyboard/assistive behavior). Reuse existing shared settings/legal/onboarding adapters where current imports show adoption gaps. Preserve SQL settings data, Mail-specific configuration, legal copy, route/data and domain actions. Acceptance: all three named table surfaces retain content/actions and accessible headers/labels; current flows/layout, consent receipt, settings conflict and refusal behavior.
- [ ] **AP-MR-08 — HTTP/update/logging/crash.** Depends: AP-MR-01 + PK05/11/12/13. Owner: actual native transport/update/logs/capture callsites. Replace generic redirect/verified-download, update auth, logging retention/redaction and detector duplication. Keep Gmail wire/profile, account consent, diagnostic fields and capture adapter. Use existing shell watchdog detector once; no second loop. Acceptance: bounded transport failures/redirect policy, update auth remains native, secret redaction, bounded shutdown/capture.
- [ ] **AP-MR-09 — child process, window & PDF.** Depends: AP-MR-01 + PK02.capture for the confirmed startup-error caller + PK08; add PK16 only for a confirmed Swift consumer. Owner: `src-tauri/src/startup_error.rs::platform::show` (raw `std::process::Command::new("osascript")` at line 53), source callers and Tauri command registration. Replace this production launch with PK02 `run` (`RunOptions`) while preserving pre-window native dialog, QA-native no-dialog behavior and stderr fallback; migrate any other production callers only when source-confirmed. Replace duplicate generic window/PDF/preview mechanics only with matching published parity; preserve mail-specific attachment/message actions and product views. Acceptance: cancellation/timeout behavior for confirmed child callers, startup error path remains visible on macOS and quiet in native QA, window lifecycle, attachment path confinement and native-target coverage.
- [ ] **AP-MR-10 — QA/test mode/release switches.** Depends: AP-MR-01 + PK14/17. Owner: `src-tauri/src/tauri_run.rs`, launch/build feature sources, `package.json` scripts and release config. Keep compile-gated QA registration, isolated test roots and security controls. Classify switches as QA selectors vs production journal/credential guards; do not strip security guards. Acceptance: test roots cannot touch production DB/credentials; QA/dev controls absent from release artifact; source, feature closure & packed payload scan satisfy M23.
- [ ] **AP-MR-12 — exception-ledger reconciliation.** Owner: app integration owner, after AP-MR-02..10 deletions land. Reconcile every existing `docs/rightkit-exceptions/M*.md` entry (5 files: M1, M2, M4, M6, M7) and matching `.rightkit/ownership-exceptions.json` record against source. For each entry record `retired by <AP/PK receipt>` when mechanics are deleted, `product data/action` when it is not shared mechanics, or `retained with exact evidence + sunset/review condition`; remove stale “kept” claims and obsolete registry rows in same change as deletion. Do not treat a ledger entry as authority to retain generic mechanics. Acceptance: every ledger/registry row has disposition, source/caller anchor, exact receipt for retired mechanics or bounded evidence/sunset for retained code; no stale exception statement remains.
- [ ] **AP-MR-11 — final parity & residue.** Depends: all contributing tasks, including AP-MR-12. Owner: app integration owner. Search expanded duplicates after edits across Rust, TS/TSX, Swift/other native files, build scripts, generated command lists, lockfiles & release artifact. Each deleted function maps to published owner receipt and production caller evidence; every residue names exact function/caller, unique need, failed shared option/missing capability and review condition. Run focused native Windows/macOS app checks after all edits. Record actual commands/results; no claims from imports/compilation alone.

## Authoritative AP capability prerequisites

Use only stable `PKnn.slice` IDs from `01-packages.md` §6. PK labels in task prose are owner context, not whole-PK gates. This table supplies manifest `requiresCapabilities`. A conditional capability may be omitted only when source/configuration evidence shows that specific caller is absent; this does not make the AP task N/A when other work applies. Unresolved scan is `unknown`, not N/A. Task-level N/A is allowed only when no AP work applies. Prior AP is ordering, separate from capability readiness; `AP-MR-01` means successful manifest identity preflight only, while its pinning subtask is re-entered incrementally and closes after adopters. Every other applicable AP must complete before AP-MR-11 closes.

| AP task | `requiresCapabilities` | Conditional caller/platform disposition | Prior AP |
|---|---|---|---|
| AP-MR-01 | `[]` | Manifest identity and snapshot validation only; do not gate on owner receipts. | package executor creates manifest |
| AP-MR-02 | `PK01.transactions`, `PK03.encrypted-lifecycle`, `PK07.root-cutover` | `PK07.migration` only if source confirms schema migration in transition; evidence actual encrypted key lifecycle and companion set. | AP-MR-01 |
| AP-MR-03 | `PK04.settings-baseline` | Qualify the existing published Settings capability with `qualify-existing`; do not wait for PK09 primitives or new host-service capability absent an actual settings bridge caller. Preserve `SqlSettings` adoption & domain queries. | AP-MR-01 |
| AP-MR-04 | `PK06.engine-host` | Add `PK06.daemon-client` only if source confirms consumption; `supervise_adopted` accepts `PrivateChild`, not arbitrary resident processes. | AP-MR-01 |
| AP-MR-05 | `PK07.approvals`, `PK07.services`, `PK15.applicability` | Source-confirmed Mail agent gate methods. Preserve exact account/action/resource policy and protocol; audit-after-effect failures are not pre-effect refusals. | AP-MR-01 |
| AP-MR-06 | `PK04.host-services`, `PK03.protected-store`, `PK05.license-client`, `PK05.device`, `PK17.release` | Qualify only shipped native hosts. For `@rightkit/license` 0.3.0 `./testing` adoption, require PK17.release & pin exact version from that receipt; update auth belongs to AP-MR-08 only if an actual caller consumes it. | AP-MR-01 |
| AP-MR-07 | `PK04.host-services`, `PK09.primitives`, `PK10.receipts` | `PK10.license-step` only for exact license-onboarding consumer; `PK10.notices` only for actual release notice generation. Retain content/actions/accessibility at three named table callers. | AP-MR-01 |
| AP-MR-08 | `PK05.license-client`, `PK11.redaction`, `PK11.log-config`, `PK11.watchdog`, `PK12.redirect`, `PK13.update` | `PK12.downloader` only if source confirms verified-download caller. Update auth only for actual caller; shell detector once; capture adapter only when used. | AP-MR-01, AP-MR-06 |
| AP-MR-09 | `PK02.capture`, `PK08.shell-modules` | `src-tauri/src/startup_error.rs::platform::show` is a confirmed macOS process caller; it passes args only, so published `run`/`RunOptions` (`PK02.capture`) suffices, with no environment/cwd configuration caller established. Add `PK08.surface` only for source-confirmed SurfaceSpec caller and `PK08.pdf` only for source-confirmed PdfHost caller; add `PK16.*` only for actual Swift consumer, not Rust PDFKit. Qualify actual supported native targets. | AP-MR-01 |
| AP-MR-10 | `PK14.mode-roots`, `PK14.harness`, `PK14.qa-chrome`, `PK17.scanner`, `PK17.recipes` | `PK17.recipes` only for exact `right-release.config.mjs`, dev scripts & `stage-dev-sidecars.mjs` callers. No `PK14.fixture-capture` or `PK17.activation`. | AP-MR-01 |
| AP-MR-12 | `PK17.adoption-evidence` | Reconcile every existing exception-ledger record after contributing deletions; ledger presence does not exempt generic mechanics. | AP-MR-02..10 |
| AP-MR-11 | `PK17.adoption-evidence`, `PK17.handoff` | Close only after every other applicable AP, including AP-MR-12, is complete and every M1–M23 row has receipt, source-backed N/A or bounded retained behavior. No activation gate. | all other applicable AP-MR tasks |

## Final acceptance record

Record each deletion/adapter as `function/path | callers | published owner + exact version/integrity | features | production call path | deleted functions | test/receipt evidence | exception rationale`.

Required scenarios: relocation/restore interruption with old/new identity retained; contention returns Busy before mutation; old DB reopens after failed transition; pending approval row persists before its identifier is returned; effect refusal before start is distinguished from applied/partial effect with failed audit or local settlement; trust remains account/action scoped; licence secrets stay native; `SqlSettings` stays authoritative; adopted child supervision remains bounded; QA isolation and release payload qualification pass.

No release/upload follows from this work. App readiness is complete only when manifest-backed pins, actual call paths, deletions, native evidence and exceptions are read back by integration owner.

## Verification targets for future owner

Commands below are declared by the inspected app manifest or exact source targets. None ran during this planning task. Run focused targets after all contributing edits, followed by app-level gates required by repository rules.

| Scope | Declared command/target | Evidence expected |
|---|---|---|
| Aggregate contract | `pnpm verify:contract` | package/release contract remains consistent. |
| Frontend types | `pnpm exec tsc --noEmit` | shared DTO/actions wired without weakened types. |
| Frontend tests | `pnpm test:frontend` | approval queue, licensing and UI caller behavior. |
| Approval queue | `tests/approvalQueue.test.ts` | pending, approve, deny and stale decision states. |
| License renderer | `tests/licenseUpdates.test.ts`; `tests/settingsLicenseUpdates.test.ts` | no token export, correct native action presentation. |
| Rust formatting | `rustright cargo fmt --manifest-path src-tauri/Cargo.toml --all --check` | changed native source formatted. |
| Rust check | `rustright cargo check --locked --manifest-path src-tauri/Cargo.toml` | native types/features compile against pinned published packages. |
| Rust tests | `rustright cargo test --locked --manifest-path src-tauri/Cargo.toml` | DB transition, approval audit and command behavior. |
| Relocation tests | `src-tauri/src/db/data_relocation.rs` tests at current `#[test]` entries | successful/failed copy and retained source state; extend for cross-process interruption where needed. |
| DB lifecycle | relevant `src-tauri/src/db` unit/integration tests | reopen, pool quiescence, identity guard and recovery. |
| Approval gate | gate/approval tests under `src-tauri/src/agent` | pre-effect refusal vs post-effect audit/settlement failure; pending decision does not imply action success. |
| Supervisor | `src-tauri/src/core_ipc/tests_supervise.rs` | adopted child identity and existing lifecycle semantics. |
| Trust | `src-tauri/src/core_ipc/tests_trust.rs` | service authentication and admission policy. |
| Release policy | `pnpm deps:audit`; source/build scans via PK17 | QA/test selectors absent; security guards retained. |
| Legal release | `pnpm test:legal-release`; `pnpm test:legal-acceptance` | current content and receipt behavior. |
| Full JS gate | `pnpm verify:js` | TS/lint/frontend/legal/contrast/tokens/build after all edits. |
| Full Rust gate | `pnpm verify:rust` | fmt/check/test/clippy after all edits. |
| App aggregate | `pnpm verify` | declared app suite; execute only after focused failures are resolved. |
| Native Windows | app owner uses configured Windows dev verification entrypoints | Credential Manager policy, DACLs, SQLite, cross-process transition lock. |
| Native macOS | app owner uses configured macOS dev verification entrypoints | Keychain namespace/ACL, SQLite pool/backup, daemon child lifecycle. |

Where an exact requested acceptance target is absent, owner adds a focused test to the owning existing module before implementation closure. Do not invent a passing test target; name required behavior and target location explicitly.

## Deletion receipt rules

For `data_relocation.rs`, enumerate each function/caller responsible for transition acquisition, root discovery, copy/staging, companion handling, verification, cutover, cleanup and recovery. Delete only mechanics proven by PK01/03/07 contract and published receipt.

Keep DB schema, account/message tables, migrations, validation callbacks, declared companion paths and product-selected new root. A shared transaction does not own MailRight's database domain model.

Trace every opener and pool in process, including UI, core/daemon and recovery entrypoints. A file lock is insufficient if an opener silently uses another root or a pool continues writes during transition.

Record old and new root identity fields in every recovery outcome. An interruption or failed reopen must leave original state reachable and prevent a second opener bypassing the transition guard.

For `SqlSettings`, search its trait impl, table accessors, transaction context, settings command callers and renderer callsites. Preserve this established SQL adoption; remove only repeated UI/bridge mechanics whose published owner is qualified.

For `supervise_adopted`, record `PrivateChild` identity at construction, adoption point, event callback, shutdown path and callers. This API does not establish support for arbitrary resident processes; retain launch/adoption for the actual MailRight core.

For agent gate changes, map each method and account/action resource check through pending state, durable audit, settlement and final result. Keep separate send/forward/delete/attach semantics and preserve server/provider outcome uncertainty. `gate.rs::handle` audits after effect execution, so audit failure does not prove effect absence.

Search every path that can return a successful response. Require pending decision state and required audit commit before success. Classify failure by stage: pre-effect refusal; effect known absent; effect applied/partial but local settlement or audit failed; or effect outcome uncertain. Do not report “no effect” for post-effect audit failure.

Search approval renderers and clients for Boolean fields such as `approved`, trust/profile IDs, account IDs, action names and resource keys. Ensure authorization remains tied to exact account, action and resource state.

For licence removal, search native command names, renderer raw-record APIs, token serialization, secret store access, update authorization and entitlement guards. Raw secret never crosses DTO boundary; preserve revoked-ID policy and explicit app feature names.

For QA, follow compile-time features, env selectors, Tauri registration and packaged payload. Preserve production journal/credential guards even where similarly named test switches are removed.

For logs/crash, search detector registration, callback, retention, redaction and shutdown. Reuse shared shell detector once, with MailRight-specific event fields/capture adapter only.

For child-process/window/PDF, enumerate actual callers first. No entire module is deletable merely because one helper moves to RightKit.

No app-local file/DB transaction should survive by being renamed. Conversely, do not delete business rules or recovery evidence merely because mechanics become shared.

Each removal requires published owner receipt, feature/native qualification, all-caller migration and a behavior-level test/receipt. Unpublished source and API declarations alone do not satisfy this rule.

## Residue and closeout format

Expand residue searches from each removed symbol to equivalent behavior: lock acquisition, pool shutdown, backup/copy, root switch, companion enumeration, audit insert, pending status update, approval timeout, token serialization, test-mode selector and release payload switch.

Classify remaining functions as schema/domain data, declarative configuration, actual product action, adapter or duplicate mechanics. Data/configuration/actions may remain with named callers and evidence. An adapter may remain only when exact published-owner gap, function and caller evidence prove it necessary; platform specificity alone is insufficient.

Record exact published owner version/integrity/source and enabled features beside each adopter; confirm `Cargo.lock`/`pnpm-lock.yaml` resolved to that release.

Record native test host/OS for transition-lock, credentials, SQLite and child-process evidence. Desktop compile alone does not establish platform qualification.

Read back all source, feature and payload scan results for M23. Keep controls that protect production data even if their names contain test/dev terminology.

Map all M1–M23 rows to qualified owner, source-backed N/A, or bounded retained product behavior. Any row without a receipt is still gated.

Close AP-MR-11 only after API/version/features, production call path, deleted functions, test evidence and exceptions are recorded for each owner row; no release or upload is implied.

## Platform and feature decision record

| Platform/feature | Required evidence before closing |
|---|---|
| Windows native host | Credential Manager persistence, file ACLs, SQLite backup/reopen, cross-process StoreTransition and child containment. |
| macOS native host | Keychain namespace/rotation, SQLite backup/reopen, cross-process StoreTransition and daemon child ownership. |
| Linux, if declared supported | Secret Service availability/failure semantics, DB key recovery and process lifecycle. |
| UI approval queue | Pending/approved/denied presentation follows durable native state; UI cannot authorize effect by itself. |
| Core IPC profile | Method descriptors, authenticated caller, account/resource identity and bounded process lifecycle. |
| QA/test features | Compile-time gate, isolated roots and no production artifact reachability. |
| Release switches | Classify each source read; protect production journal/credential guards; exclude test selectors from packed production payload. |
| SQL settings | Existing transaction backend remains source of truth across supported native hosts. |

Transition acceptance must include two independent processes. An in-process mutex test does not prove cross-process exclusion.

When killing a transition test process, record durable phase reached, lock owner state, old-root identity, new-root identity and subsequent recovery result.

When a pool cannot quiesce, show Busy before any mutation. A timeout alone must not unlock while a backend operation remains active.

When restoring or relocating, new root remains inactive until DB/key/companion verification and reopen succeed; old root remains recoverable through cleanup.

Never copy a live DB file with open connections as an ordinary fs transaction. Use qualified K03 backup/quiescence semantics.

The shared root guard must compare identities, not only path strings; record old and new identity through recovery and reject substitutions.

Companion file list is app-supplied declarative configuration. Domain schema validation and account/message semantics remain app-owned.

`SqlSettings` being shared through an existing package does not authorize replacing its SQL transaction with an independent JSON store.

`supervise_adopted` proves behavior for its adopted `PrivateChild` contract only. Record constructor and child identity to establish scope of any migrated lifecycle.

Do not add a generic resident-process exception just because some daemon is long-running. If a needed host contract is missing, keep app behavior until package release supplies it.

Mail approval trust binds exact account, action and resource. Shared state machine receives this policy as input; it cannot invent or broaden it.

An audit insert is mandatory before success only for cases whose gate contract requires it; preserve existing exact policy and test every audited outcome.

If an external send succeeds but local settlement fails, report indeterminate/settlement state accurately; do not claim send never happened or retry blindly.

Every secret/native action must preserve account consent, revocation, feature naming and existing server wire profile.

## Completion evidence shape

Attach exact package receipt ID/hash for each owner consumed by an AP-MR task.

List exact manifest/lock entries and native feature closure for Windows/macOS/Linux targets as applicable.

List every DB opener/pool and transition participant, including core process and recovery path.

List old/new identity guard and interruption-recovery cases with outcome evidence.

List SQL settings callers and confirm table/schema/transaction authority remains unchanged.

List daemon launch/adoption path and exact `PrivateChild` identity carried through supervisor.

List each agent method, policy input, pending state, audit event, effect result and response outcome.

List raw licence commands removed and safe native actions/DTO replacing each caller.

List QA selectors and production guards by source, feature and packed artifact evidence.

List removed local functions with replacement owner/API/version and caller parity evidence.

List retained domain functions with exact product behavior and callers.

List test target, native host, command, exit status and relevant assertions for each acceptance case.

List remaining publication gate with dependent AP-MR task IDs; no dependent task is silently closed.

Do not mark complete from passing compile, package import, UI-only tests or a single-process lock test.

## M19 release recipe caller closure

AP-MR-10 also depends on PK13/PK17 for applicable release recipes. Trace `right-release.config.mjs`, `package.json::dev:build:win/dev:build:mac` & `scripts/stage-dev-sidecars.mjs`. This script already calls RightKit `stage-sidecar`; preserve declarative MailRight core/engine identity & development-mode configuration. Move remaining generic launch/staging mechanics only when qualified recipe accepts those inputs; never migrate dev/test flags into production. Keep independent product legal/resource assertions. Acceptance: native development build/staging plan preserves required payload, hashes, identifiers & sign order; build remains upload-free, upload remains build-free. Distribution is outside this app packet.
