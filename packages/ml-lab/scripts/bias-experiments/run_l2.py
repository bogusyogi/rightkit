#!/usr/bin/env python3
"""L2 matrix: every seq2seq corrector + the SmolLM/Qwen chat models, each run on
RAW and on L1 output (stacked). Appends to /tmp/l1l2_results.jsonl. CPU only."""
import re, json, gc, torch
from metaphone import doublemetaphone
from transformers import AutoTokenizer, AutoModelForSeq2SeqLM, AutoModelForCausalLM

WORD = re.compile(r"[a-z]+(?:'[a-z]+)?")
def toks(s): return WORD.findall(s.lower())
def norm(s): return ' '.join(toks(s))
TESTS = [
    ("the inverse came to four thousand two hundred fifty dollars due on june", "invoice", False),
    ("on the left side the lower is broken we need to fix the icon", "logo", False),
    ("the one click drive photo description should link to the blog", "footer", False),
    ("i don't think i wanna do misbird audio but if you just want to try", "whispered", False),
    ("the font sizing on the heading is off and the gerning is too tight", "kerning", False),
    ("we looked at nestle hershey's huawei and the square space site", "squarespace", False),
    ("the situation swung hard right after the vote i heard right the first time", None, True),
    ("we met on tuesday morning to review the plan everyone agreed", None, True),
]
RESULTS = "/tmp/l1l2_L2.jsonl"

def lev(a, b):
    if a == b: return 0
    if not a or not b: return max(len(a), len(b))
    p = list(range(len(b)+1))
    for i, ca in enumerate(a, 1):
        c = [i]
        for j, cb in enumerate(b, 1): c.append(min(p[j]+1, c[j-1]+1, p[j-1]+(ca != cb)))
        p = c
    return p[-1]
def lev_ratio(a, b): return lev(a, b)/max(len(a), len(b), 1)

# --- L1 stacking layer = safe deterministic (dedup + phonetic brand) ---
def dedup(s):
    o, prev = [], None
    for w in s.split():
        if w.lower() != (prev or '').lower(): o.append(w)
        prev = w
    return ' '.join(o)
CAT = ["squarespace","heardright","oneclickdrive","tiktok","netflix","nestle","oracle",
       "amazon","youtube","instagram","zephyr","huawei"]
CK = [(n, [k for k in doublemetaphone(n) if k]) for n in CAT]
def phonetic_brand(s):
    t = s.split(); out = []; i = 0
    while i < len(t):
        hit = None
        for L in (2, 1):
            if i+L <= len(t):
                span = ''.join(toks(' '.join(t[i:i+L])))
                for k in doublemetaphone(span):
                    if not k or len(k) < 4: continue
                    for name, nk in CK:
                        for kk in nk:
                            if len(kk) >= 4 and (k == kk or (lev(k, kk) <= 1 and k[0] == kk[0])):
                                hit = (name, L); break
                        if hit: break
                if hit: break
        if hit: out.append(hit[0]); i += hit[1]
        else: out.append(t[i]); i += 1
    return ' '.join(out)
def L1(s): return phonetic_brand(dedup(s))

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
          f"controls-kept {sum(r['preserved'] for r in rows if r['control'])}/2", flush=True)

SYS = ("You are a speech-to-text corrector. Fix ONLY clear transcription errors — a wrong "
       "word that sounds like the intended one, or wrong brand/product casing. Do NOT rephrase, "
       "summarize, add, or remove anything else. Output ONLY the corrected sentence, nothing else.")

SEQ2SEQ = ["dayyanj/dj-ai-asr-grammar-corrector-small",
           "pszemraj/flan-t5-base-grammar-synthesis",
           "pszemraj/grammar-synthesis-small",
           "vennify/t5-base-grammar-correction"]
CHAT = ["HuggingFaceTB/SmolLM2-360M-Instruct",
        "Qwen/Qwen2.5-0.5B-Instruct",
        "Qwen/Qwen2.5-1.5B-Instruct"]

torch.set_num_threads(max(1, torch.get_num_threads()))

for name in SEQ2SEQ:
    try:
        tok = AutoTokenizer.from_pretrained(name)
        m = AutoModelForSeq2SeqLM.from_pretrained(name).eval()
        def gen(s):
            ids = tok(s, return_tensors='pt', truncation=True, max_length=256).input_ids
            with torch.no_grad():
                o = m.generate(ids, max_new_tokens=int(len(s.split())*1.6)+10, num_beams=1)
            return tok.decode(o[0], skip_special_tokens=True)
        short = name.split('/')[-1]
        record(f"{short} /raw", "L2-seq2seq", gen)
        record(f"{short} /L1+", "L2-seq2seq", lambda s, g=gen: g(L1(s)))
        del tok, m; gc.collect()
    except Exception as e:
        print("  seq2seq FAIL", name, repr(e)[:110], flush=True)

for name in CHAT:
    try:
        tok = AutoTokenizer.from_pretrained(name)
        m = AutoModelForCausalLM.from_pretrained(name, torch_dtype=torch.float32).eval()
        def gen(s):
            msgs = [{"role": "system", "content": SYS}, {"role": "user", "content": f"Correct: {s}"}]
            ids = tok.apply_chat_template(msgs, add_generation_prompt=True, return_tensors='pt')
            with torch.no_grad():
                o = m.generate(ids, max_new_tokens=len(s.split())+40, do_sample=False,
                               pad_token_id=tok.eos_token_id)
            out = tok.decode(o[0][ids.shape[1]:], skip_special_tokens=True).strip().strip('"')
            return s if lev_ratio(norm(s), norm(out)) > 0.45 else out  # reject wholesale rewrite
        short = name.split('/')[-1]
        record(f"{short} /raw", "L2-chat", gen)
        record(f"{short} /L1+", "L2-chat", lambda s, g=gen: g(L1(s)))
        del tok, m; gc.collect()
    except Exception as e:
        print("  chat FAIL", name, repr(e)[:110], flush=True)
print("L2 MATRIX DONE")
