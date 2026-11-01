#!/usr/bin/env python3
"""L1 matrix: every NO-LLM correction method on the real Parakeet mishears.
Appends per-method results to /tmp/l1l2_results.jsonl (read by make_md.py).
Methods: raw · dedup · phonetic-brand · spellcheck(Harper-class) · ngram-KN(wikitext) · kenlm(prebuilt)."""
import re, json, glob, os, math
from collections import Counter, defaultdict
from metaphone import doublemetaphone

WORD = re.compile(r"[a-z]+(?:'[a-z]+)?")
def toks(s): return WORD.findall(s.lower())
def norm(s): return ' '.join(toks(s))

TESTS = [  # (raw mishear, target word, is_control)
    ("the inverse came to four thousand two hundred fifty dollars due on june", "invoice", False),
    ("on the left side the lower is broken we need to fix the icon", "logo", False),
    ("the one click drive photo description should link to the blog", "footer", False),
    ("i don't think i wanna do misbird audio but if you just want to try", "whispered", False),
    ("the font sizing on the heading is off and the gerning is too tight", "kerning", False),
    ("we looked at nestle hershey's huawei and the square space site", "squarespace", False),
    ("the situation swung hard right after the vote i heard right the first time", None, True),
    ("we met on tuesday morning to review the plan everyone agreed", None, True),
]
RESULTS = "/tmp/l1l2_results.jsonl"

def record(method, layer, fn):
    rows = []
    for raw, tgt, ctrl in TESTS:
        try: out = fn(raw)
        except Exception as e: out = f"<err {e}>"
        rows.append({"raw": raw, "target": tgt, "control": ctrl, "out": out,
                     "fixed": (tgt is not None and tgt in norm(out)),
                     "preserved": norm(out) == norm(raw)})
    with open(RESULTS, 'a') as f:
        f.write(json.dumps({"method": method, "layer": layer, "rows": rows}) + '\n')
    print(f"  {method}: fixed {sum(r['fixed'] for r in rows if not r['control'])}/6, "
          f"controls-kept {sum(r['preserved'] for r in rows if r['control'])}/2")

def lev(a, b):
    if a == b: return 0
    if not a or not b: return max(len(a), len(b))
    p = list(range(len(b)+1))
    for i, ca in enumerate(a, 1):
        c = [i]
        for j, cb in enumerate(b, 1): c.append(min(p[j]+1, c[j-1]+1, p[j-1]+(ca != cb)))
        p = c
    return p[-1]

# ---- 1. raw / 2. dedup ----
record("raw", "baseline", lambda s: s)
def dedup(s):
    o, prev = [], None
    for w in s.split():
        if w.lower() != (prev or '').lower(): o.append(w)
        prev = w
    return ' '.join(o)
record("dedup", "L1-det", dedup)

# ---- 3. phonetic brand catalog (E2 logic, context-free here) ----
CATALOG = ["squarespace","heardright","oneclickdrive","tiktok","netflix","nestle","oracle",
           "amazon","youtube","instagram","zephyr","huawei"]
catkeys = [(n, [k for k in doublemetaphone(n) if k]) for n in CATALOG]
def phonetic_brand(s):
    t = s.split(); out = []; i = 0
    while i < len(t):
        hit = None
        for L in (2, 1):
            if i+L <= len(t):
                span = ''.join(toks(' '.join(t[i:i+L])))
                for k in doublemetaphone(span):
                    if not k or len(k) < 4: continue
                    for name, nk in catkeys:
                        for kk in nk:
                            if len(kk) >= 4 and (k == kk or (lev(k, kk) <= 1 and k[0] == kk[0])):
                                hit = (name, L); break
                        if hit: break
                if hit: break
        if hit:
            out.append(hit[0]); i += hit[1]
        else:
            out.append(t[i]); i += 1
    return ' '.join(out)
record("phonetic-brand", "L1-det", phonetic_brand)

# ---- 4. spellcheck (Harper-class proxy) ----
try:
    from spellchecker import SpellChecker
    sp = SpellChecker()
    def spellfix(s):
        out = []
        for w in s.split():
            lw = toks(w)
            if lw and lw[0] in sp.unknown([lw[0]]):
                out.append(sp.correction(lw[0]) or w)
            else: out.append(w)
        return ' '.join(out)
    record("spellcheck(Harper-class)", "L1-det", spellfix)
except Exception as e:
    print("  spellcheck skipped:", e)

# ---- shared n-gram corrector (candidate gen + LM rescoring, protected) ----
def build_vocab():
    v = set()
    try: v |= {w for w in open('/usr/share/dict/words').read().lower().split() if WORD.fullmatch(w) and 2 < len(w) < 14}
    except Exception: pass
    return v
DICT = build_vocab()
dm_index = defaultdict(set)
for w in DICT:
    for k in doublemetaphone(w):
        if k: dm_index[k].add(w)
def key_edits1(k):
    yield k
    A = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'
    for i in range(len(k)):
        yield k[:i]+k[i+1:]
        for c in A: yield k[:i]+c+k[i+1:]
    for i in range(len(k)+1):
        for c in A: yield k[:i]+c+k[i:]
def cands(w):
    c = {w}
    for k in doublemetaphone(w):
        if k and len(k) >= 2:
            for kk in key_edits1(k): c |= dm_index.get(kk, set())
    return c

def make_ngram_corrector(score_fn, unifreq, MARGIN=3.0, PROTECT=1e-4):
    """score_fn(prevwords, w)->logprob; unifreq(w)->relative freq. Protect common words."""
    def correct(sentence):
        out = toks(sentence)
        for i, w in enumerate(out):
            if unifreq(w) > PROTECT:   # common word -> never touch
                continue
            h = out[max(0, i-2):i]
            base = score_fn(h, w)
            best, bsc = w, base
            for c in cands(w):
                if c == w or unifreq(c) <= unifreq(w): continue  # only replace with a MORE frequent word
                sc = score_fn(h, c)
                if sc > bsc + MARGIN:
                    best, bsc = c, sc
            out[i] = best
        return ' '.join(out)
    return correct

# ---- 5. NLTK Kneser-Ney trigram on wikitext + HR domain ----
def load_corpus_lines():
    lines = []
    if os.path.exists('/tmp/l1corpus.txt'):
        lines += [l for l in open('/tmp/l1corpus.txt', errors='ignore')]
    for f in glob.glob('/Users/adrdsouza/claude/heardright/**/*.md', recursive=True):
        try: lines += open(f, errors='ignore').read().splitlines()
        except Exception: pass
    return lines
try:
    words = []
    for l in load_corpus_lines():
        if len(l) > 40: words += toks(l)
    words = words[:8_000_000]
    uni = Counter(); bi = Counter(); tri = Counter()
    for i, w in enumerate(words):
        uni[w] += 1
        if i >= 1: bi[(words[i-1], w)] += 1
        if i >= 2: tri[(words[i-2], words[i-1], w)] += 1
    Ntot = sum(uni.values()); Vv = len(uni)
    def sb_score(h, w):
        h = tuple(h[-2:])
        if len(h) >= 2 and tri[(h[-2], h[-1], w)] > 0:
            return math.log(tri[(h[-2], h[-1], w)] / bi[(h[-2], h[-1])])
        if len(h) >= 1 and bi[(h[-1], w)] > 0:
            return math.log(0.4 * bi[(h[-1], w)] / uni[h[-1]])
        return math.log(0.4 * 0.4 * (uni.get(w, 0) + 1) / (Ntot + Vv))
    corr = make_ngram_corrector(sb_score, lambda w: uni.get(w, 0)/max(Ntot, 1))
    record(f"ngram-SB-wikitext({Ntot//1000}k)", "L1-ngram", corr)
except Exception as e:
    print("  ngram-SB skipped:", repr(e)[:120])

# ---- 6. pre-built KenLM (if downloaded) ----
import glob as _g
arpa = next(iter(_g.glob(os.path.expanduser('~/.cache/huggingface/**/*gram*.*'), recursive=True)
                 + _g.glob('/tmp/*.arpa') + _g.glob('/tmp/*.bin')), None)
if arpa:
    try:
        import kenlm
        klm = kenlm.Model(arpa)
        # unigram freq proxy from the wikitext counts if available, else uniform-ish
        uf = (lambda w: uni.get(w, 0)/max(Ntot, 1)) if 'uni' in dir() else (lambda w: 0.0)
        def km_score(h, w):
            return klm.score(' '.join(h[-2:] + [w]), bos=False, eos=False)
        record(f"kenlm-prebuilt({os.path.basename(arpa)})", "L1-ngram",
               make_ngram_corrector(km_score, uf))
    except Exception as e:
        print("  kenlm skipped:", repr(e)[:120])
else:
    print("  kenlm: no pre-built ARPA found")
print("L1 MATRIX DONE")
