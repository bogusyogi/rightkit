VERDICT: PUBLISH — no qualifying blocker established: replacement-loss regression is removed, amplification fixes are retained, & remaining private-file exposure is disclosed. ([CHANGELOG:55](/Volumes/D/claude/rightkit/crates/rightkit-pdf/CHANGELOG.md:55))

Baseline **B** = `ca41ba38`; candidate **C** = `0f0395ee`. Candidate links below reference unchanged working-tree files. N/P/I applies to surviving defects; “—” means resolved.

| Earlier finding | Status / class | Candidate evidence |
|---|---|---|
| 1. Decoded Unicode exhaustion | Closed / — | Charge before clone: [inspect.rs:765](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:765) |
| 2. Symlink commits | Closed / — | Resolve, lock, recheck: [commit.rs:499](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:499) |
| 3. Windows hard-link identity | Closed / — | Handle-derived identity: [commit.rs:279](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:279) |
| 4. Generated-text graphics state | Closed / — | Isolation wrappers: [compose.rs:1484](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:1484) |
| 5. Incomplete locators | Closed / — | Exact span equality: [compose.rs:363](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:363) |
| 6. Race-test rendezvous | Closed / — | Snapshot precedes rendezvous; one winner: [commit.rs:650](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:650), [672](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:672) |
| 7. File protection | Still open / **I**, residual **P** | Protection checks: [commit.rs:182](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:182); limits: [README:89](/Volumes/D/claude/rightkit/crates/rightkit-pdf/README.md:89) |
| 8. Hard-link sibling tokens | Closed / — | Invalidation after publication: [commit.rs:527](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:527) |
| 9. Neighbor movement | Reviewed reproducer closed / — | Geometry regression: [compose.rs:4170](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:4170); corpus limit disclosed: [README:103](/Volumes/D/claude/rightkit/crates/rightkit-pdf/README.md:103) |
| 10. Repeated content references | Closed / — | Aggregate charging: [inspect.rs:227](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:227) |
| 11. Allocation before glyph limit | Closed / — | Check before reservation: [inspect.rs:750](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:750) |
| 12. Batch locator invalidation | Still open / **I** | Shared-page copies still invalidate later locators: [compose.rs:431](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:431) |
| 13. Replacement deletes both versions | Closed by removal / — | Delegated writer: [commit.rs:157](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:157); failure regression: [1014](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:1014) |
| 14. Token undercount | Closed / — | Byte charge before parsing: [inspect.rs:253](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:253); adjacent-token regression: [hostile_input.rs:760](/Volumes/D/claude/rightkit/crates/rightkit-pdf/tests/hostile_input.rs:760) |
| 15. Inherited-resource amplification | Closed by sharing / — | Cached indirect values: [merge.rs:87](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/merge.rs:87); regression: [hostile_input.rs:839](/Volumes/D/claude/rightkit/crates/rightkit-pdf/tests/hostile_input.rs:839) |
| 16. Substitution advertisement | Closed / — | Admission agreement across four operators: [compose.rs:4367](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:4367) |

**Blockers:** None established.

**Non-blocking notes**

- **#7 — I/P:** B creates ordinary temporary files, preserving no protection ([B commit.rs:37](https://github.com/bogusyogi/rightkit/blob/ca41ba38/crates/rightkit-pdf/src/commit.rs#L37)). C checks Unix ownership/mode & selected platform protections before writing ([C commit.rs:168](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:168)). Calling `commit_pdf_save` or `write_atomic` over Windows files with restrictive DACLs, or wider-mode macOS files with ACL restrictions, can still weaken confidentiality. Invariant: replacement must not broaden access. Residual exposure is explicitly disclosed ([CHANGELOG:55](/Volumes/D/claude/rightkit/crates/rightkit-pdf/CHANGELOG.md:55)).

- **#12 — I:** B rewrites shared stream objects directly ([B compose.rs:284](https://github.com/bogusyogi/rightkit/blob/ca41ba38/crates/rightkit-pdf/src/compose.rs#L284)); C protects other pages by copying their stream references ([C compose.rs:474](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:474)). `compose_candidate` with replacements targeting two pages sharing one stream can refuse its second locator. Invariant: batch locators remain valid while unrelated pages remain unchanged. Safe refusal is documented ([CHANGELOG:93](/Volumes/D/claude/rightkit/crates/rightkit-pdf/CHANGELOG.md:93)).

- **Earlier content-class observation — I/P:** B simple-font decoding has no aggregate font budget ([B inspect.rs:397](https://github.com/bogusyogi/rightkit/blob/ca41ba38/crates/rightkit-pdf/src/inspect.rs#L397)); C still bypasses `FontWork` there ([C inspect.rs:1286](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:1286)). `page_graph_from_bytes` on many simple fonts therefore lacks README’s promised aggregate CMap bound ([README:108](/Volumes/D/claude/rightkit/crates/rightkit-pdf/README.md:108)). Invariant: aggregate work stays within advertised limits. Optional extraction likewise delegates without page budgets in [B extract.rs:24](https://github.com/bogusyogi/rightkit/blob/ca41ba38/crates/rightkit-pdf/src/extract.rs#L24) & [C extract.rs:24](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/extract.rs:24). Qualifying irreversible harm or unbounded hang is not established.

- **Earlier writing-class observation — P:** composing `resolve_commit` with bare `write_atomic` still bypasses checked-save coordination: B uses string equality ([B save.rs:417](https://github.com/bogusyogi/rightkit/blob/ca41ba38/crates/rightkit-pdf/src/save.rs#L417)); C retains it ([C save.rs:522](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/save.rs:522)). Aliased or concurrent writes can bypass conflict protection. Invariant: stale candidates must not overwrite newer edits. This low-level boundary is disclosed ([README:77](/Volumes/D/claude/rightkit/crates/rightkit-pdf/README.md:77)).

**Silent API breaks & untested promises**

**N, non-blocking:** external `PdfTextIsland { … }` construction worked with B’s entirely public fields ([B model.rs:206](https://github.com/bogusyogi/rightkit/blob/ca41ba38/crates/rightkit-pdf/src/model.rs#L206)); C adds private `layout` & `run`, preventing construction ([C model.rs:336](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/model.rs:336)). Invariant: source compatibility. CHANGELOG describes additive fields but omits this consequence ([CHANGELOG:9](/Volumes/D/claude/rightkit/crates/rightkit-pdf/CHANGELOG.md:9)). Document construction break explicitly. Enum changes & `PositionedGlyph::pen_advance` are documented ([12](/Volumes/D/claude/rightkit/crates/rightkit-pdf/CHANGELOG.md:12), [41](/Volumes/D/claude/rightkit/crates/rightkit-pdf/CHANGELOG.md:41), [63](/Volumes/D/claude/rightkit/crates/rightkit-pdf/CHANGELOG.md:63)).

Tests cover editability, gaps, inherited resources, graphics state & split streams ([compose.rs:2784](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:2784), [3106](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:3106), [3652](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:3652), [3865](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:3865), [4565](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:4565)). No tests found exercising EFS or owner/group mismatch refusals promised at [README:88](/Volumes/D/claude/rightkit/crates/rightkit-pdf/README.md:88). Existing protection tests exercise mode, destination ACL & read-only cases ([commit.rs:1084](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:1084)). Font-flood coverage uses CID fonts only ([hostile_input.rs:717](/Volumes/D/claude/rightkit/crates/rightkit-pdf/tests/hostile_input.rs:717)).

**Not established:** registry availability of required `rightkit-fs` 0.3.4 ([Cargo.toml:48](/Volumes/D/claude/rightkit/crates/rightkit-pdf/Cargo.toml:48)), external corpus results, or physical crash/power-loss behavior. Supplied CI results were accepted as context.

Read-only review completed; no builds, tests, crate execution, or modifications performed.
