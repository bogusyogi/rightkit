#!/usr/bin/env node
// Provision a PINNED Chrome for Testing for the publish verify job (@rightkit/qa and @rightkit/qa-chrome
// run real Chrome and have no skip). The workspace installer (`rightkit-qa-chrome install`) is not used here:
// it downloads Google's moving "last known good Stable" and records, but does not check, a digest. This
// script installs only what ci/chrome-for-testing.json pins (version, sha256 per platform; the URL is derived
// from the version), refuses a digest mismatch, and holds no secret. An EMPTY sha256 is a record request: in a
// dry run (PUBLISH_DRY_RUN=true) the download is hashed, printed as one CHROME_PIN line and deleted, and the step
// fails; it is never installed or run, and a real publish refuses it. Run: RIGHTKIT_CHROME_FOR_TESTING_ROOT=<dir> node provision-chrome.mjs
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
export const CHROME_NPM_DIRS = ["packages/qa", "packages/qa-chrome"];
export const CHROME_RUST_CRATES = ["rightkit-browser", "rightkit-qa", "rightkit-pdf"];
export function needsChrome(selection) {
  return (selection?.npm?.test ?? []).some((dir) => CHROME_NPM_DIRS.includes(dir)) || (selection?.rust?.test ?? []).some((name) => CHROME_RUST_CRATES.includes(name));
}

/**
 * Install the pinned build under <root>/<version> unless that directory already holds this exact pin (install.json
 * carries the verified sha256), so a second call in the same job, or a persistent runner, downloads nothing.
 * Returns { exe, downloaded }. An empty sha256 is a record request and is refused here (see main).
 */
export async function provision({ root, pins, key = platformKey(), fetchImpl = fetch, log = console.log }) {
  const entry = pinFor(pins, key);
  if (entry.sha256 === "") throw new Error(`${key} has no pinned sha256; record it with a publish dry run`);
  const versionDir = path.join(root, pins.version);
  const exe = executable(versionDir, key);
  const marker = path.join(versionDir, "install.json");
  let recorded = null;
  try { recorded = JSON.parse(fs.readFileSync(marker, "utf8")).sha256; } catch { /* not installed */ }
  if (fs.existsSync(exe) && recorded === entry.sha256) {
    log(`Chrome for Testing ${pins.version} (${key}) already provisioned: ${exe}`);
    return { exe, downloaded: false };
  }
  fs.mkdirSync(root, { recursive: true });
  const staging = fs.mkdtempSync(path.join(root, ".staging-"));
  try {
    const response = await fetchImpl(entry.url, { redirect: "error", signal: AbortSignal.timeout(600_000) });
    if (!response.ok) throw new Error(`download failed: HTTP ${response.status}`);
    const bytes = Buffer.from(await response.arrayBuffer());
    const digest = crypto.createHash("sha256").update(bytes).digest("hex");
    if (digest !== entry.sha256) throw new Error(`sha256 of the download ${digest} is not the pinned ${entry.sha256}`);
    const zip = path.join(staging, "chrome.zip");
    fs.writeFileSync(zip, bytes);
    const out = path.join(staging, "out"); fs.mkdirSync(out);
    const unzip = process.platform === "win32" ? spawnSync(path.join(process.env.SystemRoot || "C:\\Windows", "System32", "tar.exe"), ["-xf", zip, "-C", out], { stdio: "inherit", windowsHide: true })
      : process.platform === "darwin" ? spawnSync("ditto", ["-x", "-k", zip, out], { stdio: "inherit" })
        : spawnSync("unzip", ["-q", zip, "-d", out], { stdio: "inherit" });
    if (unzip.status !== 0) throw new Error("extracting Chrome for Testing failed");
    if (!fs.existsSync(executable(out, key))) throw new Error("the archive has no Chrome for Testing executable");
    fs.writeFileSync(path.join(out, "install.json"), `${JSON.stringify({ version: pins.version, platform: key, url: entry.url, sha256: digest })}\n`);
    fs.rmSync(versionDir, { recursive: true, force: true });
    fs.renameSync(out, versionDir);
    log(`provisioned Chrome for Testing ${pins.version} (${key}), sha256 ${digest}: ${exe}`);
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
