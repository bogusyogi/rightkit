// The OS credential provider (`rightkit-credentials`, crates/rightkit-credentials) that updater signing runs, obtained the one sanctioned way: the gate's
// tool route (`rightkit-gate --to tool --unit cargo:rightkit-credentials`, tool-build.mjs), never a PATH binary on a Mac or Windows host. It is the
// pattern of qa-runner.mjs:
//
//   record   `<managed data root>/credential-provider/<mac|win>/record.json`, `{schema, platform, providers: {<version>: {path, sha256, version}}, enrolledAt}`,
//            and the binary at `<managed data root>/credential-provider/<platform>/<version>/rightkit-credentials[.exe]`: a COPY the enrol operation made, so a
//            later `cargo clean` cannot change what signing executes. Outside every repository and snapshot.
//   enrol    `enrollCredentialProvider`: the gate-built file of the crate's committed version (>= MIN_PROVIDER_VERSION). A second enrolment of the same
//            version is a rotation and needs `rotate`.
//   use      `resolveCredentialProvider`: the NEWEST enrolled version; its file must still be a regular file inside the managed directory whose sha256 IS
//            the recorded one. Anything else is a typed refusal naming the sanctioned install.
//
// The binary has NO `--version` (its CLI is `status|set|delete|ensure-key|exec`; any other first word, with no `--namespace`, answers
// `{"ok":false,"error":"invalid --namespace: required"}` and exits 1). So the version is not attested by the binary: it is the version of the commit the gate built
// it from (the tool route passes the crate's committed version), bound to the copy by its sha256. What enrolment does check is that the file is this CLI
// (the identity probe below), which is a regular executable that touches no credential store.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { resolveManagedRoots } from "./managed-roots.mjs";

export const CREDENTIAL_PROVIDER_SCHEMA = "rightkit.credential-provider/1";
export const CREDENTIAL_PROVIDER_DIR = "credential-provider";
export const CREDENTIAL_PROVIDER_PLATFORMS = Object.freeze(["mac", "win"]);
export const MIN_PROVIDER_VERSION = "0.2.2";
/** The one sanctioned way to install the provider; every refusal names it. */
export const CREDENTIAL_PROVIDER_INSTALL = "rightkit-gate --to tool --unit cargo:rightkit-credentials";
const HOST_PLATFORM = { darwin: "mac", win32: "win" };
const SEMVER = /^(\d+)\.(\d+)\.(\d+)$/;
const SHA256 = /^[0-9a-f]{64}$/;

export class CredentialProviderError extends Error {
  constructor(code, message) { super(message); this.name = "CredentialProviderError"; this.code = code; }
}
const fail = (code, message) => { throw new CredentialProviderError(code, message); };

/** The provider platform of a `process.platform` value, or null when the host has no managed provider (Linux, CI). */
export const providerPlatformOf = (hostPlatform) => HOST_PLATFORM[hostPlatform] ?? null;
export const providerExecutableName = (platform) => (platform === "win" ? "rightkit-credentials.exe" : "rightkit-credentials");

const parse = (version) => { const m = SEMVER.exec(String(version ?? "")); return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : null; };
const compare = (a, b) => { for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] - b[i]; return 0; };
const checkPlatform = (platform) => { if (!CREDENTIAL_PROVIDER_PLATFORMS.includes(platform)) fail("RK_CRED_PROVIDER_PLATFORM", `platform must be ${CREDENTIAL_PROVIDER_PLATFORMS.join(" or ")}, not ${JSON.stringify(platform)}`); };
const checkDataRoot = (dataRoot) => { if (!dataRoot || !path.isAbsolute(dataRoot)) fail("RK_CRED_PROVIDER_ROOT", "the managed data root must be an absolute path"); };
const checkVersion = (version) => {
  const v = parse(version);
  if (!v) fail("RK_CRED_PROVIDER_VERSION", `${JSON.stringify(version)} is not a rightkit-credentials version (major.minor.patch)`);
  if (compare(v, parse(MIN_PROVIDER_VERSION)) < 0) fail("RK_CRED_PROVIDER_VERSION", `rightkit-credentials ${version} is below ${MIN_PROVIDER_VERSION}, the first release with the \`exec\` child-environment command updater signing needs`);
};

export const credentialProviderDir = ({ dataRoot, platform }) => { checkDataRoot(dataRoot); checkPlatform(platform); return path.join(dataRoot, CREDENTIAL_PROVIDER_DIR, platform); };
export const credentialProviderRecordFile = (args) => path.join(credentialProviderDir(args), "record.json");
export const credentialProviderFile = ({ dataRoot, platform, version }) => path.join(credentialProviderDir({ dataRoot, platform }), String(version), providerExecutableName(platform));

const sha256OfFile = (file) => crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const isInside = (child, parent) => { const rel = path.relative(parent, child); return rel !== "" && !rel.startsWith("..") && !path.isAbsolute(rel); };

/** The identity probe: with no arguments the CLI answers a JSON refusal naming `--namespace` and exits 1, and reads no store. A file that answers anything else is not rightkit-credentials. */
function assertIsCredentialCli(file, spawn) {
  const run = spawn(file, [], { encoding: "utf8", windowsHide: true, timeout: 30000, stdio: ["ignore", "pipe", "pipe"] });
  let answer = null;
  try { answer = JSON.parse(String(run?.stdout ?? "").trim()); } catch { /* not JSON */ }
  if (run?.status !== 1 || answer?.ok !== false || !String(answer?.error ?? "").includes("--namespace")) {
    fail("RK_CRED_PROVIDER_IDENTITY", `${file} did not answer like rightkit-credentials (exit ${run?.status ?? run?.error?.message ?? "none"}, ${JSON.stringify(String(run?.stdout ?? "").slice(0, 80))}); it has no --version, so its identity is its refusal of a call with no --namespace`);
  }
}

function readRecord({ dataRoot, platform }) {
  const file = credentialProviderRecordFile({ dataRoot, platform });
  let meta;
  try { meta = fs.lstatSync(file); } catch (error) { if (error.code === "ENOENT") return null; throw error; }
  if (meta.isSymbolicLink() || !meta.isFile()) fail("RK_CRED_PROVIDER_RECORD", `${file} is not a regular file`);
  if (process.platform !== "win32" && (meta.mode & 0o022)) fail("RK_CRED_PROVIDER_RECORD", `${file} is writable by group or others`);
  let record;
  try { record = JSON.parse(fs.readFileSync(file, "utf8")); } catch (error) { fail("RK_CRED_PROVIDER_RECORD", `${file} is not JSON (${error.message})`); }
  if (record?.schema !== CREDENTIAL_PROVIDER_SCHEMA || record.platform !== platform || record.providers === null || typeof record.providers !== "object" || Array.isArray(record.providers)) {
    fail("RK_CRED_PROVIDER_RECORD", `${file} is not a ${CREDENTIAL_PROVIDER_SCHEMA} record for ${platform}`);
  }
  return { file, record };
}

/**
 * Enrols a gate-built rightkit-credentials (`binary`, an absolute path) of `version`: a COPY under the managed directory, its sha256 recorded. Enrols on the
 * host whose platform it is (the bytes are executed here for the identity probe). Returns `{ file, runnerFile, providerFile, entry, rotated }`.
 */
export function enrollCredentialProvider({ dataRoot, platform, binary, version, rotate = false, spawn = spawnSync, hostPlatform = process.platform, now = () => new Date() }) {
  checkPlatform(platform);
  if (HOST_PLATFORM[hostPlatform] !== platform) fail("RK_CRED_PROVIDER_PLATFORM", `a ${platform} provider is enrolled on a ${platform} host (this is ${hostPlatform}): its bytes are executed to check what they are`);
  checkVersion(version);
  const target = credentialProviderFile({ dataRoot, platform, version });
  if (!binary || !path.isAbsolute(String(binary))) fail("RK_CRED_PROVIDER_PATH", "the provider must be named by an absolute path");
  let source; let meta;
  try { source = fs.realpathSync.native(binary); meta = fs.statSync(source); } catch (error) { fail("RK_CRED_PROVIDER_PATH", `the provider ${binary} cannot be resolved (${error.code ?? error.message})`); }
  if (!meta.isFile()) fail("RK_CRED_PROVIDER_PATH", `${source} is not a regular file`);
  if (platform !== "win" && (meta.mode & 0o111) === 0) fail("RK_CRED_PROVIDER_PATH", `${source} is not executable`);
  assertIsCredentialCli(source, spawn);
  const existing = readRecord({ dataRoot, platform });
  const had = existing?.record.providers?.[version] ?? null;
  if (had && !rotate) fail("RK_CRED_PROVIDER_EXISTS", `${existing.file} already enrols rightkit-credentials ${version} (sha256 ${String(had.sha256).slice(0, 12)}); replacing it is a rotation (pass rotate)`);
  fs.mkdirSync(path.dirname(target), { recursive: true, mode: 0o700 });
  const staging = `${target}.${crypto.randomUUID()}.tmp`;
  try {
    fs.copyFileSync(source, staging, fs.constants.COPYFILE_EXCL);
    fs.chmodSync(staging, 0o755);
    assertIsCredentialCli(staging, spawn);
    fs.renameSync(staging, target);
  } finally { fs.rmSync(staging, { force: true }); }
  const entry = { path: target, sha256: sha256OfFile(target), version };
  const record = { schema: CREDENTIAL_PROVIDER_SCHEMA, platform, providers: { ...(existing?.record.providers ?? {}), [version]: entry }, enrolledAt: now().toISOString() };
  const file = credentialProviderRecordFile({ dataRoot, platform });
  const tmp = `${file}.${crypto.randomUUID()}.tmp`;
  try { fs.writeFileSync(tmp, `${JSON.stringify(record, null, 2)}\n`, { flag: "wx", mode: 0o600 }); fs.renameSync(tmp, file); } finally { fs.rmSync(tmp, { force: true }); }
  return { file, runnerFile: target, providerFile: target, entry, rotated: Boolean(had) };
}

/**
 * The enrolled provider: the NEWEST recorded version, `{ file, sha256, version, source: "managed" }`, or a typed refusal. The file the record names must be the
 * managed copy (inside the managed directory, not a link) and its bytes must still hash to the recorded sha256. It is never executed here and there is no
 * fallback: a missing enrolment names the install.
 */
export function resolveCredentialProvider({ dataRoot, platform }) {
  checkPlatform(platform);
  const read = readRecord({ dataRoot, platform });
  const versions = Object.keys(read?.record.providers ?? {}).filter((v) => parse(v)).sort((a, b) => compare(parse(b), parse(a)));
  if (!versions.length) fail("RK_CRED_PROVIDER_MISSING", `OS credential provider unavailable: rightkit-credentials >=${MIN_PROVIDER_VERSION} is not enrolled for ${platform} (${credentialProviderRecordFile({ dataRoot, platform })}); install it with \`${CREDENTIAL_PROVIDER_INSTALL}\``);
  const version = versions[0];
  const entry = read.record.providers[version];
  const expected = credentialProviderFile({ dataRoot, platform, version });
  if (entry.version !== version || !SHA256.test(String(entry.sha256)) || entry.path !== expected) fail("RK_CRED_PROVIDER_RECORD", `${read.file}: the entry for ${version} is malformed or names a file other than ${expected}`);
  let meta;
  try { meta = fs.lstatSync(expected); } catch (error) { fail("RK_CRED_PROVIDER_MISSING", `OS credential provider unavailable: the enrolled rightkit-credentials ${version} is gone (${expected}: ${error.code ?? error.message}); install it again with \`${CREDENTIAL_PROVIDER_INSTALL}\``); }
  if (meta.isSymbolicLink() || !meta.isFile()) fail("RK_CRED_PROVIDER_RECORD", `${expected} is not a regular file`);
  if (!isInside(fs.realpathSync.native(expected), fs.realpathSync.native(path.join(dataRoot, CREDENTIAL_PROVIDER_DIR)))) fail("RK_CRED_PROVIDER_RECORD", `${expected} resolves outside ${path.join(dataRoot, CREDENTIAL_PROVIDER_DIR)}`);
  const actual = sha256OfFile(expected);
  if (actual !== entry.sha256) fail("RK_CRED_PROVIDER_DIGEST", `the enrolled rightkit-credentials ${version} at ${expected} has sha256 ${actual}, not the recorded ${entry.sha256}; it changed after enrolment, so it is not run (install it again with \`${CREDENTIAL_PROVIDER_INSTALL}\`, then rotate)`);
  return { file: expected, sha256: actual, version, source: "managed" };
}

/**
 * The provider updater signing runs on a host: the enrolled one on a Mac or Windows host (its managed data root, `hostPlatform` decides), the bare PATH name
 * only where no managed data root exists (Linux, CI). Returns `{ file, managed }`; a managed host with no usable enrolment throws the typed refusal.
 */
export function updaterCredentialProvider({ env = process.env, hostPlatform = process.platform, dataRoot, resolve = resolveCredentialProvider } = {}) {
  const platform = providerPlatformOf(hostPlatform);
  if (!platform) return { file: "rightkit-credentials", managed: false };
  return { file: resolve({ dataRoot: dataRoot ?? resolveManagedRoots({ env, platform: hostPlatform }).dataRoot, platform }).file, managed: true };
}

/** `node credential-provider.mjs enroll --platform mac|win --binary <built rightkit-credentials> --version <v> [--data-root <dir>] [--rotate]` or `show`. */
export function main(argv, { out = (m) => process.stdout.write(`${m}\n`), err = (m) => process.stderr.write(`${m}\n`), env = process.env, spawn, program = "credential-provider.mjs" } = {}) {
  const [command, ...rest] = argv;
  const opt = (name) => { const i = rest.indexOf(name); return i < 0 ? undefined : rest[i + 1]; };
  try {
    const platform = opt("--platform") ?? HOST_PLATFORM[process.platform];
    const dataRoot = opt("--data-root") ?? resolveManagedRoots({ env }).dataRoot;
    if (command === "enroll") {
      const r = enrollCredentialProvider({ dataRoot, platform, binary: opt("--binary") && path.resolve(opt("--binary")), version: opt("--version"), rotate: rest.includes("--rotate"), ...(spawn ? { spawn } : {}) });
      out(`enrolled rightkit-credentials ${r.entry.version} for ${platform} (sha256 ${r.entry.sha256}) at ${r.providerFile}${r.rotated ? " (rotation)" : ""}`);
      return 0;
    }
    if (command === "show") {
      const read = readRecord({ dataRoot, platform });
      out(read ? JSON.stringify(read.record) : "no enrolment");
      return read ? 0 : 1;
    }
    err(`usage: ${program} enroll --platform mac|win --binary <built rightkit-credentials> --version <v> [--data-root <dir>] [--rotate]\n       ${program} show [--platform mac|win] [--data-root <dir>]`);
    return 2;
  } catch (error) { err(`${error.code ?? "error"}: ${error.message}`); return 1; }
}

if (process.argv[1] && (() => { try { return fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; } })()) process.exit(main(process.argv.slice(2)));
