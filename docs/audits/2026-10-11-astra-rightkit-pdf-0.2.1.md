# Astra read of rightkit-pdf 0.2.1 (working tree), 2026-10-11

Read-only review before publication. Verdict: do not publish. Findings verbatim:

1. **P1 — Hostile CID widths overflow; release builds can loop until OOM.** [inspect.rs:1129](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:1129)  
   Use an Identity-H font with descendant `/W [4294967295 4294967295 500]`. Inspection inserts one entry, then `c += 1` panics with overflow checks; unchecked arithmetic wraps to zero & continues filling billions of entries. `/W [4294967295 [500 500]]` also overflows at line 1112. Separately, `beginbfrange <0001> <0002> <FFFFFFFF> endbfrange` overflows `base + offset` at line 1185. These defects already exist at HEAD.

2. **P1 — Valid array-form CMaps become fabricated mappings, now used for editing.** [inspect.rs:1178](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:1178)  
   An Identity-H font containing `<0001> <0003> [<0041> <005A> <0042>]` maps codes to **AZB**. Parser instead treats first destination as scalar & invents **ABC**. With positive `/W` entries, source `<000100020003>` becomes editable; requesting **ACB** writes `<000100030002>`, actually **ABZ**. Save verification shares this decoder, so accepts erroneous output. Parser defect predates change; new CID editing makes it writable.

3. **P1 — `gs` bypasses new state-operator blocker & can select wrong replacement glyph.** [inspect.rs:1361](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:1361)  
   Use `(A) Tj /G gs (B) Tj`, where `/G` sets `/Font [F2-ref 10]`; F1 maps byte 67 to C, F2 maps it to Z, & both map 66 to B. Walker ignores `gs`, reports editable **AB** using F1, then replacement **AC** writes byte 67 into F2’s operand: rendered output becomes **AZ**. `gs` must affect state tracking or force refusal.

4. **P1 — Flattened edits retain obsolete internal kerns & undermeasure output.** [compose.rs:615](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:615)  
   With `tex_style_pdf(false, …)`, use `BT /F1 10 Tf 72 700 Td [(h) -100 (e) -100 (llo) -333 (big)] TJ ET`; replace **hello big** with **aa big**. Only recorded word-gap numbers are cleared, leaving `[(aa) -100 () -100 () -333 (big)]`. Fit calculation predicts **28.33 pt**, while output occupies **30.33 pt**. Bounds of 28.33 pt pass; text verification also passes.

5. **P1 — Same-length replacement omits changed `Tw` contribution.** [compose.rs:584](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:584)  
   With `tex_style_pdf(true, …)`, use `BT /F1 10 Tf 10 Tw 72 700 Td (axb) Tj ET`; replace **axb** with **a b**, retaining original 15 pt bounds. Positional branch measures only glyph-width difference & accepts **12.5 pt**. Actual width is **22.5 pt**, because new space adds 10 pt word spacing before `b`. Save verification accepts matching text.

6. **P1 — Editing one page mutates other pages sharing its content stream.** [compose.rs:1425](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:1425)  
   Give two pages identical resources & `/Contents 10 0 R`. Replace text on page 1. Composer overwrites object 10 globally, changing both pages while reporting only page 1 in `affected_pages`. Stream ownership needs checking or cloning before mutation. Pre-existing defect.

7. **P1 — Non-editable islands can silently fall through to substitution.** [compose.rs:122](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:122)  
   Use `BT /F1 10 Tf 72 700 Td (A) Tj 6.67 0 Td (B) Tj ET`, which receives `positioned_text_run`. Request **AC** with unchanged appearance & provide `TestFontResolver { id: "F1", bytes: installable_font() }`. Strategy selects `LocalTextObjectRewrite`, embeds replacement font & returns output despite blocker. Existing refusal test hides this through `FailingFontResolver`; refusal comes from resolver, not blocker enforcement.

8. **P1 — Synthetic gaps cannot round-trip repeated spaces, but compose returns success.** [compose.rs:540](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:540)  
   With space-less font, change `[(hello) -333 (big)] TJ` from **hello big** to **hello␠␠big**. Composer inserts another gap around an empty string; inspection synthesizes only one space between neighbouring glyphs. Returned candidate therefore disagrees with requested text. Normal save verification subsequently rejects it; compose should refuse unrepresentable spacing before returning success.

9. **P2 — Tiny malformed PDFs can trap inspection in unbounded loops.** [inspect.rs:826](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:826), [inspect.rs:241](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:241)  
   `/FirstChar 0 /LastChar 9223372036854775807 /Widths [500]` causes roughly \(2^{63}\) iterations despite one width. Alternatively, give reachable page `/Parent` pointing to itself & omit `/CropBox`; inherited lookup never terminates. Both predate change.

10. **P2 — Ordinary TJ gaps can be misclassified as positioned text.** [inspect.rs:1644](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:1644)  
    `[(hello) -333 () (big)] TJ` creates legitimate TJ word gap, but backwards provenance scan stops at empty string. Likewise, `[(hello) -333] TJ (big) Tj` loses provenance across show operators. Both receive `positioned_text_run` despite containing no positioning/state seam.

11. **P2 — `substitutionEditable` promises unsupported cross-stream edits.** [inspect.rs:2247](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:2247)  
    Split adjoining **Hello** across streams: first `BT /F1 12 Tf 72 700 Td (Hel) Tj`, second `(lo) Tj ET`. Graph reports `spans_across_streams` but `substitutionEditable: true`; changing appearance still always fails [compose.rs:288](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/compose.rs:288).

12. **P3 — Reported gap width includes `Tc`, contrary to documented contract.** [inspect.rs:2185](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/inspect.rs:2185)  
    At 10 pt, `3 Tc [(hello) -333 (big)] TJ` reports `widthPt = 6.33`; distance beyond `Tc` is **3.33 pt**. Detection subtracts `Tc`, but serialized width does not.

Payload-free `ReencodeFailure::Blocked` is internally consistent: constructors & matches use unit variant, mapped to `UnsupportedEncoding`. Actual returned compose errors discard local mutations; I found no direct disk-write path after those errors. Finding 7 instead bypasses intended refusal.

Reading alone cannot establish runtime reproduction, renderer behaviour, dependency-level malformed-stream panics/memory safety, or host save integration. Save verification checks text presence through shared inspection code, so provides no independent protection against findings 2–5.

**Verdict: do not publish 0.2.1.**

