// The carried cargo seam of right-release's own Cargo calls on a broker host (rightkit#1301; GATE-CONTRACT "Commit-only build",
// item 7 rows 7.20 and 7.24-R).
//
// A right-release child of a snapshot door stage (dev, test, stage-sidecar, build, qa) reaches Cargo in two ways, and both are carried:
//   - its own calls (`runCargoSync`, which stage-sidecar, cargo-target and development use): this module's
//     `runCarriedCargo`, one carried request per call through `carryBrokerCargo` (snapshot-broker.mjs): fresh nonce, `primaryCwd` from
//     the gate's snapshot-to-checkout map, the signed receipt verified, a ledger record either way. The call runs from the manifest's
//     directory and passes no `--manifest-path` and no output directory (the RustRight client refuses to rewrite an attested call, and
//     the broker pins the output), so `--manifest-path <p>` is turned into a working directory here, and the output directory the
//     caller needs is the receipt's `outputDir`, never a carrier-less `cargo metadata`;
//   - a third-party child's (the Tauri CLI): on POSIX the broker-mode wrapper (cargo-target-shim.mjs) first on PATH, which refuses
//     `--config` and `--target-dir` and ledgers every call; on Windows (no .cmd, .bat or PATHEXT shim) a stage grant
//     (`RUSTRIGHT_SNAPSHOT_GRANT`, snapshot-request.mjs `writeStageGrant`, rustright#188) that the RustRight cargo.exe shim mints
//     per-call carriers from. `cargo about` is one per-call request through this seam.
// The gate hands the child the map file through `RIGHTKIT_CARRIED_CARGO_MAP` and reads the ledger after the child ends: a refused,
// unverified or unfinished call fails the stage, and every verified call is verified again from the broker's receipt.
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { appendLedger, readLedger, SCAN_TOKEN_ENV, UNSCOPED } from "./cargo-ledger.mjs";
import { SHIM_MAP_SCHEMA, installCargoShim, locateWorkspaceByWalk, shimScanEnv } from "./cargo-target-shim.mjs";
import { BROKER_REFUSAL_STATUS, carryBrokerCargo, verifyBrokerLedger } from "./snapshot-broker.mjs";
import { readDeclaredInputs, writeStageGrant } from "./snapshot-request.mjs";

/** The environment variable that names the gate-written map of a snapshot stage's carried Cargo calls. */
export const CARRIED_CARGO_MAP_ENV = "RIGHTKIT_CARRIED_CARGO_MAP";
export const CARRIER_MISSING_CODE = "RK_CARGO_CARRIER_MISSING";

const REDIRECT = /^(--config|-Z|-C)(=|$)|^-[A-Za-z]*[CZ]/;

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
  const words = [];
  let from = cwd;
  const original = [...args];
  const record = (entry) => appendLedger(map.ledgerDir, token, entry);
  const base = { id: `${process.pid}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`, mode: "broker", cwd, argv: original };
  const refuse = (code, message) => {
    try { record({ ...base, status: "refused", code, message }); } catch { /* the refusal is returned either way */ }
    err(`rightkit-gate: ${code}: ${message}`);
    return refused(code, message);
  };
  let atSeparator = false;
  for (let i = 0; i < original.length; i += 1) {
    const w = original[i];
    if (atSeparator) { words.push(w); continue; }
    if (w === "--") { atSeparator = true; words.push(w); continue; }
    if (w === "--manifest-path" || w.startsWith("--manifest-path=")) {
      const value = w === "--manifest-path" ? original[i + 1] : w.slice("--manifest-path=".length);
      if (w === "--manifest-path") i += 1;
      if (typeof value !== "string" || !value) return refuse("RK_CARGO_BROKER_MANIFEST_PATH", "--manifest-path names no manifest");
      from = path.dirname(path.resolve(cwd, value));
      continue;
    }
    if (w === "--target-dir" || w.startsWith("--target-dir=")) return refuse("RK_CARGO_REDIRECT", `\`cargo ${original.join(" ")}\` passes --target-dir; the broker pins a carried call's output directory`);
    if (REDIRECT.test(w)) return refuse("RK_CARGO_REDIRECT", `\`cargo ${original.join(" ")}\` passes ${w}, which moves its working directory, configuration or output`);
    words.push(w);
  }
  let workspace;
  try {
    const manifest = locateWorkspaceByWalk(null, fs.realpathSync.native(from));
    workspace = fs.realpathSync.native(path.dirname(manifest));
  } catch (error) { return refuse("RK_CARGO_UNBOUND_WORKSPACE", `\`cargo ${words.join(" ")}\` from ${from}: its Cargo workspace cannot be resolved (${error.message}); a snapshot stage's Cargo calls run in the commit's workspaces`); }
  if (!Object.hasOwn(map.workspaces, workspace)) return refuse("RK_CARGO_UNBOUND_WORKSPACE", `\`cargo ${words.join(" ")}\` builds the Cargo workspace ${workspace}, which is not one this snapshot stage resolved (${Object.keys(map.workspaces).join(", ") || "none"})`);
  const capture = options.stdio === "inherit" ? "inherit" : "capture";
  const result = carryBrokerCargo({ words, cwd: from, env, section: map.broker, workspace, checkoutWorkspace: map.workspaces[workspace], record: (entry) => record({ ...entry, id: base.id }), spawn, err, stdio: capture });
  return { status: result.status, signal: null, stdout: result.stdout, stderr: result.stderr, error: null, ...(result.outcome?.status === "verified" ? { outputDir: result.outcome.outputDir } : {}), ...(result.outcome?.status === "refused" ? { refusal: { code: result.outcome.code, message: result.outcome.message } } : {}) };
}

/**
 * Writes the carrier of one snapshot stage's children under `base` and returns what to hand them. POSIX: the broker-mode wrapper
 * (installCargoShim) first on PATH and in CARGO for third-party children. Windows: the map and ledger only, no wrapper. `checkoutDirs`
 * is the gate's { [cargoDir]: absolute path of that workspace in the primary checkout }; `section` the public part of the gate's
 * snapshot broker (brokerSection). Returns `{ env, ledgerDir, dir, mode, problems(sb), cleanup() }`; `env` is the delta to add.
 */
export function installCarrier(options) {
  const { grant = null, section, snapshotRoot, checkoutDirs } = options;
  const installed = installCarrierBase(options);
  if (!grant) return installed;
  // A stage grant for the children that reach Cargo without the seam (the Tauri CLI): one file, removed with the stage.
  let written;
  try {
    const realpath = options.realpath ?? ((p) => fs.realpathSync.native(p));
    const allowedWorkspaces = Object.keys(checkoutDirs).map((cargoDir) => realpath(path.join(section.snapshot.path, ...(cargoDir === "." ? [] : cargoDir.split("/")))));
    written = writeStageGrant({ controlRoot: section.controlRoot, primary: grant.primary ?? section.checkout, commit: section.commit, snapshot: section.snapshot, declaredInputs: readDeclaredInputs(section.snapshot.path), handle: section.handle, allowedWorkspaces, maxCalls: grant.maxCalls, ttlMs: grant.ttlMs, now: grant.now, callingGate: grant.callingGate });
  } catch (error) { installed.cleanup(); throw error; }
  const cleanup = () => { fs.rmSync(written.file, { force: true }); installed.cleanup(); };
  return { ...installed, env: { ...installed.env, ...written.env }, grant: { file: written.file, body: written.body, sha256: written.sha256 }, cleanup };
}

function installCarrierBase({ base, snapshotRoot, checkoutDirs, section, known = {}, platform = process.platform, env = process.env, realpath = (p) => fs.realpathSync.native(p), onDir = () => {} }) {
  if (platform !== "win32") {
    const installed = installCargoShim({ base, snapshotRoot, targetDirs: null, broker: { checkoutDirs, section }, realCargo: null, platform, onDir });
    const scan = shimScanEnv(env, installed);
    const delta = Object.fromEntries(Object.entries(scan).filter(([k, v]) => env[k] !== v));
    if (Object.keys(known).length) { const map = JSON.parse(fs.readFileSync(installed.mapFile, "utf8")); fs.writeFileSync(installed.mapFile, `${JSON.stringify({ ...map, knownOutputDirs: known }, null, 2)}\n`, { mode: 0o644 }); }
    return { env: { ...delta, [CARRIED_CARGO_MAP_ENV]: installed.mapFile }, ledgerDir: installed.ledgerDir, dir: installed.dir, mode: "shim", shim: installed.shim, ...finish(installed.ledgerDir, installed.dir, Object.values(checkoutDirs)) };
  }
  const dir = fs.mkdtempSync(path.join(base, "rk-carrier-"));
  onDir(dir);
  try {
    const ledgerDir = path.join(dir, "ledger");
    fs.mkdirSync(ledgerDir, { recursive: true, mode: 0o700 });
    const workspaces = {};
    for (const [cargoDir, checkoutDir] of Object.entries(checkoutDirs)) workspaces[realpath(path.join(snapshotRoot, ...(cargoDir === "." ? [] : cargoDir.split("/"))))] = checkoutDir;
    const mapFile = path.join(dir, "cargo-target-map.json");
    fs.writeFileSync(mapFile, `${JSON.stringify({ schema: SHIM_MAP_SCHEMA, mode: "broker", identity: path.basename(dir), realCargo: null, shim: null, ledgerDir, workspaces, ...(Object.keys(known).length ? { knownOutputDirs: known } : {}), broker: { ...section, sentinel: path.join(dir, "unbound") } }, null, 2)}\n`, { mode: 0o644 });
    return { env: { [CARRIED_CARGO_MAP_ENV]: mapFile }, ledgerDir, dir, mode: "map", shim: null, ...finish(ledgerDir, dir, Object.values(checkoutDirs)) };
  } catch (error) { fs.rmSync(dir, { recursive: true, force: true }); throw error; }
}

/** The checkout workspace root that holds `file`, the deepest one (a workspace can sit inside another's directory). */
const deepestRoot = (roots, file) => roots.filter((r) => file === r || file.startsWith(`${r}${path.sep}`)).sort((a, b) => b.length - a.length)[0] ?? null;

function finish(ledgerDir, dir, checkoutRoots = []) {
  return {
    /** Every call of the stage that is not a completed, verified success, and every verified call the gate cannot verify again itself. */
    async problems(sb) {
      const { entries, problems } = readLedger(ledgerDir, UNSCOPED);
      const out = [...problems];
      if (sb) out.push(...await verifyBrokerLedger(entries, sb));
      return out;
    },
    /** The verified calls' output directories, in call order (the receipts'). */
    outputDirs() { return readLedger(ledgerDir, UNSCOPED).entries.filter((e) => e.status === "verified" && typeof e.outputDir === "string").map((e) => e.outputDir); },
    /** The verified calls' output directories by checkout workspace root: `[[root, outputDir]]`, the last call of a workspace winning. */
    outputsByWorkspace() {
      const out = new Map();
      for (const e of readLedger(ledgerDir, UNSCOPED).entries) {
        if (e.status !== "verified" || typeof e.outputDir !== "string" || typeof e.workspace !== "string") continue;
        const root = deepestRoot(checkoutRoots, e.workspace);
        if (root) out.set(root, e.outputDir);
      }
      return [...out];
    },
    cleanup() { fs.rmSync(dir, { recursive: true, force: true }); },
  };
}

/**
 * The output directory of the workspace a manifest belongs to, from a signed receipt, or null: a verified call this child already made
 * in that workspace (a build, whose receipt names the broker's pinned `outputDir`), else one an earlier stage of the door run
 * verified (the map's `knownOutputDirs`, seeded by the gate). Asking Cargo for it (a read-only `metadata` call) is a snapshot request
 * the broker does not accept today (rightkit#1301 F1), so a caller asks here first and only then falls back to a carried call.
 */
export function carriedOutputDir(env, manifestPath) {
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
  const mine = entries.filter((e) => e.status === "verified" && typeof e.outputDir === "string" && typeof e.workspace === "string" && deepestRoot(Object.values(map.workspaces), e.workspace) === root);
  if (mine.length) return mine.at(-1).outputDir;
  const known = map.knownOutputDirs?.[root];
  return typeof known === "string" && path.isAbsolute(known) ? known : null;
}
