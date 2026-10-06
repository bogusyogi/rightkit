// Shared helpers. Principles (suite-wide): run the REAL binaries, fresh temp app-data per scenario,
// mock only PAID providers (local HTTP), never play audio, never make a billed call, leave no orphans.
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import assert from "node:assert/strict";

export { assert };
export const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
export const TIER = process.env.RIGHTAPP_E2E_TIER || "fast";

export function enginePath() {
  const candidates = [
    process.env.RIGHTAPP_ENGINE,
    join(REPO, "target", "debug", "rightapp-engine"),
    join(REPO, "target", "release", "rightapp-engine"),
    join(REPO, "target", "debug", "rightapp-engine.exe"),
  ].filter(Boolean);
  const found = candidates.find((c) => existsSync(c));
  if (!found) throw new Error("no rightapp-engine binary; set RIGHTAPP_ENGINE or run with --build");
  return found;
}

export const scratchAppData = (name) => mkdtempSync(join(tmpdir(), `rightapp-e2e-${name}-`));
export const baseEnv = (appData) => ({ ...process.env, RIGHTAPP_APP_DATA: appData });

/** Scenario = node:test case gated on tier. */
export function scenario(name, { tier = "fast" } = {}, fn) {
  const skip = TIER !== "all" && TIER !== tier ? `tier ${tier} (running ${TIER})` : false;
  test(name, { skip }, fn);
}

/** One-shot CLI call: returns { status, json }. */
export function cli(words, { appData, input } = {}) {
  const args = [...words, ...(input === undefined ? [] : ["--input", JSON.stringify(input)])];
  const r = spawnSync(enginePath(), args, { env: baseEnv(appData), encoding: "utf8", timeout: 30_000 });
  let json = null;
  try { json = JSON.parse(r.stdout); } catch { /* not JSON */ }
  return { status: r.status, json, stderr: r.stderr };
}

/** Framed stdio session against the resident sidecar. */
export function sidecar(appData) {
  const child = spawn(enginePath(), ["--stdio"], { env: baseEnv(appData), stdio: ["pipe", "pipe", "inherit"] });
  let buf = "";
  const waiters = [];
  child.stdout.on("data", (d) => {
    buf += d;
    let i;
    while ((i = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, i);
      buf = buf.slice(i + 1);
      if (line.trim()) waiters.shift()?.(JSON.parse(line));
    }
  });
  let id = 0;
  return {
    child,
    call: (cmd, input = {}) =>
      new Promise((res) => {
        waiters.push(res);
        child.stdin.write(JSON.stringify({ id: ++id, cmd, input }) + "\n");
      }),
    /** Graceful shutdown by closing stdin; asserts the process really exits (no orphan). */
    async close() {
      child.stdin.end();
      const code = await new Promise((res) => child.once("exit", res));
      assert.equal(code, 0, "engine exits cleanly when stdin closes");
    },
  };
}
