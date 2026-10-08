#!/usr/bin/env node
import { existsSync, readFileSync, writeFileSync, realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "./background-process.mjs";
import { OWNERSHIP_APPS, formatOwnershipJson, buildOwnershipReport, renderOwnershipMarkdown } from "./ownership-report.mjs";
import { MANIFEST_FILE } from "./ownership.mjs";

const packageRoot = path.dirname(fileURLToPath(import.meta.url));
const defaultWorkspace = path.resolve(packageRoot, "../../../..");
const MAX_SCAN_OUTPUT_BYTES = 32 * 1024 * 1024;
const flag = (args, name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };

function scanWorker(appRoot, manifestPath, timeoutMs) {
  return new Promise((resolve) => {
    const worker = path.join(packageRoot, "ownership-scan-worker.mjs");
    const child = spawn(process.execPath, [worker, appRoot, manifestPath], { stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "", stderr = "", outputBytes = 0, settled = false;
    const finish = (value) => { if (settled) return; settled = true; clearTimeout(timer); resolve(value); };
    child.stdout.setEncoding("utf8"); child.stderr.setEncoding("utf8");
    const collect = (target) => (chunk) => {
      outputBytes += Buffer.byteLength(chunk);
      if (outputBytes > MAX_SCAN_OUTPUT_BYTES) { child.kill(); finish({ error: `scanner output exceeded ${MAX_SCAN_OUTPUT_BYTES} bytes; marked unscanned` }); return; }
      if (target === "stdout") stdout += chunk; else stderr += chunk;
    };
    child.stdout.on("data", collect("stdout")); child.stderr.on("data", collect("stderr"));
    child.once("error", (error) => finish({ error: `scanner worker failed to start: ${error.message}` }));
    child.once("close", (code) => {
      if (code !== 0) return finish({ error: stderr.trim() || `scanner worker exited ${code}` });
      try { finish({ value: JSON.parse(stdout) }); } catch (error) { finish({ error: `scanner worker returned invalid JSON: ${error.message}` }); }
    });
    const timer = setTimeout(() => { child.kill(); finish({ error: `scan exceeded ${timeoutMs} ms limit; marked unscanned` }); }, timeoutMs);
  });
}

async function scanConfiguredApps(workspaceRoot, manifest, timeoutMs) {
  const results = new Map();
  let cursor = 0;
  const worker = async () => {
    for (;;) {
      const index = cursor++;
      if (index >= OWNERSHIP_APPS.length) return;
      const app = OWNERSHIP_APPS[index];
      const appRoot = path.resolve(workspaceRoot, app.root);
      if (!existsSync(appRoot)) continue;
      results.set(app.key, await scanWorker(appRoot, path.join(packageRoot, MANIFEST_FILE), timeoutMs));
    }
  };
  await Promise.all(Array.from({ length: Math.min(3, OWNERSHIP_APPS.length) }, worker));
  return results;
}

export async function main(args, io = {}) {
  const workspaceRoot = path.resolve(flag(args, "--workspace") ?? defaultWorkspace);
  const manifest = JSON.parse(readFileSync(path.join(packageRoot, MANIFEST_FILE), "utf8"));
  const versions = JSON.parse(readFileSync(path.join(packageRoot, "rightkit-versions.json"), "utf8"));
  const timeoutMs = Number(flag(args, "--timeout-ms") ?? 60_000);
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1_000 || timeoutMs > 300_000) throw new TypeError("--timeout-ms must be between 1000 and 300000");
  const scanResults = await scanConfiguredApps(workspaceRoot, manifest, timeoutMs);
  const report = buildOwnershipReport({ workspaceRoot, manifest, versions, scanner: (root, catalog) => {
    const app = OWNERSHIP_APPS.find((entry) => path.resolve(workspaceRoot, entry.root) === path.resolve(root));
    if (!app) throw new Error("app root is not in configured app list");
    const scanned = scanResults.get(app.key);
    if (!scanned) throw new Error("configured app root missing; marked unscanned");
    if (scanned.error) throw new Error(scanned.error);
    // The protected worker returns { inputs, trust, report, verdict } (or { error }); never treat
    // that envelope itself as a report, which would read as a false zero-finding scan.
    if (scanned.value?.error) throw new Error(`${scanned.value.code ?? "RK_OWNERSHIP_UNVERIFIED"}: ${scanned.value.error}`);
    // A protected preflight refusal returns without evidence: nothing was scanned.
    if (!scanned.value?.evidence && scanned.value?.verdict && !scanned.value.verdict.admit)
      throw new Error(`${scanned.value.verdict.code}: ${(scanned.value.verdict.problems ?? []).join("; ") || "protected preflight refused"}; marked unscanned`);
    const scan = scanned.value?.report;
    if (!scan || typeof scan !== "object" || !Array.isArray(scan.violations) || !Array.isArray(scan.problems)) throw new Error("scanner worker returned no ownership report; marked unscanned");
    return scan;
  } });
  const markdown = renderOwnershipMarkdown(report);
  const json = formatOwnershipJson(report);
  const markdownPath = flag(args, "--markdown");
  const jsonPath = flag(args, "--json-out");
  if (markdownPath) writeFileSync(path.resolve(markdownPath), markdown);
  if (jsonPath) writeFileSync(path.resolve(jsonPath), json);
  if (args.includes("--json")) (io.stdout ?? process.stdout).write(json);
  else {
    if (!markdownPath && !jsonPath) (io.stdout ?? process.stdout).write(markdown);
    (io.stderr ?? process.stderr).write(`[right-release] ownership report: ${report.apps.length} apps, ${report.blocks.length} deduplicated blocks, ${report.exceptionRequests.length} pending exception requests\n`);
    if (markdownPath) (io.stderr ?? process.stderr).write(`[right-release] Markdown: ${path.resolve(markdownPath)}\n`);
    if (jsonPath) (io.stderr ?? process.stderr).write(`[right-release] JSON: ${path.resolve(jsonPath)}\n`);
  }
  return report.apps.some((app) => app.status === "blocked" || app.status === "unscanned" || app.rows.some((row) => row.status === "blocked" || row.status === "unscanned")) ? 1 : 0;
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try { process.exitCode = await main(process.argv.slice(2)); }
  catch (error) { console.error(`[right-release] ownership report: ${error.message}`); process.exitCode = 1; }
}
