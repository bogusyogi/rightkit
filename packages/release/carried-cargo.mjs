// The carried seam of right-release's own broker calls on a broker host (rightkit#1301; GATE-CONTRACT "Commit-only build", item 7 rows
// 7.20 and 7.24-R). ONE model: every compile of a snapshot stage is either a carried request of its own or nested under one.
//
// A right-release child of a snapshot door stage (dev, test, stage-sidecar, build, qa) reaches the broker in two carried ways:
//   - its Cargo calls (`runCargoSync`, which stage-sidecar, cargo-target and development use; a `cargo` step through build-launch.mjs):
//     this module's `runCarriedCargo`, one carried request per call through `carryBrokerCargo` (snapshot-broker.mjs): fresh nonce,
//     `primaryCwd` from the gate's snapshot-to-checkout map, the signed receipt verified, a ledger record either way. The call runs from the
//     manifest's directory and passes no `--manifest-path` and no output directory (the RustRight client refuses to rewrite an attested
//     call, and the broker pins the output), so `--manifest-path <p>` is turned into a working directory here, and the output directory the
//     caller needs is the receipt's `outputDir`, never a carrier-less `cargo metadata`;
//   - its pnpm, node and QA steps (build-launch.mjs routes them to carried-adapter-run.mjs): `runCarriedAdapter`, one carried ADAPTER
//     request per step through `carryBrokerAdapter` (rustright#190, snapshot-adapter-v1). The Tauri CLI, and any other child that compiles,
//     runs NESTED under that job (the broker attributes each compile by the parent's capability token); the broker signs the list of those
//     children (request id, argv, cwd, exit, outputDir) in the job's receipt, and the gate verifies it. A Tauri bundle's root is under the
//     `outputDir` of the signed child that built it (`carriedOutputDir`), never a directory the child reported.
// There is no PATH wrapper and no stage grant: the carrier is the map and the ledger only, on every platform. The gate hands the child the
// map file through `RIGHTKIT_CARRIED_CARGO_MAP` and reads the ledger after the child ends: a refused, unverified or unfinished call fails
// the stage, and every verified call, nested children included, is verified again from the broker's receipt.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { appendLedger, readLedger, readLedgerDir, SCAN_TOKEN_ENV } from "./cargo-ledger.mjs";
import { SHIM_MAP_SCHEMA, directCargoShape, locateWorkspaceByWalk } from "./cargo-target-shim.mjs";
import { directCargoPath } from "./managed-cargo.mjs";
import { BROKER_REFUSAL_STATUS, carryBrokerAdapter, carryBrokerCargo, verifyBrokerLedger } from "./snapshot-broker.mjs";
import { splitCarriedArgs } from "./snapshot-request.mjs";
import { runSeamAdapterCall, runSeamCargoCall, seamEnvPresent } from "./seam-broker.mjs";

/** The environment variable that names the gate-written map of a snapshot stage's carried Cargo calls. */
export const CARRIED_CARGO_MAP_ENV = "RIGHTKIT_CARRIED_CARGO_MAP";
export const CARRIER_MISSING_CODE = "RK_CARGO_CARRIER_MISSING";

/** `null` (no carrier in this environment), `{ map }` or `{ error }` (a map that cannot be read: the call is refused, never run direct). */
export function carriedCargoMapOf(env = process.env) {
  const file = env?.[CARRIED_CARGO_MAP_ENV];
  if (typeof file !== "string" || !file) return null;
  try {
    const map = JSON.parse(fs.readFileSync(file, "utf8"));
    if (map?.schema !== SHIM_MAP_SCHEMA || map.mode !== "broker" || !map.broker || typeof map.ledgerDir !== "string" || !map.workspaces) throw new Error("not a broker-mode cargo target map");
    return { map };
  } catch (error) { return { error: `the carried cargo map ${file} cannot be read (${error.message})` }; }
}

/** The broker's registered cargo (managed-cargo.mjs directCargoPath), else PATH's without the RustRight shims; null when none exists. */
function directCargoBin(env, platform = process.platform) {
  const pathKey = platform === "win32" ? (Object.keys(env).find((k) => k.toUpperCase() === "PATH") ?? "Path") : "PATH";
  const d = directCargoPath({ env: { ...env, PATH: env[pathKey] }, platform, gateWrappers: true });
  const names = platform === "win32" ? ["cargo.exe"] : ["cargo"];
  for (const dir of String(d.path ?? "").split(platform === "win32" ? ";" : ":").filter(Boolean)) for (const n of names) { const c = path.join(dir, n); if (fs.existsSync(c)) return c; }
  return null;
}

const refused = (code, message) => {
  const text = `rightkit-gate: ${code}: ${message}`;
  return { status: BROKER_REFUSAL_STATUS, signal: null, stdout: "", stderr: `${text}\n`, error: null, refusal: { code, message } };
};

/**
 * One right-release Cargo call, carried. `args` are the cargo arguments (without `cargo`); `options` is the spawn options of the call
 * (`cwd`, `env`, `stdio`). Returns a `spawnSync`-shaped result with `outputDir` (the receipt's) when the call verified.
 */
export function runCarriedCargo(args, options = {}, { map, spawn = spawnSync, err = (m) => process.stderr.write(`${m}\n`) } = {}) {
  const env = options.env ?? process.env;
  const cwd = path.resolve(options.cwd ?? process.cwd());
  const token = env[SCAN_TOKEN_ENV];
  let from = cwd;
  const original = [...args];
  const record = (entry) => appendLedger(map.ledgerDir, token, entry);
  const base = { id: `${process.pid}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`, mode: "broker", cwd, argv: original };
  const refuse = (code, message) => {
    try { record({ ...base, status: "refused", code, message }); } catch { /* the refusal is returned either way */ }
    err(`rightkit-gate: ${code}: ${message}`);
    return refused(code, message);
  };
  // The same function stage-commands.mjs uses to say what a stage's signed receipts must show (splitCarriedArgs, snapshot-request.mjs).
  const split = splitCarriedArgs(original);
  if (split.refusal) return refuse(split.refusal.code, split.refusal.message);
  const { words } = split;
  if (split.manifestPath !== null) from = path.dirname(path.resolve(cwd, split.manifestPath));
  let workspace;
  try {
    const manifest = locateWorkspaceByWalk(null, fs.realpathSync.native(from));
    workspace = fs.realpathSync.native(path.dirname(manifest));
  } catch (error) { return refuse("RK_CARGO_UNBOUND_WORKSPACE", `\`cargo ${words.join(" ")}\` from ${from}: its Cargo workspace cannot be resolved (${error.message}); a snapshot stage's Cargo calls run in the commit's workspaces`); }
  if (!Object.hasOwn(map.workspaces, workspace)) return refuse("RK_CARGO_UNBOUND_WORKSPACE", `\`cargo ${words.join(" ")}\` builds the Cargo workspace ${workspace}, which is not one this snapshot stage resolved (${Object.keys(map.workspaces).join(", ") || "none"})`);
  const capture = options.stdio === "inherit" ? "inherit" : "capture";
  // GATE-CONTRACT GC-A11: a call that compiles nothing (metadata, fmt --check, deny, audit, package --list) runs directly with the broker's
  // registered cargo into its own scratch target, never carried: the broker refuses non-compiling requests (rustright a2972f0), which
  // stopped every unit stage at its `cargo fmt -p <crate> --check` step on the Mac and Dell (2026-11-03). Recorded like the cargo
  // wrapper's direct calls (cargo-target-shim.mjs), so the stage's ledger check sees it ran.
  // Only the exact non-writing shapes the cargo wrapper accepts run directly (carriedScanRefusal: `fmt -p <unit> --check`, metadata with
  // --locked/--frozen, `deny ... check`, `audit --db <dir> --no-fetch`), and only when the first word is the subcommand itself (no
  // `+toolchain` or global option in front). Anything else still carries, and the broker refuses it: a direct call never writes the
  // snapshot (no `fmt` without --check, no lock rewrite, no `deny init`, no `audit fix`).
  const directShape = directCargoShape(words);
  if (directShape) {
    const realCargo = directCargoBin(env);
    if (!realCargo) return refuse("RK_CARGO_DIRECT_MISSING", `\`cargo ${words.join(" ")}\` compiles nothing and runs directly (GC-A11), but no cargo was found on the broker's toolchain or PATH`);
    let scratch;
    try { scratch = fs.mkdtempSync(path.join(os.tmpdir(), "rk-carried-read-")); }
    catch (error) { return refuse("RK_CARGO_DIRECT_SCRATCH", `cannot create a scratch target for \`cargo ${words.join(" ")}\` (${error.message})`); }
    const { CARGO_TARGET_DIR: _t, CARGO_BUILD_TARGET_DIR: _t2, ...rest } = env; void _t; void _t2;
    let r;
    try { r = spawn(realCargo, words, { cwd: from, env: { ...rest, CARGO: realCargo, CARGO_TARGET_DIR: scratch }, stdio: capture === "inherit" ? "inherit" : "pipe", encoding: "utf8", maxBuffer: options.maxBuffer ?? 1 << 28 }); }
    finally { try { fs.rmSync(scratch, { recursive: true, force: true }); } catch { /* temporary */ } }
    if (r.error) return refuse("RK_CARGO_DIRECT_FAILED", `${realCargo} did not start (${r.error.message})`);
    try { record({ ...base, status: "ran", direct: true, exit: r.status ?? null }); } catch { /* the result is returned either way */ }
    return { status: r.status ?? 1, signal: r.signal ?? null, stdout: r.stdout ?? "", stderr: r.stderr ?? "", error: null };
  }
  // The seam (reset plan section 3): a compile from a stage child whose environment carries the two seam variables is `rustright cargo <words>` in its snapshot workspace,
  // judged by its result line and receipt and filed for the parent; it never goes through the carrier, which the broker refuses under a held seam lock.
  if (seamEnvPresent(env)) {
    // The pending row goes in BEFORE the broker sees the call: a stage child killed or cancelled during the call leaves this row unsettled, so the door keeps its lock.
    // A pending row that cannot be written stops the call before submission (nothing is admitted that the parent cannot see).
    try { record({ ...base, mode: "seam", workspace: map.workspaces[workspace], status: "pending", settled: false }); }
    catch (error) { return refuse("RK_CARGO_LEDGER", `the seam call cannot be filed for the parent before it is sent (${error.message}); nothing was submitted`); }
    const { result, ledger } = runSeamCargoCall(words, { cwd: from, env, stdio: capture === "inherit" ? "inherit" : "pipe", client: map?.broker?.client ?? "rustright", spawnSync: spawn });
    try { record({ ...base, mode: "seam", workspace: map.workspaces[workspace], ...ledger }); }
    catch (error) { return { ...result, status: result.status === 0 ? 1 : result.status, error: null, refusal: { code: "RK_CARGO_LEDGER", message: `the seam call's outcome could not be filed for the parent (${error.message})` } }; }
    if (result.refusal) err(`rightkit-gate: ${result.refusal.code}: ${result.refusal.message}`);
    return result;
  }
  const result = carryBrokerCargo({ words, cwd: from, env, section: map.broker, workspace, checkoutWorkspace: map.workspaces[workspace], record: (entry) => record({ ...entry, id: base.id }), spawn, err, stdio: capture });
  return { status: result.status, signal: null, stdout: result.stdout, stderr: result.stderr, error: null, ...(result.outcome?.status === "verified" ? { outputDir: result.outcome.outputDir, requestId: result.outcome.requestId, requestDigest: result.outcome.requestDigest } : {}), ...(result.outcome?.status === "refused" ? { refusal: { code: result.outcome.code, message: result.outcome.message } } : {}) };
}

/**
 * One right-release pnpm, node or QA step, carried as a broker adapter job. `adapter` is the registered adapter (`rightkit-pnpm`,
 * `rightkit-node`, `rightkit-qa`), `args` the adapter's arguments (what follows `--` in `rustright run <adapter> -- <args>`); `options` is
 * the spawn options of the step (`cwd`, `env`, `stdio`). Returns a `spawnSync`-shaped result with `outputDir` (the job's) and
 * `nestedChildren` (the verified signed list), `requestId` and, for a native build tool, `executable.managedOutputs` (the output mapping the broker signed) when the call verified.
 */
export function runCarriedAdapter(adapter, args, options = {}, { map, spawn = spawnSync, err = (m) => process.stderr.write(`${m}\n`) } = {}) {
  const env = options.env ?? process.env;
  const cwd = path.resolve(options.cwd ?? process.cwd());
  const token = env[SCAN_TOKEN_ENV];
  // The seam (reset plan section 3): a stage child whose environment carries RUSTRIGHT_GATE_COMMIT and RUSTRIGHT_CLASS_A runs its adapter as `rustright run <adapter> -- <args>`
  // (seam-broker.mjs), never as a carried request; the outcome is filed in the stage's ledger so the parent judges it (carrier.finish) and the door's lock release
  // sees an unsettled call (seamUnsettled). This carrier goes with step 5 of the plan.
  if (seamEnvPresent(env)) {
    // The adapter's checkout workspace (the snapshot workspace its cwd is in), so the stage's output readers map its receipt's outputDir: the broker signs the adapter
    // job's pinned target there, and a compile the adapter starts (`tauri build`) uses that same target (rustright normalize_impl.rs, adapter parent).
    const snaps = adapterSnapshotWorkspaces(map.workspaces ?? {}, cwd);
    const row = { id: `${process.pid}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`, mode: "seam", shim: "adapter", cwd, argv: args, adapter, workspaces: snaps.map((w) => map.workspaces[w]) };
    // Pending before submission, as for a seam cargo call: a child killed during the call leaves it unsettled; a pending row that cannot be written submits nothing.
    try { appendLedger(map.ledgerDir, token, { ...row, status: "pending", settled: false }); }
    catch (error) { const message = `the seam call cannot be filed for the parent before it is sent (${error.message}); nothing was submitted`; err(`rightkit-gate: RK_CARGO_LEDGER: ${message}`); return refused("RK_CARGO_LEDGER", message); }
    const { result, ledger } = runSeamAdapterCall(adapter, args, { cwd, env, stdio: options.stdio === "inherit" ? "inherit" : "pipe", client: map?.broker?.client ?? "rustright", spawnSync: spawn });
    try { appendLedger(map.ledgerDir, token, { ...row, ...ledger }); }
    catch (error) { return { ...result, status: result.status === 0 ? 1 : result.status, refusal: { code: "RK_CARGO_LEDGER", message: `the seam call's outcome could not be filed for the parent (${error.message})` } }; }
    return result;
  }
  const result = carryBrokerAdapter({ adapter, words: args, cwd, env, section: map.broker, record: (entry) => appendLedger(map.ledgerDir, token, entry), spawn, err, stdio: options.stdio === "inherit" ? "inherit" : "capture" });
  const verified = result.outcome?.status === "verified";
  return {
    status: result.status, signal: null, stdout: result.stdout, stderr: result.stderr, error: null,
    ...(verified ? { outputDir: result.outcome.outputDir, requestId: result.outcome.requestId, nestedChildren: result.outcome.nestedChildren, ...(result.executable ? { executable: result.executable } : {}) } : {}),
    ...(result.outcome?.status === "refused" ? { refusal: { code: result.outcome.code, message: result.outcome.message } } : {}),
  };
}

/**
 * Writes the carrier of one snapshot stage's children under `base` and returns what to hand them: the map and the ledger only, on every
 * platform (no PATH wrapper, no stage grant: a compile a child makes is nested under a carried adapter job, rustright#190). `checkoutDirs`
 * is the gate's { [cargoDir]: absolute path of that workspace in the primary checkout }; `section` the public part of the gate's
 * snapshot broker (brokerSection). Returns `{ env, ledgerDir, dir, mode, problems(sb), cleanup() }`; `env` is the delta to add.
 */
export function installCarrier({ base, snapshotRoot, checkoutDirs, section, known = {}, realpath = (p) => fs.realpathSync.native(p), onDir = () => {} }) {
  const dir = fs.mkdtempSync(path.join(base, "rk-carrier-"));
  onDir(dir);
  try {
    const ledgerDir = path.join(dir, "ledger");
    fs.mkdirSync(ledgerDir, { recursive: true, mode: 0o700 });
    const workspaces = {};
    for (const [cargoDir, checkoutDir] of Object.entries(checkoutDirs)) workspaces[realpath(path.join(snapshotRoot, ...(cargoDir === "." ? [] : cargoDir.split("/"))))] = checkoutDir;
    const mapFile = path.join(dir, "cargo-target-map.json");
    fs.writeFileSync(mapFile, `${JSON.stringify({ schema: SHIM_MAP_SCHEMA, mode: "broker", identity: path.basename(dir), realCargo: null, shim: null, ledgerDir, workspaces, ...(Object.keys(known).length ? { knownOutputDirs: known } : {}), broker: { ...section, sentinel: path.join(dir, "unbound") } }, null, 2)}\n`, { mode: 0o644 });
    return { env: { [CARRIED_CARGO_MAP_ENV]: mapFile }, ledgerDir, dir, mode: "map", mapFile, shim: null, ...finish(ledgerDir, dir, workspaces) };
  } catch (error) { fs.rmSync(dir, { recursive: true, force: true }); throw error; }
}

/** The checkout workspace root that holds `file`, the deepest one (a workspace can sit inside another's directory). */
const deepestRoot = (roots, file) => roots.filter((r) => file === r || file.startsWith(`${r}${path.sep}`)).sort((a, b) => b.length - a.length)[0] ?? null;

/** The snapshot workspace root that holds `file`, the deepest one, as a key of `workspaces` (`{ [snapshot root]: checkout root }`). */
const deepestSnapshotRoot = (workspaces, file) => {
  let real = file;
  try { real = fs.realpathSync.native(file); } catch { /* as signed */ }
  return Object.keys(workspaces).filter((r) => real === r || real.startsWith(`${r}${path.sep}`)).sort((a, b) => b.length - a.length)[0] ?? null;
};

/**
 * The snapshot Cargo workspaces an adapter job's compiles may build, as keys of `workspaces`: every one at or under its cwd (a Tauri app's `tauri build` runs from the app
 * root and compiles its `src-tauri`, even when the app itself sits inside an outer workspace), else the deepest one its cwd is in. The broker pins every compile an adapter
 * job starts to that job's own target (rustright normalize_impl.rs: an adapter parent's target), so the receipt's outputDir is each of these workspaces' output.
 */
function adapterSnapshotWorkspaces(workspaces, cwd) {
  let real = cwd;
  try { real = fs.realpathSync.native(cwd); } catch { /* as given */ }
  const under = Object.keys(workspaces).filter((r) => r === real || r.startsWith(`${real}${path.sep}`)).sort();
  if (under.length) return under;
  const inside = deepestSnapshotRoot(workspaces, cwd);
  return inside ? [inside] : [];
}

/** The verified calls' signed output directories: a cargo call's, an adapter job's own and each of its nested children's. */
const outputsOf = (entry) => [
  ...(typeof entry.outputDir === "string" ? [entry.outputDir] : []),
  ...(entry.shim === "adapter" && Array.isArray(entry.nestedChildren) ? entry.nestedChildren.map((c) => c.outputDir).filter((d) => typeof d === "string") : []), // a child that compiles nothing has outputDir null
];

function finish(ledgerDir, dir, workspaces = {}) {
  const checkoutRoots = Object.values(workspaces);
  return {
    /** Every call of the stage that is not a completed, verified success, and every verified call the gate cannot verify again itself. */
    async problems(sb, opts = {}) {
      const { entries, problems } = readLedgerDir(ledgerDir);
      const out = [...problems];
      if (sb) out.push(...await verifyBrokerLedger(entries, sb, opts));
      return out;
    },
    /** The seam calls of the stage's children that have no settled outcome (the door keeps its snapshot lock when any exist; reset plan section 3). */
    // Every file of the stage's ledger (a child that carried a scan token files under it), and every record that cannot be read as a call: its owner is unknown.
    seamUnsettled() { const { entries, malformed } = readLedgerDir(ledgerDir); return entries.filter((e) => e.mode === "seam" && e.settled !== true).length + malformed; },
    /** The verified calls' output directories, in call order (the receipts'; an adapter job's nested children's included). */
    outputDirs() { return readLedgerDir(ledgerDir).entries.filter((e) => e.status === "verified").flatMap(outputsOf); },
    /** The verified compiles' output directories by checkout workspace root: `[[root, outputDir]]`, the last call of a workspace winning. A nested child counts for the workspace its cwd is in. */
    outputsByWorkspace() {
      const out = new Map();
      for (const e of readLedgerDir(ledgerDir).entries) {
        if (e.status !== "verified") continue;
        // A seam adapter job (reset plan section 3) has no signed child list: its receipt's outputDir is the target its own compiles used, filed with its workspace.
        if (e.shim === "adapter" && e.mode === "seam") {
          for (const ws of Array.isArray(e.workspaces) ? e.workspaces : []) if (typeof e.outputDir === "string" && typeof ws === "string") out.set(ws, e.outputDir);
          continue;
        }
        if (e.shim === "adapter") {
          for (const child of Array.isArray(e.nestedChildren) ? e.nestedChildren : []) {
            const snap = typeof child.cwd === "string" ? deepestSnapshotRoot(workspaces, child.cwd) : null;
            if (snap && typeof child.outputDir === "string") out.set(workspaces[snap], child.outputDir);
          }
          continue;
        }
        if (typeof e.outputDir !== "string" || typeof e.workspace !== "string") continue;
        const root = deepestRoot(checkoutRoots, e.workspace);
        if (root) out.set(root, e.outputDir);
      }
      return [...out];
    },
    cleanup() { fs.rmSync(dir, { recursive: true, force: true }); },
  };
}

/** The ids of the calls the stage's ledger holds now, whatever their outcome (a snapshot to diff after a step), or an empty set when there is no carrier. */
export function carriedLedgerIds(env = process.env) {
  const carried = carriedCargoMapOf(env);
  if (!carried?.map) return new Set();
  try { return new Set(readLedger(carried.map.ledgerDir, env[SCAN_TOKEN_ENV]).entries.map((e) => e.id)); } catch { return new Set(); }
}

/**
 * The output directories of the verified calls added to the ledger since `before` (carriedLedgerIds): a cargo call's receipt, an adapter job's own and
 * each of its nested children's, in call order. A call that was refused, unverified or unfinished contributes none.
 */
export function carriedOutputDirsSince(env = process.env, before = new Set()) {
  const carried = carriedCargoMapOf(env);
  if (!carried?.map) return [];
  let entries = [];
  try { entries = readLedger(carried.map.ledgerDir, env[SCAN_TOKEN_ENV]).entries; } catch { return []; }
  return [...new Set(entries.filter((e) => e.status === "verified" && !before.has(e.id)).flatMap(outputsOf))];
}

/**
 * The output directory the broker signed for the carried ADAPTER job of `adapter` (a native tool such as `rightkit-xcodebuild`) added to the ledger since `before`, or null: the last such
 * verified call's own `outputDir` (the one managed root the adapter owns). A call that was refused, unverified or unfinished names none.
 */
export function carriedAdapterOutputDir(env, adapter, before = new Set()) {
  const carried = carriedCargoMapOf(env);
  if (!carried?.map) return null;
  let entries = [];
  try { entries = readLedger(carried.map.ledgerDir, env[SCAN_TOKEN_ENV]).entries; } catch { return null; }
  const mine = entries.filter((e) => e.status === "verified" && e.shim === "adapter" && e.adapter === adapter && !before.has(e.id) && typeof e.outputDir === "string");
  return mine.length ? mine.at(-1).outputDir : null;
}

/**
 * The output directory of the workspace a manifest belongs to, from a signed receipt, or null: a verified call this child already made
 * in that workspace (a build, whose receipt names the broker's pinned `outputDir`, or a nested child of a carried adapter job, whose entry in
 * the job's signed list names the compile's own `outputDir`), else one an earlier stage of the door run
 * verified (the map's `knownOutputDirs`, seeded by the gate). Asking Cargo for it (a read-only `metadata` call) is a snapshot request
 * the broker does not accept today (rightkit#1301 F1), so a caller asks here first and only then falls back to a carried call.
 */
export function carriedOutputDir(env, manifestPath, { preferNested = false } = {}) {
  const carried = carriedCargoMapOf(env);
  if (!carried?.map) return null;
  const { map } = carried;
  let root;
  try {
    const manifest = locateWorkspaceByWalk(null, fs.realpathSync.native(path.dirname(path.resolve(manifestPath))));
    root = map.workspaces[fs.realpathSync.native(path.dirname(manifest))];
  } catch { return null; }
  if (!root) return null;
  let entries = [];
  try { entries = readLedger(map.ledgerDir, env[SCAN_TOKEN_ENV]).entries; } catch { entries = []; }
  // The signed output directories of this workspace's compiles, in call order: a carried cargo call's receipt, or a nested child of a
  // carried adapter job (the Tauri CLI's own compile), by the directory the child ran in.
  const mine = []; const nested = [];
  for (const e of entries) {
    if (e.status !== "verified") continue;
    // A seam adapter job's receipt names the one target the adapter and every compile it starts used (rustright normalize_impl.rs: an adapter parent's own target),
    // so for a Tauri bundle it stands where the legacy carrier's signed nested child stood.
    if (e.shim === "adapter" && e.mode === "seam") {
      if (typeof e.outputDir === "string" && Array.isArray(e.workspaces) && e.workspaces.includes(root)) { mine.push(e.outputDir); nested.push(e.outputDir); }
    } else if (e.shim === "adapter") {
      for (const child of Array.isArray(e.nestedChildren) ? e.nestedChildren : []) {
        const snap = typeof child.cwd === "string" ? deepestSnapshotRoot(map.workspaces, child.cwd) : null;
        if (snap && map.workspaces[snap] === root && typeof child.outputDir === "string") { mine.push(child.outputDir); nested.push(child.outputDir); }
      }
    } else if (typeof e.outputDir === "string" && typeof e.workspace === "string" && deepestRoot(Object.values(map.workspaces), e.workspace) === root) mine.push(e.outputDir);
  }
  // A Tauri bundle's root is under the compile the Tauri CLI itself made: the nested child's, when one is signed, whatever carried Cargo call came after it.
  if (preferNested && nested.length) return nested.at(-1);
  if (mine.length) return mine.at(-1);
  const known = map.knownOutputDirs?.[root];
  return typeof known === "string" && path.isAbsolute(known) ? known : null;
}
