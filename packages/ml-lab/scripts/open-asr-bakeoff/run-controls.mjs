#!/usr/bin/env node
/**
 * Fresh HeardRight control-rerun harness for the Open-ASR bakeoff
 * (docs/plans/2026-07-17-open-asr-top-model-dispatch-run-packet.md,
 * "Fresh HeardRight controls" + Batch 0).
 *
 * Locates and (once approved) invokes the shipped Unified, TDT, and Whisper
 * engines on the SAME conditioned corpus bytes candidates receive, emitting
 * hypotheses.jsonl in the shared Lane B/E/F/G schema under:
 *   out/control-unified/hypotheses.jsonl
 *   out/control-tdt/hypotheses.jsonl
 *   out/control-whisper/hypotheses.jsonl
 *
 * --dry-run (default): verifies wiring only.
 *   1. Resolves each engine's shipped model_registry artifact and checks it
 *      exists on disk.
 *   2. Resolves the best-available CLI runner candidate for each engine and
 *      reports whether it is production-parity, a partial proxy, or not
 *      located, with the exact command that WOULD run.
 *   3. Exercises the full hypotheses.jsonl write path end-to-end with an
 *      in-process STUB transcriber (no model load, no subprocess) over a
 *      handful of REAL candidate_manifest rows, so the output schema, id
 *      join, and file-write mechanics are proven without running inference.
 *   4. Writes wiring-report.json summarizing all of the above.
 *
 * --live: gated. Requires --approve <token> matching the packet's exact
 *   approval token (APPROVE_OPEN_ASR_TOP_MODELS_V1) AND a resolved
 *   production-parity command for the requested engine(s). As of this run,
 *   no engine has a confirmed production-parity Windows CLI runner (see
 *   resolveEngine() below), so --live currently refuses for every engine.
 *   This is intentional: the task authorizes wiring only, not execution.
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const BAKEOFF_DIR = __dirname; // tauri-app-next/scripts/open-asr-bakeoff
const SCRIPTS_DIR = path.resolve(BAKEOFF_DIR, ".."); // tauri-app-next/scripts
const APP_ROOT = path.resolve(SCRIPTS_DIR, ".."); // tauri-app-next
const REPO_ROOT = path.resolve(APP_ROOT, ".."); // D:/Claude/heardright

const MODEL_REGISTRY_ASR = process.env.RIGHTKIT_ML_MODEL_REGISTRY_ASR
  ? path.resolve(process.env.RIGHTKIT_ML_MODEL_REGISTRY_ASR)
  : path.join(REPO_ROOT, "model_registry", "final", "asr");
const APPROVAL_TOKEN = "APPROVE_OPEN_ASR_TOP_MODELS_V1";

const DEFAULT_CORPUS_DIR = path.join(BAKEOFF_DIR, "canonical-eval", "candidate_audio");
const DEFAULT_MANIFEST = path.join(BAKEOFF_DIR, "canonical-eval", "candidate_manifest.json");
const DEFAULT_OUT_DIR = path.join(BAKEOFF_DIR, "out");

function exists(p) {
  try {
    fs.accessSync(p);
    return true;
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------------------
// Engine resolution — every path/claim below was verified against this repo
// on 2026-07-17 (docs/UNIFIED_ASR_LOCK_2026-06-27.md, model_registry/final/asr/
// on disk, and the runner scripts under tauri-app-next/scripts/). Nothing
// here is invented; gaps are reported as gaps, not papered over.
// ---------------------------------------------------------------------------
function resolveEngine(name) {
  if (name === "unified") {
    const modelDir = path.join(MODEL_REGISTRY_ASR, "unified_static15_b128_sym_bits4_timestamp_hybrid");
    const proxyScript = path.join(SCRIPTS_DIR, "unified_parakeet_gate.py");
    return {
      name,
      model_path: modelDir,
      model_path_exists: exists(modelDir),
      ship_policy: "padded_window (15 s window, 2.24 s trailing silence search, timestamp commit)",
      ship_policy_source: "heardright-inference/src/asr.rs::transcribe_padded_window (Rust; no Python/Node CLI equivalent found)",
      locked_reference_wer: "6.265% (docs/UNIFIED_ASR_LOCK_2026-06-27.md line 28, padded_window hard/no-buffer, Pico-224 no-Chime) — screening reference only, NOT an acceptance authority for this run",
      runner_status: "partial_proxy",
      runner_caveat:
        "unified_parakeet_gate.py loads ONNX encoder/decoder pairs from .asr_candidates/parakeet-unified-en-0.6b-onnx " +
        "(a separate candidate tree, not model_registry/final/asr) and only implements the 'hard' single 15s " +
        "static-window decode — it does NOT implement padded_window's trailing-silence search. It will not " +
        "reproduce the locked 6.265% figure. Closest available variant name by naming convention: " +
        "'static_weight4_b128_sym_bits4_enc_int8dec__enc_dml__dec_cpu' (unverified exact match — confirm against " +
        "unified_parakeet_hybrid_matrix_impl.py::variants() before --live).",
      candidate_command: (manifestPath, clipsDir) =>
        `python ${rel(proxyScript)} --mode wer --variants static_weight4_b128_sym_bits4_enc_int8dec__enc_dml__dec_cpu ` +
        `--manifest ${rel(manifestPath)} --clips-dir ${rel(clipsDir)}`,
      blocking_gap:
        "This legacy JS wrapper predates the production-parity native runner and does not invoke it. Use " +
        "heardright-engine/examples/openasr_controls.rs directly.",
    };
  }

  if (name === "tdt") {
    const modelDir = path.join(
      MODEL_REGISTRY_ASR,
      "parakeet_tdt_v3_static1500_qint8_20260722",
    );
    return {
      name,
      model_path: modelDir,
      model_path_exists: exists(modelDir),
      ship_policy: "TDT SW silence-cut hard/no-buffer (rollback/control lane; explicit parakeet-tdt alias)",
      ship_policy_source: "docs/UNIFIED_ASR_LOCK_2026-06-27.md line 46-47 (Production Wiring); Rust engine, no CLI eval script located",
      locked_reference_wer: "8.001% (docs/UNIFIED_ASR_LOCK_2026-06-27.md line 37, TDT SW silence-cut hard/no-buffer) — screening reference only",
      runner_status: "not_located",
      runner_caveat:
        "scripts/rnnt_finalist_wer_gate.py targets a DIFFERENT model (.asr_candidates/parakeet-rnnt-0.6b, an older " +
        "plain-RNNT checkpoint) — it cannot load the locked compact TDT checkpoint and was not wired as a " +
        "proxy to avoid silently scoring the wrong model.",
      candidate_command: null,
      blocking_gap:
        "This legacy JS wrapper predates the production-parity native runner and does not invoke it. Use " +
        "heardright-engine/examples/openasr_controls.rs directly.",
    };
  }

  if (name === "whisper") {
    const modelFile = path.join(MODEL_REGISTRY_ASR, "whisper_turbo_q5_k.bin");
    return {
      name,
      model_path: modelFile,
      model_path_exists: exists(modelFile),
      ship_policy: "Pro/non-English path; Windows runtime in heardright-inference/src/whisper_win.rs (whisper.cpp-style GGUF q5_k quant)",
      ship_policy_source: "heardright-inference/src/whisper_win.rs; model_registry/final/asr/whisper_turbo_q5_k.bin",
      locked_reference_wer:
        "8.019% cited in the run packet's context block — NOT found in any committed doc/result file during this " +
        "wiring pass (docs/UNIFIED_ASR_LOCK_2026-06-27.md only documents Unified 6.265% and TDT 8.001%). Treat as " +
        "unverified per the packet's own instruction ('do not hardcode as authority') until a fresh run confirms it.",
      runner_status: "not_located",
      runner_caveat:
        "scripts/whisper_ct2_locked_clip_bakeoff.py targets CTranslate2 checkpoints (large-v3-turbo/distil-large-v3/" +
        "medium, float16/int8) — a different runtime and format from the shipped whisper.cpp-style GGUF q5_k binary. " +
        "Wiring it would score a different engine than production and was not attempted.",
      candidate_command: null,
      blocking_gap:
        "This legacy JS wrapper predates the production-parity native runner and does not invoke it. Use " +
        "heardright-engine/examples/openasr_controls.rs directly.",
    };
  }

  throw new Error(`unknown engine: ${name}`);
}

function rel(p) {
  return path.relative(APP_ROOT, p).replaceAll("\\", "/");
}

// ---------------------------------------------------------------------------
// Stub transcriber — proves the hypotheses.jsonl write path end-to-end
// without loading any model. Every stub row is flagged stub:true so it can
// never be mistaken for a real control result.
// ---------------------------------------------------------------------------
function stubTranscribe(_audioPath) {
  return { hypothesis: "", decode_s: 0, stub: true };
}

function writeHypothesesJsonl(outDir, rows) {
  fs.mkdirSync(outDir, { recursive: true });
  const file = path.join(outDir, "hypotheses.jsonl");
  const lines = rows.map((row) => JSON.stringify(row));
  fs.writeFileSync(file, lines.length ? lines.join("\n") + "\n" : "");
  return file;
}

function loadManifest(manifestPath, limit) {
  if (!exists(manifestPath)) return [];
  const rows = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
  return typeof limit === "number" ? rows.slice(0, limit) : rows;
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------
function parseArgs(argv) {
  const args = {
    mode: "dry-run",
    corpusDir: DEFAULT_CORPUS_DIR,
    manifest: DEFAULT_MANIFEST,
    outDir: DEFAULT_OUT_DIR,
    engines: ["unified", "tdt", "whisper"],
    approve: null,
    stubLimit: 5,
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--dry-run") args.mode = "dry-run";
    else if (a === "--live") args.mode = "live";
    else if (a === "--corpus-dir") args.corpusDir = argv[++i];
    else if (a === "--manifest") args.manifest = argv[++i];
    else if (a === "--out-dir") args.outDir = argv[++i];
    else if (a === "--engines") args.engines = argv[++i].split(",").map((s) => s.trim()).filter(Boolean);
    else if (a === "--approve") args.approve = argv[++i];
    else if (a === "--self-test") args.selfTest = true;
  }
  return args;
}

function dryRun(args) {
  console.log("open-asr-bakeoff run-controls --dry-run");
  console.log(`  corpus dir : ${args.corpusDir} (exists=${exists(args.corpusDir)})`);
  console.log(`  manifest   : ${args.manifest} (exists=${exists(args.manifest)})`);
  console.log(`  out dir    : ${args.outDir}`);

  const manifestRows = loadManifest(args.manifest, args.stubLimit);
  const corpusReady = exists(args.corpusDir) && exists(args.manifest) && manifestRows.length > 0;
  if (!corpusReady) {
    console.log(
      "  NOTE: conditioned corpus / candidate manifest not fully present — wiring resolution and stub write " +
        "still run below, but engine invocation naturally has nothing to score yet.",
    );
  }

  const report = {
    generated_at: new Date().toISOString(),
    mode: "dry-run",
    corpus_dir: args.corpusDir,
    corpus_dir_exists: exists(args.corpusDir),
    manifest: args.manifest,
    manifest_exists: exists(args.manifest),
    manifest_row_count_full: exists(args.manifest) ? JSON.parse(fs.readFileSync(args.manifest, "utf8")).length : 0,
    out_dir: args.outDir,
    engines: {},
  };

  for (const name of args.engines) {
    const engine = resolveEngine(name);
    const engineOutDir = path.join(args.outDir, `control-${name}`);
    const command = engine.candidate_command ? engine.candidate_command(args.manifest, args.corpusDir) : null;

    console.log(`\n[${name}]`);
    console.log(`  model path        : ${engine.model_path} (exists=${engine.model_path_exists})`);
    console.log(`  ship policy       : ${engine.ship_policy}`);
    console.log(`  runner status     : ${engine.runner_status}`);
    if (command) console.log(`  candidate command : ${command}`);
    if (engine.blocking_gap) console.log(`  blocking gap      : ${engine.blocking_gap}`);

    // Prove the write path with a stub transcriber over real candidate ids
    // (no model load; every row flagged stub:true).
    const stubRows = manifestRows.map((row) => {
      const out = stubTranscribe(row.path);
      return { id: row.id, hypothesis: out.hypothesis, decode_s: out.decode_s, stub: out.stub };
    });
    const hypFile = writeHypothesesJsonl(engineOutDir, stubRows);
    console.log(`  stub hypotheses   : ${hypFile} (${stubRows.length} row(s), stub:true)`);

    report.engines[name] = {
      model_path: engine.model_path,
      model_path_exists: engine.model_path_exists,
      ship_policy: engine.ship_policy,
      ship_policy_source: engine.ship_policy_source,
      locked_reference_wer: engine.locked_reference_wer,
      runner_status: engine.runner_status,
      runner_caveat: engine.runner_caveat,
      blocking_gap: engine.blocking_gap,
      candidate_command: command,
      stub_hypotheses_path: hypFile,
      stub_row_count: stubRows.length,
    };
  }

  fs.mkdirSync(args.outDir, { recursive: true });
  const reportPath = path.join(args.outDir, "wiring-report.json");
  fs.writeFileSync(reportPath, JSON.stringify(report, null, 2));
  console.log(`\nwrote ${reportPath}`);
  console.log("dry-run wiring verified: schema + id-join + file-write mechanics OK; no engine inference was run.");
  return 0;
}

function liveRun(args) {
  console.log("open-asr-bakeoff run-controls --live");
  if (args.approve !== APPROVAL_TOKEN) {
    console.error(
      `refused: --live requires --approve ${APPROVAL_TOKEN} per docs/plans/2026-07-17-open-asr-top-model-dispatch-run-packet.md ` +
        "('Approval and mutation guard'). This run packet has not been approved for provider/model execution.",
    );
    return 1;
  }
  let blocked = 0;
  for (const name of args.engines) {
    const engine = resolveEngine(name);
    if (engine.runner_status !== "production_parity") {
      console.error(
        `refused: [${name}] is not wired for production-parity execution in this legacy JS wrapper (status=${engine.runner_status}). ${engine.blocking_gap ?? engine.runner_caveat}`,
      );
      blocked += 1;
    }
  }
  if (blocked > 0) {
    console.error(
      `\n${blocked} engine(s) blocked. --live is not executed here; use the native openasr_controls runner directly.`,
    );
    return 1;
  }
  console.error("no engine reached execution — this branch is intentionally unreachable until a production-parity runner is wired.");
  return 1;
}

// ---------------------------------------------------------------------------
// Self-test — verifies wiring resolution + stub write path in an isolated
// scratch directory, independent of whether the real corpus/manifest exist.
// ---------------------------------------------------------------------------
function assert(cond, message) {
  if (!cond) throw new Error(`self-test failed: ${message}`);
}

function selfTest() {
  // 1. Engine resolution returns the expected shape for all three engines.
  for (const name of ["unified", "tdt", "whisper"]) {
    const engine = resolveEngine(name);
    assert(typeof engine.model_path === "string" && engine.model_path.length > 0, `${name}: model_path missing`);
    assert(["production_parity", "partial_proxy", "not_located"].includes(engine.runner_status), `${name}: invalid runner_status`);
  }

  // 2. The one current local product model exists. Unified is retired and
  // Whisper Pro is optional, so neither is required in a clean registry.
  const tdt = resolveEngine("tdt");
  assert(tdt.model_path_exists, "current TDT model_registry dir should exist on disk");

  // 3. Stub write path round-trips through a temp dir with synthetic rows.
  const scratchDir = fs.mkdtempSync(path.join(APP_ROOT, ".cache", "hr-run-controls-selftest-"));
  try {
    const rows = [
      { id: "aaaa1111", path: "aaaa1111.wav" },
      { id: "bbbb2222", path: "bbbb2222.wav" },
    ].map((row) => {
      const out = stubTranscribe(row.path);
      return { id: row.id, hypothesis: out.hypothesis, decode_s: out.decode_s, stub: out.stub };
    });
    const file = writeHypothesesJsonl(path.join(scratchDir, "control-unified"), rows);
    assert(exists(file), "stub hypotheses.jsonl was not written");
    const written = fs
      .readFileSync(file, "utf8")
      .trim()
      .split("\n")
      .map((line) => JSON.parse(line));
    assert(written.length === 2, "expected 2 stub rows written");
    assert(written.every((r) => r.stub === true), "every stub row must be flagged stub:true");
    assert(written[0].id === "aaaa1111" && written[1].id === "bbbb2222", "row ids must round-trip");
  } finally {
    fs.rmSync(scratchDir, { recursive: true, force: true });
  }

  // 4. --live refuses without the exact approval token.
  const refusedNoToken = liveRun({ engines: ["unified"], approve: null });
  assert(refusedNoToken === 1, "--live must refuse without the approval token");
  const refusedWrongToken = liveRun({ engines: ["unified"], approve: "not-the-token" });
  assert(refusedWrongToken === 1, "--live must refuse with a wrong approval token");
  const refusedNoRunner = liveRun({ engines: ["unified", "tdt", "whisper"], approve: APPROVAL_TOKEN });
  assert(refusedNoRunner === 1, "--live must refuse when no engine has a production-parity runner");
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.selfTest) {
    selfTest();
    console.log("run-controls self-test ok");
    return 0;
  }
  if (args.mode === "live") return liveRun(args);
  return dryRun(args);
}

process.exit(main());
