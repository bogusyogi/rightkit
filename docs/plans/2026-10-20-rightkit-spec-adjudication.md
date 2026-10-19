# RightKit package spec — Astra/Fable adjudication

Date: 2026-10-20. Status: **joint agreed target specification; D1–D22 resolved by Astra & Fable**. Scope: Pulse, HeardRight, ViewRight, MailRight, ScrapeRight & CodeRight; M1–M23. This document records target architecture & acceptance requirements. It does not claim unpublished APIs ship, activate enforcement, or certify app migration.

## 1. Inputs & authority

Adrian requested independent specs, then authorized discussion: “Find the proper way to adjudicate. Pull the evidence. Argue it. Best evidence wins.” His objective is shared packages with minimal separate code, supported by concrete evidence for every exception. ViewRight's iframe inspector is his named exception.

Originals remain intact:

- [Astra r1](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-m1-m22-package-spec-astra-r1.md), SHA-256 `751010245b136a0d627170083c6804fd869c30f6ea008db1b12164080d3b43af`.
- [Fable draft](D:/agent-cache/tmp/claude/D--Claude-rightkit/9caa0c4c-aaf1-46c4-970c-70f11e7c4a3c/scratchpad/rk-locked-spec.md), SHA-256 `cd00ad300f2dea95e0d23ebd1c4cb86eb585da6e92e56482435358c228dbf1c3`.

RightKit inspected HEAD: `5a5892a9e4f07b17e985ee93ffe20a54ff78178c`. Working tree contains concurrent candidate changes; HEAD does not identify those bytes. Six app hashes & deletion anchors remain in Astra §§2/6. Package archives under `D:/scratch/rk-pub/` establish inspected published exports. Registry observations are dated baseline evidence, not current publication qualification.

Fable's [archive surface diff](D:/agent-cache/tmp/claude/D--Claude-rightkit/9caa0c4c-aaf1-46c4-970c-70f11e7c4a3c/scratchpad/archive-surface-diff.md) compares declaration signatures across 16 crates. It reports zero archive-only signature heads & candidate additions; this lexical method does not prove API compatibility or cover enum variants, fields, full signatures, visibility/reexports or behavior. Original surface-m* notes came from working-tree extraction & are not shipped-surface authority. Re-derive any definitive API lock from packaged source/tag, including these omitted dimensions.

Astra subsequently fetched all three archives absent from that comparison & verified registry checksums/integrity. Relevant shipped declarations were inspected:

| Package | Archive SHA-256 | Evidence / inspected result |
|---|---|---|
| rightkit-qa 0.7.1 | `dfc649079275c505dabef3e2edf291d33d37d5fb1cc08d5c5586e32d4d3e8dc7` | [Registry checksum receipt](D:/scratch/rk-spec-adjudication-20261020/rightkit-qa-0.7.1.evidence.json); [actual module exports](D:/scratch/rk-spec-adjudication-20261020/rightkit-qa-0.7.1/rightkit-qa-0.7.1/src/lib.rs:34). Candidate dialog/fake_cli modules remain absent. |
| @rightkit/legal-ui 0.4.1 | `e234d9e6bfe3051c1edbc1dce4e959281cc3bcc2c64ce3332fd3fff12ac05362` | [Integrity receipt](D:/scratch/rk-spec-adjudication-20261020/rightkit-legal-ui-0.4.1.evidence.json); [contained layout declaration](D:/scratch/rk-spec-adjudication-20261020/rightkit-legal-ui-0.4.1/package/dist/LegalGate.d.ts:69). |
| @rightkit/onboarding 0.6.2 | `736dd2d60d7e7ff3cc1299b101243d9d3dd9915d2df95ddda8c6a37f73ccafd0` | [Integrity receipt](D:/scratch/rk-spec-adjudication-20261020/rightkit-onboarding-0.6.2.evidence.json); [existing step exports](D:/scratch/rk-spec-adjudication-20261020/rightkit-onboarding-0.6.2/package/dist/index.d.ts:2); [contained layout](D:/scratch/rk-spec-adjudication-20261020/rightkit-onboarding-0.6.2/package/dist/types.d.ts:181). |

Adjudication separates three questions:

1. **What must happen?** Adrian's current instructions & settled decisions govern intent. Tools, metadata & either agent's preference cannot silently expand authority.
2. **What exists?** Exact published bytes/tag prove shipped API; exact source & tests prove candidate behavior. Registry entries locate releases; they do not turn dirty source into shipped code.
3. **What should change?** Choose smallest shared implementation preserving demonstrated behavior. Require named consumers, deletion targets & decisive acceptance conditions. Missing evidence becomes bounded verification work, not invented fact.

[Issue #771](https://github.com/bogusyogi/rightkit/issues/771) was fetched during adjudication. Its D2 records DevTools prohibition in all app builds except ViewRight's content inspector; D6 records private RightKit local builds. These are separate from unrelated documents' reused “D4” labels.

**Document precedence:** resolved amendments here override conflicting text in both originals. Fable's row structure organizes obligations; Astra's K01–K16 semantics, errors, deletion targets & acceptance remain required target contracts, subject to amendments here. “Required owner work” means necessary before affected app migration can complete, not optional backlog. Public-surface prose is a guide, never permission to remove unlisted published APIs.

## 2. Shared decisions

- Each applicable concern has one RK implementation. Apps retain schema, product data, declarative configuration, actual domain actions & evidenced exceptions. Repeated workflow orchestration belongs in RK even when individual primitives already exist.
- Capability applicability follows actual product behavior & host stack. No new engine, account, service or platform port is required merely to populate every row. Swift hosts receive shared parity; trusted React chrome uses shared React primitives. Untrusted embedded content gains no host privileges.
- Apps consume exact published versions with integrity/tag evidence. Keep fixes. Remove unnecessary unpublished additions; repair published mistakes through a newer release. Do not solve scope reduction by downgrading pins or rewriting published bytes.
- Candidate API names remain provisional. Ownership, observable behavior, errors, trust boundaries & acceptance requirements are frozen design targets. Publication requires separate evidence.
- Adoption is complete only when production callers reach published owners, former mechanics are deleted, behavior passes relevant acceptance, & residual implementation has function-level justification. Imports, pins & declaration checks alone never prove adoption.
- Qualification follows affected platforms/features & existing required gates. Security/data transitions require refusal/recovery evidence. Preserve existing explicitly authorized Fable/Astra/RK3 release division & sign-off; this spec adds no mandatory Oracle pass per diff. Adrian's current supplied AGENTS preamble says: “Oracle is optional for explicit review requests or concrete outcome/safety risks, never required for every reply.” Existing valid measurements may be reused; stale README prose alone does not prove missing evidence.
- No DevTools in any app build except ViewRight's content-only inspector. QA, mocks, test mode & dev-control implementations are absent from production artifacts; a runtime flag is insufficient.

## 3. Disagreement register

D1–D14 originated in Fable's response. D15–D22 are Astra's additional challenges. **All resolved:** Fable explicitly agreed to this record & amendments in receipt `3860fef6-1660-4588-9fd0-7d595703cf8f`; Astra accepted final source-level D22 correction after checking cited code.

| ID | Disagreement | Agreed resolution after evidence | Evidence |
|---|---|---|---|
| D1 | M23 omitted vs included | Include explicit release-switch row. Existing scanner proves implementation; conflicting “ratified/unratified” metadata proves inconsistency, not user approval. | [M23 pending row](D:/Claude/rightkit/packages/release/ownership-mandatory-set.json:403), [registry owner](D:/Claude/rightkit/packages/release/release-switch-registry.json:3), [release integration](D:/Claude/rightkit/packages/release/release.mjs:394) |
| D2 | Baseline versions | Tables agree. Correct mandatory minimums to device 0.3.1, QA 0.7.1, legal-ui 0.4.1, onboarding 0.6.2 when implementing approved spec; qualify remaining candidates before release. | Originals' version tables; [version ledger](D:/Claude/rightkit/packages/release/rightkit-versions.json:1) |
| D3 | Row contracts vs K contracts | M rows organize contract; K semantics & acceptance remain normative target requirements. Separate published availability from required extension work. | [Astra notation](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-m1-m22-package-spec-astra-r1.md:25), Fable §5 |
| D4 | New crash watchdog vs existing shell detector | Reuse shell watchdog; extend only missing HeardRight behavior. Crash owns capture adapter. No second detector loop. | [Published WatchdogConfig](D:/scratch/rk-pub/crates-m/rightkit-tauri-shell-0.4.0/src/watchdog.rs:46), [callback](D:/scratch/rk-pub/crates-m/rightkit-tauri-shell-0.4.0/src/lib.rs:515), [HR behavior](D:/Claude/heardright/tauri-app-next/src-tauri/src/ui_stall_watchdog.rs:36) |
| D5 | Replace CodeRight TCP/bearer vs preserve client wire | RK daemon remote owns bounded loopback listener/auth profile; preserve bearer clients & capability links. Local pipe host remains distinct. | [Local transport](D:/Claude/rightkit/crates/rightkit-daemon/README.md:7), [route verification seam](D:/Claude/rightkit/crates/rightkit-daemon/README.md:188), [Code origin contract](D:/Claude/coderight/apps/coderight-tauri/src-tauri/src/agent_room.rs:20) |
| D6 | Native secret-entry prompt vs typed write-only host API | Retain declared API-key slot input through trusted caller + WriteIntent. No generic read/export, arbitrary ref mutation, or content-frame access. Native prompt redesign withdrawn. | [SecretsBackend](D:/Claude/rightkit/packages/settings-ui/src/types.ts:61) |
| D7 | Models omitted from root convergence | Models must consume validated daemon roots; remove independent live resolution & fallback. Preserve typed Android host root. | [Models fallback](D:/Claude/rightkit/crates/rightkit-models/src/suite/paths.rs:93), [app base](D:/Claude/rightkit/crates/rightkit-models/src/suite/paths.rs:147) |
| D8 | New ExecutionContext vs existing mode API | Reuse ModeInput, ProcessInput, AppRoots, decide, init_or_exit & roots_with; extend missing immutable run propagation only. | [ModeInput](D:/Claude/rightkit/crates/rightkit-daemon/src/mode.rs:243), [AppRoots](D:/Claude/rightkit/crates/rightkit-daemon/src/mode.rs:304) |
| D9 | Delete Pulse root helper immediately vs wire migration | Implement shared coordinated cutover, switch every consumer, then delete app helper with replacement. | [Unwired migration helper](D:/Claude/pulse/core/src/store.rs:269) |
| D10 | App lock in credentials vs shell | Credentials owns native OS authentication primitive; shell/Swift host owns lock state/lifecycle coordination. | Astra K08; Fable M11; non-Tauri host requirement |
| D11 | HeardRight vendored Tauri patch | Temporary third-party compatibility exception only after identifying actual patch; sunset at qualified upstream replacement. Never RK path-dependency exemption. | [Exception requirements](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-m1-m22-package-spec-astra-r1.md:407) |
| D12 | Proposed names treated as shipped API | Freeze semantics/errors/acceptance; reconcile names to existing exports. Published lock excludes unpublished symbols. | Astra §1.2; Fable L1/L4 |
| D13 | PDF, cookies & overlays omitted from inventory | Required owner work where named app behavior needs it. Owner inventory omission is not permission to retain generic app mechanics. | Astra K08/K12/K16 & deletion ledger |
| D14 | Missing host registration, receipt creation & evidence | Shared caller-gated composition is required. Native receipt creation owns timestamp/digest. Verify actual missing measurement against required budget; no blanket review ceremony. | [Handler](D:/Claude/rightkit/crates/rightkit-tauri-shell/src/lib.rs:1272), [logs registration currently manual](D:/Claude/rightkit/crates/rightkit-tauri-shell/src/logs_commands.rs:12), [renderer receipt](D:/Claude/rightkit/packages/onboarding/src/Onboarding.tsx:224) |
| D15 | Rotation order allowed in apps | Shared DB lifecycle owns complete transition, exclusion & recovery; credentials owns protected key storage. Preserve viable keys across partial promotion & legacy staging. | [Promotion then cleanup](D:/Claude/rightkit/crates/rightkit-credentials/src/rotation.rs:339), [HR legacy slot](D:/Claude/heardright/tauri-app-next/src-tauri/src/credential_store.rs:283), Astra K03 |
| D16 | All apps must use Tauri/React | Apply shared behaviors through actual native/Tauri/React host. No forced stack conversion, no privileged host in content iframe, no blanket native exemption. | Astra scope/K16; Fable M1/M2; #771 D2 |
| D17 | Working-tree exports labeled published | Correct 0.4.0 lists: preview & webview2_menu absent from inspected published lib. Verify other locked-surface claims against packaged source; omission never authorizes API removal. | [Published export list](D:/scratch/rk-pub/crates-m/rightkit-tauri-shell-0.4.0/src/lib.rs:18), Fable M1 item 2 |
| D18 | Remove forwarding wrappers; add duplicate steps; use transaction everywhere | Keep delegating settings wrappers; reuse permissionsStep/modelsStep; View save uses keyed lock + existing checked atomic writer. Transaction only for real grouped mutations. | [FS delegation](D:/Claude/rightkit/crates/rightkit-app-settings/src/atomic.rs:59), [existing steps](D:/Claude/rightkit/packages/onboarding/src/steps.tsx:52), [single-file save](D:/Claude/viewright/src-tauri/src/documents.rs:520) |
| D19 | CI assumed for private RightKit | Follow actual repository/native-host release rules. No spec-induced CI policy change; relevant configured gates remain required. | #771 D6, fetched during adjudication |
| D20 | Generic approvals/adoption erase trust & identity | Preserve Pulse cryptographic profile, Mail admission/audit policy & Code owned-vs-attached lifecycle. Existing PrivateChild adoption does not establish arbitrary resident attachment. | [Pulse signature contract](D:/Claude/pulse/core/src/remote/approval_sig.rs:3), [PrivateChild signature](D:/Claude/rightkit/crates/rightkit-daemon/src/supervisor.rs:296), Astra K06/K07 |
| D21 | Speculative Secret<T>/chunking & downloader composition | Require demonstrated payload/limit gap before new secret surface; existing Secret already redacts. Compose HTTP stream with fs atomic publication, not path-copy API as network input. | [Secret](D:/Claude/rightkit/crates/rightkit-credentials/src/secret.rs:4), Fable M11/M17 |
| D22 | HeardRight test is_pro default called production bypass | Source-level unguarded env fallback is a confirmed release-selector defect. Shipped-binary bypass/reachability remains unproven; no production incident claim. Native authorizer replaces fallback. | [Unguarded selector](D:/Claude/heardright/tauri-app-next/heardright-text-pipeline/src/settings.rs:318), [worker caller](D:/Claude/heardright/tauri-app-next/heardright-runtime/src/runtime/finalize_transcript.rs:568), [shell derivation](D:/Claude/heardright/tauri-app-next/src-tauri/src/runtime/rpc.rs:13) |

## 4. Required contract amendments

### A. Database, credentials, files & root cutover

`rightkit-db` owns full encrypted lifecycle: open/key selection, rotation, interruption recovery, restore & relocation. Apps supply schema, verification callback, companion paths & domain-worker stop/resume notification. Apps do not orchestrate key promotion or transaction phases.

One cross-process transition guard covers whole workflow with pinned database & credential-store identity. Drain conflicting readers/writers/pools; acquire in bounded time; return Busy before mutation if quiescence fails. Guard remains until all in-flight backend effects settle. Timeout cannot expose store while earlier operation can still mutate it. Restore/relocation preserve exclusion across old/new root identities. Credential-call locking alone is insufficient.

Promotion is not binary success/failure: primary write precedes marker cleanup. Preserve old key in protected durable storage until readback resolves Committed, CommittedCleanupPending, NotCommitted or Indeterminate. Never blindly roll back after cleanup failure. Recovery preserves sole viable key; recognize HeardRight's `SQLCipher/DBKey.staging` alongside RK rotation marker. Unknown key/data combinations report recovery required without destructive quarantine or deletion.

`rightkit-fs` owns safe publication, lease fix & keyed write coordination. Existing delegating settings wrappers may remain. Single-file saves compose keyed lock with AtomicWriter/commit_checked. Multi-file journal/rollback covers real grouped changes only; it does not promise externally atomic visibility across unrelated files. Preserve journal/staged/backup identities & original data until verified commit/reopen.

`rightkit-daemon` owns root cutover. Resolve one immutable root context & pass it to settings, credentials, models, DB, logs, services & children. No independent fallback to real user data after mode/root resolution fails. Pulse migration helper disappears when shared cutover replaces it. Fix migration marker handling, empty-directory links & platform link/socket policy before migration release qualifies.

### B. Hosts, caller authorization & native parity

Tauri shell provides one composition entry for shared settings, logs, legal receipts, licence/account/device, models, crash & updates. Generic app handlers remain domain actions only. Registered commands enforce caller/window/origin/capability before effects; content frames cannot reach privileged services. Exact configuration & policy remain data.

API-key input is write-only through app-declared slots, trusted settings caller & explicit WriteIntent. No generic get/export or arbitrary credential namespace mutation. Licence/account/session/update secrets remain native; renderer receives safe views & typed actions. Legacy getInstallHeaders/token bridges are migration targets, never compliant target APIs.

Shared legal receipt creation validates declared legal document set/digest & eligibility, records native time, stores receipt & handles versioned reacceptance/reopen/reset. UI communicates assent; native timestamp alone is not proof of human identity. Keep published contained layout & existing permissionsStep/modelsStep; extend shared adapters instead of adding synonyms.

Native/Swift hosts use equivalent shared behavior. Existing RightKitSwift System, Store, EngineClient, Engine, Update & Account candidate modules are starting points, not wholly new packages. No demand to turn native Swift UI into Tauri/React. ViewRight iframe inspector alone remains app-specific; preview token store, authorization, windowing & PDF host mechanics remain shareable.

Credentials owns OS-authentication primitives without Tauri dependency. Shell/Swift host owns application lock state, visibility & lifecycle. Unsupported platforms return typed Unsupported; no success substitute.

### C. Services, engines & approvals

Reuse EngineSupervisor, Dispatcher, sidecar framing, daemon Service/supervisor & child-process ownership. Shared composition owns readiness, hello/version checks, presence, refcount/idle exit, restart generations, bounded shutdown & capture. Apps supply method schemas, spawn data, policy & actual work.

Owned child, attached resident service & foreign adopted process are distinct states. Permission to attach never implies permission to kill. Revalidate process/service identity, generation & health; failure must not terminate unrelated replacement process. Current supervise_adopted(PrivateChild) alone does not implement arbitrary resident adoption.

CodeRight's existing loopback HTTP/bearer clients remain supported by daemon remote profile. Owner supplies bounded parser/listener, loopback-only binding, capability/auth verification & explicit browser-origin/CSRF rules. Public-route seam still requires verification. Do not substitute RK-Device signing on existing clients. Preserve private pipe transport semantics separately. Current [remote feature gate](D:/Claude/rightkit/packages/release/release-switch-scan.mjs:84) remains fail-closed: desktop apps do not set remoteHost=true to bypass it. Shared loopback implementation requires a separable feature, narrowly scoped scanner rule & qualification; reuse router/auth internals without introducing another transport crate.

Shared approval machinery owns request binding, expiry, replay prevention, audit-before-success & recovery. Preserve Pulse P-256/Secure Enclave device+key profile, exact action/request binding, nonce rules, expiry/skew & separate PIN authority. Preserve Mail caller policy & audit behavior. A generic approved boolean cannot replace these guarantees. App action catalogue remains domain data/actions. CodeRight desktop authority is not implicitly granted by package migration.

### D. Watchdog, logging, HTTP & models

Reuse tauri-shell detector & on_stall callback. Extend missing HeardRight semantics: monotonic elapsed timing, one outstanding ping, observer-gap suppression, recovery cancellation, cooldown, one capture in flight & bounded shutdown. Crash owns native capture/report/retention adapter. No second UiWatchdog scheduler in crash.

Host composition registers logs commands once. Reuse existing JsonlStore, RotatingTextWriter, capture & redaction; extend demonstrated retention/bundle/consent behavior. Review secret-wrapper/chunking additions against actual consumer payloads & backend constraints before committing public API.

HTTP download uses existing HTTP/model download facilities plus fs atomic stream publication & digest verification. Failure preserves old destination. Browser cookie export preserves required fields, validates declared scope, writes privately & rejects empty/invalid replacement. Native host session access remains caller-gated; renderer cannot provide arbitrary browser-profile path.

### E. Execution mode, release switches & qualification

Daemon mode's ModeInput/ProcessInput/AppRoots/decide/init_or_exit/roots_with remain shared vocabulary. QA creates isolated context; daemon propagates immutable selected mode/root/run identity; fs validates paths; credentials & DB follow declared production/test policy; models consumes same root. `.rightkit-app.json` is runner bookkeeping, not authentication proof.

M23 owns release-switch inventory & artifact checks through @rightkit/release. Classify production journal/credential guard controls separately from forbidden test selectors; do not strip security guards under a blanket “all env reads banned” rule. Address source, feature closure & actual packed payload. State unsupported inspection formats precisely; artifact qualification must cover shipped contents through an applicable scan/extraction path.

Resolve M21/M23 ownerPending metadata during implementation, using this target assignment. Labels saying ratified cannot authorize activation. No new gate or policy file is activated by this discussion.

Required evidence: relevant native feature/platform checks; upgrade compatibility; negative caller/secret tests; interruption at durable phases; bounded process shutdown/output failure; test-root isolation; release absence of QA/bypass code; actual app-call routing & deletion. Reuse valid receipts for unchanged behavior. Version record & package checksum accompany publication.

HeardRight has an unguarded HEARDRIGHT_ENGINE_TEST_MODE fallback in text-pipeline/settings.rs:318–327 with worker callers. Record release-source defect; do not claim installed-binary bypass without feature/artifact/reachability evidence. Error names remain precise: [release-switch-manifest-missing](D:/Claude/rightkit/packages/release/release-switch-scan.mjs:121) concerns Cargo manifest; [missing source switch inventory](D:/Claude/rightkit/packages/release/release.mjs:450) concerns switches.json.

## 5. Final row map

Each row uses Fable's requirement/surface/obligations/permitted-code/exception/evidence structure with amendments above. Listed target work is required where corresponding consumer behavior exists; none is represented as published merely by this table.

| Row | Shared owner | Required target / completion focus |
|---|---|---|
| M1 Window host | tauri-shell, device, @tauri; RightKitSwift native host | Caller-gated shared service registration; native parity; generic preview/window/PDF/overlay mechanics; VR content inspector exception only. K04/K08/K16. |
| M2 App frame | react-shell, platform-ui, theme; native shared counterparts | Trusted chrome primitives, theme/settings/menu/layout contracts; domain pages remain app data/composition. K09. |
| M3 Engine supervision | job-queue + daemon + child-process | Shared host lifecycle with owned/attached distinction, handshake, presence & bounded recovery. K02/K06. |
| M4 Window/CLI protocol | job-queue + sidecar; daemon service transport | Shared framing/dispatch/errors/typed events & replay; preserve required existing external wire profiles. K06. |
| M5 Other processes | child-process | Preserve capture/Windows fixes; qualify configured & streaming runners; explicit evidenced breakaway only. K02. |
| M6 Data location | daemon, fs, db; models consumer | Immutable roots, complete migration/cutover, protected recovery, no independent fallback. K01/K03/K07. |
| M7 Local services | daemon | Shared discovery/admission/approvals; Code loopback profile; preserve signature & audit policies. K07. |
| M8 File writes | fs | Lease fix, keyed saves, checked atomic publication & bounded grouped recovery only where required. K01. |
| M9 Database | db + credentials | Whole encrypted lifecycle & transition exclusion, legacy-key recovery, restore/relocate. K03. |
| M10 Settings | app-settings, settings-ui, host composition | Native stores, typed write-only secret slots, shared sections/adapters, configuration only in apps. K04/K09. |
| M11 Secrets | credentials | Protected storage/rotation primitives & OS authentication; no app orchestration or generic renderer export. K03/K05. |
| M12 Licence/device | license + device; renderer view/action client | Native activation/account/lease/Pro bridge, no raw licence secret DTOs. K05. |
| M13 Entitlement | native license authorizers & engine readers | Method-level enforcement; renderer hints not authority; feature/callgraph evidence for bypass claims. K05. |
| M14 Legal/onboarding | legal, legal-ui, onboarding + native host | Native receipt lifecycle & shared step adapters; preserve contained layout & existing step names. K10. |
| M15 Logging | logs + @logs + host composition | Shared commands, retention/redaction/bundle lifecycle; app event/schema data only. K11. |
| M16 Crash | crash + shell watchdog | Existing detector extended for HR parity; native capture/report adapter & shutdown. K11. |
| M17 HTTP | http + host; browser for cookie artifact | Redirect constraints, verified downloads, cookie artifact publication; safe native transports. K12. |
| M18 Updates | app-update + native updater + license | Native authorized update transport/cache; remove raw token/header bridge; preserve user workflow. K13. |
| M19 Release | @release | Shared recipes for signing/staging/helpers/assets/notices; current repository distribution & native-host rules. K13. |
| M20 QA | Rust/npm QA + qa-chrome | Shared harness/control/capture; domain fixtures/steps remain app data; required installed-platform evidence. K14. |
| M21 Test mode | daemon mode + fs + QA; credentials/db/models consumers | One immutable validated context, isolated storage/children, no production bypass. K14/K07. |
| M22 Agent surface | agent-tools client; daemon host/admission | Client toolbox distinct from hosted authority; shared approval mechanics preserve app trust profile. K15/K07. |
| M23 Release switches | @release scanner/registry + daemon mode | Source/feature/payload evidence; classified security controls; reconcile pending-owner metadata. |

## 6. Delivery order & exceptions

1. Correct published-surface inventory & settle exact package bytes; preserve already adopted build/test fixes & contained layouts.
2. Finish safety-critical owner work: file/pipe fixes, root convergence, DB/key recovery, caller-gated native credentials/licensing/update transport, process identity.
3. Finish shared workflow/UI/native parity & release recipes needed by named deletion consumers. Prefer existing APIs over new wrappers/types.
4. Qualify each capability on affected native hosts, publish new version when required, then move affected apps to exact latest version.
5. Delete replaced app mechanics, execute observable workflow & refusal/recovery cases, record actual retained functions with evidence. Update row enforcement only against qualified published owners.

Exception record must name function/callers, behavior, unique product need, failed shared option, smallest missing RK feature, evidence & sunset/review condition. Platform specificity, one current consumer, thin glue or historical format alone do not qualify. Missing shared capability is temporary owner work. Actual third-party Tauri patch may remain until qualified upstream replacement; unknown patch files are not preapproved. Existing compatibility remains until replacement succeeds.

## 7. Adjudication receipts

- Fable original request: `e6fd4177-e992-4166-82fd-2b0e82e2063c`; Adrian authorization verified using Pulse chat said.
- Astra draft + authority correction sent: `ce0e17c2-baa2-454b-a270-d9cf61bf9803`.
- Fable D1–D14 response: `78dbc281-8539-4d6a-b1f1-f9611abb675e`.
- Astra D1–D22 response: `74830e6a-cbe1-4b07-8d2c-47a370f0d9eb`.
- Fable accepted set, corrections & D22 source evidence: `8a68300d-e87c-4790-be3b-5f80e62eb702`.
- Fable archive comparison: `bf806d74-4df7-4d03-adc6-b62e64760abf`.
- Astra final precision & D5 gate consequence: `821187c3-4c77-4fb0-a55e-559310c63341`.
- Fable read record & explicitly agreed D1–D22, including final D5/D14/D22 wording: `3860fef6-1660-4588-9fd0-7d595703cf8f`.

This turn writes this Markdown record only. Product source, dependency pins, registry publication & enforcement activation remain separate implementation work.
