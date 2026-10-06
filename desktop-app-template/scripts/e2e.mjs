#!/usr/bin/env node
// End-to-end runner. Usage: node scripts/e2e.mjs [--tier fast|local-models|paid|all] [--only <substr>] [--build]
// Finds (or with --build, builds through managed RightKit Cargo) the REAL rightapp-engine binary and
// runs tests/e2e/*.test.mjs under node:test. Tiers: fast (default; no models, no network, no audio),
// local-models (heavy; wrap in the heavy-run helper), paid (local HTTP mocks only; NEVER a billed call).
import { spawnSync } from "node:child_process";
import { readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { enginePath } from "../tests/e2e/lib.mjs";

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const opt = (n, d) => (args.includes(`--${n}`) ? args[args.indexOf(`--${n}`) + 1] : d);
const tier = opt("tier", "fast");
const only = opt("only", null);

if (args.includes("--build")) {
  const r = spawnSync("rightkit", ["cargo", "build", "-p", "rightapp-engine"], { cwd: REPO, stdio: "inherit" });
  if (r.status !== 0) process.exit(r.status ?? 1);
}
let engine;
try { engine = enginePath(); } catch (e) { console.error(`[e2e] ${e.message}`); process.exit(2); }

const files = readdirSync(join(REPO, "tests", "e2e"))
  .filter((f) => f.endsWith(".test.mjs") && (!only || f.includes(only)))
  .sort()
  .map((f) => join("tests", "e2e", f));
console.log(`[e2e] engine=${engine} tier=${tier} files=${files.length}`);
const r = spawnSync(process.execPath, ["--test", "--test-concurrency=1", ...files], {
  cwd: REPO, stdio: "inherit",
  env: { ...process.env, RIGHTAPP_ENGINE: engine, RIGHTAPP_E2E_TIER: tier },
});
process.exit(r.status ?? 1);
