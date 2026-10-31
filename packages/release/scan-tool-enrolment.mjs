// The gate's own record of the scan tools it trusts on this host (RA-18 / L9-03; the broker-pins.mjs pattern, for analyzers instead of a broker).
//
// deterministic-scans.mjs runs shellcheck, actionlint, cargo-deny and cargo-audit only from an ENROLLED executable: an absolute path, the file's
// sha256 and its version (`options.toolPolicy`, `{ [tool]: { path, sha256, version } }`), never a binary found on PATH. This module is where that
// policy comes from, and from nowhere else:
//
//   record   `<managed data root>/scan-tools/<platform>.json`, `{schema, platform, tools: {<tool>: {path, sha256, version}}, enrolledAt}`,
//            outside every repository and snapshot, written only by `enrollScanTool` (an explicit operation, one tool per call; a second
//            enrollment of the same tool is a rotation and says so). At enrol time the file named by `path` must be a regular, executable
//            file whose sha256 IS the one enrolled: the operator's claim is checked against the bytes, nothing is computed on their behalf.
//   fallback RightKit's own checkout only: `ci/rk-pipeline.json` `tools.<name>.<platform>`, read from the revision of the RUNNING gate
//            (`git show <gate repository HEAD>:ci/rk-pipeline.json`, readGateRevisionPipeline), never the checked repository, the selected
//            commit, the snapshot or the working tree. The enrolled record decides a tool it names; the fallback only fills the tools it does not.
//
// Never read here: an environment variable (apart from the managed data root's own location, which the caller resolves), a flag of the run, the
// checked repository's files, a receipt. A tool with no entry anywhere is not enrolled: deterministic-scans.mjs answers TOOL_UNTRUSTED.
// Platforms are those whose host may run the gate's class A: darwin, win32 and linux.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { readGateRevisionPipeline } from "./broker-pins.mjs";
import { spawnSync } from "./background-process.mjs";

export const SCAN_TOOLS_SCHEMA = "rightkit.scan-tools/1";
export const SCAN_TOOLS_DIR = "scan-tools";
/** The analyzers class A accepts only from an enrolment (deterministic-scans.mjs TRUSTED_SCAN_TOOLS; kept in step by the tests). */
export const SCAN_TOOL_NAMES = Object.freeze(["actionlint", "shellcheck", "cargo-deny", "cargo-audit"]);
export const SCAN_TOOL_PLATFORMS = Object.freeze(["darwin", "win32", "linux"]);

export class ScanToolEnrolmentError extends Error {
  constructor(code, message) { super(message); this.name = "ScanToolEnrolmentError"; this.code = code; }
}
const fail = (code, message) => { throw new ScanToolEnrolmentError(code, message); };
const SHA256 = /^[0-9a-f]{64}$/;
const isLink = (meta) => meta.isSymbolicLink();

/** The record of one platform under the managed data root. */
export const scanToolsFile = ({ dataRoot, platform = process.platform }) => {
  if (!dataRoot || !path.isAbsolute(dataRoot)) fail("RK_SCAN_TOOLS_ROOT", "the managed data root must be an absolute path");
  if (!SCAN_TOOL_PLATFORMS.includes(platform)) fail("RK_SCAN_TOOLS_PLATFORM", `${platform} is not a scan-tool host (${SCAN_TOOL_PLATFORMS.join(", ")} only)`);
  return path.join(dataRoot, SCAN_TOOLS_DIR, `${platform}.json`);
};

/** `{path, sha256, version}` or a throw naming what is wrong: a trusted tool, an absolute path without `..`, a lowercase sha256, a version. */
export function validateScanTool(tool, entry, { platform = process.platform, label = "the scan-tool enrolment" } = {}) {
  const bad = (why) => fail("RK_SCAN_TOOLS_INVALID", `${label}: ${tool}: ${why}`);
  if (!SCAN_TOOL_NAMES.includes(tool)) fail("RK_SCAN_TOOLS_INVALID", `${label}: ${JSON.stringify(tool)} is not a trusted scan tool (${SCAN_TOOL_NAMES.join(", ")})`);
  if (entry === null || typeof entry !== "object" || Array.isArray(entry)) bad("not an object");
  const { path: file, sha256, version } = entry;
  const paths = platform === "win32" ? path.win32 : path.posix;
  if (typeof file !== "string" || !paths.isAbsolute(file) || file.split(/[\\/]/).includes("..") || file.includes("\0")) bad("path is not an absolute path without '..'");
  if (typeof sha256 !== "string" || !SHA256.test(sha256)) bad("sha256 is not a sha256 in lowercase hex");
  if (typeof version !== "string" || !version.trim() || version !== version.trim()) bad("version is missing");
  return { path: file, sha256, version };
}

const digestOf = (file) => {
  const hash = crypto.createHash("sha256"); const fd = fs.openSync(file, "r");
  try { const buffer = Buffer.alloc(1 << 20); for (let n = fs.readSync(fd, buffer, 0, buffer.length, null); n > 0; n = fs.readSync(fd, buffer, 0, buffer.length, null)) hash.update(buffer.subarray(0, n)); } finally { fs.closeSync(fd); }
  return hash.digest("hex");
};

/** The enrolled record `{ platform, tools, source: "enrolled", file }`, or null when none exists. A link, a non-file, another platform's record, an unknown tool or a malformed entry is a refusal, never null. */
export function readEnrolledScanTools({ dataRoot, platform = process.platform }) {
  const file = scanToolsFile({ dataRoot, platform });
  let meta;
  try { meta = fs.lstatSync(file); } catch (e) { if (e.code === "ENOENT") return null; throw e; }
  if (isLink(meta) || !meta.isFile()) fail("RK_SCAN_TOOLS_INVALID", `${file} is not a regular file`);
  const dir = fs.lstatSync(path.dirname(file));
  if (isLink(dir) || !dir.isDirectory()) fail("RK_SCAN_TOOLS_INVALID", `${path.dirname(file)} is not a real directory`);
  if (process.platform !== "win32" && (meta.mode & 0o022 || dir.mode & 0o022)) fail("RK_SCAN_TOOLS_INVALID", `${file} or its directory is writable by group or others`);
  let record;
  try { record = JSON.parse(fs.readFileSync(file, "utf8")); } catch (e) { fail("RK_SCAN_TOOLS_INVALID", `${file} is not JSON (${e.message})`); }
  if (record?.schema !== SCAN_TOOLS_SCHEMA) fail("RK_SCAN_TOOLS_INVALID", `${file} is not a ${SCAN_TOOLS_SCHEMA} record`);
  if (record.platform !== platform) fail("RK_SCAN_TOOLS_INVALID", `${file} is the record of ${record.platform}, not ${platform}`);
  if (record.tools === null || typeof record.tools !== "object" || Array.isArray(record.tools)) fail("RK_SCAN_TOOLS_INVALID", `${file} has no tools object`);
  const tools = {};
  for (const [name, entry] of Object.entries(record.tools)) tools[name] = validateScanTool(name, entry, { platform, label: file });
  return { platform, tools, source: "enrolled", file };
}

/**
 * RightKit's own scan-tool pipeline entries, from the revision of the running gate: the `tools` object of `ci/rk-pipeline.json` there (raw: the
 * caller turns it into a policy with deterministic-scans.mjs scanToolPolicyFromPipeline). Null when the gate is not run from a RightKit checkout
 * or the file has no `tools`.
 */
export function readGateRevisionScanTools({ gateFile, checkout, spawn = spawnSync, realpath }) {
  const read = readGateRevisionPipeline({ gateFile, checkout, spawn, ...(realpath ? { realpath } : {}) });
  if (!read || read.pipeline.tools === null || typeof read.pipeline.tools !== "object") return null;
  return { tools: read.pipeline.tools, revision: read.revision };
}

/**
 * The scan-tool policy a gate run hands runScans as `options.toolPolicy`: `{ policy, sources, enrolledFile, revision }` where `policy` is
 * `{ [tool]: { path, sha256, version } }`, the enrolled tools over the running gate's own revision's, and `sources[tool]` says which decided
 * (`"enrolled"` | `"rk-pipeline"`). `fromPipeline` is deterministic-scans.mjs scanToolPolicyFromPipeline (passed in so this module adds no
 * import of the scan engine). An empty policy is a valid answer: each tool no one enrolled is then TOOL_UNTRUSTED where a scan needs it.
 */
export function loadScanToolPolicy({ dataRoot, platform = process.platform, gateFile, checkout, spawn, realpath, fromPipeline }) {
  if (typeof fromPipeline !== "function") fail("RK_SCAN_TOOLS_INVALID", "the pipeline policy reader is required (scanToolPolicyFromPipeline)");
  // A platform that is not a scan-tool host has no enrolment to read: nothing is trusted there (every analyzer answers TOOL_UNTRUSTED).
  if (!SCAN_TOOL_PLATFORMS.includes(platform)) return { policy: {}, sources: {} };
  const enrolled = readEnrolledScanTools({ dataRoot, platform });
  const fallback = gateFile && checkout ? readGateRevisionScanTools({ gateFile, checkout, spawn, realpath }) : null;
  const policy = {}; const sources = {};
  if (fallback) {
    for (const [name, entry] of Object.entries(fromPipeline(fallback.tools, platform))) { policy[name] = entry; sources[name] = "rk-pipeline"; }
  }
  if (enrolled) for (const [name, entry] of Object.entries(enrolled.tools)) { policy[name] = entry; sources[name] = "enrolled"; }
  return { policy, sources, ...(enrolled ? { enrolledFile: enrolled.file } : {}), ...(fallback ? { revision: fallback.revision } : {}) };
}

/**
 * The explicit enrol operation, one tool per call. The file at `path` must exist, be a regular executable file and hash to `sha256` now
 * (the operator's value is verified, never filled in); the record under the data root gains the tool. An already enrolled tool is replaced
 * only with `rotate: true`. Returns `{ file, record, rotated }`.
 */
export function enrollScanTool({ dataRoot, platform = process.platform, tool, path: toolPath, sha256, version, rotate = false, now = () => new Date() }) {
  const file = scanToolsFile({ dataRoot, platform });
  const entry = validateScanTool(tool, { path: toolPath, sha256, version }, { platform, label: "the enrolment" });
  // The claim is judged against the bytes it names, on the host that enrols (a record for another platform cannot be checked from here).
  if (platform !== process.platform) fail("RK_SCAN_TOOLS_PLATFORM", `${platform} tools are enrolled on a ${platform} host (this is ${process.platform}): the file's bytes cannot be checked from here`);
  let real; let meta;
  try { real = fs.realpathSync.native(entry.path); meta = fs.statSync(real); } catch (e) { fail("RK_SCAN_TOOLS_INVALID", `the ${tool} at ${entry.path} cannot be resolved (${e.code ?? e.message})`); }
  if (!meta.isFile()) fail("RK_SCAN_TOOLS_INVALID", `the ${tool} at ${real} is not a regular file`);
  if (platform !== "win32" && (meta.mode & 0o111) === 0) fail("RK_SCAN_TOOLS_INVALID", `the ${tool} at ${real} is not executable`);
  const actual = digestOf(real);
  if (actual !== entry.sha256) fail("RK_SCAN_TOOLS_DIGEST", `the ${tool} at ${real} has sha256 ${actual}, not the ${entry.sha256} being enrolled; nothing was written`);
  const dir = path.dirname(file);
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const dirMeta = fs.lstatSync(dir);
  if (isLink(dirMeta) || !dirMeta.isDirectory()) fail("RK_SCAN_TOOLS_INVALID", `${dir} is not a real directory`);
  let existing = null;
  try { existing = readEnrolledScanTools({ dataRoot, platform }); } catch (e) { if (!rotate) throw e; }
  const had = existing?.tools[tool] ?? null;
  if (had && !rotate) fail("RK_SCAN_TOOLS_EXISTS", `${file} already enrolls ${tool} ${had.version} (sha256 ${had.sha256.slice(0, 12)}); replacing it is a rotation (pass rotate: true)`);
  const record = { schema: SCAN_TOOLS_SCHEMA, platform, tools: { ...(existing?.tools ?? {}), [tool]: entry }, enrolledAt: now().toISOString() };
  const tmp = `${file}.${crypto.randomUUID()}.tmp`;
  fs.writeFileSync(tmp, `${JSON.stringify(record, null, 2)}\n`, { flag: "wx", mode: 0o600 });
  try { fs.renameSync(tmp, file); } finally { fs.rmSync(tmp, { force: true }); }
  return { file, record, rotated: Boolean(had) };
}

/** `node scan-tool-enrolment.mjs enroll --platform <darwin|win32|linux> --data-root <dir> --tool <name> --path P --sha256 D --version V [--rotate]` or `show --platform <p> --data-root <dir>`. */
export function main(argv, { out = (m) => process.stdout.write(`${m}\n`), err = (m) => process.stderr.write(`${m}\n`) } = {}) {
  const [command, ...rest] = argv;
  const opt = (name) => { const i = rest.indexOf(name); return i < 0 ? undefined : rest[i + 1]; };
  try {
    const dataRoot = opt("--data-root"); const platform = opt("--platform") ?? process.platform;
    if (command === "enroll") {
      const r = enrollScanTool({ dataRoot, platform, tool: opt("--tool"), path: opt("--path"), sha256: opt("--sha256"), version: opt("--version"), rotate: rest.includes("--rotate") });
      const tool = opt("--tool");
      out(`enrolled ${tool} ${r.record.tools[tool].version} for ${r.record.platform} (sha256 ${r.record.tools[tool].sha256}) in ${r.file}${r.rotated ? " (rotation)" : ""}`);
      return 0;
    }
    if (command === "show") {
      const enrolled = readEnrolledScanTools({ dataRoot, platform });
      out(enrolled ? JSON.stringify(enrolled) : "no enrollment");
      return enrolled ? 0 : 1;
    }
    err(`usage: scan-tool-enrolment.mjs enroll|show --platform <darwin|win32|linux> --data-root <dir> [--tool <${SCAN_TOOL_NAMES.join("|")}> --path P --sha256 D --version V [--rotate]]`);
    return 2;
  } catch (e) { err(`${e.code ?? "error"}: ${e.message}`); return 1; }
}

if (process.argv[1] && (() => { try { return fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; } })()) process.exit(main(process.argv.slice(2)));
