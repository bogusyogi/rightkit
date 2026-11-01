#!/usr/bin/env node
/**
 * Byte-identical mirror of `serialize_l1.py` — the ONE L1 serialization
 * contract, wire marker `L1C1` (train = eval = app). Change both files
 * together or `test_serialize_l1_parity.mjs` fails.
 *
 * Wire format (one user turn, no system message):
 *
 *     L1C1
 *     {"k":"notes","a":"OneNote","w":"Plan","f":"...","s":"...","r":"...","v":["..."],"x":[["heard","herd"]],"t":"raw transcript"}
 *
 * Keys, fixed order: `k` app kind, `a` app, `w` window, `f` field text,
 * `s` selected text, `r` writing region, `v` vocabulary, `x` sound-alike
 * pairs, `t` transcript. Empty optional keys are omitted; `t` is always
 * present and last. Compact JSON, standard escaping, UTF-8 (no ASCII
 * escaping) — matches Python `separators=(",",":")`, `ensure_ascii=False`.
 */
const WIRE_MARKER = "L1C1";

function clean(value) {
  return String(value ?? "").trim();
}

function normalizeContext(context) {
  context = context ?? {};
  const focusedRaw = context.focused_control;
  const focused = focusedRaw && typeof focusedRaw === "object" && !Array.isArray(focusedRaw) ? focusedRaw : {};

  const k = clean(context.app_kind);
  const a = clean(context.app_name);
  const w = clean(context.window_title ?? context.app_window_title);
  const f = clean(context.field_text ?? focused.value);
  const s = clean(context.selected_text ?? context.document_selection_excerpt);
  const r = clean(context.writing_region ?? context.recent_repaired_text);

  let vocabSource = context.vocabulary;
  if (!vocabSource || (Array.isArray(vocabSource) && vocabSource.length === 0)) {
    vocabSource = context.context_resolution_terms;
  }
  const v = (vocabSource ?? []).map((term) => clean(term)).filter((term) => term);

  const x = [];
  for (const pair of context.sound_alikes ?? []) {
    if (!Array.isArray(pair) || pair.length !== 2) continue;
    const term = clean(pair[0]);
    let alts = pair[1];
    if (!Array.isArray(alts)) alts = [alts];
    for (const alt of alts) {
      const cleanAlt = clean(alt);
      if (term && cleanAlt) x.push([term, cleanAlt]);
    }
  }

  const envelope = {};
  for (const [key, value] of [["k", k], ["a", a], ["w", w], ["f", f], ["s", s], ["r", r]]) {
    if (value) envelope[key] = value;
  }
  if (v.length) envelope.v = v;
  if (x.length) envelope.x = x;
  return envelope;
}

export function serializeL1(row) {
  const envelope = normalizeContext(row.bounded_context);
  envelope.t = row.input_text;
  const body = JSON.stringify(envelope);
  return `${WIRE_MARKER}\n${body}`;
}
