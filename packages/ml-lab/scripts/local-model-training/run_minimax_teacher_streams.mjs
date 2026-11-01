#!/usr/bin/env node
import { createHash } from "node:crypto";
import { closeSync, existsSync, fsyncSync, mkdirSync, openSync, readFileSync, renameSync, rmSync, writeFileSync, writeSync } from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";

const DEFAULT_STREAMS = 4;
const DEFAULT_SESSION_ROWS = 250;
const DEFAULT_TIMEOUT_MS = 900_000;
const DEFAULT_MAX_ATTEMPTS = 12;
const RUNNER = path.join(path.dirname(fileURLToPath(import.meta.url)), "run_minimax_teacher_queue.mjs");

const canonical = (value) => JSON.stringify(value);
const sha256 = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;

export function parseArgs(argv) {
  const valueAfter = (flag, fallback) => {
    const index = argv.indexOf(flag);
    return index >= 0 ? argv[index + 1] : fallback;
  };
  return {
    queue: valueAfter("--queue"),
    outDir: valueAfter("--out"),
    streams: Number(valueAfter("--streams", `${DEFAULT_STREAMS}`)),
    limit: Number(valueAfter("--limit", "0")),
    mm: valueAfter("--mm", "D:\\Claude\\bin\\ccx.cmd"),
    claudeModel: valueAfter("--claude-model"),
    sessionRows: Number(valueAfter("--session-rows", `${DEFAULT_SESSION_ROWS}`)),
    timeoutMs: Number(valueAfter("--timeout-ms", `${DEFAULT_TIMEOUT_MS}`)),
    maxAttempts: Number(valueAfter("--max-attempts", `${DEFAULT_MAX_ATTEMPTS}`)),
    transport: "claude",
    detach: argv.includes("--detach"),
  };
}

export function buildStreamRunArgs(options) {
  if (!options.queue || !options.outDir) throw new Error("usage: --queue <cloud-request-queue.jsonl> --out <private-run-dir> [--streams 3|4]");
  if (![3, 4].includes(options.streams)) throw new Error("streams must be 3 or 4");
  if (![options.limit, options.sessionRows, options.timeoutMs, options.maxAttempts].every((value) => Number.isInteger(value) && value >= 0) || ![options.sessionRows, options.timeoutMs, options.maxAttempts].every((value) => value > 0)) throw new Error("limit must be non-negative; session rows, timeout, and max attempts must be positive integers");
  if (!options.claudeModel) throw new Error("MiniMax-M3 gateway model ID is required");
  return Array.from({ length: options.streams }, (_, streamIndex) => ({
    streamIndex,
    args: [
      RUNNER,
      "--queue", path.resolve(options.queue),
      "--out", path.join(path.resolve(options.outDir), `stream-${streamIndex}`),
      "--mm", options.mm,
      "--claude-model", options.claudeModel,
      "--transport", options.transport,
      "--session-rows", `${options.sessionRows}`,
      "--timeout-ms", `${options.timeoutMs}`,
      "--max-attempts", `${options.maxAttempts}`,
      "--shard-index", `${streamIndex}`,
      "--shard-count", `${options.streams}`,
      ...(options.limit ? ["--limit", `${options.limit}`] : []),
    ],
  }));
}

export function selectMiniMaxModel(models) {
  const model = models.find((candidate) => candidate.display_name === "MiniMax-M3" && /^claude-ccx-minimax--/u.test(candidate.id));
  if (!model) throw new Error("MiniMax-M3 is not available from ccx gateway discovery");
  return model;
}

export function streamLogPaths(outDir, streamIndex) {
  const streamDir = path.join(path.resolve(outDir), `stream-${streamIndex}`);
  return {
    stdout: path.join(streamDir, "runner.stdout.log"),
    stderr: path.join(streamDir, "runner.stderr.log"),
  };
}

async function discoverMiniMaxModel(baseUrl = "http://127.0.0.1:8801") {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 15_000);
  try {
    const response = await fetch(`${baseUrl.replace(/\/$/u, "")}/v1/models?ids=cli`, { signal: controller.signal });
    if (!response.ok) throw new Error(`ccx gateway discovery HTTP ${response.status}`);
    const body = await response.json();
    return selectMiniMaxModel(body.data ?? []);
  } finally {
    clearTimeout(timer);
  }
}

function atomicWrite(file, content) {
  const temp = `${file}.tmp-${process.pid}`;
  writeFileSync(temp, content, "utf8");
  renameSync(temp, file);
}

function durableAppend(file, value) {
  const fd = openSync(file, "a");
  try {
    writeSync(fd, `${canonical(value)}\n`, null, "utf8");
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
}

function readQueueRows(file) {
  return readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).length;
}

function acquireLock(lockFile) {
  let fd;
  try { fd = openSync(lockFile, "wx"); }
  catch { throw new Error(`run already active: ${lockFile}`); }
  try { writeSync(fd, `${process.pid}\n`, null, "utf8"); fsyncSync(fd); }
  finally { closeSync(fd); }
}

function runChild(plan, receiptFile) {
  durableAppend(receiptFile, { schema_version: "heardright.l1_minimax_teacher_stream_receipt.v1", status: "started", stream_index: plan.streamIndex, started_at: new Date().toISOString(), args_hash: sha256(canonical(plan.args)) });
  return new Promise((resolve) => {
    const outDir = plan.args[plan.args.indexOf("--out") + 1];
    mkdirSync(outDir, { recursive: true });
    const logs = streamLogPaths(path.dirname(outDir), plan.streamIndex);
    const stdout = openSync(logs.stdout, "a");
    const stderr = openSync(logs.stderr, "a");
    let closed = false;
    const closeLogs = () => {
      if (closed) return;
      closed = true;
      closeSync(stdout);
      closeSync(stderr);
    };
    const child = spawn(process.execPath, plan.args, { cwd: process.cwd(), windowsHide: true, stdio: ["ignore", stdout, stderr] });
    child.on("error", (error) => { closeLogs(); resolve({ streamIndex: plan.streamIndex, exitCode: 1, error: error.message }); });
    child.on("close", (exitCode, signal) => { closeLogs(); resolve({ streamIndex: plan.streamIndex, exitCode: exitCode ?? 1, signal: signal ?? null }); });
  }).then((result) => {
    durableAppend(receiptFile, { schema_version: "heardright.l1_minimax_teacher_stream_receipt.v1", status: result.exitCode === 0 ? "complete" : "failed", stream_index: result.streamIndex, exit_code: result.exitCode, signal: result.signal ?? null, error: result.error ?? null, completed_at: new Date().toISOString() });
    return result;
  });
}

async function run(options) {
  const discoveredModel = options.claudeModel ? { id: options.claudeModel } : await discoverMiniMaxModel();
  const resolvedOptions = { ...options, claudeModel: discoveredModel.id };
  const plans = buildStreamRunArgs(resolvedOptions);
  const outDir = path.resolve(options.outDir);
  const queue = path.resolve(options.queue);
  if (!existsSync(queue)) throw new Error(`queue does not exist: ${queue}`);
  if (!existsSync(options.mm)) throw new Error(`ccx executable does not exist: ${options.mm}`);
  mkdirSync(outDir, { recursive: true });
  const lockFile = path.join(outDir, ".streams.lock");
  const receiptFile = path.join(outDir, "stream-receipts.jsonl");
  acquireLock(lockFile);
  try {
    const queueRows = readQueueRows(queue);
    atomicWrite(path.join(outDir, "stream-plan.json"), `${JSON.stringify({ schema_version: "heardright.l1_minimax_teacher_stream_plan.v1", status: "running", provider: "MiniMax-M3", ccx_model_id: resolvedOptions.claudeModel, provider_route: "ccx:gateway-discovery", streams: resolvedOptions.streams, sequential_rows_per_stream: resolvedOptions.sessionRows, queue_rows: queueRows, max_requests: resolvedOptions.limit ? Math.min(queueRows, resolvedOptions.limit * resolvedOptions.streams) : queueRows, queue_sha256: sha256(readFileSync(queue)), stream_output_dirs: plans.map((plan) => path.basename(plan.args[plan.args.indexOf("--out") + 1])), created_at: new Date().toISOString() }, null, 2)}\n`);
    const results = await Promise.all(plans.map((plan) => runChild(plan, receiptFile)));
    const failed = results.filter((result) => result.exitCode !== 0);
    atomicWrite(path.join(outDir, "stream-plan.json"), `${JSON.stringify({ schema_version: "heardright.l1_minimax_teacher_stream_plan.v1", status: failed.length ? "incomplete" : "complete", provider: "MiniMax-M3", ccx_model_id: resolvedOptions.claudeModel, provider_route: "ccx:gateway-discovery", streams: resolvedOptions.streams, sequential_rows_per_stream: resolvedOptions.sessionRows, queue_rows: queueRows, max_requests: resolvedOptions.limit ? Math.min(queueRows, resolvedOptions.limit * resolvedOptions.streams) : queueRows, queue_sha256: sha256(readFileSync(queue)), results, updated_at: new Date().toISOString() }, null, 2)}\n`);
    if (failed.length) process.exitCode = 2;
  } finally {
    rmSync(lockFile, { force: true });
  }
}

function detach(options) {
  if (!options.queue || !options.outDir) throw new Error("usage: --queue <cloud-request-queue.jsonl> --out <private-run-dir> [--detach]");
  const outDir = path.resolve(options.outDir);
  mkdirSync(outDir, { recursive: true });
  const childArgs = process.argv.slice(2).filter((arg) => arg !== "--detach");
  const child = spawn(process.execPath, [fileURLToPath(import.meta.url), ...childArgs], {
    cwd: process.cwd(),
    detached: true,
    stdio: "ignore",
    windowsHide: true,
  });
  child.unref();
  atomicWrite(path.join(outDir, "detached-run.json"), `${JSON.stringify({ schema_version: "heardright.l1_minimax_teacher_detached_run.v1", status: "started", pid: child.pid, started_at: new Date().toISOString(), command_hash: sha256(canonical(childArgs)) }, null, 2)}\n`);
  console.log(canonical({ status: "detached", pid: child.pid, out_dir: outDir }));
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    const options = parseArgs(process.argv.slice(2));
    if (options.detach) detach(options);
    else await run(options);
  }
  catch (error) { console.error(error.stack ?? error.message); process.exitCode = 1; }
}
