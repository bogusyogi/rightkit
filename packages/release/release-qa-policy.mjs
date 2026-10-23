// QA admission policy (ADR-0001 item 10): the per-app warn -> refuse switch and the approved rightkit-qa runner
// digests, recorded in `qa-admission-policy.json` next to this module.
//
//   { "schema": 1,
//     "apps": { "<app>": { "mode": "warn" | "refuse", "since": "YYYY-MM-DD", "set_by": "<name>" } },
//     "approvedRunners": [ { "sha256": "<64 hex>", "version": "<rightkit-qa version>", "note": "..." } ] }
//
// An app with no entry is in `warn` mode (default). In `refuse` mode `admit()` throws instead of warning.
//
// Only the owner (`adrian`) may move an app back from `refuse` to `warn` (or delete its entry). What is checkable
// here, and enforced:
//   - every write through `setAdmissionMode` goes through `checkPolicyTransition`, which refuses a downgrade whose
//     new entry is not `set_by: "adrian"`;
//   - every load compares the working file with the copy committed at `baselineRef` (default HEAD) and refuses a
//     hand-edited downgrade the same way, so an uncommitted edit cannot quietly switch an app off refuse;
//   - an unreadable or malformed policy file is a hard error, never a silent fall back to warn.
// What is NOT checkable from this file: `set_by` is a recorded claim, not an authenticated identity, and a
// downgrade that was already committed is the new baseline. The file must therefore be owner-protected in the
// repository (CODEOWNERS on this path plus branch protection, and the file belongs in the protected policy bundle,
// see ownership-policy-bundle.mjs). CI should set RIGHT_RELEASE_QA_POLICY_BASELINE_REF=origin/main so the baseline
// is the protected branch rather than the local HEAD.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { canonicalJson } from "./ownership-approval.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const HEX64 = /^[a-f0-9]{64}$/;
const DATE = /^\d{4}-\d{2}-\d{2}$/;

export const QA_ADMISSION_POLICY_FILE = "qa-admission-policy.json";
export const QA_POLICY_BASELINE_REF_ENV = "RIGHT_RELEASE_QA_POLICY_BASELINE_REF";
export const ADMISSION_MODES = Object.freeze(["warn", "refuse"]);
/** The only `set_by` that may move an app from refuse back to warn. */
export const POLICY_OWNER = "adrian";

export class QaPolicyError extends Error {
  constructor(message) {
    super(message);
    this.name = "QaPolicyError";
    this.code = "QA_POLICY_INVALID";
  }
}

export function validateAdmissionPolicy(policy) {
  const problems = [];
  if (!policy || typeof policy !== "object" || Array.isArray(policy)) return ["policy must be an object"];
  if (policy.schema !== 1) problems.push("policy schema must be 1");
  if (!policy.apps || typeof policy.apps !== "object" || Array.isArray(policy.apps)) problems.push("policy apps must be an object");
  else {
    for (const [app, entry] of Object.entries(policy.apps)) {
      if (!app.trim()) problems.push("policy app name is empty");
      if (!entry || typeof entry !== "object") { problems.push(`policy app ${app} must be an object`); continue; }
      if (!ADMISSION_MODES.includes(entry.mode)) problems.push(`policy app ${app} mode must be warn or refuse`);
      if (typeof entry.since !== "string" || !DATE.test(entry.since)) problems.push(`policy app ${app} since must be YYYY-MM-DD`);
      if (typeof entry.set_by !== "string" || !entry.set_by.trim()) problems.push(`policy app ${app} set_by is required`);
    }
  }
  if (!Array.isArray(policy.approvedRunners)) problems.push("policy approvedRunners must be an array");
  else {
    for (const runner of policy.approvedRunners) {
      if (!runner || typeof runner.sha256 !== "string" || !HEX64.test(runner.sha256)) problems.push("approvedRunners entries need a lowercase 64-hex sha256");
    }
  }
  return problems;
}

/** Problems when `next` weakens `previous`: a refuse entry that is gone or now warn without `set_by: "adrian"`. */
export function checkPolicyTransition(previous, next) {
  const problems = [];
  for (const [app, before] of Object.entries(previous?.apps ?? {})) {
    if (before?.mode !== "refuse") continue;
    const after = next?.apps?.[app];
    if (!after) problems.push(`${app}: the refuse entry was removed; only ${POLICY_OWNER} may move an app back to warn (set mode warn with set_by "${POLICY_OWNER}")`);
    else if (after.mode !== "refuse" && after.set_by !== POLICY_OWNER) problems.push(`${app}: refuse -> warn requires set_by "${POLICY_OWNER}", got ${JSON.stringify(after.set_by)}`);
  }
  return problems;
}

const digestOf = (policy) => createHash("sha256").update(canonicalJson(policy)).digest("hex");

function parsePolicyText(text, label) {
  let policy;
  try { policy = JSON.parse(text); } catch (error) { throw new QaPolicyError(`QA admission policy is unreadable (${label}): ${error.message}`); }
  const problems = validateAdmissionPolicy(policy);
  if (problems.length) throw new QaPolicyError(`QA admission policy is invalid (${label}): ${problems.join("; ")}`);
  return policy;
}

function gitBaselineText({ dir, ref, runner = spawnSync }) {
  try {
    const result = runner("git", ["-C", dir, "show", `${ref}:./${QA_ADMISSION_POLICY_FILE}`], { encoding: "utf8", windowsHide: true });
    return result?.status === 0 ? String(result.stdout) : null;
  } catch { return null; }
}

/**
 * Load the policy. `baseline` (policy text) overrides the git lookup; `baseline: false` disables the comparison.
 * Returns { policy, digest, baseline: "supplied" | "git" | "none" }.
 */
export function loadAdmissionPolicy({ dir = here, baseline, baselineRef, env = process.env, gitRunner } = {}) {
  const file = path.join(dir, QA_ADMISSION_POLICY_FILE);
  if (!existsSync(file)) throw new QaPolicyError(`QA admission policy is missing (${QA_ADMISSION_POLICY_FILE}); it ships with @rightkit/release and is never optional`);
  const policy = parsePolicyText(readFileSync(file, "utf8"), QA_ADMISSION_POLICY_FILE);
  let source = "none";
  let baselineText = null;
  if (typeof baseline === "string") { baselineText = baseline; source = "supplied"; }
  else if (baseline !== false) {
    baselineText = gitBaselineText({ dir, ref: baselineRef ?? env?.[QA_POLICY_BASELINE_REF_ENV] ?? "HEAD", runner: gitRunner });
    if (baselineText !== null) source = "git";
  }
  if (baselineText !== null) {
    const problems = checkPolicyTransition(parsePolicyText(baselineText, "baseline"), policy);
    if (problems.length) throw new QaPolicyError(`QA admission policy weakens its baseline: ${problems.join("; ")}`);
  }
  return { policy, digest: digestOf(policy), baseline: source };
}

/** The mode for an app; an app with no entry is `warn`. */
export function admissionModeFor(policy, app) {
  const entry = policy?.apps?.[app];
  return entry?.mode === "refuse" ? { mode: "refuse", since: entry.since, set_by: entry.set_by } : { mode: "warn", since: entry?.since ?? null, set_by: entry?.set_by ?? null };
}

export const approvedRunnerDigests = (policy) => (policy?.approvedRunners ?? []).map((runner) => runner.sha256);

/**
 * Record an app's mode. A downgrade (refuse -> warn) is refused unless `setBy` is the owner. Writes atomically.
 * The file is read without the git baseline here (the transition rule is applied directly to what is on disk).
 */
export function setAdmissionMode({ app, mode, setBy, dir = here, now = () => new Date() }) {
  if (!app || !String(app).trim()) throw new QaPolicyError("app is required");
  if (!ADMISSION_MODES.includes(mode)) throw new QaPolicyError("mode must be warn or refuse");
  if (!setBy || !String(setBy).trim()) throw new QaPolicyError("set_by is required");
  const file = path.join(dir, QA_ADMISSION_POLICY_FILE);
  const current = parsePolicyText(readFileSync(file, "utf8"), QA_ADMISSION_POLICY_FILE);
  const next = { ...current, apps: { ...current.apps, [app]: { mode, since: now().toISOString().slice(0, 10), set_by: String(setBy) } } };
  const problems = checkPolicyTransition(current, next);
  if (problems.length) throw new QaPolicyError(`QA admission policy change refused: ${problems.join("; ")}`);
  const sorted = { ...next, apps: Object.fromEntries(Object.entries(next.apps).sort(([a], [b]) => a.localeCompare(b))) };
  mkdirSync(path.dirname(file), { recursive: true });
  const temp = `${file}.${process.pid}.tmp`;
  writeFileSync(temp, `${JSON.stringify(sorted, null, 2)}\n`);
  renameSync(temp, file);
  return sorted.apps[app];
}

// `node release-qa-policy.mjs show | set --app <app> --mode warn|refuse --set-by <name>`
export function main(args, { stdout = (text) => console.log(text), stderr = (text) => console.error(text) } = {}) {
  const value = (flag) => { const index = args.indexOf(flag); return index >= 0 ? args[index + 1] : undefined; };
  try {
    if (args[0] === "show") { stdout(JSON.stringify(loadAdmissionPolicy().policy, null, 2)); return 0; }
    if (args[0] === "set") {
      const entry = setAdmissionMode({ app: value("--app"), mode: value("--mode"), setBy: value("--set-by") });
      stdout(`qa admission: ${value("--app")} -> ${entry.mode} (since ${entry.since}, set_by ${entry.set_by})`);
      return 0;
    }
    stderr("usage: release-qa-policy.mjs show | set --app <app> --mode warn|refuse --set-by <name>");
    return 2;
  } catch (error) {
    stderr(`release-qa-policy: ${error.message}`);
    return 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) process.exit(main(process.argv.slice(2)));
