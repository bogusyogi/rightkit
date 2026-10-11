1. **P1 — Decoded Unicode can exhaust memory.** [inspect.rs:494](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:494), [inspect.rs:1546](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:1546). Identity-H CMap mapping `<0001>` to 1 MiB of `A` characters fits inside encoding-stream limit. Showing CID 1 fifty thousand times clones over 48 GiB before island construction. Assignment budgets do not bound decoded output. **Smallest fix:** limit destination length & aggregate decoded bytes before cloning; return typed refusal on exhaustion.

2. **P1 — Symlink commits still lose concurrent edits.** [commit.rs:228](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:228), [commit.rs:237](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:237). Stage two tokens against `A.pdf`; commit both to symlink `B.pdf → A.pdf`. First locks canonical A but renames over B, replacing its symlink. A remains unchanged, so sibling token survives. Waiting commit now sees B as different from A, skips source checking & overwrites first candidate; both succeed. **Smallest fix:** resolve symlink destination once & use that resolved path consistently for locking, checking & replacement; reject destination identity changes after waiting.

3. **P1 — Windows hard links bypass conflict checks.** [commit.rs:88](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:88), [commit.rs:116](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:116). Stage A, create/use hard link B, externally rewrite A, then commit to B. Windows supplies no file identity here; distinct canonical paths compare unequal, skipping SHA verification & overwriting newer B bytes. Unix inode comparison catches this case. **Smallest fix:** obtain Windows volume/file identity from opened handles; fail closed when identity cannot be established.

4. **P1 — Generated text still inherits clipping/transparency.** [compose.rs:1572](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:1572), [compose.rs:1630](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:1630). Source ending `0 0 1 1 re W n`, followed by AddText at `(100,100)`, produces invisible text while structural readback succeeds. `/GS0 gs` with `/ca 0` similarly hides text & OCR cover. Resetting text parameters cannot restore clipping. **Smallest fix:** isolate original page content inside balanced graphics-state save/restore before appending; reject content whose state cannot safely be isolated.

5. **P1 — Incomplete public locators permit partial replacement.** [compose.rs:278](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:278), [compose.rs:337](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:337). Inspect `(AB) Tj (CD) Tj`, retain island digest but remove second locator span, then request changed-font replacement with “Hi”. Membership-only validation passes; rewrite removes AB, leaves CD & appends Hi. Expected-text verification accepts Hi. **Smallest fix:** require complete span equality & perform mutations using freshly inspected spans.

6. **P2 — Race regression can pass with locking removed.** [commit.rs:330](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:330). Rendezvous occurs before reading. After release, one thread can read, write & invalidate sibling before other thread reads; broken implementation still produces one winner. **Smallest fix:** read original bytes before rendezvous, then return captured bytes; both unlocked contenders must compare identical snapshots.

Previous read’s eight findings, assessed against current implementation:

| Previous finding | Disposition | Source & regression evidence |
|---|---|---|
| 1. Quadratic TJ provenance | **FIXED** | Move semantics [inspect.rs:1812](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:1812); 50,000-empty-string [test:418](/Volumes/D/claude/rightkit/crates/rightkit-pdf/tests/hostile_input.rs:418). |
| 2. Inherited resources erased | **FIXED** | Ancestor lookup/copy [compose.rs:1357](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:1357); [test:3477](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:3477). |
| 3. Q restores wrong font | **FIXED** | Text-state restoration [inspect.rs:1744](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:1744); [test:4068](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:4068). |
| 4. Unicode-based Tw | **FIXED** | Encoded-code checks [inspect.rs:547](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:547); overflow [test:3658](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:3658). |
| 5. Concurrent commits | **PARTLY FIXED** | Destination lock [commit.rs:228](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:228); finding 2 remains. |
| 6. Path aliases | **PARTLY FIXED** | Identity comparison [commit.rs:116](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/commit.rs:116); finding 3 remains. |
| 7. Inherited text state | **FIXED** for reported Tr/Tc/Tw/Tz/Ts case | Explicit resets [compose.rs:1586](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:1586); [test:3580](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:3580). Broader failure: finding 4. |
| 8. CID width work | **FIXED** | Total budget [inspect.rs:1401](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:1401); assignment-count [test:3943](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:3943). |

Earlier twelve dispositions carried inside previous read remain **FIXED for their reported reproducers**:

| Earlier finding | Current evidence |
|---|---|
| 1. CID/CMap overflow | [inspect.rs:1366](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:1366), [1589](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:1589) |
| 2. Array CMaps | [inspect.rs:1641](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:1641) |
| 3. gs font switch | [inspect.rs:1951](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:1951) |
| 4. Retained kerns | [compose.rs:744](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:744) |
| 5. Changed Tw | [compose.rs:687](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:687) |
| 6. Shared streams | [compose.rs:374](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:374) |
| 7. Substitution fallback | [compose.rs:312](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:312) |
| 8. Repeated synthetic spaces | [compose.rs:616](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:616) |
| 9. Inspection loops | [inspect.rs:369](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:369), [1027](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:1027) |
| 10. Gap provenance | [inspect.rs:2140](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:2140) |
| 11. Cross-stream promise | [inspect.rs:2828](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:2828) |
| 12. Gap includes Tc | [inspect.rs:2728](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:2728) |

Static source/test read at `main`, `48f8fa01`; no builds or tests executed.

**DO NOT PUBLISH — findings 1–5 block publication.**