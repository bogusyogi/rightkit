#!/usr/bin/env node
import childProcess from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";
import {
  buildRequest,
  deterministicFlags,
  materializeOutput,
  sha256,
  stableJson,
} from "./contextual_repair_core.mjs";
import {
  acquireRunLock,
  clearActiveServer,
  reapActiveServer,
  recordActiveServer,
} from "./run_contextual_repair_overnight.mjs";

const scriptFile = fileURLToPath(import.meta.url);
const scriptDir = path.dirname(scriptFile);
const appRoot = path.resolve(scriptDir, "../..");
const productionRootDefault = "E:\\models\\parakeet-v3-cache-aware-nemo-20260720\\production-pipeline-v1";
const modelRootDefault = "E:\\models\\heardright-local-screening";
const runRootDefault = "E:\\models\\heardright-local-screening\\scheduled-static15-local-llm-selective-v1";
const sourceRecipesDefault = path.join(appRoot, "bakeoff", "l1-local-contextual-repair-v1", "candidate-recipes.json");
const configDefault = path.join(appRoot, "bakeoff", "l1-local-contextual-repair-v1", "scheduled-static15-selective-retest.json");
const llamaServerDefault = path.join(modelRootDefault, "runtimes", "llama.cpp", "b10069", "cuda-12.4", "llama-server.exe");
const ARMS = Object.freeze(["cold", "warm_final", "prefix_prefill"]);
const SCHEDULED_CHUNK_ARM = "scheduled_chunk_repair";
const VALID_ARMS = Object.freeze([...ARMS, SCHEDULED_CHUNK_ARM]);
const SUBSTRING_EDITS_SCHEMA = Object.freeze({
  type: "array",
  maxItems: 16,
  items: {
    type: "object",
    properties: {
      old: { type: "string", minLength: 1 },
      new: { type: "string" },
    },
    required: ["old", "new"],
    additionalProperties: false,
  },
});

function valueAfter(flag, fallback = null) {
  const index = process.argv.indexOf(flag);
  return index === -1 ? fallback : process.argv[index + 1];
}

function listAfter(flag, fallback = "") {
  return String(valueAfter(flag, fallback)).split(/[\s,]+/).map((value) => value.trim()).filter(Boolean);
}

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function readJsonl(file) {
  if (!fs.existsSync(file)) return [];
  return fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean).map(JSON.parse);
}

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const temporary = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(temporary, `${JSON.stringify(value, null, 2)}\n`);
  fs.renameSync(temporary, file);
}

function writeJsonl(file, rows) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const temporary = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(temporary, rows.length ? `${rows.map(JSON.stringify).join("\n")}\n` : "");
  fs.renameSync(temporary, file);
}

function appendJsonl(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const descriptor = fs.openSync(file, "a");
  try {
    fs.writeSync(descriptor, `${JSON.stringify(value)}\n`);
    fs.fsyncSync(descriptor);
  } finally {
    fs.closeSync(descriptor);
  }
}

function fileHash(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function normalizeWords(value) {
  return String(value ?? "")
    .toLocaleLowerCase("en-US")
    .replace(/[^\p{L}\p{N}']+/gu, " ")
    .trim()
    .split(/\s+/)
    .filter(Boolean);
}

function editDistance(left, right) {
  let prior = Array.from({ length: right.length + 1 }, (_, index) => index);
  for (let i = 1; i <= left.length; i += 1) {
    const current = [i];
    for (let j = 1; j <= right.length; j += 1) {
      current[j] = Math.min(
        prior[j] + 1,
        current[j - 1] + 1,
        prior[j - 1] + (left[i - 1] === right[j - 1] ? 0 : 1),
      );
    }
    prior = current;
  }
  return prior[right.length];
}

export function wordErrorRate(reference, hypothesis) {
  const expected = normalizeWords(reference);
  const actual = normalizeWords(hypothesis);
  return expected.length ? editDistance(expected, actual) / expected.length : (actual.length ? 1 : 0);
}

function tokenF1(reference, hypothesis) {
  const expected = normalizeWords(reference);
  const actual = normalizeWords(hypothesis);
  if (!expected.length && !actual.length) return 1;
  if (!expected.length || !actual.length) return 0;
  const counts = new Map();
  for (const token of expected) counts.set(token, (counts.get(token) ?? 0) + 1);
  let overlap = 0;
  for (const token of actual) {
    const available = counts.get(token) ?? 0;
    if (available > 0) {
      overlap += 1;
      counts.set(token, available - 1);
    }
  }
  const precision = overlap / actual.length;
  const recall = overlap / expected.length;
  return precision + recall ? 2 * precision * recall / (precision + recall) : 0;
}

export function percentile(values, ratio) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const rank = Math.max(1, Math.ceil(ratio * sorted.length));
  return sorted[Math.min(rank - 1, sorted.length - 1)];
}

export function longestCommonPrefix(left, right) {
  const limit = Math.min(left.length, right.length);
  let index = 0;
  while (index < limit && left[index] === right[index]) index += 1;
  return left.slice(0, index);
}

export function durationQuantileSample(rows, count) {
  if (!Number.isInteger(count) || count < 2) throw new Error("sample count must be at least two");
  if (rows.length < count) throw new Error(`cannot select ${count} rows from ${rows.length}`);
  const sorted = [...rows].sort((a, b) => a.duration_s - b.duration_s || String(a.id).localeCompare(String(b.id)));
  const selected = [];
  const used = new Set();
  for (let index = 0; index < count; index += 1) {
    let position = Math.round(index * (sorted.length - 1) / (count - 1));
    while (used.has(position) && position + 1 < sorted.length) position += 1;
    while (used.has(position) && position > 0) position -= 1;
    used.add(position);
    selected.push(sorted[position]);
  }
  return selected.sort((a, b) => a.duration_s - b.duration_s || String(a.id).localeCompare(String(b.id)));
}

function conservativeWordPrefix(text, approximateCharacterCount) {
  const safeTarget = Math.max(1, approximateCharacterCount - 16);
  let end = Math.min(text.length, safeTarget);
  while (end > 0 && !/\s/.test(text[end])) end -= 1;
  return text.slice(0, Math.max(1, end)).trim();
}

export function stableScheduledPrefixes(transcript, row) {
  const windows = Number(row.scheduled_background_windows ?? 0);
  if (!Number.isInteger(windows) || windows <= 0 || !transcript.trim()) return [];
  const duration = Number(row.duration_s);
  const tail = Number(row.tail_audio_s);
  const committedRatio = duration > 0 ? Math.max(0, Math.min(1, (duration - tail) / duration)) : 0;
  const prefixes = [];
  for (let index = 1; index <= windows; index += 1) {
    const ratio = committedRatio * index / windows;
    const prefix = conservativeWordPrefix(transcript, Math.floor(transcript.length * ratio));
    if (prefix && prefix !== prefixes.at(-1)) prefixes.push(prefix);
  }
  return prefixes;
}

export function scheduledTranscriptChunks(transcript, row) {
  const source = String(transcript ?? "").trim();
  if (!source) return [];
  const words = source.match(/\S+/g) ?? [];
  const windows = Number(row.scheduled_background_windows ?? 0);
  const duration = Number(row.duration_s);
  const tail = Number(row.tail_audio_s);
  if (!Number.isInteger(windows) || windows <= 0 || !(duration > 0)) return [source];
  const committedRatio = Math.max(0, Math.min(1, (duration - tail) / duration));
  const boundaries = [];
  for (let index = 1; index <= windows; index += 1) {
    const boundary = Math.floor(words.length * committedRatio * index / windows);
    if (boundary > (boundaries.at(-1) ?? 0) && boundary < words.length) boundaries.push(boundary);
  }
  const chunks = [];
  let cursor = 0;
  for (const boundary of boundaries) {
    chunks.push(words.slice(cursor, boundary).join(" "));
    cursor = boundary;
  }
  if (cursor < words.length) chunks.push(words.slice(cursor).join(" "));
  return chunks;
}

export function appendWithOverlap(buffer, next, maxCharacters = 16) {
  const left = String(buffer ?? "");
  const right = String(next ?? "");
  if (!right) return left;
  if (!left) return right;
  const maximum = Math.min(left.length, right.length, maxCharacters);
  let overlap = 0;
  for (let length = 1; length <= maximum; length += 1) {
    if (left.slice(-length).toLocaleLowerCase("en-US") === right.slice(0, length).toLocaleLowerCase("en-US")) overlap = length;
  }
  const remainder = right.slice(overlap);
  if (!remainder) return left;
  const needsSpace = overlap === 0 && !/\s$/u.test(left) && !/^\s/u.test(remainder);
  return `${left}${needsSpace ? " " : ""}${remainder}`;
}

export function cloudReferenceOutput(row) {
  return typeof row?.hypothesis === "string" ? row.hypothesis : null;
}

export function scheduledFieldText(recipe, assembled) {
  return recipe?.scheduled_context_policy === "none" ? "" : String(assembled ?? "").slice(-500);
}

function options() {
  return {
    productionRoot: path.resolve(valueAfter("--production-root", productionRootDefault)),
    modelRoot: path.resolve(valueAfter("--model-root", modelRootDefault)),
    runRoot: path.resolve(valueAfter("--run-root", runRootDefault)),
    sourceRecipes: path.resolve(valueAfter("--source-recipes", sourceRecipesDefault)),
    config: path.resolve(valueAfter("--config", configDefault)),
    llamaServer: path.resolve(valueAfter("--llama-server", llamaServerDefault)),
    pass: valueAfter("--pass", "pass1"),
    candidates: listAfter("--candidates"),
    arms: listAfter("--arms", ""),
    limit: Number(valueAfter("--limit", "0")),
  };
}

function modelManifest(modelRoot) {
  const file = path.join(scriptDir, "manifest.json");
  const registry = readJson(file);
  return new Map(registry.models.map((item) => [item.id, {
    ...item,
    model_path: path.join(modelRoot, "models", item.id, item.filename),
  }]));
}

function selectedRecipeSnapshot(sourceRecipes, config, pass) {
  const candidates = [];
  for (const choice of config.candidates) {
    const sourceCandidate = sourceRecipes.candidates.find((item) => item.id === choice.id);
    if (!sourceCandidate) throw new Error(`source recipe candidate is missing: ${choice.id}`);
    const recipe = pass === "pass1"
      ? sourceCandidate.recipes.find((item) => item.id === choice.pass1_recipe_id)
      : choice.pass2_recipe;
    if (!recipe) {
      if (pass === "pass2") continue;
      throw new Error(`pass-one recipe is missing: ${choice.pass1_recipe_id}`);
    }
    candidates.push({
      ...sourceCandidate,
      recipes: [{ ...recipe }],
    });
  }
  return {
    schema: "heardright.scheduled_static15_selective_recipes.v1",
    run_id: config.run_id,
    pass,
    candidates,
  };
}

function validateModelFiles(snapshot, registry) {
  const models = [];
  for (const candidate of snapshot.candidates) {
    const item = registry.get(candidate.id);
    if (!item) throw new Error(`model registry entry missing: ${candidate.id}`);
    if (!fs.existsSync(item.model_path)) throw new Error(`model file missing: ${item.model_path}`);
    const bytes = fs.statSync(item.model_path).size;
    if (item.expected_bytes && bytes !== item.expected_bytes) throw new Error(`model size mismatch: ${candidate.id}`);
    models.push({
      candidate_id: candidate.id,
      model_path: item.model_path,
      model_bytes: bytes,
      model_sha256: fileHash(item.model_path),
    });
  }
  return models;
}

function prepare(opts, pass = "pass1") {
  const config = readJson(opts.config);
  if (config.sample_count !== 15) throw new Error("selective run is frozen to 15 cases");
  const sourceRecipes = readJson(opts.sourceRecipes);
  const snapshot = selectedRecipeSnapshot(sourceRecipes, config, pass);
  if (!snapshot.candidates.length) throw new Error(`${pass} has no configured candidates`);
  const registry = modelManifest(opts.modelRoot);
  const models = validateModelFiles(snapshot, registry);
  fs.mkdirSync(opts.runRoot, { recursive: true });

  if (pass === "pass1") {
    const l0File = path.join(opts.productionRoot, "canonical-product-l0.jsonl");
    const l1File = path.join(opts.productionRoot, "canonical-product-l1.jsonl");
    const scoreFile = path.join(opts.productionRoot, "score-product-l1.json");
    const l0Rows = readJsonl(l0File);
    const l1Rows = readJsonl(l1File);
    const exactRoster = new Set(l1Rows.map((row) => row.id));
    const selected = durationQuantileSample(l0Rows.filter((row) => exactRoster.has(row.id)), config.sample_count);
    const visibleCases = selected.map((row) => ({
      case_id: row.id,
      transcript: row.hypothesis,
      context: {
        app: "ChatGPT.exe",
        window: "ChatGPT",
        kind: "AI assistant chat",
        writing: "English (India). Use only for spelling and punctuation; do not invent regional facts.",
        field_text: "",
        selected_text: "",
        vocabulary: "",
      },
      duration_s: row.duration_s,
      scheduled_background_windows: row.scheduled_background_windows,
      tail_audio_s: row.tail_audio_s,
      simulated_stop_tail_ms: row.simulated_stop_tail_ms,
      input_sha256: sha256(stableJson({ transcript: row.hypothesis, duration_s: row.duration_s })),
    }));
    writeJsonl(path.join(opts.runRoot, "candidate-visible", "cases.jsonl"), visibleCases);

    const l1ById = new Map(l1Rows.map((row) => [row.id, row]));
    const truthById = new Map(readJson(scoreFile).rows.map((row) => [row.id, row]));
    const scorerRows = visibleCases.map((row) => {
      const cloud = l1ById.get(row.case_id);
      const truth = truthById.get(row.case_id);
      if (!truth) throw new Error(`truth row missing: ${row.case_id}`);
      return {
        case_id: row.case_id,
        truth_normalized: truth.ref_norm,
        source_l0_normalized: truth.hyp_norm,
        cloud_status: cloud?.status ?? null,
        cloud_output: cloudReferenceOutput(cloud),
        cloud_cleanup_ms: cloud?.l1_polish_ms ?? null,
      };
    });
    writeJsonl(path.join(opts.runRoot, "private", "scorer.jsonl"), scorerRows);
    writeJson(path.join(opts.runRoot, "private", "sources.json"), {
      l0_file: l0File,
      l0_sha256: fileHash(l0File),
      l1_file: l1File,
      l1_sha256: fileHash(l1File),
      score_file: scoreFile,
      score_sha256: fileHash(scoreFile),
      selected_case_ids: visibleCases.map((row) => row.case_id),
    });
  } else if (!fs.existsSync(path.join(opts.runRoot, "candidate-visible", "cases.jsonl"))) {
    throw new Error("prepare pass1 before pass2");
  }

  const recipeFile = path.join(opts.runRoot, "candidate-visible", `recipes.${pass}.json`);
  writeJson(recipeFile, snapshot);
  const control = {
    schema: "heardright.scheduled_static15_selective_control.v1",
    run_id: config.run_id,
    pass,
    created_at: new Date().toISOString(),
    node_version: process.version,
    config_file: opts.config,
    config_sha256: fileHash(opts.config),
    source_recipes_sha256: fileHash(opts.sourceRecipes),
    recipes_sha256: fileHash(recipeFile),
    runner_sha256: fileHash(scriptFile),
    llama_server: opts.llamaServer,
    llama_server_sha256: fileHash(opts.llamaServer),
    models,
  };
  writeJson(path.join(opts.runRoot, "private", `control.${pass}.json`), control);
  console.log(JSON.stringify({ status: `${pass}_prepared`, run_root: opts.runRoot, candidates: snapshot.candidates.length, cases: readJsonl(path.join(opts.runRoot, "candidate-visible", "cases.jsonl")).length }, null, 2));
}

function validatePrepared(opts) {
  const controlFile = path.join(opts.runRoot, "private", `control.${opts.pass}.json`);
  const recipeFile = path.join(opts.runRoot, "candidate-visible", `recipes.${opts.pass}.json`);
  if (!fs.existsSync(controlFile) || !fs.existsSync(recipeFile)) throw new Error(`${opts.pass} is not prepared`);
  const control = readJson(controlFile);
  if (control.node_version !== process.version) throw new Error("Node version changed after prepare");
  if (control.runner_sha256 !== fileHash(scriptFile)) throw new Error("runner changed after prepare");
  if (control.llama_server_sha256 !== fileHash(opts.llamaServer)) throw new Error("llama-server changed after prepare");
  if (control.recipes_sha256 !== fileHash(recipeFile)) throw new Error("prepared recipe snapshot changed");
  for (const model of control.models) {
    if (!fs.existsSync(model.model_path) || fs.statSync(model.model_path).size !== model.model_bytes) throw new Error(`model changed: ${model.candidate_id}`);
  }
  return { control, recipes: readJson(recipeFile) };
}

async function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => resolve(address.port));
    });
  });
}

async function waitForHealth(port, child) {
  const started = performance.now();
  while (performance.now() - started < 120_000) {
    if (child.exitCode !== null) throw new Error(`llama-server exited early with ${child.exitCode}`);
    try {
      const response = await fetch(`http://127.0.0.1:${port}/health`, { signal: AbortSignal.timeout(1_000) });
      if (response.ok) return performance.now() - started;
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error("llama-server health timeout");
}

function spawnServer(opts, candidate, recipe, model, arm, port) {
  const logRoot = path.join(opts.runRoot, "logs", opts.pass, candidate.id, recipe.id);
  fs.mkdirSync(logRoot, { recursive: true });
  const stdout = fs.openSync(path.join(logRoot, `${arm}.stdout.log`), "a");
  const stderr = fs.openSync(path.join(logRoot, `${arm}.stderr.log`), "a");
  const args = [
    "-m", model.model_path,
    "--alias", candidate.id,
    "--host", "127.0.0.1",
    "--port", String(port),
    "-c", "8192",
    "-np", "1",
    "-ngl", "999",
    "--reasoning", "off",
    "--cache-prompt",
    "--cache-reuse", "16",
    "--no-webui",
  ];
  const child = childProcess.spawn(opts.llamaServer, args, {
    detached: true,
    windowsHide: true,
    stdio: ["ignore", stdout, stderr],
  });
  recordActiveServer(opts.runRoot, { pid: child.pid, candidate_id: candidate.id, recipe_id: recipe.id, phase: `${opts.pass}:${arm}`, executable: opts.llamaServer });
  child.unref();
  return { child, stdout, stderr };
}

function stopServer(opts, server) {
  if (!server) return;
  let terminated = false;
  try {
    childProcess.execFileSync("taskkill.exe", ["/PID", String(server.child.pid), "/T", "/F"], { windowsHide: true, stdio: "ignore", timeout: 10_000 });
    terminated = true;
  } catch {
    try { process.kill(server.child.pid, 0); } catch { terminated = true; }
  }
  for (const descriptor of [server.stdout, server.stderr]) {
    try { fs.closeSync(descriptor); } catch {}
  }
  if (terminated) clearActiveServer(opts.runRoot, server.child.pid);
}

function workingSet(pid) {
  try {
    const script = `(Get-Process -Id ${Number(pid)} -ErrorAction Stop).WorkingSet64`;
    return Number(childProcess.execFileSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], { encoding: "utf8", windowsHide: true, timeout: 5_000 }).trim());
  } catch { return null; }
}

function gpuMemory(pid) {
  try {
    const output = childProcess.execFileSync("nvidia-smi.exe", ["--query-compute-apps=pid,used_memory", "--format=csv,noheader,nounits"], { encoding: "utf8", windowsHide: true, timeout: 5_000 });
    for (const line of output.split(/\r?\n/)) {
      const [rowPid, mib] = line.split(",").map((value) => value.trim());
      if (Number(rowPid) === pid) return Number(mib) * 1024 * 1024;
    }
  } catch {}
  return null;
}

async function applyTemplate(port, request, timeoutMs = 30_000) {
  const response = await fetch(`http://127.0.0.1:${port}/apply-template`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer local" },
    body: JSON.stringify({
      messages: request.messages,
      ...(request.chat_template_kwargs ? { chat_template_kwargs: request.chat_template_kwargs } : {}),
    }),
    signal: AbortSignal.timeout(timeoutMs),
  });
  const text = await response.text();
  let parsed;
  try { parsed = JSON.parse(text); } catch { throw new Error(`non-JSON template response: ${text.slice(0, 300)}`); }
  if (!response.ok) throw new Error(`template HTTP ${response.status}: ${JSON.stringify(parsed).slice(0, 500)}`);
  if (typeof parsed.prompt !== "string") throw new Error("template response has no prompt");
  return parsed.prompt;
}

export async function promptForRequest(port, request, template = applyTemplate) {
  return typeof request.raw_prompt === "string" ? request.raw_prompt : template(port, request);
}

async function rawCompletion(port, prompt, request, nPredict = request.max_tokens, timeoutMs = 120_000, jsonSchema = null) {
  const started = performance.now();
  const response = await fetch(`http://127.0.0.1:${port}/completion`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer local" },
    body: JSON.stringify({
      prompt,
      n_predict: nPredict,
      temperature: request.temperature,
      top_k: request.top_k,
      top_p: request.top_p,
      repeat_penalty: request.repeat_penalty,
      stop: request.stop,
      cache_prompt: true,
      id_slot: 0,
      ...(nPredict !== 0 && jsonSchema ? { json_schema: jsonSchema } : {}),
    }),
    signal: AbortSignal.timeout(timeoutMs),
  });
  const text = await response.text();
  const wallMs = performance.now() - started;
  let parsed;
  try { parsed = JSON.parse(text); } catch { throw new Error(`non-JSON server response: ${text.slice(0, 300)}`); }
  if (!response.ok) throw new Error(`HTTP ${response.status}: ${JSON.stringify(parsed).slice(0, 500)}`);
  const output = parsed?.content;
  if (nPredict !== 0 && typeof output !== "string") throw new Error("response has no completion content");
  return { wall_ms: wallMs, output, response: parsed };
}

async function generatedCompletion(port, request, outputMode = null) {
  const started = performance.now();
  const prompt = await promptForRequest(port, request);
  const schema = outputMode === "substring_edits_v1" ? SUBSTRING_EDITS_SCHEMA : null;
  const generated = await rawCompletion(port, prompt, request, request.max_tokens, 120_000, schema);
  return { ...generated, wall_ms: performance.now() - started, prompt };
}

async function prefillTranscriptPrefix(port, candidate, recipe, captured, prefix) {
  const prefixRequest = buildRequest(candidate, recipe, { ...captured, transcript: prefix });
  const continuationRequest = buildRequest(candidate, recipe, { ...captured, transcript: `${prefix} CONTINUATION` });
  const [prefixPrompt, continuationPrompt] = await Promise.all([
    promptForRequest(port, prefixRequest),
    promptForRequest(port, continuationRequest),
  ]);
  const common = longestCommonPrefix(prefixPrompt, continuationPrompt);
  if (!common) throw new Error("chat template has no stable prefix");
  const stablePrompt = continuationPrompt.startsWith(`${common} `) ? `${common} ` : common;
  const prefillRequest = { ...prefixRequest, max_tokens: 0 };
  return rawCompletion(port, stablePrompt, prefillRequest, 0);
}

function syntheticCase() {
  return {
    transcript: "Keep this sentence unchanged.",
    context: { app: "ChatGPT.exe", window: "ChatGPT", kind: "AI assistant chat", writing: "English (India).", field_text: "", selected_text: "", vocabulary: "" },
  };
}

function resultFile(opts, candidate, recipe, arm) {
  return path.join(opts.runRoot, "results", opts.pass, `${candidate.id}__${recipe.id}__${arm}.jsonl`);
}

function resumeKey(opts, candidate, recipe, arm, caseId) {
  return sha256(stableJson({ run_id: readJson(opts.config).run_id, pass: opts.pass, candidate_id: candidate.id, recipe_id: recipe.id, arm, case_id: caseId }));
}

async function runArm(opts, candidate, recipe, model, cases, arm) {
  const outputFile = resultFile(opts, candidate, recipe, arm);
  const completed = new Set(readJsonl(outputFile).filter((row) => row.status === "ok" || row.status === "rejected").map((row) => row.resume_key));
  const selectedCases = arm === "cold" ? [cases[Math.floor(cases.length / 2)]] : cases;
  const pending = selectedCases.filter((row) => !completed.has(resumeKey(opts, candidate, recipe, arm, row.case_id)));
  if (!pending.length) return { arm, skipped: selectedCases.length };

  const port = await freePort();
  const startupAt = performance.now();
  const server = spawnServer(opts, candidate, recipe, model, arm, port);
  let startupMs = null;
  let consecutiveErrors = 0;
  try {
    startupMs = await waitForHealth(port, server.child);
    if (arm !== "cold") {
      const warmRequest = buildRequest(candidate, recipe, syntheticCase());
      warmRequest.max_tokens = 1;
      await generatedCompletion(port, warmRequest, recipe.output_mode);
    }
    for (const captured of pending) {
      const key = resumeKey(opts, candidate, recipe, arm, captured.case_id);
      let prefixPrefillMs = 0;
      let prefixCount = 0;
      let lastPrefixTimings = null;
      try {
        if (arm === SCHEDULED_CHUNK_ARM) {
          const chunks = scheduledTranscriptChunks(captured.transcript, captured);
          if (!chunks.length) throw new Error(`scheduled chunk derivation produced no text for ${captured.case_id}`);
          let assembled = "";
          let totalLlmMs = 0;
          let peakWorkingSet = 0;
          let peakGpuMemory = 0;
          let fallbackCount = 0;
          const chunkResults = [];
          const requestHashes = [];
          for (let index = 0; index < chunks.length; index += 1) {
            const sourceChunk = chunks[index];
            const chunkCaptured = {
              ...captured,
              transcript: sourceChunk,
              context: {
                ...captured.context,
                field_text: scheduledFieldText(recipe, assembled),
              },
            };
            const request = buildRequest(candidate, recipe, chunkCaptured);
            const beforeWorkingSet = workingSet(server.child.pid);
            const beforeGpu = gpuMemory(server.child.pid);
            const generated = await generatedCompletion(port, request, recipe.output_mode);
            const afterWorkingSet = workingSet(server.child.pid);
            const afterGpu = gpuMemory(server.child.pid);
            peakWorkingSet = Math.max(peakWorkingSet, beforeWorkingSet ?? 0, afterWorkingSet ?? 0);
            peakGpuMemory = Math.max(peakGpuMemory, beforeGpu ?? 0, afterGpu ?? 0);
            totalLlmMs += generated.wall_ms;
            requestHashes.push(sha256(stableJson(request)));
            let repairedChunk = sourceChunk;
            let chunkFlags = [];
            let accepted = false;
            let materializationError = null;
            try {
              const materialized = materializeOutput(sourceChunk, generated.output, recipe.output_mode);
              chunkFlags = deterministicFlags(sourceChunk, materialized, chunkCaptured.context);
              accepted = chunkFlags.length === 0;
              if (accepted) repairedChunk = materialized;
            } catch (error) {
              chunkFlags = ["materialization_error"];
              materializationError = String(error?.message ?? error);
            }
            if (!accepted) fallbackCount += 1;
            assembled = appendWithOverlap(assembled, repairedChunk);
            chunkResults.push({
              index,
              phase: index === chunks.length - 1 ? "stop_tail" : "background",
              source_text: sourceChunk,
              raw_output: generated.output,
              materialized_output: repairedChunk,
              accepted,
              fallback_to_source: !accepted,
              mechanical_flags: chunkFlags,
              materialization_error: materializationError,
              cleanup_wall_ms: generated.wall_ms,
              usage: generated.response.usage ?? null,
              timings: generated.response.timings ?? null,
              request_sha256: requestHashes.at(-1),
            });
          }
          const tailResult = chunkResults.at(-1);
          const backgroundResults = chunkResults.slice(0, -1);
          const finalFlags = deterministicFlags(captured.transcript, assembled, captured.context);
          appendJsonl(outputFile, {
            schema: "heardright.scheduled_static15_selective_result.v2",
            captured_at: new Date().toISOString(),
            resume_key: key,
            pass: opts.pass,
            candidate_id: candidate.id,
            recipe_id: recipe.id,
            arm,
            case_id: captured.case_id,
            duration_s: captured.duration_s,
            transcript_words: normalizeWords(captured.transcript).length,
            scheduled_background_windows: captured.scheduled_background_windows,
            simulated_stop_tail_ms: captured.simulated_stop_tail_ms,
            status: "ok",
            error: null,
            startup_ms: startupMs,
            chunk_count: chunkResults.length,
            background_chunk_count: backgroundResults.length,
            fallback_chunk_count: fallbackCount,
            total_llm_wall_ms: totalLlmMs,
            background_llm_wall_ms: backgroundResults.reduce((sum, row) => sum + row.cleanup_wall_ms, 0),
            background_max_chunk_ms: Math.max(0, ...backgroundResults.map((row) => row.cleanup_wall_ms)) || null,
            background_missed_15s_deadline: backgroundResults.some((row) => row.cleanup_wall_ms > 15_000),
            cleanup_wall_ms: tailResult.cleanup_wall_ms,
            simulated_end_to_end_stop_ms: Number(captured.simulated_stop_tail_ms) + tailResult.cleanup_wall_ms,
            raw_output: assembled,
            materialized_output: assembled,
            mechanical_flags: finalFlags,
            process_working_set_bytes: peakWorkingSet || null,
            gpu_memory_bytes: peakGpuMemory || null,
            request_sha256: sha256(stableJson(requestHashes)),
            chunk_results: chunkResults,
          });
          startupMs = null;
          consecutiveErrors = 0;
          continue;
        }
        if (arm === "prefix_prefill") {
          for (const prefix of stableScheduledPrefixes(captured.transcript, captured)) {
            const prefilled = await prefillTranscriptPrefix(port, candidate, recipe, captured, prefix);
            prefixPrefillMs += prefilled.wall_ms;
            prefixCount += 1;
            lastPrefixTimings = prefilled.response.timings ?? null;
          }
        }
        const request = buildRequest(candidate, recipe, captured);
        const beforeWorkingSet = workingSet(server.child.pid);
        const beforeGpu = gpuMemory(server.child.pid);
        const generated = await generatedCompletion(port, request, recipe.output_mode);
        let materialized = null;
        let flags = [];
        let status = "ok";
        let error = null;
        try {
          materialized = materializeOutput(captured.transcript, generated.output, recipe.output_mode);
          flags = deterministicFlags(captured.transcript, materialized, captured.context);
        } catch (materializeError) {
          status = "rejected";
          flags = ["materialization_error"];
          error = String(materializeError?.message ?? materializeError);
        }
        const afterWorkingSet = workingSet(server.child.pid);
        const afterGpu = gpuMemory(server.child.pid);
        appendJsonl(outputFile, {
          schema: "heardright.scheduled_static15_selective_result.v1",
          captured_at: new Date().toISOString(),
          resume_key: key,
          pass: opts.pass,
          candidate_id: candidate.id,
          recipe_id: recipe.id,
          arm,
          case_id: captured.case_id,
          duration_s: captured.duration_s,
          transcript_words: normalizeWords(captured.transcript).length,
          scheduled_background_windows: captured.scheduled_background_windows,
          simulated_stop_tail_ms: captured.simulated_stop_tail_ms,
          status,
          error,
          startup_ms: startupMs,
          cold_total_ms: arm === "cold" ? performance.now() - startupAt : null,
          prefix_count: prefixCount,
          prefix_prefill_ms: prefixPrefillMs,
          prefix_last_timings: lastPrefixTimings,
          cleanup_wall_ms: generated.wall_ms,
          simulated_end_to_end_stop_ms: Number(captured.simulated_stop_tail_ms) + generated.wall_ms,
          raw_output: generated.output,
          materialized_output: materialized,
          mechanical_flags: flags,
          usage: generated.response.usage ?? null,
          timings: generated.response.timings ?? null,
          tokens_cached: generated.response.tokens_cached ?? null,
          tokens_evaluated: generated.response.tokens_evaluated ?? null,
          process_working_set_bytes: Math.max(beforeWorkingSet ?? 0, afterWorkingSet ?? 0) || null,
          gpu_memory_bytes: Math.max(beforeGpu ?? 0, afterGpu ?? 0) || null,
          request_sha256: sha256(stableJson(request)),
        });
        startupMs = null;
        consecutiveErrors = 0;
      } catch (error) {
        appendJsonl(outputFile, {
          schema: "heardright.scheduled_static15_selective_result.v1",
          captured_at: new Date().toISOString(),
          resume_key: key,
          pass: opts.pass,
          candidate_id: candidate.id,
          recipe_id: recipe.id,
          arm,
          case_id: captured.case_id,
          status: "error",
          error: String(error?.stack ?? error),
        });
        consecutiveErrors += 1;
        if (consecutiveErrors >= 3) throw new Error(`${candidate.id}/${arm} circuit opened after three infrastructure failures`);
      }
    }
  } finally {
    stopServer(opts, server);
  }
  return { arm, generated: pending.length, output: outputFile };
}

async function live(opts) {
  const invalidArms = opts.arms.filter((arm) => !VALID_ARMS.includes(arm));
  if (invalidArms.length) throw new Error(`invalid arm(s): ${invalidArms.join(",")}`);
  const { control, recipes } = validatePrepared(opts);
  let cases = readJsonl(path.join(opts.runRoot, "candidate-visible", "cases.jsonl"));
  if (opts.limit > 0) cases = cases.slice(0, opts.limit);
  const candidates = recipes.candidates.filter((candidate) => !opts.candidates.length || opts.candidates.includes(candidate.id));
  if (!candidates.length) throw new Error("no candidates selected");
  const arms = opts.arms.length ? opts.arms : (opts.pass === "pass1" ? [...ARMS] : ["warm_final", "prefix_prefill"]);
  const lock = acquireRunLock(opts.runRoot);
  try {
    reapActiveServer(opts.runRoot);
    for (const candidate of candidates) {
      const recipe = candidate.recipes[0];
      const model = control.models.find((item) => item.candidate_id === candidate.id);
      for (const arm of arms) {
        const result = await runArm(opts, candidate, recipe, model, cases, arm);
        console.log(JSON.stringify({ event: "arm_complete", pass: opts.pass, candidate_id: candidate.id, recipe_id: recipe.id, ...result }));
      }
    }
  } finally {
    reapActiveServer(opts.runRoot);
    lock.release();
  }
}

function summarizeRows(rows, scorerById) {
  const ok = rows.filter((row) => row.status === "ok");
  const eligible = ok.filter((row) => !(row.mechanical_flags ?? []).length);
  const latencies = ok.map((row) => row.cleanup_wall_ms).filter(Number.isFinite);
  const endToEnd = ok.map((row) => row.simulated_end_to_end_stop_ms).filter(Number.isFinite);
  const cloudWer = eligible.flatMap((row) => {
    const reference = scorerById.get(row.case_id)?.cloud_output;
    return reference ? [wordErrorRate(reference, row.materialized_output)] : [];
  });
  const cloudF1 = eligible.flatMap((row) => {
    const reference = scorerById.get(row.case_id)?.cloud_output;
    return reference ? [tokenF1(reference, row.materialized_output)] : [];
  });
  const flagCounts = {};
  for (const row of rows) for (const flag of row.mechanical_flags ?? []) flagCounts[flag] = (flagCounts[flag] ?? 0) + 1;
  return {
    attempts: rows.length,
    ok: ok.length,
    mechanically_eligible: eligible.length,
    mechanically_eligible_rate: rows.length ? eligible.length / rows.length : null,
    cleanup_p50_ms: percentile(latencies, 0.5),
    cleanup_p95_ms: percentile(latencies, 0.95),
    end_to_end_stop_p50_ms: percentile(endToEnd, 0.5),
    end_to_end_stop_p95_ms: percentile(endToEnd, 0.95),
    cloud_wer_mean: cloudWer.length ? cloudWer.reduce((sum, value) => sum + value, 0) / cloudWer.length : null,
    cloud_token_f1_mean: cloudF1.length ? cloudF1.reduce((sum, value) => sum + value, 0) / cloudF1.length : null,
    cloud_reference_rows: cloudF1.length,
    cache_n_p50: percentile(ok.map((row) => row.timings?.cache_n).filter(Number.isFinite), 0.5),
    prompt_n_p50: percentile(ok.map((row) => row.timings?.prompt_n).filter(Number.isFinite), 0.5),
    peak_process_working_set_bytes: Math.max(0, ...ok.map((row) => row.process_working_set_bytes ?? 0)) || null,
    peak_gpu_memory_bytes: Math.max(0, ...ok.map((row) => row.gpu_memory_bytes ?? 0)) || null,
    mechanical_failure_counts: flagCounts,
  };
}

function score(opts) {
  validatePrepared(opts);
  const scorerById = new Map(readJsonl(path.join(opts.runRoot, "private", "scorer.jsonl")).map((row) => [row.case_id, row]));
  const recipes = readJson(path.join(opts.runRoot, "candidate-visible", `recipes.${opts.pass}.json`));
  const summaries = [];
  for (const candidate of recipes.candidates) {
    const recipe = candidate.recipes[0];
    for (const arm of VALID_ARMS) {
      const file = resultFile(opts, candidate, recipe, arm);
      if (!fs.existsSync(file)) continue;
      summaries.push({ candidate_id: candidate.id, recipe_id: recipe.id, arm, ...summarizeRows(readJsonl(file), scorerById) });
    }
    const warmFile = resultFile(opts, candidate, recipe, "warm_final");
    const prefixFile = resultFile(opts, candidate, recipe, "prefix_prefill");
    if (fs.existsSync(warmFile) && fs.existsSync(prefixFile)) {
      const warm = new Map(readJsonl(warmFile).filter((row) => row.status === "ok").map((row) => [row.case_id, row.materialized_output]));
      const prefixed = readJsonl(prefixFile).filter((row) => row.status === "ok");
      const matched = prefixed.filter((row) => warm.get(row.case_id) === row.materialized_output).length;
      summaries.push({ candidate_id: candidate.id, recipe_id: recipe.id, arm: "warm_vs_prefix_identity", attempts: prefixed.length, identical: matched, identical_rate: prefixed.length ? matched / prefixed.length : null });
    }
  }
  const report = {
    schema: "heardright.scheduled_static15_selective_summary.v1",
    created_at: new Date().toISOString(),
    pass: opts.pass,
    cloud_yardstick_ms: { p50: 360, p95: 570 },
    summaries,
  };
  const file = path.join(opts.runRoot, "reports", `${opts.pass}.summary.json`);
  writeJson(file, report);
  console.log(JSON.stringify({ status: "scored", report: file, summaries: summaries.length }, null, 2));
}

function dryRun(opts) {
  const { control, recipes } = validatePrepared(opts);
  const cases = readJsonl(path.join(opts.runRoot, "candidate-visible", "cases.jsonl"));
  const candidates = recipes.candidates.filter((candidate) => !opts.candidates.length || opts.candidates.includes(candidate.id));
  const arms = opts.arms.length ? opts.arms : (opts.pass === "pass1" ? [...ARMS] : ["warm_final", "prefix_prefill"]);
  console.log(JSON.stringify({
    status: "dry_run_no_generation",
    run_root: opts.runRoot,
    pass: opts.pass,
    cases: opts.limit > 0 ? Math.min(opts.limit, cases.length) : cases.length,
    candidates: candidates.map((candidate) => ({ id: candidate.id, recipe_id: candidate.recipes[0].id, model_bytes: control.models.find((model) => model.candidate_id === candidate.id)?.model_bytes })),
    arms,
    gpu_concurrency: 1,
    resumable: true,
  }, null, 2));
}

async function main() {
  const opts = options();
  if (process.argv.includes("--prepare")) return prepare(opts, "pass1");
  if (process.argv.includes("--prepare-pass2")) return prepare({ ...opts, pass: "pass2" }, "pass2");
  if (process.argv.includes("--dry-run")) return dryRun(opts);
  if (process.argv.includes("--score")) return score(opts);
  if (process.argv.includes("--live")) return live(opts);
  throw new Error("choose --prepare, --prepare-pass2, --dry-run, --live, or --score");
}

if (path.resolve(process.argv[1] ?? "") === path.resolve(scriptFile)) {
  main().catch((error) => {
    console.error(error.stack ?? String(error));
    process.exitCode = 1;
  });
}
