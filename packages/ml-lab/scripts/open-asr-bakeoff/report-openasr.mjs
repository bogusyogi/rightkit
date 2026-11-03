#!/usr/bin/env node
// report-openasr.mjs — merge per-model scored/status/env outputs into the
// packet's six required result tables + disposition. Read-only over out/<key>/;
// writes report-openasr.{json,md}. Missing outputs are marked "pending", never
// fabricated. Advance criterion is applied exactly as the run packet defines it.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const OUT = path.join(__dirname, "out");
const LEDGER = path.join(__dirname, "acquire", "acquisition-ledger.json");
const CONTRACT = path.join(__dirname, "run-contract.json");

// packet advance thresholds
const WER_PARITY_ABS_PP = 0.5;      // within 0.5pp of fresh Unified …
const SCRIPTS_MD_MIN_GAIN_PP = 2.0; // … only if scripts_md improves >= 2.0pp
const CONTROL_KEY = "control-unified";

const CANDIDATES = [
  "ark-asr-0.6b", "qwen3-asr-1.7b", "cohere-transcribe", "higgs-audio-v3-stt",
  "granite-speech-4.1-2b", "moss-transcribe-2b", "canary-qwen-2.5b", "ark-asr-3b",
];

function readJsonSafe(p) {
  try { return JSON.parse(fs.readFileSync(p, "utf8")); } catch { return null; }
}
function pct(x) { return x == null ? null : Math.round(x * 1000000) / 10000; } // 0.0628 -> 6.28
function ppDelta(cand, base) { return cand == null || base == null ? null : Math.round((cand - base) * 1000000) / 10000; }

// prefer full-corpus scored.json; fall back to smoke_scored.json (flagged)
function loadScored(key) {
  const full = readJsonSafe(path.join(OUT, key, "scored.json"));
  if (full) return { scored: full, scope: "full" };
  const smoke = readJsonSafe(path.join(OUT, key, "smoke_scored.json"));
  if (smoke) return { scored: smoke, scope: "smoke" };
  return { scored: null, scope: "pending" };
}

function modelMeta(key, ledger) {
  const m = ledger?.models?.[key] ?? {};
  return {
    revision: m.resolved_commit ?? m.requested_revision ?? null,
    license: m.license_name ?? null,
    artifact_bytes: m.total_bytes ?? null,
    download_status: m.status ?? null,
  };
}

function assemble() {
  const ledger = readJsonSafe(LEDGER);
  const contract = readJsonSafe(CONTRACT);
  const preflight = contract?.adapter_preflight_8gb ?? {};

  // fresh Unified control = the only comparator authority
  const control = loadScored(CONTROL_KEY).scored;
  const baseCorpusWer = control?.corpus?.corpus_wer ?? null;
  const baseScriptsMd = control?.by_group?.scripts_md?.corpus_wer ?? null;

  const rows = [];
  for (const key of CANDIDATES) {
    const { scored, scope } = loadScored(key);
    const status = readJsonSafe(path.join(OUT, key, "status.json"));
    const env = readJsonSafe(path.join(OUT, key, "env.json"));
    const meta = modelMeta(key, ledger);
    const pf = preflight[key] ?? null;

    if (!scored) {
      rows.push({ key, scope, disposition: "pending", ...meta, preflight_8gb: pf, reason: "no output yet" });
      continue;
    }
    const corpusWer = scored.corpus?.corpus_wer ?? null;
    const scriptsMd = scored.by_group?.scripts_md?.corpus_wer ?? null;
    const lat = scored.corpus?.latency ?? {};
    const completion = status?.completion ?? null;
    const rowsDone = status?.rows_done ?? scored.completion_count ?? null;
    const rowsTotal = status?.rows_total ?? scored.expected_count ?? null;
    const truncated = (scored.rows ?? []).filter((r) => r.status === "truncation").length;
    const hardFail = ["oom", "crash"].includes(completion) || (completion === "empty");

    // advance criterion (only meaningful on a full run)
    let disposition, reason;
    const license = (meta.license ?? "").toLowerCase();
    const nonRedist = /non-?commercial|no-?deriv|research/.test(license);
    if (scope !== "full") {
      disposition = "smoke-only";
      reason = "5-clip smoke only; full-224 pending";
    } else if (hardFail || (rowsTotal && rowsDone && rowsDone < rowsTotal)) {
      disposition = "failed"; reason = `availability: completion=${completion}, ${rowsDone}/${rowsTotal}`;
    } else if (pf === "defer") {
      disposition = "host-incompatible"; reason = "preflight defer on 8GB; reference-host only";
    } else {
      const beatsBase = baseCorpusWer != null && corpusWer != null && corpusWer <= baseCorpusWer + 1e-9;
      const within = baseCorpusWer != null && corpusWer != null && (corpusWer - baseCorpusWer) <= (WER_PARITY_ABS_PP / 100) + 1e-9;
      const smGain = baseScriptsMd != null && scriptsMd != null ? (baseScriptsMd - scriptsMd) * 100 : null;
      const smQualifies = smGain != null && smGain >= SCRIPTS_MD_MIN_GAIN_PP;
      const qualityPass = beatsBase || (within && smQualifies);
      if (qualityPass && !nonRedist && truncated === 0) {
        disposition = "product-candidate";
        reason = beatsBase ? `WER ${pct(corpusWer)}% <= Unified ${pct(baseCorpusWer)}%` : `within ${WER_PARITY_ABS_PP}pp + scripts_md +${smGain?.toFixed(2)}pp`;
      } else if (qualityPass && nonRedist) {
        disposition = "accuracy-ceiling"; reason = `meets WER but license '${meta.license}' not redistributable`;
      } else if (truncated > 0) {
        disposition = "failed"; reason = `${truncated} clip(s) truncated (long-audio cap)`;
      } else {
        disposition = "below-bar"; reason = `WER ${pct(corpusWer)}% vs Unified ${pct(baseCorpusWer)}%${smGain != null ? `, scripts_md ${smGain >= 0 ? "+" : ""}${smGain.toFixed(2)}pp` : ""}`;
      }
    }

    rows.push({
      key, scope, ...meta, preflight_8gb: pf,
      corpus_wer_pct: pct(corpusWer), mean_clip_wer_pct: pct(scored.corpus?.mean_clip_wer),
      scripts_md_wer_pct: pct(scriptsMd), scripts_md_delta_pp: ppDelta(scriptsMd, baseScriptsMd),
      exact_term_recovery: scored.scripts_md_exact_term_recovery ?? null,
      latency: { mean_s: lat.mean_s ?? null, p50_s: lat.p50_s ?? null, p95_s: lat.p95_s ?? null, rtfx: lat.rtfx ?? null },
      cold_load_s: status?.cold_load_s ?? null, peak_vram_mb: status?.peak_vram_mb ?? null,
      completion, rows_done: rowsDone, rows_total: rowsTotal, truncated,
      env: env ? { framework: env.framework, dtype: env.dtype, device: env.device, host_class: env.host_class } : null,
      disposition, reason,
    });
  }

  return {
    generated_at: new Date().toISOString(),
    comparator: { key: CONTROL_KEY, corpus_wer_pct: pct(baseCorpusWer), scripts_md_wer_pct: pct(baseScriptsMd), scope: control ? "full" : "pending" },
    advance_criterion: { wer_parity_abs_pp: WER_PARITY_ABS_PP, scripts_md_min_gain_pp: SCRIPTS_MD_MIN_GAIN_PP },
    rows,
  };
}

function toMarkdown(rep) {
  const L = [];
  L.push(`# Open-ASR bakeoff results — ${rep.generated_at}`);
  L.push(`\nComparator (fresh Unified): **${rep.comparator.corpus_wer_pct ?? "pending"}%** corpus, **${rep.comparator.scripts_md_wer_pct ?? "pending"}%** scripts_md.`);
  L.push(`Advance: reach Unified WER, or within ${rep.advance_criterion.wer_parity_abs_pp}pp while scripts_md improves ≥${rep.advance_criterion.scripts_md_min_gain_pp}pp; redistributable; no truncation.\n`);
  L.push(`## 1. Main rank`);
  L.push(`| model | scope | corpus WER% | scripts_md% | Δpp | RTFx | p95 s | cold s | VRAM MB | done | disposition |`);
  L.push(`|---|---|--:|--:|--:|--:|--:|--:|--:|--:|---|`);
  const order = [...rep.rows].sort((a, b) => (a.corpus_wer_pct ?? 999) - (b.corpus_wer_pct ?? 999));
  for (const r of order) {
    L.push(`| ${r.key} | ${r.scope} | ${r.corpus_wer_pct ?? "—"} | ${r.scripts_md_wer_pct ?? "—"} | ${r.scripts_md_delta_pp ?? "—"} | ${r.latency?.rtfx ?? "—"} | ${r.latency?.p95_s ?? "—"} | ${r.cold_load_s ?? "—"} | ${r.peak_vram_mb ?? "—"} | ${r.rows_done ?? "—"}/${r.rows_total ?? "—"} | ${r.disposition} |`);
  }
  L.push(`\n## 2. Disposition`);
  for (const r of rep.rows) L.push(`- **${r.key}** → ${r.disposition} — ${r.reason}`);
  return L.join("\n") + "\n";
}

function main() {
  const rep = assemble();
  fs.writeFileSync(path.join(__dirname, "report-openasr.json"), JSON.stringify(rep, null, 2));
  fs.writeFileSync(path.join(__dirname, "report-openasr.md"), toMarkdown(rep));
  const done = rep.rows.filter((r) => r.scope === "full").length;
  const pend = rep.rows.filter((r) => r.scope === "pending").length;
  console.log(`report-openasr: ${rep.rows.length} models (${done} full, ${pend} pending); comparator=${rep.comparator.corpus_wer_pct ?? "pending"}%`);
  console.log(`wrote report-openasr.{json,md}`);
}

main();
