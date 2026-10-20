// CI-only. Harness-chosen environment for runtime workloads. Imported by runtime-qualify.mjs, so it is part of
// the hash-checked trusted helper closure. A candidate never supplies a value: a trusted per-crate workload
// entry names the exact variables it needs; each name resolves to one provider below, and every value is
// created here, fresh per workload run, under a private directory that is deleted when the run ends.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

// Exact names the harness can provide. Anything not listed here is refused wherever a trusted entry names it.
export const ENV_PROVIDERS = Object.freeze({
  RIGHTKIT_RUNTIME_SCRATCH_ROOT: "scratch-root",
  RIGHTKIT_DAEMON_BUDGET_TLS_CERT: "tls-cert",
  RIGHTKIT_DAEMON_BUDGET_TLS_KEY: "tls-key",
});
const TLS_NAMES = Object.freeze(["RIGHTKIT_DAEMON_BUDGET_TLS_CERT", "RIGHTKIT_DAEMON_BUDGET_TLS_KEY"]);
export function assertEnvNames(names) {
  if (!Array.isArray(names) || new Set(names).size !== names.length) throw new Error("workload env must be a list of unique names");
  for (const name of names) if (typeof name !== "string" || !Object.hasOwn(ENV_PROVIDERS, name)) throw new Error(`workload env names no harness provider: ${String(name)}`);
  if (TLS_NAMES.some((name) => names.includes(name)) && !TLS_NAMES.every((name) => names.includes(name))) throw new Error("TLS certificate and key variables are provided together or not at all");
  return names;
}

// Absolute provisioned OpenSSL locations only. No PATH search: the runner image, not the environment, decides.
export const OPENSSL_CANDIDATES = Object.freeze({
  win32: Object.freeze(["C:\\Program Files\\OpenSSL-Win64\\bin\\openssl.exe", "C:\\Program Files\\Git\\usr\\bin\\openssl.exe", "C:\\Program Files\\Git\\mingw64\\bin\\openssl.exe"]),
  darwin: Object.freeze(["/opt/homebrew/opt/openssl@3/bin/openssl", "/usr/local/opt/openssl@3/bin/openssl", "/opt/homebrew/bin/openssl", "/usr/local/bin/openssl", "/usr/bin/openssl"]),
});
const FIXTURE_FILES = Object.freeze(["localhost-cert.pem", "localhost-key.pem", ".rightkit-daemon-tls-fixture", "localhost.cnf"]);
const FIXTURE_MARKER = "rightkit-daemon-runtime-tls-v1\n";
const FIXTURE_CONFIG = "[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=localhost\n";

/** One native utility call; never a shell, never output echoed. Throws without printing stderr (it can name paths). */
export function nativeRun(argv, { timeout = 30_000 } = {}) {
  const result = spawnSync(argv[0], argv.slice(1), { stdio: ["ignore", "pipe", "pipe"], timeout, windowsHide: true, shell: false, encoding: "utf8", maxBuffer: 1024 * 1024 });
  if (result.error || result.status !== 0) throw new Error(`provisioned utility failed: ${path.basename(argv[0])} (${result.error ? "spawn error" : `exit ${result.status}`})`);
  return result.stdout;
}

/** Owner-only directory: POSIX 0700 owned by this user; Windows removes inherited grants, then grants only this user's SID. */
export function ownerOnlyDirectory(directory, { platform = process.platform, run = nativeRun, env = process.env, stat = fs.statSync, chmod = fs.chmodSync, uid = process.getuid?.() } = {}) {
  if (platform === "win32") {
    const root = env.SystemRoot ?? env.SYSTEMROOT;
    if (!root) throw new Error("SystemRoot unavailable for owner-only ACL");
    const text = run([path.join(root, "System32", "whoami.exe"), "/user", "/fo", "csv", "/nh"]);
    const sid = /"(S-1-[0-9-]+)"\s*$/.exec(text.replace(/^\uFEFF/, "").trim())?.[1];
    if (!sid) throw new Error("could not resolve current native owner SID");
    run([path.join(root, "System32", "icacls.exe"), directory, "/inheritance:r", "/grant:r", `*${sid}:(OI)(CI)F`]);
    return;
  }
  chmod(directory, 0o700);
  const info = stat(directory);
  if ((info.mode & 0o777) !== 0o700 || (uid !== undefined && info.uid !== uid)) throw new Error("owner-only scratch permissions required");
}

export function findOpenssl({ platform = process.platform, exists = (file) => fs.statSync(file, { throwIfNoEntry: false })?.isFile() === true, run = nativeRun } = {}) {
  for (const candidate of OPENSSL_CANDIDATES[platform] ?? []) {
    if (!exists(candidate)) continue;
    try { run([candidate, "version"]); return candidate; } catch { /* try the next provisioned location */ }
  }
  throw new Error(`no provisioned native OpenSSL at a fixed location on ${platform}`);
}

/** Disposable localhost certificate and key, valid one day, ephemeral RSA-2048. Returns file paths only. */
export function generateTlsFixture({ parent, openssl, run = nativeRun, platform = process.platform, env = process.env, fsx = fs } = {}) {
  if (!path.isAbsolute(parent) || !path.isAbsolute(openssl)) throw new Error("TLS fixture needs absolute parent and OpenSSL paths");
  run([openssl, "version"]);
  const directory = fsx.mkdtempSync(path.join(parent, "daemon-tls-"));
  const [certName, keyName, markerName, configName] = FIXTURE_FILES;
  try {
    ownerOnlyDirectory(directory, { platform, run, env });
    fsx.writeFileSync(path.join(directory, markerName), FIXTURE_MARKER, { mode: 0o600 });
    const cert = path.join(directory, certName), key = path.join(directory, keyName), config = path.join(directory, configName);
    // An explicit minimal config keeps ambient OpenSSL configuration out of the certificate.
    fsx.writeFileSync(config, FIXTURE_CONFIG, { mode: 0o600 });
    run([openssl, "req", "-x509", "-newkey", "rsa:2048", "-sha256", "-nodes", "-config", config, "-days", "1", "-keyout", key, "-out", cert,
      "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1", "-addext", "basicConstraints=critical,CA:FALSE",
      "-addext", "keyUsage=critical,digitalSignature,keyEncipherment", "-addext", "extendedKeyUsage=serverAuth"], { timeout: 120_000 });
    for (const file of [cert, key]) if (!fsx.statSync(file, { throwIfNoEntry: false })?.size) throw new Error("OpenSSL did not produce a complete TLS fixture");
    if (platform !== "win32") for (const file of [cert, key]) fsx.chmodSync(file, 0o600);
    // The certificate must name localhost; a mismatch prints a message and still exits 0, so read it.
    if (!/does match certificate/i.test(String(run([openssl, "x509", "-in", cert, "-noout", "-checkhost", "localhost"])))) throw new Error("TLS fixture certificate does not name localhost");
    return { directory, certPath: cert, keyPath: key };
  } catch (error) {
    try { cleanupTlsFixture(directory, { fsx }); } catch { /* the original failure is the one to report */ }
    throw error;
  }
}

/** Fixed-name removal only; refuses a directory it did not create. */
export function cleanupTlsFixture(directory, { fsx = fs } = {}) {
  const info = fsx.lstatSync(directory, { throwIfNoEntry: false });
  if (!info) return;
  if (info.isSymbolicLink() || !info.isDirectory() || !path.basename(directory).startsWith("daemon-tls-")) throw new Error("not a generated TLS fixture directory");
  const entries = fsx.readdirSync(directory);
  const marker = path.join(directory, FIXTURE_FILES[2]);
  const markerInfo = fsx.lstatSync(marker, { throwIfNoEntry: false });
  if (entries.length && (!markerInfo?.isFile() || fsx.readFileSync(marker, "utf8") !== FIXTURE_MARKER)) throw new Error("missing fixture ownership marker");
  if (entries.some((name) => !FIXTURE_FILES.includes(name) || !fsx.lstatSync(path.join(directory, name)).isFile())) throw new Error("unexpected fixture content; refusing cleanup");
  for (const name of entries) fsx.unlinkSync(path.join(directory, name));
  fsx.rmdirSync(directory);
}

/**
 * Creates the declared variables for one workload run. Returns { values, dispose }: `values` holds exactly the
 * declared names; `dispose` removes every file created (key first) and throws if anything remains.
 */
export function provisionWorkloadEnv(names, deps = {}) {
  assertEnvNames(names);
  if (names.length === 0) return { values: {}, dispose() {} };
  const { tmpRoot = fs.realpathSync(os.tmpdir()), platform = process.platform, run = nativeRun, env = process.env, fsx = fs, discover = findOpenssl } = deps;
  const work = fsx.mkdtempSync(path.join(tmpRoot, "rightkit-runtime-"));
  let tls = null;
  const dispose = () => {
    const failures = [];
    if (tls) { try { cleanupTlsFixture(tls.directory, { fsx }); } catch (error) { failures.push(error.message); } }
    try { fsx.rmSync(work, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 }); } catch (error) { failures.push(error.message); }
    if (fsx.existsSync(work)) failures.push("private workload directory remains");
    if (failures.length) throw new Error(`workload environment cleanup failed: ${failures.join("; ")}`);
  };
  try {
    ownerOnlyDirectory(work, { platform, run, env });
    const values = {};
    const scratch = path.join(work, "scratch");
    fsx.mkdirSync(scratch);
    if (names.includes("RIGHTKIT_RUNTIME_SCRATCH_ROOT")) values.RIGHTKIT_RUNTIME_SCRATCH_ROOT = scratch;
    if (TLS_NAMES.every((name) => names.includes(name))) {
      const fixtureParent = path.join(work, "tls");
      fsx.mkdirSync(fixtureParent);
      tls = generateTlsFixture({ parent: fixtureParent, openssl: discover({ platform, run }), run, platform, env, fsx });
      values.RIGHTKIT_DAEMON_BUDGET_TLS_CERT = tls.certPath;
      values.RIGHTKIT_DAEMON_BUDGET_TLS_KEY = tls.keyPath;
    }
    return { values, dispose };
  } catch (error) {
    try { dispose(); } catch { /* report the provisioning failure */ }
    throw error;
  }
}

/** Child environment: ambient RIGHTKIT_* never passes; only the declared names, with harness values. */
export function workloadEnvironment(base, names, values) {
  assertEnvNames(names);
  if (JSON.stringify(Object.keys(values).sort()) !== JSON.stringify([...names].sort())) throw new Error("harness values differ from the declared workload env");
  const child = Object.fromEntries(Object.entries(base).filter(([name]) => !/^rightkit_/i.test(name)));
  return { ...child, ...values };
}
