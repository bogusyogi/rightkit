# RightKit gate contract

**Status:** target requirements & proposed acceptance plan, recorded 2026-10-28. Adrian's execution hold is: “we will get to execution once fable signs off on the contract.” Fable is Adrian's designated contract reviewer; review remains **PENDING**. Fable review is not production or publication authorization. This document does not supersede current architecture or tests, certify behavior, or authorize implementation, installation, delivery, or publication. All runtime, simulated-host, installed-host, qualification, & publication results below are **NOT RUN**. Issue: [rightkit#1254](https://github.com/bogusyogi/rightkit/issues/1254).

## 1. User requirements

These lines reproduce the user's three requirements; line breaks are normalized:

> rightkit releases from mac, dell or linux / on dell and mac, via broker / on linux no broker / but in all cases single point of entry with class a first, local compile, qualification on CI and auto publish from git
>
> private repo apps built on mac, dell or linux / on dell and mac, via broker / on linux no broker / but in all cases single point of entry with class a first, local compile, qualification held for dell or mac
>
> public repo apps built on mac, dell or linux / on dell and mac, via broker / on linux no broker / but in all cases single point of entry with class A first, local compile, qualification on CI. signed installers available

Adrian's latest correction confirms: “linux no broker, it's not that complicated.”

### Terms & fixed boundaries

- **Origin host** is Adrian's Mac, Dell, or Linux host where the shared gate runs & performs the required local compile for its supported local target. That compile proves only the source/target it actually compiled.
- **Native build target** is an artifact target declared by the product manifest. Use the origin's local compile output when it can produce the required target. Add a native build only when origin cannot produce that target; Mac/Windows final artifacts must be constructed on their native platform, never cross-compiled from Linux. RightKit/public qualification runs on CI and may qualify already sealed native bytes unchanged; private qualification runs on Mac or Dell. A private Linux target needs a compatible Linux environment hosted on Mac or Dell to qualify the exact sealed Linux bytes produced from the Linux-origin candidate; do not rebuild those bytes there. The environment runs as Linux even when hosted on Dell: the origin gate remains Linux-direct, with no broker route or spoofed host identity. That qualification environment is not selected or proven; it is an OPEN design blocker. WSL2 is only a possible environment, not a selected design or proven sole acceptance host.
- **Qualification runner** is CI for RightKit/public & Mac or Dell for private apps. Private qualification need not run on both hosts unless a target requires it. Private Linux does not acquire a mandatory GitHub CI path.
- **One entry** means one shared gate entry on each supported origin, not one executor everywhere. Adrian's native Mac & Windows gate invocations use the broker regardless of human/agent caller. Every Linux execution host always runs direct with no broker, whether standalone, WSL, VM, cloud, or hosted on Dell; RustRight on PATH and physical host do not change that route. Hosted CI retains its runner path.
- **Class A first** means agreed deterministic/mechanical checks classified as Class A by a complete command-behavior inventory run before first local compile. Current scans place some tests in B because they may indirectly spawn a compiler. The inventory must identify actual dependency/launch behavior; neither all-tests-A nor all-tests-B is presumed. Class A placement changes in either direction require dependency/launch evidence & preserve approved coverage. Compiler-backed commands such as `cargo check` & `clippy` remain B. Sage/Astra may adjudicate the inventory; Fable is the designated Oracle contract reviewer. These roles add no extra mandatory review gate. Inventory & placement remain OPEN.
- **Linux headless** means installation, gate/build/CLI use, & Pulse guest operation need no browser or interactive browser authentication. CLI token/input remains allowed. Owner-side approval may happen on Mac/Dell. Pulse's original guest use is shell/script join & messaging; [pulse#492](https://github.com/bogusyogi/pulse/issues/492) and [acceptance comment](https://github.com/bogusyogi/pulse/issues/492#issuecomment-6466661659) describe desired headless join, two-way messaging, reconnect within a valid lease, & revoked/expired denial; neither source proves a runtime result.
- Linux needs no broker or build-service prerequisite solely to invoke the shared gate. Existing app runtime services are allowed. A Linux CLI may have a package/version pin; it must not require Mac/Dell broker trust or service to run.
- Product manifests define target artifact kinds. Pulse's headless Linux bundle is required. Do not infer Linux GUI installers for every app.
- Preserve existing product signing & authentication. Native target builds are distinct provenance stages, not signing transformations. Sign once, qualify the final sealed bytes, then promote those bytes unchanged.
- The shared gate applies to managed agents & human invocations through that entry. It does not globally intercept a personal terminal outside the entry. Do not fork per-app gates or add agent bypasses.
- For local managed builds, use primary checkout, warm cache, no clone/worktree/branch/stash, & one build per host. Hosted CI uses its normal source checkout for native jobs. Keep Mac scratch at `/Volumes/D/scratch`, Dell scratch at `D:\scratch`; configure suitable durable Linux state per host policy. Temporary directories may hold disposable pack data, not durable pipeline state.
- Only RightKit explicitly requires automatic Git-triggered publication. Public apps require CI qualification & signed installers available. Latest user requirements do not specify private-app publication or public-app auto-publication; do not infer either here. Existing distribution policy remains separately applicable.

### Decision ownership & execution hold

Fable is Adrian's designated Oracle contract reviewer. Sage/Astra may design or adjudicate technical choices where needed; these role descriptions do not create extra mandatory reviewers or approval stages. Adrian retains decisions reserved to him, including production/publication authorization & trust-policy changes. The integration owner implements only already-authorized scope after Adrian's Fable-review hold is satisfied; do not add fresh approval for routine work already authorized.

| Open decision | Design / evidence owners | Reserved decision |
|---|---|---|
| Direct Linux origin trust & existing admission | Sage/Astra design if needed; Fable is designated Oracle reviewer for contract/evidence | Adrian decides any trust-policy change or production/publication authorization |
| Private Linux qualification environment | Sage/Astra map runtime OS/architecture if needed; Fable is designated Oracle reviewer for contract/evidence | Adrian decides any production scope/policy change |
| Class A inventory & placements | Sage/Astra adjudicate from actual command inventory if needed; Fable is designated Oracle reviewer | Any change beyond already-authorized scope remains with Adrian |

## 2. Origin × product matrix

All nine cells begin **NOT RUN**. Each requires a positive local compile & its required positive final-target qualification before it can pass. A private Linux candidate may have a successful negative-path WAITING result while its cell remains **BLOCKED** for completion.

| ID | Origin × product | Required origin work | Final-target qualification | Delivery/result | Status |
|---|---|---|---|---|---|
| R-M | Mac × RightKit | Installed shared entry; Class A then local compile/package through broker | CI qualifies exact sealed origin output, or native target output if origin cannot produce a required target | Git-triggered automatic publication after qualification; registry readback matches published artifact; npm & declared Cargo routes each NOT RUN | NOT RUN |
| R-D | Dell × RightKit | Installed shared entry; Class A then local compile/package through broker | CI qualifies exact sealed origin output, or native target output if origin cannot produce a required target | Same publication/readback; npm & declared Cargo routes each NOT RUN | NOT RUN |
| R-L | Linux × RightKit | Installed shared entry; Class A then direct local compile/package without broker | CI qualifies exact sealed origin output, or native target output if origin cannot produce a required target, through existing authenticated GitHub workflow | Same publication/readback; npm & declared Cargo routes each NOT RUN; no browser on Linux | NOT RUN |
| P-M | Mac × private app | Installed shared entry; Class A then local compile/package through broker | Mac or Dell qualifies each required native target | Preserve candidate/qualification linkage; publication route unspecified here | NOT RUN |
| P-D | Dell × private app | Installed shared entry; Class A then local compile/package through broker | Mac or Dell qualifies each required native target | Same private-app result | NOT RUN |
| P-L | Linux × private app | Installed shared entry; Class A then direct local compile/package without broker | Mac/Dell-hosted compatible Linux environment qualifies exact sealed Linux target bytes; Mac/Dell natively build & qualify any required Mac/Windows targets | Preserve exact origin candidate while final target awaits qualification; no package-only success | NOT RUN |
| A-M | Mac × public app | Installed shared entry; Class A then local compile/package through broker | CI qualifies sealed origin outputs where they provide required targets; native builds on target platform only for targets origin cannot produce | Signed installers available & verified on declared native hosts; publication route follows existing policy | NOT RUN |
| A-D | Dell × public app | Installed shared entry; Class A then local compile/package through broker | CI qualifies sealed origin outputs where they provide required targets; native builds on target platform only for targets origin cannot produce | Same public-app requirement | NOT RUN |
| A-L | Linux × public app | Installed shared entry; Class A then direct local compile/package without broker | CI qualifies sealed Linux output; native builds on target platform only for targets Linux cannot produce, through existing authenticated GitHub workflow | Linux has no browser/broker requirement; native installer artifacts verified on declared hosts | NOT RUN |

RightKit acceptance must report coverage by declared npm & Cargo route separately. Targeted nonpublishing route tests need not publish every package. A successful `@rightkit/release` npm run does not prove Cargo publication. Real RightKit publication requires a named candidate covered by existing user authorization for that exact candidate; this consolidation itself authorizes no publication. Fixture success never proves Git-triggered publication. Public/private app tests cover manifest-declared targets, not every imaginable platform artifact.

## 3. Current evidence & open decisions

### Snapshot facts

Source observations below are a snapshot, not a review of current dirty changes. [fe81b3ec](https://github.com/bogusyogi/rightkit/commit/fe81b3ec032ede62ea106101c5705a6eaa569462) is a comparison commit, not current HEAD. [f6c2ca45](https://github.com/bogusyogi/rightkit/commit/f6c2ca4587c6252367ba1790c3c5c3c25bd41b2d) is proposal source inspected read-only, not merged behavior. Current checkout HEAD at documentation review was local hash `03c1111f` (two local commits ahead of origin; not linked as a remote commit). The working tree had seven uncommitted gate-source files—`packages/build-control-policy/src/index.mjs`, `packages/build-control-policy/src/index.test.mjs`, `packages/release/app-check.mjs`, `packages/release/bin/rightkit-gate.mjs`, `packages/release/door-policy.mjs`, `packages/release/door-policy.test.mjs`, & `packages/release/rightkit-gate.test.mjs`—plus unrelated Cargo/manifests & QA edits. These changes were not reviewed by this snapshot. No remote delivery is claimed.

| Observation | Evidence & implication |
|---|---|
| Admission currently depends on broker evidence. | Main snapshot [unit-gate.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/packages/release/unit-gate.mjs#L58), [publish-admission.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/scripts/ci/publish-admission.mjs#L117), & [rightkit-dispatch.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/scripts/ci/rightkit-dispatch.mjs#L97) require broker admission/signature. [host-gate.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/packages/build-control-policy/src/host-gate.mjs#L29) rejects Linux as a managed launcher host; this is not proof that a portable shared entry cannot run there. |
| Direct Linux publication trust is unresolved. | GitHub caller/auth identity alone is not evidence of local compile. Current admission guards must remain until Fable reviews a replacement/trust model. Define how direct origin result, source/stage/digest/run/attempt, workflow, & artifact are correlated; include adversarial rejection cases. Do not add a custom signing or anti-forgery system, self-admission path, or CI-only shortcut. |
| Current executor may route Linux through RustRight when found on PATH. | Snapshot [gate-executor.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/packages/release/gate-executor.mjs#L15). This conflicts with user's explicit Linux-direct requirement and is a proposed behavior change, not achieved behavior. |
| Linux route proposal in rightkit#1253 conflicts with explicit Linux-direct requirement. | [rightkit#1253](https://github.com/bogusyogi/rightkit/issues/1253) describes a Linux broker/config/pin proposal; reconciliation comments [6467016714](https://github.com/bogusyogi/rightkit/issues/1253#issuecomment-6467016714) & [6467079521](https://github.com/bogusyogi/rightkit/issues/1253#issuecomment-6467079521) record the human no-broker requirement. Adrian's latest correction makes routing categorical: Linux is direct on WSL/VM/cloud/Dell-hosted Linux. The proposal does not create an exception; implementation/closure is unverified. |
| Paths & canary are host-specific. | [rk-pipeline.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/scripts/ci/rk-pipeline.mjs#L22) contains fixed host paths. [rk-release-canary.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/scripts/ci/rk-release-canary.mjs#L384) runs a Darwin leg locally & a Dell leg remotely, reading app SHAs from a Mac workspace. |
| Packaging has native helper constraints. | [local-publish.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/packages/release/local-publish.mjs#L414) blocks Linux/Windows npm local publish; [swap-helper-bundle.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/packages/release/swap-helper-bundle.mjs#L59) requires a signed Darwin helper/receipt off Mac. Choose compatible existing helper/archive mechanics; no cross-compilation or custom signer. |
| Scan classification needs inventory-based reconciliation. | Snapshot [deterministic-scans.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/packages/release/deterministic-scans.mjs#L58) maps npm/CI tests to B & its test asserts current placement. Do not label all such tests wrong or move them solely by name. Inventory exact behavior first; proposed Class A semantics above require evidenced reconciliation & Fable signoff. |
| Adapter source/config does not establish installed capability. | [rightkit#1241](https://github.com/bogusyogi/rightkit/issues/1241) is open: attested npm unit release cannot produce compile/package broker receipts; report cites recursive-`-r` & pack-argv mismatch. [RustRight #174](https://github.com/bogusyogi/rustright/issues/174) owns adapter argv/env/resource/receipt/native-proof work. Active capability, compatible versions, & actual jobs on both hosts remain unverified. Do not claim adapter absence or print secrets. |
| Some interface proposals are not established. | The proposal omitted `gate:release`, consistent with release running only through trusted publisher [rightkit-gate.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/packages/release/bin/rightkit-gate.mjs#L1430). Do not add local release entry or invented CLI flags. Phase 0 must discover installed entry/resume/fixture/evidence commands & outputs. |

### Fable review disposition

| Finding | Disposition |
|---|---|
| Contract cannot override tested current architecture before signoff. | Accepted: target-state only; current implementation/tests stay evidence, not waived. |
| Linux direct publication must not reopen self-admission. | OPEN: preserve current admission controls pending reviewed trust design. |
| Origin compile may not produce every final target. | Accepted: qualify sealed origin output when it provides target; add native final build only for targets it cannot produce, then qualify & promote final bytes. |
| P-L needs actual Linux final-target qualifier. | OPEN: name/prove compatible Mac/Dell-hosted Linux environment. |
| Class A wording risks presuming tests must move. | Accepted: inventory command behavior first; preserve approved coverage & require dependency/launch evidence for classification changes in either direction. |
| G23, invented adopt/install commands, & missing release script. | Accepted: remove invented interfaces/allocations; discover actual interface in Phase 0. |
| Cross-repo ownership/evidence claims. | Accepted: RustRight #173 broker contract, #91 migration, #174 adapter work; RightKit #1241 receipt issue & #1254 canary/native/provenance; Pulse #457 join, #503 guest trust, #492 headless guest proposal & acceptance comment. Pulse #499 closed on 2026-10-27; its title describes a historically held native journey, not an active task or this E12 proof. Issues are scope owners, not runtime proof. |

## 4. Candidate, native artifact, & qualification binding

This is a required evidence relationship, not a proposed new cryptographic protocol. Use existing authenticated storage/admission where adequate. Record at minimum:

1. Origin source SHA & tree state; host/target/architecture/toolchain; installed gate/broker versions; entry & stage; command result.
2. Local compile output digests & candidate identity, kept separate from final artifact identity.
3. If origin cannot produce a required target, record additional native target-build host, source input, output digest, helper identity/digest, & stage result. If no additional build occurs, carry sealed origin output forward unchanged. Record any allowed assembly/signing transformation; do not count qualification as compilation.
4. Final sealed artifact digest; qualification run, host, checks & result tied to those exact final bytes; publication promotes unchanged qualified bytes.
5. For RightKit/public CI, preserve existing authenticated GitHub workflow identity & source/stage/digest/run/attempt correlation. For private apps, qualification evidence comes from Mac/Dell, not a newly required GitHub CI path.

An origin candidate and final signed artifact need not have equal hashes. Their relationship must be traceable & verifiable. Qualification of an earlier candidate does not qualify a later sealed artifact. A receipt stored only in the tree it attests is invalid because recording it changes tree identity.

## 5. Test & acceptance layers

All layers are **NOT RUN**. First discover current commands & output fields; names below identify semantic outcomes, not verified CLI syntax. If no existing command implements required behavior, the shared owner proposes an interface for signoff.

| Layer | Required design/evidence | Status |
|---|---|---|
| Contract inventory | Cover nine IDs, hosts/executors, stages, qualification hosts, publication scope, RightKit npm/Cargo routes, & manifest targets. Proposed binding test must detect actual source rule IDs, scan prefixes, stage names, entry scripts, & host-policy drift; behavioral tests must verify routing/order/side effects, not only duplicated constants. | NOT RUN |
| Simulated behavior | Inject host identity, platform, PATH, environment, & broker availability. Assert route, Class A order, reached/refused stages, side effects, & reason for each ID. Negative cases must distinguish “no launch attempted” from launch denied. | NOT RUN |
| Installed-host runs | Use installed shared entry on Adrian's Mac, Dell, & standalone/cloud Linux. Record actual local compile output & candidate; do not substitute source inspection, fixtures, queued metadata, clone/worktree run, or CI result for origin compile. Linux proof must not rely only on WSL2 or Mac mounts. | NOT RUN |

Run outcomes live apart from requirement data. Keep durable state through process & host restart. Use existing authenticated CI path only for RightKit/public. Private qualification remains Mac/Dell. No synthetic broker request or fixture proves a real publish.

## 6. End-to-end acceptance

Each case starts **NOT RUN**. E1's Class A set is the complete inventory, not representative rows. E4's wait case is a negative-path check; P-L also needs positive resume & final-target qualification.

| ID | Given / when | Pass evidence |
|---|---|---|
| E1 Class A | Reconcile complete agreed inventory to executed installed-gate checks; include red Class A cases. | Every Class A finishes before first local compile. A red Class A starts zero compilation & zero later-stage work. Classification changes in either direction require dependency/launch evidence; names/duration alone are insufficient. |
| E2 Managed broker | Mac/Dell managed host, broker unavailable, invoke shared entry. | Refuses before compile with no direct fallback. |
| E3 Linux direct | Standalone/cloud Linux, RustRight absent then present on PATH, invoke installed entry. | Actual local compile succeeds direct; trace shows zero attempted browser/broker launches & no Mac path/mount dependency. Record final-target qualification separately; record an additional native build only if needed. WSL2 may be additional evidence, not sole proof. |
| E4 Private Linux hold/resume | On standalone/cloud Linux, compile/package candidate; request qualification without Mac/Dell result; later supply matching Mac/Dell qualification. For a Linux target, execute qualification on exact sealed Linux bytes in a compatible Linux runtime hosted by Mac/Dell; record runtime OS/architecture separately from physical host. | Pending state preserves exact candidate; no package-only pass or redundant Linux rebuild. Positive resume qualifies final target bytes and unblocks P-L only on matching result. Hosting Linux on Dell does not route compilation through Windows broker. |
| E5 CI fail & RightKit publish | Exercise each declared npm/Cargo route with nonpublishing fixtures; inject failed/incomplete CI; separately name an authorized real RightKit candidate. | Failure never promotes. Actual Git-triggered publish follows qualification; registry download hash matches published final artifact. Fixture cannot pass publication. |
| E6 Altered/stale binding | Alter source/tree, local output, gate version, helper, native output, final artifact, app SHA, run/attempt. | Existing auth remains; mismatches reject before promotion. No new signature system is implied. |
| E7 Candidate to final | Seal origin output when it provides required target; add native build only for a required target origin cannot produce, then package/sign/seal as applicable. | Evidence binds origin output and, only when present, additional native output/helper; records transformations, final digest, exact final-byte qualification, & unchanged promotion. No redundant compile is required. |
| E8 Canary placement | Run canary for each origin with required app SHAs. | Required legs complete independent of origin. Missing private qualification remains pending/blocked, not weaker success. |
| E9 Retry/restart | Interrupt after dispatch and after publication; resume same candidate after process & host restart. | State reconciles without duplicate dispatch/publish; ambiguous runs remain blocked pending reconciliation. |
| E10 Entry scope | Human & managed agent invoke shared gate on Mac/Dell; compare personal terminal outside entry. | Both gate callers route through broker & same ordering; outside-entry terminal is not globally intercepted. |
| E11 Native installer/targets | Use manifest-declared public/private required targets; verify/install/update/run on corresponding native host. | Signatures & hashes identify exact final artifact; CI qualifies public; Mac/Dell qualifies private. Do not infer undeclared GUI target. |
| E12 Pulse headless guest | Actual standalone/cloud Linux guest; owner-side approval on Mac/Dell; join, send/receive, disconnect/reconnect within lease; test revoked/expired denial. | Actual two-way message & reconnect; expired/revoked denial; zero attempted Linux browser/broker launches. Track under [pulse#492 comment 6466661659](https://github.com/bogusyogi/pulse/issues/492#issuecomment-6466661659). Issue text/comment supports requested scope, not success evidence. |

## 7. Failure patterns to prevent

| Failure | Guard |
|---|---|
| Broker/path selection silently changes Linux route or excludes Linux. | Select by explicit host identity; test RustRight absent/present. Preserve Mac/Dell broker routing. |
| Class A coverage or placement changes without dependency/launch evidence. | Inventory actual behavior; preserve approved coverage; require evidence for changes in either direction. |
| Adapter existence, source change, cache, or metadata is called a successful compile. | Verify installed active adapter & compatible versions, inherited environment, then complete actual broker compile/package on both Mac & Dell. |
| Mac-specific path/coordinator blocks other origin. | Resolve host tools & durable state from configuration; retain host scratch policy. |
| Synthetic admission or GitHub identity is treated as local compile attestation. | Preserve existing admission guards pending signoff; correlate full source/stage/digest/run/attempt & native final artifact. |
| Build starts while material safety/evidence finding remains unresolved. | Block on outcome/safety defects & required evidence; do not add generic fresh approval gates. |
| Pending refusal discards candidate or receipt mutates source identity. | E4 proves retention/resume; keep results outside attested tree. |
| Assistant/hook text expands authority or changes requirements. | Human requirements govern scope; hooks may deny but cannot grant authority. |
| Personal terminal is globally intercepted. | Apply managed-agent policy at shared entry; do not claim global interception. |
| Working tree is conflated with remote delivery. | Working-tree cleanliness, ahead/behind status, & publication are separate; report each from evidence. |

## 8. Corrective sequence

Execution remains on Adrian's hold until Fable signs off on the contract. Fable is the designated Oracle contract reviewer; Sage/Astra may adjudicate open technical design. This adds no separate review gate. Phase 0 may record baseline & blockers. Implementation/execution phases remain deferred until the human hold is satisfied. One integration owner controls primary-checkout evidence; builds remain one per host.

| Phase | Owner | Work | Exit evidence |
|---|---|---|---|
| 0. Baseline & review | Integration owner; Sage/Astra design if needed; Fable contract review | Record current HEAD/tree, installed versions, entry/resume/nonpublishing-fixture/evidence-read commands, & output fields. Resolve direct-origin trust, private Linux target environment, and native target mapping; otherwise record each as an explicit blocker with owner. Keep secrets out of evidence. | Record Fable review outcome, technical decisions, blockers & interface inventory. If unresolved, stop; execution remains on Adrian's hold until Fable signs off. |
| 1. Acceptance tests | Gate owner | Add nine-cell contract coverage & behavioral tests; inventory actual source rule IDs, scan prefixes, stages, entry scripts, host policy, and route coverage. Record known failures before changing behavior. | Missing IDs/drift & wrong behavior detected; no acceptance expectations weakened to fit current code. |
| 2. Class A inventory | Gate owner; Sage/Astra adjudication if needed; Fable contract review | Reconcile each agreed deterministic/mechanical check with observed command behavior. Decide placement from dependency/launch evidence in either direction; preserve approved coverage. | Complete inventory-to-execution evidence; red Class A has zero compile/post-Class-A launches. |
| 3. Shared entry & executor | Shared gate owner | Implement one installed entry with explicit Mac/Dell broker & Linux direct routing; preserve hosted native CI distinction and managed-agent scope. Interface names are TBD by Phase 0. | Simulated routes/order/refusals; no invented CLI flags or G23 allocation. |
| 4. Broker adapter capability | RustRight owner, [#174](https://github.com/bogusyogi/rustright/issues/174), coordinated with [RightKit #1241](https://github.com/bogusyogi/rightkit/issues/1241); [#173](https://github.com/bogusyogi/rustright/issues/173) is broker contract & [#91](https://github.com/bogusyogi/rustright/issues/91) migration | Reconcile required pnpm/node argv, inherited env, gate/broker versions, resources, receipts, & active installed adapter on both Mac & Dell using existing broker machinery. | Actual installed broker compile/package jobs complete on both hosts; config presence alone is insufficient. |
| 5. Portable pipeline/state | Pipeline owner | Replace hardcoded host assumptions with configured tools/scratch; persist state durably on each host. | Origin-independent coordinator; state survives restart; retries reconcile same candidate. |
| 6. Artifact handling & admission | Release/CI owner; Sage/Astra design if needed; Fable contract review | Resolve trusted origin-to-workflow result without weakening existing admission or inventing custom crypto. Qualify sealed origin output unchanged when it provides required target; otherwise build only the missing target natively. Record additional native output/helper only if that build occurs, plus transformations, final digest, qualification & promotion. | Fable review outcome & technical decision recorded; stale evidence rejects; exact qualified bytes promote unchanged; no redundant build. |
| 7. Qualification/canary | Qualification owner | Qualify RightKit/public on CI; private on Mac/Dell. Use sealed origin output when it provides required target. Select compatible Linux environment hosted by Mac/Dell to execute/qualify exact sealed private Linux artifact; do not rebuild it there. Build Mac/Windows targets natively only when origin cannot produce them. | Actual positive final-target qualification for each required target and matching source/artifact record. |
| 8. Install & E2E delivery | Integration owner | Install shared gate on Mac/Dell/Linux; run all layers, nine cells, npm/Cargo declared routes, native installers, and Pulse headless case. Publish only a named RightKit candidate already covered by existing exact-candidate authorization. | All positive cell evidence, actual RightKit Git publication/readback, public CI qualification/signed installers, private Mac/Dell qualification, headless Linux proof. |

## 9. Completion record

Record each ID as **NOT RUN**, **PASS**, **FAIL**, or **BLOCKED**, with source/tree, installed version, origin/target/host, candidate/final digests, run reference, & result. No overall completion while any required cell is pending. In particular, private Linux WAITING is a valid negative-path result but P-L stays **BLOCKED** until positive final-target qualification & resume.

Completion requires all nine positive cells; complete Class A reconciliation; actual broker compile/package jobs on both Mac & Dell; standalone/cloud Linux headless local compile; additional native target builds only where origin cannot produce a required target; reviewed origin-result trust; exact final-byte qualification; actual authorized RightKit Git publication & matching registry readback; public CI qualification & signed declared installers; private Mac/Dell qualification for each declared target; declared npm/Cargo route coverage; & Pulse headless join/two-way/reconnect proof. No source-only, fixture, simulated host, queued metadata, or waiting result substitutes for these outcomes.

## 10. Provenance

This document consolidates requirements & a proposed recovery plan. Current-source comparison: [fe81b3ec](https://github.com/bogusyogi/rightkit/commit/fe81b3ec032ede62ea106101c5705a6eaa569462); proposal snapshot: [f6c2ca45](https://github.com/bogusyogi/rightkit/commit/f6c2ca4587c6252367ba1790c3c5c3c25bd41b2d). The local checkout hash at documentation review was `03c1111f` (two commits ahead of origin; not a remote link), with unreviewed dirty source as listed above. Reconciliation references: [rightkit#1253](https://github.com/bogusyogi/rightkit/issues/1253) & its [comment 6467016714](https://github.com/bogusyogi/rightkit/issues/1253#issuecomment-6467016714); [rightkit#1241](https://github.com/bogusyogi/rightkit/issues/1241); [RustRight #174](https://github.com/bogusyogi/rustright/issues/174), [RustRight #173](https://github.com/bogusyogi/rustright/issues/173), [RustRight #91](https://github.com/bogusyogi/rustright/issues/91); [Pulse #457](https://github.com/bogusyogi/pulse/issues/457), [Pulse #503](https://github.com/bogusyogi/pulse/issues/503), [Pulse #492](https://github.com/bogusyogi/pulse/issues/492), & [Pulse #499](https://github.com/bogusyogi/pulse/issues/499).
