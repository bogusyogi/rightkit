# Gate–Broker Audit 2026-10-30 — RightKit gate vs RustRight broker

Date: 2026-10-30. Auditor: Legion synthesis of 10 slice audits + critic re-verification.
Deliverable file: `docs/GATE-BROKER-AUDIT-2026-10-30.md` (this file). Do NOT commit (per task).

## (0) Scope & exact commits / tree states audited

- Brief said: RightKit HEAD `5c388c71e` + uncommitted `packages/release` WIP; RustRight HEAD `b84ae60` clean.
- Actual found by every slice: RightKit HEAD is **`b8ae3c677`** (`0.6.14: Windows client spawn, broker
  submit marked only at a carried send, subfolder app workspace, Swift scan scratch outside the
  snapshot (#1311)`), one commit ahead of `5c388c71e`. `git diff 5c388c71e..HEAD` touches
  `deterministic-scans` / `snapshot-broker` / `snapshot-request` / `build-ownership.mjs` /
  `bin/rightkit-gate.mjs` / `ownership-mark.mjs` (new) / `broker-client.mjs` (new).
  So brief "NEW untracked broker-client.mjs" is STALE — it is committed in `b8ae`.
  `packages/release` working tree was CLEAN at slice time for most slices; by end of audit a
  concurrent sibling WIP dirtied ~25 files (notably `snapshot-broker.mjs` withoutManagedRustEnv,
  `snapshot-request.mjs` schema rename, `broker-client.mjs`, `fake-broker.mjs`, `build-launch.mjs`,
  `managed-roots.mjs`, `pnpm-adapter-grammar.mjs`, `unit-gate.mjs`). Line numbers below are
  working-tree at read time unless marked HEAD; known drift `snapshot-broker.mjs:70 -> :82`
  (submit-mark) is noted per finding.
- RustRight HEAD **`b84ae60`** (`Broker: spawn_argv is mutable for the swiftpm scratch append`), clean,
  as briefed. Recent commits in scope: `eadb4ba` (SwiftPM managed output; missing fingerprint stays
  unexplained), `849091b` (census exemption, REVERTED by `eadb4ba`), `5f81531a3` (retired
  verdict/refreshed), `1945f0d42` (freshness compile-class only).
- Installed Mac host (critic live reads): `~/Library/Application Support/RightKit/build-control/
  config.json` has `externalRoot.path=/Volumes/D/.rightkit-managed`, `admission.trustedEntry`
  present, `bounds.jobDeadlineMs=0`, adapters `rightkit-pnpm/node/swift/qa` registered;
  `~/Library/Application Support/RightKit/build-control/runtime-current` ABSENT while
  `~/Library/Application Support/RustRight/runtime/rustright` EXISTS (dead primary probe proof).
- Method: read-only audit. Every named slice file opened with `read_file` (full or targeted windows
  stated per slice). `node --check` passed on touched gate files. Two probes executed and kept in
  `/tmp`: `/tmp/rk-audit/probe-mark.mjs` (submit-mark blocker) and `/tmp/rk-slice03-probe.mjs`
  (7 carried-cargo findings). No builds / cargo / suites run per briefs (broker Rust tests and gate
  test files inspected, not executed). No Dell run; Windows-only paths code-read only.
- Citation-gap rule: where an evidence string lacked a line number it is marked `citation-gap`
  inline. Where WIP already fixed a HEAD finding it is marked `FIXED-IN-WIP (still land it)`.

## Coverage table: slice -> files opened

| slice | files opened (all with read_file) |
|---|---|
| 01 wire-protocol | protocol `request/response/codec/identity/argv/native_consume/lib` (+attestation head); broker `server/framing/connection/gate/listener/identity` (+`broker.rs` build_served window); gate `broker-client/snapshot-request/snapshot-broker/broker-pins` full; windows of `client.rs/shims.rs/job_service/service.rs` |
| 02 snapshot-lifecycle | gate `snapshot.mjs(969)/snapshot-request.mjs(553)/snapshot-broker.mjs(356)/snapshot-metadata.mjs(117)/snapshot-git.mjs(166)/snapshot-root-config.mjs(76)/committed-source.mjs(253)/snapshot-check.mjs(313)/qa-snapshot.mjs+qa-snapshot-run.mjs`; broker `snapshot.rs(1406)/snapshot_grants.rs(753)/admit.rs(271)/gateway.rs(301)/recovery.rs(165)/server/gate.rs(358)`; wiring windows `service.rs/check_admission.rs/build-ownership.mjs/ownership-mark.mjs/deterministic-scans.mjs/bin/rightkit-gate.mjs/fake-broker.mjs/snapshot_lease.rs/protocol request.rs` |
| 03 carried-cargo-classA | all 8 named gate `.mjs` + `cargo-classification/carried-cargo/carried-adapter/clippy-route/class-a/cargo-guard` tests + `GATE-CLASS-A-INVENTORY.md` + broker `cargo_evidence/test_run/contained/qualify_impl/check_admission(1869 lines windowed)/admission_context/argv.rs` + supporting `snapshot-request/snapshot-broker/managed-cargo/cargo-target-shim/adapter-env/command_policy/adapter/environment/attestation/service` windows |
| 04 ownership-locks-submit | gate `build-ownership.mjs(1216/1216)/ownership-mark.mjs/broker-client.mjs/build-ownership-submit.test.mjs/snapshot-broker.mjs` + broker `admission.rs/limits.rs/survivors.rs/recovery.rs/admit.rs/server/broker.rs/server/pause_store.rs` + windows `snapshot.rs/service.rs/gate.rs/snapshot_lease.rs/install.rs/managed-cargo.mjs/deterministic-scans.mjs`; diffed `5c388c71e..b8ae3c677` |
| 05 env-folders-target-swift | gate `cargo-target.mjs/cargo-target-shim.mjs/target-bridge.mjs/swift-build-context.mjs/swift-package-command.mjs(adapter-env.mjs/toolchain-swift.mjs/cache-policy.mjs` full `deterministic-scans.mjs` env windows; broker `write_locations.rs/cache_handle.rs/toolchain_prune.rs/native_host.rs/version_probe.rs` full; `git show 849091b/eadb4ba/b84ae60` full; env spelling sweep 320 gate vs 229 broker names |
| 06 stages-dev-package | slice files all opened; `service.rs(8629 lines)` & `stage-sidecar.mjs(1984)` in targeted windows around every stage/adapter/snapshot/ndk/swift/forecast/consume touchpoint (not end-to-end); extras `development.mjs/ios-dev/android-ndk/recipe-build.mjs:380-438/release.mjs:1191-1217/native-helpers.mjs:200-270/snapshot-request.mjs grep/adapter.rs:690-756/cargo_evidence.rs:275-293/command_policy-ndk` |
| 07 receipts-freshness | gate `deterministic-scans(receipt sections)/verifier/stage-ledger/snapshot-check/carried-cargo/compile-inputs/freshness-census/direct-receipt/snapshot-request/snapshot-broker/provenance-verify/broker-pins/fake-broker`; broker `response/protocol+job_service attestation/journal_impl(full)/history_file/warmth/cargo_evidence(full)/observed_peaks/cargo_freshness/check_admission declared-inputs/service receipt windows`; `git show 849091b/eadb4ba/b84ae60-stat/5f81531a3/1945f0d42` |
| 08 fake-vs-real-pins | gate `fake-broker.mjs/test-support/*` + pins/doctor/route files; broker `Cargo.toml`s(version.workspace), `doctor.rs/response.rs/service.rs/adapter.rs/shims.rs/cli.rs/end_to_end.rs/test_support.rs/gate_tests.rs/fake_broker.rs` windows; `node --check fake-broker.mjs` OK |
| 09 docs-vs-code | all 6 named docs (`using-the-broker` 822 lines in 2 windows), `rustright/docs/contracts` all 4 files, `rightkit/contracts/README+right-suite-api.v1.json` (+dir/grep), gate WIP diff vs `5c388c71e`, broker HEAD clean |
| 10 concurrency-errors-platforms | broker slice files fully; gate `rightkit-gate.mjs` windows (quiet 1040-1096, doctor 1890-1970, ownership 1245-1285, signals 2618+2890-3060); supporting `service.rs/client.rs/config.rs` windowed |
| critic | working-tree re-reads (`snapshot-broker.mjs:82/ownership-mark.mjs:30-32/snapshot-request.mjs COMPILE_INPUTS/build-ownership.mjs:1190-1202/snapshot.rs scan+status/client.rs:377-380/request.rs enum/check_admission COMPILE_INPUTS/command_policy probe/adapter.rs registration shims) via scoped grep windows (rustright outside workspace root); installed Mac `config.json` live reads; `ls` runtime-current probe |

## (1) Verdict summary counts by severity

- BLOCKERS: 11 (B1–B11). All break a real Mac/Dell run on first contact with the affected route.
- MAJORS: 61 (M1–M61, several merged pairs noted). Each breaks a real route or forces a wrong
  ledger/verdict; fakes hide most of them.
- MINORS: 36 (N1–N36). Wedges, races, doc gaps, divergences needing a precise edit.
- OK-VERIFIED: 47 grouped points (V1–V47) with matching code both sides, no fix.
- FALSIFIED / STALE / SUPERSEDED-BY-WIP: 7 entries (F1–F7). Zero top-severity claims falsified as
  false; two fixed in working-tree WIP, three brief-state claims stale, two code-fixed earlier.
- Unresolved verbatim: carried in §8 (all slices + critic).

Top-10 blocker titles (evidence field index uses these):
1. Submit-mark forwarding drops callHandle (every production carried send throws pre-send).
2. Declared-inputs filename `.rightkit`-only vs broker `.rustright`-first.
3. False `RR_UNEXPLAINED_COLD` first-profile/triple build in warm target (not fixed at `b84ae60`).
4. Clippy broker receipt compact-units vs array (latent until 7.12 wiring).
5. Dev+package iOS xcodebuild/xcodegen refused at door.
6. Dev+package Android Gradle refused at door.
7. No timeout on carried spawns + broker deadline off + client no read timeout (hang forever).
8. Stage-grant retired on gate, normative in broker docs.
9. Retired receipt fields `verdict/refreshed` normative in gate+broker docs, refused in code.
10. Managed-roots `runtime-current` vs contract `runtime` (all 13 fixture cases differ).
(+ B11 trustedGate pin rename; §2 lists all 11.)

## (2) Findings — BLOCKERS

### B1. Submit-mark forwarding drops callHandle — every production carried send throws pre-send
- Gate: `packages/release/snapshot-broker.mjs:70` (working tree `:82`) `createSnapshotBroker`
  returns `markBrokerSubmit: () => ownership.markBrokerSubmit()` — drops `callHandle` arg.
  Callers pass it: `deterministic-scans.mjs:2315 markBrokerSubmit(sub)`,
  `snapshot-broker.mjs:92 markSend=(callHandle)=>sb.markBrokerSubmit(callHandle)`.
- Gate contract: `packages/release/ownership-mark.mjs:30-32 markBrokerSubmit(file,handle,callHandle)`
  throws `OwnershipBusy` unless `callHandle` is `<handle>#<id>`.
- Production wiring `bin/rightkit-gate.mjs:2059 & :2691` builds `sb` via `createSnapshotBroker`
  with `buildOwnership` whose `markBrokerSubmit` is `(callHandle)=>markBrokerSubmit(file,handle,
  callHandle)` (`build-ownership.mjs:1179`) — correct underneath, wrapper discards it.
- Failure mode (Mac+Dell, real run): every gate-process carried call (runScans snapshot compile +
  snapshotCargoSpawn metadata/fetch/deny/audit via `snapshot-metadata.mjs:113`) throws OwnershipBusy
  before send; scan fails `SNAPSHOT_REQUEST` (`deterministic-scans.mjs:2316-2318`) /
  `RK_CARGO_BROKER_OWNERSHIP` (`snapshot-broker.mjs:182`). PROVEN by execution
  `/tmp/rk-audit/probe-mark.mjs`: both `sb.markBrokerSubmit(sub)` and `section.markSend(sub)` THREW,
  record stayed `submitted:false`. Tests stay green because `test-support/fake-broker.mjs:216`
  forwards correctly (`markBrokerSubmit:(callHandle)=>markCarriedSend({callHandle})`).
  Corroboration: slices 02 + 04 + critic re-read (working-tree `:82` still broken, WIP untouched).
- EXACT FIX: `snapshot-broker.mjs:70` (now `:82`) change to
  `markBrokerSubmit: (callHandle) => ownership.markBrokerSubmit(callHandle)`.
- Proof test: node probe above + new gate test constructing via `createSnapshotBroker` (not fake sb)
  asserting record handles include sub-handle; real-broker `check --commit` on Mac.

### B2. Declared-inputs filename: gate reads only `.rightkit/compile-inputs.json`
- Gate: `packages/release/snapshot-request.mjs:138 COMPILE_INPUTS=.rightkit/compile-inputs.json`,
  `readDeclaredInputs :261-283` only that file; `compile-inputs.mjs:11,36` same.
  Overlap guard `:280` covers only `.rightkit` file.
- Broker: `crates/rustright-broker/src/job_service/check_admission.rs:32 COMPILE_INPUTS=
  .rustright/compile-inputs.json`, `:34 LEGACY=.rightkit/...`, `declared_inputs :1131-1145` prefers
  `.rustright` and falls back only when absent; `snapshot_inputs :1166-1168` refuses when
  supplied != declared. Broker `plain_relative :1122` covers both.
- Doc: `rustright/docs/using-the-broker.md:293` declares `.rustright/compile-inputs.json`
  (legacy `.rightkit/...`).
- Failure mode: first repo adopting `.rustright/compile-inputs.json` (broker's own test fixture
  already uses it, `snapshot.rs:698-718`) gets every carried request refused at bind
  `declaredInputs differs from the snapshot declaration`; if both files exist gate and broker digest
  different sets and `verifySnapshotReceipt` (`snapshot-request.mjs:494-495`) can never pass.
  A repo following using-the-broker gets carrier `declaredInputs=[]`, broker refuses every carried
  compile. Neither rightkit nor rustright repo has either file today (`ls` verified) so latent until
  first adopter. Corroboration: slices 02 + 07 + 09 + critic (WIP untouched, still `.rightkit`-only).
- EXACT FIX: `snapshot-request.mjs readDeclaredInputs`: try `.rustright/compile-inputs.json` first
  then `.rightkit/compile-inputs.json`, exactly as `check_admission.rs:1133`; overlap check covers
  both declaration paths; same in `compile-inputs.mjs judgeCompileInputs`; `GATE-CONTRACT.md:94`
  names both.
- Proof test: fixture snapshot with only `.rustright/compile-inputs.json` through fake+real broker;
  both-files fixture picks `.rustright`; broker `snapshot_inputs` test with gate-produced carrier;
  real broker Mac+Dell.

### B3. False RR_UNEXPLAINED_COLD: first build of new profile/triple in warm target signs FAILED
- Broker: `crates/rustright-broker/src/job_service/cargo_evidence.rs:280-285 unexplained_units`
  treats `FingerprintMissing` as unexplained unconditionally at `b84ae60` (`eadb4ba` deliberately
  REMOVED `849091b`'s `unitsAtStart==0` census exemption; `unitsAtStart` no longer read; test
  `:532 a_missing_fingerprint_stays_unexplained_whatever_the_start_census_says`).
  `service.rs:7151-7152,7741-7746` warm-target unexplained => job FAILED `RR_UNEXPLAINED_COLD`;
  `:7424-7427` signs receipt FAILED.
- Gate: `packages/release/snapshot-request.mjs:470,474 verifySnapshotReceipt` requires signed
  SUCCEEDED exit 0, correctly fails any non-SUCCEEDED signed result.
- Failure mode: first build of a new profile/triple/unit into a nonempty target (witness `rk4` job
  `07db8fa8`: `--profile release`, `unitsAtStart` 0, cargo logs `failed to read .fingerprint`
  NotFound) compiles clean exit 0 but broker signs FAILED; gate scan/stage fails. Retry passes only
  because fingerprints now exist. Contracts accept this as stage failure (`GATE:36`), but it is a
  false positive: deliberate per `eadb4ba` validate objection, pending complete build history.
  Corroboration: slices 03 + 05 + 06 + 07 + critic re-verified verbatim (`empty-profile-at-start
  does not prove never-built`).
- EXACT FIX (broker): `cargo_evidence.rs unexplained_units`: restore exemption — fingerprint-missing
  explained iff `unitsAtStart==0 && census not capped` (`849091b` logic) scoped per profile+triple
  fingerprint dirs (not whole-target), until per-generation build-history exists; or persist
  first-build-per-(profile,triple) record in workspace state and exempt on it. Keep counting in
  compact `unexplainedCount`. Alternative (gate): expect and surface `RR_UNEXPLAINED_COLD` as
  retry-once-warm — weaker, not preferred.
- Proof test: broker unit flipped to `a_missing_fingerprint_is_explained_only_when_start_census_
  found_none`; real Mac run: debug build then first `--profile release` (or `--target
  aarch64-apple-darwin`) build in same ws target must sign SUCCEEDED first try; second identical
  build fresh.

### B4. Clippy broker receipt: compact units-count vs required units-array (latent until 7.12)
- Gate: `packages/release/clippy-route.mjs:269 routeFromBrokerReceipt` requires `attribution.units`
  be an array (`:275`) and maps per-unit rows.
- Broker: `crates/rustright-broker/src/job_service/cargo_evidence.rs:297 compact_attribution` emits
  `units` as a COUNT (`:314`) and packages as count; full rows only in evidence file
  (`service.rs:6939 compact_stage_evidence`).
- Failure mode: every real broker clippy receipt reads FAILED `RK_CLIPPY_RECEIPT_EVIDENCE`
  `lists no units` once carrier wired. Reproduced by `/tmp/rk-slice03-probe.mjs`. Latent today:
  clippy on Mac/Dell fails closed pre-ClassA by design (`clippy-route.mjs:340-348
  RK_CLIPPY_BROKER_CARRIER_NOT_WIRED`, `cargo-contract.mjs:32-35`, Linux direct via
  `cargo-target-shim`) — fix before 7.12 wiring.
- EXACT FIX: `clippy-route.mjs routeFromBrokerReceipt`: accept compact shape
  `{units:<number>,errors,warnings,freshUnits,truncated}` and fetch full units from
  `attribution.detail {path,sha256}` evidence file; or broker stops compacting for clippy.
- Proof test: `clippy-route.test.mjs routeFromBrokerReceipt` fed a compact-shaped receipt
  (`units:2`) must PASS when exit 0.

### B5. Dev+package iOS: xcodebuild/xcodegen refused at door, every run
- Gate: `packages/release/development.mjs:159 execute()` sends every iOS build command through door;
  `ios-dev.mjs:129-134` declares `{cmd:xcodegen}/{cmd:xcodebuild}`;
  `build-launch.mjs:242` refuses any program outside
  `cargo/pnpm/node/swift/rustright/rightkit-qa/own-right-release`.
- Broker: `crates/rustright-broker/src/job_service/service.rs:3890` an adapter runs only if
  registered in host config; gate `build-launch.mjs:64 ADAPTER_PROGRAMS` has only
  `rightkit-pnpm/node/qa/swift`. Critic grep-corroborated (ADAPTER_PROGRAMS lacks both; final
  refuse at `build-launch.mjs:247`).
- Failure mode: `rightkit-gate --to dev|test --platform ios` (and `iosPackage` package) on Mac
  broker host dies `RK_BUILD_LAUNCH_REFUSED` after class A, every run. Linux/direct unaffected
  (`build-launch:149`).
- EXACT FIX: in `build-launch.mjs` treat `xcodebuild/xcodegen` like snapshot QA (direct gate-session
  step, `:189-208` pattern: containment-checked, derivedData from rustright storage) since Xcode
  signing needs GUI/Keychain session broker jobs lack (`service.rs:2350` cleared env); OR register
  `rightkit-xcode` compile+snapshot adapter in broker config AND add it to buildLaunch +
  ADAPTER_PROGRAMS.
- Proof test: `build-launch.test.mjs` case xcodebuild on snapshot route + real `--to dev
  --platform ios` on Mac.

### B6. Dev+package Android: Gradle refused at door, every run
- Gate: `packages/release/android-dev.mjs:8` declared build `{cmd:./gradlew,args:[:app:assembleDebug]}`,
  `android-release.mjs:9 bundleRelease` same shape, routed via `release.mjs:1202 runCommand->
  buildLaunch` and `development.mjs:423` door; `build-launch.mjs:66` even lists gradlew as build tool
  then `:242` refuses it.
- Broker: `service.rs:3890` no gradle adapter exists in gate's adapter set.
- Failure mode: `--to dev --platform android` and `--to package --platform android` always
  `RK_BUILD_LAUNCH_REFUSED` on Mac/Dell broker hosts.
- EXACT FIX: same choice as xcode — direct gate-session exemption for `gradlew` in buildLaunch
  (snapshot-contained, outputs to snapshot `build/` as generated ledger entries), or register
  `rightkit-gradle` adapter broker+gate.
- Proof test: buildLaunch gradlew snapshot case + real `--to dev --platform android assembleDebug`
  on broker host.

### B7. Timeouts: nobody kills first — carried spawn blocks forever, broker deadline off
- Gate: `packages/release/snapshot-broker.mjs:194 carryBrokerCargo` and `:281 carryBrokerAdapter`
  `spawnBrokerClient` with NO timeout option (spawn call verified; options carry only
  cwd/env/stdio/encoding/maxBuffer/windowsHide).
- Broker: `crates/rustright-config/src/config.rs:44 classDeadlineMs` default `0=off`;
  `job_service/service.rs:3596` deadline timer only spawned if `deadline_ms>0`; client
  `rustright-cli/src/client.rs:86 connect()` sets no read timeout, exchange loop `:185` blocks.
- Live corroboration (critic): installed Mac config `bounds.jobDeadlineMs=0` — blocker is live, not
  theoretical.
- Failure mode: hung cargo/adapter (first dev/package run) hangs gate indefinitely, ownership never
  released.
- EXACT FIX: in `snapshot-broker.mjs` pass timeout (e.g. 65 min > broker class deadline) +
  killSignal to both carried spawns and ship nonzero default `limits.classDeadlineMs` in broker
  config; set UnixStream/pipe read timeout in client connect.
- Proof test: broker fake job that never exits; gate carried call must abort at bound with
  `RK_CARGO_BROKER_UNVERIFIED` and broker job CANCELLED.

### B8. Stage-grant retired on gate, normative in broker docs (doc blocker, wrong mechanism)
- Gate code: `packages/release/snapshot-broker.mjs:164-165` `There is no stage grant any more`
  (deletes `RUSTRIGHT_SNAPSHOT_GRANT`); `carried-cargo.mjs:16`, `GATE-CLASS-A-INVENTORY.md:171`
  `no stage grant`; `door-route.test.mjs:331 REQUIRED_BROKER_CAPABILITIES=
  [snapshot-readonly-v1,snapshot-adapter-v1]` only.
- Broker code live: `request.rs:132-154,239-241` grant/ledger/`GRANT_RECEIPTS` machinery.
- Docs: `rustright/docs/BROKER-CONTRACT.md:102-105` grant/ledger/`GRANT_RECEIPTS`,
  `using-the-broker.md:264-281`, install gate requires capability `stage-grant-v1 :548`.
- Failure mode: following BROKER/using, an implementer mints grants for tauri/release stages — gate
  drops env, verifies via per-call carrier+ledger instead, grant receipts never produced for gate
  runs. Docs describe wrong dev/package mechanism (never-run stages).
- EXACT FIX: `BROKER-CONTRACT` §3 grant paras + `using-the-broker:264-281,548` mark grant RETIRED
  for gate route (nested adapter children, rustright#190), or restore gate minting; align install
  capability list.
- Proof test: real-broker `--to dev` tauri fixture Mac+Dell asserting nestedChildren path and zero
  grant files; doc-drift test grepping `REQUIRED_BROKER_CAPABILITIES` vs `using:548`.

### B9. Retired receipt fields verdict/refreshed normative in docs, refused in code
- Docs: `rightkit/docs/GATE-CONTRACT.md:104,109` receipt `verdict{reusedFrom}`+`refreshed`, `:113`
  Linux minus `reusedFrom`; `rustright/docs/contracts/attested-runs.md:185-211` resultEvidence
  includes `verdict/refreshed`.
- Code: broker `protocol/src/attestation.rs:280 RESULT_EVIDENCE_FIELDS=[exit,signal,timingsMs,
  diagnosticsPath]`, `:284 RETIRED=[verdict,refreshed]`, `:262-266` verify REFUSES receipt/payload
  carrying either; gate `snapshot-request.mjs:543,553` same refusal (WIP fake-broker aligned).
- Failure mode: consumer built to GATE/attested waits for fields broker never emits and must refuse
  if present; Class-A reuse evidence design (E18/B19) unimplementable as written.
- EXACT FIX: delete `verdict/refreshed` rows `GATE:104,109,113` and `attested:185,203-205`; state
  gate-local receipt carries its own Class-A verdict under gate-owned names.
- Proof test: `gate-contract.test.mjs` asserting GATE receipt table ==
  `RESULT_EVIDENCE_FIELDS+SNAPSHOT_EVIDENCE_FIELDS` from `snapshot-request.mjs:42-46`.

### B10. Managed-roots runtimeCurrent: contract says `runtime`, gate resolves `runtime-current`
- Contract: `rustright/docs/contracts/managed-roots.md:3,19` `Both must pass fixtures`; derived dir
  is `runtime` (key `runtimeCurrent`, no `runtime-current`); fixtures `:17` expect
  `.../build-control/runtime`. Broker `managed_roots.rs:154` joins `runtime` — correct.
- Gate: `packages/release/managed-roots.mjs:12 runtimeCurrent=join(controlRoot,'runtime-current')`
  (node run proved `.../runtime-current`); `managed-roots.test.mjs:18` enshrines it.
  Python compare: same 13 case names, ALL 13 differ, only in `runtimeCurrent`.
- Failure mode: every JS consumer of `runtimeCurrent` resolves a nonexistent runtime dir; shared
  fixture contract fails on JS side on all 13 cases. Latent: `managed-cargo.mjs:54` probes
  `<root>/runtime-current/rustright` first (dead path on current install — critic proved
  `.../build-control/runtime-current` ABSENT on this Mac), mitigated by fallbacks
  `managed-cargo.mjs:59-72` (Mac `~/Library/Application Support/RustRight/runtime`, Win
  `<drive>\.rustright\runtime`); `runtimeCurrent` field itself unused in gate prod code.
- EXACT FIX: `managed-roots.mjs:12` `'runtime-current'->'runtime'`; probe `runtime` before
  `runtime-current` in `managed-cargo.mjs:54`; update `managed-roots.test.mjs:18,34,36,69`;
  re-vendor fixture from rustright HEAD; byte-diff of the two fixture files in CI.
- Proof test: node test iterating all 13 fixture cases vs `resolveManagedRoots` (darwin/win32 legacy
  subset) green.

### B11. Trusted-gate pin name retired, pipeline doc still instructs it
- Doc: `rightkit/docs/PIPELINE-ARCHITECTURE.md:177` install step sets
  `buildRules.checkAdmission.trustedGate`.
- Broker: `crates/rustright-config/src/config.rs:308 RENAMED_KEYS trustedGate->admission.
  trustedEntry`, `:299-303` checkAdmission keys removed, `:113 ADMISSION_KEYS=[trustedEntry]`;
  `using-the-broker.md:258-261,577-579` correct name. Critic: installed Mac config
  `admission.trustedEntry {path,sha256}` present — code+host agree, doc wrong.
- Failure mode: fresh host configured per PIPELINE §10 pins under retired key; broker admits nothing
  (`RK_CHECK_ERROR`, `trustedEntry` unset) or config refused.
- EXACT FIX: `PIPELINE:177` replace with `admission.trustedEntry {path,sha256}`.
- Proof test: `pipeline-contract.test.mjs` grep docs for `checkAdmission.trustedGate` outside
  history == 0; broker config test loading PIPELINE-documented JSON.

## (2) Findings — MAJORS

### M1. Untyped PAUSE/RESUME request kinds bypass typed protocol
- Gate/broker-client: `crates/rustright-cli/src/client.rs:378-398 admission_request` sends
  `{type:PAUSE_ADMISSION|RESUME_ADMISSION,protocol,runtime,reason?,holder?}` via untyped `send_value`.
- Broker: `protocol/src/request.rs:521-568 BrokerRequest` has NO such variants (exactly 20 kinds:
  §6 table); `protocol/src/codec.rs:77-80 read_request` would reject them; `job_service/service.rs:
  1588-1593` typed parse would answer `registered broker job operation is required` if ever reached.
  Only `broker/src/server/gate.rs:265-303 DataRootGate` intercepts them as raw `Value` before
  forwarding; `build_served` wraps service in DataRootGate (`broker.rs:271-281`).
- Failure mode: works today; any typed-codec refactor or direct-service listener silently breaks
  pause/resume-admission on real run. Re-confirmed by critic (enum verified without them).
- EXACT FIX (broker): add `PauseAdmission{protocol,runtime,reason?,holder?}/ResumeAdmission
  {protocol,runtime}` variants+structs (camelCase, `deny_unknown_fields`) to `request.rs`
  BrokerRequest and an `Admission` event to `response.rs`; make `client.rs admission_request` build
  the typed variant.
- Proof test: round-trip `PAUSE_ADMISSION` JSON through `BrokerRequest` + `codec::read_request` in
  `request.rs` tests; real-broker `rustright pause-admission/resume-admission` smoke.

### M2. Response line cap: three ceilings (1MiB client vs 8MiB broker vs 64MiB gate) [+M47 merged]
- Gate: `snapshot-broker.mjs:38 receiptSync` allows 64MiB stdout; `snapshot-request.mjs:404` allows
  8192 nestedChildren in one receipt; `snapshot-broker.mjs:50 receiptSync maxBuffer 64MiB`, `:194`
  capture maxBuffer 256MiB.
- Broker: emits STATUS (incl. full receipt) and `GRANT_RECEIPTS` (up to 4096 calls each with full
  receipt, `request.rs:147-148 maxCalls`) as ONE line (`service.rs:2896,2952` via one `emit_value`);
  connection cap 8MiB per queued line (`server/connection.rs:23,81-99`, over-cap detaches with NO
  terminal line); client cap 1MiB per line (`protocol/src/lib.rs:41 MAX_BROKER_LINE_BYTES`,
  enforced `client.rs:193-201 response exceeds line limit`). Live event queue drops oldest middle
  output over limit (`ports.rs:210`, `job output_limit` 8MiB `job.rs:66`) with one OUTPUT_TRUNCATED
  (`ports.rs:267`); watcher detach `RK_WATCH_BACKPRESSURE` (`service.rs:7586`).
- Failure mode: carried cargo/adapter succeeds at broker but receipt >1MiB (many nestedChildren/
  declaredInputDigests) → client errors → gate marks call unverified and fails stage; GRANT_RECEIPTS
  for nontrivial grant never readable; capture consumer parsing cargo JSON silently gets
  middle-truncated/replayed stream while receipt verifies.
- EXACT FIX: raise `MAX_BROKER_LINE_BYTES` to >=8MiB in `lib.rs` (and client cap) OR chunk STATUS
  receipt/`GRANT_RECEIPTS`; make `connection.rs terminal()` a typed RESULT REJECTED on single-line
  over-cap instead of silent detach; one shared constant. Gate: treat OUTPUT_TRUNCATED/
  WATCH_DETACHED in captured output as unverified, or read broker-recorded output log named in
  receipt for capture calls.
- Proof test: broker e2e STATUS for adapter job with >1MiB receipt (6000 nestedChildren fixture)
  read via `rustright status` + gate `receiptSync`; `GRANT_RECEIPTS` with 100 recorded calls;
  synthetic 2MiB receipt; cargo emitting >8MiB JSON via capture byte-identical to broker log.

### M3. Request size cap: gate allows 1MiB declaration + 4MiB carrier, wire caps whole line at 1MiB
- Gate: `snapshot-request.mjs:139,271` allows compile-inputs declaration up to 1MiB and carrier file
  up to 4MiB (`:33,217`).
- Broker: wire cap 1MiB for WHOLE request line incl newline (`protocol/src/lib.rs:38`,
  `server/connection.rs:21`, `framing.rs:40-74`); client embeds all inputs into
  `ManagedAgentRequest.declared_inputs` (`shims.rs:616`) and `send_value_with` (`client.rs:284-292`)
  performs NO size pre-check (only `forecast_query.rs:22-31` checks).
- Failure mode: declaration between ~(1MiB-envelope overhead) and 1MiB passes gate+carrier, broker
  framing rejects pre-handshake RESULT 125 `request exceeds input cap` (`framing.rs:13,31-33`, no
  code field); gate finds no job/receipt → unverified scan failure, one-per-real-run style.
- EXACT FIX: in `snapshot-request.mjs` lower `MAX_DECLARATION_BYTES` to leave envelope headroom
  (e.g. 768KiB) AND in `client.rs send_value_with` refuse serialized line `>MAX_REQUEST_BYTES` with
  named error before writing.
- Proof test: gate unit test with inputs list serializing to 1,048,000 bytes must refuse at gate with
  RK code; client unit test send of oversized `ManagedAgentRequest` errors pre-write.

### M4. Lease reap / crash-cleanup deadlock: status UNKNOWN forever, no sweep, reboot-proof wedge
- Gate: `build-ownership.mjs:1044-1048 coveredWorkGone` requires broker lease REAPED for bare handle
  whenever `broker.submitted` (runs even on reboot branch).
- Broker: `job_service/snapshot.rs:433-442 status()` returns UNKNOWN whenever lease record dir exists
  and never consults reaper/scan; reap proof only inside `acquire_owned :320-328` (next lease
  acquisition on that path); no startup lease sweep exists (`snapshot_leases` uses in `service.rs`
  only `:785` construct, `:1666` status, `:3644` record; `recovery.rs` whole file is
  workspace-generation recovery, no snapshot-lease pass).
- Failure mode: broker killed after `runner.spawn` (lease record+process.json remain,
  `mark_run_started` at `service.rs:6020-6022`) and journal does not resume that job → status UNKNOWN
  forever; next gate cannot reclaim ownership (needs REAPED) and cannot send the carried job that
  would trigger acquire-reap (needs ownership): host wedged until manual deletion under
  `<external>/snapshot-leases`, reboot does not help (see M6).
- EXACT FIX (broker): `snapshot.rs status()`: when record exists but no held lease, run same
  reaper+scan proof as `acquire_owned :320-327` and on proof write completion+remove record then
  answer REAPED (or add startup lease-recovery sweep doing same); no gate change.
- Proof test: broker test — acquire+record_process+forget lease, new `Leases`, process dead ⇒ status
  REAPED without any acquire; real run: `kill -9` broker mid-compile, restart, `rustright
  snapshot-lease status`.

### M5. Mark-before-send wedge: submitted with no broker record
- Gate: `snapshot-broker.mjs:182,269` marks sub-handle immediately before `spawnBrokerClient`; if
  client spawn fails, broker down, or broker refuses at `server/gate.rs:306-315 (RK_ADMISSION_PAUSED,
  answered in DataRootGate BEFORE job service, so `record_refused` at `service.rs:3637-3647` never
  runs)`.
- Broker: `job_service/snapshot.rs:433-472 status()` returns UNKNOWN for a handle with no
  claim/completion.
- Gate reclaim: `build-ownership.mjs:1044-1047` demands REAPED forever.
- Failure mode: gate crashes or ends after such a mark; host locked until manual `owner.json`
  removal — same wedge WIP fixed for uncarried probes, moved to send-failure window.
- EXACT FIX: (a) gate — on provably-unsent failure (spawn returned `r.error`, client never started)
  unmark that sub-handle under mark lock in `ownership-mark.mjs`, recomputing `submitted` from
  remaining handles; (b) broker — move admission-paused refusal into job-service submit path (or give
  DataRootGate a `record_refused` callback) so paused snapshot requests get durable terminal records.
- Proof test: real broker paused via `rustright pause-admission`, gate carried call, kill gate, next
  gate must reclaim; repeat with broker stopped at send instant.

### M6. Reboot does not unwedge submitted ownership
- Gate: `build-ownership.mjs:941-942` reboot proof skips containment nets but `:1044-1047` lease
  REAPED check is unconditional.
- Broker: `snapshot.rs:438-442 status()` after restart — in-memory held empty, on-disk lease record
  dir exists → UNKNOWN; `status()` never attempts reap proof (only `acquire_owned :320-328` does, on
  next overlapping acquire, which gate status query never triggers).
- Failure mode: host reboots mid-broker-job (all broker processes provably dead), yet every later gate
  stays busy UNKNOWN until manual file removal. Re-confirmed by critic (reboot skips containment
  nets only).
- EXACT FIX: gate `build-ownership.mjs:1044` wrap lease check in `if (!rebooted)` (broker + its job
  processes run on same host, cannot survive reboot) and record proof `host rebooted, broker work of
  previous boot cannot run`; or broker `status()` reap-checks orphaned records like `acquire_owned`.
- Proof test: fixture dead owner `submitted:true` + bootTime after acquiredAt + lease UNKNOWN must
  reclaim (extend `build-ownership.test.mjs:191-205` which currently only tests reboot without
  submitted broker work).

### M7. Windows leftover lease never reaped after broker restart
- Broker: `job_service/snapshot.rs:248-252 Leases::new` scan closure returns
  `Err("survivor scanning is unavailable")` on all non-macOS, used at `:325` in `acquire_owned` reap
  path for pre-restart records with `process.json`.
- Gate expects Dell broker to eventually answer REAPED / re-admit path.
- Failure mode: broker crash/restart on Dell with any in-flight snapshot lease → that snapshot path
  permanently `snapshot survivor scan incomplete`, status UNKNOWN, gate ownership + all future jobs on
  path wedged (runtime `confirm_reaped` Windows Job-Object path `:582-591` does not help post-restart,
  job handle lost). Re-confirmed verbatim by critic.
- EXACT FIX (broker): `snapshot.rs Leases::new` add `#[cfg(windows)]` scan — persist tree Job Object
  name in lease record at `record_process` and scan = `OpenJobObject`+`Active==0` or GONE (mirror gate
  holderScript/jobProbeScript); `Err` only on other errors.
- Proof test: Windows broker test — acquire lease, `record_process` a short-lived process, drop
  `Leases` (restart), new `Leases` `acquire_owned` same path succeeds + status of old handle REAPED.

### M8. Stale `.reclaim` file wedges host permanently
- Gate: `build-ownership.mjs:1190-1201` (critic: `:1190-1202`) reclaim publishes `<file>.reclaim`
  create-exclusive and removes in `finally`, but process death (SIGKILL/power) between publish and
  `finally` leaves it; every later acquire throws `is being reclaimed by another process (remove ...
  only if no gate is running)` with no owner pid, age, or staleness check. Contrast mark lock in
  `ownership-mark.mjs:49-64` with 10s stale-break.
- Failure mode: one unlucky crash during reclaim = permanent manual-clear wedge, message
  indistinguishable from live reclaim race. Re-confirmed verbatim by critic.
- EXACT FIX: `build-ownership.mjs` publish `.reclaim` body includes pid+startedMs (reclaimBody record
  already has them: parse it on contention; if pid dead/start-mismatch via `probe.identity`, remove
  and retry publish once).
- Proof test: acquire with pre-planted `.reclaim` naming dead pid reclaims; naming live pid stays busy
  (add beside `build-ownership.test.mjs:72` live-owner test).

### M9. Parallel carried calls same snapshot collide at lease — no wait for gate model
- Gate: `ownership-mark.mjs:27-28` explicitly supports parallel children marking/sending carried
  requests under one ownership (submit test `build-ownership-submit.test.mjs:143-159` fakes 30
  parallel sends OK). `deterministic-scans.mjs:2008-2035 runScans` honors `jobs/classAJobs>1` with no
  forcing to 1 on broker-snapshot route.
- Broker: `job_service/service.rs:3831-3854` wait-for-finished-holder loop runs only
  `if let Some(grant)`, and gate dropped grants (`snapshot-broker.mjs:153 no stage grant any more`),
  so per-call carriers hit `snapshot.rs:274-275` path-overlap acquire → immediate `RK_CHECK_ERROR`
  refusal. Corroboration: slice 02 minor + slice 04 major (strongest citation kept here).
- Failure mode: two concurrent carried cargo/adapter calls on same snapshot (parallel scans,
  right-release parallel steps) — second spuriously refused on real broker though both marked;
  sequential-only by accident. Currently latent (default `jobs=1`).
- EXACT FIX: `service.rs` extend the bounded `SNAPSHOT_RELEASE_WAIT`-for-finished-holder (and queue
  behind running same-prefix holder) to per-call carrier handles with same bare-handle prefix, not
  only `grant.is_some()`; or gate forces `aJobs=1` when `deps.snapshotBroker` set in runScans.
- Proof test: real-broker (or broker service test) two concurrent snapshot-carrier submits same
  snapshot+bare handle, distinct sub-handles, both verified receipts.

### M10. Carried `--config` refused gate-side for deny/about shapes broker exempts
- Gate: `snapshot-request.mjs:93 CARRIED_REDIRECT` + `:120 splitCarriedArgs` refuses any `--config`
  before `--` for ALL carried cargo.
- Broker: `policy/src/command_policy.rs:306 cargo_own_args` exempts deny/about shapes so their
  `--config` is the tool's own, and `:313 about_notices_shape` accepts `--config/--manifest-path`.
- Seam: `cargo-license-notices.mjs:46 cargoAboutArgs` via runCargoSync
  (`cargo about generate --frozen --fail --config legal/about.toml ...`); wrapper path exempt
  (`cargo-target-shim.mjs:89 PLUGIN`) so seam/wrapper diverge. Reproduced by probe.
- Failure mode: seam `cargo about generate ... --config ...` and any seam deny are
  `RK_CARGO_REDIRECT`-refused gate-side in a snapshot stage, never reach broker.
- EXACT FIX: `snapshot-request.mjs splitCarriedArgs`: skip `--config` refusal when
  `cargo_own_args`-equivalent `deny_check_files/about_notices_shape` matches, and do not strip
  about's `--manifest-path` (judge as about path option).
- Proof test: `carried-cargo.test.mjs` carried `about generate --frozen --fail --config <rel>
  --manifest-path <rel> <template>` verifies; `deny --frozen --config <abs outside live>` check
  verifies.

### M11. Carried redirect regex refuses attached `-pCore`/`-pZoo` broker accepts
- Gate: `snapshot-request.mjs:93 /^-[A-Za-z]*[CZ]/` (same `cargo-target-shim.mjs:57`) refuses
  `-pCore`/`-pZoo` attached package forms.
- Broker: `command_policy.rs:278 short_cluster_carries_z_or_c` stops scan at value letters `j/p/F`.
  Reproduced by probe.
- Failure mode: `cargo build -pCore` gate-refused `RK_CARGO_REDIRECT` in snapshot stage though broker
  and cargo accept.
- EXACT FIX: `snapshot-request.mjs` and `cargo-target-shim.mjs`: replace regex with cluster walk
  stopping at `j/p/F` like broker `:280-286`.
- Proof test: `splitCarriedArgs([-pCore,-FZed,-j4])` no refusal; `[-vZ,-rC]` still refused, in
  `carried-cargo.test.mjs`.

### M12. `cargo tree` closure bypasses door (unledgered) and broker has no read-only arm for it
- Gate: `release-switch-scan.mjs:217 resolveCargoTree` builds `tree --locked --manifest-path <abs>`
  and `:257` runs it via default `spawnSync('cargo')` (`prebuild-gates.mjs:122,127` pass no `run=`)
  bypassing carried seam/ledger in a snapshot build stage; `snapshot-request.mjs:506` comment assumes
  `tree --locked` is an accepted carried read-only.
- Broker: `command_policy.rs:444 snapshot_read_only` (`offline_read_only :452` + `class_a_scan :397`)
  has no `tree` arm; `service.rs:1218` snapshot request must compile.
- Failure mode: direct spawn is unledgered/unverified and broker-dependent (observation route or
  refusal); if routed through runCargoSync it is `RK_CHECK_ERROR` must-compile and stage fails.
- EXACT FIX: `release-switch-scan.mjs`: default `run` to runCargoSync and broker
  `command_policy.rs`: add `tree --locked/--frozen` (`observation_only_cargo` shape) to
  `snapshot_read_only`, or replace closure with metadata-based graph gate-side.
- Proof test: real/fake broker carried `cargo tree -e features --locked` from snapshot stage verifies
  and appears in ledger; `cargo-classification.test.mjs` tree row `acceptedAfterFix=true`.

### M13. Carried `fmt` shape: wrapper binds anything, broker accepts exactly two shapes
- Gate: `cargo-target-shim.mjs:85` wrapper binds any `fmt` with `--check` anywhere (probe:
  `fmt --check -- --edition 2021` exec'd); seam `splitCarriedArgs` shape-checks nothing.
- Broker: `command_policy.rs:417 fmt_check_shape` accepts ONLY `[-p unit --check]` or
  `[--all --check]`, used at `:406 class_a_scan`.
- Failure mode: test/tool `cargo fmt --check [--manifest-path/-- edition...]` in snapshot scan passes
  Linux direct, broker-refused must-compile on Mac/Dell. Critic: WIP mirrors `fmt_check_shape`
  (land it).
- EXACT FIX: `cargo-target-shim.mjs decideCargoInvocation` + `snapshot-request`: refuse non-exact fmt
  shapes gate-side with RK code naming the two shapes; or broker widens `fmt_check_shape` to
  `--check` forms judged by `contain_grant_paths`.
- Proof test: carried `fmt --check -- --edition 2021` and `fmt -p a --check` both have one agreed
  outcome in `cargo-classification.test.mjs` against fake broker modelling `fmt_check_shape`
  (fixture `acceptedAfterFix` currently false even for exact shapes — probe).

### M14. `locate-project`/`read-manifest` refused gate-side without lock flag broker never requires
- Gate: `cargo-target-shim.mjs:44 LOCKED` set includes `locate-project/read-manifest`; `:88` refuses
  without `--locked/--frozen` (probe: `locate-project --workspace RK_CARGO_UNBOUND_COMMAND`).
- Broker: `command_policy.rs:460 offline_read_only` accepts `locate-project/read-manifest` with any
  args (no lock flag exists).
- Failure mode: Tauri/tool locate-project in snapshot scan refused gate-side on broker hosts;
  classification table (`test-support/cargo-classification.mjs:122`) lists it as wrapper-carried,
  unreachable.
- EXACT FIX: `cargo-target-shim.mjs`: move `locate-project/read-manifest` from LOCKED to a no-lock
  bound set.
- Proof test: wrapper `decideCargoInvocation locate-project --workspace` execs and fake-broker carried
  call verifies.

### M15. Cargo aliases `b/c/t/r/d`: gate binds/sends them, broker matches full names only [+slice07]
- Gate: `cargo-target-shim.mjs:42 COMPILE` binds `b/c/t/r/d` (probe: `cargo b -p a` exec to broker);
  `snapshot-request.mjs:52` lists `b/c/t/d` compile-class.
- Broker: `command_policy.rs:552 cargo_capability` matches only full names; `:762` unknown alias =
  `RK_UNKNOWN_COMMAND`; `command_index :236` takes command literally (no alias expansion).
  Slice 07 minor corroborates (carried `cargo b/c/t/d` gets no compile capability/receipt at broker
  while gate expects compile-class evidence). Critic: WIP adds alias expansion in shim (land it).
- Failure mode: `cargo b/c/t/r/d` in snapshot scan works Linux direct, unverified/failed on Mac/Dell.
- EXACT FIX: gate expands aliases (`b->build c->check t->test r->run d->doc`) in `splitCarriedArgs`/
  wrapper before signing (ledger records sent argv); or broker `cargo_capability` accepts aliases.
- Proof test: carried `cargo b -p a` verifies with signed argv `[build,...]` in
  `carried-cargo.test.mjs`; real/client classification `cargo b --locked` == build `compile=true`.

### M16. Compile-class list too narrow: bench/run/rustdoc/fix/package/nextest compile at broker
- Gate: `snapshot-request.mjs:52 COMPILE_CLASS_COMMANDS=[check,clippy,build,doc,rustc,test,b,c,t,d]`
  (probe: bench/run/rustdoc/fix/package/nextest all false) gates freshness/attribution/toolchain
  requirement at `:514`.
- Broker: `command_policy.rs:581-620 BUILD` includes `fix/publish/package/bench/rustdoc/run`
  (+nextest/ndk `:67-70`); `service.rs:7415` emits attribution/toolchain when `capability_compile`.
  Gate fixture COMPILE (`test-support/cargo-classification.mjs:20`) inconsistent.
- Failure mode: carried bench/run/package receipt missing freshness/attribution/toolchain passes gate
  verification silently (weaker than broker contract). Critic: untouched in WIP (still narrow).
- EXACT FIX: `snapshot-request.mjs:52` set = broker BUILD set
  `[build,check,test,bench,clippy,doc,run,rustc,rustdoc,fix,publish,package,nextest]` minus
  observation-only package, derived from classification not a hand list.
- Proof test: `snapshot-request.test verifySnapshotReceipt` bench/run receipt without freshness fails
  evidence check.

### M17. Test-run cap ships ON (10 min) vs gate quiet-scan budget 26 min [+slice10 merged]
- Gate: `deterministic-scans.mjs:1294 test:rust` runs `cargo test -p <c> --locked --no-fail-fast` as a
  snapshot broker root; `:1659 QUIET_SCAN_MAX_MS=26min`, `:1666` scan timeout 30/60min expects a quiet
  cargo test suite may run up to 26min; inventory `GATE-CLASS-A-INVENTORY.md:50` keeps it in-broker.
- Broker: `job_service/test_run.rs:37 cap_for` root `cargo test` (10 min ship-on `:6`) kills run after
  `Finished`; `:97 quiet_argument` refuses `-q/--quiet/config-quiet` pre-spawn; default
  `config.rs:38 DEFAULT_TEST_RUN_CAP_MS=600_000`. Docs: `PIPELINE:116` says cap ships off (wrong),
  `using-the-broker.md:89-91` says on (right).
- Failure mode: unit suite >10 min after compile, or any carried `cargo test -q`, fails Mac/Dell only;
  legit quiet suite running 11–26 min post-compile killed `RK_TEST_RUN_CAP` exit 124 while gate
  assumed it could finish. Gate has no cap/quiet pre-check or scan-timeout alignment.
- EXACT FIX: `deterministic-scans/snapshot-request`: either gate test scans compile with
  `test --no-run` in broker + run binaries outside (`app-check.mjs:598` G22 pattern), or broker
  raises/exempts cap for snapshot-bound gate scans and gate refuses `-q` test argv with RK code;
  either way encode one shared number. Also fix `PIPELINE P7` text (ships on for `*`, 0 disables).
- Proof test: fake-broker `test_run` cap model in `cargo-classification.test`; carried `test -q`
  refused gate-side; long test scan plan shows `--no-run` split; broker `cargo test` sleeping 11 min
  after Finished under `RUSTRIGHT_QUIET_LEASE` reaches gate verdict pass.

### M18. Bare `cargo audit` seam: broker requires `--db <dir> --no-fetch`, gate sends bare
- Gate: `deps-cargo.mjs:50 runCargoAudits` sends bare `[audit]` via runCargoSync;
  `cargo-target-shim.mjs:46` audit bound with only Cargo.lock-exists check.
- Broker: `command_policy.rs:375 audit_offline_db` accepts only `audit --db <dir> --no-fetch`
  (either order) for snapshot/ClassA.
- Failure mode: bare audit inside any snapshot stage = must-compile refusal;
  `cargo-classification.test.mjs:105` explicitly exempts audit seam row from `acceptedAfterFix`
  assertion, hiding it.
- EXACT FIX: `deps-cargo.mjs:50` sends `[audit,--db,<pinned advisory db>,--no-fetch]` (same pin as
  `deterministic-scans.mjs:1138`), or document and enforce deps-audit never runs under a carrier
  (refuse RK code if map present).
- Proof test: remove `:105` audit exemption; seam audit row verifies against fake broker.

### M19. Env spellings: gate reads RIGHTKIT_-only roots/socket, broker prefers RUSTRIGHT_
- Gate: `managed-roots.mjs:9-11,25,32` reads ONLY
  `RIGHTKIT_DATA_ROOT/RIGHTKIT_MANAGED_ROOT/RIGHTKIT_EXTERNAL_ROOT/RIGHTKIT_CONTROL_ROOT/
  RIGHTKIT_BUILD_BROKER_SOCKET` (also `cargo-contract.mjs:34` brokerManagedHost socket check
  RIGHTKIT_-only).
- Broker: `managed_roots.rs:84-87 env_named` reads `RUSTRIGHT_<name>` FIRST, `RIGHTKIT_` only as
  legacy fallback (client same: `shims.rs:88-115`).
- Failure mode: host/env flipped to RUSTRIGHT_-only (broker task L-1 removes legacy) → gate resolves
  legacy default roots while broker/client use RUSTRIGHT_ roots; gate `outputRoots`
  (`snapshot-broker.mjs:34,65-72` = `<gate dataRoot>/rightkit-build-control/ws`) then mismatch broker
  `outputDir` → `isGenerationTarget` false → every snapshot receipt rejected; socket discovery also
  misses. Severity depends on host env + L-1 schedule (unresolved).
- EXACT FIX: in `managed-roots.mjs` add `envNamed(env,name)=env[RUSTRIGHT_${name}]||
  env[RIGHTKIT_${name}]` and use for DATA/MANAGED/EXTERNAL/CONTROL/SOCKET in `resolveManagedRoots`,
  `managedControlCandidates`, `discoverBrokerSocket`; same in `cargo-contract.mjs:34`.
  Critic: WIP reads `RUSTRIGHT_BUILD_BROKER_SOCKET` first for socket only — extend to all five.
- Proof test: gate unit `resolveManagedRoots({platform:'darwin',env:{RUSTRIGHT_CONTROL_ROOT:'/x',
  RUSTRIGHT_MANAGED_ROOT:'/d'}})` returns `/x,/d` and `discoverBrokerSocket(
  {RUSTRIGHT_BUILD_BROKER_SOCKET:'/s'})` returns `/s` (both fail today).

### M20. Folder names: gate omits renamed RustRight roots broker tries
- Gate: `managed-roots.mjs:21-29 managedControlCandidates=[controlRoot,dataRoot,clientRoot,
  legacyRoot]` only.
- Broker: `managed_roots.rs:171-179` also tries renamed roots (`~/Library/Application
  Support/RustRight/broker`, `/Volumes/D/.rustright`; `identity.rs:47-62` renamed module).
- Failure mode: after `rustright-install cutover --migrate-roots` with forwarding alias gone, gate
  `discoverBrokerSocket` (used `managed-cargo.mjs:125`, `development.mjs`, `local-runtime.mjs:175`)
  finds no `broker.sock` → `RK_BROKER_UNAVAILABLE` while rustright client finds broker fine.
- EXACT FIX: append renamed candidates in gate `managedControlCandidates`: darwin
  `<home>/Library/Application Support/RustRight/broker` and `/Volumes/D/.rustright`, win32
  `D:\.rustright` (after legacy, deduped, mirroring broker order).
- Proof test: mirror broker test `managed_roots.rs:369-377` in `managed-roots.test.mjs`: `exists()`
  true only at `<renamed>/broker.sock` must be discovered (fails today).

### M21. Signed-coverage: gate compared top-level fields, broker signs nested resultEvidence [FIXED-IN-WIP]
- Gate (HEAD `5c388c71e`): `snapshot-request.mjs:45 SIGNED_TOP_LEVEL_FIELDS=
  [verdict,refreshed,exit,signal,timingsMs,diagnosticsPath]` and `:545` compares `payload[name]`
  (top level); test fake `test-support/fake-broker.mjs:138` spreads `...top` into payload top level,
  masking it.
- Broker: `job_service/service.rs:2572 payload[resultEvidence]={exit,signal,timingsMs,
  diagnosticsPath}` nested, never top-level; protocol `attestation.rs:260 covered()` checks
  `payload.resultEvidence`.
- Failure mode at HEAD: loop dead — tampered receipt `timingsMs/diagnosticsPath` accepted (exit/signal
  only partly cross-checked vs signed result `:470/:543`); payload with no `resultEvidence` accepted,
  where Rust `verify_receipt` refuses. Critic: SUPERSEDED-BY-WIP — working tree signs
  `resultEvidence{exit,signal,timingsMs,diagnosticsPath}` (`fake-broker.mjs:148-154`,
  `snapshot-request.mjs:548-553` requires it). Land the WIP.
- EXACT FIX: gate `snapshot-request.mjs`: require `isObject(payload.resultEvidence)` and
  `same(receipt[name],payload.resultEvidence[name])` for exit,signal,timingsMs,diagnosticsPath; drop
  verdict/refreshed from list and refuse them inside `payload.resultEvidence`; fix
  `fake-broker.mjs:138` to nest top under `resultEvidence` (done in WIP — commit as-is).
- Proof test: `snapshot-request.test` tamper `timingsMs.cargo/diagnosticsPath` on receipt copy only
  (real-shape payload) must fail evidence; fake-broker receipts still verify.

### M22. Freshness presence: broker may omit it on timeout, gate requires it [+slice03 merged]
- Gate: `snapshot-request.mjs:513 compile=adapter===null&&isCompileClass` and `:517` requires
  freshness+attribution+toolchain all non-null.
- Broker: `service.rs:7138 build_evidence` bounded 30s (`EVIDENCE_SCAN_DEADLINE service.rs:91`) →
  `evidence=None` on timeout, and `:7409` freshness emitted only if evidence Some, while
  attribution/toolchain still emitted `:7415`; `build_evidence` also None when `target_at_start` None
  (`service.rs:6824`, set only `:5677` if workspace Some). Slice 03 minor corroborates (slow/huge
  census → gate unverified though broker designed receipt to survive).
- Failure mode: huge/slow target census >30s → signed receipt lacks freshness → gate unverified,
  scan/stage fails though compile succeeded.
- EXACT FIX: broker `service.rs finish()`: on evidence timeout/None for `capability_compile` emit
  signed freshness `{available:false,reason:evidence-scan-timed-out}` instead of omitting; gate accepts
  `available:false+reason` as present (validate shape like attribution `:524`).
- Proof test: broker test forcing EVIDENCE timeout produces receipt `verifySnapshotReceipt` accepts
  (node) with `freshness.available=false`; fake receipt compile with freshness unavailable-reason
  verifies, absent-with-no-reason still fails.

### M23. Declared digest Windows separator: gate always `/`, broker hashes `\` on Windows
- Gate: `compile-inputs.mjs:20 judgeCompileInputs` joins hashed names with `/` always;
  `stage-ledger.mjs:167 declaredInputProblems` compares `judge()` digest to receipt digest.
- Broker: `check_admission.rs:1198 copy_input name=name.join(child)` (Path, `\` on Windows) hashed
  `:1209`. Gate's other implementation `snapshot-request.mjs:292` correctly uses platform sep.
- Failure mode (Dell/win32): any declared input directory (or nested path) digest differs `/` vs `\`
  → stage-ledger declared-input problem, stage fails.
- EXACT FIX: `compile-inputs.mjs inputFiles`: join hashed name with platform separator (win32 `\`
  else `/`), or delegate to snapshot-request `declaredInputDigestsOf`.
- Proof test: `compile-inputs.test judgeCompileInputs(root,{platform:win32-simulated sep})` nested
  dir sha256 == `declaredInputDigestsOf(...,{platform:'win32'})` and != posix digest.

### M24. Direct census divergence: gate FingerprintLog lacks cutoff/cause/scope/cap of broker
- Gate: `freshness-census.mjs:94 FingerprintLog` has no execution-status cutoff and no Caused-by/
  FingerprintMissing and reasons carry no cause (`:115`); `fingerprintDirectories :152` walks ALL
  profiles/triples; cap `:27 250_000`.
- Broker: `cargo_evidence.rs:94` cutoff at first Compiling/Fresh/..., `:105` Caused-by parsing, `:152`
  reason `{kind,cause}`; `cargo_freshness.rs:247` census only argv profile+triple dirs; `service.rs:
  138` cap 200_000.
- Failure mode: Linux direct census counts nested-cargo reasons and all-profile units, missing
  fingerprint reads `kind=failed`; direct vs broker freshness for same compile incomparable.
- EXACT FIX: port broker FingerprintLog semantics (EXECUTION_STATUS cutoff, awaiting_cause, cause_of)
  and census scope (profile+triple) and cap 200_000 into `freshness-census.mjs`.
- Proof test: shared fingerprint-log fixture fed to gate FingerprintLog and broker test vectors yields
  identical `{unit,kind,cause}` incl post-Compiling nested lines ignored.

### M25. Dev/test sidecars never staged by door
- Gate: `bin/rightkit-gate.mjs:1397` compile and `:1415` package call `stageSidecarsOnce` before
  building, but `:2188-2215 runDevTest` goes check → right-release dev|test with no
  `stageSidecarsOnce`; `development.mjs:159-163` stages only `stageDevelopmentBinaries`, never
  sidecars.
- Failure mode: fresh-snapshot `--to dev` for Tauri app whose externalBin sidecars are declared
  top-level (gate-staged at compile) fails build-script missing
  `src-tauri/binaries/<name>-<triple>`; works only if app dev target duplicates a stage-sidecar
  prepare step.
- EXACT FIX: in runDevTest after `openSnapshotStage` call `stageSidecarsOnce({configs:
  sidecarConfigsOf-equivalent for appDir, stage: to, executor})` before `carrierForChildren`,
  mirroring line 1415.
- Proof test: door test fixture — sidecar app, fresh snapshot, `--to dev` with no prepare step passes
  and ledger shows staged entries.

### M26. Test QA journeys have no snapshot form (stage unusable for those apps)
- Gate: `build-launch.mjs:74-84 qaBuildReason` — journeys run ALWAYS builds (no prebuilt mode); `:202`
  refuses it on snapshot route.
- Broker: `service.rs:4017-4022` uncarried compile from snapshot refused, so QA-internal
  `rustright cargo --no-run` cannot run there either.
- Failure mode: any `tests.suites` entry using `rightkit-qa journeys run` can never pass `--to test`
  on broker hosts (designed refusal, but stage unusable for those apps).
- EXACT FIX: extend qa-snapshot plan (`build-launch:205 qaSnapshotPlan`, run-tests pattern) to
  journeys — carried `--no-run` build + listing under carrier managed dir, then direct QA; or add
  `rightkit-qa --from-cargo-json` journeys mode.
- Proof test: `--to test` fixture with journeys suite on broker host.

### M27. Package assemble-app prepare bypasses door (unledgered / refused)
- Gate: `assemble-app-command.mjs:48-55 runPrepare` spawns via development `resolveCommand` directly;
  `:115` calls it for `--build`.
- Broker: `service.rs:4017-4022` root request inside managed snapshot refused. Gate
  `development.mjs:454 resolveCommand` returns bare cmd (cargo/custom) unchanged.
- Failure mode: `assemble-app --build` prepare.release cargo/custom steps on snapshot broker route
  either bypass broker from checkout cwd or die `RK_CHECK_ERROR` root-in-snapshot; never
  carried/ledgered.
- EXACT FIX: route runPrepare through buildLaunch exactly as `recipe-build.mjs:418`
  (`door(step.cmd,args,{env,stepEnv,cwd})`); keep `resolveCommand` only for own-right-release.
- Proof test: `assemble-app --build` fixture with cargo prepare step under carrier map asserts one
  carried ledger entry.

### M28. Compile/package/dev cmake and arbitrary-command sidecars refused at door
- Gate: `stage-sidecar.mjs:1723-1729` cmake configure/build via runStep, `:1563` command-kind via
  runStep, `:833-847` runStep delegates to buildLaunch; `build-launch.mjs:242` refuses
  cmake/make/python/custom.
- Broker: `service.rs:3768-3773` snapshot adapter must be registered compile+snapshot; none for cmake
  in gate ADAPTER_PROGRAMS (`build-launch:64`).
- Failure mode: apps with cmake sidecars (whisper-cpp class) or command sidecars with non-door tools
  fail `STAGE_SIDECAR_STEP_REFUSED` at first staging on broker hosts, all stages.
- EXACT FIX: register `rightkit-cmake` (tool cmake, compile+snapshot, flags
  `-S/-B/-D/-G/-A/--build/--config/--target/--parallel`) in broker host config + add cmake branch in
  buildLaunch/ADAPTER_PROGRAMS; for command kind refuse at config validation naming admissible cmds.
- Proof test: `stage-sidecar` cmake fixture staged under `--to compile` on Mac+Dell broker.

### M29. Dev artifact hand-back: bundleDirectory/source inside snapshot miss broker output
- Gate: `development.mjs:344-355 bundleDirectory` must stay inside app root and installer read from
  there; `stage-binary.mjs:36` source entries resolved under snapshot root.
- Broker: `service.rs:5403-5406` compile-adapter `CARGO_TARGET_DIR` is managed layout target
  (per-generation); Tauri bundles land there not in snapshot. Gate itself knows this at
  `development.mjs:389-390` (`carriedOutputDir preferNested`) but only when `bundleDirectory` omitted.
- Failure mode: `--to dev` install with declared `bundleDirectory` (or `stage[].source` under
  `target/`) finds 0 installers/ENOENT after successful broker build.
- EXACT FIX: in `resolveTauriBundleDirectory` when `carriedCargoMapOf(env)?.map`, ignore
  `bundleDirectory` and use `resolveBundleRoot` receipt path; in `resolveStageSource` translate
  snapshot `target/` prefixes via `carriedOutputDir`; or refuse both at
  `validateDevelopmentConfig` under carrier with actionable message.
- Proof test: `development.test` carried-env + `bundleDirectory` fixture resolves to receipt
  `outputDir`.

### M30. Native consume vs snapshot/dirty checkout and Windows git path
- Gate: `bin/rightkit-gate.mjs:1275` snapshot route — checkout may hold anything, never a build
  source. Gate has zero consume awareness (no CONSUME reference in `packages/release`).
- Broker: `job_service/consume_impl.rs:87-114 clean_head(primary)` requires no tracked change AND no
  untracked file; `:89` hardcodes `/usr/bin/git`; `:178-181` any enrolled root job failing it gets
  `RK_CONSUME_SOURCE`.
- Failure mode: consume-enrolled repo — every carried dev/package/compile job on Dell/Windows fails
  (no `/usr/bin/git`); on Mac fails whenever primary checkout dirty/untracked (normal gate usage)
  despite snapshot being exact commit.
- EXACT FIX (broker): consume_gate — for snapshot-bound jobs consume binding commit
  (`snapshot::Binding` commit/tree) instead of `clean_head(primary)`; resolve git via registered
  tool/PATH per-platform. Gate: routePlan surfaces enrollment via doctor.
- Proof test: broker consume test with dirty primary + snapshot-bound job on unix and windows git
  resolution.

### M31. Android cargo-ndk carried unlocked + unprobed registration
- Gate: `android-ndk.mjs:51,56-66` emits `cargo ndk ... build` with no `--locked`;
  `snapshot-request.mjs:67 LOCK_BOUND_COMMANDS` lacks `ndk` so `lockedArgv (:75-88)` never inserts it.
  Route probe `bin/rightkit-gate.mjs:1905` checks only 2 capabilities.
- Broker: `policy/src/command_policy.rs:1225-1227` unregistered cargo-ndk = `RK_UNKNOWN_COMMAND`.
  Gate `snapshot-request.mjs:397` tree-at-exit mismatch (rewritten Cargo.lock) discards result.
- Failure mode: android dev/package on broker host without cargo-ndk external-tool registration fails
  late after class A; with stale lock, ndk build rewrites snapshot Cargo.lock and gate voids job.
- EXACT FIX: `snapshot-request lockedArgv` — for `ndk` insert `--locked` after its cargo subcommand
  token (build/check); gate routePlan/doctor probe requires cargo-ndk external command registration
  when config declares ndk.
- Proof test: `snapshot-request.test lockedArgv` ndk case + carried ndk fixture job SUCCEEDED with
  signed `requestedArgv` containing `build --locked`.

### M32. Package standalone win-package-recipe signs inside one spawn tree
- Gate: `win-package-recipe.mjs:160-165` default deps spawn raw spawnSync; `:247-252` tauri via direct
  pnpm spawn; `:274-312` Authenticode sign-raw/sign-installer in same process.
- Gate contract: `build-launch.mjs:131-132` signing belongs to gate session, never broker job. Broker
  `service.rs:2347-2383` adapter env cleared to registered keys only.
- Failure mode: direct callers of `runWinPackageRecipe` as a node package step run as one
  rightkit-node carried job — Azure/credential-store signing executes inside broker-contained job with
  scrubbed env and no session, fails or signs with wrong identity. Mainline expansion
  (package-recipe-targets) avoids by splitting phases; exported recipe does not.
- EXACT FIX: `win-package-recipe defaultWinPackageDeps` — route tauri spawns through buildLaunch and
  refuse sign phases when `carriedCargoMap` present (name recipe-build split instead), or delete
  standalone runner keeping `validateWinPackage` only.
- Proof test: standalone recipe invoked under carrier env refuses before sign-raw naming split route.

### M33. Stage-sidecar nested-broker heuristic vs token attribution
- Gate: `stage-sidecar.mjs:1850-1866 isNestedBrokerContext` guesses from `TAURI_ENV_*` + env
  socket/PATH (`managed-cargo.mjs:98-102`); `assertNotNested` runs unconditionally at `:1905`.
- Broker: `service.rs:4071-4075` nested attribution is parent capability token; `:1382-1396`
  snapshot-adapter roots explicitly support nested children in signed list. Gate
  `build-launch.mjs:34-35` itself states never decide by env marker.
- Failure mode: Tauri beforeBuildCommand running stage-sidecar inside carried pnpm adapter job
  (supported nested route) can false-refuse `STAGE_SIDECAR_NESTED_BROKER`, or false-pass outside it
  and exit 125 at broker.
- EXACT FIX: stage-sidecar `assertNotNested` — skip refusal when `carriedCargoMapOf(env)?.map`
  (nested-under-adapter supported); off-carrier keep heuristic.
- Proof test: `stage-sidecar.test` nested context + carrier map present does not refuse.

### M34. Fake read-only allowlist models pre-fix broker (default flag wrong)
- Gate fake: `test-support/fake-broker.mjs:307` default fake (`readOnlySnapshots=false`) admits
  compile-only via `classifyCargo` and refuses rest with `SNAPSHOT_MUST_COMPILE`; and
  `carried-cargo.test.mjs:242` asserts carried metadata `broker refuses today`.
- Broker: `job_service/service.rs:1218` admits `!compiles` when `snapshot_read_only(argv)` at HEAD
  `b84ae60`.
- Failure mode: default fake models pre-fix broker; carried metadata/fetch/deny/audit/fmt paths only
  tested under opt-in flag; stale refusal expectations stay green while real broker accepts.
- EXACT FIX: fake-broker.mjs makes `readOnlySnapshots` default true (delete flag); update
  `carried-cargo.test.mjs:242` expectation to verified metadata receipt.
- Proof test: `cargo-classification.test.mjs` row run with no flag + real-broker carried
  `cargo metadata --no-deps` Mac run.

### M35. Fake read-only shape drift, both directions
- Gate fixture: `test-support/cargo-classification.mjs:60-99 acceptedAfterFix` — no fmt branch, no
  `--version/locate-project/read-manifest`, fetch only exactly `[--locked]` and `words[0]` subcommand
  (no `+toolchain`), about = any about+`--frozen` (line 63), metadata exact-flag-set/spaced/
  format-version=1 (83-85).
- Broker: `policy/src/command_policy.rs:460` accepts `--version|-V|version|locate-project|
  read-manifest`; `:461` metadata `--no-deps` OR `lock_pinned_offline` (any extra flags); `:462-469`
  about only `generate`+`lock_pinned_offline` after generate; `:405` fetch|metadata `--locked` OR
  `--frozen` via `class_a_scan`; `:406+417` fmt `-p X --check`|`--all --check`; `:347` deny via
  `command_index` (skips `+stable`).
- Failure mode: fake refuses carried fmt-check/version/locate/`+stable` fetch real accepts (false
  reds/workarounds) and accepts `about --frozen` non-generate real refuses (green test, real
  `RK_CHECK_ERROR`). Also delete gate comment `snapshot-request.mjs:507` claiming `tree --locked`
  carried (real: no tree arm at `command_policy.rs:403-470`).
- EXACT FIX: rewrite `acceptedAfterFix` as transliteration of
  `offline_read_only+class_a_scan+deny_check_files+audit_offline_db+fmt_check_shape` incl
  `command_index`/`+toolchain` handling.
- Proof test: shared argv table test — same ~60 argv vectors asserted in
  `cargo-classification.test.mjs` and rustright-policy tests (`command_policy.rs:840-1074`).

### M36. Fake receipt stage evidence always present and wrong shape
- Gate fake: `test-support/fake-broker.mjs:143 signedReceipt` always signs
  `freshness{unitsAtStart:3,fresh:3,notFresh:0,invalidationReasons:[]}+attribution{available:true,
  rows:[]}+toolchain` for cargo AND adapter, asserted `snapshot-broker.test.mjs:410`.
- Broker: `service.rs:7409-7421` freshness only when `build_evidence` Some, attribution+toolchain only
  root cargo compile (never adapter); `:6834/6855/6860-6862` freshness can be
  `{available:false,reason:...}` and `:6880 invalidationReasons:null`; attribution real shape
  `cargo_evidence.rs:410-433 {available,source:"cargo-json",units:[{package,packageId,target,kind,
  errors..}],truncated}` compacted `:297-311` to counts — never rows.
- Failure mode: gate ledger/admission consumers coded against `rows[]`/always-fresh census never see
  unavailable/null/adapter-absent shapes; unexplained-cold path unexercised — fake freshness always
  fully fresh.
- EXACT FIX: fake `signedReceipt` default — compile cargo → real census shape
  `{available:true,source:"fingerprint-census",fresh,notFresh,unitsAtStart,censusMs,
  invalidationReasons:[{unit,kind}],coldCause:null}`, attribution
  `{available:true,source:"cargo-json",units:0,packages:0,errors:0}`, adapter/read-only → no
  attribution/toolchain keys; add tamper `freshnessUnavailable`.
- Proof test: `snapshot-request.test` verify against recorded real receipt fixture + adapter receipt
  with no stage keys.

### M37. Fake decouples client exit from signed result (impossible green state)
- Gate fake: `test-support/fake-broker.mjs:151` result always SUCCEEDED/exit0 while `:335` exits
  `behavior.exit` arbitrary, asserted `carried-cargo.test.mjs:87-88` (`exit:3` call `receipt still
  verifies`).
- Broker: `service.rs:2543-2548` signed result = status/exitCode of actual job; `:7358` receipt
  exit = `details.exit_code`; client exit = terminal RESULT exitCode (`cli/src/exit_status.rs:44-46`).
- Failure mode: impossible state (cargo failed, broker signed success) is green; real failing compile
  yields signed FAILED → gate unverified, different ledger/stage outcome than test teaches.
- EXACT FIX: fake client — derive signed result from `behavior.exit` (`exit!=0` → result FAILED/
  exitCode=exit, `top.exit=exit`); change `carried-cargo.test:88` to expect unverified.
- Proof test: carried build with exit 101 asserting `RK_CARGO_BROKER_UNVERIFIED` signed FAILED, plus
  real-broker failing-crate run.

### M38. Fake has no doctor/capability probe (route gate untested)
- Gate fake: `test-support/fake-broker.mjs:357` any command but cargo/run/status/snapshot-lease =
  `unsupported command` exit 2 — no `doctor`.
- Gate prod: `bin/rightkit-gate.mjs:1910+1917 REQUIRED_BROKER_CAPABILITIES=
  [snapshot-readonly-v1,snapshot-adapter-v1]` read via `rustright doctor --json` `.broker.
  capabilities`. Broker: `cli/src/doctor.rs:249-257` probes STATUS runtime-health-probe and
  `:353-365` renders `broker.capabilities` from HANDSHAKE, constants
  `protocol/src/response.rs:58,67,70`, emitted `service.rs:1551`.
- Failure mode: every snapshot-route test bypasses probe via injected brokerCapabilities/second shell
  fixture (`door-route.test` fixtureClient); fake-broker hosts can never exercise
  `RK_BROKER_CAPABILITY_MISSING` vs real doctor JSON shape/exit-1-but-JSON.
- EXACT FIX: fake-broker client adds `doctor --json` emitting `{healthy,broker:{endpoint,reachable,
  phase,error,admissionPaused,capabilities:[journal-v1,snapshot-readonly-v1,stage-grant-v1,
  snapshot-adapter-v1]},runtime:{version}}` + `behavior.capabilities` override; route one door test
  through makeFakeBroker client on PATH with production carrier.
- Proof test: `rightkit-gate --to dev --route` with fake client `capabilities=[]` →
  `RK_BROKER_CAPABILITY_MISSING`, full set → snapshot route.

### M39. Version pins stale and brokerVersion unpinned (upgrade = provenance refusal, no warning)
- Gate pins: `ci/rk-pipeline.json:142,150-151` darwin and `:158,166-167` win32 pin
  `brokerVersion=rustright-5af765b`/`brokerSourceRevision=5af765bd..` and brokerDigests
  `1698bb../ed3b4c..` at gate HEAD `b8ae3c677`, vs broker HEAD `b84ae60` (post-5af765b fixes
  eadb4ba/849091b). Receipt `brokerVersion=runtime.version` (`service.rs:7281`).
- Gate checks: `broker-pins.mjs:42-47 validatePins` keeps only host/keyId/publicKey/brokerDigest
  (drops brokerVersion/clientPath); `snapshot-request.mjs:539` checks brokerVersion merely non-empty
  — fake `rustright-test` (`fake-broker.mjs:136`) passes. Only pin test
  `broker-pins.test.mjs:170-179` checks well-formedness. Critic live proof: `rustright --version`
  prints `0.1.0` (no sha).
- Failure mode: broker upgrade to `b84ae60` on Mac/Dell → payload brokerDigest ≠ pin →
  `provenance-verify.mjs:104 'runtime'` refusal of every receipt until re-enroll+rk-pipeline edit, no
  test warns; conversely any broker version with enrolled digest passes unexamined.
- EXACT FIX: after installing `b84ae60` broker, re-enroll both hosts and update `ci/rk-pipeline.json`
  brokerDigest/brokerVersion/brokerSourceRevision from runtime manifest; add brokerVersion to
  validatePins record (optional-compare in `verifySnapshotReceipt` when pin carries it).
- Proof test: `broker-pins.test` asserting committed pins' brokerVersion equals
  `rustright doctor --json`.runtime.version on broker hosts (opt-in real test) + unit: receipt with
  other brokerVersion vs pin → refused.

### M40. Client discovery divergence: PATH-first vs installed-runtime-first
- Gate route: `bin/rightkit-gate.mjs:1918,2063,2696,2747` client = PATH `which/where rustright` first
  absolute hit.
- Gate cargo: `managed-cargo.mjs:54 managedRuntimeClient` resolves installed
  `runtime-current|~/Library/Application Support/RustRight/runtime/rustright|D:\.rustright\runtime\
  rustright.exe`. `ci/rk-pipeline.json:138,154 attestation.clientPath` read by NO gate code (grep:
  only suite-api.generated clientPaths). Broker refuses runtime-identity digest mismatch
  (`server/gate_tests.rs:203-213`). Fake `sb.client` is temp script (`fake-broker.mjs:197-198`) with
  no identity/socket.
- Failure mode: PATH-first rustright can be agent shim/dev build/stale client whose clientDigest or
  default socket ≠ installed broker → REJECTED digest mismatch or wrong endpoint on real run;
  fake cannot surface it.
- EXACT FIX: rightkit-gate `carrierBroker/queryBrokerCapabilities` resolve via
  `managedRuntimeClient()` first, PATH only as fallback, and log resolved client path; or consume
  pins clientPath in `loadBrokerPins`.
- Proof test: gate test with PATH containing decoy rustright + installed-layout client asserting
  installed one chosen; real `rustright doctor` via gate-resolved path on Mac+Dell.

### M41. Fake adapter registry and grammar reimplemented, swift scratch missing
- Gate fake: `test-support/fake-broker.mjs:396` hardcoded `ADAPTERS=[rightkit-pnpm,node,qa,swift]`,
  `:400-432 snapshotArgvProblem` restricts only pnpm (exec/run/pack) and swift (spaced relative
  `--package-path`, rejects `=` and any `..`), node/qa unrestricted.
- Broker: registry is host config `AdapterRegistry` (`policy/src/adapter.rs:110`) judged by
  `check_argv:480/snapshot_argv_check:637/check_environment:746`; swift accepts `--package-path=` and
  `./x/../` resolving inside (`adapter.rs swift_package_dir ~698`, eadb4ba test); broker appends
  `--scratch-path <outputDir>/swiftpm/<sha256>` + `swiftpm-scratch` transformation
  (`service.rs:5929,5941-5944`, `b84ae60` mutable `spawn_argv`).
- Failure mode: dev/package stages (never run on real broker) green against invented grammar — real
  broker denies unregistered-flag node/qa argv fake accepts, and gate has no evidence swift
  output/bin-path reads from broker scratch dir not snapshot `.build`. Critic ok-verified: installed
  Mac pnpm grammar (15 subcommands/15 flags) identical to gate constant; swift/node registrations
  match gate argv except caller `--scratch-path` still denied (M-slice05 stands).
- EXACT FIX: fake — load adapter list+snapshotSubcommands from behavior fixture mirroring installed
  registry dump; implement swift `=`/canonical package-path and record appended `--scratch-path` in
  adapterCalls; gate swift consumer reads bin path via broker scratch (verify vs `service:5929`
  layout).
- Proof test: real-broker carried rightkit-swift build + `build --show-bin-path` sharing one swiftpm
  dir; fake parity table vs `adapter.rs` tests.

### M42. Two disagreeing fake metadatas, neither is cargo
- Gate fakes: `test-support/fake-broker.mjs:365-388 synthMetadata` regex-parses TOML,
  `target_directory` always `fx.outputDir`, `publish:null`, no exclude/default-members/
  workspace-inheritance; vs `test-support/broker-guard.mjs:46-98 METADATA_SCRIPT` (tomllib) emits at
  most ONE package, `workspace_root`=manifest dir always, `targets:[]`, `features:{}`.
- Broker: `tests/end_to_end.rs:124-130 FAKE_CARGO` metadata answers only locate-project; broker itself
  runs registered real cargo.
- Failure mode: gate workspace discovery/member/target logic validated against regex/single-package
  inventions; real metadata (virtual workspace, inherited version/edition, renamed deps,
  `target_directory`=broker generation target only for carried calls) diverges per-app — classic
  one-per-real-run mismatch source.
- EXACT FIX: replace both synths with one fixture — run real `cargo metadata` once at fixture build
  on CI/hosted (guard already allows CI cargo) or ship recorded metadata JSON per fixture workspace;
  broker-guard stub must emit all workspace members.
- Proof test: `snapshot-metadata.test` against recorded metadata of a virtual workspace with
  exclude+default-members; diff guard stub vs `cargo metadata --no-deps` on rightkit workspace in CI.

### M43. No shared contract test on either side (split-brain structurally invisible)
- Gate fake reimplements client+broker in JS (`fake-broker.mjs:265-358`).
- Broker: `tests/end_to_end.rs:240-261` only MANAGED_AGENT cargo metadata over socket (no snapshot
  carrier, no attestation, no STATUS receipt, no adapter); server tests use Scripted/echo fake Dispatch
  (`server/test_support.rs:62-70`, `gate_tests.rs:58-86` data-root only); CLI has own fake broker
  (`rustright-cli/tests/fake_broker.rs`); gate `RIGHTKIT_TEST_REAL_BROKER=1` opt-in
  (`broker-guard.mjs:13,30`) used by no carried test (only `release-artifact-path-real.test.mjs`).
- Failure mode: each side's tests prove self-consistency with its own fake; this slice's drift is
  structurally invisible until real Mac/Dell runs.
- EXACT FIX: shared fixture — rustright publishes test-vectors (carrier bytes, signed
  receipt+payload, STATUS envelope, doctor JSON, nestedChildren row) generated by real broker code
  under `crates/rustright-protocol/tests/vectors/`; rightkit vendors pinned copies in
  `packages/release/test-support/contract-vectors/` and asserts `verifySnapshotReceipt`+`receiptSync`
  accept them and `writeSnapshotRequest` bytes hash-match; broker asserts gate carrier vector admits.
- Proof test: vector test in both repos CI + one opt-in Mac e2e — gate `--to compile` on fixture repo
  via `RIGHTKIT_TEST_REAL_BROKER=1`.

### M44. Windows endpoint discovery: no implicit pipe default either side
- Gate: `managed-roots.mjs:34 discoverBrokerSocket` returns undefined on win32;
  `snapshot-broker.mjs:73-76` spawns resolved `.exe` directly bypassing `rustright.cmd`.
- Broker: `managed_roots.rs:210 discover_broker_socket` returns None on Windows unless
  `RUSTRIGHT_BUILD_BROKER_SOCKET` set; broker binds config `windowsPipe` (`server/broker.rs:326`);
  `.cmd` shim (`cli/src/install.rs:197`) sets only `MANAGED_AGENT_SHIM`, never socket env.
- Failure mode: fresh Dell shell without manually-set env — every client call fails
  endpoint-not-configured/`RK_BROKER_UNAVAILABLE`. Proven Dell runs imply an ambient env setter
  outside gate+installer code (unresolved: who sets it).
- EXACT FIX: `discover_broker_socket` Windows branch returns `identity::WINDOWS_PIPE`
  (`\\.\pipe\rustright-broker`, `identity.rs:14`) as implicit default; mirror in gate
  `managed-roots.mjs:34`.
- Proof test: Dell `cmd /c set RUSTRIGHT_BUILD_BROKER_SOCKET=& rustright doctor --json` succeeds
  against running broker.

### M45. Cancellation/signal propagation: blocking spawnSync never sends cancel
- Gate: `bin/rightkit-gate.mjs:2904` SIGINT/SIGTERM handlers only set AbortController; carried work is
  blocking `spawnSync` (`snapshot-broker.mjs:194`) so JS handler cannot run mid-call and gate never
  sends `rustright cancel` (no cancel call in `snapshot-broker.mjs`).
- Broker client: `cli/src/signals.rs:48` arms own SIGINT/SIGTERM→CANCEL only if signal reaches client
  process itself. Broker cancels abandoned job only after disconnect grace
  (`job_service/journal_impl.rs:39` 15min; `service.rs:3266`).
- Failure mode: `kill <gate-pid>` (not pgrp Ctrl-C) or Windows stop — broker job keeps compiling,
  orphaned client keeps watching after gate SIGKILL, slot+snapshot held ≥15min.
- EXACT FIX: make carried spawn async in `snapshot-broker.mjs`; on abort read requestId file and spawn
  `rustright cancel <id>`, and kill client process tree; register handlers before stage work in
  runStages path too.
- Proof test: start gate compile, SIGTERM gate pid only, assert broker STATUS=CANCELLED within 10s Mac
  and Dell.

### M46. Gate short timeouts vs Windows PIPE_BUSY_WAIT (spurious unverified)
- Gate: `snapshot-broker.mjs:50 receiptSync` timeout 60_000; `bin/rightkit-gate.mjs:1920` doctor
  timeout 20_000; `build-ownership.mjs:163` lease-status run timeout 60_000.
- Broker client: `cli/src/client.rs:103 PIPE_BUSY_WAIT=120s` retries ERROR_PIPE_BUSY internally;
  broker keeps 8 pipe instances (`server/listener.rs:68`).
- Failure mode (Dell): pipe burst >20/60s — gate kills status/doctor client that would have connected
  at ~120s → spurious receipt-not-ready/`RK_BROKER_CAPABILITY_MISSING`/lease UNKNOWN (held ownership).
- EXACT FIX: raise gate timeouts to ≥130_000 for status/doctor/lease calls, or lower PIPE_BUSY_WAIT
  below 20s and surface a retryable busy code gate retries.
- Proof test: occupy all 8 Dell pipe instances 90s, run gate doctor/status — must succeed, not
  unverified.

### M47. Broker-down reconnect amplification: N serial calls × 15 min
- Gate: `snapshot-broker.mjs:194` each carried call spawnSyncs client with gate env unchanged.
- Broker client: `cli/src/client.rs:411` default reconnect budget 15min per outage (`:607` budget
  restarts per outage). Gate never health-probes before a stage (no health.json read in gate slice;
  READY probe exists broker `service.rs:1578` STATUS runtime-health-probe).
- Failure mode: broker down mid dev/package stage — each of N serial carried calls burns up to 15min
  reconnect → hours before stage fails, ownership held whole time.
- EXACT FIX: in `snapshot-broker.mjs` set `RUSTRIGHT_RECONNECT_WAIT_MS` (`client.rs:433` env) to small
  bound for carried calls after first failure, and preflight `rustright doctor` once per stage in
  rightkit-gate runStages.
- Proof test: stop broker, run `--to dev` fixture with 3 carried calls — total fail time <2min with
  `RK_BROKER_UNAVAILABLE`.

### M48. Survivors Mac-only + opt-in off: detached processes wedge ownership elsewhere
- Gate: `build-ownership.mjs:1044` release requires broker lease REAPED and no covered processes; gate
  never sets `buildRules.stopSurvivors` (no occurrence in `packages/release` gate code).
- Broker: `config/src/config.rs:234 stop_survivors` default false;
  `job_service/survivors.rs:9` only macOS reports survivors; `:81` non-mac unix `start_ms=None` never
  selected; `:144` windows `stop()` no-op.
- Failure mode: first dev run leaves resident/detached process (`cargo run` server) on Dell or Linux —
  broker never stops it, gate ownership held until reboot/manual kill.
- EXACT FIX (broker): implement windows platform `start_ms/stop` in `survivors.rs:135` via Job Object
  enumeration in rustright-process and enable `stopSurvivors` for snapshot-bound jobs regardless of
  repo flag.
- Proof test: Dell broker job spawning detached marked survivor — at job end survivor terminated and
  gate ownership releases clean.

### M49. Cloud route doc-doc contradiction
- `rightkit/docs/GATE-CONTRACT.md:11` `cloud has no broker and never runs through it` vs
  `rustright/docs/BROKER-CONTRACT.md:9,53,230` `cloud builds go through the broker` (B35 expects
  admission). Code: gate-executor Linux always direct (`GATE-CLASS-A-INVENTORY.md:164`).
- Failure mode: cloud/Linux run planned from BROKER-CONTRACT waits for a broker that must not exist;
  B35 unsatisfiable.
- EXACT FIX: `BROKER:9,53,230` scope cloud sentence to Mac/Dell-hosted cloud agents or delete per GATE
  precedence rule `BROKER:19` (gate wins).
- Proof test: doc-drift grep in `gate-contract.test` for cloud+broker claims in both contracts.

### M50. Carrier wire format undocumented in gate contract (missing nonce/argvSha256/callingGate)
- `GATE-CONTRACT.md:88-95` request table lists only commit/snapshot/declaredInputs/ownership vs actual
  carrier `snapshot-request.mjs:205-216 {schema rustright.snapshot-request/1, primaryCwd, argvSha256,
  nonce, declaredInputs:{inputs}}` in `<controlRoot>/snapshot-requests :201`, env
  `RUSTRIGHT_SNAPSHOT_REQUEST={path,sha256}` 4096B/4MiB (`:6-8,32-33`),
  `RUSTRIGHT_CALLING_GATE` required (`snapshot-broker.mjs:156-157`), broker `request.rs:224-228`.
  `GATE:13` names schema only.
- Failure mode: implementer from locked GATE text alone produces inadmissible requests
  (missing nonce/argvSha256/primaryCwd/callingGate) — the one-per-real-run mismatch pattern.
- EXACT FIX: add carrier/env/limits table to GATE §1 Broker interface mirroring
  `snapshot-request.mjs:1-18` header.
- Proof test: doc test asserting every carrier key and `RUSTRIGHT_*` env in `snapshot-request.mjs`
  appears in `GATE-CONTRACT.md`.

### M51. Receipt shape vs signed evidence: attribution/toolchain/nestedChildren/snapshotEvidence in no doc
- `GATE-CONTRACT.md:97-109` flat receipt list and `BROKER-CONTRACT.md:96-98` (+profile/stage/resources/
  environment `as defined in gate Returned evidence GR32`, fields GATE never defines) vs actual signed
  payload `snapshotEvidence{...snapshot{path,treeAtStart,treeAtExit}}` (`snapshot-request.mjs:42,
  390-400`), `stageEvidence{freshness,attribution,toolchain}` (`:44,505-529`), nestedChildren for
  adapters (`:403-449,555`) — attribution/toolchain/nestedChildren/snapshotEvidence appear in NO
  contract doc (grep 0 hits).
- Failure mode: hosted admission/consumer written to contracts rejects or ignores real signed receipts;
  compile receipts missing attribution shape rules fail gate `:525-529` unexplained by docs.
- EXACT FIX: replace both receipt lists with signed-tree shape from `attestation.rs:244-260,275-280`;
  delete or verify BR19 profile/stage/resources/environment.
- Proof test: `provenance-real-receipt.fixture.json` keys diffed against documented receipt tree in
  `gate-contract.test`.

### M52. Attested-runs contract stale core (status, admission, env, transformations)
- `rustright/docs/contracts/attested-runs.md:3-6` `UNCOMPILED and UNTESTED...Nothing here is
  installed`; `:105-107` compile adapter takes `class A check admission, compileInPlace freeze`;
  `:115-116` broker refuses github-actions-only repos; `:313-318` env `RIGHTKIT_ATTEST_NONCE/
  RIGHTKIT_ADAPTER_ENV`; `:145` transformations kind enum only `cargo-target|cargo-target-dir|
  quiet-off` vs `using-the-broker.md:255,611` broker runs no gate and no longer reads `.rightgit`;
  broker `service.rs` transformations `swiftpm-scratch:5944 incremental-off:5455
  cargo-fingerprint-log:5520 native-parallelism:5569`; code env `RUSTRIGHT_*` (`shims.rs:72`,
  +legacy until L-1 `:94-99`).
- Failure mode: consumer (Legion audit cited `:8`) validating kind enum or freeze/admission semantics
  rejects every current receipt; `RIGHTKIT_` env breaks when L-1 lands.
- EXACT FIX: rewrite header status, §2 compile bullet (trustedEntry+snapshot bind only), delete
  `:115-116`, `RUSTRIGHT_` names primary, transformations open-ended list incl current kinds.
- Proof test: grep attested-runs for `compileInPlace/github-actions-only==0`; transformation kinds in
  `service.rs` subset of doc list (script).

### M53. Broker verdict-reuse sections self-contradict (retired behavior normative)
- `BROKER-CONTRACT.md:141-151,214(B19),312,315` broker runs `check --commit --unit --snapshot`,
  stores/reuses verdicts, re-runs pnpm-audit vs same doc `:30` D2 moved to gate + broker
  `request.rs:247-250 in_admission_check` retired (broker does not refuse/wait) +
  `attestation.rs:284`; `BROKER-FIRST-IMPLEMENTATION-ORDER.md:11,99-108` Seq3 reuse `proven only
  against stand-in gates` now deleted behaviour.
- Gate: `GATE:13 broker runs no gate`.
- Failure mode: implementer re-adds broker Class-A (forbidden) or plans E18/B19 on broker half that
  cannot exist.
- EXACT FIX: mark BROKER §4/body, B19, steps 2/5 SUPERSEDED 2026-10-29 with pointer to gate-owned
  reuse; FIRST-ORDER Seq3/5 status retired.
- Proof test: doc grep for `reusedFrom` outside retirement notes == 0 in both contracts.

### M54. Output-dir leaf and key wrong in gate doc (misses `t/` leaf, 16-hex vs 12-hex key)
- `GATE-CONTRACT.md:84 ws/<repo-key>/g/<gen>/target` (`repo-key=sha256[:16] :82`) vs
  `BROKER-CONTRACT.md:92` leaf `target` or `t` recorded in state.json leaves; gate
  `snapshot-request.mjs:354-364 <root>/<12hex>/g/<NNN 3+ digits>/{target|t}`, `:356-357` 12-hex
  `workspace_key` explicitly NOT snapshot 16-hex key; broker `gc.rs:715-716` leaf sibling logic.
- Failure mode: reader hardcoding `/target` or 16-hex key (as GATE says) misses Dell new-generation
  outputs under `t/` and computes wrong ws path (historical real-run failure class).
- EXACT FIX: `GATE:84` rewrite to broker leaf rule + `gate never computes key, reads outputDir`.
- Proof test: `isGenerationTarget` unit cases incl win32 `t` leaf already in `snapshot-request.test`;
  add doc-string assert.

### M55. Snapshot tree method missing seed (reimplementation computes wrong tree)
- `GATE-CONTRACT.md:86` method `GIT_INDEX_FILE=<tmp> git --work-tree=<snapshot> add -A; write-tree`
  (no seed stated) vs `BROKER-FIRST-IMPLEMENTATION-ORDER.md:87-89` empty index skips ignore-matched
  committed files (repro `6960e73` vs `8ad08e9`, rightkit#1255); code both seeded — gate
  `snapshot.mjs:9,243 read-tree`, broker `check_admission.rs:1026 read-tree base_tree`.
- Failure mode: reimplementation from locked GATE text computes wrong tree id — false RK_CHECK_ERROR
  or blind to deleted ignore-matched file.
- EXACT FIX: `GATE:86` insert `read-tree <sha>^{tree}` as first index op (needs Adrian reopen per
  FIRST:89 — flag).
- Proof test: fixture commit with `.gitignore`-matched tracked file, gate `snapshotTreeId` == broker
  verify == `rev-parse` tree (exists in snapshot tests; add doc method replay).

### M56. Pipeline route/HEAD rules contradict gate executor table
- `PIPELINE-ARCHITECTURE.md:16-17 broker where reachable...plain cargo on any other host`; `:100 clean
  HEAD only`; `:110` P1 gap broker pinned check `--worktree` cached per tree vs
  `GATE-CONTRACT.md:36,44` any selected commit, dirty checkout irrelevant, Linux direct regardless of
  PATH; `CLASS-A:164 resolveExecutor` returns direct before broker check; broker check retired
  (`request.rs:247`).
- Failure mode: following PIPELINE refuses legitimate dirty-checkout commit builds and routes
  Linux-with-rustright-on-PATH into broker.
- EXACT FIX: `PIPELINE:16-17,100,110` align to GATE executor table (`gate-executor.mjs:12`).
- Proof test: gate-executor tests for linux+rustright-on-PATH == direct (exist); doc grep
  `clean HEAD only` == 0.

### M57. No wire schema anywhere + pins record undocumented
- `rightkit/contracts/README.md:3-13` schemas are content-pipeline only (`ls`: 10 json, zero
  broker/gate; grep snapshot hits only attested-runs.md); `rustright/docs/contracts` has no
  snapshot-request/grant/receipt/request-ref schema files vs schemas exist only as code constants
  (`snapshot-request.mjs:25,31`, `request.rs:154`) and gate `broker-pins.mjs PINS_SCHEMA
  rightkit.broker-pins/1` record `<dataRoot>/pins/broker-<platform>.json` (consumer enrollment
  `attested-runs.md:271-276` never names this file/schema).
- Failure mode: cross-repo wire changes drift with no versioned artifact to diff (root cause of
  split); new host enrollment unimplementable from docs.
- EXACT FIX: add `rustright/docs/contracts/snapshot-request.md` (+json schema) owning
  carrier/request-ref/pins shapes, linked from GATE §1.
- Proof test: schema validates carrier written by `writeSnapshotRequest` and pins file from
  `enrollBrokerPins` in both repos' tests.

### M58. Pin staleness live proof + dead primary client probe (critic, extends M39)
- Gate: `ci/rk-pipeline.json:150-151,166-167` pins `brokerVersion rustright-5af765b` both platforms vs
  broker HEAD `b84ae60`; `snapshot-request.mjs` checks brokerVersion merely non-empty.
- Gate: `managed-cargo.mjs:54` primary probe `<control>/runtime-current/rustright` vs `ls` proves
  `~/Library/Application Support/RightKit/build-control/runtime-current` ABSENT on this Mac while
  fallback `~/Library/Application Support/RustRight/runtime/rustright` EXISTS (30 Oct).
- Failure mode: post-upgrade provenance refusal until re-enroll; primary probe dead, resolution
  survives only via fallback.
- EXACT FIX: re-enroll both hosts after `b84ae60` install, update `rk-pipeline.json`
  digests/version/revision; probe `runtime` before `runtime-current` (M10/B10 family).
- Proof test: `broker-pins.test` asserting committed pins brokerVersion == `doctor --json`
  runtime.version on broker hosts.

### M59. Cache-root ordering: broker-host throws instead of skipping (macOS)
- Gate: `cache-command.mjs:33 resolveSharedCacheRoot` called BEFORE broker-host gate at `:34`; vs
  `cache-policy.mjs:34-41` throws `RIGHT_RELEASE_CACHE_ROOT is required on macOS` when unset. Broker
  owns target/cache on Mac so that env is legitimately unset there.
- Failure mode: `right-release cache status|prune|migrate` on broker Mac without
  `RIGHT_RELEASE_CACHE_ROOT` throws raw error instead of `BROKER_MANAGED_MESSAGE` skip
  (`cache-command.mjs:26-38` intent).
- EXACT FIX: `cache-command.mjs runCacheCommand` — evaluate `BROKER_GATED_COMMANDS && isBrokerHost(
  env)` before `resolveSharedCacheRoot`; pass `cacheRoot:null`/omitted in skipped result.
- Proof test: node test `runCacheCommand(['status'],{env:darwin broker env without
  RIGHT_RELEASE_CACHE_ROOT})` returns brokerManaged skip (throws today).

### M60. SwiftPM caller-passed `--scratch-path` latent denial
- Gate appends `--scratch-path` for swift scans (`deterministic-scans.mjs:2277-2279`,
  `swiftScratchPath :1907-1916` keyed `<tmpRoot>/rightkit-swift-scratch/<sha256(repo)[:16]>/<scan
  id>`); `build-launch/carried-adapter-run` strip nothing (grep: no `scratch` in either).
- Broker: `service.rs:5920-5929,5941-5949` appends its own `--scratch-path
  <outputDir>/swiftpm/<sha256(snapshot-relative pkg path)>`; adapter registers no `--scratch-path`
  so `snapshot_argv_check` (`service.rs:5912`) denies a caller-passed one.
- Failure mode: today det-scans swift scans are NOT carried (viaSnapshotBroker cargo/rustright only,
  `deterministic-scans.mjs:2192`) so no clash; but any app step/dev-package swift call passing
  `--scratch-path/--build-path` through rightkit-swift adapter gets `RK_ADAPTER_ARGV_DENIED` on first
  real dev/package run.
- EXACT FIX: `build-launch.mjs adapterLaunch` for rightkit-swift — strip caller
  `--scratch-path/--build-path` (+`= `forms) before carry, recording it, since broker pins scratch.
- Proof test: carried adapter fake-broker test — swift `build --package-path p --scratch-path /tmp/x`
  is carried without the flag and receipt verifies.

### M61. Declared-inputs declaration-file + subcommand-parse + fixture-drift (carried-classA minors kept major-adjacent)
- Listed here to preserve evidence (full detail in MINORS N-section): gate
  `compile-inputs.mjs:11/snapshot-request.mjs:138` single-file read (M-family B2 engine);
  `snapshot-request.mjs:55 cargoSubcommandOf` + `cargo-guard.mjs:16 cargoSubcommand` skip only a
  subset of broker globals (N10); fixture `acceptedAfterFix` drift both directions (M35 engine).
  Kept as major-adjacent pointer; fixes live with B2/M10/M35.

## (2) Findings — MINORS

### N1. Identity strictness asymmetry (gate path)
Gate/client STATUS after cutover relies on by-id relaxation (`client.rs:343-346 AllowWithJournal`;
broker `service assert_envelope` exempts WATCH/STATUS/CANCEL/CONSUME_* from digest equality
`service.rs:1423-1430`) vs broker `DataRootGate check_envelope (gate.rs:195-200)` STRICT for every
type (used when service absent `:336-339` and for PAUSE/RESUME `:273-276`); invalid identity yields
`immutable...identity is required` there vs `digest mismatch` in service (`:1421-1422`). Failure:
during data-root outage a stale-runtime STATUS/WATCH gets bare RESULT REJECTED instead of gated
STATUS UNAVAILABLE. Fix: `gate.rs check_envelope` adds same by-id exemption list as
`service.rs:1427` and maps identity errors to digest-mismatch message. Proof: `gate_tests.rs` case.

### N2. BrokerEvent enum incomplete vs actual emissions
Gate parses broker output only via CLI stdout JSON (`snapshot-broker.mjs:41-47`,
`build-ownership.mjs:877-882`); client parses events as Value (`client.rs:206-210`) vs typed
`protocol/src/response.rs:21-48 BrokerEvent` lacks FORECAST/SNAPSHOT_LEASE_STATUS/GRANT_RECEIPTS/
QUIET_LEASE/QUIET_RELEASED/QUIET_STATUS/PROMOTED/CONSUME_*/ADMISSION actually emitted
(`service.rs:1672,2952`; `response.rs:455-459` proves unknown types rejected by typed decode);
`DrainStatusEvent (:200-206)` omits admitting/queued/resident/held/nonIdempotentRunning
(`service.rs:1613-1623`); `ListEvent` omits broker/owners (`:1631`). No runtime failure today (Value
paths). Fix: add missing variants/fields to `response.rs` or annotate partial. Proof: decode captured
real-broker lines as BrokerEvent in `response.rs` tests.

### N3. Carrier schema spelling latent L-1 break [FIXED-IN-WIP]
Gate `snapshot-request.mjs:25` wrote `rightkit.snapshot-request/1` (verify reconstructs same
`snapshot-broker.mjs:340`) vs client `shims.rs:233` accepts `rustright.snapshot-request/1` OR
`rightkit...` (`legacy...removed by task L-1`). Worked; L-1 day refuses every carried call. Critic:
WIP renames to `rustright.snapshot-request/1` — land it. Proof: gate carrier test new schema + real
carried call; `shims.rs:233` already accepts it.

### N4. Windows client spawn: two paths for same client
Gate `broker-client.mjs:1-8,20-38` (NEW in `b8ae` vs `5c388`) refuses any `rustright.cmd` not exactly
installer shape, spawns target exe `shell:false` (installer generates that 7-line shim
`install.rs:196-202`, marker `broker-client.mjs:12 = install.rs:26`) vs gate doctor/quiet paths spawn
`rustright`/absolute `.cmd` via `resolveInvocation` through `cmd.exe /d /s /c`
(`bin/rightkit-gate.mjs:1076,1915`; `deterministic-scans.mjs:1451-1462`) and lease path raw spawnSync
`rustright.exe` (`managed-cargo.mjs:54,69-70`). Inconsistent verdicts on Dell; shell path subject to
`SAFE_SHIM_ARG` refusal (`deterministic-scans.mjs:1430,1458-1459`). Fix: route
`queryBrokerCapabilities (:1915)` and quietLeaseLane (`:1076,:1086`) through `spawnBrokerClient`.
Proof: gate test win32 fixture — doctor query with `.cmd` client uses resolved exe, no cmd.exe.

### N5. Unknown-field tolerance inconsistent + evidenceClass special-case
Gate sends no typos (typed client) vs broker `deny_unknown_fields` on snapshot/grant/consume/qualify/
forecast/lease/grant-receipts/attestation structs (`request.rs:113,120,127,134,158,167,338,350,364,
376,464,470,492,501,511`) but NOT on ManagedAgent/Build/Submit/JobId/Envelope/Promote/QuietAcquire/
Cancel/StorageGc/Execution/RuntimeIdentity-raw; `evidenceClass` STRIPPED before typed parse
(`service.rs:1588,7910-7919`) then raw-checked in normalize (`:3930-3936`): `class1` →
`RK_CLASS1_UNAVAILABLE` despite protocol enum accepting it (`request.rs:44-49`), non-string (42) →
silently class2; client never sets it (`shims.rs:604`). Fix: add `deny_unknown_fields` to remainder
(or document additive-only), validate evidenceClass type (`RK_EVIDENCE_CLASS_INVALID`), mark Class1
doc refused-unported. Proof: broker tests extra field `grantt` + `evidenceClass:42` → REJECTED.

### N6. SUBMIT id honored on bare MANAGED_AGENT, undocumented in type
Gate client sets id only on SUBMIT (`client.rs:307-315`) vs broker `supplied_request_id` reads
top-level `id` from raw envelope for BOTH Submit and ManagedAgent (`service.rs:1637,1642-1644`;
`journal_impl.rs:122-129` validates 1–200 `[A-Za-z0-9._-]`), while `ManagedAgentRequest` has no id
field (`request.rs:184-256`) and no deny_unknown so typed parse silently drops it. No failure today.
Fix: document on `ManagedAgentRequest` doc comment (`request.rs:179-181`) that envelope `id` is read
raw, or add typed id field. Proof: broker test direct MANAGED_AGENT with id twice yields one job.

### N7. Refusal-record canonicalization (`cc4b52c` incomplete)
Gate assumes refused handles terminal-or-nothing (`snapshot-broker.mjs:182` mark-before-send) vs
broker `service.rs write_rejection` (canon root fallback + `canonicalize(source.path).ok()` filter,
fail-closed) but `snapshot.rs:399 record_refused` itself does
`canonicalize(path).unwrap_or_else(|_| path.to_path_buf())` keying claim/completion on UNRESOLVED
spelling. Any future/other caller reintroduces `cc4b52c`: completion on non-canonical path never
matches `status()` canonical path → UNKNOWN forever. Fix: `snapshot.rs:399` → `let Ok(path)=
canonicalize(path) else return Ok(false)`. Proof: `record_refused` on nonexistent absolute path
asserts `Ok(false)` and no claim file.

### N8. Submit-mark verification asymmetry
Gate `snapshot-broker.mjs:102-103 markCarriedRequest` returns `section.markSend(callHandle)` UNCHECKED
vs env-file path `:108-109` verifies `marked.broker.submitted===true` and handles include callHandle,
refusing otherwise. Silent-no-op ownership hook sends unmarked; later release sees `submitted=false`
and may release while broker work live. Fix: `:103` applies same two assertions as `:109`. Proof:
carryBrokerCargo with `markSend` returning `{}` must refuse `RK_CARGO_BROKER_OWNERSHIP`, send nothing.

### N9. Uncarried probe exception vs gate claim
Gate `snapshot-metadata.mjs:3-10` states broker refuses ROOT request inside managed snapshot and gate
refuses all non-carried shapes vs broker `service.rs:4015-4022` allows `uncarried_snapshot_probe`
(cargo version, rustup target list) root requests inside snapshot; adapter roots always refused
(`:4017`). Read-only by broker classification so contained; contradicts gate contract doc. Fix: broker
`command_policy` removes probe exception for workspace under `<external>/snapshots`, or gate
doc+`carriedScanRefusal` names probe shapes broker-allowed and ledger-exempt. Critic: WIP
VERSION_PROBES exact match (8 forms incl `[version]` vs `command_policy.rs:432-439` 7+`[version]`,
rustc flags-only, rustup `target list`) — coherent, land it. Proof: real broker `rustright cargo
--version` with cwd in snapshot, no carrier env.

### N10. Carrier file cleanup on gate crash
Gate `snapshot-request.mjs:200-219` writes `<controlRoot>/snapshot-requests/<uuid>.json` (nonce
`:214`, mode 0600) removed only in `finally` (`snapshot-broker.mjs:184,207-208`) vs broker
`snapshot_grants.rs:289-310 retire_expired` sweeps only grant ledgers. Gate `-9` between write and
send leaks carrier files (replay still blocked by one-completion-per-handle `snapshot.rs:336-343`).
Fix: gate `createSnapshotBroker` or broker startup best-effort sweeps `snapshot-requests/*.json`
older than 24h excluding `grant-*.json`. Proof: plant old uuid.json, run sweep, assert removed and
`grant-<uuid>.json` kept.

### N11. Unsealed commit resolve in bind
Gate resolves all identity git sealed (`snapshot-git.mjs:20-21,136-145`) vs broker
`snapshot.rs:47` bind resolves `commit.sha` via `check_admission::git → git_within
(:1266-1269)` which does NOT `env_clear` (inherits broker env incl `GIT_DIR/
GIT_CONFIG_PARAMETERS`; only sets `GIT_OPTIONAL_LOCKS/GIT_TERMINAL_PROMPT`), unlike `SealedGit
:1005 env_clear` used by `verify_snapshot :866-877` right after. Fail-closed (`eq_ignore_ascii_case`
to submitted sha `:48`, no wrong-commit acceptance) but polluted `GIT_*` env could refuse valid jobs.
Fix: `snapshot.rs:47` uses `SealedGit in_checkout` like verify, or `env_clear`+sealed env in
`git_within`. Proof: broker unit bind with `GIT_DIR` pointing at other repo still binds.

### N12. Subcommand-parse skips too few globals
Gate `snapshot-request.mjs:55 cargoSubcommandOf` and `cargo-guard.mjs:16 cargoSubcommand` skip only
`--color/--explain/--config/-Z` values, returning `--manifest-path` VALUE as subcommand (probe:
`x/Cargo.toml`) vs broker `command_policy.rs:250 command_index` skips
`--manifest-path/--config/-C/-Z/--color/--target-dir` + values (and `test_run.rs:239`). Non-carried
guard paths misclassify (`cargo --manifest-path X test` compute-policy deny, light/heavy split
missed); carried path unaffected (manifest-path stripped first). Fix: both gate parsers skip broker's
global value options `[--manifest-path,--config,-C,-Z,--color,--target-dir,--explain]` spaced and `=`
forms. Proof: `cargo-guard.test cargoSubcommand([--manifest-path,x,test])=test`, policy deny fires.

### N13. Mark-lock stale-break / delete race
Gate `ownership-mark.mjs:63` any marker may delete a lock older than 10s unconditionally; `:67`
original holder's `finally rmSync(lock)` can delete a successor's fresh lock. Broker unaffected. Holder
stalled >10s (suspended Dell) → two markers interleave rename; `broker.handles` update lost
(`submitted` stays true, reclaim unaffected). Fix: lock file contains pid+uuid token; stale-break and
`finally` delete only when content still equals own token (compare-before-remove, retry on mismatch).
Proof: parallel-mark test with injected 11s stall, all handles land.

### N14. Clean release spuriously kept: no REAPED wait
Gate `build-ownership.mjs:1146-1151 release()` single-shot `coveredWorkGone` incl lease check vs
broker `service.rs:3838-3839` (`lease released after terminal event`), completion written only in
Lease Drop (`snapshot.rs:642-679`) after `confirm_reaped`/survivor scan. Gate finishes, receipt
verified, release races broker scan → `released:false lease is ACTIVE/UNKNOWN`, record kept;
self-heals via next gate's reclaim only while broker stays up. Fix: cover lease at selfPid release
with bounded poll like job holder loop `:952-959` (10s, 100ms) before returning kept. Proof: broker
service test status REAPED before client observes terminal receipt, or gate test lease flipping
ACTIVE→REAPED on 2nd call releases cleanly.

### N15. Lease completion write error swallowed
Broker `snapshot.rs:660-671` Drop writes completion+removes record under `let _ = write()` vs gate
`build-ownership.mjs:1046` trusts REAPED as only exit. Disk-full/EPERM during Drop → no completion,
status UNKNOWN forever, no broker log. Fix: Drop logs `write()` error via eprintln/lifecycle (as
`service.rs:3645` does for `record_refused`) and keeps record dir when completion write fails so
status stays honestly UNKNOWN and acquire reap path can retry. Proof: unit test with `completed/`
read-only, dropped clean lease → status != REAPED, record retained.

### N16. brokerLeaseState bypasses broker-client shim resolution
Gate `build-ownership.mjs:874 run(spawn, client, [snapshot-lease...])` raw spawnSync vs
`broker-client.mjs:64-69 spawnBrokerClient` used by `snapshot-broker.mjs:38,183,270`; today
`client=managedRuntimeClient` (`managed-cargo.mjs:54,69`) is native `rustright/rustright.exe` so it
works. Any caller/tests injecting PATH `rustright.cmd` client gets Node EINVAL → UNKNOWN → reclaim
wedged on Dell. Fix: `build-ownership.mjs:874` uses `spawnBrokerClient(spawn, client, args, opts,
{platform})`. Proof: `build-ownership.test.mjs:824` brokerLeaseState with `C:\x\rustright.cmd` +
win32 resolves shim or returns `RK_BROKER_CLIENT_UNRESOLVED`-shaped UNKNOWN naming shim, not EINVAL.

### N17. Pause invisible to gate
Broker `gate.rs:77,306-325 RK_ADMISSION_PAUSED` REJECTED exit125 for root admissions, STATUS/LIST
tagged `admissionPaused`, persisted across restarts (`pause_store.rs:28-51`; no-holder pause never
lifts `:146-165`) vs gate slice files contain zero pause/resume/`admissionPaused` handling (grep: only
`local-runtime.mjs:56` reads health field). Operator/wrapper pause mid-gate → carried calls fail as
generic `RK_CARGO_BROKER_UNVERIFIED`, no wait, no named code. Fix: `snapshot-broker.mjs` carry paths
surface client stderr/code `RK_ADMISSION_PAUSED` in refusal code+message (`broker admission paused;
rustright resume-admission`) instead of unverified-receipt. Proof: fake-broker returns paused RESULT;
gate scan row code names pause; real: pause broker, run check stage, message names resume-admission.

### N18. Local-runtime raw client spawns (missed surface, critic)
Gate `local-runtime.mjs:109-112 listBrokerJobs` spawns `run(client,[list],...)` and `:180` spawns bare
`rustright` for `rustc -Vv`, with NO `broker-client.mjs` shim resolution and NO `locateBrokerClient`,
unlike every WIP-migrated caller (`bin/rightkit-gate.mjs`, `disk-policy.mjs`, `progress-control.mjs`,
`unit-gate.mjs`). Dell `.cmd` client → EINVAL/UNKNOWN lease or failed list; shapes themselves are
broker-accepted probes. Fix: route both through `brokerClientInvocation/spawnBrokerClient` like
`disk-policy.mjs` WIP. Proof: win32-fixture test asserting resolved `.exe`, no cmd.exe in argv.

### N19. Stale comment target leaf
Gate `cargo-target.mjs:13-15` comment says broker target is
`<managed>/rightkit-build-control/ws/<hash>/g/<generation>/target` (leaf always `target`) vs broker
`rustright-state/src/lib.rs:50-64,422-434` leaf per-generation persisted `target` OR `t` (Windows new
generations default `t`). No code failure (`cargo-target.mjs:27-52` never constructs path; uses cargo
metadata `target_directory` or signed receipt `outputDir`; `snapshot-request.mjs:349-366` accepts both
leaves) — comment misleads future edits toward hardcoding `/target`. Fix: `:14` change `/target` to
`/<leaf>` noting leaf `target|t` per generation. Proof: grep gate for hardcoded `g/` construction
(none) + `snapshot-request isGenerationTarget` tests for leaf `t`.

### N20. Swift sealed build dir unused env
Gate `swift-build-context.mjs:29,38,41 planSwiftBuild buildDir=<vault>/cache/swift-build/<platform>/
<fingerprint16>`, exports `RIGHT_RELEASE_SWIFT_BUILD_DIR`, watchDirs includes `<appRoot>/.build` vs
broker has no such var/dir (broker scratch `<outputDir>/swiftpm/<sha256-64>`, `service.rs:5929`);
grep: `RIGHT_RELEASE_SWIFT_BUILD_DIR` consumed nowhere except `swift-build-context.test.mjs:56-57`.
Sealed swiftpm build is gate-local; if routed via broker adapter, var not reserved
(`adapter-env.mjs:24` prefixes) but `check_environment` (`adapter.rs:751-753`) denies unregistered env
names and broker ignores buildDir → products land in swiftpm/ scratch, gate collects from empty vault
buildDir. Fix: delete export (dead) or document gate-local-only in header; never forward via
`RUSTRIGHT_ADAPTER_ENV`. Proof: grep consumer test + swiftpm sealed build on Mac collecting product
from buildDir (existing tests cover plan shape only).

### N21. Package `wired:false` is dead metadata
Gate `gate-stages.mjs:40` package `{wired:false, blocker: awaiting carrier+dev pins}` and `:42`
qualify `wired:false` vs `bin/rightkit-gate.mjs:1410-1428` runs package unconditionally; no production
code reads `wired` (grep `packages/release`). `--route` and operators report package route as normal
while contract table says unwired; first proof failures (B5/B6/B3) surface mid-run not at route. Fix:
flip `wired:true` only after Mac+Dell+Linux package proof and update blocker text, or make routePlan
refuse `wired:false` stages with blocker text. Proof: `door-stages.test` asserts wired flag matches
routePlan outcome per stage.

### N22. Package swap-helper target readback ignores carried outputDir
Gate `swap-helper-bundle.mjs:73-81` parses cargo metadata stdout `target_directory` only vs
`cargo-target.mjs:41-42` and `development.mjs:399-400` every other caller preferring
`result.outputDir` (signed receipt) on carried calls; broker `service.rs:5410-5414` carried metadata
env pins managed target so stdout usually agrees — unproven equivalence. Unit package of
`@rightkit/release` on snapshot broker: if carried metadata prints snapshot-local
`target_directory`, swap candidates miss and `SWAP_HELPER_BUILD_FAILED` after successful build. Fix:
`prepareSwapHelper targetDir = meta.outputDir ?? JSON.parse(meta.stdout).target_directory`, matching
`cargo-target.mjs`. Proof: `swap-helper-bundle.test` fake run returning `{outputDir, stdout with
different target_directory}` picks outputDir.

### N23. Forecast dev/package invisible, query primary-only
Gate: no FORECAST use anywhere in `packages/release` (grep) vs broker `service.rs:1156`
`forecast_input` None for adapters (pnpm/node/swift = most dev/package work) and
`forecast_query.rs:63-72` FORECAST refuses non-primary/snapshot workspaces. Queue/run forecasts for
dev and package stages systematically absent (null), never a refusal (`forecast.rs:5-6` report-only).
Fix (broker): record adapter roots in forecast table keyed adapter+argv; or document cargo-only in
BROKER-CONTRACT and gate ETA copy. Proof: `forecast_query` test with adapter job ahead returns
non-null `localWaitMs`.

### N24. Compact freshness shape / test fidelity
Gate `freshness-census.mjs:248 validateFreshness` requires `unitsAtStart` count and
`invalidationReasons[]`; fake `fake-broker.mjs:130` stage freshness `{unitsAtStart,fresh,notFresh,
invalidationReasons:[]}` vs broker `cargo_evidence.rs:240 compact_freshness` removes
`invalidationReasons` → `invalidationCounts/causeCounts/unexplainedCount/rebuiltUnits` (+detail
`service.rs:6952`), stream `unitsAtStart` may be null (`service.rs:6831`); `unexplainedCount :265`
counts only cause=unexplained while `unexplained_units :285` also fingerprint-missing so signed
`unexplainedCount=0` on `RR_UNEXPLAINED_COLD` receipts. Gate broker path passes (equality only
`snapshot-request.mjs:518`) but tests prove a shape real broker never emits. Fix: fake emits compact
shape; gate adds `validateBrokerFreshness` for compact fields; broker `compact_freshness`
`unexplainedCount` includes fingerprint-missing. Proof: `snapshot-request` verifies fixture receipt
with real compact freshness incl `unitsAtStart:null` and `detail{path,sha256}`.

### N25. Toolchain identity empty-digest signed
Gate `snapshot-request.mjs:48 TOOLCHAIN_FORMAT` requires `<name>|cargo:<64hex>|rustc:<64hex>` enforced
`:528` vs broker `service.rs:7517 toolchain_identity digest() unwrap_or_default ''` and toolchain name
`unwrap_or('')`. Broker host with unregistered/unverifiable rustc shim or no `defaults.toolchain`
emits `stable|cargo:<hex>|rustc:` → every compile receipt fails gate evidence. Fix (broker): refuse
startup/job when toolchain identity incomplete (empty name or non-64hex digest) instead of signing it.
Proof: broker unit `toolchain_identity` with missing rustc registration errors, not empty digest.

### N26. Status receipt presence: absent vs null
Gate `snapshot-broker.mjs:44` requires own `receipt` field, null ⇒ receipt-not-ready `:45` vs broker
protocol `response.rs:171 StatusEvent.receipt` Option with `skip_serializing_if=none` (absent, not
null, for live jobs; `ResultEvent` same `:108`). Depends on rustright-cli passthrough. Live-job status
misreported `no receipt field (not a STATUS envelope)` instead of not-ready (still fail-closed). Fix:
gate treats absent receipt like null (receipt-not-ready), or broker serializes `receipt:null` on
STATUS. Proof: `rustright status` of running job → gate `receiptSync` throws receipt-not-ready naming
state. (citation-gap: CLI re-serialization not opened — severity only.)

### N27. Evidence detail file unread gate-side
Gate: no reader of freshness/attribution detail (`snapshot-request.mjs` whole file; search
`packages/release` found no `.evidence.json`) vs broker `service.rs:6943` writes `<id>.evidence.json`
(suffix `:76`) and signs `detail{path,sha256}` into compact freshness/attribution `:6952`; protocol
`attestation.rs:288 verify_detail` exists Rust-side only. Full invalidationReasons/attribution rows
unverifiable and inaccessible gate-side; diagnosis limited to `rebuiltUnits` cap16 and unsigned receipt
`unexplainedUnits` (`service.rs:7427`, not copied into payload). Fix: gate adds optional
`verifyDetail(detail,bytes)` + fetches evidence file when a consumer needs rows, or documents
counts-only contract in snapshot-request. Proof: gate test verifies evidence file bytes against signed
`detail.sha256` and rejects altered file.

### N28. Retired-fields direct-receipt drift
Gate `direct-receipt.mjs:17 DIRECT_RECEIPT_REQUIRED` still requires `verdict` and `refreshed`;
`buildDirectReceipt :85` emits both vs broker protocol `attestation.rs:282 RETIRED_RESULT_FIELDS=
[verdict,refreshed]` refused `:262` and gate carried path `snapshot-request.mjs:542` refuses them.
Direct (Linux) receipts keep retired names; any unified consumer applying broker matrix refuses direct
receipts (separate schema `rightkit.gate-direct-receipt/1 :12`, no current broker-path failure). Fix:
rename direct fields (e.g. `scanVerdict/liveRefreshed`) with schema `/2`, or explicitly exempt direct
schema in contract docs and shared consumers. Proof: `verifyDirectReceipt` accepts new schema; carried
verifier still refuses verdict/refreshed on broker receipts.

### N29. Fake carrier value validation missing
Gate fake `test-support/fake-broker.mjs:317-322 exactly()` checks key sets only — never schema value,
commit/tree hex, tree==expectedTree, snapshot.path absolute, handle non-empty, declaredInputs relative
vs broker `cli/src/shims.rs:233-245` schema ∈ `{rustright.snapshot-request/1,
rightkit.snapshot-request/1}`, commit_id 40|64 hex `:147-149`, tree==expectedTree, abs paths, inputs
non-empty relative; structs `deny_unknown_fields :118,125,131`. WIP schema rename and any future
carrier value regression passes fake, fails real client `invalid snapshot request carrier`. Fix: fake
client adds value checks mirroring `shims.rs:234-245`. Proof: carried-cargo carrier with schema
`bogus/1` and tree≠expectedTree → exit 2 from fake (today accepted).

### N30. Fake STATUS envelope and unknown-id drift
Gate fake `test-support/fake-broker.mjs:353-354` status always `{type:STATUS,state:"SUCCEEDED"}` even
for FAILED tampers; `:348` unknown id = stderr `no such request` exit 1 vs broker
`service.rs:2899-2907` finished `state=receipt.state`, live state from job_summary with `receipt:null`,
unknown → `unknown_job RK_JOB_NOT_FOUND` or `RK_JOB_SUSPENDED` when journaled. Gate
`snapshot-broker.mjs:49-59` throws only on missing/null receipt, ignores state. Broker-restart
suspended job (receipt later valid) indistinguishable in tests from not-found. Fix: fake status —
state from tamper/result, unknown id exit-125-shaped `RK_JOB_NOT_FOUND`; adds
`behavior.statusSuspended`; documents/tests gate choice on SUSPENDED. Proof: `snapshot-broker.test`
suspended status → named unverified code, not generic receipt failure.

### N31. Fake lease-status default REAPED vs real UNKNOWN
Gate fake `test-support/fake-broker.mjs:337-343 snapshot-lease` returns REAPED whenever heldHandles
doesn't cover handle (ignores snapshot path), asserted `snapshot-broker.test.mjs:139-140` vs broker
`job_service/snapshot.rs:433-472` REAPED only with durable `rustright.snapshot-reaped.v1` completion
for canonical path+covered handle, else UNKNOWN (never-held = UNKNOWN); CLI echoes pair
(`service.rs:1672`). Reclaim tests overstate clearance: dead owner whose job never completed is
UNKNOWN→held forever on real broker, REAPED in fake world. Fix: fake lease tracks completed handles
from actual fake jobs; no completion → UNKNOWN; keys on snapshot arg. Proof: `snapshot-broker.test`
`lease([])` expects UNKNOWN; after one fake carried job completes, REAPED.

### N32. Fake handle-claim timing
Gate fake `test-support/fake-broker.mjs:259-263,326 claimHandle` write-wx at submit, never released vs
broker `job_service/snapshot.rs:342` refusal on prior COMPLETION record at lease acquire; lease held
only while job runs (`status:433-437`). Fake refuses handle reuse after a request real broker rejected
pre-completion (no completion written), and cannot model ACTIVE→REAPED timing gate reclaim waits on.
Fix: fake — claim at submit as ACTIVE, move to completed set when status first served terminal;
refused requests write no completion. Proof: second same-handle submit after `behavior.refuse` first →
accepted by fake (matches no-completion); after success → refused.

### N33. Win shim legacy marker rejected
Gate `broker-client.mjs:12,34-35 parseAgentShim` accepts only marker
`rustright.build-control.agent-shims.install.v1` vs broker `cli/src/install.rs:26-28` defines
`INSTALL_MARKER` + `LEGACY_INSTALL_MARKER rightkit.build-control.agent-shims.install.v1` still
recognized as ours on reinstall/cutover (`:315` prior-manifest test), current contents `:201`.
Dell agent-bin `rustright.cmd` installed pre-rename (legacy marker, same 7-line shape) →
`RK_BROKER_CLIENT_UNRESOLVED`; `broker-client.test.mjs:22-36` has no legacy-accept case. Fix:
`broker-client.mjs` accepts both markers in `SHIM_LINES[1]` (mirror `identity.rs` legacy schema
handling), keeps exact-shape checks. Proof: `broker-client.test parseAgentShim(SHIM with legacy
marker) === EXE`; Dell `where rustright` shim readback.

### N34. Fake version-probe shape (cargo -V)
Gate fake `test-support/fake-broker.mjs:285,295` any argv containing `--version/-V` answered locally
`cargo 1.90.0 (fake)`, no rustc/rustup command at all (`:357`) vs broker
`job_service/version_probe.rs:16-20` exact argv forms only; `:40-41` unpinned toolchain → ordinary
queued job with receipt; uncarried-in-snapshot probes `command_policy.rs:432-440` incl rustc and
`rustup target list`. Gate `rustc -V` probe (`rightkit-gate.mjs:1295` via `rustright rustc -V`) and
Tauri `rustc -vV` inside snapshot untestable against fake; `cargo build --version` fake-succeeds, real
submits build. Fix: fake exact-matches `VERSION_ARGUMENTS` table for cargo+rustc, adds rustc/rustup
client commands returning pinned-identity answers. Proof: fake client `cargo build --version` with
carrier-less snapshot cwd → refused root request; `rustc -vV` → version text, no receipt.

### N35. Fake is synchronous: no queue/pause/coalesce/restart
Gate fake `test-support/fake-broker.mjs:330-335` writes request-ref+db then exits, status terminal
immediately vs broker async lifecycle — admission pause `RK_ADMISSION_PAUSED` exit125
(`gate_tests.rs:247-249`), data-root UNAVAILABLE (`gate_tests.rs:60-68`), coalesced SUBMITTED
(`service.rs:3403`), journal resumes receipt field (`service.rs:7287-7291`), cutover drain reject
(`service.rs:~1558`, citation-gap: exact line approximate). Gate timeouts (`receiptSync` 60s
`snapshot-broker.mjs:50`), no-poll rule and submit-marking under queued/paused/restarted broker never
exercised; dev/package long stages hit these first. Fix: fake behaviors — `statusQueued` (receipt null
+ state QUEUED for N status calls), `admissionPaused` (cargo exit 125 `RK_ADMISSION_PAUSED`),
`coalesced` (second identical submit returns first id). Proof: carried call under `admissionPaused` →
ledger unverified with code named, ownership submitted-marking still correct.

### N36. Remaining small divergences (preserved, short form)
- N36a Fake nested-children depth and outputDir heuristic: `fake-broker.mjs:140 signedReceipt` rewrites
  every child's `parentRequestId` to root id; `:517-521 nestedCargo outputDir=null` iff
  metadata/`-vV`/`--version` else fabricated `<outputDir>/nested/<basename>` vs broker
  `nested_child_row service.rs:~7848` (`citation-gap` exact line approximate)
  `parentRequestId=live.parent_id`, `outputDir=live.nested_target` (broker layout, null for
  compiler/adapter children), cap `MAX_NESTED_CHILDREN=8192 :7971`, refusal past cap `:1391`. Fix:
  keeps scripted parent ids, spawns grandchildren via behavior children-of-children, layout fn
  mirroring broker nested target; stops rewriting parentRequestId. Proof: carried-adapter depth-2 e2e,
  `carriedOutputDir(preferNested)` equals grandchild output.
- N36b Bounded_cmd not bounded: broker `job_service/bounded_cmd.rs:25` drain uses `read_to_end` with
  no byte cap despite module name; `:41` timeout kills only direct child (grandchildren orphaned);
  contrast `ports.rs:442` locate drain `take(64*1024)`. Fix: wrap streams in `Read::take(1MiB)`, kill
  process group/Job Object on timeout. Proof: fake probe printing 100MiB + grandchild; broker RSS
  bounded, grandchild gone at timeout.
- N36c Concurrent gates no wait: gate `build-ownership.mjs:1181 publish-or-judgeOwner` throws
  OwnershipBusy immediately; caller `bin/rightkit-gate.mjs:1259` returns 1 BLOCKED with no wait/retry
  vs broker queues same-checkout, refuses other-checkout `RK_REPOSITORY_BUSY` (`service.rs:3476`),
  multi-repo concurrent. Second gate (different repo, same host) fails instantly instead of queueing.
  Fix: bounded ownership wait (poll judgeOwner until deadline, opt-in `--wait`) or document exit-1-busy
  as retryable contract. Proof: two simultaneous gate runs different repos; second acquires or exits
  distinct retryable code.
- N36d Windows path case in snapshot map: `snapshot-broker.mjs:125 inside()/escapes` uses
  case-sensitive `path.relative` on win32 vs gate folds case for volumes
  (`build-ownership.mjs:133`), broker canonicalize strips verbatim prefix only
  (`rustright-core/src/lib.rs:151`). Checkout `D:\Claude\App` vs realpath `d:\claude\app` → `inside()`
  false → `RK_ADAPTER_BROKER_CWD` refusal. Fix: `inside()` compares win32 paths case-folded. Proof:
  win32 unit test drive/case-variant paths returns true.
- N36e Recovery claim-wait expiry: broker `job_service/recovery.rs:25 CLAIM_WAIT=120s`; `:97` ensure
  returns after 120s while another thread still recovering; caller `service.rs:1049` proceeds, relies
  on `mark_running` fail-closed fence (`recovery.rs:16`). Slow Dell disk, recovery >120s → first gate
  job gets confusing `workspace-already-has-active-root` refusal instead of waiting. Fix: `ensure`
  distinguishes timeout (enum); `ThinState::workspace` retries/queues rather than proceeds-to-refusal.
  Proof: recovery unit test with ensure blocked >CLAIM_WAIT asserts timeout signal, not proceeded.
- N36f No port allocation for test runs: gate hands no port env to carried calls
  (`snapshot-broker.mjs` crater: no port env); broker `job_service/ports.rs:41 SpawnSpec`
  program/args/cwd/env only, no port-base; no TcpListener allocation in job_service (searched).
  Concurrent broker test jobs from different repos binding same fixed test port flake addr-in-use; only
  quiet-marked suites serialized by gate lease. Fix: broker injects per-root
  `RUSTRIGHT_TEST_PORT_BASE` env in SpawnSpec env assembly, or gate assigns offsets per scan in
  `deterministic-scans.mjs`. Proof: two concurrent broker `cargo test` jobs binding env-provided port
  both pass.
- N36g–N36k preserved in full as written in M-section source: N36g submit-marker/ownership record
  undocumented (GATE §1 fix); N36h broken GR16/GR32/E48 cross-references; N36i legacy RIGHTKIT_ env
  spellings presented as current; N36j first-order status + stale pins (editorial); N36k carrier schema
  HEAD-vs-WIP dual acceptance. See M-finding bodies for exact lines/fixes (deduplicated to avoid
  double-count; evidence retained here by reference).

## Falsified / stale / superseded (dropped from findings, reason kept)

- F1. Brief claim `broker-client.mjs` NEW untracked — STALE. Committed in `b8ae3c677` (`git diff
  5c388..HEAD` +69 lines). Audited as committed file.
- F2. Brief claim gate HEAD `5c388c71e` + uncommitted WIP — STALE. WIP landed as `b8ae3c677`; most
  slices found `packages/release` clean; end-of-audit sibling WIP dirtied ~25 files (listed in §0).
- F3. Slice-07 RESULT-FIELDS top-level signing — SUPERSEDED-BY-WIP (was true at HEAD, fixed in working
  tree: `resultEvidence` nesting + required `resultEvidence`, `fake-broker.mjs:148-154`,
  `snapshot-request.mjs:548-553`). Kept as M21-fixed; land WIP.
- F4. Slice-01/08 carrier schema `rightkit.snapshot-request/1` — SUPERSEDED-BY-WIP (WIP renames to
  `rustright.snapshot-request/1`; broker accepts both). Kept as N3-fixed; land WIP.
- F5. Swift scratch `.build`-in-snapshot — FIXED in code (`eadb4ba` + `b84ae60` mutable `spawn_argv`;
  broker appends `--scratch-path <outputDir>/swiftpm/<sha256>`, gate verifies requestedArgv only so
  append cannot break verification). Residual risk: dev/package swift never ran it (M60 + §4).
- F6. Unexplained-rebuild `849091b` exemption — NOT fixed (reverted by `eadb4ba`); slice-09 semantics
  match contracts (first-profile false-FAILED remains by design per code+contracts). Kept as B3
  (code fix owed), not a docs mismatch.
- F7. Gate submit marker "missing" — PRESENT in code (`ownership-mark.mjs`, `b8ae` subject) but in NO
  contract doc. Not falsified; filed as N36g docs gap. Critic: 0 top-severity claims falsified as false;
  two citation drifts noted (submit-mark `:70→:82`; carried-spawn lines shifted by WIP inserts).

## (3) Verified-matching contract points (ok-verified, grouped)

Wire/protocol (slice 01): V1 request-kind table — gate sends NO socket JSON itself; rustright-cli
(`client.rs/shims.rs`) builds all 20 typed kinds (`request.rs:521-568`), dispatched as raw Value
(`service.rs:1596-1678`); gate-side broker JSON limited to CLI stdout parsing (receiptSync lease,
`bin/rightkit-gate` doctor/quiet) + carrier/descriptor/request-ref files. V2 protocol+runtime identity
— `lib.rs:26,44-50` accepts `rustright.build-control.client.v2` + legacy `rightkit...v2`;
`identity.rs:10,47-53` both runtime spellings, nonempty version + 64-hex digests; checks
`service.rs:1418-1432`, `gate.rs:195-200`. V3 handshake+capabilities — gate `bin/rightkit-gate.mjs:
1905,1912-1921` requires `snapshot-readonly-v1+snapshot-adapter-v1` from doctor `.broker.capabilities`;
broker advertises `[journal-v1,snapshot-readonly-v1,stage-grant-v1,snapshot-adapter-v1]`
(`response.rs:54-70`, HANDSHAKE first line `service.rs:1551`, `gate.rs:183-185`); gate correctly no
longer requires `stage-grant-v1` (`snapshot-broker.mjs:153-154`). V4 kinds gate uses MANAGED_AGENT+
SUBMIT (cargo/adapter) — carrier fields `snapshot-request.mjs:204-215` vs client `SnapshotCarrier`
`deny_unknown_fields` (`shims.rs:130-141`) → `ManagedAgentRequest` (`request.rs:230-238`,
camelCase+deny_unknown), SUBMIT wraps `job{type:MANAGED_AGENT}` (`:292-315`); adapter set post-hoc
(`shims.rs:695`, `Frontend::Adapter` → `adapter`). V5 STATUS/SNAPSHOT_LEASE_STATUS/QUIET_*/LIST/WATCH/
CANCEL shapes match (STATUS receipt null-live `service.rs:2896,2900-2906`; lease schema/states/echo;
QUIET ttlMs/reason/waitDrainMs/holderPid; `JobIdRequest.id`=lease id; CancelRequest.source default
USER_CANCEL). V6 unused kinds enumerated (FORECAST/DRAIN_STATUS/STORAGE_GC/PRUNE_PLAN/PROMOTE/
CONSUME_*/QUALIFY/QUALIFICATION/GRANT_RECEIPTS/QUIET_STATUS — typed, Legion/installer surfaces;
`native_consume.rs:14-18` PascalCase golden-tested). V7 framing — one NDJSON line/connection, 1MiB incl
newline, trailing-whitespace rule, EOF-silent-close (`framing.rs:40-74`, `codec.rs:42-74`,
`connection.rs:200-214`); extra-bytes terminal RESULT REJECTED one-request-per-connection
(`:244-264`); framing rejects `{type:RESULT,exitCode:125,status:REJECTED,error}` no code/handshake,
tolerated (`client.rs:231`). V8 error/rejection shapes gate parses (CLI exit/status, lease UNKNOWN on
failure; `reject_event` code-only-when-present `service.rs:1018-1023`; parse-failure generic message
`:1591`; `RK_ADMISSION_PAUSED` `gate.rs:308-314`; `Flavor::Job` coded subset `client.rs:237-248`).
V9 descriptor/request-ref/nonce aux (`snapshot-request.mjs:221-222,200-203,219,197`, `:31,246` vs
`shims.rs:117-122,154-216`, `attestation.rs:52-55`, `cli.rs:303-309`). V10 broker-pins is NOT wire
identity (`broker-pins.mjs:19,38-48` enrollment/gate-revision only vs `identity.rs:27-32` handshake
digest; consumed by provenance `snapshot-request.mjs:23,465`).
Snapshot lifecycle (slice 02): V11 create/name/path identity (`snapshot.mjs:58-60,67-71,741,738,748,
182-191` vs `snapshot.rs:61-75,277,330,344`). V12 root = broker external root (`snapshot.mjs:125-131`
+ `managed-roots.mjs:8` vs installed `config.json externalRoot.path=/Volumes/D/.rightkit-managed` +
`snapshot.rs:79-85`; live `cat`+`ls` this run). V13 commit/tree sealed method (`snapshot.mjs:218-231,
238-252`, `snapshot-git.mjs:29-44` vs `snapshot.rs:40-56`, `check_admission.rs:859-877,881,976-995,
963-968`; limits equal 200k/4GiB/1MiB). V14 lease durability `ee49601+d83737a` + reap semantics
(`snapshot.rs:370-373,354-357,659-675,624-627,426-429,433-472`; tests `:1038-1174` inspected). V15 lease
status protocol three-layer match (`build-ownership.mjs:77-78,872-882` vs `snapshot_lease.rs:11-38,42-45`
vs `request.rs:500-507` vs `service.rs:1660-1672`). V16 start/exit bind + evidence
(`snapshot-request.mjs:389-399,481-495` vs `service.rs:5832-5851,6344-6353`; `snapshot.rs:27-35` vs
`SNAPSHOT_EVIDENCE_FIELDS :42`). V17 grant scope/expiry unreachable (gate strips
`snapshot-broker.mjs:153-154,238`, no writer; broker `snapshot_grants.rs` whole-file read; latent
refused-first-grant note kept). V18 snapshot-metadata WIP removal correct (diff `5c388..HEAD` removes
once-block only; breakage is B1 wrapper, not removal). V19 QA split vs broker model + commit-only route
(`qa-snapshot.mjs:111-124,143-163`, `qa-snapshot-run.mjs:31-52`, `committed-source.mjs:118-172`,
`snapshot-check.mjs:68-100` vs `service.rs:3662-3719`, `snapshot.rs:47-58`).
Carried cargo/classA (slice 03): V20 metadata seam (all gate seam shapes accepted:
`command_policy.rs:460-461,405`). V21 class-A scans as constructed (`deterministic-scans.mjs:1130,
1137,1138,2228` vs `command_policy.rs:397-411,347,375` + `check_admission.rs:427,1529`). V22 fmt exact
gate scans (`deterministic-scans.mjs:229` vs `:417`; `local-publish.mjs:54`). V23 locked argv
(`snapshot-request.mjs:75,67`, applied `snapshot-broker.mjs:152` WIP vs `command_policy.rs:501,477`).
V24 cwd-in-snapshot (`carried-cargo.mjs:76`, `snapshot-broker.mjs:169-177` vs `service.rs` START move +
`check_admission.rs:363,281`). V25 env-carried WIP (19 `MANAGED_RUST_ENVIRONMENT_KEYS` identical
`snapshot-request.mjs:35-40`/`environment.rs:16-36`/`shims.rs:27-47`; WIP `withoutManagedRustEnv`
strips exactly; land it). V26 adapter-env reserved lists byte-identical (`adapter-env.mjs:18-24` vs
`adapter.rs:120-164,746`; comment pin `7b24288` refresh opportunistically). V27 freshness-compile-only
`1945f0d42` (gate `:514-519` vs `service.rs:2570,7415`, `attestation.rs:299`). V28 linux-direct +
clippy-blocked (`managed-cargo.mjs:14`, `clippy-route.mjs:340-348`, `cargo-contract.mjs:32-35`).
V29 class-A placement (`app-check.mjs:8`, `class-a-launch-guard.mjs:68` vs `check_admission.rs:11`,
`service.rs:1211`). V30 adapter-cwd-nested (`snapshot-broker.mjs:246-255,281`,
`snapshot-request.mjs:416` vs `adapter.rs:637`, `service.rs:5902-5918`). V31 schema WIP both-spellings
(`shims.rs:233`).
Ownership (slice 04): V32 key derivation (gate fixed host path `build-ownership.mjs:107-118,1160-1216`
vs broker lease key canonical path `service.rs:1660-1666`, `snapshot.rs:367,540-541`; admit re-resolve
`admit.rs:60-90`). V33 sub/bare-handle lease contract (`build-ownership.mjs:1046` bare query vs
`snapshot.rs:517-518,435,464-469,439-442`; echo `gate :881` == `service.rs:1672`; CLI fallback
`snapshot_lease.rs:35-37`). V34 marker gate-local, broker never reads it (gate `ownership-mark.mjs:
21-47` per-send; broker zero refs; `rightkit-gate.mjs` diff removed install-time marks;
`build-ownership-submit.test.mjs:38-57`). V35 pre-lease refusals terminal (broker `service.rs:3632-3647`,
`snapshot.rs:395-431`, tests `:1101-1125`; exceptions M5 gate.rs-level). V36 unrelated-process lock
#1313 fixed both sides (gate `:1028-1040,1013-1026`, tests `:1909,1955` vs `survivors.rs:30-31,95-101`,
tests `:159-195`; residual macOS-only/opt-in noted). V37 one-build-per-host gate-enforced
(`build-ownership.mjs:3-7,1067-1069`, callers exit BLOCKED no retry vs `admission.rs:30-35`
`build_concurrency=3`, lease per-path only). V38 windows shim shape match (`broker-client.mjs:12,20-29`
vs `install.rs:41,196-202,26`; legacy-marker gap N33). V39 recovery/limits no visible mismatch
(`recovery.rs:11-17,79-103`, `limits.rs` RLIMIT only, `broker.rs:117-127,278-281`,
`pause_store.rs:28-38`).
Env/folders (slice 05): V40 adapter forwarding identical (`adapter-env.mjs:9-10,18-24,63-73` vs
`shims.rs:81,88-115,369-381`, `adapter.rs:120-164,746-756`). V41 `CARGO_TARGET_DIR` ownership + shim
(`cargo-target-shim.mjs:100,113,116`, `snapshot-broker.mjs:148-154`, `deterministic-scans.mjs:885,
2299-2304` vs `service.rs:1531-1532,5408-5436`, `environment.rs:16-36`, `shims.rs:27-47`,
`write_locations.rs:16-17`). V42 target-dirs generation shape + artifact collection
(`snapshot-request.mjs:353-366`, `snapshot-broker.mjs:34` vs `rustright-state lib.rs:134-142,300-307,
424-434`, `service.rs:5850`). V43 cache-dirs separation (gate `cache-policy.mjs:88-118`,
`build-release.mjs:323,335-343,315-319`, `cache-command.mjs:27-39`, `target-bridge.mjs:40-41`,
`cargo-target.mjs:4-11` vs `service.rs:1521,4096`, `thin_cache.rs:704`, `roots.rs:78-79`,
`cache_handle.rs`). V44 HOME/XDG/TMP/state dirs (gate `cache-policy.mjs:73-85,35,122-126` vs broker
`managed_roots.rs:46,129,109-110`, `service.rs:1487-1490,1533-1535,5579-5587`,
`write_locations.rs:12-34`). V45 toolchain pins + version probes (`toolchain_prune.rs:83-93,104-125,
207-219`, `version_probe.rs:16-21,40-42` vs gate `toolchain-swift.mjs:186-208`,
`deterministic-scans.mjs:1692`, `cache-policy.mjs:519`, `swift-build-context.mjs:16-20`). V46 scan-cache
keying + BUILD env names (`deterministic-scans.mjs:146-163`, `swift-build-context.mjs:8` vs broker
`environment.rs/write_locations.rs`, `cache_handle.rs`). V47 SwiftPM scratch present at HEAD (broker
`service.rs:5856,5918-5949` append + checks + transformation vs gate verifies requestedArgv only;
`native-helpers.mjs:238-243` same relative `--package-path` for build/show-bin-path → same scratch;
`adapter.rs:697-727` accepts `.`; still owed real run).
Receipts/freshness (slice 07): V48 snapshot+stage evidence field lists — gate `snapshot-request.mjs:42
SNAPSHOT_EVIDENCE_FIELDS=[requestId,brokerVersion,executor,commit,snapshot,declaredInputDigests,
ownership,outputDir]` + `:44 STAGE=[freshness,attribution,toolchain]` vs broker `service.rs:2557`
copies exactly those 8 (`:7271-7282`), protocol `attestation.rs:245` same 8, `:275` same 3, gate
equality `:481/:514`. V49 result field names/types — gate `:46 TIMING_KEYS=[admission,queue,bind,
cargo,afterExitBind,diagnostics]` null|finite `:534`, exit int|null, signal null|string|int,
diagnosticsPath null|nonempty `:536` vs broker `service.rs:7367` exact keys, exit `details.exit_code
:7358=:7226`, signal `:7227`, diagnosticsPath `:7376` (see M21 coverage major). V50 retired
verdict/refreshed `5f81531a3` in HEAD (gate `:542` refuses, typed list `:535` drops them vs broker
`attestation.rs:262` refuses, builder `service.rs:7204` emits neither; diff verified). V51 freshness
only compile-class `1945f0d42` in HEAD (gate `:516` required=compile only, equality when present `:518`
vs broker `service.rs:2568` filter_map, read-only no census `:5677`). V52 attestation sign/verify —
gate `provenance-verify.mjs:85` schema `rustright.provenance.v1`, ES256, keyId=sha256(SEC1 pubkey), P-256
ieee-p1363 over `rustright-provenance-v1\n`+canonicalJson (`:57` byte-sorted), host/brokerDigest/jobId+
receipt.id/nonce/requestDigest vs broker `protocol attestation.rs:203` identical, canonical `:83`,
signed_bytes `:322`; `job_service attestation.rs:32,87`; pins `broker-pins.mjs:39` same invariants,
enrolled never from receipt `:101`. V53 declared digest algorithm+limits posix (gate
`snapshot-request.mjs:291` name+NUL+bytes+NUL, children byte-sorted `:298`, limits 200k/4GiB `:140`,
declaration 1MiB `:139` vs broker `check_admission.rs:1196,1209`, limits `:36-39`;
`compile-inputs.mjs:49` same order; Windows separator N-filed). V54 journal/history/receipt-files not
gate-read (gate `snapshot-broker.mjs:37` only `rustright status <id>` receipt field; no reader for
`queued/<id>.json` `journal.rs:30`, `receipts/<id>.json` `lifecycle.rs:162`,
`observed-peaks.json state/:49` via `history_file.rs:21`; `journal_impl.rs:279` receipt-before-forget,
`service.rs:7447-7452`). V55 warmth not a gate dependency (no gate reader of
`warmthAtStart/warmth`; broker `warmth.rs:42` classify + `service.rs:7304,7462`; tests `:116-154`).
V56 verifier grant receipts gate-internal (`verifier.mjs:26,54,81,98,93-116,136` — no broker
counterpart; broker/direct receipts carry NO age check, freshness is per-call nonce+jobId+requestDigest
`provenance-verify.mjs:105`).
Fake/pins (slice 08): V57 descriptor + carrier file rules (`fake-broker.mjs:297-322` ≤4096B, exact
`{path,sha256}`, regular non-link direct child `≤4MiB` sha-matched, exact key sets, nonce==ATTEST_NONCE,
argvSha256, primaryCwd==cwd vs `shims.rs:154-209` O_NOFOLLOW, `:118-141 deny_unknown`,
`:241-244` bindings; value-checks gap N29). V58 signed evidence field sets incl WIP resultEvidence fix
(WIP `fake-broker.mjs:148-154` payload signs `resultEvidence{exit,signal,timingsMs,diagnosticsPath}`,
`snapshot-request.mjs:548-553` requires it vs broker `attestation.rs:275,280`, `service.rs:2568-2574`,
snapshotEvidence 8 `service.rs:2557` = gate `:42`; timings keys fake `:146` = `:7367-7374`; retired
refused both sides fake `:67-69`; pre-WIP drift now fixed — commit WIP). V59 lease/status/doctor wire
shapes + capability strings (lease argv+schema fake `:337-343`, gate REQUIRED caps
`rightkit-gate.mjs:1910` vs broker `snapshot_lease.rs:11,43-44`, echo `service.rs:1672`,
`response.rs:70 BROKER_CAPABILITIES`, doctor JSON `doctor.rs:357-365`; STATUS `{type,id,state,receipt}`
fake `:354` = `:2899-2906` modulo state-value N30). V60 pins crypto + generation target layout
(`broker-pins.mjs:44-45` SEC1 130-hex, keyId=sha256(sec1); fake keyPair `:26-31`, ES256 ieee-p1363
`:158`; `isGenerationTarget snapshot-request.mjs:359-364 <ws>/<12hex>/g/<3+digits>/{target|t}`,
outputRoots `<data>/rightkit-build-control/ws snapshot-broker.mjs:46` vs broker
`output_dir=layout.target service.rs:5850` under `<external>/rightkit-build-control/ws`
`lib.rs:307`; attestation verify `protocol attestation.rs`).
Docs (slice 09): V61 snapshot roots + Linux default (`GATE:82` Mac/Dell `<managed>/snapshots/
<sha256[:16]>`, Linux `$XDG_STATE_HOME/rightkit/snapshots` fallback vs `snapshot.mjs:130,142`,
`snapshot.test.mjs:110-118` incl env-cannot-move). V62 stage + executor lists (`PIPELINE:14,100-101`,
`GATE-CLASS-A-INVENTORY.md:157-164,171` check|compile|package|qualify|release + dev|test, broker/
refuse/direct vs `gate-stages.mjs:15`, `gate-executor.mjs:12`; `gate-contract.test` inventory binding).
V63 keep-going rule BR29 (`GATE:46`, `BROKER:59` build/check/clippy/doc `--keep-going`, test/bench
`--no-fail-fast`, broker adds+records transformation vs `cargo_argv.rs:123-161,179-189`, gate
`deterministic-scans.mjs:211-220`; attested enum lacks kind — M52). V64 SwiftPM scratch doc matches code
(`BROKER:100` append + inside-output check + transformation vs `service.rs:5918-5945`; witness still
owed). V65 freshness/unexplained-cold semantics match contracts (`GATE:36`, `BROKER:87,96` census in
receipt vs `849091b/eadb4ba`; `RR_UNEXPLAINED_COLD` name in NO doc — name it in BROKER §3). V66 service
vs bundle identity (`using:11,20-21` `dev.rustright.broker` pipe, `dev.rightkit.rustright-broker`
bundle vs `identity.rs:10,20`, tests `:99-100`). V67 fixture-files manifest (`GATE:48`
`.rightkit/fixture-files.json` vs `deterministic-scans.mjs:985-1000`). V68 request-ref schema/atomicity
(`request-id.md:14,17-19,24-26` vs `cli.rs:300-305`, `fake_broker.rs:646`, gate
`snapshot-request.mjs:31,244-248`; env-name title → RUSTRIGHT_ primary per N36i).
Stages/dev-package (slice 06): V69 broker capabilities gate requires (strings + fail-closed probe
`rightkit-gate.mjs:1905,1912-1922` vs `response.rs:58,67,70`, `service.rs:1551`). V70 signing outside
broker mainline (`build-launch.mjs:131-132` sign refused as command; `test-command.mjs:200-219`
in-process; `sign-dev-artifact.mjs:95-123` caller session; `swift-mac-app.mjs:120-145` gate-session
spawns vs broker `service.rs:2350` scrubbed env, no signing adapter). V71 swift native helpers vs
scratch fix (`native-helpers.mjs:240-243` build then same-argv `--show-bin-path`, no `--scratch-path`
vs broker `adapter.rs:708-726`, `service.rs:5918-5944`; `.build` defect fixed broker-side; first real
macPackage run still owed). V72 package sealed bound-targets cross-check (`bin/rightkit-gate.mjs:
1449-1460` sealed boundTargets == `ds.carriedByStage[package]` output dirs vs broker
`service.rs:2555-2564,2948`). V73 unit compile/package expected argv vs signed request
(`stage-commands.mjs:34-50,86-89` vs broker `job.rs:195-200`, `service.rs:2929-2932`; real unit package
still owed). V74 stage taxonomy + broker touchpoints (`gate-stages.mjs:15`,
`bin/rightkit-gate.mjs:1383-1497` vs `service.rs:1215-1226` must-compile-or-read-only). V75 android dev
install + assemble-app tools outside broker (`android-dev.mjs:94-110` adb direct; `assemble-app-command.
mjs:97-112` makensis+sign-windows direct post-`verifyReleaseBinding` vs no device/installer adapter in
`service.rs:3887-3920`).
Concurrency (slice 10): V76 quiet lease contract (gate `bin/rightkit-gate.mjs:1054` TTL 30min, `:1055`
waitDrain 2min, `:1081` flags, `:1088` env `RUSTRIGHT_QUIET_LEASE` vs broker `quiet.rs:23,267`,
`quiet_impl.rs:300,106`, CLI `quiet.rs:55`; refusal fail-closed `:3038`). V77 restart/resume + health
(gate blocks in client across restart vs broker journal resume `mod.rs:45`, re-WATCH same id
`client.rs:555`, lazy recovery `recovery.rs:4`, `health.json` atomic `health.rs:62` every 5s
`broker.rs:37`, final STOPPED `:214`, pipe Low-IL no DACL widen `pipe_security.rs:5`; bounded by 15min
budgets M47). V78 windows hidden spawns + paths (`background-process.mjs:16,32` `windowsHide:true` vs
broker `bounded_cmd.rs:19 CREATE_NO_WINDOW`; canonicalize verbatim-strip match
`rustright-core/lib.rs:147` vs `snapshot-broker.mjs:171`; exception: `ports.rs:428 run_locate_project`
lacks `CREATE_NO_WINDOW` → add `creation_flags(0x08000000)`). V79 connection framing limits (gate
delegates to client; broker `connection.rs:21,23,244,120`; `node --check` 4 gate files OK).
Critic extras: V80 pnpm grammar vs live registration identical (gate `pnpm-adapter-grammar.mjs:18-26`
15 subcommands/15 flags/opaqueAfterTarget/snapshotSubcommands vs installed Mac `config.json`
adapters[rightkit-pnpm] identical order; WIP liveAdapterGrammar fallback — land it). V81 swift/node
adapter registration vs gate argv (installed `rightkit-swift` subcommands `[build]`, flags
`-c/--package-path/--product/--arch/--show-bin-path`, snapshot true, snapshotSubcommands null = no
token restriction `adapter.rs:639-645`; `rightkit-node` subcommands null, snapshot true; gate
`native-helpers.mjs` sends only registered shapes; caller scratch-path still denied M60). V82
MANAGED_RUST 19 keys identical WIP-coherent (gate `snapshot-request.mjs:35-39` vs broker
`environment.rs:16-36` byte-identical incl `CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER/
RUSTC_WORKSPACE_WRAPPER`; land WIP). V83 unit-gate WIP checkout fix + sync probe (runs rk-pipeline
from checkout not snapshot with HEAD==commit guard vs broker refuses root-in-snapshot; defaultRun sync
`nodeSpawnSync :17` so win32 probe safe — land it).

## (4) Dev & package stage risk section (slice 06, never run on real broker)

- Hard blockers B5 (xcode), B6 (gradle), B3 (first release-profile false cold — package's first
  `--profile release`/universal/triple build is the canonical victim), B7 (hang), M44/M46 (Dell
  endpoint/timeouts), M45 (no cancel), M47 (reconnect amplification), M48 (survivors), M30 (consume
  enrollment + `/usr/bin/git`), M31 (ndk), M32 (win recipe signing in-job), M28 (cmake/command
  sidecars), M25/M26/M27/M29/M33/M60 (sidecars/journeys/prepare/hand-back/nested heuristic/scratch).
  Package-specific: sealed bound-targets cross-check V72 and expected-argv V73 are sound; swap-helper
  readback N22 unproven-equivalent; `wired:false` N21 hides B5/B6/B3 until mid-run.
- Real-run ordering risk: class A carried scans must pass B1+B2+M10–M18 first; then compile carried
  cargo+adapter; then package right-release build carried children (`:1329-1344`); then qualify on
  sealed bytes (no compile); release upload only. Broker `service.rs` stage-relevant windows read;
  scheduler/cache/recovery internals not line-read (unresolved).
- iOS package (`iosPackage`/xcodebuild archive) spawn route in `build-release/ios-*` not fully opened —
  folded into B5 on dev evidence; package-side line proof pending (unresolved).

## (5) Fake-broker drift section (slice 08 + neighbors)

- Root cause: each side tests self-consistency with its own fake (M43); default fake models pre-fix
  broker (M34); read-only shapes drift both directions (M35); stage evidence always-present wrong shape
  (M36); exit decoupled from result (M37); no doctor probe (M38); pins stale + version unpinned (M39);
  discovery divergence (M40); adapter grammar reimplemented (M41); two fake metadatas neither is cargo
  (M42); carrier value checks missing (N29); status/lease/claim timing drift (N30–N32); version-probe
  shape (N34); synchronous only (N35); nested depth heuristic (N36a). Fix order: M43 shared vectors
  first (locks both sides), then M34+M35+M36+M37 (correctness of every carried test), then M38+N35
  (route/async), then M39+M40+M41+M42+N29–N34+N36a (fidelity), with M21/N3 WIP landings inside.
- Clinic: after M34–M37 fixes, `carried-cargo.test.mjs:87-88,242`, `cargo-classification.test.mjs:60-111`,
  `snapshot-broker.test.mjs:139-140,397-410`, `snapshot-request.test`, `broker-client.test.mjs:22-36`
  expectations change (enumerated per finding); `cargo-classification.test.mjs:105` audit exemption and
  `:242` stale refusal must flip, not be deleted silently.

## (6) Undocumented interface inventory — the contract that should exist (appendix)

### 6a. Request kinds (broker enum `request.rs:521-568`, all 20)
FORECAST, MANAGED_AGENT, SUBMIT, WATCH, STATUS, CANCEL, LIST, DRAIN_STATUS, STORAGE_GC, PRUNE_PLAN,
PROMOTE, CONSUME_PREPARE, CONSUME_COMMIT, QUALIFY, QUALIFICATION, QUIET_ACQUIRE, QUIET_RELEASE,
QUIET_STATUS, SNAPSHOT_LEASE_STATUS, GRANT_RECEIPTS. Untyped extras intercepted raw only:
PAUSE_ADMISSION{protocol,runtime,reason?,holder?}, RESUME_ADMISSION{protocol,runtime}
(`client.rs:378-398`, `server/gate.rs:265-303`) — must become typed (M1). Gate uses: MANAGED_AGENT+
SUBMIT (cargo/adapter), STATUS/SNAPSHOT_LEASE_STATUS/QUIET_ACQUIRE/QUIET_RELEASE/LIST/WATCH/CANCEL.
Gate never uses: FORECAST/DRAIN_STATUS/STORAGE_GC/PRUNE_PLAN/PROMOTE/CONSUME_*/QUALIFY/QUALIFICATION/
GRANT_RECEIPTS/QUIET_STATUS (Legion/installer/operator surfaces; ConsumeOperation variants
Build|Dequeue|Publish PascalCase `native_consume.rs:14-18`).

### 6b. Carrier / descriptor / request-ref / lease fields
Carrier file `<controlRoot>/snapshot-requests/<uuid>.json` (0600):
`{schema: rustright.snapshot-request/1 (legacy rightkit.../1 until L-1, shims.rs:233),
commit{sha(40|64hex),tree}, snapshot{path(abs),expectedTree}, declaredInputs{inputs[] (non-empty
relative)}, ownership{handle(non-empty)}, primaryCwd(abs), argvSha256, nonce(64-128 lower hex)}`
(`snapshot-request.mjs:190-225,205-216`; `shims.rs:118-141,233-245 deny_unknown`). Descriptor
`{path,sha256}` ≤4096B `Buffer.byteLength`, file regular non-link direct child of
`<control>/snapshot-requests`, ≤4MiB sha-checked (`snapshot-request.mjs:221-222`; `shims.rs:117-122,
154-216` O_NOFOLLOW). Env: `RUSTRIGHT_SNAPSHOT_REQUEST={path,sha256}`,
`RUSTRIGHT_CALLING_GATE` required (`snapshot-broker.mjs:156-157`), `RUSTRIGHT_SNAPSHOT_GRANT` deleted
(`:153-154,238`), `RUSTRIGHT_ADAPTER_ENV` (+legacy `RIGHTKIT_ADAPTER_ENV`) reserved-checked
(`adapter-env.mjs:9-10,18-24,63-73`; `adapter.rs:120-164,746-756`; `shims.rs:81,88-115,369-381`).
Request-ref `{schema: rustright.request-ref.v1, requestId, submittedAt, workspace}` atomic temp+rename
(`cli.rs:300-309`; gate `:31,244-248`). Lease status: schema `rustright.snapshot-lease-status.v1`,
states ACTIVE|REAPED|UNKNOWN, echo snapshot+ownership (`build-ownership.mjs:77-78,872-882`;
`snapshot_lease.rs:11-38,42-45`; `request.rs:500-507`; `service.rs:1660-1672,1672`). Sub-handle
`<handle>#<uuid>` (`snapshot-request.mjs:181`; BROKER:96); marker `owner.json broker{submitted,at,
handles[]}` + sibling `owner.json.mark.lock` (`ownership-mark.mjs:21-47,52`; N36g). Limits: declaration
≤1MiB (→768KiB per M3), carrier ≤4MiB, descriptor ≤4096B, lease query 4096/1024 (`service.rs:1660-1666`),
`requestId` 1-200 `[A-Za-z0-9._-]` (`journal_impl.rs:122-129`), nestedChildren cap 8192
(`snapshot-request.mjs:404`; `service.rs:7971`), `GRANT_RECEIPTS maxCalls 4096` (`request.rs:147-148`),
wire 1MiB/line (`lib.rs:38,41`), broker pending 8MiB (`connection.rs:21,23`), gate receiptSync 64MiB /
capture 256MiB.

### 6c. Receipt / payload signed tree
Receipt: `snapshotEvidence{requestId,brokerVersion,executor,commit,snapshot{path,treeAtStart,treeAtExit},
declaredInputDigests,ownership,outputDir}` (8, `snapshot-request.mjs:42`; `service.rs:2557,7271-7282`;
`attestation.rs:245`); `stageEvidence{freshness,attribution,toolchain}` (3, `:44`; `:275`; collect
`service.rs:2568` filter_map); `resultEvidence{exit,signal,timingsMs{admission,queue,bind,cargo,
afterExitBind,diagnostics},diagnosticsPath}` nested (`service.rs:2572,7367-7376`; `:275,280`; M21);
`nestedChildren[]` adapters (`:403-449,555`; cap 8192); retired `verdict/refreshed` REFUSED both sides
(`attestation.rs:262,282,284`; gate `:542-543,553`; direct-receipt drift N28). Freshness compact
`{available,source:fingerprint-census,fresh,notFresh,unitsAtStart(count|null),censusMs,
invalidationReasons[]or-null→invalidationCounts/causeCounts/unexplainedCount/rebuiltUnits(+detail
service.rs:6952),coldCause}` (`cargo_evidence.rs:240,297-311`; `service.rs:6831,6834,6855,6860-6880,
7138,7409-7421`); attribution `{available,source:cargo-json,units:0N,packages,errors,truncated}`
(`:410-433` rows → compact counts; never adapter); toolchain `<name>|cargo:<64hex>|rustc:<64hex>`
(gate `:48,528`; broker `:7517` empty-digest gap N25). Provenance envelope schema
`rustright.provenance.v1`, ES256, keyId=sha256(SEC1), P-256 ieee-p1363 over
`rustright-provenance-v1\n`+canonicalJson byte-sorted keys; binds host/brokerDigest/jobId+receipt.id/
nonce/requestDigest (`provenance-verify.mjs:57,85,104-105`; `attestation.rs:83,154,203,322`;
`job_service attestation.rs:32,87`; pins `broker-pins.mjs:39,44-45,101`). Evidence file
`<id>.evidence.json` + signed `detail{path,sha256}` (`service.rs:6943,:76,:6952`; verify
`attestation.rs:288`; gate has no reader N27). Handshake capabilities
`[journal-v1,snapshot-readonly-v1,stage-grant-v1,snapshot-adapter-v1]` (`response.rs:54-70`;
`service.rs:1551`); gate requires `[snapshot-readonly-v1,snapshot-adapter-v1]` via doctor
`.broker.capabilities` (`rightkit-gate.mjs:1905,1912-1921`; `doctor.rs:248-257,353-365`).

### 6d. Env names (RUSTRIGHT_ primary, RIGHTKIT_ legacy until L-1; RUST_RIGHT_ in neither repo)
Roots/socket: `RUSTRIGHT_DATA_ROOT/MANAGED_ROOT/EXTERNAL_ROOT/CONTROL_ROOT/BUILD_BROKER_SOCKET`
(broker first, gate must add M19; socket Windows default gap M44). Gate-internal only:
`RIGHT_RELEASE_*` (broker reads none; SWIFT* env on neither side, only `SWIFT_PACKAGE_*` error codes
gate-side). Broker-internal + `RUSTRIGHT_` canonicals whose `RIGHTKIT_` twins gate still uses (sweep:
320 gate vs 229 broker names; `comm` sets in slice 05). Managed cargo 19 keys identical
(`snapshot-request.mjs:35-40`; `environment.rs:16-36`; `shims.rs:27-47`; WIP strips exactly V25/V82).
Adapter reserved exact+prefix lists byte-identical both sides (`adapter-env.mjs:18-24`;
`adapter.rs:120-164`; `RIGHTKIT_CARGO_SCAN`/`RIGHTKIT_CLASS_A_LAUNCH_GUARD` never forwardable).
`RIGHTKIT_BUILD_OWNERSHIP` token lineage/exact (`build-ownership.mjs:1013-1026`);
`RUSTRIGHT_JOB_ID` survivor rows (`survivors.rs:30-31`); `RUSTRIGHT_QUIET_LEASE`
(`rightkit-gate.mjs:1088`; `quiet_impl.rs:300`); `RUSTRIGHT_TEST_PORT_BASE` proposed (N36f);
`RUSTRIGHT_RECONNECT_WAIT_MS` (`client.rs:433`, M47); `RUSTRIGHT_CALLING_GATE` required;
`RUSTRIGHT_EXCLUSIVE/RUSTRIGHT_GATE_RUN` (docs still RIGHTKIT_ per N36i).

### 6e. Folders
Snapshots `<managed>/snapshots/<sha256[:16]>` Mac/Dell, Linux `$XDG_STATE_HOME/rightkit/snapshots`
(V61); broker `<external>/snapshots` + expected dir pre-exist + same-volume check
(`snapshot.rs:61-85`). Leases `<external>/snapshot-leases{,/claims,/completed}` broker-created
(`:277,330,344`). Generations `<ws>/<12hex>/g/<NNN>/{target|t}` under `<control|external>/
rightkit-build-control/ws` (V42; leaf rule M54; key 12-hex NOT snapshot 16-hex). Managed roots legacy
`/Volumes/D/.rightkit-managed`, `D:\.rightkit-managed` + renamed `/Volumes/D/.rustright`,
`D:\.rustright`, `~/Library/Application Support/RustRight/broker` (M20). Runtime dir `runtime` not
`runtime-current` (B10). Cache V2 `targets|test-targets/<platform>/<app>/<fingerprint>`, cargo-home,
sccache, leases/, gc/ (`cache-policy.mjs:88-118`) vs broker global/cargo-home + compiler-cache
(V43). Swift scratches: gate direct `<tmpRoot>/rightkit-swift-scratch/<sha16>/<scan-id>`
(`deterministic-scans.mjs:1907-1916,2277-2279`); broker `<outputDir>/swiftpm/<sha256 pkg-relpath>`
(`service.rs:5929`); different paths/routes, no shared warm state. Snapshot-requests
`<controlRoot>/snapshot-requests/<uuid>.json` + `grant-<v4uuid>.json` (`snapshot_grants.rs:136-198`;
`sweep :289-310`; orphan sweep N10). Pins `<dataRoot>/pins/broker-<platform>.json` schema
`rightkit.broker-pins/1` (M57). Temp `TMPDIR=TMP=TEMP=<ws layout.temp>` (`service.rs:1533-1535`);
job HOME broker's own (macOS `:1487-1490`); system-volume guard (`:5579-5587`).

## (7) Ordered fix plan — one gate candidate + one broker candidate

Dependency order (land WIP first — direction uniformly toward broker per critic, no new WIP-introduced
mismatch found):
- GATE-0 (land working-tree WIP as-is): resultEvidence nesting (M21), carrier schema rename (N3),
  withoutManagedRustEnv 19-key strip (V25), VERSION_PROBES exact (N9), fmt_check_shape mirror + alias
  expansion (M13/M15 partial), spawnBrokerClient routing x4 files (N4), pnpm grammar + live fallback
  (V80), unit-gate checkout (V83), RUSTRIGHT_ socket spelling. Proof per finding; `node --check` each.
- GATE-1 (unblock every carried send): B1 submit-mark one-line fix + new createSnapshotBroker test
  (minutes: `snapshot-request.test`, `build-ownership-submit.test`, probe-mark rerun).
- GATE-2 (declarations+shapes): B2 `.rustright`-first read + overlap both paths; N23 separator;
  M10 `--config` exempt; M11 cluster walk; M13 fmt refuse; M14 locate no-lock move; M15 alias expand;
  M16 compile-class = BUILD set; M18 audit `--db --no-fetch`; M12 tree route-or-metadata; M60 swift
  scratch strip. Tests: `carried-cargo.test`, `cargo-classification.test`, `cargo-target-shim.test`,
  `compile-inputs.test`, `snapshot-request.test` (minutes, focused only).
- GATE-3 (ownership/concurrency/timeouts): M5 unmark-unsent; M6 reboot guard; M8 `.reclaim` pid+probe;
  N13 token lock; N14 REAPED poll; N8 markSend assert; N4+N16+N18 shim routing; M45 async+cancel; B7
  timeouts; M46 ≥130s status/doctor/lease; M47 reconnect bound + per-stage doctor preflight; N36c
  `--wait` or retryable code doc; N36d case-fold. Tests: `build-ownership.test`, `snapshot-broker.test`,
  `door-*.test` focused (minutes).
- GATE-4 (env/roots/pins/doctor): M19 envNamed all five + M20 renamed candidates + B10 runtime rename +
  M58 probe order + M39/M58 re-enroll + rk-pipeline digests/version/revision + brokerVersion compare;
  M40 installed-first discovery; N33 legacy shim marker; M38 fake doctor + one door route test through
  fake client. Tests: `managed-roots.test`, `broker-pins.test`, `door-route.test` (minutes).
- GATE-5 (stages): B5/B6 xcode/gradle direct-session exemption (or adapter registration + ADAPTER_PROGRAMS);
  M25 sidecars in runDevTest; M27 prepare via door; M28 cmake adapter + command validation; M29
  hand-back via receipt outputDir; M31 ndk `--locked` + probe; M32 recipe split/refuse; M33 nested skip
  when carrier map; N21 wired flag honesty; N22 outputDir-first; M59 cache ordering. Tests:
  `build-launch.test`, `development.test`, `stage-sidecar.test`, `door-stages.test`,
  `swap-helper-bundle.test` (minutes).
- GATE-6 (fake fidelity + docs): M34 default-true + M35 transliteration + M36 compact shapes + M37
  exit-coupled result + N29 value checks + N30/N31/N32 state/lease/claim + N34 exact probes + N35
  queued/paused/coalesced + N36a parents/layout + M42 single recorded metadata (expect listed test
  flips); doc edits B8/B9/B11/M49–M57/N36g–N36k + shared vectors M43 vendor. Tests: full
  `packages/release` touched files only (minutes each), not whole suite.
- BROKER-1: B3 per-profile/triple exemption (or durable history) + unit flip; M22
  `{available:false,reason}` on evidence timeout; M4/M6 status reap-proof (or startup sweep); M5
  pause refusal into job-service (or record_refused callback); M7 windows Job-Object scan; M1 typed
  Pause/Resume; M2 line caps ≥8MiB + terminal REJECTED + shared constant; M3 pre-write size refuse;
  M9 per-call wait-for-holder; N5 deny_unknown + evidenceClass type; N6 id doc/field; N7 canonical
  refuse; N11 sealed bind; N15 Drop log + retain; N25 toolchain refuse-incomplete; N26 `receipt:null`;
  M44 windows pipe default; M46 busy code or lower wait; M48 windows survivors + snapshot-bound
  stopSurvivors; N36b take(1MiB) + group kill; N36e timeout enum; N36f port-base env; M30 consume binding
  commit + git resolution; M2/M47 docs (`RR_UNEXPLAINED_COLD`, adapter forecast note). Tests: focused
  `cargo_evidence`, `snapshot`, `check_admission`, `command_policy`, `adapter`, `recovery`,
  `forecast_query`, `toolchain`, `connection/framing/codec`, `service` windows (minutes each),
  never full suite here.
- Final sweep checklist (Mac + Dell, then Linux direct): compile gate (`node --check` touched files);
  focused gate tests per fix above (minutes not full suite); real-broker `check --commit` (B1 proof);
  `.rustright`-only declaration fixture (B2); first `--profile release` in warm target SUCCEEDED (B3);
  clippy compact receipt (B4, post-7.12); `--to dev --platform ios|android` (B5/B6); hang/timeout +
  SIGTERM-cancel + reconnect-bound + pipe-burst + survivor-reap (B7/M44–M48); uncommitted-files review
  (`git status` — sibling WIP coexistence); concurrency (parallel same-snapshot both verify; two-gate
  BLOCKED/retryable); dev/package matrix (§4) incl swift scratch share, ndk locked, cmake fixture,
  journeys refusal-or-plan, hand-back paths, consume-dirty, win sign-split; docs-vs-code grep gates
  (verdict/refreshed==0 outside retirement, `checkAdmission.trustedGate`==0, cloud claims scoped,
  carrier/receipt tables match code constants, fixture byte-diff clean).

## (8) Unresolved items (verbatim, per slice + critic)

Slice 01: attestation.rs beyond line 70 not opened (receipt binding belongs to attestation slice);
gate test bodies broker-client/build-ownership-lease/broker-pins not opened; shims carrier/grant tests
after :762 + service status/lease tests grep-located only; DataRootGate Diagnostic override keys
uninspected (`gate.rs:248-255`); JOB_EVENT STDOUT chunk size not inspected (STATUS/GRANT finding is
aggregate lines, not streamed stdout); Windows-only behavior read not executed on this Mac — Dell
real-broker verification still required for spawn-path finding.
Slice 02: Windows verbatim repo-key + lease Job-Object reap code-only, no Dell run; journal replay
terminal handling for started snapshot jobs not fully opened (wedge assumes not resumed); client carrier
reader (`shims.rs snapshot_carrier`) file-name/descriptor rules vs gate `<uuid>.json` writer adjacent
slice (only broker grant-file reader checked); gate test files (`snapshot.test.mjs` 1907 lines,
snapshot-*.test, build-ownership.test) + `tests.rs/gate_tests.rs` not full-read; real-broker dev/package
lifecycle never run (adapter lease shares per-call carrier code checked).
Slice 03: broker host adapter registry config not in either repo (compile+snapshot flags,
snapshotSubcommands, scriptPositional, scratchRoot, spec.env vs `build-launch.mjs:62-64,210,230,240`
assumptions) — needs host config read Mac+Dell; admissionContext minter for
`admission_context.rs:199` route (`service.rs:3312`) unfound — exact-shape list (`:244-249`) stricter
than class_a_scan, gate traversal unconfirmed; dev/package never run + builds prohibited (source+test
evidence only); working tree changed mid-audit (25 modified files; line numbers ±few); `qualify_impl.rs`
always-not-configured (`:355-357`) gate consumer outside slice not inspected.
Slice 04: `build-ownership.test.mjs` (2188 lines) grep+windows only — hence complete=false;
`bin/rightkit-gate.mjs` (3214 lines) grep+windows only; `service.rs` (~7000 lines) + `snapshot.rs` lease
windows only; `eadb4ba/849091b` freshness + `b84ae60` SwiftPM scratch outside slice-04 not verified;
`broker-client.test.mjs` + `fake-broker.mjs` partially grepped; no real-broker run (code+probe inferred
except finding 1 executed with fakes).
Slice 05: `b84ae60` compile not executed (builds forbidden; `:5856` mut fix source-read only); live
adapter registrations (swift/pnpm/node flags, env allowlists) in host config not repos — registered
flag/env sets unverified vs gate assumptions; `deterministic-scans.mjs` non-env ~1400 lines unread
(out of slice); SwiftPM appended `--scratch-path` for non-build subcommands (e.g. `package
dump-package`) unverified — no swift run; host launch env (RIGHTKIT_* vs RUSTRIGHT_* on Mac/Dell)
uninspected — M19 severity depends on it + L-1.
Slice 06: adapter registrations (pnpm/node/swift/qa flags, env lists, snapshotSubcommands, cargo-ndk
external tool) in installed host config not repos — argv grammars verified to policy-crate only;
`service.rs` outside stage windows not line-read; carried metadata stdout `target_directory` vs receipt
`outputDir` equivalence needs one real unit-package run (N22); iOS package (`iosPackage`/archive) route
in `build-release/ios-*` not fully opened — folded into xcode blocker, package line proof pending; no
real dev/package run (code-derived failure modes, tests named per finding).
Slice 07: `deterministic-scans.mjs` ~160-1850 planning scans searched not line-read (a receipt consumer
hiding in planning code could be missed); `rustright-cli status` re-serialization not opened (absent vs
null affects only N26 severity); broker extended request-core digest (commit/snapshot/declaredInputs/
ownership into protocol request_core) not opened broker-side (`live.request_digest` origin unverified);
`service.rs` (8629 lines) cited windows only; no real broker run (code-derived, not Mac/Dell observed —
dev/package never-run per brief).
Slice 08: fake-broker user tests (adapter-env-qa, build-ownership-submit, cargo-classification,
door-closure, door-stages, qa-snapshot, snapshot-check, snapshot-metadata, snapshot-request,
swap-package-stage) inventoried + grep-skimmed, not full-read; installed broker state Mac/Dell (runtime
version, brokerDigest, shim marker generation, adapter registry) not inspectable from repos — stale-pin
+ legacy-shim findings need host readback (`rustright doctor --json`, broker-pins show); `service.rs`
(~8000 lines) + `adapter.rs` targeted windows only (nested_target site + full check_argv grammar not
fully traced); gate swift/bin-path consumers vs broker swiftpm scratch layout (`eadb4ba`) outside slice
not traced.
Slice 09: `rightkit/contracts/*.v1.schema.json` (8 content schemas) listed + grep-checked, not opened
(out of broker/gate scope); BR19 fields profile/stage/resources/environment presence in real receipts
unverified against a body (only GATE never defines them); broker-pins record absence from docs asserted
from listing + attested-runs read (full-text grep for `broker-pins/1` not run); `service.rs/
check_admission.rs/snapshot_grants.rs` cited windows only; Dell/Windows-only doc claims source-only, no
Dell run.
Slice 10: who sets `RUSTRIGHT_BUILD_BROKER_SOCKET` on proven Dell runs unfound (no setter in gate,
shims, rustright-install; likely agent-launch/login env not inspected); installed Dell/Mac
`config.json` bounds.jobDeadlineMs/limits.classDeadlineMs nonzero unknown (host files not in repos) —
B7 mitigation in practice unknown; `rustright-process` contained runner terminate/tree-kill +
snapshot_grants REAPED criteria not in slice (orphan-tree after client kill on Windows Job Object not
directly inspected); queue admission no explicit queue-length cap found (unbounded-queue claim by
absence; scheduler internals not fully read).
Critic: broker `.rs` bodies via scoped sed/grep windows (rustright outside workspace root, reader
confined), not `read_file`: compact_attribution+unexplained_units, RR_UNEXPLAINED_COLD/status lines,
client pause/connect, request enum, COMPILE_INPUTS/declared_inputs, uncarried probe, adapter
snapshot_subcommands/registration (registrations in host config not source), environment keys,
snapshot scan/status, version_probe forms; slice-05 DEV xcode/gradle grep-only (full ios-dev/android-dev/
development door paths not line-read); `doctor --json` runtime.version unobtained (earlier printed
nothing; broker may be down) — pin comparison rests on `5af765b` vs `b84ae60` + `rustright --version`
0.1.0; gate test bodies (broker-client, build-ownership full, door-route, snapshot-request WIP) grepped
not line-read; broker tests referenced not re-run (no builds); Dell-only (pipe, `.cmd` shim, Job
Objects) source/config only, no Dell run.
