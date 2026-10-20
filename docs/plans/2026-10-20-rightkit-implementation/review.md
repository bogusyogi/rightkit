# Implementation-packet adversarial review

Date: 2026-10-20. Status: **jointly settled & frozen; final Astra high verdict ready** after rechecking all seven corrected packets. Scope: seven packets under [index](README.md); signed M1–M23 spec remains unchanged. This records planning review, not qualification/publication results.

## Astra challenges to Fable's package packet

| ID | Challenged proposal | Evidence & required resolution | Disposition |
|---|---|---|---|
| RV01 | Publish staged candidates first, new owner work second; all apps wait | Signed spec §2/§6 & Astra r1 §11 require scoped capability increments. Combine required compatible candidate work; intermediate bugfix only if named consumer independently benefits. Each AP task waits only for its PK subset. | Fable accepted, Pulse `ec69767e-b1f6-40db-ae99-e0d9864bbd3c`; final text re-read & accepted |
| RV02 | Publish release tool first as global bootstrap; reserve versions | `packages/release/publish-cargo.mjs` invokes publication/crate gates; app production-mode/frontend gates live in `release.mjs`. Prove real dependency before imposing bootstrap. Choose versions after registry/compatibility check. | Fable accepted, same receipt; final text re-read & accepted |
| RV03 | Apply old runtime-budget deferral to new safety/security workflows | `ci/runtime-budget-deferred.json` reason covers P-g renamed-dependency republish. No extension to new contracts; qualify relevant features/hosts before marking PK satisfied. | Fable accepted, same receipt; final text re-read & accepted |
| RV04 | Serial arrows imply nonexistent dependency edges; naïve DB→credentials cycle | Cargo manifests show fs/child independent & npm logs/license independent. DB→credentials→app-settings→DB would cycle. DB owns workflow over protected-store trait; credentials adapter depends toward DB. Recompute final graph including optional/target/peer dependencies. | Fable accepted, same receipt; final text re-read & accepted |
| RV05 | Automatically publish staged sandbox/automation features | Signed §2 requires proven consumer/owner gap. Existing satisfying published dependency stays; unrelated additions held. | Fable accepted, same receipt; final text re-read & accepted |
| RV06 | Fix DB runtime-WAL-refusal empty-file leftover by cleanup | `crates/rightkit-db/CHANGELOG.md:7` deliberately preserves requested file/sidecars because another connection could have committed data. Remove invented cleanup repair; preserve retention regression. | Fable accepted & PK03-2 corrected; Pulse `ffc0ba04-c619-41de-aa2a-257b65912482` |
| RV07 | Treat every broken pipe as capture error; assume stdio fix unreachable | `crates/rightkit-child-process/src/run.rs` documents normal early-closed stdin; `native_capture/windows.rs:199,222` treats output pipe closure as EOF. Propagate actual non-EOF errors & test each selected spawn path. | Fable accepted & PK02-1 corrected; same receipt |
| RV08 | Publish unused unpublished APIs unchanged | PK01/PK02 draft contradicted signed §2. Preserve published APIs; remove unnecessary unpublished additions unless named consumer requires them. | Fable accepted; same receipt; PK09 consistency checked |
| RV09 | Cycle prevention contradicted by PK03 text & reciprocal root tasks | DB defines protected-store/transition interfaces; credentials implements adapter; daemon composes DB participant. No DB dependency on credentials or daemon. | Final text re-read & accepted; receipt `277dd929-5e4a-4667-9acf-c7409b995c65` |
| RV10 | Dependency lanes mislabeled independent; dry-run called registry absence proof | Preserve real internal edges. Fresh registry lookup precedes version selection; dry-run alone is not absence evidence. | Final text re-read & accepted; receipt `277dd929-5e4a-4667-9acf-c7409b995c65` |
| RV11 | Owner metadata could become effective before qualification | Signed §6 requires qualified owner before row enforcement. Prepare metadata; bind activation to receipts & authenticated trust-sync. | Final text re-read & accepted; receipt `277dd929-5e4a-4667-9acf-c7409b995c65` |
| RV12 | Manifest example has incorrect AP dependencies, single-host proof & missing delivery pointer | Use cumulative immutable snapshots, per-target evidence, publisher identity, registry readback, selected snapshot hash & all PK/AP dispositions. | Final text re-read & accepted; receipt `277dd929-5e4a-4667-9acf-c7409b995c65` |
| RV13 | Release recipes remain prose; blanket workspace gates added | Named app release helpers need concrete owner task; run configured/new consumer feature sets, not invented universal workspace gate. | Same receipt; root added exact M19 caller closures to apps |
| RV14 | Unused additions/automatic intermediate releases or generic Swift tests could qualify prematurely | Require named consumer & independent benefit; qualify affected native features. Publication dispatch supplies requested go. Preserve exact Swift tag exception & OS-auth UI. | Final text re-read & accepted; receipts `277dd929-5e4a-4667-9acf-c7409b995c65` & `f2d51608-c8ac-4a51-a50f-f48b568848ef` |

## Astra challenges to app drafts

| ID | Finding | Required correction | Disposition |
|---|---|---|---|
| AV01 | Pulse/HR historical RK HEAD presented as current; AP prerequisite lists disagree | Separate historical spec inspection from current HEAD; align task and map dependencies; use actual app hashes | Corrected |
| AV02 | HR task says reuse local detector despite target shared detector | Use published shell detector; local implementation supplies parity baseline, then disappears | Corrected |
| AV03 | View engine/protocol/Swift requirements inferred without caller proof | Map actual engine behavior; ordinary Tauri invoke is not sidecar protocol; Rust PDFKit need not adopt Swift | Corrected |
| AV04 | N/A or required rows inferred only from original anchor list | Inspect current manifests/callers; absence from initial anchors is no proof. No new capability just to fill rows | Corrected; genuinely unresolved applicability remains explicit inventory work |
| AV05 | SR/CR tasks cover only original anchors; remaining rows lack concrete work | Add actual paths/symbols, PK prerequisites & acceptance for every applicable row; remove repetitive checklist filler | Corrected; 16 SR & 14 CR tasks |
| AV06 | Broad platform-adapter retention could preserve duplicated workflow | Apply signed function-level exception criteria; missing shared parity remains owner work | Corrected |
| AV07 | Inconsistent future manifest fields & whole-app readiness gates | One schema in 01 §6, immutable snapshots, per-AP readiness | App references normalized; final schema re-read & accepted |
| AV08 | View roots/metadata DB & HR production root resolvers had no task | `app_paths.rs`/`persistence.rs`; HR telemetry/text-pipeline/model-store source. Add AP-VR-13 & AP-HR-18 with refusal/recovery cases. | Corrected; Oracle re-read & accepted |
| AV09 | SR/CR existing iOS consumers omitted by desktop-focused inspection | `scraperight/ios-native/project.yml`; `coderight/apps/coderight-ios/project.yml`, native stores/licence/purchases/cloud callers. Add AP-SR-16 & AP-CR-14. | Corrected; Oracle re-read & accepted |
| AV10 | Mail audit failure incorrectly promised no effect | `agent/gate.rs:155–182` effects precede audit for relevant actions. Distinguish pre-effect refusal from applied/partial/indeterminate settlement; no blind retry. | Corrected; Oracle re-read & accepted |
| AV11 | HR blanket secret-crossing ban contradicted permitted typed write-only input | Signed D6 & `ApiKeysPanel.tsx::saveKey` preserve input through declared slots/WriteIntent; forbid native export. | Corrected; Oracle re-read & accepted |
| AV12 | SR named absent `qa:guard`; native purchase mapped to wrong PK | Use declared commands; Purchases belongs PK16, PK10 remains legal/onboarding. | Corrected; source script map checked |
| AV13 | Pulse DB task risked mutating externally owned Codex state | `core/src/budget.rs` owns budget DB; `core/src/bridge/deliver_codex.rs` reads external Codex state. Preserve read-only external access; no rekey/relocation of foreign DB. | Corrected in AP-PU-04 |

## Fable challenges to app packets

| ID | Challenge | Resolution & source evidence |
|---|---|---|
| FV01 | HeardRight release-selector task omitted third production reader | AP-HR-16 now covers command-host `delivery.rs::test_mode`, runtime `engine.rs`, text-pipeline `settings.rs` & fixture writers. Separate delivery test suppression from entitlement authorization; preserve production behavior. |
| FV02 | ViewRight M5 called conditional despite raw process launches | `src-tauri/src/lib.rs::report_fatal_startup` launches osascript, PowerShell & zenity. M5/AP-VR-11 require PK02 with bounded execution & existing fatal-alert behavior. |
| FV03 | ViewRight raw logs DB open & COM menu implementation omitted | AP-VR-13 includes `logs.rs::load_persisted_enabled`; AP-VR-08 migrates generic `context_menu.rs::install_view_source_context_item` through PK08. Read-only diagnostics semantics & iframe-only inspector/source actions remain. |
| FV04 | MailRight PK09 table consumers unnamed | AP-MR-07 names `AttachmentPreviewModal.tsx`, `SettingsAdvancedSection.tsx` & `SettingsAgentsAudit.tsx`; shared table primitive replaces mechanics while product cells/actions remain. |
| FV05 | CodeRight plaintext token fallback had no disposition | AP-CR-05 removes new plaintext writes, preserves native legacy migration until protected write/readback succeeds & refuses unavailable secure storage. Current commands lack registration/callers, so source existence is not shipped reachability. |
| FV06 | CodeRight SSH target pointed at façade | AP-CR-03 targets `crates/daemon-client/src/ssh_tunnel.rs`; top-level `src/ssh_tunnel.rs` is only a re-export. |
| FV07 | Unregistered canvas commands proposed as dead code | Initial automatic deletion rejected after finding frontend invokes & domain tests; Fable withdrew deletion, receipt `f2d51608-c8ac-4a51-a50f-f48b568848ef`. **FH35 later corrected our overclaim:** those invokes/tests do not prove functioning native routing or authorize feature activation. Current disposition below supersedes mandatory restoration. |

Fable accepted FV01–06 dispositions & FV07 correction in receipt `f2d51608-c8ac-4a51-a50f-f48b568848ef`. Astra re-read final package packet for RV01–14; accepted corrections include real dependency edges, native-host qualification, protected-store cycle avoidance, scoped release groups & manifest evidence. Fable's package changes accepted in receipt `277dd929-5e4a-4667-9acf-c7409b995c65`; tiny consistency corrections confirmed in `f2d51608-c8ac-4a51-a50f-f48b568848ef`.

## Final readback

Fresh-context **gpt-6-astra, high** reviewed all seven packets & returned [four blocking findings plus one advisory](review-astra-high.md). Original inputs are preserved under `D:/scratch/rk-packet-review-20261020/frozen/`, matching `review-input-hashes.json`.

| ID | Independent finding | Joint disposition / correction |
|---|---|---|
| AH01 | Whole-PK readiness blocks valid intermediate adoption & inherits deferred #821 | Resolved & independently rechecked. PK rollups are reporting only; AP readiness uses 65 explicit qualified capability slices with source-backed conditional applicability. #821 is separately deferred, outside required completion. |
| AH02 | Enforcement activation requires its own completed PK17 | Resolved & independently rechecked. Qualified scanner/mode/atomic-baseline receipts precede authenticated policy activation; activation cannot gate those same qualification receipts. |
| AH03 | Existing satisfying releases cannot enter publication-only manifest | Resolved & independently rechecked. Qualification-only events retain honest historical provenance & fetched-byte evidence; no gratuitous republish. |
| AH04 | HeardRight iOS workflows omitted | Resolved & independently rechecked. AP-HR-15 & PK16 cover current native licence/account/bridge callers, exact deletion boundaries & native acceptance. |
| AH05 | Workspace check can impose unrelated release prerequisites | Resolved & independently rechecked. Qualification uses affected dependency/consumer closure. R10 preserves verified standing pre-push rules; unrelated dirty work stays with its owner. |

Fable agreed AH01–04 & scoped AH05 in receipt `769cd7dd-3790-49fd-83ba-0fd6e8452c41`. Astra requested exact gate authority & rejected changing unrelated work to clear it (`865cdbb8-0c35-40a7-91a5-723cd1483ddc`). Fable froze corrected package packet in receipt `5dc55296-c0a0-4eb3-a19d-bd439c6a76d2`. Astra high independently verified all seven corrected packet hashes, rechecked affected package rules, all six AP maps & HeardRight native closure, then returned **ready**; final dispositions are appended to [its report](review-astra-high.md). Corrected hashes are in `D:/scratch/rk-packet-review-20261020/review-final-hashes.json`.

Fable high completed against original frozen inputs: [39 findings](review-fable-high.md), delivered in receipt `173670bd-a6af-4d58-a101-be9ff22c3037`. All were reconciled against corrected packets & independently rechecked by Astra high. Earlier acceptance is historical; final FH01–FH39 section in its report binds current assurance. No builds, package tests or app tests are claimed by this planning review.

## Fable high reconciliation

All findings below were checked against current corrected packets, not accepted merely because a reviewer raised them. Final changed-packet assurance is **ready**. FH01/FH09 overlap prior AH corrections; FH35 changed our earlier conclusion.

| ID | Disposition / changed requirement |
|---|---|
| FH01 | Already resolved by AH01: capability slices gate each AP; PK rollups do not. |
| FH02 | Accepted. AP-HR-19 removes entitlement fallback using published `PK05.entitlements-baseline`; first capability-ready task, independent of account work. AP-HR-16 handles mode selectors separately. |
| FH03 | Accepted. Six explicit exception-ledger reconciliation tasks cover existing M*.md plus ownership registries; stale retained statements are retired with their code deletion. Package packet names owner mappings. |
| FH04 | Accepted. PK05-7 owns staged breaking `@rightkit/tauri` change; its tests, peer ranges & named dialog consumer must qualify. |
| FH05 | Accepted. One app-neutral release consumer suffices; PK17-9 cannot demand two apps. |
| FH06 | Accepted. AP-CR-03 includes loopback server bind/parser/auth/discovery/startup mechanics, alongside client lifecycle. Preserve existing wire/product policy. |
| FH07 | Accepted with stricter immutable evidence. Bootstrap script emits schema/hash/evidence-checked snapshots; separate producer metadata; emitter later compares without rewriting original bytes. Superseding events invalidate only affected readiness. |
| FH08 | Accepted narrowly. AP tasks adopting new renderer licence client also pin qualified `PK17.release`; native-only work does not inherit this gate. |
| FH09 | AH03 already supplied `qualify-existing`; extended explicit inventory to every unchanged consumed pin. Historical publisher evidence remains honest. |
| FH10 | Accepted. PK17-4 adds actual Windows wv2-0_39 feature/check rows. |
| FH11 | Accepted. Required working-tree logs/engine gate rows have an explicit owner; unrelated pdfium changes stay outside scope. |
| FH12 | Accepted. Fetched QA 0.7.1 differs from same-number dirty source; unpublished additions require necessity decision & fresh version, never relabel-as-published. |
| FH13 | Accepted. Pulse uses actual `host_with_options`/`DispatcherHandler` callsites, not nonexistent symbols. |
| FH14 | Accepted. App-lock consumer is Pulse; HeardRight lock consumer unsupported. Actual Pulse OS-auth callers named; storage baseline alone cannot qualify OS-auth. |
| FH15 | Accepted. Pulse/HeardRight Swift source types/functions are explicit; domain callbacks/payloads do not imply whole-file deletion. |
| FH16 | Accepted. ScrapeRight `crates/store` is a plain SQLite consumer; no encryption inferred. |
| FH17 | Accepted with actual workspace path. View/Mail use existing `D:/Claude/tools/setup-workspace.py`, not nonexistent app-local copies. |
| FH18 | Accepted. CodeRight creates missing desktop switch inventory & reconciles existing M22 declaration with source-backed applicability. |
| FH19 | Accepted subject to fresh whole-repo caller/public-contract evidence. Unused SSH implementation is deletion candidate, not automatic migration work. |
| FH20 | Accepted. Pulse/HeardRight/Mail/Code production raw process sites are named; HR shell-text relaunch becomes typed executable/argv. Simple sites reuse qualified existing APIs. |
| FH21 | Accepted. AP-PU-15 explicitly covers DeviceLinkHost shared pairing/session/service mechanics; signed product authority remains data/actions. |
| FH22 | Accepted where still applicable; previous AH map already removed several over-gates. Root cutover & approvals require actual consumed slices only. |
| FH23 | Accepted. Shell optional pdf-host depends on PDF owner; never reverse. |
| FH24 | Accepted. Index lists PK04/06/08/10/11/13 shared shell-package edits. |
| FH25 | Accepted. View/Mail AP01 means identity preflight plus incremental per-adopter pinning, not completed global pin set. |
| FH26 | Accepted. Canonical Swift URL derives from verified RightKit origin; no unverified redirect claim. Correct AP IDs & current HR0.8.0 recorded; publisher updates stale ledger at readback. |
| FH27 | Accepted. Staged OAuth delta explicitly held unless required consumer justifies scoped qualification. |
| FH28 | Accepted. FS dependency of DB is required. |
| FH29 | Accepted. Undefined A2 removed. |
| FH30 | Accepted. PK17-8/9 order corrected. |
| FH31 | Accepted. Referenced app symbols, paths & line anchors corrected from source. |
| FH32 | Accepted. View `useGlobalShortcuts` dispatcher is named PK09 deletion target. |
| FH33 | Accepted. ScrapeRight adopts watchdog; no existing callback claim. |
| FH34 | Resolved with plain SQLite baseline & root/open ownership, no encrypted lifecycle gate for View diagnostics DB. |
| FH35 | **Partly accepted; automatic deletion rejected.** Our forced canvas restoration lacked runtime evidence. Preserve intended domain capability as currently unwired; migrate generic host/store without activating endpoints. APP-OS-009 proves intent, not runtime. See adjudication below. |
| FH36 | Accepted; current cookie artifact map uses PK12 + PK01. |
| FH37 | Accepted. HR watchdog maps to PK11 only. |
| FH38 | Accepted. PK17-4 adds crash default/window-errors gate rows. |
| FH39 | Accepted. Pulse M5 is required, with existing ledger plus raw spawn inventory. |

### FH35 adjudication

Root & Astra high independently re-read CodeRight source. `apps/coderight-tauri/src-tauri/src/lib.rs:1717` omits canvas commands; no managed `NativeWindowsState`, `restore_on_setup` caller or engine action route was found. `App.tsx:1168` query rendering & `NativeCanvasWindowApp.tsx:143,149` invokes establish stranded implementation; tests mock IPC. `ipc_policy.rs:292` describes inventory for future registration, not enforced reachability. Build surgery requires per-target triage, not blanket deletion; existing M1 deletion decisions concern other symbols. `docs/canon/desktop-master-canon-atoms.md:210` APP-OS-009 selects agent-defined canvas windows; its Integrated claim at298 is stale.

Root selected preservation of intended schema/actions without new feature activation. AP-CR-07/13 move/delete generic host/store duplication through qualified owners where separable, preserve unchanged registration/reachability & reconcile stale claims. Canvas acceptance proves adapter/schema parity plus no accidental activation. Any inseparable canvas dependency receives explicit pending disposition isolated from unrelated migration; it creates no permanent shared-code exception. Future feature activation is separately scoped. Astra high accepted this narrower decision & withdrew its prior live-behavior overclaim.

### Delivery authority

Adrian's 2026-10-20 01:48UTC instruction in RK Fable was verified with `pulse chat said 354b58e4-adcc-40ff-8e61-965af57807bd`: freeze settled packets, commit/push each into its own repository & file one issue per packet; then Fable executes package qualification/publication. Fable is sole integration/delivery owner across these commits; Astra verifies frozen-byte/readback receipts. No duplicate pushes. Final delivery must bind each issue to its exact packet hash, repository commit & immutable normative RK links.

## Final freeze

Fable jointly concurred with app corrections in receipt `86a6ec88-092e-4a39-a078-3e27ae91be2f`. Final independent pass found & closed AH06 (stale manifest task inventory) & AH07 (CodeRight desktop switch creation absent despite claimed correction). Source counts, exact AP-set validation & explicit switch creation now agree. Misplaced duplicate map row removed; Pulse/HeardRight ledger closure excludes self & overall signoff.

Current [Astra high assurance](review-astra-high.md#final-fh01fh39-review--2026-10-20) verifies all seven hashes, unchanged signed spec, 98 unique AP tasks, all23 M rows per app & 66 capability slices. Package packet SHA-256: `495db109dd7299c90e524e9ca96ef9401adcae9a429bc6e579a0338b8152d982`. Frozen seven-packet hash table is embedded in index. No outcome finding remains open in planning scope; execution must supply actual qualification, publication & migration evidence.
