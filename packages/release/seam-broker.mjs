// The seam's one broker control query from the gate side (reset plan section 3): where the broker listens and what identity a query carries, and the
// reclaim judge built on it, and the broker protocol name and runtime identity the control query carries. It imports only kept modules.
import fs from "node:fs";
import path from "node:path";
import { resolveBrokerClient } from "./broker-client.mjs";
import { discoverBrokerSocket } from "./managed-roots.mjs";
import { CLASS_A_ENV, CLASS_A_VERDICT_SCHEMA, GATE_COMMIT_ENV, classAVerdict, judgeSeamOutcome, seamSettled, snapshotStatusAllowsReclaim, snapshotStatusMessage, snapshotStatusSync, writeClassAVerdict } from "./gate-seam.mjs";
import { spawnSync as nodeSpawnSync } from "node:child_process";

/** `rustright_protocol::BUILD_CONTROL_PROTOCOL`: carried in every request envelope, the seam's SNAPSHOT_STATUS query included. */
export const BUILD_CONTROL_PROTOCOL = "rustright.build-control.client.v2";
/** The runtime manifest beside the installed client, whose identity every control request carries. */
export const RUNTIME_MANIFEST_FILE = "runtime-manifest.json";
/** The id of the client's own health probe (`rustright-cli` doctor: PROBE_ID), a by-id STATUS the front gate answers; the declared Linux route reads the answering roots with it. */
export const HANDSHAKE_PROBE_ID = "runtime-health-probe";
export const RUNTIME_MANIFEST_SCHEMA = "rustright.build-control.runtime.v2";
const LEGACY_RUNTIME_MANIFEST_SCHEMA = "rightkit.build-control.runtime.v2";
const isDigest = (v) => typeof v === "string" && /^[0-9a-f]{64}$/.test(v);

/** The runtime identity of a parsed object (`schema` current or legacy, a version, both digests lowercase 64-hex), as RustRight's `RuntimeIdentity::new` validates it; else null. */
export function normalizeRuntimeIdentity(raw) {
  if (raw === null || typeof raw !== "object" || Array.isArray(raw) || ![RUNTIME_MANIFEST_SCHEMA, LEGACY_RUNTIME_MANIFEST_SCHEMA].includes(raw.schema) || typeof raw.version !== "string" || raw.version === "" || !isDigest(raw.clientDigest) || !isDigest(raw.brokerDigest)) return null;
  return { schema: RUNTIME_MANIFEST_SCHEMA, version: raw.version, clientDigest: raw.clientDigest, brokerDigest: raw.brokerDigest };
}

/** The runtime identity a `runtime-manifest.json` text holds: `{ ok: true, identity }` or `{ ok: false, problem }`. */
export function parseRuntimeIdentity(text) {
  let doc;
  try { doc = JSON.parse(text); } catch { return { ok: false, problem: "the runtime manifest is not JSON" }; }
  const identity = normalizeRuntimeIdentity(doc);
  return identity ? { ok: true, identity } : { ok: false, problem: "the runtime manifest is not a runtime identity (schema, version, clientDigest and brokerDigest are required)" };
}

/** Whether an environment carries the seam (both variables set): a stage child's adapter call then goes through the seam, never the carrier. */
export function seamEnvPresent(env) {
  return Boolean(env && typeof env[GATE_COMMIT_ENV] === "string" && /^[0-9a-f]{40}$/.test(env[GATE_COMMIT_ENV]) && typeof env[CLASS_A_ENV] === "string" && path.isAbsolute(env[CLASS_A_ENV]));
}

/**
 * One seam call from a stage child (reset plan section 3), synchronous like the step that makes it: `<client> <argv>` in `cwd` with the inherited
 * environment (the two seam variables already in it). The result line and the durable receipt are judged exactly as a class B row's
 * (judgeSeamOutcome); the tree is not compared, since a stage child knows only the commit. The returned status is never 0 unless the judge passed
 * (a child exiting 0 whose receipt fails its check is a failure). `ledger` is the record the caller files for its parent: `status` "verified"
 * (passed), "refused" (refused before admission), "failed" (executed and failed) or "unsettled" (no settled outcome: no or a bad result line,
 * no durable receipt, a client failure after admission, a spawn error); `settled` is the positive evidence the door's lock release needs.
 */
function runSeamCall(argv, { cwd, env, stdio = "pipe", client = "rustright", spawnSync = nodeSpawnSync, maxBuffer = 1 << 28, fsApi = fs }) {
  const isAdapter = argv[0] === "run";
  const expect = { argv: isAdapter ? argv.slice(3) : argv.slice(1), cwd, adapter: isAdapter ? argv[1] : null };
  if (!seamEnvPresent(env)) throw new TypeError("seam call: the environment carries no seam (RUSTRIGHT_GATE_COMMIT and RUSTRIGHT_CLASS_A)");
  // The broker binds the verdict's workspace to this call's own directory (rustright gate_admission.rs passing_class_a: workspace == cwd relative to the snapshot). The
  // stage hands the root verdict of its scope; a call made in another directory names that directory's verdict, derived from it (fresh review of badf93c, D1).
  let classA;
  try { classA = verdictForCwd(env, cwd, { fsApi }); }
  catch (error) { const message = `the Class A verdict for ${cwd} cannot be named (${error.message}); nothing was submitted`; return { result: { status: 125, signal: null, stdout: "", stderr: `rightkit-gate: RK_SEAM_VERDICT: ${message}\n`, error: null, refusal: { code: "RK_SEAM_VERDICT", message } }, ledger: { status: "refused", settled: true, code: "RK_SEAM_VERDICT", message } }; }
  env = { ...env, [CLASS_A_ENV]: classA };
  const r = spawnSync(client, argv, { cwd, env, stdio: stdio === "inherit" ? ["ignore", "inherit", "pipe"] : "pipe", encoding: "utf8", maxBuffer });
  if (r.error) return { result: { status: 1, signal: null, stdout: "", stderr: String(r.error.message ?? r.error), error: r.error, refusal: { code: "SPAWN_ERROR", message: String(r.error.message ?? r.error) } }, ledger: { status: "unsettled", settled: false, code: "SPAWN_ERROR" } };
  const judged = judgeSeamOutcome({ child: r, stderr: r.stderr ?? "", commit: env[GATE_COMMIT_ENV], tree: null, classA, expect });
  const pass = judged.status === "pass";
  const preAdmissionRefusal = Boolean(judged.result) && !judged.evidence && judged.receiptRead !== true && judged.result.requestId === null;
  const settled = seamSettled(judged);
  const status = pass ? (r.status ?? 0) : (Number.isInteger(r.status) && r.status !== 0 ? r.status : (preAdmissionRefusal ? 125 : 1));
  const evidence = judged.evidence ? { outputDir: judged.evidence.outputDir, targetTriple: judged.evidence.targetTriple, requestId: judged.evidence.requestId, receipt: judged.evidence.receiptPath, receiptSha256: judged.evidence.receiptSha256 } : {};
  return {
    result: { status, signal: r.signal ?? null, stdout: r.stdout ?? "", stderr: r.stderr ?? "", error: null, ...evidence, ...(pass ? {} : { refusal: { code: judged.code, message: judged.detail } }) },
    ledger: { status: pass ? "verified" : !settled ? "unsettled" : preAdmissionRefusal ? "refused" : "failed", settled, ...(pass ? {} : { code: judged.code, message: String(judged.detail ?? "").slice(0, 2000) }), ...evidence },
  };
}

/**
 * The Class A verdict a seam call from `cwd` names: the verdict the environment hands (the root verdict of the door's scope) when its workspace is already `cwd`'s, else
 * the verdict of `cwd`'s snapshot-relative directory derived from it (same commit, tree, repository, scans and scope; gate-seam.mjs verdictForRun derives a nested
 * workspace's verdict the same way). The snapshot is the ancestor of `cwd` named by the verdict's repository key, under the same data root as the verdict directory
 * (`<data>/<snapshots>/<repoKey>` beside `<data>/class-a-verdicts`). A verdict that cannot be read, or is not a passing root verdict of this commit, is returned as handed: the broker refuses it.
 */
export function verdictForCwd(env, cwd, { fsApi = fs } = {}) {
  const file = env[CLASS_A_ENV];
  let v; try { v = JSON.parse(fsApi.readFileSync(file, "utf8")); } catch { return file; } // unreadable: the broker refuses it (RK_CLASS_A_INVALID), with its own code
  if (v?.schema !== CLASS_A_VERDICT_SCHEMA || v.status !== "pass" || v.commit !== env[GATE_COMMIT_ENV] || v.workspace !== "." || typeof v.repoKey !== "string") return file;
  const real = (p) => { try { return fsApi.realpathSync.native(p); } catch { return path.resolve(p); } };
  const dataRoot = real(path.dirname(path.dirname(file)));
  let at = real(cwd); let snapshot = null;
  for (;;) {
    if (path.basename(at) === v.repoKey && path.dirname(path.dirname(at)) === dataRoot) { snapshot = at; break; }
    const up = path.dirname(at); if (up === at) break; at = up;
  }
  if (!snapshot) throw new Error(`${cwd} is not inside the snapshot of repository ${v.repoKey} under ${dataRoot}`);
  const rel = path.relative(snapshot, real(cwd)).split(path.sep).filter(Boolean).join("/");
  const ws = rel || ".";
  if (ws === ".") return file;
  return writeClassAVerdict(path.dirname(file), classAVerdict({ commit: v.commit, tree: v.tree, repoKey: v.repoKey, workspace: ws, scans: v.scans, scope: v.scope ?? "full" }), { fsApi });
}

/** One adapter run through the seam from a stage child: `rustright run <adapter> -- <args>`. Returns `{ result, ledger }` (runSeamCall). */
export function runSeamAdapterCall(adapter, args, options = {}) {
  if (typeof adapter !== "string" || !/^[a-z][a-z0-9-]*$/.test(adapter)) throw new TypeError(`runSeamAdapter: ${JSON.stringify(adapter)} is not an adapter id`);
  return runSeamCall(["run", adapter, "--", ...args], options);
}
/** One cargo call through the seam from a stage child: `rustright cargo <words>`. Returns `{ result, ledger }` (runSeamCall). */
export function runSeamCargoCall(words, options = {}) { return runSeamCall(["cargo", ...words], options); }

/** The spawnSync-shaped result of one seam adapter run (runSeamAdapterCall without the ledger record). */
export function runSeamAdapter(adapter, args, options = {}) {
  if (!seamEnvPresent(options.env ?? process.env)) throw new TypeError("runSeamAdapter: the environment carries no seam (RUSTRIGHT_GATE_COMMIT and RUSTRIGHT_CLASS_A)");
  return runSeamAdapterCall(adapter, args, { ...options, env: options.env ?? process.env, cwd: options.cwd ?? process.cwd() }).result;
}

const isPipe = (endpoint) => /^\\\\\.\\pipe\\/i.test(String(endpoint));

/**
 * Where a control query to the broker goes and what identity it carries: the endpoint RustRight's discovery names (a user-owned socket, or a pipe) and the
 * runtime identity of the manifest beside the installed `client`. `{ ok, endpoint, identity }` or `{ ok: false, problem }`.
 */
export function brokerControlTarget(o, { client = o?.client } = {}) {
  const platform = o?.platform ?? process.platform;
  const env = o?.env ?? process.env;
  const read = o?.readFile ?? ((file) => fs.readFileSync(file, "utf8"));
  if (typeof client !== "string" || !client) return { ok: false, problem: "the RustRight client is not on PATH, so its runtime identity cannot be read" };
  let exe;
  try { exe = resolveBrokerClient(client, { platform, readFile: read }).file; } catch (error) { return { ok: false, problem: `the RustRight client cannot be resolved (${error.message})` }; }
  let real = exe;
  if (platform !== "win32") { try { real = (o?.realpath ?? fs.realpathSync.native)(exe); } catch { real = exe; } }
  const paths = platform === "win32" ? path.win32 : path.posix;
  const manifest = paths.join(paths.dirname(real), RUNTIME_MANIFEST_FILE);
  let text;
  try { text = read(manifest); } catch (error) { return { ok: false, problem: `cannot read the runtime manifest ${manifest} beside the client (${error?.message ?? error})` }; }
  const identity = parseRuntimeIdentity(text);
  if (!identity.ok) return { ok: false, problem: `${manifest}: ${identity.problem}` };
  let endpoint;
  try { endpoint = (o?.discoverEndpoint ?? discoverBrokerSocket)(env, { platform, exists: fs.existsSync }); } catch (error) { return { ok: false, problem: `the broker endpoint cannot be discovered (${error.message})` }; }
  if (!endpoint) return { ok: false, problem: "no broker endpoint is configured (no explicit socket variable and no broker.sock under a managed control root)" };
  if (platform !== "win32" && !isPipe(endpoint)) {
    try {
      const stat = (o?.lstat ?? fs.lstatSync)(endpoint);
      const uid = typeof (o?.uid ?? process.getuid?.()) === "number" ? (o?.uid ?? process.getuid()) : null;
      if (!stat.isSocket()) return { ok: false, problem: `${endpoint} is not a socket, so it is not the broker's endpoint` };
      if (uid !== null && stat.uid !== uid) return { ok: false, problem: `${endpoint} is owned by another user (uid ${stat.uid}), so it is not this user's broker` };
    } catch (error) { return { ok: false, problem: `cannot connect to the broker at ${endpoint} (${error?.code ?? error?.message ?? error})` }; }
  }
  return { ok: true, endpoint, identity: identity.identity };
}

/**
 * The seam's reclaim judge (snapshot.mjs acquireSnapshotLock `judge`): asks the broker SNAPSHOT_STATUS for the dead holder's lock and returns true only on
 * an exact IDLE echo. Everything it needs is bound here, at construction, from values that exist at that moment (`o`, the resolved `client`, the snapshot
 * path): nothing is read from an outer scope at call time (two closure bugs of 2026-11-09 read a const that was out of scope or not yet initialized, and the
 * catch turned them into "no proof"). `query` is injectable for tests; the default crosses the real socket.
 */
/**
 * How long the reclaim judge waits for the broker's SNAPSHOT_STATUS. The broker answers it from a census of every durable completion proof
 * (about 20k on the Mac, 2026-11-09): a direct query answered IDLE in 8.1 s and the former 10 s deadline read as "no proof", which kept a dead
 * holder's lock for good (rightkit#1548). Bounded, never retried: a deadline is still no proof.
 */
export const RECLAIM_QUERY_MS = 60_000;
export function snapshotReclaimJudge({ o, client, snapshotPath, query = snapshotStatusSync, timeoutMs = RECLAIM_QUERY_MS }) {
  if (typeof snapshotPath !== "string" || !path.isAbsolute(snapshotPath)) throw new TypeError("snapshotReclaimJudge: snapshotPath must be absolute");
  const bound = { o, client: typeof client === "string" && client ? client : null, snapshotPath };
  return (holder) => {
    try {
      if (!bound.client) return false;
      const target = brokerControlTarget(bound.o, { client: bound.client });
      if (!target.ok) return false;
      const message = snapshotStatusMessage({ protocol: BUILD_CONTROL_PROTOCOL, runtime: target.identity, snapshot: bound.snapshotPath, owner: holder });
      return snapshotStatusAllowsReclaim(query({ endpoint: target.endpoint, message, timeoutMs }), { snapshot: bound.snapshotPath, owner: holder });
    } catch { return false; }
  };
}
