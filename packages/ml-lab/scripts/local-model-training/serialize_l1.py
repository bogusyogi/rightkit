#!/usr/bin/env python3
"""The ONE L1 serialization contract, v2 = wire marker `L1C1` (train = eval = app).

The 2026-07-23 run failed packet parity because the trainer silently serialized
only `input_text`. The 2026-07-26 fix (`Context: ... | Transcript: ...`) carried
only four flat context fields and dropped every nested-schema row to
`Context: none`. This version renders the full sanitized runtime envelope so
app kind, writing region, vocabulary, and sound-alikes reach the model, and it
normalizes BOTH `bounded_context` shapes the corpus contains.

Every L1 consumer (packet compiler, JS parity fixture, Rust app payload builder)
must render the model input through this function or a byte-identical mirror.
`test_serialize_l1_parity.mjs` asserts Python and JS stay identical; the Rust
unit test asserts Rust matches the frozen fixture.

Wire format (one user turn, no system message):

    L1C1
    {"k":"notes","a":"OneNote","w":"Plan","f":"...","s":"...","r":"...","v":["..."],"x":[["heard","herd"]],"t":"raw transcript"}

Keys, fixed order: `k` app kind, `a` app, `w` window, `f` field text,
`s` selected text, `r` writing region, `v` vocabulary, `x` sound-alike pairs,
`t` transcript. Empty optional keys are omitted; `t` is always present and last.
Compact JSON separators, standard escaping, UTF-8 (no ASCII escaping), no new
tokenizer tokens.

Design decision (2026-08-02): `k` is DATA, read from the source row, never
derived from the app name here. The Rust app-kind substring map
(`app_kind()` in `l3_cleanup/wire.rs`) stays the single owner of
derivation; duplicating it into Python/JS/Rust serializers is the exact
divergence risk the plan flags as hidden coupling, so serialization only reads
a label the corpus already carries.
"""
from __future__ import annotations

import json

WIRE_MARKER = "L1C1"


def _clean(value: object) -> str:
    return str(value or "").strip()


def _normalize_context(context: dict | None) -> dict:
    """Map either bounded_context schema into the L1C1 envelope (no `t`).

    Flat schema (original gold + synthetic): app_name, window_title,
    field_text, selected_text, writing_region, vocabulary, sound_alikes.
    Nested schema (July-27 long-context rows): app_kind, app_window_title,
    focused_control{role,label,value}, document_selection_excerpt,
    recent_repaired_text, context_resolution_terms.
    """
    context = context or {}
    focused = context.get("focused_control")
    focused = focused if isinstance(focused, dict) else {}

    k = _clean(context.get("app_kind"))
    a = _clean(context.get("app_name"))
    w = _clean(context.get("window_title") or context.get("app_window_title"))
    f = _clean(context.get("field_text") or focused.get("value"))
    s = _clean(context.get("selected_text") or context.get("document_selection_excerpt"))
    r = _clean(context.get("writing_region") or context.get("recent_repaired_text"))

    vocab_source = context.get("vocabulary")
    if not vocab_source:
        vocab_source = context.get("context_resolution_terms")
    v = [_clean(term) for term in (vocab_source or []) if _clean(term)]

    x: list[list[str]] = []
    for pair in context.get("sound_alikes") or []:
        if not isinstance(pair, (list, tuple)) or len(pair) != 2:
            continue
        term = _clean(pair[0])
        alts = pair[1]
        alts = alts if isinstance(alts, (list, tuple)) else [alts]
        for alt in alts:
            alt = _clean(alt)
            if term and alt:
                x.append([term, alt])

    envelope: dict = {}
    for key, value in (("k", k), ("a", a), ("w", w), ("f", f), ("s", s), ("r", r)):
        if value:
            envelope[key] = value
    if v:
        envelope["v"] = v
    if x:
        envelope["x"] = x
    return envelope


def serialize_l1(row: dict) -> str:
    envelope = _normalize_context(row.get("bounded_context"))
    envelope["t"] = row["input_text"]
    body = json.dumps(envelope, ensure_ascii=False, separators=(",", ":"))
    return f"{WIRE_MARKER}\n{body}"


if __name__ == "__main__":
    import io
    import sys

    # Windows Python defaults these streams to cp1252; the contract is UTF-8.
    stdin = io.TextIOWrapper(sys.stdin.buffer, encoding="utf-8")
    stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8")
    for line in stdin:
        if line.strip():
            stdout.write(json.dumps(serialize_l1(json.loads(line)), ensure_ascii=False) + "\n")
    stdout.flush()
