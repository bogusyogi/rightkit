# RightKit M1–M22 package specification — Astra r1

Date: 2026-10-20. Author: RK Astra. Status: **independent specification for comparison**. This revision defines proposed contracts; it is not jointly locked with Fable, implementation authorization, or evidence of qualification/publication.

## 1. Outcome & scope

Pulse, HeardRight, ViewRight, MailRight, ScrapeRight & CodeRight consume shared RK implementations for M1–M22. Applications supply product data, declarative policy & actual domain actions. Shared workflows must not remain six copies of orchestration around RK primitives.

Architecture classification: D2; objective `optimize` for minimum app infrastructure & one implementation per shared concern; critical rigor for authorization, credentials, data migration & process ownership. Independent specification authored after source inspection; no consultation with Fable on this draft. Earlier user decisions remain inputs.

Scope includes native hosts & Swift helpers participating in these six desktop applications. It does not create an engine, agent server, account system, onboarding step, or platform port where an application has no corresponding behavior. M23 & unrelated product capabilities remain outside this specification.

**Completion:** each applicable M row has an active published owner, exercised runtime entrypoint, deleted replacement functions & explicit evidence for remaining app implementation. Exact dependency pins alone do not satisfy any row.

### 1.1 Fixed decisions

1. Reuse existing APIs first; finish qualified candidate APIs second; extend their owners only for demonstrated gaps. No new general `app-engine-host` crate, universal adapter framework, or mandatory code generator.
2. Only ViewRight's iframe-specific inspector is preaccepted as app implementation. Its preview token store, authorization, file publication, PDFKit hosting & ordinary window mechanics are separately shareable.
3. One current consumer is sufficient when behavior is app-neutral. “Platform-specific”, “historical format”, “thin adapter” & “product domain” do not establish exceptions.
4. Credentials, signed licence records, account sessions & update bearer headers stay native. Renderer receives safe views & invokes typed actions.
5. Preserve product behavior during adoption. Share configuration without forcing identical geometry, thresholds, command sets, legal text, entitlement requirements or trust policies.
6. Preserve correctness fixes. Remove unnecessary unpublished additions; correct published behavior through a new compatible release. Never rewrite a published version or solve scope reduction solely by downgrading apps.
7. Publication is incremental by qualified capability. Dependent apps move to latest qualified published versions, with exact pins & resolved integrity.

### 1.2 Contract notation

`Existing` names denote inspected source APIs. `Proposed` names below define this revision's intended public shape; they are not claims those symbols ship today. Equivalent naming may be reconciled during comparison, but owner, semantics, errors & acceptance conditions are normative. All new option-bearing APIs preserve existing defaults; use new builders/types where changing public structs would break callers.

New capabilities are optional at package build/registration boundaries. Unsupported platforms return typed `Unsupported`, never fabricated success. App manifests choose capabilities; apps do not duplicate capability implementations. Test-only APIs & bypasses are compiled out of production.

## 2. Source baseline & availability

Read from local primary checkouts. These hashes identify commits, not clean trees: RK has concurrent uncommitted candidate work. Candidate APIs must be qualified from their actual packaged bytes. This document changes no product source or pins.

| Repository | Inspected HEAD |
|---|---|
| RightKit | `5a5892a9e4f07b17e985ee93ffe20a54ff78178c` |
| Pulse | `08ada52deafc8f895658fbd7ce93825e5a142ee9` |
| HeardRight | `fa82ecc295189565c4bdc162dc106009d7e036b3` |
| ViewRight | `b7e91c03642e0ddf89657adefe5a5eb2386f5aec` |
| MailRight | `4d835646e79f64ddfd52afd1a7d4c378b19dc04b` |
| ScrapeRight | `8ac9b1aac62075bc54fd530774af56949193ce10` |
| CodeRight | `15b3ac93e7896a94be913a21c592154e898589df` |

Owner lookup executed: `rightkit owns --compact` (redirects to RustRight). Its installed version index is older than inspected source & prior registry readback. Use it for owner identity, not release availability. [Mandatory catalog](D:/Claude/rightkit/packages/release/ownership-mandatory-set.json:1) & [declaration checker](D:/Claude/rightkit/packages/release/ownership-mandatory-set.mjs:4) are also insufficient evidence of runtime adoption. M21 currently has no owner & “never fails”.

Previously fetched package sources under `D:/scratch/rk-pub/` supply published behavior comparisons. Version numbers here are baseline observations, not a fresh assertion about registry latest at execution time:

| Family | Observed published baseline | Working candidate / decision |
|---|---|---|
| child-process | 0.5.2 | 0.5.3 fixes plus bounded additions; §5.2 |
| fs | 0.4.4 | 0.4.5 lease fix plus bounded additions; §5.1 |
| daemon | 0.3.0 | 0.3.1 root migration needs repair; §5.7 |
| db / app-settings / credentials | 0.5.0 / 0.2.4 / 0.3.0 | Compose existing APIs; extend only contracts below |
| license / device | 0.2.3 / 0.3.1 | Native licence client candidate 0.2.4; retain device build fix |
| job-queue / sidecar | 0.4.0 / 0.3.0 | Existing supervision/protocol is substantial |
| tauri-shell / @rightkit/tauri | 0.4.0 / 0.2.3 | Candidates 0.5.0 / 0.3.0; qualify feature by feature |
| logs / crash / HTTP | 0.3.3 / 0.3.3 / 0.3.6 | Existing core APIs; adoption plus bounded extensions |
| legal / legal-ui / onboarding | 0.6.1 / 0.4.1 / 0.6.2 | Changelog staging labels lag earlier registry observation; preserve published compatibility |
| app-update / release | 0.4.0 / 0.4.0 | Candidate work must meet native authorization & release checks |
| QA Rust / npm / Chrome | 0.7.1 / 0.5.1 / 0.1.8 | Retain Rust test-mode binding fix |
| agent-tools | 0.2.0 | Client toolbox is not itself an app-hosted agent service |
| RightKitSwift | App pins observed at 0.8.1 | Working VERSION 0.9.0; package manifest already contains System, Store, EngineClient, Engine, Update & Account |

Do not invent final version numbers now. For each publication, record actual version, commit + packaged-tree digest, supported target/features, registry integrity/tag & exact dependent pins. If a candidate version has since published, corrections require a new version. Stale Swift ledger entries 0.4.0/0.7.0 must not overwrite app pins or define availability.

## 3. M1–M22 ownership crosswalk

These are capability obligations, not a demand to install every optional subsystem in every app.

| Row | Shared owner / composition | Contract |
|---|---|---|
| M1 Window host | rightkit-tauri-shell + rightkit-device + @rightkit/tauri; native Swift equivalents | K04, K08, K16 |
| M2 App frame | @rightkit/react-shell + @rightkit/platform-ui + @rightkit/theme | K09 |
| M3 Engine supervision | rightkit-job-queue EngineSupervisor; daemon supervisor for daemon/private-child lifecycle; child-process underneath | K02, K06 |
| M4 Window/CLI protocol | job-queue contract + sidecar framing; daemon transport for resident service | K06 |
| M5 Other child processes | rightkit-child-process | K02 |
| M6 Data location | rightkit-daemon AppRoot/Suite; fs transaction & db coordination underneath | K01, K03, K07 |
| M7 Local services | rightkit-daemon | K07 |
| M8 Safe file writes | rightkit-fs | K01 |
| M9 Database | rightkit-db; credentials only for protected key storage | K03 |
| M10 Settings | rightkit-app-settings + @rightkit/settings-ui; shared host registration | K04, K09 |
| M11 Secrets | rightkit-credentials; native typed service APIs | K03, K05 |
| M12 Licence/device | rightkit-license + rightkit-device; @rightkit/license as renderer view/action client | K05 |
| M13 Pro entitlement | native rightkit-license authorizers & engine readers | K05 |
| M14 Legal/onboarding | @rightkit/legal + legal-ui + onboarding | K10 |
| M15 Logging | rightkit-logs + @rightkit/logs | K11 |
| M16 Crash reporting | rightkit-crash + shared host heartbeat adapter | K11 |
| M17 HTTP | rightkit-http; native host bridge; browser-specific session adapter where needed | K12 |
| M18 Updates | @rightkit/app-update policy/UI + native updater transport + license UpdateAuthProvider | K13 |
| M19 Release tooling | @rightkit/release | K13 |
| M20 QA | rightkit-qa + @rightkit/qa + qa-chrome | K14 |
| M21 Test mode | fs validates root; daemon carries execution context; QA creates/binds it; release verifies absence of bypasses | K14 |
| M22 Agent surface | agent-tools for clients; daemon admission/transport for hosts; product method handlers | K07, K15 |

## 4. Acceptance drivers

| ID | Observable scenario required |
|---|---|
| Q1 | Upgrade published packages & restart existing installations: settings, identities, legal assent, credentials, history & command behavior remain compatible. |
| Q2 | Untrusted iframe/window/client invokes a privileged command: request fails before storage, network, process launch or secret access; permitted UI still works. |
| Q3 | Terminate process at every durable migration/rotation phase: restart selects verified old/new state or reports recovery required; never loses sole valid key/data copy. |
| Q4 | Child hangs, loses pipe output, exits during handshake or refuses shutdown: deadline is bounded, incomplete output is an error, ownership & reap outcome are explicit. |
| Q5 | Concurrent settings edits, file saves, reconnects or retries: conflicts are explicit; authorization/admission cannot be replayed for a different request or generation. |
| Q6 | Windows/macOS native host performs same product workflow: platform adapter preserves focus, ACL/keychain, geometry, lifecycle & persistence guarantees. Linux is qualified wherever that consumer ships. |
| Q7 | Production artifact receives test environment variables, test-root paths or QA commands: no licence bypass/control endpoint is available; supported QA build touches only its bound isolated namespace. |
| Q8 | App migration finishes: old implementation is deleted, actual callers reach published owner, retained implementation has function-level evidence. |

## 5. Package contracts

### K01 — rightkit-fs: publication, coordination & recovery

**Reuse:** AtomicWriter/commit_checked, achieved publication reporting, FileLock/Lease, StateDir, private writes/DACL handling, confined opens, verified copy, strict directory publication. [Exports](D:/Claude/rightkit/crates/rightkit-fs/src/lib.rs:100). A nonsecret output still requires correct publication.

**Required fixes:** publish a fully initialized lease guard through strict no-replace publication before acquisition. Preserve one stable lock inode & unknown/live legacy-owner refusal; never restore empty-placeholder fallback. Keep capture of original platform errors. Qualify deterministic contender test plus process interruption.

**Proposed additions:**

| API | Contract |
|---|---|
| `KeyedWriteLocks::with_destination(path, operation)` | Shared in-process coordination keyed by normalized destination identity, including nonexistent leaf under verified parent. No unbounded permanent lock map. Used with existing AtomicWriter; optional FileLock for cooperating processes. |
| `FileTransaction::prepare(root, entries, options) -> PreparedTransaction` | Entries contain bounded relative destination, staged candidate, expected prior identity/hash & publication mode. Reject escapes, overlapping destinations, duplicate entries & substituted parents. Preparation changes no live destination. |
| `PreparedTransaction::commit() -> TransactionReport` | Durable journal records prior/candidate identities & phase before mutation. Preserve rollback material until committed state is durable. Report `Committed`, `RolledBack`, or `RecoveryRequired`; post-publication failure is not reported as “nothing happened”. |
| `recover_transactions(root) -> RecoveryReport` | Acquire same operation lock, validate journal/staged/backup identities, resume or restore deterministically. Unknown or externally modified material is retained & reported for recovery; never age-delete it. |

Transaction guarantees are recoverability & serialized cooperating writers. **No all-file atomic visibility** for independently reading paths. Consumers needing coherent multi-file reads use a generation directory plus atomic pointer, or a declared reader/writer lock protocol. Cross-volume moves stage/copy into destination filesystem before publication. DB connections are quiesced by K03, never copied live by fs.

ViewRight's save contract retains digest checks before encoding & immediately before replacement. Shared keyed lock replaces its local map; this does not promise compare-and-swap against an uncontrolled external editor between final check & rename. Preserve current conflict behavior & make that concurrency boundary explicit.

**Bounded additions already proposed:** keep Windows access-denied retry only as opt-in with one shared finite retry budget for codes 5/32/33; never change ACLs or retry forever. Keep strict directory no-replace policies if their existing named consumer qualifies them; they are not a prerequisite for every six-app migration. No generic filesystem transaction DSL.

**Delete targets:** ViewRight `destination_write_lock` & generic `write_atomic` mechanics; ScrapeRight raw Save-As publication; app-local multi-file staging/journal/rollback mechanics once K03/K07 consume this owner. Product encoders & file lists remain inputs.

**Accept K01:** Q1/Q3/Q5/Q6/Q8; inject failure before/after each rename/journal flush; assert old/new/recovery state, private-mode preservation, collision refusal, cleanup identity checks & external-writer conflict behavior. Exercise native Windows contention.

### K02 — rightkit-child-process: bounded execution & ownership

**Reuse:** OwnedCommand/OwnedChild, no-window command constructors, process table, owned tree termination, shutdown reporting. No app Win32 job-object or POSIX process-group implementation.

**Required 0.5.3 fixes:** capture errors propagate after both streams are collected; native Windows spawn receives configured stdin/stdout/stderr without losing env, cwd, raw arguments, handle allowlist or containment. An `Ok` result cannot mean output was silently lost.

**Retain candidate APIs with demonstrated consumers:** `run_command(OwnedCommand, input, &RunOptions)` for caller-configured env/cwd; `run_streaming` for ScrapeRight progress with bounded tails, cancellation & deadline; `WindowsJobBuilder::allow_breakaway(bool)` + `contain_current_process_with` for CodeRight's explicit replacement process. Preserve containment by default.

**Remove from unpublished candidate:** newly added *silent* breakaway on caller-built/current-process jobs. Explicit breakaway satisfies named replacement use. This does not remove an older published OwnedCommand API without compatibility handling. No blanket detached-launch conversion.

**Caller supplies:** executable/resource identity, argv, environment policy, input, timeout, output parsing & actual operation. RK owns spawn/capture/kill/reap. No shell text interpolation to replace typed launch.

**Accept K02:** Q4/Q6/Q8; stdout/stderr failure, large stdin with surviving descendant, timeout/cancel, nonzero exit, explicit Windows native spawn, replacement allowed only when requested, ordinary descendants still contained. No publication based only on compilation.

### K03 — rightkit-db + rightkit-credentials: encrypted-store lifecycle

**Reuse:** DbOptions, online backup, rekey, quarantine/recovery, journaled restore & connection-pool close/wait. Credentials already supports Linux Secret Service & rotation (`begin_rotation`, `pending_rotation`, `commit_rotation_of`, `abort_rotation_of`). [DB](D:/Claude/rightkit/crates/rightkit-db/src/lib.rs:31), [rotation](D:/Claude/rightkit/crates/rightkit-credentials/src/rotation.rs:348).

**Proposed db module `encrypted_lifecycle`** composes these owners. Credentials remains responsible only for secret storage/rotation; db owns database/key consistency. Feature-gate dependency to avoid requiring credentials for unencrypted stores.

| Operation | Required semantics |
|---|---|
| `EncryptedStore::open(config)` | Serialized transition; try authenticated primary, current pending & explicitly mapped legacy staging keys before quarantine. Distinguish missing store, missing key, denied/unavailable credentials & corrupt DB. Never replace unreadable state with empty DB automatically. |
| `rotate_key(request) -> RotationReport` | Verify current key opens target; durably retain old key in protected recovery storage; stage next key; rekey DB; verify reopen; promote exact staged revision. Read back resulting primary/pending state even when promotion returns an error. Classify `Committed`, `CommittedCleanupPending`, `NotCommitted` or `Indeterminate`; cleanup-only failure never triggers rollback. A verified noncommit may roll DB back only with coordinated primary-key restoration/readback. Retain both viable recovery keys until durable DB/key consistency is proven. |
| `recover_rotation() -> RecoveryReport` | Determine which authenticated primary/pending/legacy/recovery key opens DB; reconcile marker & credential state without deleting sole viable key. Unknown state blocks ordinary writes; retain recovery material. Process death is handled as deliberately as returned errors. |
| `backup/restore` | Delegate existing APIs; coordinate writer/pool quiescence, verification, key identity & restart. SQL schema/domain integrity validation is supplied by app. |
| `relocate(plan) -> RelocationReport` | Quiesce DB, verified online backup to new root, include companion plan through K01/K07, activate root only after verification; preserve original until new root reopens successfully. |

Config includes database identity/path, existing credential namespace/account, explicit legacy staging-account aliases, encryption mode, schema/migrations, validation callback & host quiescence notification. HeardRight's [legacy staging account](D:/Claude/heardright/tauri-app-next/src-tauri/src/credential_store.rs:283) is `SQLCipher/DBKey.staging`; it must be read/migrated before switching to RK's `<account>.rotation-pending` convention. A successfully authenticated legacy staging key may be the sole live DB key. Delete its old item only after canonical protected storage & DB reopen readback prove recovery remains possible.

**Transition exclusion:** db owns one bounded-acquisition, cross-process `StoreTransition` guard for open/key selection, rotate, recovery, restore & relocate. It spans whole workflow, not individual credential calls, with stable database & credential-store identity pinned throughout. Lock order is lifecycle guard → credential rotation lock → internal DB operation. Drain/close conflicting readers, writers & pools before rekey/cutover; all supported openers participate. Host supplies stop/resume notification for its domain workers, never a substitute local mutex. Failure to quiesce returns `Busy` before mutation. A timeout must not release protection while an in-flight backend operation can still change state; retain ownership until operation settles or enforce recovery-required exclusion. Restore/relocation journal both old/new root identities so no opener can evade guard by changing path.

RK supplies default OS backend. No app mirror of staging/promotion/recovery state machine. Native credential-store denial never falls back to plaintext. Existing [credential commit](D:/Claude/rightkit/crates/rightkit-credentials/src/rotation.rs:339) writes primary before marker cleanup; its `Err` alone does not establish noncommit. Shared lifecycle must classify actual state under its operation lock, never convert every error into a rollback.

**Delete:** HeardRight `history/key_rotation.rs::rotate_with` mechanics & equivalent open/recovery choreography; MailRight `data_relocation.rs` transition/journal/copy/verification mechanics. Retain Mail account/message tables, Heard transcript schema & declarative companion paths. Mail's existing `SqlSettings` & online backup adoption are preserved, not replaced by another implementation.

**Accept K03:** Q1/Q3/Q5/Q6/Q8; primary unavailable, staging unreadable, rekey fail, primary-written/cleanup-failed promotion, indeterminate readback, rollback fail, old DB key durably retained, legacy staging as sole viable key, kill after each phase, pool with active readers, wrong destination key, collision & successful reopen. Verify namespace/keychain attributes & Windows persistence policy remain compatible.

### K04 — settings persistence & shared host registration

**Owners:** rightkit-app-settings stores; rightkit-tauri-shell host commands; @rightkit/tauri typed bridge; settings-ui presentation. Existing `Store`, `Document`, revision CAS, flat/envelope format, bounded reads, strict refusal & external polling remain canonical. [Document](D:/Claude/rightkit/crates/rightkit-app-settings/src/document.rs:30), [write options gap](D:/Claude/rightkit/crates/rightkit-app-settings/src/atomic.rs:42).

**Proposed:** `StoreOptions::write_security(profile)` translates to existing fs private-directory/file policy. Profiles are explicit public/default versus owner-private; preserve current default for compatibility. Return typed failure if requested protection cannot be achieved. Do not duplicate chmod/DACL code in settings.

**Proposed host registration:** `HostServices` registers typed settings/logs/licence/update services once under `HostCallerPolicy`. Extend existing shell handler/feature mechanism; reuse candidate logs commands. Each service declares trusted window labels/origins, enabled actions & its native service instance. Caller identity comes from native invocation context, never caller JSON. Central policy also guards existing privileged shell commands when enabled for untrusted multi-webview hosts.

Settings bridge contract: `describe`, `get`, `patch(expectedRevision, changes)`, `subscribe`, `reset(keys)`; all mutations validate complete candidate values before publication, return resulting revision & typed conflicts, then notify subscribers after commit. Notification failure cannot pretend persistence failed. External updates preserve CAS semantics. Namespace & path cannot be supplied by untrusted renderer.

Licence bridge calls K05. Update bridge calls K13. Logs bridge calls K11. **No generic renderer credential get/set/delete**, arbitrary file path, SQL, shell command or authorization-header command. Compatibility aliases live once in RK & have explicit removal criteria.

App input: schema/defaults, legacy format/import descriptor, namespace, size limits, null semantics, UI descriptions, actual product side-effect handlers. Shared `apply settings → publish committed revision → update engine snapshot` orchestration belongs K06 when repeated; app action changes asset scopes/models as required.

**Delete:** command-name maps, duplicate get/save wrappers, polling/subscription loops & generic migration/default merging across six apps. Preserve Pulse's read-only refusal/BOM/size behavior & Mail's SQL transaction settings backend.

**Accept K04:** Q1/Q2/Q5/Q6/Q8; stale revision, failed validation/private ACL, newer schema/oversized file, external edit, read-only backend, compatibility aliases, malicious content window & no secret response paths. Generate bindings only if concrete schemas would otherwise be duplicated.

### K05 — licence, identity, account & entitlement

**Owners:** rightkit-license native client/lifecycle; rightkit-device installation identity/authorization adapters; credentials storage; @rightkit/license safe renderer API. Retain device 0.3.1 dependency-floor fix.

**Qualify existing native candidate:** `LicenseClient::{status,activate,refresh,deactivate,request_trial,lease_activate,lease_renew,lease_deactivate,import_legacy}`; `LicenseView`, `UpdateAuthProvider`, native engine record reader. [Candidate client](D:/Claude/rightkit/crates/rightkit-license/src/client/mod.rs:1). No second native client.

Native client owns whole activation/lease/refresh/deactivation/migration lifecycle, persistent anti-rollback clock, verification, revocation, rotation/tombstone recovery & redaction. Status is display-only when busy/unresolved; authorizers fail closed. Preserve agreed perpetual/subscription/trial policies, verified device binding & explicit disabled/unconfigured behavior. Native entitlement invalidation propagates to live engine generations.

**Proposed extension inside client, not new account framework:** `AccountSessionClient` with `request_sign_in(email)`, `complete_sign_in(code)`, `status`, `sign_out`, `list_devices`, `rename_device`, `remove_device`, `request_link`, `redeem_link`. Bind app/service origin & existing wire profile at native construction. Passwordless session tokens & server-issued credentials stay native; return redacted account/device/status DTOs. HeardRight is sufficient named consumer. Keep existing service paths/payloads as compatibility vectors; no invented backend endpoints.

Shared Pro-bridge exchange & lease transport become native typed operations in this owner. Swift `RightKitAccount` supplies existing Apple counterpart; implement shared semantics once per language runtime, not once per app. Domain labels, supported authentication mode, compiled origin, feature names, purchase links & consent copy are configuration. Changing backend trust protocol is not required to share client mechanics.

Account deletion is an explicit typed destructive action invoked by existing confirmed product flow; shared client preserves server refusals such as active-subscription restrictions & clears local session only under documented outcome. A transport timeout is not proof deletion failed or succeeded. Server business rules remain server-owned.

**Native boundary:** raw licence key/email may pass in as user input; activation/session/signed/lease tokens never pass out. `LicenseView` is a fixed allowlist. Legacy licence import verifies app/device/signature before committing; cleanup occurs only after durable readback; tombstones prevent resurrection. Secret writes remaining from old renderer clients are removed after migration.

**Delete:** renderer token storage/mappers/network refresh in ViewRight/CodeRight/MailRight; app-native lifecycle/keychain/lease/bridge/account client workflows in HeardRight & ScrapeRight where replaced; equivalent Pulse flows if used. Retain actual domain feature checks, expressed against shared native authorizer.

**Accept K05:** Q1/Q2/Q3/Q5/Q6/Q8; wrong app/device, replayed clock, expired/revoked entitlement, offline policies, interrupted deactivation, concurrent refresh/sign-out, old token arriving after tombstone, imported legacy fields, secret-store denial, renderer inspection & updater headers. Current source candidates are not proof these checks passed.

### K06 — engine supervision, protocol & host composition

**Existing:** job-queue `EngineSupervisor`, snapshots, handshake/deadlines/restart budget, `contract::Dispatcher/CommandSet/serve/run_cli`; sidecar framing; daemon private-child supervisor. HeardRight & ScrapeRight already use EngineSupervisor; MailRight already uses daemon `supervise_adopted`. Pulse resident engine already uses daemon hosting & job-queue dispatcher. No blanket replacement of working adopted layers.

**Host composition:** extend shell `EngineHost`/`EngineSpec`/`EnginePolicy` & Swift counterpart to register startup, ready/health state, command allowlist, method deadlines, event forwarding, snapshots & shutdown once. [Existing shell host](D:/Claude/rightkit/crates/rightkit-tauri-shell/src/engine.rs:28). App supplies command table/domain handlers, executable config & snapshot schema/order; RK owns generation fencing, replay, queue limits, reconnect & cancellation.

Two supported lifecycles: owned child, or authenticated resident service. Resident attach verifies endpoint/service identity & generation; arbitrary discovered PID is never considered owned. Existing `supervise_adopted` takes `PrivateChild`; it is not permission to kill unrelated attached services. Extend daemon client lifecycle with typed `Owned` / `Attached` provenance, `attach_or_launch`, `acquire_session`, `release_session`, `shutdown_owned`. Owned replacements retain process identity; attached clients release their session without killing shared service.

CodeRight's current bearer-loopback wire & presence/refcount policy must work through a bounded RK transport/session adapter; do not silently substitute daemon remote `RK-Device` authentication. Legacy wire codecs can live inside shared owner. Launch paths/resources & domain method definitions remain app data.

Protocol contract retains method names, request IDs, error codes, cancellation, event ordering & frame/queue bounds. Stale-generation responses cannot complete new requests. Non-idempotent uncertain-delivery calls are not automatically replayed. Auth/entitlement snapshots apply before admitting relevant work; revoke requires appropriate acknowledgement/invalidation.

**Delete:** generic host startup/adoption/heartbeat/retry/refcount loops; custom request routers/framing already supplied by owner; duplicate engine snapshot orchestration. Preserve OCR's existing rightkit-ocr supervision; do not add another supervisor around it.

**Accept K06:** Q2/Q4/Q5/Q6/Q8; wrong endpoint identity, recycled PID, two shells attach, owner exits, one client detaches, replacement starts, stale reply arrives, snapshot/revocation races, exhausted restart budget, graceful stop & kill fallback.

### K07 — daemon roots, local services & approval workflow

**Reuse:** Suite/AppRoot, discovery, authenticated transport, daemon remote pairing/method policies, event epoch/cursors & lifecycle hooks. Remote event pagination already exists; no new event bus.

**Root migration:** finish `AppRoot::migrate_from_with`. Preserve empty directories, relative/absolute/dangling links without traversal, verified files & permissions. Strict mode remains default. Destination collision refuses; original tree stays intact. Known candidate empty-directory omission must be fixed before publication.

Completion distinguishes `Ready`, `ReadyWithOmissions` & `Incomplete` in a versioned marker. Only explicitly declared disposable entries may be omitted; every omission is recorded. Required files/links block activation. Old readers must never interpret incomplete candidate as migrated success: retain old marker shape only for complete compatible moves; use separate pending/optional metadata until compatibility permits richer marker. Subsequent calls retry unresolved entries rather than returning early on source match. Junctions/reparse points require explicit supported policy or refusal, never traversal by accident.

`RootCutover` composes K01/K03: stop writers, copy/verify, persist selected generation, switch every reader/service, reopen & verify, then mark completed. Pulse's unused migration helper must be wired only after all root consumers switch together. Old roots remain recoverable; cleanup is a separate identity-checked operation, not automatic deletion on copy success.

**Service registration:** app provides identity, method descriptors, transport profile & action handlers. RK owns endpoint publication, peer authentication, admission, lifecycle, bounded connections/events & shutdown. Register no public listener merely to satisfy M7/M22.

**Proposed `approval` module:** `ApprovalStore`, `ApprovalCoordinator::{request,decide,claim,settle,cancel}` with request ID, effect digest, caller/account scope, expiry, decision revision & execution outcome. Native `ApprovalVerifier` supplies verified principal/claims. Coordinator enforces expiry, revocation, request binding & one execution claim; durable intent/audit precedes effect when policy requires it. A crash after external effect produces `OutcomeUnknown` unless effect supports idempotent reconciliation; no exactly-once external-effect claim.

Pulse signed-owner profile retains P-256/Secure Enclave semantics, action/client binding, nonce, expiry, enrolled key registry & separate PIN permissions. Mail profile retains account-scoped DB decisions, send/forward/delete/attach actions & audit outcomes. Do not weaken either into a generic Boolean approval callback. Move reusable verification, pending-state, audit formatting/redaction & timeout mechanics into RK; wire formats/profile fields & effect handlers remain declared inputs.

**Delete:** Pulse/Mail repeated approval state machinery & generic local-service hosting; Mail root relocation choreography; Pulse parallel root discovery/cutover helpers. Existing successful adoption remains.

**Accept K07:** Q1/Q2/Q3/Q5/Q6/Q8; linked empty dir, required/optional link creation denied, interrupted marker, old reader, collision, path substitution; approval replay across actions/accounts, revoke, expiry, double claim, failed audit & uncertain effect result. Pulse signing vectors must remain byte-compatible.

### K08 — native shell, overlays, preview & PDF host

**Owners:** tauri-shell host; rightkit-overlay native placement/presentation; rightkit-pdf rendering/PDFKit adapter; optional Swift host counterparts. Avoid app-specific dependencies in these packages.

**Window surfaces:** extend existing WindowSpec/reveal policies with `SurfaceSpec { id, size, minimum, placement, activation, close_behavior }`; `set_surface(id)` serializes resize/center/reveal. Config supplies legal/setup/main geometry. First-ready fallback is shared, bounded & cancellable; background/never-show launch cannot be overridden by fallback.

**Overlay host:** `OverlayHost::{show,update,hide,dispose}` composes existing style/placement primitives. Own work-area clamp, mixed-DPI monitor transitions, native no-activate/focus policy, topmost/fullscreen handling, click-through, listeners/timers & native cleanup. App renders pill/notch content & sends declared visibility/state inputs. App-specific animation values are data; generic animation scheduling/OS window code is shared.

**Preview:** qualify existing candidate `PreviewStore`, limits, expiry/revoke & scheme handler. Host config supplies allowed content kinds/CSP; RK owns token generation, size/count budgets, lookup, content isolation & revocation. Untrusted preview gets no privileged IPC. [Candidate](D:/Claude/rightkit/crates/rightkit-tauri-shell/src/preview.rs:129).

**PDF:** preserve already-shared rightkit-pdf inspect/compose/save behavior. Add optional native `PdfHost` backend for macOS PDFKit with `open(authorizedResource, owner)`, `set_frame`, `state`, `find`, `print`, `suspend`, `close`. tauri-shell owns caller/window/resource authorization & registration; pdf owns native view/render/find/print lifecycle. Never accept renderer-provided arbitrary native handles. Backend resources close with owner view; stale owner tokens refuse. Other platforms select existing backend or typed unsupported.

**App lock:** shared shell lock coordinator composes credentials authentication & daemon/engine lock state. `lock`, `begin_unlock`, `complete_unlock`, `status`; bind challenge to caller/session & expiry, invalidate privileged engine access before announcing locked, never export PIN-derived keys. Product decides which named actions require unlock through policy. Preserve Pulse distinction between local PIN & remote admin approval.

**Delete:** Heard hub/pill native window lifecycle/clamping; Scrape `window_surface`; duplicated native lock orchestration; View preview store & native PDF hosting mechanics. **Retain View iframe inspector interaction only**, with native denial to privileged commands & content-only window policy. Shared packages must not introduce whole-app production devtools as part of this extraction.

**Accept K08:** Q2/Q5/Q6/Q8; focus remains with previous app, off-screen windows recover on monitor removal, setup geometry transitions, background launch, close/dispose leak checks, PDF owner isolation, preview revoke/expiry, lock/restart & inspector confinement.

### K09 — app frame, settings UI & neutral UI primitives

Keep react-shell/platform-ui/theme/settings-ui owners. Consume shared shell/frame/settings sections with declarative routes, section descriptors, labels/assets, row controls & domain content slots. RK owns common loading/error/busy, keyboard/focus/accessibility, responsive shell layout & settings subscription mechanics.

Add a primitive only when a named current app implementation will be deleted: examples are standard path-picker row, progress/status row, confirmation surface & native action section. Do not turn entire mail/document/chat product views into a generic configuration language. A data-only descriptor is acceptable app remainder; duplicated form save/error/subscription state machine is not.

Retain existing defaults & visual behavior. Compatible geometry variations remain options. No global redesign or new universal UI package in this scope.

**Accept K09:** Q1/Q6/Q8; keyboard/focus, disabled/pending/error states, schema validation, theme & per-app geometry, plus visible parity for migrated surfaces. Delete only corresponding shared UI mechanics, not product content.

### K10 — legal & onboarding workflows

Reuse legal manifests/assent hashes, legal-ui rendering & onboarding step/persistence APIs. Shared step adapters own permission request/status/error/retry, model-install progress/cancel, native licence/account operations & completion persistence. App supplies ordered step descriptors, content/assets, permission/model IDs & actual supported capabilities.

Proposed adapters: `permissionStep(backend, spec)`, `modelInstallStep(backend, spec)` & native-client variants of existing licence/sign-in steps. Backend traits reflect concrete OS/model operations; they do not contain duplicate navigation, polling or error workflows. Legal assent remains bound to required version/document hash; persistence errors keep gate closed. A bare legacy Boolean must not bypass required assent.

**Contained layout decision:** keep compatible `layout="contained"` in legal-ui/onboarding. A host-sized legal/setup surface is a valid shared layout option & avoids app CSS forks. Do not make every app enable it, couple it to Pulse by name, or republish solely to remove this small published option. Existing window default stays unchanged; future package qualification covers both layouts. This revises the earlier “optional therefore unnecessary” criterion: usefulness is established by reusable host behavior, not number of adopters.

**Delete:** app-local onboarding state/navigation/pending/skip/error mechanics where owner already supplies them. Scrape model-job identifiers & model selection remain data/domain actions; pending-job scheduling belongs job-queue, not component-local storage loops.

**Accept K10:** Q1/Q2/Q6/Q8; first run/reacceptance, failed assent save, cancel/deny/retry permissions, offline/model failure, skip/back/finish, relaunch at incomplete step & viewport/contained layouts. No step disappears because shared UI lacks parity.

### K11 — logs, diagnostics & stall reporting

Reuse logs JsonlStore, RotatingTextWriter, stderr capture, redaction, collection & crash APIs. CodeRight already uses RK rotation; extend/compose instead of adding another log backend. Shell registers native commands once under caller policy; @rightkit/logs handles renderer collection.

Proposed `rightkit_crash::UiWatchdog` with `WatchdogConfig { ping_interval, response_deadline, episode_cooldown, retention }`, `start(ping, capture, sink)` & `stop`. RK owns episode detection, bounded capture, cooldown, cancellation & retention; tauri-shell/native adapter supplies main-thread ping & platform dump operation. Heard's current timings are compatibility inputs, not universal defaults. Emit structured start/stall/recovery/capture-failure events without secrets.

App supplies diagnostic fields & permitted sources as descriptors. Shared collection enforces size/retention/redaction/consent; no arbitrary filesystem collection from renderer. Logging disabled by user remains disabled where currently supported. Audit durability for effects is K07; ordinary diagnostics must not become a second approval ledger.

**Delete:** per-app log command wrappers, custom rotation/capture/collection loops, Heard ui_stall_watchdog scheduler/episode mechanics. Retain product event names/fields & explicit platform dump implementation only until adapter extraction qualifies.

**Accept K11:** Q2/Q4/Q6/Q8; stalled UI produces one bounded episode, resumes, stops cleanly; capture fails visibly; known secrets scrubbed from stdout/stderr/errors; caps/retention/consent preserved.

### K12 — HTTP, browser authentication & cookie artifacts

Reuse rightkit-http policy/transport. Bind service origin, redirect rules, request deadline, response cap & retry class in native service configuration. Migrate duplicate raw transport/retry helpers, preserving domain payloads/status mapping. Never blindly retry paid or non-idempotent operations after uncertain delivery; caller receives delivery uncertainty.

Scrape's browser-auth mechanics split across shell native WebView session access & browser artifact handling. Proposed `rightkit-browser::CookieExport` filters domains from declared profile, serializes accepted cookie format & publishes privately through fs. Host receives trusted session handle, not arbitrary renderer browser-profile path. Preserve secure/httpOnly/expiry/domain fields required by current consumer; deny empty/invalid exports without replacing old file.

Product platform/domain allowlists, destination identity & yt-dlp integration are configuration. OS browser locks, session lifetime, sensitive file cleanup & shared artifact encoding belong RK. This is not OAuth: existing OAuth flows continue using existing owner/default browser behavior. No foreign-profile secret extraction API is introduced merely for standardization.

**Accept K12:** Q1/Q2/Q4/Q6/Q8; redirect to unapproved origin, timeout after possible delivery, cancellation, oversized body; sign-in closes safely, correct cookie subset/private permissions, failed export leaves old artifact, clear invalidates engine auth snapshot.

### K13 — updates & release qualification

@rightkit/app-update owns policy/status/UI scheduling; native host owns transport/install & licence UpdateAuthProvider attaches credentials directly to native requests. Preserve manifest versus artifact authorization distinction, staged artifact identity, platform/tier & licence revision binding. Recheck revision at protected use; redirect cannot forward credentials outside compiled allowlist. Renderer never receives bearer headers.

Shared host adapter supplies `check`, `download`, `cancel`, `status`, `install` & bounded progress/status subscription. App supplies release channel/provider/product identity & copy. Use existing updater implementation/install path; this spec introduces no new updater/signing framework.

@rightkit/release adds adoption evidence validation without treating declaration scanner as runtime proof: active target/features, resolved exact version/integrity, exercised entrypoint, source-deletion list & exception entries. Verify test/control stripping for K14. Preserve build/upload separation & existing native-host publication policy. Do not revise unrelated signing infrastructure.

**Accept K13:** Q1/Q2/Q3/Q6/Q7/Q8; deactivation during download, changed staging identity, redirected auth, installer interruption, wrong platform/tier, package portability without sibling checkout, actual resolved pins & no false compliance on unused dependency.

### K14 — QA & M21 execution isolation

Retain Rust QA 0.7.1 engine test-mode binding fix. QA owns run creation & launcher binding; fs owns `test_mode::{Request,Decision,ValidatedRoot}`; daemon Suite owns root/service namespace propagation; release owns production exclusion proof. M21 becomes an enforced row with these explicit responsibilities, not another package.

Proposed shared `ExecutionContext` in daemon carries app identity, production/test mode, validated root, run identity & permitted QA features. QA supplies validated context at launch. Engine/helper inherits same bound context; app cannot reinterpret a loosely checked environment Boolean. Production compilation excludes QA admission, mock entitlements, dev approvals & control registrations. Production secret/device/licence namespaces cannot resolve from QA context.

Shared QA invoke/control registration consumes command descriptors & caller policy. Scenarios, expected product results & fixture data remain app inputs. Fixtures must not require production credentials or mutate real data. No generic raw-command escape hatch just for tests.

**Accept K14:** Q2/Q6/Q7/Q8; correct QA build/run starts engine & exercises a real command; wrong/missing root binding, foreign app ID, symlink/reparse escape, production root & mismatched helper context refuse before effect. Inspect production feature graph/artifact plus negative runtime probes. A scan for strings alone is insufficient. Remove app test-mode parsers & duplicate control transport after these tests.

### K15 — agent clients versus hosted services

Keep rightkit-agent-tools Toolbox/ToolAdmission for client-side tools & common request exposure/schema validation. [Existing admission](D:/Claude/rightkit/crates/rightkit-agent-tools/src/toolbox.rs:1). K07 provides hosted service transport & approval state; neither role substitutes for the other.

Pulse/Mail hosted agent methods use shared transport/admission/approval/audit mechanics. App supplies method descriptors, account/resource authorization facts & actual operations. CodeRight tools use published Toolbox where contracts match; existing policy must remain fail-closed. Do not enable a new `desktop.*` authority merely to pass M22.

An app with no hosted agent service has no requirement to invent one. Applicability records distinguish `client`, `host`, `both`, `absent`, tied to real entrypoints. No arbitrary blanket N/A text. Host methods & client tools must each use their assigned owner where present.

**Accept K15:** Q2/Q5/Q8; forbidden exposure, invalid args, denied approval & revoked account cause no effect; admitted call settles once with structured outcome; errors do not expose secrets. No host passes solely because it depends on client toolbox.

### K16 — RightKitSwift parity & optional native modules

Qualify existing 0.9.0 candidate products before proposing replacements: RightKitSystem supervised children; Store AppSettingsStore; EngineClient/Engine; HTTP; Update; Account; FS/DB/Credentials; Overlay/Hub. [Package products](D:/Claude/rightkit/swift/RightKitSwift/Package.swift:11).

Mirror semantic contracts of K01–K15 only where a named Swift consumer needs them. Sharing permits one RK implementation per native runtime; it does not justify per-app copies. Bind exact remote version tag, preserve installed namespaces/file protection/keychain attributes & protocol vectors. No local package path or new four-language codegen system.

Move generic Pulse Swift hub/supervised service/helper transport & Heard helper credential/CloudKit hosting mechanics into matching existing products. Retain CloudKit container/zone/record schema & product merge rules as configuration/domain handlers; AppKit/CloudKit alone does not justify app-owned infrastructure. This is targeted desktop/helper adoption, not an unratified full Apple mandatory profile.

**Accept K16:** Q1/Q2/Q3/Q4/Q6/Q8 on native macOS; fresh remote package resolution, helper protocol compatibility, supervision/cleanup, settings protection & account scope changes. Apple-specific checks cannot be replaced by Windows compilation.

## 6. Concrete source-to-deletion map

Paths are source anchors, not whole-file deletion instructions. Delete indicated mechanics; preserve behavior in owner & product data/actions at callsite. References capture inspected revision; rebase evidence if concurrent edits change them.

| ID | Source anchor / existing behavior | Owner contract & removable mechanics | App remainder |
|---|---|---|---|
| P1 | [Pulse state_root](D:/Claude/pulse/core/src/store.rs:128), [migrated_app_root](D:/Claude/pulse/core/src/store.rs:269) | K07 root resolution/cutover; wire all readers together | Product area names/legacy paths |
| P2 | [Pulse read/patch storage](D:/Claude/pulse/core/src/store.rs:166) | K04 read policy/Document host composition; existing adoption preserved | Schemas, 16 MiB limit, BOM/read-only policy as config |
| P3 | [Pulse resident engine](D:/Claude/pulse/engine/src/resident.rs:13) | K06/K07 lifecycle/hosting; already uses owners | Service/method definitions |
| P4 | [Pulse approval signatures](D:/Claude/pulse/core/src/remote/approval_sig.rs:1) | K07 verification/replay/registry/approval mechanics; retain P-256 wire profile | Action digest inputs, trust policy configuration |
| P5 | [Pulse device-link hosting](D:/Claude/pulse/engine/src/devicelink_host.rs:1762) | K07 transport/session lifecycle; extract generic pairing mechanics | Product device actions/message schema |
| P6 | [Pulse Swift hub dependency](D:/Claude/pulse/mac/Hub/Package.swift:23) | K16 shared hub/store/supervisor products | Views, assets, command/data schema |
| H1 | [Heard supervisor](D:/Claude/heardright/tauri-app-next/heardright-app-runtime/src/engine_supervisor/supervisor.rs:412) | K06 compose existing EngineSupervisor; remove repeated host transport/replay | Speech routing/BYOK choices as data/actions |
| H2 | [Heard key rotation](D:/Claude/heardright/tauri-app-next/src-tauri/src/history/key_rotation.rs:24) | K03 stage/rekey/promote/rollback/open recovery | History schema & credential identity |
| H3 | [Heard credential store](D:/Claude/heardright/tauri-app-next/src-tauri/src/credential_store.rs:53) | K03 existing Store adoption; remove generic wrappers/cache/rotation | Exact namespace & policy |
| H4 | [Heard lease client](D:/Claude/heardright/tauri-app-next/src-tauri/src/license/lease.rs:245) | K05 activation/renewal/link/remote tombstone workflow | App/service identifiers & display data |
| H5 | [Heard account session](D:/Claude/heardright/tauri-app-next/src-tauri/src/account_session.rs:221) | K05 passwordless/session/device operations | Backend business policy & consent copy |
| H6 | [Heard watchdog](D:/Claude/heardright/tauri-app-next/src-tauri/src/ui_stall_watchdog.rs:36) | K11 ping/episode/cooldown/capture orchestration | Configured timings & permitted fields |
| H7 | [Heard window host](D:/Claude/heardright/tauri-app-next/src-tauri/src/hub_window.rs:420) | K08 window/focus/monitor clamp/overlay lifecycle | Pill/hub content & geometry values |
| H8 | [Heard CloudKit helper](D:/Claude/heardright/tauri-app-next/src-tauri/src/cloud_sync_cloudkit.rs:181) | K16 shared helper transport/hosting/account-scoping mechanics | Container/record schema & merge behavior |
| V1 | [View licence native lifecycle](D:/Claude/viewright/src-tauri/src/commands/license.rs:124) | K05 clock/storage/refresh/lifecycle orchestration | Feature/update requirements |
| V2 | [View renderer licence](D:/Claude/viewright/src/state/updateLicense.ts:190), [headers](D:/Claude/viewright/src/state/updateLicense.ts:387) | K05/K13 token storage/refresh/header creation | Safe DTO rendering & actions |
| V3 | [View document save](D:/Claude/viewright/src-tauri/src/documents.rs:511) | K01 keyed lock/atomic commit/conflict mechanism | Encoder & expected source fingerprint |
| V4 | [View PDF adapter](D:/Claude/viewright/src-tauri/src/commands/pdf_adapter.rs:106) | K08 shared command registration/auth over existing rightkit-pdf | Domain request construction |
| V5 | [View PDFKit host](D:/Claude/viewright/src-tauri/src/mac_pdf_view.rs:103) | K08 optional native view/find/print/lifetime adapter | View-specific presentation inputs |
| V6 | [View preview protocol](D:/Claude/viewright/src-tauri/src/preview_protocol.rs:75) | K08 shared PreviewStore/scheme/budgets/revocation | Content-kind/CSP policy & HTML producer |
| V7 | [View iframe inspector](D:/Claude/viewright/src-tauri/src/html_devtools.rs:1) | Accepted EX1; no general production devtools extraction | Iframe inspect interaction only |
| V8 | [View logs](D:/Claude/viewright/src-tauri/src/logs.rs:1) | K11 shared recording/collection/commands | Diagnostic fields & enabled preference |
| M1 | [Mail relocation](D:/Claude/mailright/src-tauri/src/db/data_relocation.rs:94) | K01/K03/K07 copy/verify/state/cutover/housekeeping mechanics | Companion list, schema validation, legacy layout |
| M2 | [Mail shell supervisor](D:/Claude/mailright/src-tauri/src/core_ipc/shell_mode.rs:58) | K06 preserve existing supervise_adopted; reduce common start/stop orchestration | mailright-core executable/methods |
| M3 | [Mail approval gate](D:/Claude/mailright/src-tauri/src/agent/gate.rs:145), [decision audit](D:/Claude/mailright/src-tauri/src/agent/gate.rs:229) | K07/K15 admission/pending/decision/audit/settlement workflow | Send/forward/delete/attach domain action & account policy |
| M4 | [Mail SqlSettings](D:/Claude/mailright/src-tauri/src/db/accounts_labels.rs:6) | K04 existing adoption; no extra file store | Table/schema & transaction context |
| M5 | [Mail licence commands](D:/Claude/mailright/src-tauri/src/commands/license.rs:27) | K04/K05 replace raw record get/set/delete with safe native actions | Product entitlement configuration |
| M6 | [Mail QA registration](D:/Claude/mailright/src-tauri/src/tauri_run.rs:109) | K14 shared compile-gated registration/isolation | App scenarios & launch mode |
| S1 | [Scrape supervisor](D:/Claude/scraperight/src-tauri/src/engine_supervisor.rs:230), [snapshots](D:/Claude/scraperight/src-tauri/src/engine_supervisor.rs:420) | K06 existing supervisor; share repeated host/snapshot composition | Init/settings/auth schemas |
| S2 | [Scrape sign-in export](D:/Claude/scraperight/src-tauri/src/inapp_sign_in.rs:27) | K12/K08 browser session/filter/serialization/private artifact lifecycle | Platform/domain allowlist & destination identity |
| S3 | [Scrape settings](D:/Claude/scraperight/src-tauri/src/commands.rs:14) | K04/K06 existing FileBackend; share validation/save/snapshot sequencing | Product validation & asset-scope actions |
| S4 | [Scrape window surface](D:/Claude/scraperight/src-tauri/src/window_surface.rs:13) | K08 size/center/reveal/fallback workflow | 960×640 setup & 1200×760 main configuration |
| S5 | [Scrape onboarding](D:/Claude/scraperight/src-tauri/src/commands_models.rs:253) | K10 persistence/navigation; K06 pending model jobs | Selected models & result data |
| S6 | [Scrape native licence](D:/Claude/scraperight/src-tauri/src/license.rs:58), [Pro bridge](D:/Claude/scraperight/src-tauri/src/pro_bridge.rs:328) | K05 consolidate already-native verification/store/bridge/lease workflows | Endpoint/profile data & feature use |
| S7 | [Scrape file actions](D:/Claude/scraperight/src-tauri/src/commands_files.rs:248) | K01 publication; K08 safe shared open/reveal | Transcript/media encoding & destination choice |
| C1 | [Code licence store](D:/Claude/coderight/apps/coderight-tauri/src-tauri/src/license.rs:35), [renderer client](D:/Claude/coderight/apps/coderight-tauri/src/state/updateLicense.ts:103) | K04/K05 remove raw storage commands/client mapping/refresh | Compiled app identity/feature requirements |
| C2 | [Code daemon launch/adopt](D:/Claude/coderight/apps/coderight-tauri/src-tauri/src/lib.rs:661), [spawn](D:/Claude/coderight/apps/coderight-tauri/src-tauri/src/lib.rs:942) | K02/K06 ownership/heartbeat/adoption/presence/cleanup | Domain protocol descriptor/resources/actions |
| C3 | [Code daemon lifecycle facade](D:/Claude/coderight/apps/coderight-tauri/src-tauri/src/daemon_lifecycle.rs:1) | K06 migrate actual desktop-daemon-client implementation, not just re-export file | Engine/domain semantics |
| C4 | [Code logs](D:/Claude/coderight/apps/coderight-tauri/src-tauri/src/logs.rs:1), [RK rotation](D:/Claude/coderight/apps/coderight-tauri/src-tauri/src/lib.rs:1243) | K11 common adapters; preserve existing rotation adoption | Event fields & configured retention |
| C5 | [Code token store](D:/Claude/coderight/apps/coderight-tauri/src-tauri/src/token_store.rs:22), [Pro bridge](D:/Claude/coderight/apps/coderight-tauri/src-tauri/src/pro_bridge.rs:127) | K03/K05 secure store/lease transport mechanics after callsite qualification | Provider schema & account consent policy |

This map names initial deletion units, not a quantified claim of all removable code. Each migration expands its unit to all callers/copies before closure. Discovery of another implementation matching an existing contract adds a deletion target; it does not create an automatic app exception or new package feature.

## 7. Retained implementation & exception rules

Normal app inputs need no “exception”: product schemas/migrations, command/effect implementations, encoders, views/assets, IDs, routes & explicit policy values. They must not embed infrastructure state machines under a callback name.

| ID | Retained implementation | Evidence / reason | Boundary & removal condition |
|---|---|---|---|
| EX1 | ViewRight iframe inspector interaction | Explicit user exception; html_devtools entrypoint targets content inspection | Only iframe interaction/inspection UI. Shared host security, preview storage & ordinary window mechanics still migrate. Revisit if another app needs identical inspector behavior. |
| EX2 | Temporary legacy record/wire adapters during migration | Existing installed data, licence namespaces & authenticated protocols | Prefer adapter inside RK. App adapter allowed only if package cannot yet express exact format; record symbols, parity test & target package fix. Delete after that published fix & migration verification. |
| EX3 | Temporary patched third-party host dependency | HeardRight vendored Tauri Arc issue is dependency compatibility, not product uniqueness | Identify exact upstream defect/patch & replacement version; retain only until qualified dependency resolves it. No blanket native-host exemption. |

EX2/EX3 are exception **classes requiring concrete entries**, not preapproval for unspecified app files. Any actual residual entry records app/platform, symbols/callers, observable behavior, failed shared option, smallest missing RK capability, parity evidence & removal condition. Missing evidence blocks that migration's completion. There is no permitted “all PDF/CloudKit/speech/driver code is product-specific” entry.

## 8. Publication & migration sequence

| Stage | Work | Dependency / exit |
|---|---|---|
| A | Freeze source/function inventory per capability; qualify device/QA fixes, child capture/native stdio & fs lease correction | Fix regressions observed through public interfaces; do not bundle unrelated candidate changes |
| B | Native licence/account/update boundary + caller-gated host registration | Credentials/fs foundation; verify secret isolation before removing old flow |
| C | Data/root/DB transaction composition & recovery | K01/K03/K07 failure cases; retain old state until reopen proof |
| D | Engine/session/approval composition, shell/overlay/native adapters | Published foundations & actual consumer parity; independent slices may proceed concurrently |
| E | UI/onboarding/settings/logging/QA adapters & Swift parity | Each slice owns named app deletion targets; no dependency on unrelated feature completion |
| F | Publish each qualified minimal slice, exact-pin consumers, migrate callers, delete duplicates, run target behavior checks | Registry/tag bytes match qualified source; six-app row matrix & exception entries updated |

Stages describe dependencies, not one long serial queue. One repository integration owner controls edits to HEAD/index/builds/publication; independent source work can proceed in disjoint capability lanes. No app depends on an unpublished sibling checkout to make migration appear complete.

For package P, required order is: qualify packaged P with its real consumer contract → publish P → verify fetched bytes → exact-pin & resolve consumer → integrate/delete → verify installed/runtime behavior. Candidate qualification may use isolated packaging fixtures; released app dependencies must resolve published versions. Publication version assignment is mechanical readback, not permission to widen feature scope.

## 9. Definition of complete & comparison checklist

Each applicable `(app, M row, platform)` record contains:

- Owner package/API & enabled target/features.
- Exact published version, lock resolution/integrity or immutable Swift tag.
- Real entrypoint/caller → owner call path.
- Before/after functions deleted, including duplicate wrappers/state machines.
- Observable parity/failure evidence from appropriate native host.
- Remaining product inputs & any function-level temporary exception.

Reject completion if unused dependency, disabled feature, old parallel code path, unsupported blanket N/A, unqualified candidate, missing native test, or generic callback still contains extracted workflow. “Absent capability” needs entrypoint/config evidence; no invented engine/server to satisfy a checklist.

**Independent comparison should reconcile:** owner placement; exact API inputs/outputs; error/partial-commit behavior; data/credential trust boundaries; named caller deletions; justified remainders; supported native modes; minimal publication slices. Compare those contracts, not document length or claimed LOC savings.

## 10. Baseline evidence hashes

SHA-256 of selected working-tree sources read for this revision. These distinguish inspected candidates from HEAD & do not certify source behavior.

| Source under RightKit | SHA-256 |
|---|---|
| crates/rightkit-license/src/client/mod.rs | `e891426b47d5a9a84f7c0ebd7cc23598d128bb21d36fde3aab8c53b374ff3030` |
| crates/rightkit-daemon/src/app_root.rs | `f004d5f4751656d26cfa0ea3e1d327f90429aa3b569d4d82388785239887d35a` |
| crates/rightkit-fs/src/lease.rs | `4f6e9fa82be0d23dea86b85b336b0581eb4418c388b2bdf51dd721bf21925de9` |
| crates/rightkit-tauri-shell/src/commands.rs | `401faf94a9d0dd939a4303abf4df5f13664524d5187bd55dab8f35e4fcf621b6` |
| packages/release/ownership-mandatory-set.json | `f111edbdcf3aa43c36d96bada9558b72a6b50a662f8d873eb207e51d5dc99f34` |
| swift/RightKitSwift/Package.swift | `cda13ac8ddd264afb066829534daf6d18ad5c558acaf99136315eb62abb486bc` |

## 11. Scope rejected or deferred

No new universal host crate; no new event bus; no rewrite of already-adopted supervisors/backup/rotation; no general renderer secret bridge; no silent breakaway addition; no weakening of lease locking; no whole-app production devtools; no generic transaction DSL; no automatic effect replay or exactly-once claim; no framework/codegen without duplicate schema evidence; no mass publication of staged work; no global UI redesign; no new desktop-control authority; no broad product-domain exemptions.

Potential dead code/plaintext/test-bypass findings from earlier review remain separate verification leads unless a live call path is established. This specification requires safe replacement behavior without presenting those leads as proven production incidents.

**Deliverable:** package contracts K01–K16, complete M1–M22 ownership allocation, concrete six-app deletion anchors, exception criteria, acceptance scenarios & incremental publication/migration rules, ready for independent comparison.
