VERDICT: PUBLISH — no qualifying blocker established; placement improves, while remaining protection risks are disclosed ([CHANGELOG:58](/Volumes/D/claude/rightkit/crates/rightkit-pdf/CHANGELOG.md:58)).

B = published `ca41ba38`; its crate source matches `fdad2f20`, used for baseline citations below. C = `3c917803`, matching working tree. Paths below are relative to `crates/rightkit-pdf`.

| Earlier finding | Status; N/P/I | Candidate evidence |
|---|---|---|
| 1. Unicode exhaustion | Closed; — | `src/inspect.rs:765` |
| 2. Symlink commits | Closed; — | `src/commit.rs:499` |
| 3. Windows hard-link identity | Closed; — | `src/commit.rs:279` |
| 4. Generated graphics state | Closed; — | `src/compose.rs:1594` |
| 5. Incomplete locators | Closed; — | `src/compose.rs:376` |
| 6. Race-test rendezvous | Closed; — | `src/commit.rs:650` |
| 7. File protection | Still open; I/P | `src/commit.rs:168`; `README.md:89` |
| 8. Sibling tokens | Closed; — | `src/commit.rs:527` |
| 9. Neighbor movement | Reviewed fixture closed; — | `src/compose.rs:4299`; corpus qualification remains limited at `README.md:103` |
| 10. Repeated streams | Closed; — | `src/inspect.rs:227` |
| 11. Pre-budget allocation | Closed; — | `src/inspect.rs:750` |
| 12. Batch invalidation | Still open; I/P | `src/compose.rs:359,411` |
| 13. Replacement loss | Closed by removal; — | `src/commit.rs:157,1014` |
| 14. Token undercount | Closed; — | `src/inspect.rs:253` |
| 15. Inherited-resource amplification | Closed by sharing; — | `src/merge.rs:87` |
| 16. Substitution advertisement | Reviewed operator cases closed; — | `src/compose.rs:4554` |
| Struct-literal documentation omission | Closed; — | `CHANGELOG.md:7` |
| Simple-font budget/extraction observations | Still open; I/P | `src/inspect.rs:1286`; `src/extract.rs:24` |
| Bare-writer coordination | Still open; P | `src/save.rs:522`; `README.md:77` |

**Blockers:** None established.

**Comparative notes**

- **Placement — I.** B emits upright bounds-anchored text ([B compose.rs:413](https://github.com/bogusyogi/rightkit/blob/fdad2f20/crates/rightkit-pdf/src/compose.rs#L413), `:1062`). C emits `Tm × CTM`, retaining requested text-space `Tf` (`src/inspect.rs:3250`; `src/compose.rs:1880,1900`). Row-vector multiplication at `src/model.rs:436` matches [ISO 32000-1 §§9.4.2–9.4.4](https://developer.adobe.com/document-services/docs/assets/35e4369068f86065372c18787171a17e/PDF_ISO_32000-1.pdf). Upright, rotated, skewed, mirrored & nonuniform transforms compose correctly algebraically; scale enters fit once (`src/compose.rs:1069`). Consumer: `compose_candidate` with changed appearance. Invariant: preserve source placement. Reset rise/scaling/spacing & extent-only bounds remain disclosed limitations (`CHANGELOG.md:5`).

- **Read-back — I.** B returns without geometry verification (`B src/compose.rs:213`); C identifies each replacement by unique installed stream, including runs grouped together (`src/compose.rs:247,1137,1639`). Multiple replacements & two on one line therefore do not collide by text or island identity. Empty replacement skips geometry verification because no glyph exists (`:1094`). Consumer/invariant: changed-font composition must preserve placement. Missing glyphs or failed page walks refuse safely; first-glyph checks cannot establish whole-run appearance (`:1146,1171`; `README.md:112`).

- **Page transforms — I.** B lacks geometry verification (`B src/compose.rs:213`); C’s walker starts with identity & returns rotation separately (`src/inspect.rs:368,425,2171`). Consequently `/Rotate` & `/UserUnit` apply outside both compared placements: neither is double-applied. However, origin tolerance is **0.5 default-user-space units**, not universally 0.5 physical points (`src/compose.rs:1179`; [ISO UserUnit definition](https://opensource.adobe.com/dc-acrobat-sdk-docs/standards/pdfstandards/pdf/PDF32000_2008.pdf?from=20423&from_column=20423)). Consumer: changed-font edit on `/UserUnit ≠ 1`; invariant: advertised physical tolerance. Rise remains another disclosed false-pass case (`README.md:112`).

- **Grouping — N, non-blocking.** B floors text-space em at 1 (`B src/inspect.rs:1326,1346`); C uses device-axis em (`src/inspect.rs:2844,2890`). Correctly joined downscaled quarter-turn text can now fragment because gap/baseline calculations still use page axes (`:2829`). Unscaled positive sizes ≥1 remain unchanged; sub-point text loses old floor, so “all unscaled pages unchanged” is false. Consumer: `page_graph_from_bytes`, then replacement using saved locators. Invariant: grouping/locator compatibility. Changed groups invalidate old digests; re-inspection is required (`:3207`; `CHANGELOG.md:6`).

- **Batches — I/P.** B re-resolves edited content (`B src/compose.rs:246`); C still does (`src/compose.rs:359`), despite reverse-address ordering & deferred installation (`:311,247`). Arbitrary N-command success is **not confirmed**. Ledger 208’s black-X/red-Y/black-Z case survives: removing Y can join X/Z, invalidating X. Shared-stream copying & interleaved operands also remain refusal cases (`:411`; `README.md:106`). Consumer: pristine locators passed together to `compose_candidate`; invariant: order-independent batch resolution. Smaller upright downscale windows reduce gap-driven joins; larger upscale windows increase them. Overall incidence is not established.

- **Earlier residuals.** Protection checks improve B’s ordinary temp creation (`B src/commit.rs:37` → C `:168`), but checked saves still risk disclosed ACL loss: confidentiality invariant, **I/P** (`CHANGELOG.md:58`). Simple-font inspection/extraction retain aggregate-budget gaps (**I/P**, B `src/inspect.rs:397`, `src/extract.rs:24` → C `:1286`, `:24`). `resolve_commit` plus bare `write_atomic` retains alias-conflict bypass (**P**, B `src/save.rs:417` → C `:522`); coordination boundary is disclosed (`README.md:77`).

- **Hostile input — N memory cost, non-blocking.** B discards each inspection graph (`B src/compose.rs:246`); C additionally retains one graph per replacement page & scans glyphs per replacement (`src/compose.rs:1139`). Consumer: large multi-page batch; invariant: bounded resource use. Per-page budgets remain (`src/inspect.rs:386`); no new panic or unbounded hang established.

**API/tests:** No remaining silent public API break found (`CHANGELOG.md:7,15,44,66,105,109`). Tests cover placement/rotation, joined replacements & both command orders (`src/compose.rs:4970,4987,5152,5199,5251`). No dedicated tests found for skew/mirror/nonuniform placement, `/UserUnit`, empty changed-font replacement, sub-point grouping, ledger-208 regrouping, or EFS/owner/group refusals promised at `README.md:88,104,112`.

**Not established:** external rendered-corpus results, physical crash behavior, dependency publication availability (`Cargo.toml:48`), or CI execution independently. Supplied CI is context only.

Read-only review complete; no builds, tests, crate execution or modifications performed.
