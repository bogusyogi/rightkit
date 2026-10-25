// The stages behind the one door (Adrian, 2026-10-25): rightkit-gate --to <stage> runs every stage up to <stage>, in order, for the
// commit at HEAD, and stops at the first that fails. Class A always runs first. The stages reuse the existing owners instead of
// reimplementing them: the gate's own scans for check/compile, `right-release` for package (source QA, then the build that seals the
// exact artifact set), installed QA on that sealed set, and upload of that same sealed set. Those right-release commands refuse to
// run unless a live door run launched them (requireDoorRun), so there is no side door into a stage.
//
// Run token: <git common dir>/rightkit-gate/runs/<nonce>.json { pid, commit, tree, stage } while a door run is active; the child gets
// RK_GATE_RUN=<nonce>. Receipt: <git common dir>/rightkit-gate/receipts/<commit>.json, machine-owned, binding the commit and tree to
// the executor, toolchain, lockfile digests, gate digest, each stage's result and the sealed manifest digest (the bytes that were
// packaged, qualified and released). Nobody edits these by hand; the door writes them and reads them.
import { createHash, randomBytes } from "node:crypto";
import fs from "node:fs";
import path from "node:path";

export const STAGES = Object.freeze(["check", "compile", "package", "qualify", "release"]);
export const RECEIPT_SCHEMA = "rightkit.gate-receipt/1";
export const RUN_ENV = "RK_GATE_RUN";

const sha256 = (data) => createHash("sha256").update(data).digest("hex");

/** The stages a run to `target` covers, in order; null when `target` is not a stage. */
export function stagesTo(target) {
  const i = STAGES.indexOf(target);
  return i < 0 ? null : STAGES.slice(0, i + 1);
}

/** The door's state directory under the repository's git common dir. */
export function gateStateDir(commonDir) {
  return path.join(commonDir, "rightkit-gate");
}

/** Starts a door run: writes the run file and returns { nonce, file, env }. The caller removes it with endRun. */
export function startRun({ commonDir, commit, tree, stage, pid = process.pid, write = fs.writeFileSync, mkdir = fs.mkdirSync }) {
  const nonce = randomBytes(16).toString("hex");
  const dir = path.join(gateStateDir(commonDir), "runs");
  mkdir(dir, { recursive: true });
  const file = path.join(dir, `${nonce}.json`);
  write(file, `${JSON.stringify({ pid, commit, tree, stage, startedAt: new Date().toISOString() })}\n`);
  return { nonce, file, env: { [RUN_ENV]: nonce } };
}

export function endRun(run, rm = fs.rmSync) {
  if (run?.file) rm(run.file, { force: true });
}

/**
 * Whether this process was launched by a live door run for `commit`. Returns null when it was, else the refusal message. A run file
 * whose pid is gone, or that names another commit, is no run.
 */
export function requireDoorRun({ env = process.env, commonDir, commit, readFile = (f) => fs.readFileSync(f, "utf8"), alive = pidAlive, command = "this command" }) {
  const nonce = env[RUN_ENV];
  const refusal = `${command} runs only through the one door: rightkit-gate --to <check|compile|package|qualify|release> (it runs class A first, then every stage in order)`;
  if (!nonce || !/^[0-9a-f]{32}$/.test(nonce)) return refusal;
  let run;
  try { run = JSON.parse(readFile(path.join(gateStateDir(commonDir), "runs", `${nonce}.json`))); } catch { return `${refusal}; RK_GATE_RUN names no live door run`; }
  if (!alive(run.pid)) return `${refusal}; the door run that launched it has ended`;
  if (commit && run.commit !== commit) return `${refusal}; the door run is for ${String(run.commit).slice(0, 12)}, HEAD is ${commit.slice(0, 12)}`;
  return null;
}

function pidAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try { process.kill(pid, 0); return true; } catch (e) { return e.code === "EPERM"; }
}

/** Reads the receipt for `commit`, or a fresh one bound to it. */
export function readReceipt({ commonDir, commit, readFile = (f) => fs.readFileSync(f, "utf8") }) {
  try {
    const r = JSON.parse(readFile(path.join(gateStateDir(commonDir), "receipts", `${commit}.json`)));
    if (r.schema === RECEIPT_SCHEMA && r.commit === commit) return r;
  } catch { /* none yet */ }
  return null;
}

export function writeReceipt({ commonDir, receipt, write = fs.writeFileSync, mkdir = fs.mkdirSync }) {
  const dir = path.join(gateStateDir(commonDir), "receipts");
  mkdir(dir, { recursive: true });
  write(path.join(dir, `${receipt.commit}.json`), `${JSON.stringify(receipt, null, 2)}\n`);
}

/**
 * The inputs a receipt binds besides the commit: the tree id, each lockfile's digest, the gate's own digest and the toolchain versions.
 * A receipt from an earlier run counts for a stage only when every one of these is unchanged.
 */
export function bindingOf({ tree, lockFiles = {}, gateFile, readFile = (f) => fs.readFileSync(f), toolchain = {}, executor }) {
  let gate = null;
  try { gate = sha256(readFile(gateFile)); } catch { /* unreadable: no binding, nothing reused */ }
  return { tree, locks: Object.fromEntries(Object.entries(lockFiles).map(([k, v]) => [k, sha256(v)]).sort(([a], [b]) => a.localeCompare(b))), gate, toolchain, executor };
}

export function sameBinding(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** The sealed release directory build-release wrote for `commit` on `platform`: { id, manifest, digest } or null. */
export function sealedFor({ root, commit, platform, readdir = fs.readdirSync, readFile = (f) => fs.readFileSync(f) }) {
  const base = path.join(root, ".right-release", "sealed");
  let ids = [];
  try { ids = readdir(base).filter((id) => id.endsWith(`-${commit.slice(0, 8)}`)); } catch { return null; }
  for (const id of ids.sort()) {
    const file = path.join(base, id, platform, "release-manifest.json");
    try { const bytes = readFile(file); return { id, manifest: file, digest: sha256(bytes) }; } catch { /* not this platform */ }
  }
  return null;
}
