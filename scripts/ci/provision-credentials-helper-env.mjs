#!/usr/bin/env node
// Export the sealed signed-helper route for the credentials R2 consumer
// (rightkit-credentials macOS native-helper run: RK_SOURCE_HELPER_APP /
// RK_CONSUMER_HELPER_NAME / RK_CONSUMER_HELPER_TEAM_ID).
//
// Reads only the same-run signed artifact that sign-credentials-helper handed
// over under RUNNER_TEMP. Never downloads, builds, signs, authenticates or
// runs the helper: signature, designated requirement and sole access-group
// verification belong to ci/consumers/rightkit-credentials/
// prepare_signed_helper.py at staging time. Fails closed (non-zero exit, no
// GITHUB_ENV write) unless the signed root is a real directory inside
// RUNNER_TEMP whose layout, executable hash and same-run source receipt match
// the sealed handover. Values never come from the candidate.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const HELPER_NAME = "RightKitCredentialsQualification";
export const HELPER_TEAM_ID = "6KLGD3LLKF";
export const HELPER_BUNDLE_ID = "org.rightkit.qualification.credentials";
export const HELPER_ACCESS_GROUP = `${HELPER_TEAM_ID}.${HELPER_BUNDLE_ID}`;
export const HELPER_APP = `${HELPER_NAME}.app`;
export const HELPER_EXECUTABLE = `Contents/MacOS/${HELPER_NAME}`;
export const HELPER_PLIST = "Contents/Info.plist";
export const HELPER_PROFILE = "Contents/embedded.provisionprofile";
// Exact sealed handover from sign-credentials-helper: the signed app plus its
// executable hash, same-run receipt, carried unsigned receipt, carried
// entitlements and carried source-inputs. No extra file may ride along.
export const SIGNED_FILES = [
  HELPER_APP,
  "entitlements.plist",
  "helper.sha256",
  "source-inputs.json",
  "source-receipt.txt",
  "unsigned-source-receipt.txt",
];
const HEX64 = /^[0-9a-f]{64}$/;

const fail = (why) => { throw new Error(`provision-credentials-helper-env refused: ${why}`); };

const real = (p) => { try { return fs.realpathSync(p); } catch { return null; } };

const sorted = (list) => [...list].sort();
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

function readLines(file) {
  try {
    return fs.readFileSync(file, "utf8").split("\n").filter((line) => line.length > 0);
  } catch {
    fail(`cannot read ${path.basename(file)}`);
  }
}

/**
 * Resolve the credentials helper env without touching GITHUB_ENV. `github`
 * defaults to this run's GitHub context. Throws unless every input is trusted
 * and the sealed handover matches this run.
 */
export function resolveCredentialsHelperEnv({ signedRoot, runnerTemp, github = {} }) {
  const ctx = {
    repository: github.repository ?? process.env.GITHUB_REPOSITORY,
    sha: github.sha ?? process.env.GITHUB_SHA,
    runId: github.runId ?? process.env.GITHUB_RUN_ID,
    runAttempt: github.runAttempt ?? process.env.GITHUB_RUN_ATTEMPT,
    workflowRef: github.workflowRef ?? process.env.GITHUB_WORKFLOW_REF,
  };
  if (!signedRoot || !path.isAbsolute(signedRoot) || signedRoot.split(path.sep).includes("..")) fail("signed root must be an absolute path without parent traversal");
  const temp = runnerTemp && real(runnerTemp);
  if (!temp) fail("RUNNER_TEMP scratch root is absent");
  let stat;
  try { stat = fs.lstatSync(signedRoot); } catch { fail("signed root does not exist"); }
  if (stat.isSymbolicLink()) fail("signed root must not be a symlink");
  const root = real(signedRoot);
  if (!root || !fs.statSync(root).isDirectory()) fail("signed root must be an existing real directory");
  if (root !== temp && (path.relative(temp, root).startsWith(`..${path.sep}`) || path.relative(temp, root) === "..")) fail("signed root must be inside RUNNER_TEMP");
  const entries = fs.readdirSync(root);
  if (!same(sorted(entries), sorted(SIGNED_FILES))) fail("signed handover must contain exactly the sealed helper files");
  for (const entry of entries) {
    if (fs.lstatSync(path.join(root, entry)).isSymbolicLink()) fail("signed handover must not contain symlinks");
  }
  const app = path.join(root, HELPER_APP);
  if (!fs.statSync(app).isDirectory()) fail("signed helper app must be a directory");
  const executable = path.join(app, HELPER_EXECUTABLE);
  const plist = path.join(app, HELPER_PLIST);
  const profile = path.join(app, HELPER_PROFILE);
  for (const [file, what] of [[executable, "helper executable"], [plist, "helper Info.plist"], [profile, "embedded provisioning profile"]]) {
    let link;
    try { link = fs.lstatSync(file); } catch { fail(`signed ${what} is missing`); }
    if (link.isSymbolicLink()) fail(`signed ${what} must not be a symlink`);
    if (!link.isFile()) fail(`signed ${what} is missing`);
  }
  const info = fs.readFileSync(plist, "utf8");
  if (!info.includes(`<string>${HELPER_BUNDLE_ID}</string>`) || !info.includes(`<string>${HELPER_NAME}</string>`)) fail("helper Info.plist identity mismatch");
  const recorded = readLines(path.join(root, "helper.sha256"));
  if (recorded.length !== 1) fail("helper hash file must hold exactly one entry");
  const digest = recorded[0].split(" ")[0];
  if (!HEX64.test(digest) || !recorded[0].endsWith(`  ${HELPER_NAME}`)) fail("helper hash entry is malformed");
  const actual = crypto.createHash("sha256").update(fs.readFileSync(executable)).digest("hex");
  if (actual !== digest) fail("signed helper executable does not match the sealed hash");
  const receipt = readLines(path.join(root, "source-receipt.txt"));
  const fields = {};
  for (const line of receipt) {
    const at = line.indexOf("=");
    if (at > 0) fields[line.slice(0, at)] = line.slice(at + 1);
  }
  for (const [key, value] of [["repository", ctx.repository], ["source", ctx.sha], ["run", ctx.runId], ["attempt", ctx.runAttempt], ["workflow", ctx.workflowRef]]) {
    if (!value || fields[key] !== value) fail("signed helper receipt does not match this run");
  }
  if (fields.job !== "sign-credentials-helper") fail("signed helper receipt names another job");
  if (!receipt.includes(recorded[0])) fail("signed helper receipt does not bind the sealed hash");
  return {
    RK_SOURCE_HELPER_APP: path.join(root, HELPER_APP),
    RK_CONSUMER_HELPER_NAME: HELPER_NAME,
    RK_CONSUMER_HELPER_TEAM_ID: HELPER_TEAM_ID,
  };
}

function main() {
  const signedRoot = process.env.CREDENTIALS_HELPER_SIGNED_ROOT;
  if (!signedRoot) fail("CREDENTIALS_HELPER_SIGNED_ROOT is not set");
  const githubEnv = process.env.GITHUB_ENV;
  if (!githubEnv) fail("GITHUB_ENV is not set");
  const env = resolveCredentialsHelperEnv({ signedRoot, runnerTemp: process.env.RUNNER_TEMP });
  for (const [name, value] of Object.entries(env)) {
    if (typeof value !== "string" || value.includes("\n")) fail(`unexportable value for ${name}`);
  }
  fs.appendFileSync(githubEnv, Object.entries(env).map(([name, value]) => `${name}=${value}\n`).join(""));
  for (const [name, value] of Object.entries(env)) console.log(`${name}=${name === "RK_SOURCE_HELPER_APP" ? value : value}`);
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  try { main(); } catch (error) { console.error(`provision-credentials-helper-env: ${error.message}`); process.exit(1); }
}
