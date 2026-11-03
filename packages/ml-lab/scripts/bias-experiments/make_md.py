#!/usr/bin/env python3
"""Assemble /tmp/l1l2_results.jsonl into one review MD table."""
import json, os
RES = ["/tmp/l1l2_results.jsonl", "/tmp/l1l2_L2.jsonl"]
OUT = "/Users/adrdsouza/claude/heardright/docs/experiments/2026-06-23-L1-L2-matrix.md"
ERRS = ["invoice", "logo", "footer", "whispered", "kerning", "squarespace"]
HDR = ["invoice", "logo", "footer", "whisper", "kerning", "square→Sq"]

data = []
for p in RES:
    if os.path.exists(p):
        data += [json.loads(l) for l in open(p)]
rows = []
for d in data:
    cells, ctrl = {}, []
    for r in d["rows"]:
        if r["control"]:
            ctrl.append("✓" if r["preserved"] else "✗")
        else:
            cells[r["target"]] = "✓" if r["fixed"] else "·"
    fixed = sum(cells.get(e) == "✓" for e in ERRS)
    ck = sum(c == "✓" for c in ctrl)
    rows.append((d["method"], d["layer"], cells, ctrl, fixed, ck))
rows.sort(key=lambda x: (-x[4], -x[5]))

L = ["# L1 / L2 correction matrix — measured on real Parakeet mishears (2026-06-23)", "",
     "Each cell = does that method fix that real clip error? `✓` fixed · `·` not fixed · "
     "controls (`ctrl1/2`) must stay UNCHANGED: `✓` preserved · `✗` mangled (a false-accept — bad).",
     "Targets: invoice(←inverse) logo(←lower) footer(←photo) whisper(←misbird) kerning(←gerning) "
     "square→Squarespace. `/raw` = on raw ASR; `/L1+` = stacked on the safe deterministic L1.", "",
     "| method | layer | " + " | ".join(HDR) + " | ctrl1 | ctrl2 | **fixed** | ctrl-kept |",
     "|---|---|" + "---|" * (len(HDR) + 4)]
for m, layer, cells, ctrl, fixed, ck in rows:
    c = (ctrl + ["?", "?"])[:2]
    L.append(f"| {m} | {layer} | " + " | ".join(cells.get(e, "·") for e in ERRS)
             + f" | {c[0]} | {c[1]} | **{fixed}/6** | {ck}/2 |")

L += ["", "## What each method actually output (changes only)", ""]
for d in data:
    L.append(f"**{d['method']}**")
    any_change = False
    for r in d["rows"]:
        if " ".join(r["out"].lower().split()) != " ".join(r["raw"].lower().split()):
            tag = "CTRL!" if r["control"] else (r["target"] or "")
            L.append(f"- [{tag}] `{r['raw'][:46]}…` → `{r['out'][:80]}`")
            any_change = True
    if not any_change:
        L.append("- (no changes)")
    L.append("")

os.makedirs(os.path.dirname(OUT), exist_ok=True)
open(OUT, "w").write("\n".join(L))
print("wrote", OUT, "—", len(rows), "methods")
