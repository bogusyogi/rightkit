# Dell Class A pilot state

This file tracks the current work order under [reset steps 4–8](2026-11-07-gate-broker-reset.md) & the [acceptance design](2026-11-18-gate-acceptance-design.md). Codex writes this index. Fable owns the inventory & its evidence verdict. Muse owns the expected outcomes & its verdict. Owners supply their own artifacts. A successor takes the same role only after its chat ID & the artifact hash it received are recorded here.

| Role | Chat ID | State |
|---|---|---|
| Production trace, reviewer | Fable `8489b2b5-8949-4424-ad7b-6bbcd35db00e` | 5a passed with three file-count corrections. |
| Fixture author, reviewer | Muse `01a1ee3e-b286-73d0-b026-39f00bbe25ae` | 5b v1 received. Muse alone revises the fixture. |
| First implementation, state keeper | Codex `01a1ee3e-f505-7850-8627-8de6e657aae7` | Owns one Class A stage. Later implementation & step 8 leadership depend on this pilot. |

## Evidence received

**Active assignment — 5c.** Astra (this Codex chat) owns implementation for one Class A stage. Gate code derives expected row IDs before cache lookup, allows predicted changes to run, & refuses any row mismatch before dispatch. Existing 5b fixtures supply expected answers for verification.

**Handoff.** Astra records one implementation commit & its evidence here. Fable then checks actual row executions & child starts against the ledger. Muse independently checks exact row identities, refusal diagnostics & zero dispatch against frozen 5b cases, including SC-1. Both review that same commit in parallel. Astra collects both verdicts. Adrian has no assignment or messages to relay.

- [5a inventory](/D:/scratch/loop-review/inventory/5a-classA-S2.md), reviewed SHA-256 `5463eb8e8c27a18a577c740b045dd03b2c2358fd17e9cc19c6e4ae726b55775e`. All 46 Class A IDs reconcile in both warm reports. Fable applied Muse's three count corrections. Verified corrected SHA-256 `2897109153a78d888a7dac578a57c51beb8866a0109f62af7918f6df9b450c7d`. Step 5a is closed for this fixture.
- [Muse verdict](/D:/scratch/loop-review/inventory/5a-muse-verdict.txt), received through Pulse as `d8730c5e-30fe-49f4-a044-7f6b7f9e397b`. The reverse exact-ID send failed. This file is the coordination fallback.
- [5b v1](/D:/scratch/loop-review/inventory/5b-classA-S2-expected.md), SHA-256 `76ed817d30b18e78371a252f9863a6b994bdc0b7eaedd15578eb0a85031c5ebe`. Muse froze it before implementation after reviewing 5a. It provides independent authorship. The two existing artifacts cannot establish blind derivation.

## Before 5c

Muse's held-out prediction SHA-256 is `9cf3ec3c24a581cb9738f6398799b96d29f2b15f996399da8fbf10d39e0ff0f2`. Muse reports deriving it from sealed baseline keys & fixture semantics before seeing Fable's held-out prediction. Its [warm metadata supplement](/D:/scratch/loop-review/inventory/5b-classA-S2-supplement.md) has SHA-256 `6a88dcb8703275b1dab597d3c97bfad9684acfd12f183942d76c72d6795991bf`; 5b v1 is unchanged.

Fable's held-out production prediction SHA-256 is `39b1dc51817caccb535e0e6f7b172457b62bb24276af51b2521948332441586d`. Fable reports it did not open Muse's prediction. Codex verified both file digests & recorded them here before opening either row list.

1. Preserve both reviewed revisions. Fable records the corrected hash. Muse supplies a concrete paired control where the hit total stays correct, two identified rows swap dispositions, the report names both & stage dispatch stays zero. Give the corresponding matching case. Mark any unresolved control as a blocker for its own case.
2. Reconcile warm reuse with the six planner metadata commands that repeat cached row work. Naming a command in the forecast accounts for its start. It does not establish that the work was reused. For multi-file fixture edits, specify exact dependent rows & collateral invalidations from the fixture graph.
3. Use one held-out case on fixture commit `34788a913ffc` for blind derivation. Append a single trailing JavaScript comment to the tracked `app/packages/witness-notest/index.js`; keep toolchain, day, environment, dependencies & the rest of the tree fixed. This is a paper fixture, requiring no change to the checkout. Fable freezes its production prediction; Muse independently freezes its expected rows from the fixture graph & contract. Each retains its artifact privately until both hashes are recorded. Then compare, record differences & freeze the implementation input. Preserve the existing S2 results as prior evidence.

## Verdict & scope

**5b reconciliation complete.** Commit `50a4f54f2` records both prediction hashes before comparison. The [Muse expectation](/D:/scratch/loop-review/inventory/5b-heldout-notest-muse-prediction.md) & [Fable production prediction](/D:/agent-cache/tmp/claude/ae528ff5a5b39e97/8489b2b5-8949-4424-ad7b-6bbcd35db00e/scratchpad/heldout-fable-production-prediction.md) agree on all 46 row dispositions for a previously unseen, inert trailing comment. Run `node-check:app/packages/witness-notest/index.js`, `oxlint:app-packages-witness-notest`, `tsc:app-packages-witness-notest` & `secret-scan`. Reuse the remaining 42. The secret scan executes in process, so three child starts can represent four row executions. Both authors used sealed baseline keys; the blind comparison establishes independence from each other's held-out answer.

**5c input frozen.** Use the recorded 5b v1, held-out expectation & supplement hashes. SC-1 substitutes `license` for `secret-scan` in the forecast. Lookup must report both mismatches, exit unsuccessfully & dispatch zero stage rows despite equal totals. Fable predicts the sealed candidate will also launch six unnecessary planner metadata commands. The acceptance supplement requires zero such starts when those metadata rows should hit. This is a known production gap. The comparison verified forecast agreement only; no edited-fixture run, SC-1 refusal or production fix has been verified.

The four behaviors are warm reuse with zero corresponding starts, expected changed rows executing, unexpected misses stopping before dispatch with row identity & diagnosis, & stale hits stopping even when aggregate counts match. Each needs a passing case & a control that must fail. Codex proposes done. Fable & Muse each give one verdict against the same revision. A block on an owned case fails the work order. Record unrelated findings separately. Escalate an actual disagreement once with both evidence files.

Dell step 4 remains open; checkout work proceeds independently. Admission changes await Adrian's recorded floor decision. Step 6 waits for a named RustRight owner writing its own verdict. Mac steps 4 & 8 remain untouched until a Mac chat is named. Compile, gate tests, build, CI, install & publication retain their approval requirements.

## 5c checkout handoff

**Review revision `ca2f25821024538527331822da50741f74c0a91b` (local, unpushed).** Fable's request was sent through Pulse as `3495d19d-5bca-4d51-befe-3babfb3aec83`. Pulse could not resolve Muse's chat on reply to its original handoff. Muse's work order is recorded here: check the same revision against frozen 5b expectations, supplement & SC-1, & write `5c-muse-verdict.txt` beside the checkout replay. Use installed evidence only for installed behavior.

Astra implemented the snapshot Class A cache decision. Existing stores require matching independent history. A new store records first-use history before app checks create their verdict directory. History advances from verified retained rows after the end-of-stage binding check.

Five file-only checks passed for history & ShellCheck input closure. [Checkout replay](/D:/scratch/loop-review/inventory/5c-checkout-replay.json) compared both sealed 46-row warm reports using the new production comparison function. The injected missing license entry yielded one mismatch. The equal-count swap yielded two, license & secret-scan. All six fixture workspaces produced metadata topology fingerprints. This replay never called the dispatcher.

Fable owns installed execution evidence, including metadata starts & zero dispatch on refusal. Muse owns fixture coverage, including S1 vs a deliberately cold start. S11 currently moves an existing store aside at its original path; under the new rule that operation must produce a surprise-loss refusal. A true cold control needs a separately declared new store. The frozen S2b added `lib.sh` already enters the ShellCheck command directly; the new file-only regression additionally edits a transitively sourced `.inc` outside that command.

**Status: awaiting installed evidence.** Existing Dell coverage records only layer-one keys & cannot initialize full-key history for the old cache. Use a separately identified cold store for the first approved acceptance sequence. Preserve the independent history for every subsequent warm, edit & store-loss case. The Dell batch wrapper is absent at its workspace-rule path. No new installed gate receipt exists from this change.

**Fable verdict: NOT MET.** The [execution-evidence verdict](/D:/scratch/loop-review/inventory/5c-fable-verdict.txt) found no installed ledger for this revision. It requires warm reuse, held-out edit, equal-count swap & store-loss evidence. A subsequent [revision-bound file replay](/D:/scratch/loop-review/inventory/5c-revision-replay.json) records source & input hashes, [producer](/D:/scratch/loop-review/inventory/5c-replay.mjs), fixture commit, working-tree differences & [five check results](/D:/scratch/loop-review/inventory/5c-file-checks.txt). Installed dispatch evidence & Muse's verdict remain outstanding. The work order stays open.
