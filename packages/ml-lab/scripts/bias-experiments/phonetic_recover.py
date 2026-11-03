#!/usr/bin/env python3
"""E2 - open-vocab phonetic recovery (post-decode), independent of the ASR.

Question it answers: given a greedy MISHEAR string (e.g. "tick tock", "nesslay"),
can a Double-Metaphone match against a brand catalog recover the canonical brand
("TikTok", "Nestle") AFTER decode -- the layer that scales to a large catalog
without touching the acoustic search?

Input: mishear strings, one per line on stdin (pipe E1's score-OFF transcripts
in), OR run with --demo for a built-in set. For each line it scans unigram +
bigram spans, Double-Metaphones them, and matches catalog DM keys (exact, or
edit-distance<=1 on the key to catch e.g. NSL~NSTL), gated by a length guard.

Read the output adversarially: the BENIGN control lines ("swung hard right",
"cut me some slack") SHOULD return no match. Any brand they recover is a FALSE
POSITIVE -- the precise reason the real layer must be context-gated (active app /
personal set), not fire on every phonetic near-miss in open speech.

This is an EXPERIMENT to measure recall vs false-positives of the phonetic layer,
not production code. The catalog here is tiny; the shipping one is large + gated.

    pip install --user metaphone
    python3 phonetic_recover.py --demo
    # or, against real greedy output from E1:
    #   <run E1, copy the "bias OFF" transcripts to a file> ; python3 phonetic_recover.py < off.txt
"""
import sys
from metaphone import doublemetaphone

CATALOG = [
    # the test brands (what you record for E1)
    "TikTok", "Nestle", "Oracle", "Amazon", "Netflix",
    "Hershey's", "Huawei", "Squarespace", "HeardRight",
    # a few dozen common brands so real matches AND false positives are visible
    "Google", "Apple", "Microsoft", "Slack", "Notion", "YouTube", "Spotify",
    "Adobe", "Nvidia", "Intel", "Samsung", "Xiaomi", "Porsche", "Ferrari",
    "Versace", "Givenchy", "Hermes", "Gucci", "Pepsi", "Gatorade", "Starbucks",
    "Walmart", "Costco", "Verizon", "Salesforce", "Atlassian", "Figma", "Vercel",
]


def dm_keys(s):
    return [k for k in doublemetaphone(s) if k]


def lev(a, b):
    if a == b:
        return 0
    if not a or not b:
        return max(len(a), len(b))
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


CAT = [(name, dm_keys(name)) for name in CATALOG]


def match_span(span):
    """(brand, cand_key, cat_key, dist) matches for one text span."""
    out = []
    for ck in dm_keys(span):
        if len(ck) < 3:                  # guard: short keys collide with everything
            continue
        for name, names_keys in CAT:
            for nk in names_keys:
                if len(nk) < 3:
                    continue
                d = lev(ck, nk)
                if d == 0 or (d == 1 and ck[0] == nk[0]):   # exact, or 1 edit + same onset
                    out.append((name, ck, nk, d))
    return out


def candidates(line):
    toks = [t for t in line.replace(",", " ").split() if t]
    spans = []
    for i, t in enumerate(toks):
        spans.append(t)                          # unigram
        if i + 1 < len(toks):
            spans.append(toks[i] + toks[i + 1])  # despaced bigram (tick tock -> ticktock)
            spans.append(toks[i] + " " + toks[i + 1])
    return spans


DEMO = [
    "tick tock", "nesslay", "net flicks", "you tube", "squares pace", "wah way",
    "hard right", "hood right", "or a coal",                       # plausible mishears
    "i heard right the first time", "swung hard right", "cut me some slack",  # benign controls
]


def main():
    lines = DEMO if "--demo" in sys.argv else [l.strip() for l in sys.stdin if l.strip()]
    for line in lines:
        seen = {}
        for span in candidates(line):
            for name, ck, nk, d in match_span(span):
                if name not in seen or d < seen[name][1]:
                    tag = "exact" if d == 0 else f"~{d}"
                    seen[name] = (f'"{span}" {ck}≈{nk} ({tag})', d)
        if seen:
            hits = ", ".join(
                f"{b} [{info}]" for b, (info, _) in sorted(seen.items(), key=lambda x: x[1][1])
            )
            print(f"{line!r}\n   -> {hits}")
        else:
            print(f"{line!r}\n   -> (no brand match)")


if __name__ == "__main__":
    main()
