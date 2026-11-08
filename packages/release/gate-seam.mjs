// The gate's half of the one gate-broker seam (reset plan docs/plans/2026-11-07-gate-broker-reset.md section 3; rightkit#1536, rustright#235).
//
// The gate writes a Class A verdict for the workspace, takes the snapshot lock for the commit, spawns `rustright cargo <argv>` (or
// `rustright run <adapter-id> -- <argv>`) in the snapshot with RUSTRIGHT_GATE_COMMIT and RUSTRIGHT_CLASS_A set, parses exactly one
// `rustright-result <json>` line from the child's stderr, and reads the durable receipt that line names. No carrier file, no nonce,
// no grant, no lease handle, no signature verified here (CI's publication admission verifies the broker's signature).
//
// Dependency-free (node built-ins only). Every function here is pure or takes its I/O as an argument, so the writer, parser and judge
// are tested without a broker; the integration against the installed broker runs after the cutover (plan section 7, step 4).
import { execFileSync as nodeExecFileSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

export const CLASS_A_VERDICT_SCHEMA = "rightkit.class-a-verdict/1";
export const GATE_COMMIT_ENV = "RUSTRIGHT_GATE_COMMIT";
export const CLASS_A_ENV = "RUSTRIGHT_CLASS_A";
export const RESULT_LINE_PREFIX = "rustright-result ";
/** The exit the client reports when nothing was executed (a refusal, a client failure); an executed command keeps its own exit. */
export const NOT_EXECUTED_EXIT = 125;

const SHA1 = /^[0-9a-f]{40}$/;
const SAFE_WORKSPACE = /^(?:\.|[A-Za-z0-9_@+.-]+(?:\/[A-Za-z0-9_@+.-]+)*)$/;

export class SeamError extends Error {
  constructor(code, message) { super(message); this.code = code; }
}

/** The snapshot-relative workspace path the verdict and the request bind to: `"."` for the root, forward slashes, no `..`. */
export function normalizeWorkspace(cwd) {
  const n = path.posix.normalize(String(cwd ?? ".").split(path.sep).join("/")).replace(/\/+$/, "");
  const out = n === "" || n === "." ? "." : n.replace(/^\.\//, "");
  if (!SAFE_WORKSPACE.test(out) || out.split("/").includes("..")) throw new SeamError("WORKSPACE", `${JSON.stringify(cwd)} is not a snapshot-relative workspace path`);
  return out;
}

/**
 * The Class A verdict for one workspace at one commit, or a SeamError. `scans` is every planned Class A row of the run with its status;
 * the verdict exists only when all of them passed (a cache hit is a pass), so an incomplete or red plan can never publish one.
 */
export function classAVerdict({ commit, tree, repoKey, workspace, scans }) {
  if (!SHA1.test(String(commit ?? ""))) throw new SeamError("VERDICT", `commit ${JSON.stringify(commit)} is not a full sha`);
  if (!SHA1.test(String(tree ?? ""))) throw new SeamError("VERDICT", `tree ${JSON.stringify(tree)} is not a full tree id`);
  if (typeof repoKey !== "string" || !/^[A-Za-z0-9_-]{8,}$/.test(repoKey)) throw new SeamError("VERDICT", `repoKey ${JSON.stringify(repoKey)} is not a repository key`);
  const ws = normalizeWorkspace(workspace);
  if (!Array.isArray(scans) || !scans.length) throw new SeamError("VERDICT", "a Class A verdict needs at least one scan; an empty plan proves nothing");
  const ids = new Set();
  const rows = scans.map((s) => {
    if (!s || typeof s.id !== "string" || !s.id) throw new SeamError("VERDICT", "a Class A scan without an id cannot be recorded");
    if (ids.has(s.id)) throw new SeamError("VERDICT", `Class A scan id ${s.id} appears twice`);
    ids.add(s.id);
    if (s.status !== "pass") throw new SeamError("CLASS_A_RED", `Class A is not complete and green: ${s.id} is ${s.status ?? "unknown"}; no verdict is written`);
    return { id: s.id, status: "pass" };
  });
  return { schema: CLASS_A_VERDICT_SCHEMA, commit, tree, repoKey, workspace: ws, status: "pass", scans: rows };
}

/** Writes a verdict atomically (temp file, rename) with mode 0600 and returns its absolute path. Refuses a relative directory. */
export function writeClassAVerdict(dir, verdict, { fsApi = fs } = {}) {
  if (!path.isAbsolute(dir)) throw new SeamError("VERDICT", `verdict directory ${dir} must be absolute`);
  if (verdict?.schema !== CLASS_A_VERDICT_SCHEMA || verdict.status !== "pass") throw new SeamError("VERDICT", "only a passing verdict of the current schema is written");
  fsApi.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const file = path.join(dir, `class-a-${verdict.commit.slice(0, 12)}-${verdict.workspace === "." ? "root" : crypto.createHash("sha256").update(verdict.workspace).digest("hex").slice(0, 12)}.json`);
  const tmp = `${file}.${process.pid}.${crypto.randomBytes(4).toString("hex")}.tmp`;
  fsApi.writeFileSync(tmp, `${JSON.stringify(verdict)}\n`, { mode: 0o600 });
  fsApi.renameSync(tmp, file);
  return file;
}

/** The two variables that carry the seam; nothing else is added to the child's environment by the seam. */
export function seamEnv({ commit, classA }) {
  if (!SHA1.test(String(commit ?? ""))) throw new SeamError("SEAM", `commit ${JSON.stringify(commit)} is not a full sha`);
  if (typeof classA !== "string" || !path.isAbsolute(classA)) throw new SeamError("SEAM", `the Class A verdict path must be absolute (${JSON.stringify(classA)})`);
  return { [GATE_COMMIT_ENV]: commit, [CLASS_A_ENV]: classA };
}

const nullableString = (v) => v === null || typeof v === "string";

/** Whether two absolute paths name the same file: Windows' extended `\\?\` prefix dropped, separators and case normalized there, resolved everywhere. */
export function samePath(a, b, platform = process.platform) {
  if (typeof a !== "string" || typeof b !== "string") return false;
  const norm = (p) => {
    let s = p.replace(/^\\\\\?\\UNC\\/i, "\\\\").replace(/^\\\\\?\\/, "");
    if (platform === "win32") { s = path.win32.resolve(s).replace(/\//g, "\\").toLowerCase(); return s; }
    return path.posix.normalize(s);
  };
  return norm(a) === norm(b);
}

/**
 * Exactly one `rustright-result <json>` line in the child's stderr: the result. None (the child was killed before it replied, or an
 * old client ran) or several is a failure of the row, never a success and never a retry.
 */
export function parseResultLine(stderr) {
  const lines = String(stderr ?? "").split(/\r?\n/).filter((l) => l.startsWith(RESULT_LINE_PREFIX));
  if (lines.length === 0) throw new SeamError("NO_RESULT_LINE", "the client printed no rustright-result line (killed before it replied, or not the seam client); the row fails");
  if (lines.length > 1) throw new SeamError("AMBIGUOUS_RESULT_LINE", `the client printed ${lines.length} rustright-result lines; one outcome per request`);
  let r;
  try { r = JSON.parse(lines[0].slice(RESULT_LINE_PREFIX.length)); } catch (e) { throw new SeamError("BAD_RESULT_LINE", `the rustright-result line is not JSON: ${e.message}`); }
  if (!r || typeof r !== "object" || Array.isArray(r)) throw new SeamError("BAD_RESULT_LINE", "the rustright-result line is not an object");
  const bad = [];
  if (!nullableString(r.requestId)) bad.push("requestId");
  if (!(r.exit === null || Number.isInteger(r.exit))) bad.push("exit");
  if (!nullableString(r.signal)) bad.push("signal");
  if (!nullableString(r.outputDir) || (typeof r.outputDir === "string" && !path.isAbsolute(r.outputDir))) bad.push("outputDir");
  if (!nullableString(r.receipt) || (typeof r.receipt === "string" && !path.isAbsolute(r.receipt))) bad.push("receipt");
  if (typeof r.receiptPersisted !== "boolean") bad.push("receiptPersisted");
  if (r.code !== undefined && (typeof r.code !== "string" || !/^RK_[A-Z0-9_]+$/.test(r.code))) bad.push("code");
  if (r.message !== undefined && typeof r.message !== "string") bad.push("message");
  if (bad.length) throw new SeamError("BAD_RESULT_LINE", `the rustright-result line has invalid field(s): ${bad.join(", ")}`);
  return r;
}

/**
 * The receipt the result line names, read directly, with its postconditions judged against this request: `gate.commit`, `gate.tree`
 * and `gate.classA` must be this run's, `gate.snapshotUnchanged` must be true. Returns { receipt, sha256, problems }.
 */
export function readSeamReceipt(file, { commit, tree, classA = null, fsApi = fs } = {}) {
  let text;
  try { text = fsApi.readFileSync(file, "utf8"); } catch (e) { return { receipt: null, sha256: null, problems: [`receipt ${file} cannot be read (${e.code ?? e.message})`] }; }
  let receipt;
  try { receipt = JSON.parse(text); } catch (e) { return { receipt: null, sha256: null, problems: [`receipt ${file} is not JSON (${e.message})`] }; }
  const problems = [];
  const g = receipt?.gate;
  if (!g || typeof g !== "object") problems.push("receipt has no gate object");
  else {
    if (g.commit !== commit) problems.push(`receipt gate.commit ${JSON.stringify(g.commit)} is not this run's ${commit}`);
    if (g.tree !== tree) problems.push(`receipt gate.tree ${JSON.stringify(g.tree)} is not this run's ${tree}`);
    if (classA !== null && !samePath(g.classA, classA)) problems.push(`receipt gate.classA ${JSON.stringify(g.classA)} is not the verdict this request named`);
    if (g.snapshotUnchanged !== true) problems.push("the broker found the snapshot changed during the job (gate.snapshotUnchanged is not true)");
  }
  if (receipt && Object.hasOwn(receipt, "testExecutables") && receipt.testExecutables !== null && !Array.isArray(receipt.testExecutables)) problems.push("receipt testExecutables is neither a list nor null");
  return { receipt, sha256: crypto.createHash("sha256").update(text).digest("hex"), problems };
}

/**
 * One verdict for the row from the child's exit, its result line and its receipt. Order: a refusal or client failure (code present, or
 * exit 125 with nothing admitted) fails with that code; a success without a durable receipt fails; the receipt's postconditions are read
 * next and fail the row even when the command exited 0 (a postcondition failure also makes the client exit 125 while `result.exit` keeps
 * the executed command's exit, so this comes before the exit comparison); then a client exit that disagrees with its line; then an
 * executed command that failed keeps its exit as code EXIT; otherwise pass, with the evidence downstream reads (outputDir,
 * testExecutables, receipt digest).
 */
export function judgeSeamOutcome({ child, stderr, commit, tree, classA = null, fsApi = fs }) {
  let result;
  try { result = parseResultLine(stderr); } catch (e) { return { status: "fail", code: e.code, detail: e.message, result: null, evidence: null }; }
  if (result.code) return { status: "fail", code: result.code, detail: `the broker refused or the client failed before anything ran: ${result.message ?? result.code}`, result, evidence: null };
  if (result.exit === NOT_EXECUTED_EXIT && result.requestId === null) return { status: "fail", code: "RK_NOT_EXECUTED", detail: "the client reported exit 125 with no request admitted and no code", result, evidence: null };
  if (!result.receiptPersisted || typeof result.receipt !== "string") return { status: "fail", code: "RECEIPT_MISSING", detail: "the broker left no durable receipt for this request; without it the row proves nothing", result, evidence: null };
  const read = readSeamReceipt(result.receipt, { commit, tree, classA, fsApi });
  if (read.problems.length) return { status: "fail", code: read.receipt?.gate && read.receipt.gate.snapshotUnchanged === false ? "RK_SNAPSHOT_CHANGED" : "RECEIPT_MISMATCH", detail: read.problems.join("; "), result, evidence: null };
  const evidence = { receiptSha256: read.sha256, receiptPath: result.receipt, requestId: result.requestId, outputDir: result.outputDir, testExecutables: Array.isArray(read.receipt.testExecutables) ? read.receipt.testExecutables : null, gate: read.receipt.gate };
  if (child && Number.isInteger(child.status) && result.exit !== null && child.status !== result.exit) return { status: "fail", code: "RESULT_EXIT_MISMATCH", detail: `the client exited ${child.status} but its result line says ${result.exit}`, result, evidence };
  if (result.exit !== 0 || result.signal) return { status: "fail", code: "EXIT", detail: result.signal ? `the command was stopped by signal ${result.signal}` : `exit ${result.exit}`, result, evidence };
  return { status: "pass", code: null, detail: null, result, evidence };
}

/**
 * The process-birth identity the broker compares a lock holder with (`rustright-process` identity.rs node_identity_token), spelled the
 * same: `darwin:<pid>:<ps lstart>` (LC_ALL=C LANG=C, trimmed), `linux:<pid>:<starttime>` (field 22 of /proc/<pid>/stat),
 * `win32:<pid>:<creation time, Unix ms>`. Null when it cannot be read; a pid alone is never an identity.
 */
export function processIdentity(pid = process.pid, { platform = process.platform, spawnSync = null, readFile = (f) => fs.readFileSync(f, "utf8") } = {}) {
  if (!Number.isInteger(pid) || pid <= 0) return null;
  if (platform === "darwin") {
    if (typeof spawnSync !== "function") return null;
    const r = spawnSync("/bin/ps", ["-o", "lstart=", "-p", String(pid)], { env: { LC_ALL: "C", LANG: "C", PATH: "/bin:/usr/bin" }, encoding: "utf8" });
    const lstart = String(r?.stdout ?? "").trim();
    return r && r.status === 0 && lstart ? `darwin:${pid}:${lstart}` : null;
  }
  if (platform === "linux") {
    let text; try { text = readFile(`/proc/${pid}/stat`); } catch { return null; }
    const close = text.lastIndexOf(")"); if (close < 0) return null;
    const start = text.slice(close + 1).trim().split(/\s+/)[19];
    return start && /^\d+$/.test(start) ? `linux:${pid}:${start}` : null;
  }
  if (platform === "win32") {
    if (typeof spawnSync !== "function") return null;
    // CIM CreationDate in UTC, as milliseconds since the Unix epoch; the broker's GetProcessTimes value matches it.
    const r = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `$p = Get-CimInstance Win32_Process -Filter "ProcessId=${pid}"; if ($p) { [DateTimeOffset]::new($p.CreationDate.ToUniversalTime()).ToUnixTimeMilliseconds() }`], { encoding: "utf8" });
    const ms = String(r?.stdout ?? "").trim();
    return r && r.status === 0 && /^\d+$/.test(ms) ? `win32:${pid}:${ms}` : null;
  }
  return null;
}

/** Parses an identity string into { platform, pid, start } or null. */
export function parseIdentity(text) {
  const m = /^(darwin|linux|win32):(\d+):(.+)$/.exec(String(text ?? ""));
  if (!m) return null;
  const pid = Number(m[2]);
  return Number.isInteger(pid) && pid > 0 ? { platform: m[1], pid, start: m[3] } : null;
}

// ---- the broker's proof that a dead holder's snapshot is idle (reset plan section 3; astra 2026-11-09) ------------------------------------
// SNAPSHOT_STATUS { protocol, runtime, snapshot, owner: { pid, identity, commit, token } } -> { type: "SNAPSHOT_STATUS", schema: "rustright.snapshot-status/1",
// snapshot, owner, state: "IDLE" | "BUSY" | "UNKNOWN", reasons: [string] }. Read-only, no lease. Only an exact echo of snapshot and owner with state IDLE
// allows a reclaim; BUSY, UNKNOWN, a different echo, an unreachable broker or any error is "no proof".
export const SNAPSHOT_STATUS_SCHEMA = "rustright.snapshot-status/1";

/** The query for one dead holder's lock (`owner` is the lock body as read). */
export function snapshotStatusMessage({ protocol, runtime, snapshot, owner }) {
  if (typeof protocol !== "string" || !protocol || typeof snapshot !== "string" || !path.isAbsolute(snapshot)) throw new SeamError("STATUS", "a snapshot status query needs the protocol and the absolute snapshot path");
  if (!owner || !Number.isInteger(owner.pid) || typeof owner.identity !== "string" || typeof owner.commit !== "string" || typeof owner.token !== "string") throw new SeamError("STATUS", "a snapshot status query needs the lock's { pid, identity, commit, token }");
  return { type: "SNAPSHOT_STATUS", protocol, runtime, snapshot, owner: { pid: owner.pid, identity: owner.identity, commit: owner.commit, token: owner.token } };
}

/** Whether a reply proves the snapshot idle for exactly this owner. Anything short of that is false. */
export function snapshotStatusAllowsReclaim(reply, { snapshot, owner }) {
  if (!reply || typeof reply !== "object" || reply.type !== "SNAPSHOT_STATUS" || reply.schema !== SNAPSHOT_STATUS_SCHEMA) return false;
  if (reply.snapshot !== snapshot || !reply.owner || typeof reply.owner !== "object") return false;
  for (const k of ["pid", "identity", "commit", "token"]) if (reply.owner[k] !== owner[k]) return false;
  return reply.state === "IDLE";
}

// The broker greets every connection with a HANDSHAKE frame before it answers anything (rustright service.rs emit HANDSHAKE on connect), and may interleave
// other frames; the reader consumes framed lines until the one whose `type` is the answer (SNAPSHOT_STATUS) or a refusal/error frame, within the deadline.
const STATUS_SCRIPT = `const net=require("node:net");const [endpoint,msg,timeout,want]=process.argv.slice(1);const c=net.connect({path:endpoint});let buf="";const t=setTimeout(()=>process.exit(3),Number(timeout));const done=(line)=>{process.stdout.write(line);clearTimeout(t);c.destroy();process.exit(0);};c.on("connect",()=>c.write(msg+"\\n"));c.on("data",(d)=>{buf+=d;let i;while((i=buf.indexOf("\\n"))>=0){const line=buf.slice(0,i);buf=buf.slice(i+1);let f=null;try{f=JSON.parse(line);}catch{continue;}if(!f||typeof f!=="object")continue;if(f.type===want||f.type==="REFUSED"||f.type==="ERROR"||f.type==="REFUSAL")done(line);}});c.on("error",()=>process.exit(2));c.on("close",()=>process.exit(4));`;

/**
 * One control query, synchronously (the lock is taken inside the synchronous sync): a child node connects to the broker's socket or pipe, writes the
 * message as one line and returns the frame that answers it (`want`, default SNAPSHOT_STATUS), skipping the HANDSHAKE and any other frame before it; a
 * refusal or error frame is returned as the answer (the caller's judge then says no). Null on any failure (no broker, timeout, bad JSON): no proof.
 */
export function snapshotStatusSync({ endpoint, message, timeoutMs = 10_000, want = "SNAPSHOT_STATUS", execFileSync = nodeExecFileSync, execPath = process.execPath }) {
  if (typeof endpoint !== "string" || !endpoint) return null;
  let out;
  try { out = execFileSync(execPath, ["-e", STATUS_SCRIPT, endpoint, JSON.stringify(message), String(timeoutMs), String(want)], { encoding: "utf8", timeout: timeoutMs + 2_000, stdio: ["ignore", "pipe", "ignore"], windowsHide: true }); }
  catch { return null; }
  try { const reply = JSON.parse(String(out).trim()); return reply && typeof reply === "object" ? reply : null; } catch { return null; }
}

/** The lock body the broker reads (`${snapshot}.lock`): exactly { pid, identity, commit, token }. */
export function snapshotLockBody({ pid = process.pid, identity, commit }) {
  if (!SHA1.test(String(commit ?? ""))) throw new SeamError("LOCK", `commit ${JSON.stringify(commit)} is not a full sha`);
  const parsed = parseIdentity(identity);
  if (!parsed || parsed.pid !== pid) throw new SeamError("LOCK", `identity ${JSON.stringify(identity)} is not this process's identity`);
  return { pid, identity, commit, token: crypto.randomUUID() };
}
