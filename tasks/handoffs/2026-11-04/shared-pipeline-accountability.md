# COLD-START HANDOFF: Shared pipeline optimization — incomplete work & execution failures

## 0. Handoff Control

- **Handoff ID:** shared-pipeline-accountability
- **Created:** 2026-11-04T16:40:15.431654+00:00
- **Source task / chat:** Codex Bottlenecks, 01a1a59c-5993-7b40-8879-2416ae789b1b
- **Target:** Adrian & any successor, including Claude
- **Author:** Codex / root, accountable for this session
- **Receiver role:** Review & continuation planning
- **Proceed mode:** READBACK_ONLY
- **Readiness:** READY_WITH_GAPS
- **Handoff reason:** Adrian lost confidence after incomplete delivery, paused then cleared the goal, & requested full summary, failures & bounded next steps. This packet does not restart work.
- **Source evidence mode:** LIVE_CONTEXT
- **Transcript evidence path:** NOT_APPLICABLE: LIVE_CONTEXT packet; original transcript pointer is bound in source metadata below.
- **Source prefix receipt:** NOT_APPLICABLE: LIVE_CONTEXT packet; manual frozen-prefix metadata is preserved in shared-pipeline-source.json.
- **Source binding metadata:** /Volumes/D/claude/rightkit/tasks/handoffs/2026-11-04/shared-pipeline-source.json
- **Packet path:** /Volumes/D/claude/rightkit/tasks/handoffs/2026-11-04/shared-pipeline-accountability.md
- **Receipt path:** /Volumes/D/claude/rightkit/tasks/handoffs/2026-11-04/shared-pipeline-accountability.receipt.json

## 1. Intent & Mission

- **Original user intent verbatim:**

> your singular focus should be to optimize my build pipeleine, not to build, not to publish. to optimize every step of the process

> to clarify, you're not optimizing rk and hr pipelines. you're using them 2 as test, you're optimizing the shared pipeleine that all apps etc use. the reason rk and hr since they're setup different. hr is an app with i think 3 workspaces and rk is crate we own and publish. so please fix the goal

> class a outside broker, needs completion before admitting to broker. enforced by broker. class a enforved by gate

> im taking a nap so don't ask me any questions that block you, keep going till
> 1. you've tried every possible combination, keep them bounded so each isn't running for hours
> 2. you're blocked on something that prevents you from doing anything at all

- **Underlying goal:** optimize shared gate/broker pipeline for all apps, packages & crates using RightKit & HeardRight as different real fixtures, later adding Pulse through verified user relay. Measure both Mac & Dell. Establish baselines, change measured bottlenecks, retest correctness & timing, report lowest observed correct stage values. Include Class A, Class B, compiler work, complete build, qualification, CI, cache layers, concurrency, resource allocation & daily Legion regression/warmth checks.

- **Current objective:** deliver honest accounting & continuation context only. Goal was cleared; no autonomous optimization remains active.

- **Definition of success:** complete explicitly bounded valid matrix on both hosts; identify ineffective/unsupported/blocked cells; preserve required coverage & signed evidence; verify enforced invariants on actual runtime; demonstrate daily measurement; report matched before/after results without confusing published/source/installed/tested states.

- **Out of scope:** product publication, fixing every product crate, product feature work, whole-gate rewrite, new benchmark platform, blanket all-crate builds, fresh target trees or cache clearing. Compilation is a measurement instrument, not delivery of product releases.

- **First responsibility:** verify packet & live state, read back what is complete and missing. 

- **Must not do first:** resume the old goal, restart matrices, republish the gate, reinstall broker, delete stale markers, or redesign runners.

### Background: Everything brought into this chat

### A. Earlier audit supplied by Adrian — reported facts, not work performed by this session

The audit described two weeks of avoidable delay: the 31 Oct JSON/unit-list request became a serial full-run loop; 92 gate runs had Class A median 146 seconds/crate; dependency-freshness median 120 seconds, 41/81 timeouts, no measurement yet advice/pass, costing 1.99 of 2.66 Class A hours. It cited uncompiled bulk edits across 684 files, broken crates surfaced one at a time, roughly 65 sleep/poll loops, repeated gate releases after objections, false/reversed batching and timing claims, Dell lane left blocked, and a cache fix that initially did not cache timeout results. These are attributed to the supplied audit/earlier agents, not silently reassigned as my actions.

### B. Fable proposal & Adrian's topology correction

Attachment: `/Users/adrdsouza/.codex/attachments/e94fea16-523c-4c65-af3f-7712c6a8e5cd/Pasted text.txt`.

Fable first proposed an all-units exception + installing 0.6.44 + whole-repo command, then withdrew the exception workaround. It clarified that app workspace lists & unit-list parsing existed, but listed units still ran serially. It proposed removing RightKit's special case after migration to one workspace. Correct retained shape: committed workspace inventory, automatic changed units plus reverse dependents, compatible grouping, complete per-unit reporting. One workspace does NOT authorize compiling all crates on every edit, deleting CI selection, or treating old quoted exception wording as a new blanket exception.

### C. Muse report

Muse separated published fixes from active code, identified serial `--units-file`, repeated scans, advisory timeouts, missing guardrails & incomplete sweep. Reconciliation found its fallback-config-only assertion that Mac necessarily ran 0.6.29 unreliable: host launcher can select another sealed gate. Its claim that no timeout budgets existed was also wrong. Making optional freshness timeout fail compilation would create another blocker. Always inspect actual entry, hash & process. Later 0.6.45 shared Class A was partial progress, not full grouped compile implementation.

### D. Claude's operational accounting

Claude reported 0.6.42–0.6.45 published, Mac using 0.6.45, 65 crates published with checksums, one workspace/root lock with 1,475 packages, circles broken & advisory ledger approved. Pending: roughly 97 crates never gated, 10 failures with uncommitted fixes, dependency-driven version-bump inventory unknown, 48 staged npm packages largely unchecked, batched compile/affected graph absent, HR rerun & consumer pins incomplete. It reported wrong dependency floors, untested sqlcipher DB behavior, bulk-test-removal damage, stale-lock/receipt/retry/process-group problems, and alleged another Codex chat edited its tree after saying it would not. That earlier allegation's exact ownership was not independently settled here; preserve provenance instead of assigning it casually. The product sweep/publications did not become this optimization job.

### E. Adrian's required operating model

One canonical target per Cargo workspace, enforced by broker. Full Class A outside compile queue/pool, completed before admission, gate executes it & broker enforces evidence. One active admitted Cargo combination per workspace; duplicate/queue semantics must be verified, not weakened. Broker owns shared resource pool; Cargo parallelizes work inside its allocation. Daily Legion warm measurements plus gate/broker inefficiency checks. Study whether A/B runs once or more, parallel/sequential ordering, cache effects, native host differences & historical evidence. Terra subagents do heavy implementation; root integrates & owns results. No blocking questions while Adrian slept.

### F. HR readiness & Pulse addition

HR supplied `a5ea6bdbdc518910e7dceae517b089038b744932`, 213/213 Class A in 21.4s, gate 0.6.45 sidecar-metadata fix, with Rust compilation unproven. Actual benchmark compile failed after 76.871s on published dependency API mismatch before generic Class B. Later hrmac reported local `506cab27211f1445d3d8c262d9cb0c4925f26f03`; dictation-core >=0.2.2 unavailable, QA 0.10.3 requires real-data/snapshot harness port rather than a simple repin. Do not publish product dependencies merely to manufacture benchmark success.

Pulse fixture `bc4eab382ec5761caea7436e2b5341e27cb4e9cd` was confirmed on both hosts; fixture package pin release 0.6.42, host-selected gate 0.6.45. Its eight declared workspaces include root, engine, hub/qa-e2e, hub/src-tauri, membrane/engine, swift/RightKitHub/native, winadmin & windows. Both-host Class A passed; no Pulse compile was executed by this session. Owner reported uncompiled product changes & possible clippy/API drift.

## 2. Current State

- **Phase:** stopped, incomplete. Goal cleared. No new benchmark or implementation authorized merely by reading this packet.
- **Completed:** 11 local gate commits; latest focused package suite 45 passing Node tests; Mac HR 12 Class A concurrency observations & Dell HR 12; Pulse Class A once/host; Mac RK candidate A lane block; one real two-crate union and one matched generic-batching on/off pair; historical request/failure reports; original release-only instruction recovered.
- **In progress:** exhaustive valid matrix, all-cache invalidation/transition tests, same-candidate cross-host compile proof, contention/fairness measurements, broker-enforced Class A witness, build/qualification/CI measurements, active daily Legion monitor, final deployed gate improvement.
- **Blocked:** HR real compile on published dependency/API problems; CI proof remains disabled; width flag does not produce independent width under inherited Cargo jobserver. These do not block every independent lane.
- **Not started:** Pulse full compile, complete signed-build/qualification comparison, live CI candidate run, daily monitor activation.
- **Active goal / plan:** none; user cleared goal after explicitly pausing it. Successor remains READBACK_ONLY until directed otherwise.
- **Last action:** accounting/handoff writes only. Mac batch pair completed; remaining Dell matrix driver was explicitly terminated during pause cleanup.
- **Last observed result:** Dell driver, launcher & gate absent; ownership named job absent; exact lease UNKNOWN/empty. Stale owner marker left untouched. Unrelated broker CONTROL activity prevented claiming global broker idle.
- **Current hypothesis:** remaining costs include duplicate generic/publisher check/clippy/test obligations, repeated snapshot verification & preflight, preparation/inventory overhead, process startup and queue/receipt costs. Each needs matched measurement, not wholesale redesign.

### Measured evidence — distinguish scopes

| Workload | Result | Meaning |
|---|---|---|
| HR A, Mac, lanes 1/2/4/8 | Medians 30.138 / 25.960 / 24.585 / 24.329s | 12 passes, zero observed scan-cache hits; historical runtime cohort |
| HR A, Dell, lanes 1/2/4/8 | Medians 64.455 / 55.694 / 55.756 / 51.894s | 12 passes; platform/background conditions differ |
| HR full compile | 76.871s, failed | Dependency mismatch; no successful compile or B optimum |
| Pulse A | Mac 20.969s; Dell 38.406s | 89 passed each; different skipped B plans; not compile proof |
| RK Mac 8c A cross-tool-call repeat | 31.145s; 833 hits, 276 ran, 2 skipped | Genuine cross-call syntax reuse |
| RK Mac 8c A concurrency block | Lowest warm sample 29.801s at 8 lanes | Eight resumed warm samples all 833 hits; first exposure per lane separate, one contaminated sample excluded |
| RK Mac 8c package gate | 71.625s; 45 tests passed | Two pnpm broker adapter requests, Node suite; no recorded nested Cargo children/compiler round trips |
| RK Mac 8c initial two-crate union | 152.525s whole run; B 89.8s | 26 B rows, 18 broker receipts, eight grouped commands; NOT 152s Class A |
| RK Mac same-source batching on/off | 151.563 / 168.712s whole run; B 74.7 / 89.1s | 26 B rows each; 18/22 carried requests; publisher grouping remains on in both; one ordered pair only |
| Dell RK 698614 A recovery | 111.590s, pass | Offline metadata repaired after failed seed; no symlink deletion required |
| Dell RK 8c new driver | First passing cell about 104.3s wall | Subsequent cells not a finished matrix; stopped after user pause |

Never compare different cohorts as clean causal speedups. Gate scans, test execution, broker queue/admission, phase totals & whole-run walls are different measurements. Preserve timeouts/unknowns as non-success. No valid universal completion percentage or global optimum was established.

## 3. Environment & Active Work

- **Work type:** MIXED
- **OS / shell:** macOS / zsh; Dell Windows / PowerShell, Python 3.11.
- **Workspace / repo:** `/Volumes/D/claude/rightkit`; Dell `D:\claude\rightkit`.
- **Branch / version:** primary main; candidate HEAD re-read this handoff turn: `8c6dd59e6360241b601665906afce8077446d617` before adding handoff documentation. All 11 optimization commits local/unpushed. Do not push: repository CI/publication behavior needs explicit control.
- **Baseline revision:** `2dddd6d81ac7b90f121aba3ca102ca244144c79f` (0.6.45), predates implementation cohort.
- **Dirty state:** exact tracked status read this turn follows. Preserve unrelated product files. `AGENTS.md` is regenerated release-profile correction from leggy; do not discard it or stage all files.

```text
 M AGENTS.md
 M ci/gate.config.json
 M crates/rightkit-agent-engine-core/src/cache.rs
 M crates/rightkit-agent-engine-core/src/compactor_retry.rs
 M crates/rightkit-agent-engine-core/src/default_compaction.rs
 M crates/rightkit-agent-engine-core/src/engine.rs
 M crates/rightkit-agent-engine-core/src/engine_loop.rs
 M crates/rightkit-agent-engine-core/src/engine_loop_dispatch.rs
 M crates/rightkit-agent-engine-core/src/engine_runtime_recording.rs
 M crates/rightkit-agent-engine-core/src/engine_session_controls.rs
 M crates/rightkit-agent-engine-core/src/engine_tool_dispatch.rs
 M crates/rightkit-agent-engine-core/src/engine_tool_dispatch_approval.rs
 M crates/rightkit-agent-engine-core/src/engine_tool_dispatch_concurrent.rs
 M crates/rightkit-agent-engine-core/src/lineage_budget.rs
 M crates/rightkit-agent-engine-core/src/mop.rs
 M crates/rightkit-agent-engine-core/src/post_edit_diagnostics.rs
 M crates/rightkit-agent-engine-core/src/post_turn_gate.rs
 M crates/rightkit-agent-engine-core/src/prompt_cache_break.rs
 M crates/rightkit-agent-engine-core/src/read_cache.rs
 M crates/rightkit-agent-engine-core/src/repeat_detector.rs
 M crates/rightkit-agent-engine-core/src/session_state_summary.rs
 M crates/rightkit-agent-engine-core/src/tool_dispatch_plumbing.rs
 M crates/rightkit-agent-engine-core/src/verify_fanout/verifiers.rs
 M crates/rightkit-agent-tools/src/coding_tools/background_shell/process_model.rs
 M crates/rightkit-agent-tools/src/coding_tools/http_client.rs
 M crates/rightkit-agent-tools/src/coding_tools/managed_pty.rs
 M crates/rightkit-agents/src/driver_host/claude_harness/delegation.rs
 M crates/rightkit-llm/src/agent_providers/http.rs
 M crates/rightkit-notifications/src/lib.rs
 M crates/rightkit-notifications/src/platform/linux.rs
```

- **Tools / dependencies:** Node26.8.1, pnpm12.16.1, Cargo1.99.x observed; derive current nearest pins, do not globally upgrade. Mac zsh, Dell PowerShell/Python3.11.
- **Services / processes:** last frozen brokers both bebfe52f67f4ec5132c162c08508a50d06b05d2e. Mac broker SHA4e4ceaffadddb8c58c32bbdc803ea07202e1aff73b2dd0f17149b1d254c76ac7, client SHAedc9f9c41875e462aec8463e1f3603d51b6b42cd220ca74db831a29dabb387e2, pool9. Dell broker SHA6b3a3a769a5d92605f511861ec122c4dab7872195785840be2a6427a044996a6, client SHA1189cb4b29ba89a840ab5ec467f6e42447c99f793306cc918b89f9686fd6091e, pool16. These are LAST FROZEN identities, not a fresh claim peers have made no changes since. Installed selected gate was0.6.45 on both; candidate source was executed directly for its tests.
- **Agents / tasks / threads:** native team inventory this turn: nine completed workers, workspace_batch_fix interrupted, no active implementation subagent. External chats remain independent: Broker (Claude B) on Dell, hrmac, pulsy (Claude B), leggy (Claude B) on Mac. Resolve/check activity with Pulse before messaging; no outgoing message is needed to consume this packet.
- **Scheduled work:** no daily Legion measurement schedule activated by this session; other owners' schedules not freshly inventoried.
- **Credentials / access:** use existing Pulse/win/SSH & provisioned signer. No secret values in packet; no E: access, credential dump, login/setup or environment dump.

### Candidate commits, oldest first

| Commit | Change |
|---|---|
| fa48d81b3 | Shared unit planning, selection, receipt matrix, preparation/probe reuse |
| 0e2515a67 | Compile/publication guard separation, stable preparation config |
| 0d633e43f | Selected npm dependency-closure builds |
| 393208bb8 | Ownership key exclusion & teardown timing |
| d73919c36 | Actual compatible batch execution, direct probes, bulk process-table read |
| 1432b52b3 | Narrow syntax cache & structured preparation diagnostics |
| 323c8db64 | Cache eligibility & signed failure classes |
| 16640c46d | Validated generated-wrapper identity normalization |
| 0427e419d | Preserve authenticated batch failure; bounded bisection |
| 698614b3d | Timing/control instrumentation, signed postchecks, disabled CI-proof draft/cache paths |
| 8c6dd59e6 | Exact tool-call ID exclusion & hash diagnostics for cross-call syntax reuse |

### My failures — owned explicitly

1. **Scope & sequence failure.** I did not freeze a complete executable matrix before iterating infrastructure. I allowed supporting tooling, protocol design & repeated repairs to dominate while requested stages were untouched.
2. **No complete baseline across stages.** I optimized selected A/package paths before establishing build, qualification, CI & real multi-workspace throughput baselines. Four hours later those stages still lacked useful completed measurements.
3. **Too many candidate iterations.** Eleven local commits were not registry churn, but repeated small corrections caused repeated Class A/package runs & re-baselining. I should have assembled known related fixes & verified input identities sooner.
4. **Cache fixes were incomplete repeatedly.** Initial preparation reuse was defeated by unstable configuration ordering, per-run ownership, planner eligibility, generated wrapper paths, then CODEX_TOOL_CALL_ID. Same-harness reuse was prematurely treated as sufficient before separate-tool-call reuse was tested. Preparation reuse itself has no demonstrated hit; syntax reuse does.
5. **Batch planning was mistaken for execution.** Early candidate grouping did not execute through the real grouped path; toolchain proof gaps and six failures exposed this. A planner flag was not enough.
6. **I built fragile measurement wrappers.** Mac lease-status probes timed out, leading to process-uncertain halts requiring separate proof. Dell PowerShell null/exit-status/PATH handling needed repair. Most serious: an Observe/CIM timeout silently broke the loop and killed a healthy gate at66.6s despite a300s budget; full preflight was unfinished. Replacement Job Object driver then needed process-start-time binding to avoid PID-reuse mistakes.
7. **Observer cost & evidence were poor.** Dell observer calls had substantial cumulative wall time; that is not additive CPU overhead or exclusive time lost. Some exceptions were discarded. I had to investigate my own measurement apparatus instead of the pipeline.
8. **I allowed a contaminated comparison.** I released HR's tree for edits without clearly reserving shared host activity. HR lock updates & Class A overlapped RK measurements; harness halted, sample excluded & rerun.
9. **Dev/debug detour was unnecessary.** I elevated stale generated 'dev profile' wording, stopped compile work, investigated an imagined profile migration and described release measurements as noncompliant. Actual human order at2026-11-02T19:06:28.152Z explicitly bans dev/debug. It was already reflected in live gate/broker contracts. I should have resolved source authority before proposing any hold. Original record was recovered; hold withdrawn; warm release remains correct. The detour occurred approximately20:58–21:04 IST while other work continued, not an exclusive stopwatch allocation.
10. **Misleading timing shorthand.** 'Two-crate batch passed in152.5s' omitted that it was the whole gate run. Class B was89.8s; this was not Class A152s. Subsequent matched on/off pair used different B timings74.7/89.1s. Never mix them.
11. **Accepted/repeated wrong intermediate findings.** Examples: publisher requests initially counted per-unit references rather than unique jobs; npm adapter receipt profile label mistaken for actual Rust compiler work; broker pool+K oversubscription claim accepted before owner corrected existing implicit-root token reservation. Corrections are authoritative here.
12. **Designed an ineffective knob.** Candidate `--cargo-jobs` exists, but Cargo uses inherited jobserver pool; `-j` is not independent width control on this route. Do not run/report a fake width sweep. Remove/restrict that claim or implement actual broker capacity semantics only if needed & proven.
13. **Over-expanded unfinished proof work.** CI admission & Class A witness designs consumed effort before the bounded matrix was complete. Both remain disabled/unimplemented end-to-end; papers & schemas are not delivered enforcement.
14. **Historical review overstatement risk.** Indexed/triaged text is not every chat fully read. History reports contain coverage limits, copied relays & unresolved issue states. Full historical baseline across months was not established.
15. **Communication substituted for completion.** Frequent updates described activity and intermediate passes without consistently relating them to unmet requirements. After Adrian challenged delivery, I offered another plan and assurances rather than a completed outcome.
16. **Paused work was not stopped promptly enough.** A previously launched Dell driver could continue cells after goal pause. During accounting I explicitly stopped it. First stop command failed quoting; second identity guard failed UTC parsing; exact held-process UTC-string check succeeded. Driver/gate gone & lease empty were verified. Those extra cells are not credited to the pre-request accounting.
17. **Avoidable retrieval/tool errors.** Oversized transcript/raw-JSON searches produced enormous truncated outputs; unsupported Pulse `--message` and wrong ID routing failed; some peer messages were hard to read because I compressed spacing. Subsequent handoff must use bounded structured extraction & readable exact commands.
18. **Stale artifacts remain a landmine.** Earlier consolidated report says this chat made no edits (true before implementation, false later). Historical evidence/profile sections still say dev hold active; old agent finals recommend pausing release. This packet supersedes those statements explicitly. Do not propagate them as current state.

I did not perform the earlier agents' 684-file test deletion or their npm release churn during this optimization window. Those are supplied-history failures. My failures above are this session's execution/accountability, not a transfer of blame.

## 4. Decisions, Invariants & User Corrections

| ID | Decision | Source / why | Status | Reopen only when |
|---|---|---|---|---|
| DECISION-01 | Optimize shared pipeline; RK/HR/Pulse are fixtures | Direct user clarification & verified Pulse addition | LOCKED | Adrian changes scope |
| DECISION-02 | One existing warm target/workspace, no cold/no-cache/clones/worktrees | Direct user requirements | LOCKED | Explicit new user decision |
| DECISION-03 | Release only; dev/debug forbidden | Original human transcript below, broker item17 | LOCKED | Explicit later human decision, not stale generated text |
| DECISION-04 | Class A outside compile queue/pool & completion enforced before broker admission | User11:16UTC | LOCKED | Adrian changes invariant |
| DECISION-05 | Preserve failure, skipped, blocked & unmeasured distinctions | Audit + required correctness | LOCKED | Never reinterpret timeout as measured pass |
| DECISION-06 | `-j` does not independently bound inherited jobserver width; implicit root token already reserved | Cargo source review + broker correction | ACTIVE_ASSUMPTION | Real deployed-source/behavior evidence changes |
| DECISION-07 | Goal cleared; handoff readback only | Latest user control event | LOCKED | User explicitly directs continuation |

Release-only original: `/Users/adrdsouza/ClaudeProfiles/claude-b-home/.claude/projects/-Volumes-D-claude-rightkit/0d526158-e8f7-4be7-9144-850d4c77796c.jsonl`, lines2389/2398, human queued message source_uuid3e30fa6a-f541-4cec-9372-6dc8cc96a9dd, timestamp2026-11-02T19:06:28.152Z, origin.kind=human & humanTurn=true. Text: “i wnat all dev completely gone. all debug too. completely gone, all apps. not allowed through the gate, or the broker. single build dir per app/ cargo.toml or whatever. never another, hard block.” Referenced source session was not resolvable by Pulse said, so original transcript itself was read. Leggy independently verified it. Latest peer report (Pulse 9f71322d-d473-40b0-95cc-812fd666760a) says parent commit 7c4e4ba58b is pushed with user approval in that chat, synced & checked on Mac/Dell. This push was not independently rechecked here. Generated nested AGENTS files remain uncommitted & owner-managed.

## 5. Artifacts & Evidence

Canonical evidence root `/Volumes/D/scratch/rk-hr-performance-sweep/`; Dell mirror `D:\scratch\rk-hr-performance-sweep`.

| Artifact | Path / URL | Role | State | Version / hash | Validation + last checked |
| --- | --- | --- | --- | --- | --- |
| Ten-minute accounting | /Volumes/D/scratch/rk-hr-performance-sweep/accounting-10min-20261104.md | 26 intervals, nap16:57:58IST to request21:11:37IST;4h13m39s | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| Requirement accounting | /Volumes/D/scratch/rk-hr-performance-sweep/accounting-requirements-20261104.md | Partial/notdone, no invented percent | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| Current evidence ledger | /Volumes/D/scratch/rk-hr-performance-sweep/inventory/SHARED-PIPELINE-EVIDENCE.md | Source/cohort caveats & raw paths | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| Original synthesis | /Volumes/D/scratch/rightkit-gate-consolidated-2026-11-04.md | Historical scope/issues; final no-edits statement superseded | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| Historical transcript inventory | /Volumes/D/scratch/gate-history-2026-11-04/report.md | Inventory limits; families.md/process-evidence.md/evidence.md companions | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| History dispositions | /Volumes/D/scratch/gate-history-2026-11-04/final-disposition-2026-11-04.md | Mixed freshness; dev-hold section superseded by this packet | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| A cross-call proof | /Volumes/D/scratch/rk-hr-performance-sweep/runner/candidate-a-20261104T150411.765478Z | 31.145s833hits | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| Mac A factor + resume | /Volumes/D/scratch/rk-hr-performance-sweep/runner/candidate-a-20261104T151903.634310Z & candidate-a-20261104T152509.875455Z | Exclude contaminated originalj2r2; preserve followupcleanup | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| Package45 tests | /Volumes/D/scratch/rk-hr-performance-sweep/runner/candidate-rk-compile-20261104T150737.176227Z | Real two-pnpm adapter route | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| Initial union | /Volumes/D/scratch/rk-hr-performance-sweep/runner/candidate-rk-units-20261104T151124.104845Z | 152.525s; raw signed receipts | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| Matched batch pair | /Volumes/D/scratch/rk-hr-performance-sweep/runner/candidate-rk-units-20261104T153357.051915Z | 151.563/168.712s | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| Dell interrupted wrapper | D:\scratch\rk-hr-performance-sweep\dell-rk-matrix-a-20261104T152858Z | Not fullgatepass; scopedAonly | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| Dell replacement + pause | D:\scratch\rk-hr-performance-sweep\dell-rk-matrix-job-20261104T153817.852963Z\pause-cleanup.json | Owneddriver/gategone, owner marker retained | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| Duplicate obligations | /Volumes/D/scratch/rk-hr-performance-sweep/union-dedup-profile-audit.md | Four extra grouped check/clippy requests + two extra tests; profile-hold advice superseded | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| Witness brief | /Volumes/D/scratch/rk-hr-performance-sweep/class-a-witness-worker-implementation.md | Design only; latest wire decisions accepted in Broker b3afd39d/fe1680d8 | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| CI draft | /Volumes/D/scratch/rk-hr-performance-sweep/ci-activation-checklist.md | Disabled; no hosted proof | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |
| Daily readiness | /Volumes/D/scratch/legion-daily/READINESS.md | Owner source work; not installed/scheduled | Historical evidence; see role | NOT_APPLICABLE: individual run identities in linked artifacts | Reviewed for accounting 2026-11-04; runtime freshness not asserted |

Issues cited historically: RightKit#1505 central gate repair; #1328/#1458 scan/selection/batching; #1502 acceptance ledger; RustRight#226/#227 pool/queue; Legion#138 daily probe. Additional historical mapping covers RightKit#1346/#1355 sampleapp, #1503 pnpmadapter, #1418 nestedbinding, #1504 snapshotidentity, #1259 commitselection, #1310 Linuxrouting, #1229/#1332 Swift, #1217 provisioning, #1299 tiers, #1429/#1340 updates, #687 ownershipcache, #1436/#1437 QA & RustRight#130 sccache/#219 HRadoption. Current issue open/closed status was not refreshed for this handoff; source/history links are leads, not current acceptance.


## 6. Failures, Dead Ends & Attempts

| ID | Attempt / command | Exact symptom / result | Cause / diagnosis | Evidence | DO_NOT_RETRY_UNLESS | Replacement / next diagnostic |
| --- | --- | --- | --- | --- | --- | --- |
| FAILURE-01 | HR compile | dictation-core0.2.1 calls removed fs API; QA needs port | dictation-core0.2.1 calls removed fs API; QA needs port | /Volumes/D/scratch/rk-hr-performance-sweep/inventory/SHARED-PIPELINE-EVIDENCE.md | Owner supplies real readyrevision & dependencyavailability | Keep recorded failure; continue independent fixture |
| FAILURE-02 | Dell old A wrapper | CIM timeout killedhealthyroot before300s | CIM timeout killedhealthyroot before300s | /Volumes/D/scratch/rk-hr-performance-sweep/inventory/SHARED-PIPELINE-EVIDENCE.md | Oldwrapper retired; use reviewedJobdriver, ownedcleanupverified | Recheck staleowner/lease throughcanonicalgate, neverunlinkblindly |
| FAILURE-03 | Mac cleanup probes | Lease query10s timeout despitegatepass | Lease query10s timeout despitegatepass | /Volumes/D/scratch/rk-hr-performance-sweep/inventory/SHARED-PIPELINE-EVIDENCE.md | Separate ownedprocess+lease+ownerproof established | Retain originalhalt, add followup; don't call first run clean |
| FAILURE-04 | Dell online seed | ERR_PNPM_PACKAGE_MANAGER_PRUNE_DIRECT_DEPS_READ_MANIFEST / OS448 stale jest-dom symlink | ERR_PNPM_PACKAGE_MANAGER_PRUNE_DIRECT_DEPS_READ_MANIFEST / OS448 stale jest-dom symlink | /Volumes/D/scratch/rk-hr-performance-sweep/inventory/SHARED-PIPELINE-EVIDENCE.md | Need genuinely missing preparation inputs, not just oldfailure | Offlinegate laterpassed; do notdeletecaches orrepeatseedneedlessly |
| FAILURE-05 | Syntax-cache comparisons | Differentlane values create differentkeys; firstobservedlane misses | Differentlane values create differentkeys; firstobservedlane misses | /Volumes/D/scratch/rk-hr-performance-sweep/inventory/SHARED-PIPELINE-EVIDENCE.md | Same actualinput/key/cache state for comparison | Separateinitialexposure fromwarmrepeats; no cacheclear |
| FAILURE-06 | Duplicate scans | Similar flags but differing env/testvalidators | Similar flags but differing env/testvalidators | /Volumes/D/scratch/rk-hr-performance-sweep/inventory/SHARED-PIPELINE-EVIDENCE.md | Proven fullsemantic/input/parser/coverage equivalence | Keepboth ifunproven; don'tdropdoc/tests blindly |
| FAILURE-07 | CI fixture | Proof/export/claim draft disabled | Proof/export/claim draft disabled | /Volumes/D/scratch/rk-hr-performance-sweep/inventory/SHARED-PIPELINE-EVIDENCE.md | Immutableclosure/workflowpins, realadmission & publish-free route verified | No push/dispatch merely to findout |
| FAILURE-08 | Width sweep | -j ignored underinheritedpool | -j ignored underinheritedpool | /Volumes/D/scratch/rk-hr-performance-sweep/inventory/SHARED-PIPELINE-EVIDENCE.md | Real broker-supported distinctwidth observable | Markunsupported, measureactualpool/contention instead |


## 7. Learnings, Gotchas & Landmines

| Signal | Hidden trap / learning | Required safe behavior | Source |
|---|---|---|---|
| Old dev-profile instruction | Generated wording contradicted human release-only order | Use release only; never reopen resolved profile question | Original human transcript cited in section 4 |
| Report says no edits | It predates 11 local candidate commits | Treat candidate commit table as current source baseline | Section 3, commit 8c6dd59e6 |
| Whole gate duration | Includes A, B, preparation & other work | Label stage & whole-run timing separately | Matched pair artifact in section 5 |

One complete A per identical valid snapshot/scope is intended; rerun when actual required input, policy/tool, expiry or later-stage check obligations change. Do not manufacture a blanket never-repeat rule. Cargo can parallelize its graph, but broker jobserver, implicit-root reservation, feature/profile/target and memory constraints govern real concurrency. Two different control names are not two different experiments unless actual behavior changes.

## 8. Open Loops & Context Gaps

| Gap | Severity | Impact | Recovery | Safe subset | Owner |
|---|---|---|---|---|---|
| Current external broker/gate state after clearedgoal | HIGH | Oldhashes maynotmatch | Re-read actualprocess/config/selectedentry on bothhosts | Read-onlyaccounting | Successor+Broker |
| Fullvalidmatrix denominator | HIGH | Cannotclaimexhaustivecoverage orpercentage | Freeze explicitfiniteeffectivecells beforemoreexperiments | Reuse existingvalidresults | Successor |
| ClassA witness implementation | HIGH | Useradmissioninvariant unmet | Implementbothendsagainstfixedwire &testrealhosts | Source/design review | RightKit+Broker |
| HRdependencies/productport | HIGH | FullHRcompileblocked | Ownerprovidesreadycommit; don'tpublishfromoptimizationlane | A/historyreview | hrmac/RightKitproductowner |
| CI/daily qualification | HIGH | Requestedstagesundelivered | Existingapprovedroutes &signing/pins; oneboundedprobeeachwhenready | Read-onlyreadiness | Relevantowners |
| Historicalcoverage | MEDIUM | Notalltranscripts/issuesreadendtoend | Usefamilyindex&originalsourceforspecificopenitem | Source-groundedboundedreview | Successor |
| Benchmarkbaselinecomparability | MEDIUM | Commits/runtimes/cache changed | Comparematchedcells only | Individualobservations | Successor |

## 9. Safety, Authority & Boundaries

- **May do:** review this packet, verify bytes/state, produce readback. User asked handoff, not resumed execution.
- **Do not change:** unrelateddirtyproductfiles, parentgitlinks, fixtureinputs while running, runtimepinsmid-cohort.
- **Do not run:** optimizationmatrices, compiles/builds/CI/install/publish from this packet alone. Existing prior benchmark authorization was used then goal paused/cleared; obtain fresh direction in successor chat through user's words, not agentassertion.
- **Irreversible / production actions:** no productpublication orunapprovedinstall/push-triggeredCI; maintainnative signing &existingcredentialrules.
- **Spend / external effects:** no newpaidservices ormessages required. Coordinateonlywhenusercontinuationrequiresit.
- **Secrets handling:** neverprintcredentials/environmentvalues; Ddevelopment/scratchonly,Eofflimits.
- **Reserved decisions:** usercontrolsresumption &productionactions. Do notaskuser torepeatresolvedreleasepolicy orapprovedscope.

## 10. Exact Resume Sequence

This is a proposed finish sequence, not an active job or ETA. It explicitly removes supporting-tool expansion.

1. **15-minute readiness/coverage pass.** Reuse existing evidence, freeze one runtime/commit perhost & enumerate exact missing effectivecells. Produce one table: runnable/blocked/unsupported/alreadymeasured. If unavailabledata, nameit; no newframework, no runtimeupgrade justforneatness.
2. **Complete one missing livecell perreadyfixture/host beforefurtherdesign.** StartwithpausedDellAremainingcells & Pulseexistingcompileentry once, providedactualreadiness. EachA≤300s, compile≤900s, cleanup≤60s. Preserveonewarmtarget. Ifmonitorbreaks, recordlaneaborted; no thirdrunnerrewrite. Useexistinggate logs/receipts/nativecancellation. Otherhostcontinues.
3. **Freezeboundedcomparisonblocks.** ExistingA1/2/4/8 andgenericbatchon/off arevalidsampledlevels; adddepth onlyifactualqueueplanchanges. Keepcachedsame-inputrepeats distinct fromlegitimatechanged-source/dependencytransitions. Mark-jineffective; nofictionalwidthsweep. Three observationsperselectedcellwhere neededtoassessspread, not endlessbest-ofreruns. Capblock30minperhost; recordcensored/unrun cells.
4. **Fix largestdemonstratedcost once.** Duplicatecheck/clippyobligations arecandidate, notyetprovenremovable. Canonicalizeonlywithfullenv/cwd/toolchain/target/feature/lock/parser/coverageproof; leave differentdocs/tests aloneuntilproved. Onechange, onefocusedcorrectnesscheck, onematchedcomparison. Retain ifcorrect&measurablybetter; otherwiserestoreonlyownedchange.
5. **Mandatoryinvariants work as separate boundedownerjobs.** Brokerwitnessprotocolfieldsetsagreed, butnoCLI/workerintegrationimplementedatstop. UseexistingfullApathonce, notparallelreplacement. Broker ownsadmission,gate ownsevidence, no second scheduler/keyservice. FirstintegrationresultmustrealA-before-admission positive/refusal onbothhosts. Ifcontractcan'texecute, stopclaimingitdone &showexactgap.
6. **Do notomitbuild/qualification/CI again.** Identifyexistingnonpublishingroute &attemptoneboundedbaselineperreadystage. CIrequiresverifieddispatch/pins/publishisolation, oneCIperrepo. Ifblocked, recordprerequisite/owner; don'tspendrestofsessioninventingprotocols whileotherreadycellswait.
7. **DailyLegionlast-mile &finalaccounting.** Reuseoneprovenwarmmeasurementcommand; silentunlessmeaningfulregression/failure, noadvisorybuildblock. Verifyactualscheduledrunonbothhosts. Deliverperstagebefore/after,rawspread,cachehits/misses,coverage/failures,remainingblockedcells &recommendedsettings. No universalminimumclaim.

### Resume Step 1 — verify packet receipt

- **Owner:** Successor
- **Working directory / system:** /Volumes/D/claude/rightkit
- **Exact action:**

```bash
legion script handoff/validate-handoff /Volumes/D/claude/rightkit/tasks/handoffs/2026-11-04/shared-pipeline-accountability.md --verify-receipt /Volumes/D/claude/rightkit/tasks/handoffs/2026-11-04/shared-pipeline-accountability.receipt.json
```
- **Expected result:** PASS; packet bytes match receipt.
- **Evidence path:** /Volumes/D/claude/rightkit/tasks/handoffs/2026-11-04/shared-pipeline-accountability.receipt.json
- **Timeout / retry:** 30 seconds; zero retries.
- **If failure:** Stop packet-based execution; identify hash or missing-file mismatch & report it.
- **Depends on:** None; immediately executable read-only check.

### Resume Step 2 — inspect source state

- **Owner:** Successor
- **Working directory / system:** /Volumes/D/claude/rightkit
- **Exact action:**

```bash
# Inspect source state without changing files.
git -C /Volumes/D/claude/rightkit status --short --untracked-files=no
git -C /Volumes/D/claude/rightkit rev-parse HEAD
git -C /Volumes/D/claude/rightkit log -12 --oneline
```
- **Expected result:** Candidate 8c6dd59 remains identifiable; later documentation commits distinguished; unrelated dirty files preserved.
- **Evidence path:** /Volumes/D/claude/rightkit/tasks/handoffs/2026-11-04/shared-pipeline-accountability.md
- **Timeout / retry:** 30 seconds; zero retries.
- **If failure:** Diagnose repository path/access read-only; do not reset or clean.
- **Depends on:** Step 1 receipt PASS.

### Resume Step 3 — read requirement accounting

- **Owner:** Successor
- **Working directory / system:** /Volumes/D/claude/rightkit
- **Exact action:**

```bash
# Read existing accounting records.
cat /Volumes/D/scratch/rk-hr-performance-sweep/accounting-requirements-20261104.md
cat /Volumes/D/scratch/rk-hr-performance-sweep/inventory/COVERAGE-LEDGER.md
```
- **Expected result:** Readback separates measured, missing, blocked & unsupported cells; no benchmark starts.
- **Evidence path:** /Volumes/D/scratch/rk-hr-performance-sweep/accounting-requirements-20261104.md
- **Timeout / retry:** 900 seconds total review; zero automatic retries.
- **If failure:** Report exact missing record & use available evidence; do not invent coverage.
- **Depends on:** Steps 1 & 2; actual benchmark resumption requires user direction.

Prepared but NOT run Pulse command, onlyafterresumption/readiness: `python3 /Volumes/D/scratch/rk-hr-performance-sweep/controls/pulse-compile.py /Volumes/D/scratch/rk-hr-performance-sweep/controls/pulse-compile.json`. Itguardsexactfixturebc4 &installedgate0.6.45,900s+cleanup. Staleguardfailure meansre-evaluateidentity, neverbypassguardblindly.

Dell replacementdriver `D:\scratch\rk-hr-performance-sweep\controls\dell-rk-matrix-job.py` SHA D561E4867FB4599A94B950114E5AC404BAF97FE858027AD23C7FDDE83E07736C usedWindowsJobObjects,suspendedassign-before-resume &heldprocesscreationtimebinding. Readresultsfirst; do notrerunallcompletedcellsautomatically. OldPowerShellmatrixrunnerthatkilledhealthyrootisretired.

## 11. State Verification & Invalidation

- **Verification command:**

```bash
# Inspect source state without changing files.
git -C /Volumes/D/claude/rightkit rev-parse HEAD
git -C /Volumes/D/claude/rightkit status --short --untracked-files=no
```
- **Expected state:** sourcecandidate8c6 identified; noownedbenchmarkactive; sourcechangesnotmistakenforinstalledruntime.
- **Invalidated by:** broker/gatepinchange, source/fixture/lock/toolchainchange, backgroundload, newownerwork, lateruserinstructions.
- **Refresh action:** boundedread-onlystatecapture; retainoldcohortseparately.
- **Validator command:**

```bash
legion script handoff/validate-handoff /Volumes/D/claude/rightkit/tasks/handoffs/2026-11-04/shared-pipeline-accountability.md --write-receipt /Volumes/D/claude/rightkit/tasks/handoffs/2026-11-04/shared-pipeline-accountability.receipt.json
```
- **Receiver receipt check:**

```bash
legion script handoff/validate-handoff /Volumes/D/claude/rightkit/tasks/handoffs/2026-11-04/shared-pipeline-accountability.md --verify-receipt /Volumes/D/claude/rightkit/tasks/handoffs/2026-11-04/shared-pipeline-accountability.receipt.json
```

## 12. First Output & Readback Contract

```text
READBACK
MISSION: Optimize shared pipeline; fixtures are RK, HR & Pulse.
CURRENT_STATE: Partial local fixes and measurements; goal cleared; no work resumes from this packet.
LOCKED_DECISIONS: Release-only, warm one target/workspace, Class A outside compile pool before broker admission, preserve required coverage.
SAFETY_BOUNDARIES: Preserve dirty product work; no cold/cache-clearing/blanket publication; no runtime changes mid-cohort.
NEXT_ACTION: Verify receipt and read-only state, then list exact missing cells.
CRITICAL_GAPS: Current peer runtime state, valid matrix denominator, Class A enforcement, CI/daily activation, HR readiness.
ASSUMPTIONS: Historical identities must be rechecked.
FIRST_VERIFICATION: git status/rev-parse + owned cleanup evidence.
PACKET_RECEIPT: verified sha256 from sidecar.
```

- **First deliverable after readback:** one completed/missing/blocked/unsupported matrix; no newframework orpromisedETA.
- **Gap report format:** `GAP: severity | missing | impact | recovery | safe subset | owner`.

## 13. Ready-to-Paste First Message

Read `/Volumes/D/claude/rightkit/tasks/handoffs/2026-11-04/shared-pipeline-accountability.md` & verify adjacentreceipt. ThisisreviewonlyuntilAdriandirectscontinuation. Preserveoriginalmission,correctedrelease-onlyauthority,allfailedattempts &dirtywork. Return READBACK, then follow Proceed mode READBACK_ONLY & provide bounded missing-cell accounting. Donotrepeatprofiledetour orrebuildmeasurementmachinery.

## 14. Context Gap Report

- **Gap summary:** HIGH runtime/coverage/admission/readinessgaps listedsection9; nofatalgapforreadbackonly.
- **Safe-to-proceed scope:** packetverification, read-onlystatecapture &accounting. Executionrequiresnewuserdirectionafterclearedgoal.
- **Fatal recovery owner:** NOT_APPLICABLE; noexecutionauthorizedbythishandoff.
- **Exact recovery sequence:** three read-onlyresumeactionsabove; request noalreadyresolvedpolicydecision.

## 15. Handoff Author Gate

- [x] Original user intent + active goal are exact, not paraphrased away.
- [x] Source evidence mode is explicit; Legion context path + prefix receipt are bound.
- [x] Live state was re-read from workspace/tools this turn.
- [x] Completed/in-progress/blocked/not-started states are separated.
- [x] Branch/version/dirty state or non-code equivalent is exact.
- [x] Active agents/processes/scheduled work were inventoried.
- [x] Decisions include rationale + reopen conditions.
- [x] Artifacts include locations + validation freshness.
- [x] Failures include raw evidence + DO_NOT_RETRY_UNLESS.
- [x] User corrections, learnings, gotchas, naming locks, & do-not-touch zones are captured.
- [x] No secret values appear.
- [x] Every gap has severity, impact, recovery, safe subset, & owner.
- [x] Readiness matches gap severities.
- [x] Resume Step 1 is immediately executable.
- [x] State verification + invalidation rules are exact.
- [x] Readback detects mission/state/boundary/next-action mismatch.
- [x] Cold-chat simulation passed with no unseen-context dependency.
- [x] `legion script handoff/validate-handoff` PASS receipt matches exact packet bytes.
