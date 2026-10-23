#!/usr/bin/env node
// rk-npm-preflight: the local, broker-admitted proof for ONE npm package before a hosted dispatch (the npm twin of
// rk-preflight.mjs, kept in its own file so the crate helper and the source-binding work stay disjoint).
//
// Stages, in order, every one refusing before the next:
//   1. tool parity (the same pins as the crate pre-flight: actionlint, shellcheck, rustfmt for the scans module);
//   2. the hosted `scans` job replayed on this tree with PUBLISH_REGISTRY=npm and the unit scope;
//   3. the hosted plan job's own contract commands, exactly as publish.yml runs them;
//   4. the npm rows, each through the broker's registered `rightkit-pnpm` adapter (node + pinned pnpm.mjs) with an
//      attestation nonce, in the order the hosted verify job runs them: `install --frozen-lockfile --store-dir <pin>`,
//      the workspace build closure (select.mjs pnpmBuildClosureArgs), then `--dir <package dir> run test`.
// Every row is verified against the broker's signed receipt: enrolled key, host and broker digest, nonce, job id,
// adapter id, node digest, pnpm script digest, argv, SUCCEEDED/0, request digest, then `rustright verify-receipt`.
// A row with no broker request, a refused attestation, or a changed tree is RED. The receipt is written per SHA
// under the same receipts root as the crate pre-flight, schema rk-preflight.v2, registry "npm", mode "local-test".
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { checkTools, planCommandsFromWorkflow, readJson, SCHEMA, verifyAttestation, brokerRequestIds, DEFAULT_OUT } from "./rk-preflight.mjs";
import { pnpmBuildClosureArgs } from "./select.mjs";
import { closureAgeExceptions, validateClosureApproval } from "./npm-recipe-consumer.mjs";

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
export const ADAPTER = "rightkit-pnpm";

export function parseArgs(argv) {
  const opts = { out: DEFAULT_OUT, budgetMin: 15 };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const take = () => {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("--")) throw new Error(`${arg} needs a value`);
      i += 1;
      return value;
    };
    if (arg === "--registry") opts.registry = take();
    else if (arg === "--unit") opts.unit = take();
    else if (arg === "--out") opts.out = path.resolve(take());
    else if (arg === "--budget-min") opts.budgetMin = Number(take());
    else throw new Error(`unknown argument ${arg}`);
  }
  if (opts.registry !== "npm") throw new Error("--registry must be npm (crates use rk-preflight.mjs)");
  if (!/^@rightkit\/[a-z][a-z0-9-]{0,63}$/.test(opts.unit ?? "")) throw new Error("--unit names exactly one @rightkit/* package");
  if (!Number.isFinite(opts.budgetMin) || opts.budgetMin <= 0) throw new Error("--budget-min must be a positive number");
  return opts;
}

/** Workspace packages from pnpm-workspace.yaml's tracked package.json files: name -> { dir, version, scripts, private }. */
export function readPackages(root = ROOT) {
  const out = new Map();
  const dirs = fs.readdirSync(path.join(root, "packages"), { withFileTypes: true }).filter((d) => d.isDirectory()).map((d) => path.posix.join("packages", d.name));
  for (const dir of dirs) {
    const file = path.join(root, dir, "package.json");
    if (!fs.existsSync(file)) continue;
    const pkg = JSON.parse(fs.readFileSync(file, "utf8"));
    if (!pkg.name) continue;
    out.set(pkg.name, { name: pkg.name, dir, version: pkg.version, scripts: pkg.scripts ?? {}, private: Boolean(pkg.private) });
  }
  return out;
}

/** The hosted npm verify's rows for one package, in its order: install, build closure, the package's test script. */
export function packageRows(unit, pkg, { storeDir, timeoutMinutes = 30 } = {}) {
  if (!storeDir) throw new Error("the pnpm store pin (ci/rk-pipeline.json tools.pnpm.<platform>.storeDir) is required");
  if (!pkg.scripts.test) throw new Error(`${unit} has no test script; an npm package without one needs allow_no_test, which this pre-flight never supplies`);
  return [
    { id: "install", kind: "install", args: ["install", "--frozen-lockfile", "--store-dir", storeDir], timeoutMinutes },
    { id: "build-closure", kind: "build", args: pnpmBuildClosureArgs([unit]), timeoutMinutes },
    // The hosted verify has no sibling product checkouts beside rightkit; the local row runs with the same scope so
    // sibling-dependent contract tests (asset-gated, ci/asset-gated.json npm) skip here as they do there.
    // RK_ISOLATED_CHECKOUT (not RIGHTKIT_*: the broker reserves that prefix in adapter environments) is the switch
    // right-suite-contract.test.mjs honours; the adapter's env allowlist names it.
    { id: "test", kind: "test", args: ["--dir", pkg.dir, "run", "test"], env: { RK_ISOLATED_CHECKOUT: "1" }, timeoutMinutes },
  ];
}

/** The hosted verify's receipt step refuses an npm unit without a reviewed PK recipe (ci/consumer-recipes.json npm.<unit>,
 * full-file-hash inventory, scripts/ci/consume.mjs consumeReviewedNpm) or whose recipe files differ from the inventory
 * (scripts/ci/npm-recipe-consumer.mjs). Same refusal before any adapter row: hosted 39889913884 (issues-tracker, no
 * entry) and 39889932622 (legal, stale hash) were refused on both hosts after a GREEN local pre-flight. */
export function npmRecipePolicy(root, unit) {
  const inventoryFile = path.join(root, "ci", "consumer-recipes.json");
  let inventory;
  try { inventory = JSON.parse(fs.readFileSync(inventoryFile, "utf8")); } catch { return { ok: false, reason: "reviewed ci/consumer-recipes.json inventory required" }; }
  const reviewed = inventory.schema === 1 ? inventory.npm?.[unit] : null;
  if (!reviewed || !/^PK\d{2}$/.test(reviewed.pkId ?? "") || typeof reviewed.id !== "string" || !reviewed.files || typeof reviewed.files !== "object") {
    return { ok: false, reason: `missing reviewed PK example (ci/consumer-recipes.json npm.${unit}); generic portability is not PK qualification` };
  }
  for (const [file, sha256] of Object.entries(reviewed.files)) {
    const full = path.join(root, file);
    if (!fs.existsSync(full) || fs.lstatSync(full).isSymbolicLink() || !fs.statSync(full).isFile()) return { ok: false, reason: `reviewed recipe file is missing or not a regular file: ${file}` };
    if (crypto.createHash("sha256").update(fs.readFileSync(full)).digest("hex") !== sha256) return { ok: false, reason: `recipe file differs from reviewed inventory: ${file}` };
  }
  // The hosted verify refuses a closure dependency inside pnpm's release-age window unless it is the exact reviewed
  // exception with an approved byte row (scripts/ci/npm-recipe-consumer.mjs). Hosted 39903647141 refused @rightkit/qa 0.6.1
  // on both hosts for qa-chrome 0.1.11 after a GREEN local pre-flight.
  try {
    if (closureAgeExceptions(reviewed, unit).length) {
      const approved = JSON.parse(fs.readFileSync(path.join(root, "ci", "approved-artifacts.json"), "utf8"));
      validateClosureApproval(approved.artifacts, reviewed, unit);
    }
  } catch (error) { return { ok: false, reason: `reviewed closure exception: ${error.message ?? error}` }; }
  return { ok: true, pkId: reviewed.pkId, id: reviewed.id, files: Object.keys(reviewed.files).length };
}

/** What the signed receipt of an adapter row must bind: the adapter id, this host's node and the pinned pnpm script. */
export function adapterExpectation(pins) {
  return { kind: "adapter", adapter: ADAPTER, executable: { sha256: pins?.nodeSha256 }, script: { path: pins?.pnpmScriptPath, sha256: pins?.pnpmScriptSha256 } };
}

function run(command, args, { cwd, env, timeoutMinutes, logFile }) {
  return new Promise((resolve) => {
    const started = Date.now();
    let out = "";
    const child = spawn(command, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"] });
    const log = fs.createWriteStream(logFile);
    const sink = (chunk) => { const text = chunk.toString(); out += text; log.write(text); };
    child.stdout.on("data", sink);
    child.stderr.on("data", sink);
    let timedOut = false;
    const timer = setTimeout(() => { timedOut = true; child.kill("SIGKILL"); }, timeoutMinutes * 60_000);
    child.on("close", (code) => { clearTimeout(timer); log.end(); resolve({ code: code ?? 1, out, timedOut, seconds: Math.round((Date.now() - started) / 1000) }); });
    child.on("error", (error) => { clearTimeout(timer); log.end(); resolve({ code: 127, out: `${out}\n${error.message}`, timedOut: false, seconds: Math.round((Date.now() - started) / 1000) }); });
  });
}

export async function main(argv, deps = {}) {
  const out = deps.stdout ?? ((line) => process.stdout.write(`${line}\n`));
  const err = deps.stderr ?? ((line) => process.stderr.write(`${line}\n`));
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (error) {
    err(`rk-npm-preflight: ${error.message}`);
    return 2;
  }
  const root = deps.root ?? ROOT;
  const git = (...args) => {
    const r = spawnSync("git", args, { cwd: root, encoding: "utf8" });
    if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr.trim()}`);
    return r.stdout.trim();
  };
  const dirty = git("status", "--porcelain", "--untracked-files=no");
  if (dirty) {
    err(`rk-npm-preflight: tracked files are modified; the pre-flight proves a commit, not a working tree:\n${dirty}`);
    return 2;
  }
  const sha = git("rev-parse", "HEAD");
  const pipeline = JSON.parse(fs.readFileSync(path.join(root, "ci", "rk-pipeline.json"), "utf8"));
  const packages = (deps.readPackages ?? readPackages)(root);
  const pkg = packages.get(opts.unit);
  if (!pkg) { err(`rk-npm-preflight: ${opts.unit} is not a workspace package under packages/`); return 2; }
  const receiptDir = path.join(opts.out, sha);
  fs.mkdirSync(receiptDir, { recursive: true });
  const unitFile = opts.unit.replaceAll("/", "__");
  const logDir = fs.mkdtempSync(path.join(receiptDir, `${unitFile}.logs-`));
  const receipt = { schema: SCHEMA, sha, registry: "npm", unit: opts.unit, version: pkg.version, dir: pkg.dir, startedAt: new Date().toISOString(), host: process.platform, route: "local", ok: false, tools: null, scans: null, planChecks: null, rows: [], mode: "local-test", adapter: ADAPTER };
  const save = () => {
    const file = path.join(receiptDir, `${unitFile}.json`);
    const temp = `${file}.${process.pid}.tmp`;
    fs.writeFileSync(temp, JSON.stringify(receipt, null, 2));
    fs.renameSync(temp, file);
  };
  save();
  const unchanged = () => git("rev-parse", "HEAD") === sha && !git("status", "--porcelain", "--untracked-files=no");
  let rows;
  try {
    rows = packageRows(opts.unit, pkg, { storeDir: pipeline.tools?.pnpm?.[process.platform]?.storeDir });
  } catch (error) {
    receipt.problems = [error.message]; receipt.failedAt = "rows"; save();
    err(`rk-npm-preflight: ${error.message}`);
    return 2;
  }
  receipt.requiredRows = rows.map((r) => r.id);

  // 1. Tool parity (the scans module needs the same pins as for crates).
  const tools = checkTools({ pins: pipeline.tools ?? {} });
  receipt.tools = tools.observed;
  if (tools.problems.length) {
    receipt.problems = tools.problems; save();
    for (const p of tools.problems) err(`rk-npm-preflight: ${p}`);
    return 2;
  }
  const scPath = path.dirname(pipeline.tools.shellcheck[process.platform].path);
  const env = { ...process.env, PATH: `${scPath}${path.delimiter}${process.env.PATH ?? ""}`, RUSTFMT: tools.observed.rustfmt.path, RUSTUP_TOOLCHAIN: pipeline.tools.rustfmt.toolchain };

  // 2. Deterministic scans with the npm unit scope.
  {
    const scansOut = path.join(logDir, "scans");
    fs.rmSync(scansOut, { recursive: true, force: true });
    const r = await run(process.execPath, [path.join(root, "scripts", "ci", "run-deterministic-scans.mjs"), "--content-root", root, "--live-root", root, "--out", scansOut], {
      cwd: root, env: { ...env, PUBLISH_REGISTRY: "npm", PUBLISH_UNITS: opts.unit, GITHUB_SHA: sha }, timeoutMinutes: 20, logFile: path.join(logDir, "scans.log"),
    });
    receipt.scans = { ok: r.code === 0 && !r.timedOut && unchanged(), timedOut: r.timedOut, exit: r.code, seconds: r.seconds, log: path.join(logDir, "scans.log") };
    out(`${receipt.scans.ok ? "PASS" : "FAIL"} scans (${r.seconds}s)`);
    if (!receipt.scans.ok) {
      receipt.failedAt = "scans"; receipt.finishedAt = new Date().toISOString(); save();
      for (const line of r.out.split("\n").filter((l) => /FAIL|refused/.test(l)).slice(0, 12)) err(`  ${line}`);
      return 1;
    }
  }

  // 3. The hosted plan job's contract commands, then the reviewed npm PK recipe (deterministic, no execution).
  {
    const planCommands = planCommandsFromWorkflow(root);
    const started = Date.now();
    let failed = null;
    for (const [i, [command, args]] of planCommands.entries()) {
      const r = await run(command, args, { cwd: root, env, timeoutMinutes: 20, logFile: path.join(logDir, `plan-check-${i + 1}.log`) });
      if (r.code !== 0 || r.timedOut) { failed = { index: i + 1, command: `node ${args.join(" ")}`, exit: r.code, timedOut: r.timedOut, lines: r.out.split("\n").filter((l) => /^not ok|error:|refused/.test(l.trim())).slice(0, 8) }; break; }
    }
    if (!failed) {
      const recipe = npmRecipePolicy(root, opts.unit);
      receipt.npmRecipe = recipe;
      if (!recipe.ok) failed = { index: planCommands.length + 1, command: "npm-recipe policy", exit: 1, timedOut: false, lines: [recipe.reason] };
    }
    receipt.planChecks = { ok: !failed && unchanged(), exit: failed ? failed.exit : 0, timedOut: Boolean(failed?.timedOut), seconds: Math.round((Date.now() - started) / 1000), commands: planCommands.length, failed };
    out(`${receipt.planChecks.ok ? "PASS" : "FAIL"} plan-checks (${receipt.planChecks.seconds}s)`);
    if (!receipt.planChecks.ok) {
      receipt.failedAt = "plan-checks"; receipt.finishedAt = new Date().toISOString(); save();
      if (failed) { err(`  ${failed.command}`); for (const line of failed.lines) err(`  ${line}`); }
      return 1;
    }
  }

  // 4. The npm rows through the broker adapter, each attested.
  const pins = pipeline.attestation?.[process.platform];
  const expect = adapterExpectation(pins);
  for (const row of rows) {
    if (!unchanged()) { receipt.failedAt = "source-changed"; save(); return 2; }
    const args = ["run", ADAPTER, "--", ...row.args];
    const logFile = path.join(logDir, `${row.id.replace(/[^a-z0-9.-]+/gi, "_")}.log`);
    const nonce = crypto.randomBytes(32).toString("hex");
    const r = await run("rustright", args, { cwd: root, env: { ...env, ...(row.env ?? {}), RIGHTKIT_ATTEST_NONCE: nonce }, timeoutMinutes: row.timeoutMinutes, logFile });
    const ids = brokerRequestIds(r.out);
    const attestation = ids.length === 1
      ? (deps.verifyAttestation ?? verifyAttestation)(ids[0], { nonce, argv: row.args, pins, expect, workspace: root })
      : { verified: false, requestId: null, nonce, reason: ids.length ? "more than one broker request for one row" : "no broker request" };
    const ok = r.code === 0 && !r.timedOut && attestation.verified === true && unchanged();
    const entry = { id: row.id, kind: row.kind, cwd: ".", command: `rustright ${args.join(" ")}`, exit: r.code, timedOut: r.timedOut, seconds: r.seconds, brokerRequests: ids, attestation, log: logFile, ok };
    receipt.rows.push(entry);
    out(`${ok ? "PASS" : "FAIL"} ${row.id} (${r.seconds}s${r.timedOut ? ", timed out" : ""})`);
    save();
    if (!ok) {
      receipt.failedAt = row.id; receipt.finishedAt = new Date().toISOString(); save();
      if (!attestation.verified) err(`  attestation: ${attestation.reason}`);
      for (const line of r.out.split("\n").filter((l) => /ERR_PNPM|error|FAIL|not ok/.test(l)).slice(0, 12)) err(`  ${line}`);
      return 1;
    }
  }
  receipt.ok = unchanged();
  receipt.seconds = Math.round((Date.now() - Date.parse(receipt.startedAt)) / 1000);
  receipt.budgetMinutes = opts.budgetMin;
  receipt.overBudget = receipt.seconds > opts.budgetMin * 60;
  receipt.finishedAt = new Date().toISOString();
  save();
  out(`${receipt.ok ? "GREEN" : "RED"} ${opts.unit} at ${sha.slice(0, 12)}: ${receipt.rows.length} row(s), receipt ${path.join(receiptDir, `${unitFile}.json`)}`);
  return receipt.ok ? 0 : 2;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).then((code) => process.exit(code), (error) => { process.stderr.write(`rk-npm-preflight: ${error.message}\n`); process.exit(2); });
}
