# Decisions

## 2026-10-28 14:34 IST — Consolidate gate requirements

- **Authorization:** Adrian said, “alright, go ahead with the consolidation please”.
- **Issue:** [rightkit#1254](https://github.com/bogusyogi/rightkit/issues/1254).
- **Decision:** `docs/GATE-CONTRACT.md` consolidates the three target requirements, nine-cell matrix, boundaries, acceptance evidence, & proposed phases pending Fable signoff. `docs/PIPELINE-ARCHITECTURE.md` identifies it as target state; existing architecture & tests remain current evidence until reviewed & reconciled.
- **Scope:** documentation only. No runtime success is claimed. All execution evidence remains NOT RUN. This consolidation does not authorize implementation, installation, delivery, or publication.

## 2026-10-28 — Correct gate-contract status after review

- **Source:** Fable review of `docs/GATE-CONTRACT.md` at local checkout HEAD `03c1111f`.
- **Decision:** Preserve current admission/authentication guards pending reviewed direct-origin trust design; qualify sealed origin output unchanged when it provides the required target, adding a native build only where origin cannot produce that target; keep private Linux qualification OPEN until a compatible Mac/Dell-hosted Linux environment is identified. Mark contract as target-state pending signoff; do not promote proposal text to current behavior.
- **Evidence:** [rightkit#1254](https://github.com/bogusyogi/rightkit/issues/1254), [rightkit#1241](https://github.com/bogusyogi/rightkit/issues/1241), [RustRight #174](https://github.com/bogusyogi/rustright/issues/174), [Pulse #492 acceptance comment](https://github.com/bogusyogi/pulse/issues/492#issuecomment-6466661659).
- **Scope:** Documentation correction only. All execution remains NOT RUN; this entry grants no additional implementation or publication authority.

## 2026-10-28 — Record execution hold & Linux-route reconciliation

- **Human hold:** Adrian said, “we will get to execution once fable signs off on the contract.” Fable reviews this contract; Fable is not production/publication authorizer. Sage/Astra adjudicate technical design, Oracle independently reviews, & Adrian retains reserved production, publication, & trust-policy decisions. No new approval is added for already-authorized routine work.
- **Requirement/source reconciliation:** The explicit user requirement says Linux has no broker. [rightkit#1253](https://github.com/bogusyogi/rightkit/issues/1253) contains an implementation proposal for Linux broker/config/pin; [comment 6467016714](https://github.com/bogusyogi/rightkit/issues/1253#issuecomment-6467016714) records the requirement. The proposal does not create a WSL broker exception or a new Adrian decision; implementation/closure is unverified.
- **Ownership:** [rightkit#1241](https://github.com/bogusyogi/rightkit/issues/1241) tracks the attested npm receipt gap; [RustRight #174](https://github.com/bogusyogi/rustright/issues/174) owns adapter argv/env/resource/receipt/native-proof work. RustRight #173 is broker contract; #91 is migration.
- **Scope:** Documentation only; execution remains NOT RUN.
