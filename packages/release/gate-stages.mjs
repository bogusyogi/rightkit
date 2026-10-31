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
import { writeFileAtomic } from "./atomic-write.mjs";
import { platformDirOf } from "./platform-dir.mjs";

export const STAGES = Object.freeze(["check", "compile", "package", "qualify", "release"]);
export const RECEIPT_SCHEMA = "rightkit.gate-receipt/1";
export const RUN_ENV = "RK_GATE_RUN";
/** Set beside RUN_ENV for a run that carries a source split: the checkout whose git directory holds the run file. */
export const CHECKOUT_ENV = "RK_GATE_CHECKOUT";

const sha256 = (data) => createHash("sha256").update(data).digest("hex");

/**
 * Where every stage the gate runs reads its source from (docs/GATE-CONTRACT.md "Commit-only build": every stage, `--to dev` and
 * `--to test` included, builds a commit; uncommitted changes never enter a build). One row per stage name the gate dispatches:
 * the STAGES, `dev` and `test`, `stage` (the hand-off to the publisher) and each STAGES entry again as a RightKit unit stage
 * (`--to <stage> --unit <selector>`). `compiles` and `packages` say whether the stage starts a compiler or seals bytes.
 *   source "snapshot": the gate-owned snapshot of the selected commit (the checkout is never a build source on any host: rightkit#1301 deleted
 *     the checkout route, so no row carries an interim checkout rule). `implemented: true` says the executable route exists in code and runs;
 *     `witnessed: false` with `pending` says which host acceptance (native fixture witness on the Mac and the Dell) has not yet been proved. The two
 *     are separate facts (GB-86): an implemented route may run for its witness, and a route whose host acceptance is unproved is never presented as
 *     ready for apps (`appFacingReady`). A stage the route planner cannot run fails at planning with a named capability, not through this table.
 *   source "sealed" or "publisher", with `basis`: the stage compiles nothing from source (it acts on bytes an earlier stage
 *     sealed, or runs only in the trusted publisher on hosted CI with its normal checkout, docs/GATE-CONTRACT.md).
 *   source "checkout", with `gap`: a known breach, listed so it cannot grow; stageSourceProblems ignores it, stageSourceGaps names it.
 * door-stages.test.mjs fails on a stage with no row, on a compiling or packaging row that has none of these, and on a gap that is
 * not in its expected list.
 */
export const STAGE_SOURCES = Object.freeze({
  check: { compiles: false, packages: false, source: "snapshot" },
  compile: { compiles: true, packages: false, source: "snapshot" },
  // Implemented in code (row 7.04: build-release.mjs, release.mjs, cli/right-release.mjs, release-source-identity.mjs): right-release builds and seals the selected commit from the snapshot. `witnessed` becomes true (and `pending` goes) when door-package.test.mjs passes on Linux and the native fixture witness passes on the Mac and the Dell (dev broker).
  package: { compiles: true, packages: true, source: "snapshot", implemented: true, witnessed: false, pending: "route implemented in code (rows 7.04 and 7.04c: release-artifact-path.mjs binds each Cargo manifest's target, one resolver reads every declared artifact path); awaiting 7.04c green on L, M and D: the dev-broker rows need the snapshot carrier (rows 7.20 to 7.24) and RustRight's item-4 dev pins" },
  // Narrowed by decision 2 (7.06 correction): qualification rebuilds nothing, but its source reads (config, QA plan, test code) go through the split.
  qualify: { compiles: false, packages: false, source: "snapshot", implemented: true, witnessed: false, pending: "qa --stage installed takes its identity from the split (release-qa-command.mjs, row 7.04); awaiting its tests on every host" },
  release: { compiles: false, packages: false, source: "publisher", basis: "--to release runs only in the trusted publisher on hosted CI with its normal checkout (docs/GATE-CONTRACT.md), and only uploads sealed bytes" },
  dev: { compiles: true, packages: false, source: "snapshot" },
  test: { compiles: true, packages: false, source: "snapshot" },
  stage: { compiles: false, packages: false, source: "sealed", basis: "stage packs the sealed directory and its receipt into the publisher's draft release; it compiles nothing" },
  // Row 7.06a: a unit stage builds the selected commit's snapshot on every host (runUnitSnapshot); hosted CI keeps its runner checkout, which is the commit.
  "unit:check": { compiles: false, packages: false, source: "snapshot" },
  "unit:compile": { compiles: true, packages: false, source: "snapshot" },
  "unit:package": { compiles: true, packages: true, source: "snapshot" },
  "unit:qualify": { compiles: false, packages: false, source: "snapshot" },
  "unit:release": { compiles: false, packages: false, source: "publisher", basis: "a unit release is dispatched to the trusted publisher (releaseUnit); it compiles nothing here" },
});

/**
 * What a row says about readiness: `implemented` (the executable route exists), `witnessed` (host acceptance proved; absent means nothing is pending for the
 * row) and `pending` (what remains). `appFacingReady` is true only for an implemented route with no pending host acceptance.
 */
export function stageReadiness(stage, table = STAGE_SOURCES) {
  const row = table[stage];
  if (!row) return null;
  const implemented = row.source === "snapshot" ? row.implemented !== false : true;
  const pending = typeof row.pending === "string" && row.pending ? row.pending : null;
  return { stage, implemented, witnessed: pending === null && row.witnessed !== false, pending, appFacingReady: implemented && pending === null && row.witnessed !== false };
}

/** Rows of `table` that compile or package without a snapshot source, a stated exclusion basis or a registered gap. */
export function stageSourceProblems(table = STAGE_SOURCES) {
  const out = [];
  for (const [stage, row] of Object.entries(table)) {
    if (!row.compiles && !row.packages) continue;
    if (row.source === "snapshot") continue;
    if (typeof row.basis === "string" && row.basis && ["sealed", "publisher"].includes(row.source)) continue;
    if (row.source === "checkout" && typeof row.gap === "string" && row.gap) continue;
    out.push({ stage, problem: `${stage} compiles or packages and its source is ${JSON.stringify(row.source)} with no snapshot source, exclusion basis or registered gap` });
  }
  return out;
}

/** The registered breaches in `table`: stages that compile or package from the checkout. */
export function stageSourceGaps(table = STAGE_SOURCES) {
  return Object.entries(table).filter(([, row]) => row.source === "checkout" && (row.compiles || row.packages)).map(([stage]) => stage);
}

/** The stages a run to `target` covers, in order; null when `target` is not a stage. */
export function stagesTo(target) {
  const i = STAGES.indexOf(target);
  return i < 0 ? null : STAGES.slice(0, i + 1);
}

/** The door's state directory under the repository's git common dir. */
export function gateStateDir(commonDir) {
  return path.join(commonDir, "rightkit-gate");
}

/**
 * Starts a door run: writes the run file and returns { nonce, file, env }. The caller removes it with endRun. `source` is the
 * commit-only build's source split (source-split.mjs): { checkout, snapshot, commit, tree, appDir }, written into the run so a
 * right-release child can learn which snapshot it may read; its env then names the checkout the run file lives under.
 */
export function startRun({ commonDir, commit, tree, stage, source, pid = process.pid, write = fs.writeFileSync, mkdir = fs.mkdirSync }) {
  const nonce = randomBytes(16).toString("hex");
  const dir = path.join(gateStateDir(commonDir), "runs");
  mkdir(dir, { recursive: true });
  const file = path.join(dir, `${nonce}.json`);
  write(file, `${JSON.stringify({ pid, commit, tree, stage, ...(source ? { source } : {}), startedAt: new Date().toISOString() })}\n`);
  return { nonce, file, env: { [RUN_ENV]: nonce, ...(source ? { [CHECKOUT_ENV]: source.checkout } : {}) } };
}

export function endRun(run, rm = fs.rmSync) {
  if (run?.file) rm(run.file, { force: true });
}

/**
 * Whether this process was launched by a live door run for `commit`. Returns null when it was, else the refusal message. A run file
 * whose pid is gone, or that names another commit, is no run.
 */
export function requireDoorRun({ env = process.env, commonDir, commit, readFile = (f) => fs.readFileSync(f, "utf8"), alive = pidAlive, command = "this command" }) {
  const refusal = `${command} runs only through the one door: rightkit-gate --to <check|compile|package|qualify|release> (it runs class A first, then every stage in order)`;
  const found = readDoorRun({ env, commonDir, readFile, alive });
  if (found.problem === "none") return refusal;
  if (found.problem === "unread") return `${refusal}; RK_GATE_RUN names no live door run`;
  if (found.problem === "ended") return `${refusal}; the door run that launched it has ended`;
  if (commit && found.run.commit !== commit) return `${refusal}; the door run is for ${String(found.run.commit).slice(0, 12)}, HEAD is ${commit.slice(0, 12)}`;
  return null;
}

/** The live door run RK_GATE_RUN names: { run } or { problem: "none" | "unread" | "ended" }. */
export function readDoorRun({ env = process.env, commonDir, readFile = (f) => fs.readFileSync(f, "utf8"), alive = pidAlive }) {
  const nonce = env[RUN_ENV];
  if (!nonce || !/^[0-9a-f]{32}$/.test(nonce)) return { problem: "none" };
  let run;
  try { run = JSON.parse(readFile(path.join(gateStateDir(commonDir), "runs", `${nonce}.json`))); } catch { return { problem: "unread" }; }
  if (!alive(run.pid)) return { problem: "ended" };
  return { run };
}

function pidAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try { process.kill(pid, 0); return true; } catch (e) { return e.code === "EPERM"; }
}

/**
 * The receipt file for `commit` and the app it was run for: a repository with one app at its root keeps `<commit>.json`; an app in a
 * nested directory (CodeRight's apps/coderight-tauri and apps/coderight-ios, CutRight's apps/cutright, HeardRight's tauri-app-next)
 * gets `<commit>--<dir, _ as _u, / as __>.json`, so two apps of one repository never share or overwrite a receipt (Astra, 2026-10-26).
 */
export function receiptKey(commit, app = ".") {
  // Reversible: "_" becomes "_u" before "/" becomes "__", so apps/a__b (apps__a_u_ub) and apps/a/b (apps__a__b) never share a key.
  return app && app !== "." ? `${commit}--${app.replaceAll("_", "_u").replaceAll("/", "__")}` : commit;
}

/** Reads the receipt for `commit` (and `app`), or null. */
export function readReceipt({ commonDir, commit, app = ".", readFile = (f) => fs.readFileSync(f, "utf8") }) {
  try {
    const r = JSON.parse(readFile(path.join(gateStateDir(commonDir), "receipts", `${receiptKey(commit, app)}.json`)));
    if (r.schema === RECEIPT_SCHEMA && r.commit === commit && (r.app ?? ".") === (app ?? ".")) return r;
  } catch { /* none yet */ }
  return null;
}

export function writeReceipt({ commonDir, receipt, write = (file, data) => writeFileAtomic(file, data), mkdir = fs.mkdirSync }) {
  const dir = path.join(gateStateDir(commonDir), "receipts");
  mkdir(dir, { recursive: true });
  write(path.join(dir, `${receiptKey(receipt.commit, receipt.app ?? ".")}.json`), `${JSON.stringify(receipt, null, 2)}\n`);
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

/** Same source: tree, lockfiles and gate digest equal (the host's toolchain and executor may differ). */
export function sameSource(a, b) {
  return Boolean(a && b) && a.tree === b.tree && a.gate === b.gate && JSON.stringify(a.locks) === JSON.stringify(b.locks);
}

/** Entries a compile-output census walks before it gives up (and the compile is then run again, warm, rather than trusted). */
export const OUTPUT_CENSUS_LIMIT = 250000;

/**
 * What the compile stage produced, as the door can prove it later (GB-13, GATE E6/E7/E16): for each output directory the broker's
 * receipt (or the direct host's resolution) named, every file's relative path and size, digested together. `truncated` means the walk
 * hit `limit`; such a census never proves anything. A missing directory is recorded as missing, never as empty.
 */
export function outputCensus(dirs = [], { limit = OUTPUT_CENSUS_LIMIT, readdir = (d) => fs.readdirSync(d, { withFileTypes: true }), stat = (f) => fs.statSync(f) } = {}) {
  const unique = [...new Set(dirs.filter((d) => typeof d === "string" && d))].sort();
  const lines = []; let truncated = false; const missing = [];
  for (const dir of unique) {
    const stack = [""];
    let exists = true;
    while (stack.length && !truncated) {
      const rel = stack.pop();
      let entries;
      try { entries = readdir(path.join(dir, rel)); } catch { if (rel === "") exists = false; continue; }
      for (const e of entries) {
        const next = rel ? `${rel}/${e.name}` : e.name;
        if (e.isDirectory()) { stack.push(next); continue; }
        let size = -1; try { size = stat(path.join(dir, next)).size; } catch { /* vanished while walking: recorded as -1 */ }
        lines.push(`${dir}\0${next}\0${size}`);
        if (lines.length > limit) { truncated = true; break; }
      }
    }
    if (!exists) missing.push(dir);
  }
  lines.sort();
  return { dirs: unique, entries: lines.length, truncated, missing, digest: sha256(lines.join("\n")) };
}

/** The environment a Cargo compile reads that picks its profile, target or flags: part of what a compile result is valid for. */
const COMPILE_ENV = /^(CARGO_(?!HOME$|TARGET_DIR$|BUILD_TARGET_DIR$|REGISTRIES_|NET_|HTTP_|TERM_)|RUSTFLAGS$|RUSTDOCFLAGS$|RUSTC(_|$)|RUSTUP_TOOLCHAIN$)/;
export function compileKeyOf(env = {}) {
  const picked = Object.entries(env).filter(([k, v]) => COMPILE_ENV.test(k) && v !== undefined).sort(([a], [b]) => a.localeCompare(b));
  return sha256(JSON.stringify(picked));
}

/** What a passed compile stage records about its outputs: the directories, their census and the key of the compile environment. */
export function compileOutputsRecord({ dirs = [], env = {}, census = outputCensus }) {
  const c = census(dirs);
  return { dirs: c.dirs, entries: c.entries, digest: c.digest, truncated: c.truncated, key: compileKeyOf(env) };
}

/**
 * Whether a recorded compile still stands for the outputs on disk now: null when it does, else the reason it does not. A record with no
 * `outputs` (a receipt from before this binding) proves nothing and does not stand.
 */
export function compileReuseProblem(recorded, { env = {}, census = outputCensus } = {}) {
  const out = recorded?.outputs;
  if (!out || typeof out !== "object" || !Array.isArray(out.dirs) || typeof out.digest !== "string") return "the compile result records no outputs";
  if (out.truncated) return "the recorded output census was incomplete";
  if (out.key !== compileKeyOf(env)) return "the compile environment (profile, target or flags) differs from the recorded one";
  const now = census(out.dirs);
  if (now.missing.length) return `output directory ${now.missing[0]} is gone`;
  if (now.truncated) return "the output census is too large to prove";
  if (now.digest !== out.digest || now.entries !== out.entries) return "the compile outputs on disk differ from the recorded census (a required output was removed, replaced or added)";
  return null;
}

/** The sealed release directory build-release wrote for `commit` on `platform`: { id, manifest, digest } or null. */
export function sealedFor({ root, commit, platform, readdir = fs.readdirSync, readFile = (f) => fs.readFileSync(f) }) {
  const base = path.join(root, ".right-release", "sealed");
  let ids = [];
  try { ids = readdir(base).filter((id) => id.endsWith(`-${commit.slice(0, 8)}`)); } catch { return null; }
  for (const id of ids.sort()) {
    const file = path.join(base, id, platformDirOf(platform), "release-manifest.json");
    try { const bytes = readFile(file); return { id, manifest: file, digest: sha256(bytes) }; } catch { /* not this platform */ }
  }
  return null;
}
