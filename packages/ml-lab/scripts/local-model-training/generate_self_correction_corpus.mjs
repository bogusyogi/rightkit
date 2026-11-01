#!/usr/bin/env node
/**
 * Target-first deterministic generator for the L1 app-aware "self_correction"
 * bucket (schema `heardright.l1_appaware_pair.v1`, 700 rows). Mirrors
 * generate_precision_corpus.mjs's op format (buildPermittedEditOps /
 * replayPermittedEditOps) and lineage-hash discipline, but produces raw
 * pre-serialization rows (plain input_text, not L1C1/Context-prefixed).
 *
 * Design: author output_text (the clean final sentence) first, then derive
 * input_text by injecting a superseded value + a correction cue BEFORE the
 * final value: "Let's meet at 7, oh no, 8." -> "Let's meet at 8."
 */
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const scriptPath = fileURLToPath(import.meta.url);
const here = path.dirname(scriptPath);
const outputDir = path.resolve(here, "corpus/l1-appaware-v1");
const corpusPath = path.resolve(outputDir, "self_correction.jsonl");
const summaryPath = path.resolve(outputDir, "self_correction.summary.json");

const SCHEMA = "heardright.l1_appaware_pair.v1";
const BUCKET = "self_correction";

const CUES = ["oh no", "no wait", "no, wait", "no no", "no, no", "I mean", "I meant", "make that", "actually make that", "scratch that", "strike that", "sorry", "actually"];

const NAMES = ["Priya Shah", "Marcus Webb", "Elena Cho", "Tobias Reyes", "Naomi Frost", "Diego Lang", "Sasha Byrne", "Owen Marsh", "Farah Odom", "Callum Reed", "Ines Duarte", "Ravi Sen", "Talia Brooks", "Wesley Kim", "Junia Voss", "Rhea Malik", "Bram Wolfe", "Cleo Nash", "Idris Faulk", "Marta Solis"];
const PRODUCTS = ["Nimbus", "Cobalt Suite", "Fathom", "Lighthouse", "Ridgeline", "Halcyon", "Marrow", "Vantage"];
const DOCS = ["contract", "invoice", "proposal", "itinerary", "runbook", "deck", "brief", "summary"];
const TOPICS = ["launch", "migration", "renewal", "onboarding", "audit", "rollout", "integration", "retro"];
const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const OPTION_PAIRS = [["classic", "modern"], ["basic", "premium"], ["quiet", "bold"], ["minimal", "detailed"], ["remote", "onsite"], ["weekly", "biweekly"]];
const TONE_PAIRS = [["formal", "casual"], ["urgent", "calm"], ["playful", "serious"], ["short", "detailed"], ["direct", "gentle"], ["neutral", "enthusiastic"]];
const VERB_PAIRS = [["Send", "Hold"], ["Approve", "Reject"], ["Archive", "Restore"], ["Cancel", "Reschedule"], ["Merge", "Revert"], ["Publish", "Draft"]];
const DEPTS = ["marketing", "sales", "support", "legal", "design", "finance"];
const QUEUES = ["billing", "engineering", "onboarding", "escalations", "legal", "research"];
const APP_WINDOWS = [["Slack", "#launch-planning"], ["Outlook", "New Message"], ["Notion", "Meeting Notes"], ["Messages", "Team Chat"], ["Teams", "Weekly Sync"], ["Notes", "Quick Note"], ["Mail", "Draft"], ["Calendar", "New Event"]];
const ONES = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen"];
const TENS = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];

const QUOTAS = { number: 200, datetime: 130, name: 110, phrase: 110, multi: 60, negative_trap: 90 };
const CONTEXT_QUOTAS = { number: 34, datetime: 22, name: 19, phrase: 19, multi: 10, negative_trap: 16 };
const SUB_ORDER = ["number", "datetime", "name", "phrase", "multi", "negative_trap"];

const sha = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const pad = (n, width = 3) => String(n).padStart(width, "0");
const pad2 = (n) => String(n).padStart(2, "0");

function numWord(n) {
  if (n < 20) return ONES[n];
  const t = Math.floor(n / 10);
  const o = n % 10;
  return o ? `${TENS[t]}-${ONES[o]}` : TENS[t];
}

function ordinal(n) {
  const j = n % 10;
  const k = n % 100;
  if (j === 1 && k !== 11) return `${n}st`;
  if (j === 2 && k !== 12) return `${n}nd`;
  if (j === 3 && k !== 13) return `${n}rd`;
  return `${n}th`;
}

function ticketFor(seed) {
  return `TCK-${5000 + seed}`;
}

function fileFor(li, offset = 0) {
  const n = li + offset;
  return `report_2026-08-${pad2(1 + (n % 28))}_v${1 + (n % 5)}.csv`;
}

function emailFor(li) {
  const local = ["dana", "priya", "marcus", "elena", "farah", "owen"][li % 6];
  const domain = ["northfield.co", "brightloop.io", "cedarbay.dev"][li % 3];
  return `${local}${1 + li}@${domain}`;
}

// Mirrors generate_precision_corpus.mjs buildPermittedEditOps/replayPermittedEditOps verbatim.
function buildPermittedEditOps(input, output, authorityTags) {
  if (input === output) return [];
  const rows = input.length + 1;
  const columns = output.length + 1;
  const lcs = new Uint16Array(rows * columns);
  const at = (i, j) => i * columns + j;
  for (let i = input.length - 1; i >= 0; i -= 1) {
    for (let j = output.length - 1; j >= 0; j -= 1) {
      lcs[at(i, j)] = input[i] === output[j] ? 1 + lcs[at(i + 1, j + 1)] : Math.max(lcs[at(i + 1, j)], lcs[at(i, j + 1)]);
    }
  }
  const ops = [];
  let i = 0;
  let j = 0;
  let start = null;
  let expected = "";
  let replacement = "";
  const flush = () => {
    if (start === null) return;
    const core = { op: "replace_range", unit: "utf16_code_unit", start, end: start + expected.length, expected_text: expected, replacement_text: replacement, authority_tags: [...authorityTags] };
    ops.push({ ...core, op_hash: sha(JSON.stringify(core)) });
    start = null; expected = ""; replacement = "";
  };
  while (i < input.length || j < output.length) {
    if (i < input.length && j < output.length && input[i] === output[j]) { flush(); i += 1; j += 1; continue; }
    if (start === null) start = i;
    if (j < output.length && (i === input.length || lcs[at(i, j + 1)] > lcs[at(i + 1, j)])) { replacement += output[j]; j += 1; }
    else { expected += input[i]; i += 1; }
  }
  flush();
  return ops.map((op, index) => ({ op_id: `edit-${String(index + 1).padStart(3, "0")}`, ...op }));
}

function replayPermittedEditOps(input, ops) {
  let output = input;
  let previousStart = input.length + 1;
  for (const op of [...ops].sort((a, b) => b.start - a.start || b.end - a.end)) {
    if (op.op !== "replace_range" || op.unit !== "utf16_code_unit" || op.start < 0 || op.end < op.start || op.end > input.length || op.end > previousStart) throw new Error(`invalid edit range ${op.op_id}`);
    if (input.slice(op.start, op.end) !== op.expected_text) throw new Error(`edit source mismatch ${op.op_id}`);
    output = `${output.slice(0, op.start)}${op.replacement_text}${output.slice(op.end)}`;
    previousStart = op.start;
  }
  return output;
}

function mkValue(final, superseded, cue) {
  return { final, superseded, cue };
}

function assembleOutput(parts) {
  return parts.map((p) => (typeof p === "string" ? p : p.final)).join("");
}

function assembleInput(parts) {
  return parts.map((p) => (typeof p === "string" ? p : `${p.superseded}, ${p.cue}, ${p.final}`)).join("");
}

function cueFor(li) {
  return CUES[li % CUES.length];
}

function cuePairFor(li) {
  const a = (2 * li) % CUES.length;
  const b = (2 * li + 1) % CUES.length;
  return [CUES[a], CUES[b]];
}

function contextIndices(count, quota) {
  const step = Math.max(1, Math.floor(count / quota));
  const set = new Set();
  for (let k = 0; k < quota; k += 1) set.add(k * step);
  return set;
}

function contextPairFor(li) {
  return APP_WINDOWS[li % APP_WINDOWS.length];
}

function numberTemplate(li) {
  const cue = cueFor(li);
  const t = li % 8;
  if (t === 0) { const f = 3 + (li % 12); const s = f + 4 + (li % 5); const value = mkValue(numWord(f), numWord(s), cue); return { templateId: "number-t1", parts: ["Order ", value, " extra chairs for the orientation."] }; }
  if (t === 1) { const f = 10 + (li % 40); const s = f - (5 + (li % 4)); const value = mkValue(String(f), String(s), cue); return { templateId: "number-t2", parts: ["Print ", value, " copies of the onboarding packet."] }; }
  if (t === 2) { const f = 200 + li * 3; const s = f - 50 - (li % 20); const value = mkValue(`$${f}`, `$${s}`, cue); return { templateId: "number-t3", parts: ["The renovation budget is capped at ", value, "."] }; }
  if (t === 3) { const f = 5 + (li % 45); const s = f + 10 + (li % 7); const value = mkValue(`${f} percent`, `${s} percent`, cue); return { templateId: "number-t4", parts: ["Set the early-bird discount to ", value, "."] }; }
  if (t === 4) { const f = 100 + (li % 30); const s = f + 200; const value = mkValue(String(f), String(s), cue); return { templateId: "number-t5", parts: ["Meet me in room ", value, " for the debrief."] }; }
  if (t === 5) { const f = 4000 + li; const s = f + 111; const value = mkValue(String(f), String(s), cue); return { templateId: "number-t6", parts: ["Call extension ", value, " if the line drops."] }; }
  if (t === 6) { const f = 20 + ((li % 6) * 5); const s = f - (8 + (li % 5)); const value = mkValue(String(f), numWord(s), cue); return { templateId: "number-t7", parts: ["We are hosting ", value, " guests at the launch dinner."] }; }
  const f = 6 + (li % 20); const s = f + 3 + (li % 4); const ticket = ticketFor(li); const value = mkValue(String(f), String(s), cue);
  return { templateId: "number-t8", parts: ["Reserve ", value, ` pallets for ticket ${ticket}; do not change the ticket number.`], extraPreserve: [ticket] };
}

function datetimeTemplate(li) {
  const cue = cueFor(li);
  const t = li % 6;
  if (t === 0) {
    const hF = 1 + (li % 11); const offset = 1 + (li % 3); const hS = 1 + (((hF - 1) + offset) % 12);
    const mF = [0, 15, 30, 45][li % 4]; const mS = [0, 15, 30, 45][(li + 1) % 4];
    const style = li % 3; const ampmF = li % 2 ? "am" : "pm"; const ampmS = li % 2 ? "pm" : "am";
    const final = style === 0 ? `${hF}` : style === 1 ? `${hF}:${pad2(mF)}` : `${hF}:${pad2(mF)} ${ampmF}`;
    const superseded = style === 0 ? `${hS}` : style === 1 ? `${hS}:${pad2(mS)}` : `${hS}:${pad2(mS)} ${ampmS}`;
    return { templateId: "datetime-t1", parts: ["Let's start the standup at ", mkValue(final, superseded, cue), "."] };
  }
  if (t === 1) {
    const style = li % 3; const dayF = 1 + (li % 28); const monthF = MONTHS[li % 12]; const dayS = 1 + ((li + 9) % 28); const monthS = MONTHS[(li + 2) % 12];
    const final = style === 0 ? `${monthF} ${dayF}` : style === 1 ? `${monthF} ${ordinal(dayF)}` : `${pad2(1 + (li % 12))}/${pad2(dayF)}`;
    const superseded = style === 0 ? `${monthS} ${dayS}` : style === 1 ? `${monthS} ${ordinal(dayS)}` : `${pad2(1 + ((li + 2) % 12))}/${pad2(dayS)}`;
    return { templateId: "datetime-t2", parts: ["The renewal is due on ", mkValue(final, superseded, cue), "."] };
  }
  if (t === 2) { return { templateId: "datetime-t3", parts: ["We'll ship it out on ", mkValue(WEEKDAYS[li % 7], WEEKDAYS[(li + 3) % 7], cue), "."] }; }
  if (t === 3) {
    const hF = 1 + (li % 11); const hS = 1 + ((li + 4) % 11); const wF = WEEKDAYS[li % 7]; const wS = WEEKDAYS[(li + 2) % 7];
    const final = `next ${wF} at ${hF} ${li % 2 ? "am" : "pm"}`; const superseded = `next ${wS} at ${hS} ${li % 2 ? "pm" : "am"}`;
    return { templateId: "datetime-t4", parts: ["Push the release to ", mkValue(final, superseded, cue), "."] };
  }
  if (t === 4) { return { templateId: "datetime-t5", parts: ["The onboarding session runs ", mkValue(`${15 * (1 + (li % 6))} minutes`, `${15 * (1 + ((li + 2) % 6))} minutes`, cue), "."] }; }
  return { templateId: "datetime-t6", parts: ["The permit expires on the ", mkValue(ordinal(1 + (li % 28)), ordinal(1 + ((li + 5) % 28)), cue), "."] };
}

function nameTemplate(li) {
  const cue = cueFor(li);
  const t = li % 6;
  if (t === 0) { const doc = DOCS[li % 8]; return { templateId: "name-t1", parts: [`Send the ${doc} to `, mkValue(NAMES[li % 20], NAMES[(li + 7) % 20], cue), "."] }; }
  if (t === 1) { const topic = TOPICS[li % 8]; return { templateId: "name-t2", parts: [mkValue(NAMES[(li + 3) % 20], NAMES[(li + 11) % 20], cue), ` is leading the ${topic} kickoff.`] }; }
  if (t === 2) { const topic = TOPICS[(li + 1) % 8]; return { templateId: "name-t3", parts: ["Loop in ", mkValue(NAMES[(li + 5) % 20], NAMES[(li + 13) % 20], cue), ` on the ${topic} thread.`] }; }
  if (t === 3) { return { templateId: "name-t4", parts: ["The product is called ", mkValue(PRODUCTS[li % 8], PRODUCTS[(li + 3) % 8], cue), "."] }; }
  if (t === 4) { const doc = DOCS[(li + 2) % 8]; const email = emailFor(li); return { templateId: "name-t5", parts: [`Forward the ${doc} to `, mkValue(NAMES[(li + 2) % 20], NAMES[(li + 9) % 20], cue), ` at ${email}; keep the email exactly as written.`], extraPreserve: [email] }; }
  const topic = TOPICS[(li + 3) % 8]; const ticket = `TCK-${8000 + li}`;
  return { templateId: "name-t6", parts: [`Assign the ${topic} ticket ${ticket} to `, mkValue(NAMES[(li + 6) % 20], NAMES[(li + 15) % 20], cue), ", not the previous owner."], extraPreserve: [ticket] };
}

function phraseTemplate(li) {
  const cue = cueFor(li);
  const t = li % 6;
  if (t === 0) { const pair = VERB_PAIRS[li % 6]; return { templateId: "phrase-t1", parts: [mkValue(pair[1], pair[0], cue), " the meeting."] }; }
  if (t === 1) { return { templateId: "phrase-t2", parts: ["Send it to ", mkValue(DEPTS[(li + 2) % 6], DEPTS[(li + 5) % 6], cue), "."] }; }
  if (t === 2) { const pair = OPTION_PAIRS[(li + 1) % 6]; return { templateId: "phrase-t3", parts: ["Choose the ", mkValue(pair[1], pair[0], cue), " option."] }; }
  if (t === 3) { const pair = TONE_PAIRS[(li + 4) % 6]; return { templateId: "phrase-t4", parts: ["Use ", mkValue(`the ${pair[1]} tone`, `the ${pair[0]} tone`, cue), " for the launch announcement."] }; }
  if (t === 4) { return { templateId: "phrase-t5", parts: ["Route the ticket to the ", mkValue(QUEUES[(li + 3) % 6], QUEUES[(li + 1) % 6], cue), " queue."] }; }
  return { templateId: "phrase-t6", parts: ["Rename the file to ", mkValue(fileFor(li, 7), fileFor(li, 0), cue), "; keep the extension exact."] };
}

function timeValueSimple(li, cue) {
  const hF = 1 + (li % 11); const hS = 1 + (((hF - 1) + 2 + (li % 3)) % 12);
  return mkValue(`${hF}:${pad2([0, 15, 30, 45][li % 4])}`, `${hS}:${pad2([0, 15, 30, 45][(li + 1) % 4])}`, cue);
}
function countValueSimple(li, cue, base, span) {
  const f = base + (li % span); const s = f + 2 + (li % 3);
  return mkValue(String(f), String(s), cue);
}
function dateValueSimple(li, cue) {
  const dayF = 1 + (li % 28); const monthF = MONTHS[li % 12]; const dayS = 1 + ((li + 9) % 28); const monthS = MONTHS[(li + 2) % 12];
  return mkValue(`${monthF} ${dayF}`, `${monthS} ${dayS}`, cue);
}
function weekdayValueSimple(li, cue) {
  return mkValue(WEEKDAYS[li % 7], WEEKDAYS[(li + 3) % 7], cue);
}
function nameValueSimple(li, cue, offsetF, offsetS) {
  return mkValue(NAMES[(li + offsetF) % 20], NAMES[(li + offsetS) % 20], cue);
}
function moneyValueSimple(li, cue) {
  const f = 200 + li * 4; const s = f - 60 - (li % 15);
  return mkValue(`$${f}`, `$${s}`, cue);
}
function optionValueSimple(li, cue) {
  const pair = OPTION_PAIRS[li % 6];
  return mkValue(pair[1], pair[0], cue);
}

function multiTemplate(li) {
  const [cue1, cue2] = cuePairFor(li);
  const t = li % 6;
  const topic = TOPICS[li % 8];
  if (t === 0) { return { templateId: "multi-t1", parts: ["Meet ", nameValueSimple(li, cue1, 0, 7), " at ", timeValueSimple(li, cue2), " for the debrief."] }; }
  if (t === 1) { return { templateId: "multi-t2", parts: ["Book ", countValueSimple(li, cue1, 4, 16), " seats for ", nameValueSimple(li, cue2, 2, 9), "'s workshop."] }; }
  if (t === 2) { return { templateId: "multi-t3", parts: [nameValueSimple(li, cue1, 5, 13), ` owns the ${topic} deadline on `, dateValueSimple(li, cue2), "."] }; }
  if (t === 3) { return { templateId: "multi-t4", parts: ["Go with the ", optionValueSimple(li, cue1), " plan and reserve ", countValueSimple(li, cue2, 3, 12), " seats."] }; }
  if (t === 4) { return { templateId: "multi-t5", parts: ["Invite ", nameValueSimple(li, cue1, 6, 15), " to the review on ", weekdayValueSimple(li, cue2), "."] }; }
  return { templateId: "multi-t6", parts: ["Cap the budget at ", moneyValueSimple(li, cue1), " through ", dateValueSimple(li, cue2), "."] };
}

function trapTemplate(li) {
  const t = li % 6;
  if (t === 0) { const wd = WEEKDAYS[li % 7]; return { templateId: "negative_trap-t1", text: `There's no meeting scheduled for ${wd}.`, coreClause: "no meeting scheduled" }; }
  if (t === 1) { const ticket = `TCK-${9000 + li}`; return { templateId: "negative_trap-t2", text: `No worries, ticket ${ticket} is already resolved.`, coreClause: "No worries", extraPreserve: [ticket] }; }
  if (t === 2) { const doc = DOCS[li % 8]; return { templateId: "negative_trap-t3", text: `Sorry for the delay; the ${doc} is attached as promised.`, coreClause: "Sorry for the delay" }; }
  if (t === 3) { const topic = TOPICS[li % 8]; return { templateId: "negative_trap-t4", text: `Actually, the ${topic} numbers improved this quarter.`, coreClause: "Actually, the" }; }
  if (t === 4) { const topic = TOPICS[(li + 2) % 8]; return { templateId: "negative_trap-t5", text: `I have no objection to the new ${topic} schedule.`, coreClause: "no objection" }; }
  const month = MONTHS[li % 12]; const day = 1 + (li % 28);
  return { templateId: "negative_trap-t6", text: `The renewal actually closed on ${month} ${day}, ahead of schedule.`, coreClause: "actually closed", extraPreserve: [`${month} ${day}`] };
}

const TEMPLATE_FN = { number: numberTemplate, datetime: datetimeTemplate, name: nameTemplate, phrase: phraseTemplate, multi: multiTemplate };

function finalizeRow(subBucket, li, templateId, output_text, input_text, mustPreserveList, extraPreserve, hasContext, contextPair, globalSeq) {
  const must_preserve = [...mustPreserveList, ...(extraPreserve || [])];
  const bounded_context = hasContext ? { app_name: contextPair[0], window_title: contextPair[1] } : {};
  const deterministic_diff = buildPermittedEditOps(input_text, output_text, ["self_correction", subBucket]);
  const id = `self_correction-${pad(globalSeq, 3)}`;
  const seed = `${subBucket}-${pad(li + 1, 3)}`;
  const template_hash = sha(JSON.stringify({ bucket: BUCKET, sub_bucket: subBucket, template_id: templateId }));
  const source_hash = sha(JSON.stringify({ input_text, bounded_context, seed }));
  const row_hash = sha(JSON.stringify({ id, output_hash: sha(output_text), diff_hash: sha(JSON.stringify(deterministic_diff)), template_hash, source_hash }));
  return {
    schema: SCHEMA, id, bucket: BUCKET, sub_bucket: subBucket, input_text, output_text, bounded_context, must_preserve, deterministic_diff,
    source: "builder_deterministic_v1", construction: "target_first_deterministic_v1", template_id: templateId, seed, template_hash, source_hash, row_hash, split: "train",
  };
}

export function generateDataset() {
  const entries = [];
  let globalSeq = 0;
  for (const subBucket of SUB_ORDER) {
    const count = QUOTAS[subBucket];
    const contextSet = contextIndices(count, CONTEXT_QUOTAS[subBucket]);
    for (let li = 0; li < count; li += 1) {
      globalSeq += 1;
      const hasContext = contextSet.has(li);
      const contextPair = contextPairFor(li);
      if (subBucket === "negative_trap") {
        const { templateId, text, coreClause, extraPreserve } = trapTemplate(li);
        const output_text = text;
        const input_text = li % 3 === 0 ? `${text.charAt(0).toLowerCase()}${text.slice(1)}` : text;
        const row = finalizeRow(subBucket, li, templateId, output_text, input_text, [coreClause], extraPreserve, hasContext, contextPair, globalSeq);
        entries.push({ row, cues: [], supersededValues: [] });
        continue;
      }
      const { templateId, parts, extraPreserve } = TEMPLATE_FN[subBucket](li);
      const output_text = assembleOutput(parts);
      const input_text = assembleInput(parts);
      const values = parts.filter((p) => typeof p !== "string");
      const row = finalizeRow(subBucket, li, templateId, output_text, input_text, values.map((v) => v.final), extraPreserve, hasContext, contextPair, globalSeq);
      entries.push({ row, cues: values.map((v) => v.cue), supersededValues: values.map((v) => v.superseded) });
    }
  }
  return entries;
}

export function buildRows() {
  return generateDataset().map((e) => e.row);
}

function digitTokens(text) {
  return text.match(/\d+/g) || [];
}

export function validate(entries, { full = true } = {}) {
  const errors = [];
  const rows = entries.map((e) => e.row);
  const bucketCounts = {};
  const templateCounts = {};
  const hashPattern = /^sha256:[0-9a-f]{64}$/;
  const ids = new Set();
  let replayFailures = 0; let digitFailures = 0; let cueLeakFailures = 0; let supersededLeakFailures = 0;
  let preserveFailures = 0; let trapClauseFailures = 0; let trapDigitFailures = 0; let lineageFailures = 0;

  for (const entry of entries) {
    const row = entry.row;
    bucketCounts[row.sub_bucket] = (bucketCounts[row.sub_bucket] ?? 0) + 1;
    const tkey = `${row.sub_bucket}|${row.template_id}`;
    templateCounts[tkey] = (templateCounts[tkey] ?? 0) + 1;
    if (ids.has(row.id)) errors.push(`${row.id}: duplicate id`);
    ids.add(row.id);
    if (row.schema !== SCHEMA || row.bucket !== BUCKET || row.split !== "train" || row.source !== "builder_deterministic_v1" || row.construction !== "target_first_deterministic_v1") errors.push(`${row.id}: contract field mismatch`);
    if (![row.template_hash, row.source_hash, row.row_hash].every((h) => hashPattern.test(h))) { lineageFailures += 1; errors.push(`${row.id}: hash format`); }

    let replayed = null;
    try { replayed = replayPermittedEditOps(row.input_text, row.deterministic_diff); } catch (error) { replayFailures += 1; errors.push(`${row.id}: ${error.message}`); }
    if (replayed !== null && replayed !== row.output_text) { replayFailures += 1; errors.push(`${row.id}: replay mismatch`); }

    const inTok = digitTokens(row.input_text);
    const outTok = digitTokens(row.output_text);
    if (!outTok.every((t) => inTok.includes(t))) { digitFailures += 1; errors.push(`${row.id}: digit not subset`); }

    if (row.sub_bucket === "negative_trap") {
      if (JSON.stringify([...inTok].sort()) !== JSON.stringify([...outTok].sort())) { trapDigitFailures += 1; errors.push(`${row.id}: trap digit not preserved`); }
      if (!row.output_text.includes(row.must_preserve[0])) { trapClauseFailures += 1; errors.push(`${row.id}: trap clause missing`); }
    } else {
      const outLower = row.output_text.toLowerCase();
      if (CUES.some((c) => outLower.includes(c.toLowerCase()))) { cueLeakFailures += 1; errors.push(`${row.id}: cue leaked into output`); }
      for (const sv of entry.supersededValues) {
        if (sv && row.output_text.includes(sv)) { supersededLeakFailures += 1; errors.push(`${row.id}: superseded value leaked into output: ${sv}`); }
      }
    }
    for (const item of row.must_preserve) {
      if (!row.output_text.includes(item)) { preserveFailures += 1; errors.push(`${row.id}: must_preserve missing from output: ${item}`); }
    }
  }

  const shares = Object.entries(templateCounts).map(([key, count]) => { const [sb] = key.split("|"); return count / bucketCounts[sb]; });
  const maxShare = shares.length ? Math.max(...shares) : 0;
  if (maxShare > 0.2000001) errors.push(`template share cap exceeded: ${maxShare}`);

  const contextRowCount = rows.filter((r) => Object.keys(r.bounded_context).length > 0).length;
  if (full) {
    if (rows.length !== 700) errors.push(`row count ${rows.length}`);
    for (const [sb, expected] of Object.entries(QUOTAS)) if (bucketCounts[sb] !== expected) errors.push(`bucket ${sb}=${bucketCounts[sb]}`);
    if (contextRowCount !== 120) errors.push(`context row count ${contextRowCount}`);
  }
  if (errors.length) throw new Error(errors.slice(0, 40).join("\n"));
  return {
    row_count: rows.length, bucket_counts: bucketCounts, template_counts: templateCounts, max_template_share: maxShare,
    replay_failures: replayFailures, digit_failures: digitFailures, cue_leak_failures: cueLeakFailures, superseded_leak_failures: supersededLeakFailures,
    preserve_failures: preserveFailures, trap_clause_failures: trapClauseFailures, trap_digit_failures: trapDigitFailures, lineage_failures: lineageFailures,
    context_row_count: contextRowCount,
  };
}

function main() {
  const entries = generateDataset();
  const metrics = validate(entries, { full: true });
  const rows = entries.map((e) => e.row);
  mkdirSync(outputDir, { recursive: true });
  const corpusText = `${rows.map((r) => JSON.stringify(r)).join("\n")}\n`;
  writeFileSync(corpusPath, corpusText, "utf8");
  const summary = {
    schema: "heardright.l1_appaware_self_correction_validation.v1",
    status: "pass",
    corpus_file: "self_correction.jsonl",
    corpus_sha256: sha(corpusText),
    generator_file: "tauri-app-next/scripts/local-model-training/generate_self_correction_corpus.mjs",
    generator_sha256: sha(readFileSync(scriptPath)),
    row_count: metrics.row_count,
    bucket_counts: metrics.bucket_counts,
    context_row_count: metrics.context_row_count,
    max_template_share: metrics.max_template_share,
    checks: [
      "replay(input_text,deterministic_diff)===output_text for every row",
      "output digits are a subset of input digits",
      "negative_trap rows preserve all digits and retain the trap clause",
      "cue text absent from output_text for correction rows",
      "superseded value absent from output_text for correction rows",
      "must_preserve items present in output_text",
      "exact sub-bucket quotas and total row count 700",
      "template_id share <= 20% per sub-bucket",
      "exact context-bearing row count 120",
      "lineage hash format sha256:<64 hex>",
    ],
  };
  writeFileSync(summaryPath, `${JSON.stringify(summary, null, 2)}\n`, "utf8");
  console.log(JSON.stringify({ ...summary, ...metrics }, null, 2));
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) main();
