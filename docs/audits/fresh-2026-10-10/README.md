# Fresh RightKit audit — 10 October 2026

Three source-led reports, prepared independently of prior audits:

- [Dependencies & publication prerequisites](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/01-dependencies.md)
- [Package boundaries, duplication & decomposition](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/02-package-boundaries.md)
- [App adoption inventory & handoffs](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/03-app-adoption.md)

Coverage: 24 npm packages, 56 workspace crates, Swift products/helpers, secondary native/template manifests & 22 local checkout roots, plus archive/absent-product entries. Exact versions, dependency scopes, source evidence & per-app acceptance checks accompany reports.

Priority: resolve release/version evidence, validate narrow dependency removals, consolidate duplicated mechanisms with parity fixtures, then distribute app-specific adoption work.

Apple follow-up now applies both requested iOS & macOS skills to all 14 Swift products, helpers & consumers. macOS suite passed (201 cases, 3 skips); all-product iOS build passed; StoreKit simulator passed 8 tests; CloudKit helper built; recording helper passed 13 tests with 1 expected skip; license contract parity passed 2 tests. Legacy archive helper failed 2 tests. Swift source findings now have one prioritized closure checklist.

Frozen native Audit certification remains **INCOMPLETE / UNPROVEN**. Apple build/test evidence is established separately; live registry, signed app/device & cross-platform publication qualification remain distinct work.

Fresh evidence, rerunnable inventory scripts & source hashes are under `evidence/`. Existing product changes were preserved.

[Initial verification results](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/verification.json) — original inventory counts, citation checks & source-hash stability.

[Apple native evidence](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/apple-verification.md) · [Apple closure checklist](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/apple-closure.md) · [Updated report verification](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/apple-verification.json)

[Concurrent workspace delta](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/workspace-drift.json) — Pulse release pin advanced to `0.2.146`; original inventory remains baseline. RightKit product source was preserved.

## Merged from the earlier static pass

This set (B) is the base. It keeps its own structure, claims and evidence links. An earlier static pass over the same baseline (set A: dependencies, structure and app-adoption reports; it counted lines and read importers but built nothing) contributed the parts below, each marked "(from static pass, unverified by build)" in the text. Set A's three report files have been deleted.

**Evidence folders.** Set A's per-crate, per-package and per-app evidence stays at [2026-10-10-audit-evidence/](/Volumes/D/claude/rightkit/docs/audits/2026-10-10-audit-evidence/) (crates-g1..g4, npm, swift, apps-g1..g5, scanner JSON, `cargo-dups.txt`, `crate-weights.csv`). The plan comparison [2026-10-10-plan-vs-current.md](/Volumes/D/claude/rightkit/docs/audits/2026-10-10-plan-vs-current.md) is also kept.

**Folded into [01-dependencies.md](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/01-dependencies.md):**
- Transitive-weight table and headline placement counts (shell 248, daemon 159, agent-tools 128 and so on).
- Feature-gate table (18 crates whose dependency is used on one path or platform).
- Replace-with table, **except** the wholesale mcp/models `ureq` to `rightkit-http` rows, which are kept as "per-client review" because B showed the merge breaks `max_redirects(0)`, range/resume and the no-network build.
- Duplicate version families (39) and metadata drift.
- The undeclared `react-dom` peer of `@rightkit/app-shell`.
- RightKitSwift version truth.

**Folded into [02-package-boundaries.md](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/02-package-boundaries.md):**
- Duplicated-primitives table with sizes (atomic write incl. sync `storage.rs` without fsync, ONNX setup, subprocess runner, media probe, `TimedWord`, `CONTROL_METHODS`, and others), with a "B cross-check" column, plus the npm duplicates.
- The "release ownership group is separable" fact, with B's keep-one-package decision stated beside it.
- Retire `rightkit-qts-ggml` (B's inventory says Keep; open) and delete Pulse's `PulseMacPrototype`.

**Folded into [03-app-adoption.md](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/03-app-adoption.md):**
- Per-app summary table (duplicates, LOC, stale pins, blockers) for 16 repositories.
- Handover lists for all 16 (SellRight, RightSites, xtool and lightcraft expanded from `apps-g5.md`).
- Swift rows B lacked (RightKitSwift, Pulse mac), the Windows-platform and C ABI decisions, and the RightKit gaps table including the missing Rust C ABI.
- Five concrete blockers: VoiceRight `qts` path, WorkRight release-doctor ban, RightSites `r2-rightsites` provider, lightcraft workflow, SellRight Playwright.

**Corrections applied in the merged text:** Pulse already uses `rightkit-fsindex 0.1.0` (03:354-356), so set A's "hub scanner to fsindex" item is marked corrected and its LOC footnoted; CodeRight's `project.yml` also pins RightKitSwift `from: 0.1.0`; RightKitSwift version truth is tags 0.1.0, 0.1.1, 0.3.0 to 0.3.7, source 0.3.8 unpublished, feed stale at 0.1.1 / 0.3.1. Where set A and B give LOC for the same item, B's stands; none conflicted, and differences of counting basis (stale-pin lines vs distinct names, Pulse pin targets vs observed pins) are footnoted in 03.

**DISPUTED, Adrian decides** (set A cites importer and line evidence; B keeps product boundaries; details in 02):
1. `rightkit-automation` + `rightkit-control` (+ browser `policy.rs`): shared effects core to remove ~1,100 duplicate lines.
2. RightKitSwift `RightKitSpeech`: keep ~674 LOC shared, move ~3,241 LOC (Core ML Parakeet, Whisper) out.

**Not imported (refuted or declined by B):** splits of release, models, asr, qa and shell; the shell + tauri, qa + qa-chrome and playback to media merges; the npm type-only to dev moves; the fabric item.
