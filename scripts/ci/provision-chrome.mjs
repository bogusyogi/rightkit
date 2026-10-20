#!/usr/bin/env node
// Provision only ci/chrome-for-testing.json's verified archives at <root>/current, using
// @rightkit/qa-chrome's fixed-path install.json contract. An empty pin may be recorded
// during an explicit dry run, but is never installed or run.
import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const PREFIX = "https://storage.googleapis.com/chrome-for-testing-public/";

export function platformKey(platform = process.platform, arch = process.arch) {
  if (platform === "darwin") return arch === "arm64" ? "mac-arm64" : "mac-x64";
  if (platform === "win32") return arch === "ia32" ? "win32" : "win64";
  return "linux64";
}
export function executable(versionDir, key) {
  const dir = path.join(versionDir, `chrome-${key}`);
  if (key.startsWith("mac")) return path.join(dir, "Google Chrome for Testing.app", "Contents", "MacOS", "Google Chrome for Testing");
  return path.join(dir, key.startsWith("win") ? "chrome.exe" : "chrome");
}

export const shellExecutable = (versionDir, key) => path.join(versionDir, `chrome-headless-shell-${key}`, key.startsWith("win") ? "chrome-headless-shell.exe" : "chrome-headless-shell");
export const shellDownloadUrl = (version, key) => `${PREFIX}${version}/${key}/chrome-headless-shell-${key}.zip`;

export const downloadUrl = (version, key) => `${PREFIX}${version}/${key}/chrome-${key}.zip`;

/**
 * The pin for one platform: { url, sha256 }. The URL is always derived from the pinned version (Google's
 * bucket layout), never taken from the file. sha256 is "" only for a record run (see main). Throws without a version.
 */
export function pinFor(pins, key) {
  if (!/^\d+\.\d+\.\d+\.\d+$/.test(pins?.version ?? "")) throw new Error("ci/chrome-for-testing.json has no pinned version; set \"version\" (a Chrome for Testing version such as 1.2.3.4)");
  const entry = pins?.platforms?.[key];
  if (!entry || typeof entry.sha256 !== "string") throw new Error(`ci/chrome-for-testing.json has no entry for ${key}`);
  if (entry.sha256 !== "" && !/^[0-9a-f]{64}$/.test(entry.sha256)) throw new Error(`the sha256 pinned for ${key} is not 64 lowercase hex characters`);
  return { url: downloadUrl(pins.version, key), sha256: entry.sha256 };
}

/**
 * A platform with an empty sha256 may only be RECORDED, and only in a dry run: download to a temp file, hash it,
 * print one CHROME_PIN line, delete it. The download is never installed or run. Returns the line.
 */
export async function recordPin({ version, key, url, dryRun, fetchImpl = fetch, tmpDir = fs.mkdtempSync(path.join(process.env.RUNNER_TEMP || os.tmpdir(), "rk-cft-pin-")) }) {
  if (!dryRun) throw new Error(`${key} has no pinned sha256 and this is not a dry run; a real publish never records or uses an unpinned download`);
  const file = path.join(tmpDir, "chrome.zip");
  try {
    const response = await fetchImpl(url, { redirect: "error", signal: AbortSignal.timeout(600_000) });
    if (!response.ok || !response.body) throw new Error(`download failed: HTTP ${response.status}`);
    const hash = crypto.createHash("sha256"); let bytes = 0;
    const out = fs.createWriteStream(file);
    for await (const chunk of response.body) { hash.update(chunk); bytes += chunk.length; if (!out.write(chunk)) await new Promise((r) => out.once("drain", r)); }
    await new Promise((r) => out.end(r));
    return `CHROME_PIN ${key} ${version} ${hash.digest("hex")} ${bytes}`;
  } finally { fs.rmSync(tmpDir, { recursive: true, force: true }); }
}

/** Units whose tests drive real Chrome for Testing: ordinary CI provisions the pin once per job before running any of them. */
export const CHROME_NPM_DIRS = ["packages/qa", "packages/qa-chrome", "packages/settings-ui", "packages/onboarding"];
export const CHROME_RUST_CRATES = ["rightkit-browser", "rightkit-qa", "rightkit-pdf"];
export function needsChrome(selection) {
  return (selection?.npm?.test ?? []).some((dir) => CHROME_NPM_DIRS.includes(dir)) || (selection?.rust?.test ?? []).some((name) => CHROME_RUST_CRATES.includes(name));
}

/**
 * Windows only: let Chrome's sandbox read the install. Chrome starts its network service and renderers in an
 * AppContainer, which may read and execute only what "ALL APPLICATION PACKAGES" (S-1-15-2-1) and "ALL RESTRICTED
 * APPLICATION PACKAGES" (S-1-15-2-2) are granted. Program Files carries that grant; a directory under the runner's
 * tool cache does not, so Chrome logs "Sandbox cannot access executable ... Access is denied", its network service
 * crashes at startup and a navigation issued meanwhile hangs or aborts. Granting again is harmless, so this runs on
 * every provision, including one that finds the pin already installed.
 */
export function grantSandboxRead(versionDir, { platform = process.platform, run = spawnSync } = {}) {
  if (platform !== "win32") return false;
  const icacls = path.join(process.env.SystemRoot || "C:\\Windows", "System32", "icacls.exe");
  const done = run(icacls, [versionDir, "/grant", "*S-1-15-2-1:(OI)(CI)(RX)", "*S-1-15-2-2:(OI)(CI)(RX)", "/T", "/C", "/Q"], { stdio: ["ignore", "ignore", "inherit"], windowsHide: true });
  if (done.status !== 0) throw new Error(`granting Chrome's sandbox read access to ${versionDir} failed (icacls exit ${done.status})`);
  return true;
}

function extractZip(zip, out) {
  return process.platform === "win32" ? spawnSync(path.join(process.env.SystemRoot || "C:\\Windows", "System32", "tar.exe"), ["-xf", zip, "-C", out], { stdio: "inherit", windowsHide: true })
    : process.platform === "darwin" ? spawnSync("ditto", ["-x", "-k", zip, out], { stdio: "inherit" })
      : spawnSync("unzip", ["-q", zip, "-d", out], { stdio: "inherit" });
}

/**
 * The pinned chrome-headless-shell for one platform, or null when the pin file has none for it. Only win64 is pinned:
 * the Windows rightkit-browser fixture runs the shell because chrome.exe always starts the crashpad handler, which
 * cannot create its named pipe inside an AppContainer.
 */
export function shellPinFor(pins, key) {
  const entry = pins?.headlessShell?.[key];
  if (!entry) return null;
  if (!/^[0-9a-f]{64}$/.test(entry.sha256 ?? "")) throw new Error(`the headlessShell sha256 pinned for ${key} is not 64 lowercase hex characters`);
  return { url: shellDownloadUrl(pins.version, key), sha256: entry.sha256 };
}

/** Compatibility entry point: provision the complete managed install, including the optional shell. */
export async function provisionHeadlessShell(options) {
  const key = options.key ?? platformKey();
  if (!shellPinFor(options.pins, key)) return null;
  const { downloaded } = await provision({ ...options, key });
  return { exe: shellExecutable(path.join(options.root, "current"), key), downloaded };
}

/** Install Chrome plus every pinned companion atomically, matching the frozen consumer marker. */
export async function provision({ root, pins, key = platformKey(), fetchImpl = fetch, log = console.log, extract = extractZip }) {
  const entry = pinFor(pins, key);
  if (entry.sha256 === "") throw new Error(`${key} has no pinned sha256; record it with a publish dry run`);
  const shell = shellPinFor(pins, key);
  const files = [{ kind: "chrome", ...entry }, ...(shell ? [{ kind: "headless_shell", ...shell }] : [])];
  const expected = { version: pins.version, platform: key, files };
  const current = path.join(root, "current");
  const previous = path.join(root, "current.previous");
  const exe = executable(current, key);
  const complete = (dir) => fs.existsSync(executable(dir, key)) && (!shell || fs.existsSync(shellExecutable(dir, key)));
  let recorded;
  try { recorded = JSON.parse(fs.readFileSync(path.join(current, "install.json"), "utf8")); } catch { /* not installed */ }
  if (JSON.stringify(recorded) === JSON.stringify(expected) && complete(current)) {
    grantSandboxRead(current);
    log(`Chrome for Testing ${pins.version} (${key}) already provisioned: ${exe}`);
    return { exe, downloaded: false };
  }
  fs.mkdirSync(root, { recursive: true });
  const staging = fs.mkdtempSync(path.join(root, ".staging-"));
  try {
    const out = path.join(staging, "out"); fs.mkdirSync(out);
    for (const item of files) {
      const response = await fetchImpl(item.url, { redirect: "error", signal: AbortSignal.timeout(600_000) });
      if (!response.ok) throw new Error(`download failed: HTTP ${response.status}`);
      const bytes = Buffer.from(await response.arrayBuffer());
      const digest = crypto.createHash("sha256").update(bytes).digest("hex");
      if (digest !== item.sha256) throw new Error(`sha256 of the ${item.kind} download ${digest} is not the pinned ${item.sha256}`);
      const zip = path.join(staging, `${item.kind}.zip`);
      fs.writeFileSync(zip, bytes);
      if (extract(zip, out).status !== 0) throw new Error(`extracting ${item.kind} failed`);
    }
    if (!complete(out)) throw new Error("the archives have no required Chrome for Testing executable or headless shell");
    fs.writeFileSync(path.join(out, "install.json"), `${JSON.stringify(expected)}\n`);
    grantSandboxRead(out);
    fs.rmSync(previous, { recursive: true, force: true });
    const hadCurrent = fs.existsSync(current);
    if (hadCurrent) fs.renameSync(current, previous);
    try { fs.renameSync(out, current); }
    catch (error) {
      if (hadCurrent) fs.renameSync(previous, current);
      throw error;
    }
    log(`provisioned Chrome for Testing ${pins.version} (${key}): ${exe}`);
    return { exe, downloaded: true };
  } finally { fs.rmSync(staging, { recursive: true, force: true }); }
}

/** Provision the repository pin and return the environment tests read: the managed root and the executable. */
export async function provisionForTests({ root, pins = JSON.parse(fs.readFileSync(path.join(ROOT, "ci", "chrome-for-testing.json"), "utf8")), key, fetchImpl, log } = {}) {
  const { exe, downloaded } = await provision({ root, pins, key, fetchImpl, log });
  return { downloaded, env: { RIGHTKIT_CHROME_FOR_TESTING_ROOT: root, RIGHTKIT_CHROME_FOR_TESTING: exe } };
}

async function main() {
  const root = process.env.RIGHTKIT_CHROME_FOR_TESTING_ROOT;
  if (!root) throw new Error("RIGHTKIT_CHROME_FOR_TESTING_ROOT is not set");
  const dryRun = process.env.PUBLISH_DRY_RUN === "true";
  const pins = JSON.parse(fs.readFileSync(path.join(ROOT, "ci", "chrome-for-testing.json"), "utf8"));
  const key = platformKey();
  const entry = pinFor(pins, key);
  if (entry.sha256 === "") {
    const line = await recordPin({ version: pins.version, key, url: entry.url, dryRun });
    console.log(line);
    if (process.env.GITHUB_STEP_SUMMARY) fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY, `\n\`${line}\`\n`);
    throw new Error("pin recorded above; add it to ci/chrome-for-testing.json");
  }
  await provision({ root, pins, key });
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  main().catch((error) => { console.error(`provision-chrome: ${error.message}`); process.exit(1); });
}
