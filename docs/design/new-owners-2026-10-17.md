# Decision record: four new owners requested by apps (2026-10-17)

Adjudicated by Sage from the four issues (#302 for context), the workflow, llm, db and job-queue crates, `CAPABILITY-MAP.md`, `decisions.md`, and PostRight, GenRight and MailRight docs.

Gate: Adrian's admission rule (decisions.md, 2026-10-15) allows a capability into RightKit only with at least 2 consuming products. No builds now (code-all-then-one-build).

## #296 Resumable skill executor (G-1)

1. **Extend `rightkit-workflow`** (feature `sequence`). `rightkit-llm` must not gain a workflow dependency, and job-queue jobs are whole-job units. The machine already has the hard parts: journal fsynced before `Dispatched`, `IdempotencyKey(run, step)`, `Run::value()` for step output, restart into `Unresolved`. Missing is a generic linear run over non-GUI steps.
2. API (generic names): `Sequence { steps }`; `SequenceRun::open(path, run_id, clock)`; `run_next(driver, gate) -> Done(value) | Stopped(reason) | Unresolved`, one step per call so a restart at every boundary is normal; `trait Gate { before_perform(step, key) }` (the app's budget reservation goes here); `ReplayPolicy { Unresolved, ReissueOnce }` (O-1).
3. Deps: none new (blake3, serde, serde_json, getrandom, all permissive).
4. App keeps: manifests, prompts, route policy, tell rules, repair ceiling, the model call inside `perform`, the commit step's idempotency key, tool execution (G-3, separate), memory writes.
5. Adrian: no. Admission holds because the extension is generic and the crate has other consumers. Caveat: the journal is a file per run, not the app's SQLite, so "commit once" rests on the commit step's idempotency key.

## #299 Platform rules owner (RG-1)

1. **New crate `rightkit-platform-rules`.** `rightkit-contracts` validates schemas, not dated data. One ownership entry.
2. API: `Catalog::bundled()` and `merge(overrides)`; `get(platform, account_type, capability, format) -> Rule { rule_id, value, source_grade, source_url, fetched, revision, supersedes }`; `revision(platform) -> RuleRevision` (stable hash); `check(revision, planned) -> Result<(), Violation>`; `is_current(revision)`. The `sourced` promotion rule is enforced in code. A change is a new revision, never an edit.
3. Deps: serde, serde_json. Data is bundled JSON, like `rightkit-llm/assets/model-prices.json`.
4. App keeps: fetching and citing sources, hold-or-regenerate behaviour on a revision change, the revision field on its records. Adding `ruleRevision` to the brief and envelope (RG-2/G-7) is a separate `rightkit-contracts` issue.
5. Adrian: no (R-1 decided). Condition: name the second consumer (the CutRight export manifest, per PostRight's table) in the issue before merge.

## #301 Spend ledger (RK-C1)

1. **New crate `rightkit-spend`** on `rightkit-db`, not in `rightkit-llm`: scrapers, transcribers and fees spend too. Donor: PostRight `crates/engine/src/spend.rs`. The issue says PostRight has no ledger, but SPEND-LEDGER.md says it is built; PostRight migrates its rows.
2. API: `Ledger::attach(conn)` (migrations via `rightkit-db`, so it can share the app's file); `set_allowance(scope, period, limit_micro, expected_revision)`; `reserve(scope, request_id, period, route_id, price_version, estimate_micro)` (idempotent on `request_id`); `mark_in_flight`; `commit(actual_micro)`; `release`; `expire_stale`; `acknowledge_overrun`; `report`. Integer micro-units, checked arithmetic, CAS on allowance revision. An expired in-flight reservation commits at ceiling, never releases.
3. Deps: `rightkit-db` (rusqlite MIT, SQLite public domain), thiserror (MIT/Apache-2.0).
4. App keeps: allowance amounts (D1), scope mapping, estimate arithmetic, overrun acknowledgement, daemon single-writer. Price rows go to #302 in `rightkit-llm`; the ledger stores only the route and price version passed in, so qualifying the price is the caller's check before `reserve`.
5. Adrian: no for the crate; limits remain his D1. Admission relies on D10 and on GenRight, which already reserves against daily and monthly caps plus a per-device offline allowance (`paid_spend_allowance.rs`). So key allowances by an opaque period, not only month, and confirm GenRight in the issue before merge.

## #318 Shared zstd owner

1. **Do not create it yet.** One consumer (MailRight), conditional on probe G saving over 30%, fails the 2-consumer rule. MailRight's own plan says compression must use a crate RightKit owns or approves, so until then it ships the duplicate-copy removal only; its lead may instead ask for `zstd` to be approved for that one product.
2. Designed, not built: `rightkit-compress` with `compress(&[u8], level)`, `decompress(&[u8], max_out)` (output bound required, against decompression bombs), a one-byte format tag.
3. Deps when built: `zstd` (MIT) over libzstd (BSD-3-Clause, chosen from its BSD/GPLv2 dual licence; allowed by `deny.toml`; needs a C compiler, `cc` already in the lock). Fallback: `ruzstd` (MIT, pure Rust, weaker encoder).
4. App keeps: what to compress, the 30% decision, the stored format flag.
5. Adrian: no. Reopen when probe G passes and a second product names a use.
