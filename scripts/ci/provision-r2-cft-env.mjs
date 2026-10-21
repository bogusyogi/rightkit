#!/usr/bin/env node
// Export the trusted pinned-CFT route for R2 registry consumers (rightkit-browser
// browser-r2 run: RK_R2_CFT_ROOT / RK_R2_CFT_SHA256 / RK_R2_HEADLESS_SHELL_SHA256).
//
// Reads only ci/chrome-for-testing.json pins and the managed root that
// provision-chrome.mjs already installed under RUNNER_TEMP. Never downloads,
// installs, or runs Chrome. Fails closed (non-zero exit, no GITHUB_ENV write)
// unless the managed root is a real directory inside RUNNER_TEMP whose
// current/install.json marker matches the pin for this host, with the
// executables present. Values never come from the candidate.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { executable, pinFor, platformKey, shellExecutable, shellPinFor } from "./provision-chrome.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const HEX64 = /^[0-9a-f]{64}$/;

const fail = (why) => { throw new Error(`provision-r2-cft-env refused: ${why}`); };

const real = (p) => { try { return fs.realpathSync(p); } catch { return null; } };

/**
 * Resolve the R2 CFT env values without touching GITHUB_ENV. `key` defaults to
 * this host's Chrome-for-Testing platform key. Throws unless every input is
 * trusted and the provisioned install matches the pin.
 */
export function resolveR2CftEnv({ managedRoot, runnerTemp, pins, key = platformKey() }) {
  if (!managedRoot || !path.isAbsolute(managedRoot) || managedRoot.split(path.sep).includes("..")) fail("managed root must be an absolute path without parent traversal");
  const temp = runnerTemp && real(runnerTemp);
  if (!temp) fail("RUNNER_TEMP scratch root is absent");
  let stat;
  try { stat = fs.lstatSync(managedRoot); } catch { fail("managed root does not exist"); }
  if (stat.isSymbolicLink()) fail("managed root must not be a symlink");
  const root = real(managedRoot);
  if (!root || !fs.statSync(root).isDirectory()) fail("managed root must be an existing real directory");
  if (root !== temp && (path.relative(temp, root).startsWith(`..${path.sep}`) || path.relative(temp, root) === "..")) fail("managed root must be inside RUNNER_TEMP");
  const pin = pinFor(pins, key);
  if (!HEX64.test(pin.sha256 ?? "")) fail(`${key} has no pinned sha256; record it with a publish dry run`);
  const current = path.join(root, "current");
  let recorded;
  try { recorded = JSON.parse(fs.readFileSync(path.join(current, "install.json"), "utf8")); } catch { fail("provisioned Chrome for Testing marker is missing; provision-chrome.mjs must run first"); }
  const files = Array.isArray(recorded?.files) ? recorded.files : [];
  const chrome = files.find((f) => f?.kind === "chrome");
  if (recorded?.version !== pins.version || recorded?.platform !== key || chrome?.sha256 !== pin.sha256) fail("provisioned install does not match the pinned Chrome for Testing");
  if (!fs.existsSync(executable(current, key))) fail("provisioned Chrome for Testing executable is missing");
  const env = { RK_R2_CFT_ROOT: root, RK_R2_CFT_SHA256: pin.sha256 };
  const shell = shellPinFor(pins, key);
  if (shell) {
    const shellFile = files.find((f) => f?.kind === "headless_shell");
    if (shellFile?.sha256 !== shell.sha256) fail("provisioned install does not match the pinned headless shell");
    if (!fs.existsSync(shellExecutable(current, key))) fail("provisioned headless shell executable is missing");
    env.RK_R2_HEADLESS_SHELL_SHA256 = shell.sha256;
  }
  return env;
}

function main() {
  const managedRoot = process.env.RK_R2_CFT_MANAGED_ROOT;
  if (!managedRoot) fail("RK_R2_CFT_MANAGED_ROOT is not set");
  const githubEnv = process.env.GITHUB_ENV;
  if (!githubEnv) fail("GITHUB_ENV is not set");
  const pins = JSON.parse(fs.readFileSync(path.join(ROOT, "ci", "chrome-for-testing.json"), "utf8"));
  const env = resolveR2CftEnv({ managedRoot, runnerTemp: process.env.RUNNER_TEMP, pins });
  for (const [name, value] of Object.entries(env)) {
    if (typeof value !== "string" || value.includes("\n")) fail(`unexportable value for ${name}`);
  }
  fs.appendFileSync(githubEnv, Object.entries(env).map(([name, value]) => `${name}=${value}\n`).join(""));
  for (const [name, value] of Object.entries(env)) console.log(`${name}=${name === "RK_R2_CFT_ROOT" ? value : `${value.slice(0, 12)}…`}`);
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  try { main(); } catch (error) { console.error(`provision-r2-cft-env: ${error.message}`); process.exit(1); }
}
