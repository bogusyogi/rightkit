#!/usr/bin/env node
/**
 * WER scorer for ASR bake-offs (@rightkit/ml-lab, entry `rk-asr-wer`).
 *
 * The normalizer and edit-distance are a line-for-line port of the pure-Python fallback of HeardRight's
 * unified WER harness (`norm()` and `_wer()`): strip <|tag|> markers, lowercase, drop everything outside
 * [a-z0-9' ], collapse whitespace; classic O(len(ref)*len(hyp)) Levenshtein over whitespace-split words,
 * normalized by max(1, len(ref_words)). No external WER package (`wer_dep: custom-levenshtein-port`).
 *
 * Contract (asr-bakeoff/CONTRACT.md):
 *   hypotheses.jsonl rows: {id, hypothesis, decode_s, ...}
 *   id_to_truth.json: {id: {clip, gt, group, duration_s?}}
 *   The scorer joins hypotheses by id -> id_to_truth -> gt.
 *
 * Optional exact-term recovery: `--terms a,b,c` lists vocabulary terms (the caller's, none built in);
 * `--term-group <group>` limits the scan to one truth group (default: every row).
 *
 * Run it only after the hypotheses are sealed. It never calls a provider or model.
 */
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath, pathToFileURL } from "node:url";

const __filename = fileURLToPath(import.meta.url);

function sha256Self() {
  return crypto.createHash("sha256").update(fs.readFileSync(__filename)).digest("hex");
}

// ---------------------------------------------------------------------------
// Normalizer — ported verbatim from unified_parakeet_gate.py::norm()
//   TAG = re.compile(r"<\|[^|]*\|>")
//   PUNCT = re.compile(r"[^a-z0-9' ]")
//   def norm(text):
//       text = TAG.sub(" ", (text or "").lower())
//       text = PUNCT.sub(" ", text)
//       return " ".join(text.split())
// ---------------------------------------------------------------------------
const TAG_RE = /<\|[^|]*\|>/g;
const PUNCT_RE = /[^a-z0-9' ]/g;

export const NORMALIZER_DESC =
  "case/punctuation-insensitive word normalizer ported from unified_parakeet_gate.py::norm(): " +
  "lowercase, strip <|tag|> markers, drop every character outside [a-z0-9' ], collapse whitespace. " +
  "Applied identically to reference and hypothesis before word-level edit distance.";

export function norm(text) {
  let t = String(text ?? "").toLowerCase();
  t = t.replace(TAG_RE, " ");
  t = t.replace(PUNCT_RE, " ");
  return t.split(/\s+/).filter(Boolean).join(" ");
}

// ---------------------------------------------------------------------------
// WER — ported verbatim (Levenshtein DP) from unified_parakeet_gate.py::_wer()
// fallback, extended with operation backtracking so we can also report
// substitutions/insertions/deletions (the reused harness only returned the
// scalar WER). The total edit distance and WER value are identical to the
// reused algorithm; only the breakdown is new instrumentation.
// ---------------------------------------------------------------------------
export function wordErrorStats(refText, hypText) {
  const r = refText.split(/\s+/).filter(Boolean);
  const h = hypText.split(/\s+/).filter(Boolean);
  const n = r.length;
  const m = h.length;

  // dp[i][j] = min edits to turn r[0..i) into h[0..j)
  const dp = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(0));
  const op = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(""));
  for (let j = 0; j <= m; j++) {
    dp[0][j] = j;
    op[0][j] = "i";
  }
  for (let i = 0; i <= n; i++) {
    dp[i][0] = i;
    op[i][0] = "d";
  }
  op[0][0] = "";

  for (let i = 1; i <= n; i++) {
    for (let j = 1; j <= m; j++) {
      const delCost = dp[i - 1][j] + 1; // matches prev[j] + 1 in the python DP
      const insCost = dp[i][j - 1] + 1; // matches cur[j-1] + 1
      const subCost = dp[i - 1][j - 1] + (r[i - 1] === h[j - 1] ? 0 : 1); // prev[j-1] + (0 if match)
      let best = subCost;
      let bestOp = r[i - 1] === h[j - 1] ? "=" : "s";
      if (delCost < best) {
        best = delCost;
        bestOp = "d";
      }
      if (insCost < best) {
        best = insCost;
        bestOp = "i";
      }
      dp[i][j] = best;
      op[i][j] = bestOp;
    }
  }

  const edits = dp[n][m];
  const wer = edits / Math.max(1, n);

  let subs = 0;
  let ins = 0;
  let dels = 0;
  let i = n;
  let j = m;
  while (i > 0 || j > 0) {
    const o = op[i][j];
    if (o === "=") {
      i -= 1;
      j -= 1;
    } else if (o === "s") {
      subs += 1;
      i -= 1;
      j -= 1;
    } else if (o === "d") {
      dels += 1;
      i -= 1;
    } else if (o === "i") {
      ins += 1;
      j -= 1;
    } else {
      break;
    }
  }

  return { wer, edits, subs, ins, dels, ref_words: n, hyp_words: m };
}

// ---------------------------------------------------------------------------
// Duration bucketing per the run packet's required robustness table:
// <=15s, 15-30s, 30-90s, >90s
// ---------------------------------------------------------------------------
export function durationBucket(durationS) {
  if (durationS == null || !Number.isFinite(durationS)) return "unknown";
  if (durationS <= 15) return "<=15s";
  if (durationS <= 30) return "15-30s";
  if (durationS <= 90) return "30-90s";
  return ">90s";
}

const DURATION_BUCKET_ORDER = ["<=15s", "15-30s", "30-90s", ">90s", "unknown"];

// Exact-term recovery: each caller-supplied term is matched case-insensitively as a substring of the
// already-lowercased normalized text (norm() strips punctuation but keeps alphanumerics, so a
// single-token term like "oneclickdrive" survives intact).
function exactTermRecovery(scanRows, terms) {
  const byTerm = {};
  for (const term of terms) {
    const needle = term.toLowerCase();
    let refCount = 0;
    let recovered = 0;
    let missed = 0;
    const missedClips = [];
    for (const row of scanRows) {
      const inRef = row.ref_norm.includes(needle);
      if (!inRef) continue;
      refCount += 1;
      if (row.hyp_norm.includes(needle)) {
        recovered += 1;
      } else {
        missed += 1;
        missedClips.push(row.clip ?? row.id);
      }
    }
    byTerm[term] = {
      ref_occurrences: refCount,
      recovered,
      missed,
      recovery_rate: refCount > 0 ? round4(recovered / refCount) : null,
      missed_clips: missedClips,
    };
  }
  return byTerm;
}

function newAgg() {
  return { edits: 0, words: 0, n: 0, wer_sum: 0, decodes: [], dur_sum: 0, decode_sum: 0 };
}

function addRow(agg, row) {
  agg.edits += row.errors;
  agg.words += row.words;
  agg.n += 1;
  agg.wer_sum += row.wer;
  // latency: only rows carrying a numeric decode_s contribute to timing stats
  if (typeof row.decode_s === "number" && Number.isFinite(row.decode_s)) {
    agg.decodes.push(row.decode_s);
    agg.decode_sum += row.decode_s;
    if (typeof row.duration_s === "number" && Number.isFinite(row.duration_s)) {
      agg.dur_sum += row.duration_s;
    }
  }
}

function percentile(sorted, p) {
  if (sorted.length === 0) return null;
  if (sorted.length === 1) return sorted[0];
  const idx = (sorted.length - 1) * p;
  const lo = Math.floor(idx);
  const hi = Math.ceil(idx);
  if (lo === hi) return sorted[lo];
  return sorted[lo] + (sorted[hi] - sorted[lo]) * (idx - lo);
}

function latencyStats(agg) {
  const timed = agg.decodes.length;
  if (timed === 0) {
    return { n_timed: 0, mean_s: null, p50_s: null, p95_s: null, rtfx: null };
  }
  const sorted = [...agg.decodes].sort((a, b) => a - b);
  const round3 = (x) => (x == null ? null : Math.round(x * 1000) / 1000);
  return {
    n_timed: timed,
    mean_s: round3(agg.decode_sum / timed),
    p50_s: round3(percentile(sorted, 0.5)),
    p95_s: round3(percentile(sorted, 0.95)),
    // RTFx = total audio seconds / total decode seconds (OpenASR convention); higher = faster
    rtfx: agg.decode_sum > 0 && agg.dur_sum > 0 ? round3(agg.dur_sum / agg.decode_sum) : null,
  };
}

function finalizeAgg(agg) {
  return {
    n: agg.n,
    errors: agg.edits,
    words: agg.words,
    corpus_wer: agg.words > 0 ? round4(agg.edits / agg.words) : agg.n > 0 ? 1 : null,
    mean_clip_wer: agg.n > 0 ? round4(agg.wer_sum / agg.n) : null,
    latency: latencyStats(agg),
  };
}

function round4(x) {
  return Math.round(x * 10000) / 10000;
}

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function readJsonl(file) {
  const text = fs.readFileSync(file, "utf8");
  const rows = [];
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    rows.push(JSON.parse(trimmed));
  }
  return rows;
}

// Tolerant field lookup on truth rows, mirroring unified_parakeet_gate.py's
// own alias pattern (clip_name()/reference_text() accept multiple key names).
// The shared id_to_truth.json schema has been observed under both
// {clip, gt} and {clip_id, ground_truth} during concurrent Lane B writes;
// accept either so the scorer does not silently score against undefined refs.
function truthClip(truth) {
  return truth.clip ?? truth.clip_id ?? truth.file ?? null;
}

function truthGt(truth) {
  return truth.gt ?? truth.ground_truth ?? truth.transcript ?? truth.authority ?? "";
}

/**
 * Score a sealed hypotheses.jsonl against id_to_truth.json.
 * @param {Array<{id:string, hypothesis:string, decode_s?:number}>} hypRows
 * @param {Record<string, {clip?:string, clip_id?:string, gt?:string, ground_truth?:string, group:string, duration_s?:number}>} idToTruth
 */
/**
 * @param {{terms?: string[], termGroup?: string | null}} [options] exact-term recovery: the terms to
 *   scan for and the truth group to scan (null: every row).
 */
export function scoreCorpus(hypRows, idToTruth, { terms = [], termGroup = null } = {}) {
  const rows = [];
  const missingTruth = [];
  const seenIds = new Set();

  for (const hyp of hypRows) {
    const id = String(hyp.id ?? "");
    if (seenIds.has(id)) continue; // duplicate row id; keep first, do not double-count
    seenIds.add(id);
    const truth = idToTruth[id];
    if (!truth) {
      missingTruth.push(id);
      continue;
    }
    const refNorm = norm(truthGt(truth));
    const hypNorm = norm(hyp.hypothesis);
    const stats = wordErrorStats(refNorm, hypNorm);
    rows.push({
      id,
      clip: truthClip(truth),
      group: truth.group,
      duration_s: truth.duration_s ?? null,
      duration_bucket: durationBucket(truth.duration_s),
      decode_s: hyp.decode_s ?? null,
      wer: round4(stats.wer),
      errors: stats.edits,
      words: stats.ref_words,
      subs: stats.subs,
      ins: stats.ins,
      dels: stats.dels,
      ref_norm: refNorm,
      hyp_norm: hypNorm,
    });
  }

  // ids present in truth but absent from hypotheses (availability failures)
  const missingRows = Object.keys(idToTruth).filter((id) => !seenIds.has(id) || missingTruth.includes(id));
  const missingFromHyp = Object.keys(idToTruth).filter((id) => !hypRows.some((h) => String(h.id ?? "") === id));

  const corpusAgg = newAgg();
  const byGroup = {};
  const byBucket = {};
  for (const row of rows) {
    addRow(corpusAgg, row);
    byGroup[row.group] = byGroup[row.group] || newAgg();
    addRow(byGroup[row.group], row);
    byBucket[row.duration_bucket] = byBucket[row.duration_bucket] || newAgg();
    addRow(byBucket[row.duration_bucket], row);
  }

  const by_group = {};
  for (const [group, agg] of Object.entries(byGroup).sort(([a], [b]) => a.localeCompare(b))) {
    by_group[group] = finalizeAgg(agg);
  }

  const by_duration_bucket = {};
  for (const bucket of DURATION_BUCKET_ORDER) {
    if (byBucket[bucket]) by_duration_bucket[bucket] = finalizeAgg(byBucket[bucket]);
  }

  const termRows = termGroup == null ? rows : rows.filter((row) => row.group === termGroup);

  return {
    rows,
    corpus: finalizeAgg(corpusAgg),
    by_group,
    by_duration_bucket,
    term_group: termGroup,
    exact_term_recovery: exactTermRecovery(termRows, terms),
    missing_truth_ids: missingTruth,
    missing_hypothesis_ids: missingFromHyp,
    completion_count: rows.length,
    expected_count: Object.keys(idToTruth).length,
  };
}

export function scorerHeader() {
  return {
    scorer_src_sha256: sha256Self(),
    normalizer_desc: NORMALIZER_DESC,
    node_version: process.version,
    wer_dep: "custom-levenshtein-port",
    wer_dep_version: "n/a (no external package; Levenshtein DP ported inline)",
    generated_at: new Date().toISOString(),
  };
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------
function parseArgs(argv) {
  const args = { hyp: null, truth: null, out: null, terms: [], termGroup: null };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--hyp") args.hyp = argv[++i];
    else if (a === "--truth") args.truth = argv[++i];
    else if (a === "--out") args.out = argv[++i];
    else if (a === "--terms") args.terms = String(argv[++i] ?? "").split(",").map((t) => t.trim()).filter(Boolean);
    else if (a === "--term-group") args.termGroup = argv[++i] ?? null;
    else if (a === "--self-test") args.selfTest = true;
  }
  return args;
}

function runCli() {
  const args = parseArgs(process.argv.slice(2));

  if (args.selfTest) {
    selfTest();
    console.log("score-asr-wer self-test ok");
    return 0;
  }

  if (!args.hyp || !args.truth) {
    console.error("usage: node score-asr-wer.mjs --hyp <hypotheses.jsonl> --truth <id_to_truth.json> [--out <out.json>] [--terms a,b] [--term-group <group>]");
    console.error("       node score-asr-wer.mjs --self-test");
    return 2;
  }

  const hypRows = readJsonl(path.resolve(args.hyp));
  const idToTruth = readJson(path.resolve(args.truth));
  const result = scoreCorpus(hypRows, idToTruth, { terms: args.terms, termGroup: args.termGroup });
  const output = { ...scorerHeader(), hyp_path: args.hyp, truth_path: args.truth, ...result };

  if (args.out) {
    fs.mkdirSync(path.dirname(path.resolve(args.out)), { recursive: true });
    fs.writeFileSync(path.resolve(args.out), JSON.stringify(output, null, 2));
    console.log(`wrote ${args.out}`);
  } else {
    console.log(JSON.stringify(output, null, 2));
  }
  console.log(
    `corpus_wer=${output.corpus.corpus_wer} n=${output.corpus.n}/${output.expected_count}`,
  );
  return 0;
}

// ---------------------------------------------------------------------------
// Self-test — hand-checked cases
// ---------------------------------------------------------------------------
function approxEq(a, b, eps = 1e-9) {
  return Math.abs(a - b) < eps;
}

function assert(cond, message) {
  if (!cond) throw new Error(`self-test failed: ${message}`);
}

function selfTest() {
  // 1. Hand-checked case from the task spec: 1 substitution in 5 words = 0.2 WER.
  {
    const stats = wordErrorStats(norm("a b c d e"), norm("a b x d e"));
    assert(approxEq(stats.wer, 0.2), `1-sub/5-words expected 0.2 got ${stats.wer}`);
    assert(stats.subs === 1 && stats.ins === 0 && stats.dels === 0, "expected exactly 1 substitution");
    assert(stats.edits === 1, "expected 1 total edit");
  }

  // 2. Insertion-only: ref shorter than hyp.
  {
    const stats = wordErrorStats(norm("a b c"), norm("a b x c"));
    assert(approxEq(stats.wer, 1 / 3), `1-ins/3-words expected 0.3333 got ${stats.wer}`);
    assert(stats.ins === 1 && stats.subs === 0 && stats.dels === 0, "expected exactly 1 insertion");
  }

  // 3. Deletion-only: hyp drops a word.
  {
    const stats = wordErrorStats(norm("a b c d"), norm("a b d"));
    assert(approxEq(stats.wer, 0.25), `1-del/4-words expected 0.25 got ${stats.wer}`);
    assert(stats.dels === 1 && stats.subs === 0 && stats.ins === 0, "expected exactly 1 deletion");
  }

  // 4. Perfect match after normalization (case/punct-insensitive).
  {
    const stats = wordErrorStats(norm("Hello, World!"), norm("hello world"));
    assert(approxEq(stats.wer, 0), `expected 0 WER got ${stats.wer}`);
    assert(stats.edits === 0, "expected 0 edits");
  }

  // 5. Empty hypothesis against non-empty reference = 1.0 WER (all deletions).
  {
    const stats = wordErrorStats(norm("a b c"), norm(""));
    assert(approxEq(stats.wer, 1), `empty hyp expected WER 1.0 got ${stats.wer}`);
    assert(stats.dels === 3, "expected 3 deletions");
  }

  // 6. `<|tag|>` marker stripping (matches unified_parakeet_gate.py's TAG regex,
  // e.g. Whisper-style special tokens like <|nospeech|>) — not GigaSpeech's
  // <COMMA>-style tags, which that harness's TAG regex does not match.
  {
    const stats = wordErrorStats(norm("i went home <|nospeech|> then slept"), norm("i went home then slept"));
    assert(approxEq(stats.wer, 0), `tag-stripped ref/hyp expected 0 WER got ${stats.wer}`);
  }

  // 7. Full pipeline: scoreCorpus over synthetic hypotheses.jsonl + id_to_truth.json.
  // Uses the ACTUAL production id_to_truth.json schema
  // ({clip_id, ground_truth, group, duration_s} per Lane B's confirmed shape,
  // deliberately named after the isolation gate's forbidden keys), not the
  // {clip, gt} aliases, to prove the tolerant field lookup reads the real file.
  {
    const idToTruth = {
      row_a: { clip_id: "a.wav", ground_truth: "the quick brown fox", group: "scripts_md", duration_s: 5 },
      row_b: { clip_id: "b.wav", ground_truth: "jumps over the lazy dog", group: "librispeech", duration_s: 20 },
      row_c: { clip_id: "c.wav", ground_truth: "a completely separate sentence here", group: "librispeech", duration_s: 45 },
      row_d: { clip_id: "d.wav", ground_truth: "final long form clip over ninety seconds long", group: "ami", duration_s: 95 },
    };
    const hypRows = [
      { id: "row_a", hypothesis: "the quick brown fox", decode_s: 0.1 }, // 0 errors / 4 words
      { id: "row_b", hypothesis: "jumps over a lazy dog", decode_s: 0.2 }, // 1 sub / 5 words
      { id: "row_c", hypothesis: "a completely separate sentence here", decode_s: 0.3 }, // 0 errors / 5 words
      { id: "row_d", hypothesis: "final long form clip over ninety seconds long", decode_s: 0.4 }, // 0 errors / 8 words
    ];
    const result = scoreCorpus(hypRows, idToTruth);
    assert(result.completion_count === 4, `expected 4 rows scored got ${result.completion_count}`);
    assert(result.corpus.errors === 1, `expected 1 total error got ${result.corpus.errors}`);
    assert(result.corpus.words === 22, `expected 22 total ref words got ${result.corpus.words}`);
    assert(approxEq(result.corpus.corpus_wer, 1 / 22, 1e-4), `expected corpus WER ~1/22 got ${result.corpus.corpus_wer}`);
    assert(result.by_group.scripts_md.errors === 0, "scripts_md group should have 0 errors");
    assert(result.by_duration_bucket["<=15s"].n === 1, "expected 1 row in <=15s bucket");
    assert(result.by_duration_bucket["15-30s"].n === 1, "expected 1 row in 15-30s bucket");
    assert(result.by_duration_bucket["30-90s"].n === 1, "expected 1 row in 30-90s bucket");
    assert(result.by_duration_bucket[">90s"].n === 1, "expected 1 row in >90s bucket");
    assert(result.missing_truth_ids.length === 0, "expected no missing-truth ids");
    assert(result.missing_hypothesis_ids.length === 0, "expected no missing-hypothesis ids");
  }

  // 8. Missing-row / availability-failure accounting.
  {
    const idToTruth = {
      row_x: { clip: "x.wav", gt: "one two three", group: "ami", duration_s: 3 },
      row_y: { clip: "y.wav", gt: "four five six", group: "ami", duration_s: 3 },
    };
    const hypRows = [{ id: "row_x", hypothesis: "one two three", decode_s: 0.1 }];
    const result = scoreCorpus(hypRows, idToTruth);
    assert(result.completion_count === 1, "expected 1 scored row");
    assert(result.expected_count === 2, "expected 2 truth rows");
    assert(result.missing_hypothesis_ids.includes("row_y"), "row_y should be reported missing");
  }

  // 8b. Exact-term recovery over one group, with caller-supplied terms.
  {
    const idToTruth = {
      row_p: { clip_id: "terms_001.wav", ground_truth: "Please email the invoice with the new logo to Northwind today", group: "terms", duration_s: 10 },
      row_q: { clip_id: "terms_002.wav", ground_truth: "The kerning looks off on the Contoso landing page", group: "terms", duration_s: 10 },
      row_r: { clip_id: "other_001.wav", ground_truth: "invoice", group: "other", duration_s: 2 },
    };
    const hypRows = [
      // recovers "invoice" and "Northwind" but drops "logo" (mis-heard as "log")
      { id: "row_p", hypothesis: "please email the invoice with the new log to northwind today" },
      // drops "kerning" (mis-heard as "colouring") but recovers Contoso
      { id: "row_q", hypothesis: "the colouring looks off on the contoso landing page" },
      { id: "row_r", hypothesis: "in voice" },
    ];
    const terms = ["invoice", "logo", "northwind", "contoso", "kerning"];
    const result = scoreCorpus(hypRows, idToTruth, { terms, termGroup: "terms" });
    const rec = result.exact_term_recovery;
    assert(rec.invoice.ref_occurrences === 1 && rec.invoice.recovered === 1, "invoice should be recovered (other group not scanned)");
    assert(rec.logo.ref_occurrences === 1 && rec.logo.recovered === 0 && rec.logo.missed === 1, "logo should be missed");
    assert(rec.northwind.recovered === 1 && rec.contoso.recovered === 1, "northwind and contoso should be recovered");
    assert(rec.kerning.ref_occurrences === 1 && rec.kerning.missed === 1, "kerning should be missed");
    assert(rec.logo.missed_clips.includes("terms_001.wav"), "logo miss should be attributed to terms_001.wav");
    const all = scoreCorpus(hypRows, idToTruth, { terms: ["invoice"] });
    assert(all.exact_term_recovery.invoice.ref_occurrences === 2 && all.exact_term_recovery.invoice.missed === 1, "no group scans every row");
    assert(Object.keys(scoreCorpus(hypRows, idToTruth).exact_term_recovery).length === 0, "no terms: nothing scanned");
  }

  // 9. Header sanity: the scorer hashes its own source.
  {
    const header = scorerHeader();
    assert(typeof header.scorer_src_sha256 === "string" && header.scorer_src_sha256.length === 64, "scorer_src_sha256 must be a sha256 hex digest");
    assert(header.wer_dep === "custom-levenshtein-port", "wer_dep must record the actual dependency used");
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(fs.realpathSync(process.argv[1])).href) {
  process.exit(runCli());
}
