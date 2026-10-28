# RightKit gate contract

**Status:** requirements & proposed acceptance contract, written 2026-10-28. This is the canonical source for requested gate behavior. Runtime, simulated-host, installation, qualification, & publication evidence remains **NOT RUN**. Consolidating this document does not certify or change runtime behavior.

User authorization: “alright, go ahead with the consolidation please”. Existing authorization governs exact-candidate delivery; this document adds no scope. Issue: [rightkit#1254](https://github.com/bogusyogi/rightkit/issues/1254).

## 1. User requirements

The following reproduces the three requirements from the request; line breaks are normalized:

> rightkit releases from mac, dell or linux / on dell and mac, via broker / on linux no broker / but in all cases single point of entry with class a first, local compile, qualification on CI and auto publish from git
>
> private repo apps built on mac, dell or linux / on dell and mac, via broker / on linux no broker / but in all cases single point of entry with class a first, local compile, qualification held for dell or mac
>
> public repo apps built on mac, dell or linux / on dell and mac, via broker / on linux no broker / but in all cases single point of entry with class A first, local compile, qualification on CI. signed installers available

### Definitions

- **Origin** is where shared gate & local compile run: Adrian’s Mac, Dell, or standalone/cloud Linux.
- **Qualification host** is CI for RightKit/public & Mac or Dell for private apps.
- **Class A first** means every agreed deterministic/mechanical check, including tests whose evidenced behavior allows them to run before compilation, completes before the first local compile. A test may run later only when its behavior demonstrably requires newly built artifacts or depends on compilation; record evidence for each such mapping. Tests are not Class B merely because they execute project code, are named “test”, or take time.
- **One installed entry** means the same user-facing shared gate on each origin. When invoked on Adrian’s Mac or Dell, it always selects the RustRight broker, whether caller is a human or agent. When invoked on Linux, it selects direct execution even if RustRight happens to be on PATH.

CLI token/input is allowed without browser authentication. Pulse headless Linux join, two-way messaging, & reconnect remain required ([pulse#492](https://github.com/bogusyogi/pulse/issues/492)); owner approval may happen on Mac/Dell.

### Fixed execution boundaries

| Boundary | Requirement / constraint |
|---|---|
| Stages & compile | `check → compile → package → qualify → release`; all stages retain order. Each cell uses installed shared gate & local origin compile; CI cannot substitute. |
| Gate routing | Mac/Dell gate calls always use broker; Linux gate calls always run direct. Managed Mac/Dell refuse if broker is unavailable. Hosted Mac/Windows CI runners are not Adrian’s managed hosts; use direct native CI execution. |
| Qualification | RightKit/public qualification runs on CI. Private qualification runs on Mac or Dell for the required target; do not require both hosts universally. Private Linux compile/package remains pending until an actual required Mac/Dell qualification. |
| Linux | No browser, interactive browser authentication, broker, or broker-trust pin is prerequisite to invoke shared gate. A package/version pin for Linux CLI may be used. |
| Artifact scope | Product manifests define Linux artifact kinds. Pulse headless bundle is mandatory. Do not require a Linux GUI installer for every app. |
| Signing | Preserve existing native signing/authentication. Sign once, qualify final sealed bytes, then promote those bytes unchanged. Source/candidate/final digests may differ only through explicitly recorded native signing/package assembly. |
| Entry scope | Managed agents use shared gate; no per-app gate fork or agent bypass. Invoking gate from a human terminal on Mac/Dell still selects broker. Commands outside the shared entry are not globally intercepted by this contract. |
| Builds | Current workspace execution constraint: primary checkout, warm cache, no clone/worktree/branch/stash for build, & one build per host. This is a workspace constraint, not a new user quote. Disposable temp storage is for pack data only; durable pipeline state survives process & host restart per host policy. |
| Publication | Only RightKit requires Git auto-publication. Public apps require CI qualification/signed installers; private publication remains existing policy, unspecified here. |

## 2. Required origin × product matrix

Every cell begins **NOT RUN**. A cell is complete only after local compile & its stated qualification evidence both exist. “WAITING” is a desired semantic state for a private Linux candidate without a Mac/Dell result; it is not a verified CLI enum. When qualification is absent, that cell remains **BLOCKED** for overall completion.

| ID | Origin × product | Entry & local compile | Qualification | Delivery/result | Status |
|---|---|---|---|---|---|
| R-M | Mac × RightKit release | Installed shared gate; Class A, then local compile/package through broker | CI qualifies same candidate | Git auto-publication after green qualification; registry download hash matches final artifact | NOT RUN |
| R-D | Dell × RightKit release | Installed shared gate; Class A, then local compile/package through broker | CI qualifies same candidate | Same Git publication & registry readback | NOT RUN |
| R-L | Linux × RightKit release | Installed shared gate; Class A, then direct local compile/package without broker | CI qualifies same candidate through existing authenticated GitHub path | Same Git publication & registry readback; Linux needs no browser | NOT RUN |
| P-M | Mac × private app | Installed shared gate; Class A, then local compile/package through broker | Mac or Dell qualifies required target | Candidate records qualification & existing publication policy | NOT RUN |
| P-D | Dell × private app | Installed shared gate; Class A, then local compile/package through broker | Mac or Dell qualifies required target | Same private-app result; no blanket CI qualification | NOT RUN |
| P-L | Linux × private app | Installed shared gate; Class A, then direct local compile/package without broker | Held until actual Mac or Dell qualifies required target; private qualification does not require GitHub CI | Preserve exact candidate while pending; package alone does not claim qualification | NOT RUN |
| A-M | Mac × public app | Installed shared gate; Class A, then local compile/package through broker | CI qualifies same candidate & native signed installers | Signed installer is available & verified on its declared native host; publication follows existing repo policy | NOT RUN |
| A-D | Dell × public app | Installed shared gate; Class A, then local compile/package through broker | CI qualifies same candidate & native signed installers | Same native signed-installer requirement | NOT RUN |
| A-L | Linux × public app | Installed shared gate; Class A, then direct local compile/package without broker | CI qualifies same candidate through existing authenticated GitHub path | Native signed installer is available & verified; Linux needs no browser | NOT RUN |

RightKit cells apply to declared unit/registry routes: track npm & Cargo separately where present. Nonpublishing route tests need not publish every package; an authorized `@rightkit/release` npm publish proves only npm, not Cargo. Public/private representative app checks cover manifest-declared targets; do not infer universal Linux GUI targets. Matrix coverage is not proof. Public CI does not imply auto-publication; private publication remains unspecified.

## 3. Current source facts & proposal status

Source links below are immutable snapshots. Main at [fe81b3ec](https://github.com/bogusyogi/rightkit/commit/fe81b3ec032ede62ea106101c5705a6eaa569462); gate-contract proposal source at [f6c2ca45](https://github.com/bogusyogi/rightkit/commit/f6c2ca4587c6252367ba1790c3c5c3c25bd41b2d). The proposal branch was used for read-only inspection; it is not merged by this document.

1. **Admission is broker-coupled on main.** [unit-gate.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/packages/release/unit-gate.mjs#L58) requires signed broker receipts/host; [publish-admission.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/scripts/ci/publish-admission.mjs#L117) accepts broker admission; [host-gate.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/packages/build-control-policy/src/host-gate.mjs#L29) rejects Linux. Preserve auth & source/stage/digest/run matching; broker signatures are optional for Linux.
2. **Paths/canary assume specific hosts.** [rk-pipeline.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/scripts/ci/rk-pipeline.mjs#L22) & [unit-gate.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/packages/release/unit-gate.mjs#L72) hardcode host paths. [rk-release-canary.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/scripts/ci/rk-release-canary.mjs#L384) runs Darwin locally & Dell remotely, reading app SHAs from Mac workspace. Make orchestration origin-independent; preserve Mac/Dell scratch & configure durable Linux state.
3. **Native helper constrains packaging.** [local-publish.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/packages/release/local-publish.mjs#L414) refuses Linux/Windows npm local publication; [swap-helper-bundle.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/packages/release/swap-helper-bundle.mjs#L59) requires signed Darwin helper/receipt off Mac. Reuse versioned helper or bind local candidate to CI-sealed bytes using existing machinery. No cross-compilation, custom signer, or anti-forgery system.
4. **Class A ordering is wrong in current source.** [deterministic-scans.mjs](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/packages/release/deterministic-scans.mjs#L58) maps npm/CI tests to B; [its test](https://github.com/bogusyogi/rightkit/blob/fe81b3ec032ede62ea106101c5705a6eaa569462/packages/release/deterministic-scans.test.mjs#L2219) expects a test after compile. Inventory behavior; tests can be Class A even when they execute project code. Only evidenced dependency on new artifacts/compilation permits later placement.
5. **Adapter capability is not installation proof.** Mac build-control has a `rightkit-pnpm` adapter; source/config presence does not prove required pnpm/node argv & inherited environment work on both Mac & Dell. Capability mismatch has been noted for required recursive/install/pack actions; actual cause of prior failures is unproven. Do not report successful adapter repair until actual broker compile/package jobs complete on both hosts. Never expose host config secrets.

**Target interface — PENDING current-main implementation & baseline verification:** `pnpm gate:<stage>` invokes `rightkit-gate`; `rightkit-gate adopt` generates the `gate:*` scripts/hooks; the app shim defers managed hosts to the installed pinned gate; `rightkit-gate install-host-gate` installs a managed launcher from that pin; G23 rejects alternate managed-agent build entry. These are design commitments, not proven CLI behavior on main. The [f6c2ca45 proposal](https://github.com/bogusyogi/rightkit/blob/f6c2ca4587c6252367ba1790c3c5c3c25bd41b2d/docs/GATE-CONTRACT.md) generator omitted `gate:release`; main root has no generated gate scripts. Main has `rightkit-gate --to release --unit ...` source usage, not a successful run. Phase 0 discovers exact commands/outputs; implement missing interfaces in shared scope. Invent no flags or resume command.

## 4. Three layers of evidence

All three layers are **NOT RUN**; doc consolidation is not test binding or runtime success.

| Layer | Required outcome | Evidence/status |
|---|---|---|
| Contract coverage | Proposed `ci/gate-contract.json` represents nine IDs, origin, executor, local stages, qualification host, output/publish policy, & declared RightKit registry routes/public app targets. Proposed `scripts/ci/gate-contract.test.mjs` checks contract coverage against actual source rule IDs, scan prefixes, stage names, entry scripts, & host policy; it also traces policy changes to this document & human requirements. Both files were absent on main & proposal snapshot. | NOT RUN / proposed |
| Simulated behavior | Inject host facts (origin platform, managed-host identity, PATH, environment, broker availability). Assert actual gate route, Class A-before-first-compile order, reached/refused stages, output/side effects, & refusal reason for each matrix ID. Tests must detect wrong behavior, not only compare duplicated constants. Reject missing IDs & wrong behavior. Supporting fixes must trace to these acceptance requirements. | NOT RUN |
| Installed host runs | Invoke the installed shared entry on real Mac, Dell, & standalone/cloud Linux. Bind source/tree, installed gate, host/executor, command result, each stage, local output digests, final artifact & qualification evidence. Store receipt outside the source it describes. A synthetic broker request, cloned/worktree run, source inspection, fixture, or runner metadata is not a real run. | NOT RUN |

Keep receipts/results separate from requirement data. Use existing GitHub auth for RightKit/public results & Mac/Dell evidence for private qualification. Never commit a receipt into the source it attests; that changes the source identity.

## 5. End-to-end acceptance cases

Each case starts **NOT RUN**. Fixtures are nonpublishing & never prove real publication.

| ID | Given / when | Required result | Minimum evidence |
|---|---|---|---|
| E1 Class A completeness | Given complete inventory of agreed Class A checks, execute each through installed shared gate, then run focused red cases | Every check executes before first local compile; red Class A launches zero compilation/post-Class-A work. | Inventory-to-execution reconciliation, ordered run results, red-case launch trace. |
| E2 Managed broker refusal | Mac or Dell gate host with broker unavailable; invoke shared entry | Refuse before compile; no direct fallback. | Host identity, broker status, result, zero compile jobs. |
| E3 Linux direct headless | Standalone/cloud Linux (WSL2 may be extra), RustRight absent then present; invoke installed entry & CLI | Direct local compile in both cases; zero attempted browser/broker launches; no Mac path/mount dependency. | Host/gate version, zero-launch trace, path evidence, actual compile output & candidate ID. |
| E4 Private Linux wait/resume | Linux compile/package candidate; separately invoke actual qualification interface without Mac/Dell result, then provide actual Mac or Dell result | Pending semantic state retains exact candidate; qualification is not satisfied by package. Resume with matching candidate after required host result. | Before/after durable state, candidate IDs, host qualification result. `package` alone is not this test. |
| E5 CI block & RightKit publish | Test every declared npm/Cargo route with nonpublishing fixtures; reject failed/incomplete CI. Separately qualify/publish exact real RightKit candidate under existing authorization. | Failed candidate never promotes. Real RightKit Git run publishes only after green qualification. | Route coverage by registry; for actual publication, exact Git run & registry download hash. An npm success does not imply Cargo success. |
| E6 Stale/altered binding | Change source/tree, local output, gate version, helper, final artifact, app SHA, or run/attempt | Reject mismatched evidence before publish; existing valid authentication remains intact. | Binding failure record per class & accepted control fixture. |
| E7 Candidate-to-final binding | Build candidate from source/tree on each origin with existing native helper/signing/package process | Bind local result to final sealed bytes through only explicit native signing/package transformations; CI/public or Mac/Dell/private qualifies those final bytes & publisher promotes unchanged. | Source/tree; gate version, command result, local output digests; native helper identity/digest; allowed transformations; final artifact digest; qualification-to-final-byte mapping. |
| E8 Canary from each origin | Same candidate & required app SHA set; run canary from Mac, Dell, Linux | Required native legs complete regardless of origin; missing private qualification stays pending/blocked. Queued metadata is not completion. | Completed leg results, source/app SHAs, candidate/baseline hashes, status by origin. |
| E9 Retry idempotency | Interrupt coordinator after dispatch & after publication; resume same candidate | Reconcile state without duplicate dispatch/publish; ambiguous duplicates fail closed for reconciliation. | Durable state & run IDs; one dispatch & at most one publish per candidate. |
| E10 Shared entry scope | Human & managed agent invoke installed shared entry on Mac/Dell; personal terminal invokes outside entry | Both gate callers use broker with same ordering; no app-local or agent bypass. Outside-entry personal terminal is not globally intercepted. | Gate identity, human/agent traces, personal-terminal control. |
| E11 Manifest-declared targets | Public installers & representative private-app required targets from manifests; install/update & run on corresponding native host | Public signatures verify; each tested artifact is the exact sealed artifact. No undeclared GUI/platform target is inferred. | Manifest target list, host, signature, artifact hash, install/update/run result. |
| E12 Pulse Linux guest | Actual headless standalone/cloud Linux guest with owner-side approval on Mac/Dell; join, message, disconnect/reconnect | Join, two-way messaging, reconnect without Linux browser/broker; revoked access denied where required by existing policy. | Host/gate identity, zero attempted browser/broker launch, join/control result, message & reconnect transcript. |

## 6. Failures to prevent

| Failure | Prevention |
|---|---|
| Broker coupling excludes Linux or a tool on PATH selects executor | Host identity sets route; test RustRight absent/present on Linux. |
| Class A weakened or tests moved to B by name/latency | Inventory actual command behavior; assert all agreed Class A before first local compile. |
| Config/source mistaken for installed capability or successful compile | Verify active Mac/Dell adapter, compatible gate/broker versions, inherited env, & actual broker compile/package jobs. |
| Host-specific paths/coordinator prevent origin portability | Resolve tools & durable scratch per host; keep transient pack files disposable. |
| Synthetic admission, cache, or queued CI metadata called proof | Require actual compile output, completed correlated qualification, & downloaded final artifact hash for publication. |
| Build proceeds despite material safety/evidence defect | Block on outcome/safety defects & required evidence; do not add generic approval gates. |
| Receipt mutation changes the SHA it attests | Store evidence outside source tree & bind immutable source/artifact IDs. |
| Hook/assistant text expands authority or changes requirements | Human requirements govern scope; hooks may deny but never grant authority. |
| Personal terminal globally intercepted | Enforce managed-agent policy at entry/hooks; do not claim global interception. |

Do not carry unsupported counts or claims of clean state/success without evidence. Working-tree cleanliness, ahead/behind status, & publication are separate; report each from evidence. A refusal does not prove retention; E4 tests pending-state preservation & resume.

## 7. Corrective plan

Proposed work only; all exits remain **NOT RUN**. One integration owner controls checkout, evidence, & delivery. Use primary checkout, warm cache, no build clones/worktrees/branches/stashes, & one build per host.
Immediately after Phase 0 & before behavior repair, implement proposed matrix/binding & targeted behavioral regression tests; record known failing cases. Preserve requirement-based expectations: do not weaken them to match current code, & do not require all cases green before fixes begin.

| Phase | Owner | Work | Exit evidence |
|---|---|---|---|
| 0. Baseline & interface inventory | Integration owner | Record HEAD/tree state, installed gate & broker versions, active Mac/Dell adapter config/capability without secrets. Discover canonical entry, resume, nonpublishing fixture, evidence-read commands, & actual output fields; invent no flags. If absent, assign implementation to the shared owner. | Source/installed version map & command/output inventory; unresolved mismatches recorded, not guessed. |
| 1. Restore Class A | Gate owner | Inventory every agreed deterministic/mechanical check & actual command behavior. Reclassify from evidence; only new-artifact/compile-dependent checks may follow Class A. Correct scan logic & acceptance expectation. | Complete inventory reconciles to executed scan plan; all Class A before first local compile; red Class A starts zero compile/post-Class-A work. |
| 2. Unify entry & host executor | Shared gate owner | Implement one installed entry & route by managed-host identity: Adrian’s Mac/Dell broker always; Linux direct regardless PATH. Ensure app-local shim defers to installed pin on managed hosts. Keep hosted CI runners distinct from managed hosts. | Simulated matrix route/stage/side-effect tests; person & agent use same gate; outside-entry terminal remains unaffected; Linux stage result for RightKit/public flows through existing authenticated GitHub route only. |
| 3. Repair installed broker adapters | RustRight broker owner | Reconcile required pnpm/node argv & inherited environment, gate/broker versions, & active installed configuration on both Mac & Dell using existing broker machinery. Add no ad hoc broker engine. | Actual compile & package broker jobs complete on both hosts through installed adapter with correlated results; config/schema presence alone does not pass. |
| 4. Portable pipeline & durable state | Pipeline owner | Remove hardcoded host tools/paths from shared coordinator. Preserve `/Volumes/D/scratch` Mac & `D:\scratch` Dell policy; configure durable Linux state & host tool resolution. Temp data only for disposable packing. | State survives process & host restart; retries reconcile one candidate/run & do not duplicate dispatch/publish. |
| 5. Admission, packaging, & binding | Release/CI owner | Support valid origin result through existing authenticated workflow; preserve source/stage/digest/run matching & applicable existing provenance. Settle signed-helper archive strategy. Bind local compile to CI-sealed final artifact or native helper input, allowed transformations, & final digest. | Stale/altered bindings reject; successful qualification maps to final bytes & publisher promotes unchanged. No custom signer/cryptography. |
| 6. Qualification & canary placement | Qualification owner | Make canary origin-independent. RightKit/public qualify on CI; private qualification on Mac or Dell. Keep private Linux package & qualification separate; wait state stores same candidate & resumes only with matching actual host result. | Per-origin qualification records source/tree, gate, candidate, app SHAs, stages, host, final digest; private qualification is never inferred from package or CI. |
| 7. Install shared gate | Host integration owner | Install compatible shared gate on Mac, Dell, Linux; adopt it for managed apps & agent paths. Do not fork app gates or intercept personal terminal globally. | Installed versions & human/agent invocations match policy; Linux install/CLI is headless & needs no broker service solely to run gate. |
| 8. Run cells & deliver | Integration owner | Complete three evidence layers & all nine cells. Production publication uses existing user authorization for exact candidate; test fixtures remain nonpublishing. | All required local compiles & positive qualifications; real RightKit Git publication & registry download hash; public signed installers & private targets required by manifests; explicit npm/Cargo route coverage; Pulse headless E2E. No row is complete from source/test binding alone. |

## 8. Completion record

For each ID, record **NOT RUN**, **PASS**, **FAIL**, or **BLOCKED** with source/tree, installed versions, host, candidate/final digest, run link, & result. Runtime remains **NOT RUN** until proven. Private cases lacking Mac/Dell qualification are **BLOCKED**; Linux wait is negative-path success only.

Completion requires all nine positive cell outcomes; full Class A inventory & ordering; adapter jobs on both Mac & Dell; standalone/cloud Linux headless compile/guest proof; candidate-to-final linkage; RightKit Git publication plus matching registry download hash; CI qualification & signed native installers for public apps; Mac/Dell qualification for private apps; & no scope beyond existing user authorization. Failed fixtures, synthetic broker requests, source inspection, data binding, & simulated hosts never substitute for actual installed runs. Do not claim any of these outcomes until their evidence exists.

## 9. Provenance

This contract consolidates the user requirements & reviewed recovery specification created 2026-10-28. Main fe81b3ec & proposal f6c2ca45 links above are immutable comparison points; proposal is not merged implementation. Issue [rightkit#1254](https://github.com/bogusyogi/rightkit/issues/1254) tracks the work.
