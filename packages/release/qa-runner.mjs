// The rightkit-qa runner an app's `qa.version` names, obtained the one sanctioned way (rightkit#1309 pins are routine; never a PATH binary).
//
// `right-release qa` used to take the runner from `qa.runner` (a file inside the source) or the first `rightkit-qa` on PATH. A source snapshot holds no
// runner and PATH is not a trust root, so a package stage's source QA had nothing admissible to run. The runner is now a managed tool, the pattern of
// scan-tool-enrolment.mjs:
//
//   record   `<managed data root>/qa-runners/<mac|win>/record.json`, `{schema, platform, runners: {<version>: {path, sha256, version}}, enrolledAt}`, and the
//            binary itself at `<managed data root>/qa-runners/<platform>/<version>/rightkit-qa[.exe]`: a COPY the enrol operation made, so a later
//            `cargo clean` or target rotation cannot change what a receipt digests. Outside every repository and snapshot.
//   enrol    `enrollQaRunner` (also `right-release qa runner enroll`): the operator names a built file (RightKit's own gate-built `crates/rightkit-qa`
//            output) and the version; the file must be a regular executable that answers `--version` with exactly `rightkit-qa <version>`; its copy is
//            digested and recorded. A second enrolment of the same version is a rotation and needs `rotate`.
//   use      `resolveManagedQaRunner`: the record names the version the app declared; the managed file must still be a real file inside the managed
//            directory whose sha256 IS the recorded one and which still answers `--version` as that version. Anything else refuses, naming it.
//   admit    unchanged and separate: the sealed receipt carries the runner's sha256, and qa-admission-policy.json `approvedRunners` (version + platform +
//            sha256, release-qa-policy.mjs approve-runner) decides whether the receipt is admitted. This module trusts nothing on its own.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { resolveManagedRoots } from "./managed-roots.mjs";

export const QA_RUNNERS_SCHEMA = "rightkit.qa-runners/1";
export const QA_RUNNERS_DIR = "qa-runners";
export const QA_RUNNER_PLATFORMS = Object.freeze(["mac", "win"]);
const HOST_PLATFORM = { darwin: "mac", win32: "win" };
const SEMVER = /^\d+\.\d+\.\d+$/;
const SHA256 = /^[0-9a-f]{64}$/;

export class QaRunnerError extends Error {
  constructor(code, message) { super(message); this.name = "QaRunnerError"; this.code = code; }
}
const fail = (code, message) => { throw new QaRunnerError(code, message); };

/** True when `qa.version` names a rightkit-qa release (`0.10.0`), as opposed to the legacy QA plan version (`"1"`). */
export const isRunnerVersion = (version) => SEMVER.test(String(version ?? ""));

export const runnerExecutableName = (platform) => (platform === "win" ? "rightkit-qa.exe" : "rightkit-qa");

const checkPlatform = (platform) => { if (!QA_RUNNER_PLATFORMS.includes(platform)) fail("RK_QA_RUNNER_PLATFORM", `platform must be ${QA_RUNNER_PLATFORMS.join(" or ")}, not ${JSON.stringify(platform)}`); };
const checkDataRoot = (dataRoot) => { if (!dataRoot || !path.isAbsolute(dataRoot)) fail("RK_QA_RUNNER_ROOT", "the managed data root must be an absolute path"); };

export const qaRunnersDir = ({ dataRoot, platform }) => { checkDataRoot(dataRoot); checkPlatform(platform); return path.join(dataRoot, QA_RUNNERS_DIR, platform); };
export const qaRunnersRecordFile = (args) => path.join(qaRunnersDir(args), "record.json");
export const qaRunnerFile = ({ dataRoot, platform, version }) => {
  if (!isRunnerVersion(version)) fail("RK_QA_RUNNER_VERSION", `${JSON.stringify(version)} is not a rightkit-qa version (major.minor.patch)`);
  return path.join(qaRunnersDir({ dataRoot, platform }), version, runnerExecutableName(platform));
};

const sha256OfFile = (file) => crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const isInside = (child, parent) => { const rel = path.relative(parent, child); return rel !== "" && !rel.startsWith("..") && !path.isAbsolute(rel); };

/** The runner's own answer to `--version` must be exactly `rightkit-qa <version>`; the file that answers is the file that is digested. */
function assertReportsVersion(file, version, spawn) {
  const run = spawn(file, ["--version"], { encoding: "utf8", windowsHide: true, timeout: 30000 });
  const output = String(run?.stdout ?? "").trim();
  if (run?.status !== 0 || output !== `rightkit-qa ${version}`) {
    fail("RK_QA_RUNNER_VERSION_MISMATCH", `${file} --version reported ${JSON.stringify(output.slice(0, 80))} (exit ${run?.status ?? run?.error?.message ?? "none"}), expected "rightkit-qa ${version}"`);
  }
}

function readRecord({ dataRoot, platform }) {
  const file = qaRunnersRecordFile({ dataRoot, platform });
  let meta;
  try { meta = fs.lstatSync(file); } catch (error) { if (error.code === "ENOENT") return null; throw error; }
  if (meta.isSymbolicLink() || !meta.isFile()) fail("RK_QA_RUNNER_RECORD", `${file} is not a regular file`);
  if (process.platform !== "win32" && (meta.mode & 0o022)) fail("RK_QA_RUNNER_RECORD", `${file} is writable by group or others`);
  let record;
  try { record = JSON.parse(fs.readFileSync(file, "utf8")); } catch (error) { fail("RK_QA_RUNNER_RECORD", `${file} is not JSON (${error.message})`); }
  if (record?.schema !== QA_RUNNERS_SCHEMA || record.platform !== platform || record.runners === null || typeof record.runners !== "object" || Array.isArray(record.runners)) {
    fail("RK_QA_RUNNER_RECORD", `${file} is not a ${QA_RUNNERS_SCHEMA} record for ${platform}`);
  }
  return { file, record };
}

/**
 * The explicit enrol operation. `runner` is a built rightkit-qa file (an absolute path); its copy under the managed directory is what is recorded.
 * Enrols on the host whose platform it is (the bytes are executed here to read their version). Returns `{ file, runnerFile, entry, rotated }`.
 */
export function enrollQaRunner({ dataRoot, platform, runner, version, rotate = false, spawn = spawnSync, hostPlatform = process.platform, now = () => new Date() }) {
  checkPlatform(platform);
  if (HOST_PLATFORM[hostPlatform] !== platform) fail("RK_QA_RUNNER_PLATFORM", `a ${platform} runner is enrolled on a ${platform} host (this is ${hostPlatform}): its bytes are executed to read their version`);
  const target = qaRunnerFile({ dataRoot, platform, version });
  if (!runner || !path.isAbsolute(String(runner))) fail("RK_QA_RUNNER_PATH", "the runner must be named by an absolute path");
  let source; let meta;
  try { source = fs.realpathSync.native(runner); meta = fs.statSync(source); } catch (error) { fail("RK_QA_RUNNER_PATH", `the runner ${runner} cannot be resolved (${error.code ?? error.message})`); }
  if (!meta.isFile()) fail("RK_QA_RUNNER_PATH", `${source} is not a regular file`);
  if (platform !== "win" && (meta.mode & 0o111) === 0) fail("RK_QA_RUNNER_PATH", `${source} is not executable`);
  assertReportsVersion(source, version, spawn);
  const existing = readRecord({ dataRoot, platform });
  const had = existing?.record.runners?.[version] ?? null;
  if (had && !rotate) fail("RK_QA_RUNNER_EXISTS", `${existing.file} already enrols rightkit-qa ${version} (sha256 ${String(had.sha256).slice(0, 12)}); replacing it is a rotation (pass rotate)`);
  const dir = path.dirname(target);
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const staging = `${target}.${crypto.randomUUID()}.tmp`;
  try {
    fs.copyFileSync(source, staging, fs.constants.COPYFILE_EXCL);
    fs.chmodSync(staging, 0o755);
    assertReportsVersion(staging, version, spawn);
    fs.renameSync(staging, target);
  } finally { fs.rmSync(staging, { force: true }); }
  const entry = { path: target, sha256: sha256OfFile(target), version };
  const record = { schema: QA_RUNNERS_SCHEMA, platform, runners: { ...(existing?.record.runners ?? {}), [version]: entry }, enrolledAt: now().toISOString() };
  const file = qaRunnersRecordFile({ dataRoot, platform });
  const tmp = `${file}.${crypto.randomUUID()}.tmp`;
  try { fs.writeFileSync(tmp, `${JSON.stringify(record, null, 2)}\n`, { flag: "wx", mode: 0o600 }); fs.renameSync(tmp, file); } finally { fs.rmSync(tmp, { force: true }); }
  return { file, runnerFile: target, entry, rotated: Boolean(had) };
}

/**
 * The managed runner for `version` on `platform`: `{ file, sha256, version, source: "managed" }`, or a typed refusal. The record is read, the file it names
 * must be the managed copy (inside the managed directory, not a link), its bytes must still hash to the recorded sha256, and it must still answer
 * `--version` with that version. There is no fallback: a missing enrolment is a refusal that names the operation that makes one.
 */
export function resolveManagedQaRunner({ dataRoot, platform, version, spawn = spawnSync }) {
  checkPlatform(platform);
  if (!isRunnerVersion(version)) fail("RK_QA_RUNNER_VERSION", `qa.version ${JSON.stringify(version)} is not a rightkit-qa version (major.minor.patch)`);
  const expected = qaRunnerFile({ dataRoot, platform, version });
  const read = readRecord({ dataRoot, platform });
  const entry = read?.record.runners?.[version];
  if (!entry) fail("RK_QA_RUNNER_MISSING", `rightkit-qa ${version} is not enrolled for ${platform} (${qaRunnersRecordFile({ dataRoot, platform })}); build crates/rightkit-qa ${version} through the gate and run \`right-release qa runner enroll --runner <built rightkit-qa> --version ${version} --platform ${platform}\``);
  if (entry.version !== version || !SHA256.test(String(entry.sha256)) || entry.path !== expected) fail("RK_QA_RUNNER_RECORD", `${read.file}: the entry for ${version} is malformed or names a file other than ${expected}`);
  let meta;
  try { meta = fs.lstatSync(expected); } catch (error) { fail("RK_QA_RUNNER_MISSING", `the enrolled rightkit-qa ${version} is gone (${expected}: ${error.code ?? error.message}); enrol it again`); }
  if (meta.isSymbolicLink() || !meta.isFile()) fail("RK_QA_RUNNER_RECORD", `${expected} is not a regular file`);
  if (!isInside(fs.realpathSync.native(expected), fs.realpathSync.native(path.join(dataRoot, QA_RUNNERS_DIR)))) fail("RK_QA_RUNNER_RECORD", `${expected} resolves outside ${path.join(dataRoot, QA_RUNNERS_DIR)}`);
  const actual = sha256OfFile(expected);
  if (actual !== entry.sha256) fail("RK_QA_RUNNER_DIGEST", `the enrolled rightkit-qa ${version} at ${expected} has sha256 ${actual}, not the recorded ${entry.sha256}; it changed after enrolment`);
  assertReportsVersion(expected, version, spawn);
  return { file: expected, sha256: actual, version, source: "managed" };
}

/** `node qa-runner.mjs enroll --data-root <dir> --platform mac|win --runner <file> --version <v> [--rotate]` or `show --data-root <dir> --platform mac|win`. */
export function main(argv, { out = (m) => process.stdout.write(`${m}\n`), err = (m) => process.stderr.write(`${m}\n`), env = process.env, spawn, program = "qa-runner.mjs" } = {}) {
  const [command, ...rest] = argv;
  const opt = (name) => { const i = rest.indexOf(name); return i < 0 ? undefined : rest[i + 1]; };
  try {
    const platform = opt("--platform") ?? HOST_PLATFORM[process.platform];
    const dataRoot = opt("--data-root") ?? resolveManagedRoots({ env }).dataRoot;
    if (command === "enroll") {
      const r = enrollQaRunner({ dataRoot, platform, runner: opt("--runner") && path.resolve(opt("--runner")), version: opt("--version"), rotate: rest.includes("--rotate"), ...(spawn ? { spawn } : {}) });
      out(`enrolled rightkit-qa ${r.entry.version} for ${platform} (sha256 ${r.entry.sha256}) at ${r.runnerFile}${r.rotated ? " (rotation)" : ""}; approve it for admission with \`right-release qa policy approve-runner --runner ${r.runnerFile} --version ${r.entry.version} --platform ${platform} --set-by <name>\``);
      return 0;
    }
    if (command === "show") {
      const read = readRecord({ dataRoot, platform });
      out(read ? JSON.stringify(read.record) : "no enrolment");
      return read ? 0 : 1;
    }
    err(`usage: ${program} enroll --platform mac|win --runner <built rightkit-qa> --version <v> [--data-root <dir>] [--rotate]\n       ${program} show [--platform mac|win] [--data-root <dir>]`);
    return 2;
  } catch (error) { err(`${error.code ?? "error"}: ${error.message}`); return 1; }
}

if (process.argv[1] && (() => { try { return fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; } })()) process.exit(main(process.argv.slice(2)));
