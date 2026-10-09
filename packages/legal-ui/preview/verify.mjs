#!/usr/bin/env node
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "node:net";
import axe from "axe-core";

const previewDir = fileURLToPath(new URL(".", import.meta.url));
const packageDir = resolve(previewDir, "..");
const outputDir = join(packageDir, ".preview", "verification");
const qaScript = resolve(packageDir, "..", "..", "..", "tools", "lib", "qa-engine", "qa.mjs");
const variants = ["heardright-light", "viewright-light", "coderight-light", "mailright-light", "scraperight-light"];
const states = ["default", "pending", "rejected"];

function run(command, args, options = {}) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(command, args, { ...options, windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
    let output = "";
    child.stdout.on("data", (chunk) => { output += chunk; });
    child.stderr.on("data", (chunk) => { output += chunk; });
    child.once("error", reject);
    child.once("exit", (code) => code === 0 ? resolveRun(output) : reject(new Error(`${command} ${args.join(" ")} exited ${code}\n${output}`)));
  });
}

function port() {
  return new Promise((resolvePort, reject) => {
    const listener = createServer();
    listener.once("error", reject);
    listener.listen(0, "127.0.0.1", () => {
      const address = listener.address();
      listener.close(() => resolvePort(address.port));
    });
  });
}

async function waitForHttp(url, timeout = 30_000) {
  const started = Date.now();
  let lastError = "";
  while (Date.now() - started < timeout) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
      lastError = `HTTP ${response.status}`;
    } catch (error) { lastError = error instanceof Error ? error.message : String(error); }
    await new Promise((resolveWait) => setTimeout(resolveWait, 100));
  }
  throw new Error(`Preview server did not become ready: ${lastError}`);
}

function actionsFor(id, state, screenshot) {
  const expected = state === "pending" ? "Saving…" : state === "rejected" ? "[data-rk-legal-gate-error]" : "button[type=submit]";
  return [
    { type: "click", selector: 'input[value="individual"]' },
    { type: "click", selector: 'input[type="checkbox"]' },
    { type: "click", selector: 'button[type="submit"]' },
    state === "pending" ? { type: "waitForText", text: expected } : { type: "waitFor", selector: expected },
    { type: "screenshot", out: screenshot },
    {
      type: "eval",
      expression: `(async () => { ${axe.source}\nconst results = await axe.run(document, { runOnly: { type: "tag", values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"] } }); await fetch("/__axe-report?id=${encodeURIComponent(id)}&state=${encodeURIComponent(state)}", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(results) }); return { ok: true, violations: results.violations.length }; })()`,
    },
  ];
}

async function axeReport(serverPort, id, state) {
  const response = await fetch(`http://127.0.0.1:${serverPort}/__axe-report?id=${encodeURIComponent(id)}&state=${encodeURIComponent(state)}`);
  if (!response.ok) throw new Error(`Missing axe report for ${id}/${state}.`);
  return response.json();
}

function contactSheet(records) {
  const cards = records.map(({ id, state, file }) => `<figure><img src="${file}" alt="${id} ${state}" /><figcaption>${id} — ${state}</figcaption></figure>`).join("\n");
  return `<!doctype html><meta charset="utf-8"><title>LegalGate verification</title><style>body{margin:24px;background:#f7f3ec;color:#272322;font:14px system-ui}main{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:20px}figure{margin:0;background:#fff;padding:12px;border:1px solid #e4dcda}img{display:block;width:100%;height:auto}figcaption{margin-top:8px;font-weight:600}</style><main>${cards}</main>`;
}

async function main() {
  if (!existsSync(qaScript)) throw new Error(`Shared QA engine missing: ${qaScript}`);
  rmSync(outputDir, { recursive: true, force: true });
  mkdirSync(outputDir, { recursive: true });
  const serverPort = await port();
  const server = spawn(process.execPath, ["serve.mjs"], { cwd: previewDir, env: { ...process.env, PORT: String(serverPort) }, windowsHide: true, stdio: "ignore" });
  const records = [];
  try {
    await waitForHttp(`http://127.0.0.1:${serverPort}/`);
    for (const id of variants) {
      for (const state of states) {
        const filename = `${id}-${state}.png`;
        const actionsPath = join(outputDir, `${id}-${state}.actions.json`);
          writeFileSync(actionsPath, JSON.stringify(actionsFor(id, state, join(outputDir, filename))));
        try {
          const query = state === "default" ? "" : `&qaState=${state}`;
          await run(process.execPath, [qaScript, "--url", `http://127.0.0.1:${serverPort}/?id=${id}${query}`, "--actions", actionsPath, "--width", "900", "--height", "640"], { cwd: packageDir });
          const result = await axeReport(serverPort, id, state);
          if (result.violations.length) throw new Error(`${id}/${state}: ${JSON.stringify(result.violations)}.`);
          records.push({ id, state, file: filename, axe: result });
        } finally {
          rmSync(actionsPath, { force: true });
        }
      }
    }
  } finally {
    server.kill();
  }
  if (records.length !== 15 || records.some(({ file }) => !existsSync(join(outputDir, file)))) throw new Error("Verification artifacts are incomplete.");
  writeFileSync(join(outputDir, "axe-report.json"), JSON.stringify({ standard: "WCAG 2.0/2.1/2.2 A/AA", records }, null, 2) + "\n");
  writeFileSync(join(outputDir, "contact-sheet.html"), contactSheet(records));
  console.log(`Verified ${records.length} states; artifacts: ${outputDir}`);
}

main().catch((error) => { console.error(error.stack || error); process.exit(1); });
