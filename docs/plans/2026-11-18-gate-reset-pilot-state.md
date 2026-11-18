# Dell Class A pilot state

This file tracks the current work order under [reset steps 4–8](2026-11-07-gate-broker-reset.md) & the [acceptance design](2026-11-18-gate-acceptance-design.md). Codex writes this index. Fable owns the inventory & its evidence verdict. Muse owns the expected outcomes & its verdict. Owners supply their own artifacts. A successor takes the same role only after its chat ID & the artifact hash it received are recorded here.

| Role | Chat ID | State |
|---|---|---|
| Production trace, reviewer | Fable `8489b2b5-8949-4424-ad7b-6bbcd35db00e` | 5a passed with three file-count corrections. |
| Fixture author, reviewer | Muse `01a1ee3e-b286-73d0-b026-39f00bbe25ae` | 5b v1 received. Muse alone revises the fixture. |
| First implementation, state keeper | Codex `01a1ee3e-f505-7850-8627-8de6e657aae7` | Owns one Class A stage. Later implementation & step 8 leadership depend on this pilot. |

## Evidence received

- [5a inventory](/D:/scratch/loop-review/inventory/5a-classA-S2.md), reviewed SHA-256 `5463eb8e8c27a18a577c740b045dd03b2c2358fd17e9cc19c6e4ae726b55775e`. All 46 Class A IDs reconcile in both warm reports. Fable applied Muse's three count corrections. Verified corrected SHA-256 `2897109153a78d888a7dac578a57c51beb8866a0109f62af7918f6df9b450c7d`. Step 5a is closed for this fixture.
- [Muse verdict](/D:/scratch/loop-review/inventory/5a-muse-verdict.txt), received through Pulse as `d8730c5e-30fe-49f4-a044-7f6b7f9e397b`. The reverse exact-ID send failed. This file is the coordination fallback.
- [5b v1](/D:/scratch/loop-review/inventory/5b-classA-S2-expected.md), SHA-256 `76ed817d30b18e78371a252f9863a6b994bdc0b7eaedd15578eb0a85031c5ebe`. Muse froze it before implementation after reviewing 5a. It provides independent authorship. The two existing artifacts cannot establish blind derivation.

## Before 5c

1. Preserve both reviewed revisions. Fable records the corrected hash. Muse supplies a concrete paired control where the hit total stays correct, two identified rows swap dispositions, the report names both & stage dispatch stays zero. Give the corresponding matching case. Mark any unresolved control as a blocker for its own case.
2. Reconcile warm reuse with the six planner metadata commands that repeat cached row work. Naming a command in the forecast accounts for its start. It does not establish that the work was reused. For multi-file fixture edits, specify exact dependent rows & collateral invalidations from the fixture graph.
3. Use one held-out case on fixture commit `34788a913ffc` for blind derivation. Append a single trailing JavaScript comment to the tracked `app/packages/witness-notest/index.js`; keep toolchain, day, environment, dependencies & the rest of the tree fixed. This is a paper fixture, requiring no change to the checkout. Fable freezes its production prediction; Muse independently freezes its expected rows from the fixture graph & contract. Each retains its artifact privately until both hashes are recorded. Then compare, record differences & freeze the implementation input. Preserve the existing S2 results as prior evidence.

## Verdict & scope

The four behaviors are warm reuse with zero corresponding starts, expected changed rows executing, unexpected misses stopping before dispatch with row identity & diagnosis, & stale hits stopping even when aggregate counts match. Each needs a passing case & a control that must fail. Codex proposes done. Fable & Muse each give one verdict against the same revision. A block on an owned case fails the work order. Record unrelated findings separately. Escalate an actual disagreement once with both evidence files.

Dell step 4 remains open; checkout work proceeds independently. Admission changes await Adrian's recorded floor decision. Step 6 waits for a named RustRight owner writing its own verdict. Mac steps 4 & 8 remain untouched until a Mac chat is named. Compile, gate tests, build, CI, install & publication retain their approval requirements.
