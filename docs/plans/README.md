# RightKit plans and reviews index

Dated 2026-10-08. Index only. No file has moved. Paths are relative to `/Volumes/D/claude`.

Status values: **active** (governs current work), **superseded** (replaced; do not execute), **historical** (finished, rejected, or kept as evidence). Start with the active rows. The current state of each plan lives in `tools/rightkit/STATUS.md`.

## Active

| Path | Date | Purpose | Status |
|---|---|---|---|
| `tasks/reviews/2026-10-08-rightkit-converged-plan.md` | 2026-10-08 | Converged review plan, rev 4: phases P0 to P4 for ownership prevention, broker admission, publication, enforcement, cleanup | active |
| `tasks/reviews/2026-10-08-rightkit-catalog-revalidation.json` | 2026-10-08 | Revalidated ownership catalog changes (tiers, fingerprints, detection) backing the P0 catalog commit | active |
| `tools/rightkit/docs/plans/2026-10-08-content-pipeline-execution-plan.md` | 2026-10-08 | Content-pipeline packages RK0 to RK10: scope, owners, machines, waves, status table | active |
| `tools/rightkit/docs/plans/2026-10-08-content-pipeline-requests.md` | 2026-10-08 | Requests from GenRight, CutRight and PostRight that gate RK0 to RK10 | active |
| `docs/plans/2026-10-08-demo-recording-requirements.md` | 2026-10-08 | Source of truth for demo recording requirements R1 to R17 and the architecture decisions | active |
| `docs/plans/2026-10-08-donor-ledger.md` | 2026-10-08 | Internal ledger of the workflows borrowed by the content plans, mapped to their sources | active |
| `docs/plans/2026-08-15-rightkit-architecture.md` | 2026-08-15 | RightKit build and release architecture, "Locked Thin Path" (agent lock: user decision) | active |
| `docs/plans/2026-10-05-rightkit-build-capacity-and-dev-parity.md` | 2026-10-05 | Build capacity, cache durability, dev/release parity. Step 1 approved and in implementation; steps 2 to 5 proposed | active |
| `tools/rightkit/docs/plans/2026-10-08-rk2-spike-b-results.md` | 2026-10-08 | RK2 Spike B results: Windows.Graphics.Capture on off-screen WebView2 windows (decision `3451a1f9`) | active |
| `tasks/handoffs/2026-10-08-rustright-handoff.md` | 2026-10-08 | Handoff from the Mac lead to RustRight: broker ownership, live state, open broker work (not a plan, but it changes ownership) | active |

## Historical and reviews (inputs to the active plans)

| Path | Date | Purpose | Status |
|---|---|---|---|
| `tasks/reviews/2026-10-08-astra-rightkit-review.md` | 2026-10-08 | Astra (gpt-6-astra high) read-only review of 47 crates and 29 packages. Input to the converged plan | historical |
| `tasks/reviews/2026-10-08-fable-rightkit-review.md` | 2026-10-08 | Fable 5.1 independent review of every crate and package. Input to the converged plan | historical |
| `tasks/reviews/2026-10-08-astra-plan-signoff.md` | 2026-10-08 | Astra sign-off with changes on plan rev 3 | historical |
| `tasks/reviews/2026-10-08-astra-plan-confirm.md` | 2026-10-08 | Astra confirmation of the sign-off edits | historical |
| `tasks/reviews/2026-10-08-fable-plan-signoff.md` | 2026-10-08 | Fable sign-off with nine required amendments | historical |
| `tasks/reviews/2026-10-08-fable-plan-confirm.md` | 2026-10-08 | Fable confirmation of plan rev 2 | historical |
| `tasks/reviews/2026-10-08-fable-plan-final.md` | 2026-10-08 | Fable final check of plan rev 3: confirmed | historical |
| `docs/plans/2026-10-08-astra-adversarial-review.md` | 2026-10-08 | Astra adversarial review of the content-pipeline plans: not executable as ordered | historical |
| `docs/plans/2026-10-08-fable-adversarial-review.md` | 2026-10-08 | Fable adversarial review, round two, of the content-pipeline plans | historical |
| `docs/plans/2026-10-08-astra-review-rec.md` | 2026-10-08 | Astra review of the demo architecture: blockers on qualification gates and composition primitives | historical |
| `docs/plans/2026-10-08-astra-review-cut.md` | 2026-10-08 | Astra review of the CutRight plan (app-side input to RK9 and RK0) | historical |
| `docs/plans/2026-10-08-astra-review-gen.md` | 2026-10-08 | Astra review of the GenRight plan (app-side input) | historical |
| `docs/plans/2026-10-08-astra-review-post.md` | 2026-10-08 | Astra review of the PostRight plan (app-side input; flags the HTTP client gap for RK10) | historical |
| `docs/plans/2026-10-08-postright-execution-plan.md` | 2026-10-08 | Pointer only. The canonical plan moved to `postright/docs/plans/2026-10-08-postright-execution-plan.md` | historical (pointer) |

## Superseded

| Path | Date | Purpose | Status |
|---|---|---|---|
| `tools/rightkit/docs/plans/2026-08-12-machine-wide-cargo-cache-v3.md` | 2026-08-12 | Machine-wide Cargo cache v3. Its header says superseded by v4 and not to execute | superseded |
| `tools/rightkit/docs/plans/2026-08-12-rightkit-cargo-control-plane-v4.md` | 2026-08-12 | Cargo control plane v4. Header says superseded by v5 and not to execute | superseded |
| `docs/plans/2026-07-13-rightkit-shared-sdk-architecture.md` | 2026-07-13 | Shared SDK architecture and migration plan. Distribution section superseded 2026-07-15 (private git package, not a registry) | superseded (distribution section) |

## Historical

| Path | Date | Purpose | Status |
|---|---|---|---|
| `tools/rightkit/docs/plans/2026-08-12-rightkit-build-control-plane-v5.md` | 2026-08-12 | Build control plane v5. Frozen for Phase 0; the completed implementation is v2.8 | historical |
| `tools/rightkit/docs/plans/2026-08-13-rightkit-managed-agent-build-control-v1.md` | 2026-08-13 | Managed-agent build control v1, frozen for implementation rev 4 | historical |
| `tools/rightkit/docs/plans/RIGHTKIT-THIN-BUILD-CONTROL-v2.8-IMPLEMENTATION.md` | 2026-08 | Thin build control v2.8. Status: complete, historical implementation evidence (macOS and Windows acceptance) | historical |
| `tools/rightkit/docs/plans/rightkit-thin-build-control-v2.7-measurement-remediation-protocol.md` | 2026-08 | Measurement and remediation protocol. Explicitly not a successor architecture | historical |
| `docs/plans/2026-07-21-rightkit-shared-build-cache-v2.md` | 2026-07-21 | Shared build cache v2. Header says "Proposed revision 2"; not confirmed as approved | historical (proposal) |
| `docs/plans/2026-10-07-rightkit-wave2-devin-handoff.md` | 2026-10-07 | Wave 2 handoff to one Devin chat, with lanes per crate or package. Baseline `dabb314f` | historical |
| `docs/plans/rightkit-ax/DESIGN.md` | 2026-07 | Design for `rightkit-ax` / `right-ax`. Status: implemented, staged as `@rightkit/ax@0.2.0` | historical |
| `docs/plans/rightkit-git/DESIGN.md` | 2026-07 | Design for `@rightkit/git`. Status: implemented, published as `@rightkit/git@0.2.5` | historical |

## Adjacent (not RightKit-owned)

| Path | Date | Purpose | Status |
|---|---|---|---|
| `docs/plans/2026-08-08-global-cargo-enforcement.md` | 2026-08-08 | Global Cargo test-ban enforcement across agents. Shares the heavy-command slot with the `rightkit cargo` path. Current status not verified | historical (not verified) |

## Not indexed here

- `tools/rightkit/docs/plans/covenant/`, `evidence/` and the `rightkit-build-control-v2-dispatch*.json` files are receipts and folders, not plans.
- `tools/rightkit/docs/OWNERSHIP-*.md` and `RIGHTKIT-*.md` are reference docs, not plans.
- `docs/plans/cutright/`, `genright`-related objectives and the `2026-10-06-*` app objectives belong to the app plans.
