// QA admission policy (ADR-0001 item 10): the per-app, per-stage warn -> refuse switch and the approved rightkit-qa
// runners, recorded in `qa-admission-policy.json` next to this module.
//
//   { "schema": 1,
//     "apps": { "<app>": { "mode": "warn" | "refuse", "since": "YYYY-MM-DD", "set_by": "<name>",
//                          "stages": { "source":    { "mode": .., "since": .., "set_by": .. },
//                                      "installed": { "mode": .., "since": .., "set_by": .. } } } },
//     "approvedRunners": [ { "sha256": "<64 hex>", "version": "<rightkit-qa version>", "platform": "mac" | "win",
//                            "set_by": "<name>", "since": "YYYY-MM-DD", "note": "..." } ] }
//
// An app with no entry is in `warn` mode (default). In `refuse` mode `admit()` throws instead of warning.
//
// Stages: the build (`source`) and publication (`installed`) admissions each have their own mode. An app entry is the
// app-level { mode, since, set_by } (governs both stages), a `stages` object, or both; a stage entry overrides the
// app-level mode for that stage. An app entry needs a mode or a non-empty `stages`. Only `source` and `installed` are
// stage keys.
//
// Approved runners: `sha256` is required (lowercase hex). `platform` ("mac" | "win") binds the approval to the
// platform the receipt is sealed or published for; an entry without it approves the digest on every platform (legacy
// entries). `version`, `set_by`, `since` and `note` record who approved which rightkit-qa build and when. Add an entry
// with `approve-runner`, which checks the file's `--version` output against the published rightkit-qa version.
//
// Only the owner (`adrian`) may loosen admission. What is checkable here, and enforced:
//   - every write goes through `checkPolicyTransition`, which refuses (a) a downgrade of any app and stage from refuse
//     to anything else (a warn entry, a removed stage entry or a removed app) whose deciding entry is not
//     `set_by: "adrian"`, and (b) a new approved runner (a sha256 + platform pair not in the previous policy) that is
//     not `set_by: "adrian"`; removing a runner tightens admission and is always allowed;
//   - every load compares the working file with the copy committed at `baselineRef` (default HEAD) and refuses a
//     hand-edited loosening the same way, so an uncommitted edit cannot quietly switch an app off refuse or approve a
//     runner. A baseline ref that was asked for explicitly (the argument or RIGHT_RELEASE_QA_POLICY_BASELINE_REF) and
//     cannot be read is a hard error; only the default (HEAD, nothing requested) falls back to no baseline, for a
//     tree that is not a git checkout (an app's node_modules);
//   - an unreadable or malformed policy file is a hard error, never a silent fall back to warn.
// What is NOT checkable from this file: `set_by` is a recorded claim, not an authenticated identity, and a
// loosening that was already committed is the new baseline. The file must therefore be owner-protected in the
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
/** The admissions an app's mode can be set for: the build (source) and the publication (installed). */
export const ADMISSION_STAGES = Object.freeze(["source", "installed"]);
/** The platforms an approved runner can be bound to. */
export const RUNNER_PLATFORMS = Object.freeze(["mac", "win"]);
/** The only `set_by` that may move an app back from refuse, or approve a runner. */
export const POLICY_OWNER = "adrian";

export class QaPolicyError extends Error {
  constructor(message) {
    super(message);
    this.name = "QaPolicyError";
    this.code = "QA_POLICY_INVALID";
  }
}

const isObject = (value) => Boolean(value) && typeof value === "object" && !Array.isArray(value);
const nonEmpty = (value) => typeof value === "string" && value.trim() !== "";

function validateModeEntry(entry, label, problems) {
  if (!ADMISSION_MODES.includes(entry.mode)) problems.push(`${label} mode must be warn or refuse`);
  if (typeof entry.since !== "string" || !DATE.test(entry.since)) problems.push(`${label} since must be YYYY-MM-DD`);
  if (!nonEmpty(entry.set_by)) problems.push(`${label} set_by is required`);
}

function validateRunnerEntry(runner, problems) {
  const label = `approvedRunners entry ${runner.sha256.slice(0, 16)}`;
  if (runner.version !== undefined && !nonEmpty(runner.version)) problems.push(`${label} version must be a non-empty string`);
  if (runner.platform !== undefined && !RUNNER_PLATFORMS.includes(runner.platform)) problems.push(`${label} platform must be ${RUNNER_PLATFORMS.join(" or ")}`);
  if (runner.set_by !== undefined && !nonEmpty(runner.set_by)) problems.push(`${label} set_by must be a non-empty string`);
  if (runner.since !== undefined && (typeof runner.since !== "string" || !DATE.test(runner.since))) problems.push(`${label} since must be YYYY-MM-DD`);
  if (runner.note !== undefined && typeof runner.note !== "string") problems.push(`${label} note must be a string`);
}

export function validateAdmissionPolicy(policy) {
  const problems = [];
  if (!isObject(policy)) return ["policy must be an object"];
  if (policy.schema !== 1) problems.push("policy schema must be 1");
  if (!isObject(policy.apps)) problems.push("policy apps must be an object");
  else {
    for (const [app, entry] of Object.entries(policy.apps)) {
      const label = `policy app ${app}`;
      if (!app.trim()) problems.push("policy app name is empty");
      if (!entry || typeof entry !== "object") { problems.push(`${label} must be an object`); continue; }
      if (entry.stages !== undefined) {
        if (!isObject(entry.stages)) problems.push(`${label} stages must be an object`);
        else {
          if (!Object.keys(entry.stages).length && entry.mode === undefined) problems.push(`${label} needs a mode or a non-empty stages object`);
          for (const [stage, stageEntry] of Object.entries(entry.stages)) {
            if (!ADMISSION_STAGES.includes(stage)) { problems.push(`${label} has unknown stage ${JSON.stringify(stage)} (expected ${ADMISSION_STAGES.join(" or ")})`); continue; }
            if (!isObject(stageEntry)) { problems.push(`${label} stage ${stage} must be an object`); continue; }
            validateModeEntry(stageEntry, `${label} stage ${stage}`, problems);
          }
        }
      }
      // The app-level fields are required unless the app is governed by its stages alone.
      if (entry.mode !== undefined || entry.stages === undefined) validateModeEntry(entry, label, problems);
    }
  }
  if (!Array.isArray(policy.approvedRunners)) problems.push("policy approvedRunners must be an array");
  else {
    for (const runner of policy.approvedRunners) {
      if (!runner || typeof runner.sha256 !== "string" || !HEX64.test(runner.sha256)) problems.push("approvedRunners entries need a lowercase 64-hex sha256");
      else validateRunnerEntry(runner, problems);
    }
  }
  return problems;
}

/** The entry that decides `stage` for `app`: its stage entry, else its app-level entry when that has a mode, else none. */
function decidingEntry(policy, app, stage) {
  const entry = policy?.apps?.[app];
  return entry?.stages?.[stage] ?? (entry?.mode ? entry : undefined);
}

const runnerKey = (runner) => `${runner?.sha256}|${runner?.platform ?? ""}`;

/**
 * Problems when `next` loosens `previous`: for every app and stage, a refuse that is gone or now warn without the
 * deciding entry being `set_by: "adrian"`; and every approved runner that is new without `set_by: "adrian"`.
 */
export function checkPolicyTransition(previous, next) {
  const grouped = new Map();
  const note = (app, stage, body) => {
    const key = `${app}\u0000${body}`;
    if (!grouped.has(key)) grouped.set(key, { app, body, stages: [] });
    grouped.get(key).stages.push(stage);
  };
  for (const app of Object.keys(previous?.apps ?? {})) {
    for (const stage of ADMISSION_STAGES) {
      if (decidingEntry(previous, app, stage)?.mode !== "refuse") continue;
      const after = decidingEntry(next, app, stage);
      if (after?.mode === "refuse") continue;
      if (!after) note(app, stage, `the refuse entry was removed; only ${POLICY_OWNER} may move an app back to warn (set mode warn with set_by "${POLICY_OWNER}")`);
      else if (after.set_by !== POLICY_OWNER) note(app, stage, `refuse -> warn requires set_by "${POLICY_OWNER}", got ${JSON.stringify(after.set_by)}`);
    }
  }
  const problems = [...grouped.values()].map(({ app, body, stages }) => `${app} (${stages.join(", ")}): ${body}`);
  const known = new Set((previous?.approvedRunners ?? []).map(runnerKey));
  for (const runner of next?.approvedRunners ?? []) {
    if (known.has(runnerKey(runner)) || runner?.set_by === POLICY_OWNER) continue;
    problems.push(`approvedRunners: runner ${String(runner?.sha256).slice(0, 16)}${runner?.platform ? ` (${runner.platform})` : ""} is new with set_by ${JSON.stringify(runner?.set_by ?? null)}; only ${POLICY_OWNER} may approve a runner (set_by "${POLICY_OWNER}")`);
  }
  return problems;
}

const digestOf = (policy) => createHash("sha256").update(canonicalJson(policy)).digest("hex");
const sha256Of = (bytes) => createHash("sha256").update(bytes).digest("hex");

function parsePolicyText(text, label) {
  let policy;
  try { policy = JSON.parse(text); } catch (error) { throw new QaPolicyError(`QA admission policy is unreadable (${label}): ${error.message}`); }
  const problems = validateAdmissionPolicy(policy);
  if (problems.length) throw new QaPolicyError(`QA admission policy is invalid (${label}): ${problems.join("; ")}`);
  return policy;
}

/** { text } when git shows the policy file at `ref`; { text: null, detail } when it cannot. */
function gitBaselineText({ dir, ref, runner = spawnSync }) {
  try {
    const result = runner("git", ["-C", dir, "show", `${ref}:./${QA_ADMISSION_POLICY_FILE}`], { encoding: "utf8", windowsHide: true });
    if (result?.status === 0) return { text: String(result.stdout) };
    return { text: null, detail: String(result?.stderr ?? "").trim().split(/\r?\n/)[0] || `git exited ${result?.status ?? "without a status"}` };
  } catch (error) { return { text: null, detail: error.message }; }
}

/**
 * Load the policy. `baseline` (policy text) overrides the git lookup; `baseline: false` disables the comparison.
 * A baseline ref requested explicitly (`baselineRef` or the env var) that cannot be read is an error: the comparison
 * the caller asked for must not silently become no comparison. With no ref requested the baseline is HEAD, and a tree
 * that is not a git checkout has none.
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
    const requested = [baselineRef, env?.[QA_POLICY_BASELINE_REF_ENV]].find(nonEmpty);
    const ref = requested ?? "HEAD";
    const found = gitBaselineText({ dir, ref, runner: gitRunner });
    if (found.text === null && requested) throw new QaPolicyError(`QA admission policy baseline ${ref} cannot be read (${found.detail}); refusing to continue without the comparison that was requested`);
    baselineText = found.text;
    if (baselineText !== null) source = "git";
  }
  if (baselineText !== null) {
    const problems = checkPolicyTransition(parsePolicyText(baselineText, "baseline"), policy);
    if (problems.length) throw new QaPolicyError(`QA admission policy weakens its baseline: ${problems.join("; ")}`);
  }
  return { policy, digest: digestOf(policy), baseline: source };
}

/**
 * The mode for an app at `stage` ("source" | "installed"): the stage entry, else the app-level entry, else `warn`.
 * Without a stage it is the app-level result.
 */
export function admissionModeFor(policy, app, stage) {
  const entry = stage ? decidingEntry(policy, app, stage) : policy?.apps?.[app];
  return entry?.mode === "refuse" ? { mode: "refuse", since: entry.since, set_by: entry.set_by } : { mode: "warn", since: entry?.since ?? null, set_by: entry?.set_by ?? null };
}

export const approvedRunnerDigests = (policy) => (policy?.approvedRunners ?? []).map((runner) => runner.sha256);

/** Write the policy atomically, apps and runners in a deterministic order. */
function writePolicy(file, policy) {
  const sorted = {
    ...policy,
    apps: Object.fromEntries(Object.entries(policy.apps).sort(([a], [b]) => a.localeCompare(b))),
    approvedRunners: [...policy.approvedRunners].sort((a, b) => a.sha256.localeCompare(b.sha256) || (a.platform ?? "").localeCompare(b.platform ?? "")),
  };
  mkdirSync(path.dirname(file), { recursive: true });
  const temp = `${file}.${process.pid}.tmp`;
  writeFileSync(temp, `${JSON.stringify(sorted, null, 2)}\n`);
  renameSync(temp, file);
}

/** Refuse a result that loosens `current` or is not a valid policy; the file is left untouched. */
function assertAllowed(current, next) {
  const problems = [...checkPolicyTransition(current, next), ...validateAdmissionPolicy(next)];
  if (problems.length) throw new QaPolicyError(`QA admission policy change refused: ${problems.join("; ")}`);
}

/**
 * Record an app's mode. With a `stage` only that stage's entry is written (the app-level mode and the other stage are
 * kept); without one the app-level entry is written and any `stages` are removed, so one mode governs both stages.
 * A refuse -> warn move at any stage is refused unless `setBy` is the owner. Writes atomically.
 * The file is read without the git baseline here (the transition rule is applied directly to what is on disk).
 * Returns the entry written.
 */
export function setAdmissionMode({ app, mode, setBy, stage, dir = here, now = () => new Date() }) {
  if (!app || !String(app).trim()) throw new QaPolicyError("app is required");
  if (!ADMISSION_MODES.includes(mode)) throw new QaPolicyError("mode must be warn or refuse");
  if (!setBy || !String(setBy).trim()) throw new QaPolicyError("set_by is required");
  if (stage !== undefined && !ADMISSION_STAGES.includes(stage)) throw new QaPolicyError(`stage must be ${ADMISSION_STAGES.join(" or ")}`);
  const file = path.join(dir, QA_ADMISSION_POLICY_FILE);
  const current = parsePolicyText(readFileSync(file, "utf8"), QA_ADMISSION_POLICY_FILE);
  const entry = { mode, since: now().toISOString().slice(0, 10), set_by: String(setBy) };
  let written;
  if (stage) {
    const stages = { ...current.apps[app]?.stages, [stage]: entry };
    written = { ...current.apps[app], stages: Object.fromEntries(ADMISSION_STAGES.filter((name) => stages[name]).map((name) => [name, stages[name]])) };
  } else written = entry;
  const next = { ...current, apps: { ...current.apps, [app]: written } };
  assertAllowed(current, next);
  writePolicy(file, next);
  return entry;
}

function readRunnerBytes(file) {
  try { return readFileSync(file); } catch (error) { throw new QaPolicyError(`runner file is unreadable (${file}): ${error.message}`); }
}

/**
 * Approve a rightkit-qa runner build for a platform: only the owner may (it loosens admission), the file must report
 * `rightkit-qa <version>` from `--version`, and that version must be the published rightkit-qa version in
 * rightkit-versions.json (`registry.cargo`). Records { sha256, version, platform, set_by, since, note? } and returns it;
 * an entry with the same sha256 and platform that is already present is returned unchanged. Writes atomically.
 */
export function approveRunner({
  runner, version, platform, setBy, note, dir = here, now = () => new Date(), spawn = spawnSync, versionsFile = path.join(here, "rightkit-versions.json"),
}) {
  if (setBy !== POLICY_OWNER) throw new QaPolicyError(`approving a runner loosens QA admission; only ${POLICY_OWNER} may approve one (set_by ${JSON.stringify(setBy ?? null)})`);
  if (!RUNNER_PLATFORMS.includes(platform)) throw new QaPolicyError(`platform must be ${RUNNER_PLATFORMS.join(" or ")}`);
  if (!runner || !String(runner).trim()) throw new QaPolicyError("runner is required");
  if (!nonEmpty(version)) throw new QaPolicyError("version is required");
  if (note !== undefined && typeof note !== "string") throw new QaPolicyError("note must be a string");
  let published;
  try { published = JSON.parse(readFileSync(versionsFile, "utf8")).registry?.cargo?.["rightkit-qa"]; }
  catch (error) { throw new QaPolicyError(`the published rightkit-qa version cannot be read (${versionsFile}): ${error.message}`); }
  if (published !== version) throw new QaPolicyError(`rightkit-qa ${version} is not the published version (rightkit-versions.json registry.cargo has ${published ?? "no rightkit-qa"}); only the published runner can be approved`);
  // The file that is digested is the file that is run: an absolute path, never a PATH lookup, and re-digested after.
  const file = path.resolve(String(runner));
  const sha256 = sha256Of(readRunnerBytes(file));
  const run = spawn(file, ["--version"], { encoding: "utf8", windowsHide: true });
  const output = String(run?.stdout ?? "").trim();
  if (run?.status !== 0 || output !== `rightkit-qa ${version}`) {
    throw new QaPolicyError(`${file} --version reported ${JSON.stringify(output.slice(0, 80))} (exit ${run?.status ?? run?.error?.message ?? "none"}), expected "rightkit-qa ${version}"`);
  }
  if (sha256Of(readRunnerBytes(file)) !== sha256) throw new QaPolicyError(`${file} changed while its version was being checked; nothing was approved`);
  const policyFile = path.join(dir, QA_ADMISSION_POLICY_FILE);
  const current = parsePolicyText(readFileSync(policyFile, "utf8"), QA_ADMISSION_POLICY_FILE);
  const present = current.approvedRunners.find((item) => item.sha256 === sha256 && (item.platform ?? null) === platform);
  if (present) return present;
  const entry = { sha256, version, platform, set_by: setBy, since: now().toISOString().slice(0, 10), ...(note ? { note } : {}) };
  const next = { ...current, approvedRunners: [...current.approvedRunners, entry] };
  assertAllowed(current, next);
  writePolicy(policyFile, next);
  return entry;
}

/**
 * `node release-qa-policy.mjs show | set ... | approve-runner ...` (also `right-release qa policy ...`). The options
 * are test seams: `dir` (the policy directory), `env`, `now`, `spawn`, `versionsFile`, and `program` (the usage name).
 */
export function main(args, {
  stdout = (text) => console.log(text), stderr = (text) => console.error(text), dir, env, now, spawn, versionsFile, program = "release-qa-policy.mjs",
} = {}) {
  const value = (flag) => { const index = args.indexOf(flag); return index >= 0 ? args[index + 1] : undefined; };
  try {
    if (args[0] === "show") { stdout(JSON.stringify(loadAdmissionPolicy({ dir, env }).policy, null, 2)); return 0; }
    if (args[0] === "set") {
      // A `--stage` with no value must not fall through to the app-level write, which removes the stages.
      const stage = args.includes("--stage") ? value("--stage") ?? "" : undefined;
      const entry = setAdmissionMode({ app: value("--app"), mode: value("--mode"), setBy: value("--set-by"), stage, dir, now });
      stdout(`qa admission: ${value("--app")}${stage ? ` (${stage} stage)` : ""} -> ${entry.mode} (since ${entry.since}, set_by ${entry.set_by})`);
      return 0;
    }
    if (args[0] === "approve-runner") {
      const entry = approveRunner({ runner: value("--runner"), version: value("--version"), platform: value("--platform"), setBy: value("--set-by"), note: value("--note"), dir, now, spawn, versionsFile });
      stdout(`qa runner approved: ${entry.sha256} rightkit-qa ${entry.version} (${entry.platform ?? "any platform"}, since ${entry.since}, set_by ${entry.set_by})`);
      return 0;
    }
    stderr(`usage: ${program} show\n       ${program} set --app <app> --mode warn|refuse --set-by <name> [--stage source|installed]\n       ${program} approve-runner --runner <file> --version <v> --platform mac|win --set-by <name> [--note <text>]`);
    return 2;
  } catch (error) {
    stderr(`release-qa-policy: ${error.message}`);
    return 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) process.exit(main(process.argv.slice(2)));
