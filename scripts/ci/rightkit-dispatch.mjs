#!/usr/bin/env node
// rightkit-dispatch: the broker-admitted entry for hosted qualification (Adrian, 2026-10-22: every build enters
// through the RustRight broker; deterministic checks first; per-unit qualification on GitHub; direct publish).
// Registered as the `rightkit-dispatch` adapter (docs/contracts/attested-runs.md §2): the broker pins this file by
// digest, clears the environment (no token is forwarded; gh resolves its own host and keyring configuration under the
// broker-injected HOME), stamps RUSTRIGHT_JOB_ID and writes the receipt. This script proves the commit, dispatches one workflow run bound to that request, and
// prints one JSON record line with the hosted run id. It compiles nothing, waits for nothing after discovery, and
// never prints an environment value.
//
//   rightkit-dispatch --probe
//   rightkit-dispatch --workflow ci --sha <40 hex>
//   rightkit-dispatch --workflow publish --registry crates|npm --unit <one name> --sha <40 hex> --qualification <file>
//                     [--allow-host-exclusion <s>] [--allow-no-test <s>] [--allow-ignored <s>] [--allow-missing-assets <s>]
//
// Every mode is covered: --probe sends nothing; --workflow ci is a parsed mode whose production admission is a TYPED refusal
// ([ci-not-admitted]: there is no local admission for broad CI dispatch), and when an admission is injected it takes the same claim path as publish;
// --workflow publish is the admitted route. Each dispatch binds the source commit, the commit's real git tree (read from GitHub and compared with the
// qualification receipt's tree BEFORE any claim or POST), the artifact digest, the allow-set policy and the route into one candidate identity.
//
// Exit 0: dispatched and discovered, or joined to the candidate's existing run. Exit 2: refused or pending (usage, main moved, tree mismatch, no broker
// request id, typed admission refusal, conflicting or pending claim, run not found or ambiguous). Exit 3: gh failed or the claim could not be proved.
// A pending outcome also prints one JSON line {"pending":true,...} (no runId): the dispatch may have been accepted and is NEVER re-sent.
//
// SELF-CONTAINED CLOSURE: the broker pins this one file by digest, so its executable closure is this file plus node built-ins. The shared claim code is
// inlined below (a static import of a sibling helper would be evaluated before any digest check could cover it); the admission helpers are digest-checked
// and only then imported (requirePreflight). scripts/ci/dispatch-claim.mjs is a non-pinned re-export shim of the exports below and is never imported here.
import { spawnSync } from "node:child_process";
import path from "node:path";
import fs from "node:fs";
import crypto from "node:crypto";
import { fileURLToPath, pathToFileURL } from "node:url";

export const REPO = "bogusyogi/rightkit";
export const WORKFLOWS = { ci: "ci.yml", publish: "publish.yml" };
const ALLOW_FLAGS = ["allow-host-exclusion", "allow-no-test", "allow-ignored", "allow-missing-assets"];
const DISCOVERY_MS = 60_000;
const POLL_MS = 3_000;

export function ghPath(platform = process.platform) {
  return platform === "win32" ? "C:\\Program Files\\GitHub CLI\\gh.exe" : "/opt/homebrew/bin/gh";
}

/** The child's environment: the broker-injected HOME (gh's own host/keyring configuration lives under it), temp
 * and system paths, no prompts, no update checks. No token is forwarded and no private config dir hides the host's gh setup. */
export const GH_TIMEOUT_MS = 60_000;
export const PROBE_TIMEOUT_MS = 30_000;
export function ghEnvironment(env = process.env) {
  const out = { GH_NO_UPDATE_NOTIFIER: "1", GH_PROMPT_DISABLED: "1" };
  for (const key of ["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TMPDIR", "TMP", "TEMP", "SYSTEMROOT", "SystemRoot", "PATH"]) if (env[key] !== undefined) out[key] = env[key];
  return out;
}

/** Runs `gh <args>` within GH_TIMEOUT_MS; returns { ok, stdout }. Nothing from the environment reaches the result. */
export function defaultGh(args, env = process.env) {
  const result = spawnSync(ghPath(), args, { encoding: "utf8", env: ghEnvironment(env), windowsHide: true, maxBuffer: 16 * 1024 * 1024, timeout: GH_TIMEOUT_MS });
  if (result.error) return { ok: false, stdout: "" };
  return { ok: result.status === 0, stdout: result.stdout ?? "" };
}

/** The probe: `gh api user --silent` with every stream discarded, within PROBE_TIMEOUT_MS; only ok/not ok comes back. */
export function defaultProbeGh(env = process.env) {
  const result = spawnSync(ghPath(), ["api", "user", "--silent"], { stdio: ["ignore", "ignore", "ignore"], env: ghEnvironment(env), windowsHide: true, timeout: PROBE_TIMEOUT_MS });
  return { ok: !result.error && result.status === 0, stdout: "" };
}

export function parseArgs(argv) {
  const opts = { allow: {} };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const take = () => {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("--")) throw new Error(`${arg} needs a value`);
      i += 1;
      return value;
    };
    if (arg === "--probe") opts.probe = true;
    else if (arg === "--workflow") opts.workflow = take();
    else if (arg === "--registry") opts.registry = take();
    else if (arg === "--unit") opts.unit = take();
    else if (arg === "--sha") opts.sha = take();
    else if (arg === "--qualification") opts.qualification = take();
    else if (arg.startsWith("--") && ALLOW_FLAGS.includes(arg.slice(2))) opts.allow[arg.slice(2).replaceAll("-", "_")] = take();
    else throw new Error(`unknown argument ${arg}`);
  }
  if (opts.probe) return opts;
  if (typeof opts.workflow !== "string" || !Object.hasOwn(WORKFLOWS, opts.workflow)) throw new Error("--workflow must be ci or publish");
  if (!/^[0-9a-f]{40}$/.test(opts.sha ?? "")) throw new Error("--sha must be the full 40-hex commit this request was admitted for");
  if (opts.workflow === "publish") {
    if (!["crates", "npm"].includes(opts.registry)) throw new Error("--registry must be crates or npm");
    if (!/^[@a-z0-9][a-z0-9._/-]*$/i.test(opts.unit ?? "") || /\s/.test(opts.unit)) throw new Error("--unit names exactly one crate or package");
    if (typeof opts.qualification !== "string" || !opts.qualification) throw new Error("--qualification <file> is required: the rightkit-gate unit qualification receipt for this commit and unit");
  } else if (opts.registry || opts.unit || opts.qualification || Object.keys(opts.allow).length) throw new Error("--workflow ci takes only --sha");
  return opts;
}

/** The workflow_dispatch inputs for this request: correlation and the admitted commit are always present. */
export const QUALIFICATION_SCHEMA = "rightkit.unit-qualification/1";
/** The gate's qualification receipt, re-checked here (this adapter is the pinned, broker-run copy): commit, unit, stages, one sha. */
export function readQualification(file, { sha, unit, registry, readFile = (f) => fs.readFileSync(f, "utf8") }) {
  let q;
  try { q = JSON.parse(readFile(file)); } catch (error) { throw new Error(`cannot read the qualification receipt: ${error.message}`); }
  const kind = registry === "npm" ? "npm" : "cargo";
  if (q?.schema !== QUALIFICATION_SCHEMA) throw new Error(`qualification receipt schema is ${JSON.stringify(q?.schema)}, not ${QUALIFICATION_SCHEMA}`);
  if (q.commit !== sha) throw new Error(`qualification receipt is for ${String(q.commit).slice(0, 12)}, not the admitted ${sha.slice(0, 12)}`);
  if (q.unit?.kind !== kind || q.unit?.name !== unit) throw new Error(`qualification receipt covers ${q.unit?.kind}:${q.unit?.name}, not ${kind}:${unit}`);
  if (!/^[0-9a-f]{40}$/.test(q.tree ?? "") || !/^\d+\.\d+\.\d+/.test(q.unit?.version ?? "")) throw new Error("qualification receipt lacks a tree or a version");
  for (const stage of ["check", "compile", "package", "qualify"]) if (q.stages?.[stage]?.ok !== true) throw new Error(`qualification receipt: stage ${stage} is not ok`);
  for (const stage of ["compile", "package"]) if (!Array.isArray(q.stages[stage].receipts) || q.stages[stage].receipts.length < 1) throw new Error(`qualification receipt: stage ${stage} carries no signed broker receipts`);
  if (!/^[0-9a-f]{64}$/.test(q.artifactSha256 ?? "") || q.stages.qualify.artifactSha256 !== q.artifactSha256 || q.stages.package.artifactSha256 !== q.artifactSha256) throw new Error("qualification receipt does not bind one artifact sha256 across package and qualify");
  // What the workflow receives: the facts it verifies, nothing host-specific (no paths).
  return { schema: q.schema, commit: q.commit, tree: q.tree, runtime: q.runtime ?? null, executor: q.executor ?? null, unit: { kind: q.unit.kind, name: q.unit.name, version: q.unit.version },
    // The compile and package stages carry the broker's signed job receipts the gate collected; the hosted admission (publish-admission.mjs)
    // is what verifies them against the committed pins. They are forwarded as written, never trimmed or re-signed here.
    stages: Object.fromEntries(["check", "compile", "package", "qualify"].map((s) => [s, { ok: true, ...(s === "compile" || s === "package" ? { receipts: q.stages[s].receipts ?? [] } : {}) }])), artifactSha256: q.artifactSha256 };
}

export function dispatchInputs(opts, requestId, qualification = null) {
  const inputs = { request_id: requestId, expected_sha: opts.sha };
  if (opts.workflow === "publish") {
    inputs.registry = opts.registry;
    inputs.units = opts.unit;
    if (!qualification) throw new Error("a publish dispatch carries the qualification receipt");
    inputs.qualification = JSON.stringify(qualification);
    // workflow_dispatch refuses inputs over 65535 characters in all; say so here rather than as an opaque 422 after the broker job.
    if (JSON.stringify(inputs).length > 65_000) throw new Error("the qualification receipt with its broker receipts exceeds the workflow dispatch input limit");
    for (const key of ["allow_host_exclusion", "allow_no_test", "allow_ignored", "allow_missing_assets"]) inputs[key] = opts.allow[key] ?? "";
  }
  return inputs;
}

/** The runs this request could have started: same workflow file, same commit, workflow_dispatch, and the request id
 * as one exact whitespace-delimited token of the run-name (never a substring of another token). */
export function matchingRuns(runs, { workflow, sha, requestId }) {
  const file = `.github/workflows/${WORKFLOWS[workflow]}`;
  return runs.filter((run) => run.head_sha === sha && run.event === "workflow_dispatch" && String(run.path ?? "").endsWith(file) && String(run.display_title ?? run.name ?? "").split(/\s+/).includes(requestId));
}

/** The one run this request started, or null when none or more than one matches (ambiguity is a refusal, never a guess). */
export function matchRun(runs, query) {
  const matches = matchingRuns(runs, query);
  return matches.length === 1 ? matches[0] : null;
}

// ---- GB-47 / RA-04 / RA-05: the shared candidate claim at the authenticated dispatch boundary (inlined: see SELF-CONTAINED CLOSURE above) ------------
// Hosts keep local pipeline state, but nothing local stops the Mac and the Dell (or two requests) from both dispatching one candidate. The one place every
// origin goes through is GitHub, so ONE claim per registry/package/version (the publication conflict identity; GC-T21) lives there:
//   refs/rightkit-claims/<registry>/<unit hash>/<version>
// The ref is created atomically (creation fails if it exists) and points at a claim COMMIT whose message is the claim JSON; the candidate's digest is a
// VALUE inside it, never part of the key, so a different digest for the same version meets the same ref and is refused. Every later transition is a
// compare-and-swap: a new claim commit whose parent is the revision the writer read, moved onto the ref with force=false (GitHub accepts only a
// fast-forward), so a writer holding a stale revision is rejected. States:
//   reserved   one request owns the claim; NOTHING has been sent (a POST only ever follows a successful reserved -> sending swap);
//   sending    the send intent is persisted BEFORE the POST: from here the dispatch is sent-or-unknown, and absence of a run is never read as
//              "not sent": no request (not even the same request id) sends again; the run is discovered by the effect token (the request id);
//   dispatched the run id is recorded; later requests verify it through the Actions API and join it.
// A reserved claim whose owner died before sending is reclaimed by a swap only after CLAIM_STALE_MS (or at once by a retry of the same request id); the old
// owner's own sending swap then fails, so it cannot send. Unreadable or corrupt claims are conflicts (fail closed); an unreadable namespace is unavailable.
export const CLAIM_SCHEMA = "rightkit.dispatch-claim/2";
export const CLAIM_STALE_MS = 10 * 60_000;
const RUNS_PER_PAGE = 100;
const RUNS_MAX_PAGES = 10;
const CLAIM_STATES = ["reserved", "sending", "dispatched"];
const sha256Hex = (text) => crypto.createHash("sha256").update(text).digest("hex");
const sortKeys = (v) => (Array.isArray(v) ? v.map(sortKeys) : v && typeof v === "object" ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, sortKeys(v[k])])) : v);
const canonical = (value) => JSON.stringify(sortKeys(value));
const parseJson = (text) => { try { return JSON.parse(text); } catch { return null; } };
const isoAt = (ms) => new Date(ms).toISOString();
const refused = (status, code, message, extra = {}) => ({ status, code, message, ...extra });

/** What a candidate IS: any difference is a different candidate (and, for one version, a conflict). `tree` is the commit's real git tree proved from GitHub. */
export function claimIdentity({ repo, workflow, sha, tree, registry, unit, version, artifactSha256 = null, allow = {} }) {
  if (!/^[0-9a-f]{40}$/.test(tree ?? "")) throw new Error("a claim identity binds the proved git tree of the source commit");
  return { schema: CLAIM_SCHEMA, repo, workflow, sha, tree, registry, unit, version, artifactSha256, policy: sha256Hex(canonical(allow)), route: "broker" };
}
export const claimDigest = (identity) => sha256Hex(canonical(identity));
/** The ONE conflict key of a registry/package/version (the unit is hashed: a scoped npm name is not a safe ref segment). No candidate digest is in it. */
export function claimKey({ registry, unit, version }) {
  let safe = String(version).replace(/[^0-9A-Za-z._+-]/g, "_");
  if (/\.\.|\.lock$|\.$|^\./.test(safe) || !safe) safe = `v-${sha256Hex(String(version)).slice(0, 16)}`;
  return `rightkit-claims/${registry}/${sha256Hex(String(unit)).slice(0, 16)}/${safe}`;
}

function claimProblem(claim, key) {
  if (!claim || typeof claim !== "object" || claim.schema !== CLAIM_SCHEMA) return "it is not a dispatch claim of this schema";
  if (claim.key !== key || !claim.identity || typeof claim.identity !== "object" || claimKey(claim.identity) !== key) return "it names another conflict key";
  if (claimDigest(claim.identity) !== claim.identityDigest) return "its identity does not match its digest";
  if (typeof claim.requestId !== "string" || !claim.requestId || typeof claim.attempt !== "string" || !claim.attempt) return "it has no owner";
  if (!CLAIM_STATES.includes(claim.state) || !Number.isInteger(claim.seq) || !Number.isFinite(Date.parse(claim.at))) return "its state is malformed";
  if (claim.state === "dispatched" && !claim.run?.id) return "it is dispatched without a run";
  return null;
}

/** Reads the claim of one conflict key: absent | present { revision, claim } | corrupt | legacy | unavailable. Absent is only ever an explicit empty listing. */
export function readClaim({ gh, env, repo, key }) {
  const listed = gh(["api", `repos/${repo}/git/matching-refs/${key}?per_page=100`], env);
  const refs = listed.ok ? parseJson(listed.stdout) : null;
  if (!Array.isArray(refs) || refs.length >= 100) return { status: "unavailable" };
  const exact = `refs/${key}`;
  if (refs.some((r) => String(r?.ref).startsWith(`${exact}/`))) return { status: "legacy" };
  const mine = refs.find((r) => r?.ref === exact);
  if (!mine) return { status: "absent" };
  const revision = mine.object?.sha;
  if (mine.object?.type !== "commit" || !/^[0-9a-f]{40}$/.test(revision ?? "")) return { status: "corrupt", reason: "the claim ref does not point at a claim commit" };
  const got = gh(["api", `repos/${repo}/git/commits/${revision}`], env);
  const body = got.ok ? parseJson(got.stdout) : null;
  if (!body || typeof body !== "object") return { status: "unavailable" };
  const claim = parseJson(body.message ?? "");
  const problem = claimProblem(claim, key);
  if (problem) return { status: "corrupt", revision, reason: problem };
  return { status: "present", revision, claim };
}

function writeClaimCommit({ gh, env, repo, tree, parent, claim, now }) {
  const date = isoAt(now());
  const args = ["api", "-X", "POST", `repos/${repo}/git/commits`, "-f", `message=${JSON.stringify(claim)}`, "-f", `tree=${tree}`];
  if (parent) args.push("-f", `parents[]=${parent}`);
  for (const who of ["author", "committer"]) args.push("-f", `${who}[name]=rightkit-dispatch`, "-f", `${who}[email]=noreply@rightkit.invalid`, "-f", `${who}[date]=${date}`);
  const made = gh(args, env);
  const sha = made.ok ? parseJson(made.stdout)?.sha : null;
  return /^[0-9a-f]{40}$/.test(sha ?? "") ? sha : null;
}

/** Compare-and-swap: the new claim commit descends from `revision`; force=false means GitHub moves the ref only if `revision` is still its value.
 * A failed move is read back once: a lost acknowledgement of a move that did happen is recognised by the ref being exactly our new commit. */
function advanceClaim({ gh, env, repo, key, revision, tree, claim, now }) {
  const commit = writeClaimCommit({ gh, env, repo, tree, parent: revision, claim, now });
  if (!commit) return null;
  if (gh(["api", "-X", "PATCH", `repos/${repo}/git/refs/${key}`, "-f", `sha=${commit}`, "-F", "force=false"], env).ok) return commit;
  const back = readClaim({ gh, env, repo, key });
  return back.status === "present" && back.revision === commit ? commit : null;
}

/** Pages through the workflow_dispatch runs of one commit. complete is true only when every page was read; an error or a page cap is never "no runs". */
export function listRuns({ gh, env, repo, sha }) {
  const runs = [];
  for (let page = 1; page <= RUNS_MAX_PAGES; page += 1) {
    const listed = gh(["api", `repos/${repo}/actions/runs?event=workflow_dispatch&head_sha=${sha}&per_page=${RUNS_PER_PAGE}&page=${page}`], env);
    const body = listed.ok ? parseJson(listed.stdout) : null;
    if (!body || !Array.isArray(body.workflow_runs)) return { ok: false, complete: false, runs };
    runs.push(...body.workflow_runs);
    if (body.workflow_runs.length < RUNS_PER_PAGE || (Number.isInteger(body.total_count) && runs.length >= body.total_count)) return { ok: true, complete: true, runs };
  }
  return { ok: true, complete: false, runs };
}

/**
 * Decides before any send. Returns:
 *   { status: "reserved", ref, beginSend(), record(run) }  this request owns the claim. beginSend() persists the send intent by swap and returns false
 *                                                        when it cannot (then NOTHING may be sent); only after it returns true may the POST happen;
 *                                                        record(run) swaps the run onto the claim (a failure never enables a resend);
 *   { status: "joined", run, claim, adopt? }              an existing run of this very candidate: send nothing (adopt() records it, best effort);
 *   { status: "conflict" | "pending" | "unavailable", code, message }   refuse. pending = sent-or-unknown or owned by a live request.
 * `discover(token)` returns { ok, complete, runs } for the runs carrying the effect token; `verifyRun(id, token)` returns { status: "confirmed", run } or
 * { status: "unconfirmed" }.
 */
export function reserveClaim({ gh, env, repo, identity, requestId, discover, verifyRun, now = Date.now, staleMs = CLAIM_STALE_MS, attempt = crypto.randomUUID() }) {
  const digest = claimDigest(identity);
  const key = claimKey(identity);
  const ref = `refs/${key}`;
  const label = `${identity.unit}@${identity.version}`;
  const own = (start) => {
    let state = start;
    const step = (next) => {
      const revision = advanceClaim({ gh, env, repo, key, revision: state.revision, tree: identity.tree, claim: next, now });
      if (!revision) return false;
      state = { revision, claim: next };
      return true;
    };
    return {
      status: "reserved", ref, token: requestId,
      beginSend: () => step({ ...state.claim, state: "sending", seq: state.claim.seq + 1, at: isoAt(now()) }),
      record: (run) => step({ ...state.claim, state: "dispatched", seq: state.claim.seq + 1, run: { id: run.id, attempt: run.run_attempt ?? null, url: run.html_url ?? null }, at: isoAt(now()) }),
    };
  };
  for (let pass = 0; pass < 3; pass += 1) {
    const read = readClaim({ gh, env, repo, key });
    if (read.status === "unavailable") return refused("unavailable", "claim-unavailable", "the shared claim namespace cannot be read, so a candidate claim cannot be proved; nothing was sent");
    if (read.status === "legacy") return refused("conflict", "claim-legacy", `${label} has claims in the retired per-digest layout; reconcile them before any send`);
    if (read.status === "corrupt") return refused("conflict", "claim-unreadable", `the claim for ${label} cannot be read (${read.reason}); it is treated as a conflicting claim and nothing is sent`);
    if (read.status === "absent") {
      const claim = { schema: CLAIM_SCHEMA, key, identity, identityDigest: digest, requestId, attempt, state: "reserved", seq: 0, at: isoAt(now()), effect: { kind: "workflow_dispatch", workflow: identity.workflow, token: requestId } };
      const commit = writeClaimCommit({ gh, env, repo, tree: identity.tree, parent: null, claim, now });
      if (!commit) return refused("unavailable", "claim-unavailable", "the candidate claim object could not be written; nothing was sent");
      // Creation fails when another origin created the ref first; the next pass reads theirs. A lost acknowledgement of our own creation reads back as ours.
      if (gh(["api", "-X", "POST", `repos/${repo}/git/refs`, "-f", `ref=${ref}`, "-f", `sha=${commit}`], env).ok) return own({ revision: commit, claim });
      continue;
    }
    const { claim, revision } = read;
    const token = claim.requestId;
    if (claim.identityDigest !== digest) return refused("conflict", "claim-conflict", `${label} is already claimed for different bytes, source or policy (${claim.identityDigest.slice(0, 12)}); this candidate (${digest.slice(0, 12)}) is refused`);
    const adopt = (run) => () => advanceClaim({ gh, env, repo, key, revision, tree: identity.tree, claim: { ...claim, state: "dispatched", seq: claim.seq + 1, run: { id: run.id, attempt: run.run_attempt ?? null, url: run.html_url ?? null }, adoptedBy: { requestId, attempt }, at: isoAt(now()) }, now });
    if (claim.state === "dispatched") {
      const verified = verifyRun(claim.run.id, token);
      if (verified?.status !== "confirmed") return refused("pending", "claim-run-unconfirmed", `the claim for ${label} names run ${claim.run.id}, which the Actions API does not confirm; reconcile it before any send`, { token });
      return { status: "joined", run: verified.run, claim };
    }
    if (claim.state === "sending") {
      const found = discover(token);
      if (found.ok && found.complete && found.runs.length === 1) return { status: "joined", run: found.runs[0], claim, adopt: adopt(found.runs[0]) };
      if (found.ok && found.runs.length > 1) return refused("pending", "claim-ambiguous", `${found.runs.length} runs carry the claim's request token ${token}; ambiguous, nothing is sent`, { token });
      return refused("pending", "claim-sent-or-unknown", `${label} is claimed by request ${token} (send intent recorded ${claim.at}); a dispatch may already have been accepted and ${found.ok && found.complete ? "no run of it is visible yet" : "its run listing is unavailable or incomplete, so no run of it is visible yet"}: nothing is re-sent; reconcile that request's run before any further send`, { token });
    }
    // reserved: nothing has been sent by its owner (a send follows only a successful swap to "sending"), so a takeover cannot duplicate an effect.
    if (claim.attempt === attempt) return own({ revision, claim });
    const age = now() - Date.parse(claim.at);
    if (claim.requestId !== requestId && !(age >= staleMs)) return refused("pending", "claim-reserved-elsewhere", `${label} is reserved by request ${token} (at ${claim.at}) and not yet sent; wait for it or for ${Math.round(staleMs / 60_000)} minutes before reclaiming`, { token });
    const taken = { ...claim, requestId, attempt, seq: claim.seq + 1, at: isoAt(now()), takeover: { requestId: token, attempt: claim.attempt, at: claim.at }, effect: { kind: "workflow_dispatch", workflow: identity.workflow, token: requestId } };
    const next = advanceClaim({ gh, env, repo, key, revision, tree: identity.tree, claim: taken, now });
    if (next) return own({ revision: next, claim: taken });
  }
  return refused("unavailable", "claim-unavailable", "the candidate claim could not be created, reclaimed or read after three attempts; nothing was sent");
}

// Broker pins this adapter. Pin its admission implementation too: importing mutable helpers would defeat that pin.
const PREFLIGHT_SHA256 = "4985ce98802c106fd3b30c1d026adc79fb6f6d16bd8e631d9b68d62b613d836b";
const NPM_PREFLIGHT_SHA256 = "3e8fbfdcf2a42a0dfd1c50515a8ec781603d1774c64c7c7a0987cf4e25d5583f";
const NPM_SELECT_SHA256 = "7ca660e23cc584242f0fd7e973f7475b5142c9bb1249f80650a2662adf84423b";
// rk-npm-preflight.mjs imports this module (its version-pin plan-check); a pinned importer that trusts an unpinned module is not pinned.
const NPM_VERSION_PINS_SHA256 = "5f490eb13c11dab32bb57a0d4b95eea94564e75ec1d853af5dbc1093a215b7a8";
// Filled from reviewed bytes; validate the complete static admission import tree before import.
const ADMISSION_DEPENDENCIES = {
  "scripts/ci/rk-consumer-stage.mjs": "f111d02dbc77b917f4560585020746daee5ae4f6e2256a5865bd3bcbc95f5b8f",
  "scripts/ci/rk-routing.mjs": "6b3c6350a653c5284146c98dcd7df306c5ce8b96d5a5dd14c2e8f39c0dfb29c1",
  "scripts/ci/consumer-recipes.mjs": "919a1f28d6b005ea963d7e1f9958aebdb34c0ca2a9696ba3eecee475b426bcb8",
  "scripts/ci/workflow-yaml.mjs": "c9e6fe758036e9e246915b2205d520e7ffc8e9f0e0a1a4117c2affc96dd015f5",
  "packages/release/workflow-yaml.mjs": "13491a0cbebe60575f459be004333d4782c1dbe66fe18963c1b1eba0410f4ced",
  "packages/release/broker-client.mjs": "9037947b6b216e53573ead100eb61aec1c730db909885cb86d07de707d31c176",
};
export async function requirePreflight(opts) {
  if (opts.workflow !== "publish" || !["crates", "npm"].includes(opts.registry)) throw Object.assign(new Error("only scoped crates/npm publication has local admission; general CI dispatch is not admitted"), { code: "ci-not-admitted" });
  const npm = opts.registry === "npm";
  if (npm && !/^@rightkit\/[a-z][a-z0-9-]{0,63}$/.test(opts.unit ?? "")) throw new Error("npm admission requires exactly one @rightkit/* package");
  // The broker runs the digest-pinned copy of this file from its policy directory, so the repository is the adapter's
  // working directory (the request's workspace), never a path relative to this file.
  const root = process.cwd();
  const helper = path.join(root, "scripts", "ci", "rk-preflight.mjs");
  if (!fs.existsSync(helper) || !fs.existsSync(path.join(root, ".git"))) throw new Error(`working directory ${root} is not the RightKit checkout`);
  if (crypto.createHash("sha256").update(fs.readFileSync(helper)).digest("hex") !== PREFLIGHT_SHA256) throw new Error("preflight implementation digest changed; repin adapter with its reviewed helper");
  for (const [relative, expected] of Object.entries(ADMISSION_DEPENDENCIES)) {
    const file = path.join(root, relative);
    if (!fs.existsSync(file) || crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex") !== expected) throw new Error(`admission dependency digest changed: ${relative}`);
  }
  const npmHelper = path.join(root, "scripts", "ci", "rk-npm-preflight.mjs");
  if (npm) {
    for (const [file, expected] of [[npmHelper, NPM_PREFLIGHT_SHA256], [path.join(root, "scripts", "ci", "select.mjs"), NPM_SELECT_SHA256], [path.join(root, "scripts", "ci", "release-version-pins.mjs"), NPM_VERSION_PINS_SHA256]]) {
      if (!fs.existsSync(file) || crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex") !== expected) throw new Error("npm admission implementation digest changed; repin reviewed dependencies");
    }
  }
  const { receiptRefusal, exceptionAllowsets, crateRows, resolveRowWorkspace, verifyAttestation, recheckHostedAdmission, DEFAULT_OUT } = await import(pathToFileURL(helper));
  const git = (...args) => {
    const r = spawnSync("git", args, { cwd: root, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
    if (r.status !== 0) throw new Error("cannot prove committed admission inputs");
    return r.stdout.trim();
  };
  if (git("rev-parse", "HEAD") !== opts.sha || git("status", "--porcelain", "--untracked-files=no")) throw new Error("checkout changed since local admission");
  const config = JSON.parse(git("show", `${opts.sha}:ci/gate.config.json`));
  const pipeline = JSON.parse(git("show", `${opts.sha}:ci/rk-pipeline.json`));
  let version;
  let rows;
  let expect;
  let packageDir;
  if (npm) {
    // Import after digest checks. npm helper imports preflight; preflight never imports npm helper.
    const { packageRows, readPackages, adapterExpectation } = await import(pathToFileURL(npmHelper));
    const pkg = readPackages(root).get(opts.unit);
    if (!pkg) throw new Error(`${opts.unit} is not a workspace package`);
    const manifestPath = path.join(root, pkg.dir, "package.json");
    const committed = git("show", `${opts.sha}:${pkg.dir}/package.json`);
    if (fs.readFileSync(manifestPath, "utf8").trim() !== committed) throw new Error("npm package manifest differs from requested commit");
    const manifest = JSON.parse(committed);
    if (manifest.name !== opts.unit || typeof manifest.version !== "string" || !manifest.version || manifest.version !== pkg.version) throw new Error("npm package name/version differs from requested commit");
    version = manifest.version;
    packageDir = pkg.dir;
    rows = packageRows(opts.unit, pkg, { storeDir: pipeline.tools?.pnpm?.[process.platform]?.storeDir });
    expect = adapterExpectation(pipeline.attestation?.[process.platform]);
  } else {
    const manifest = git("show", `${opts.sha}:crates/${opts.unit}/Cargo.toml`);
    version = manifest.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
    rows = crateRows(opts.unit, config);
  }
  const receipt = JSON.parse(fs.readFileSync(path.join(DEFAULT_OUT, opts.sha, `${opts.unit.replaceAll("/", "__")}.json`), "utf8"));
  const reason = receiptRefusal(receipt, { ...opts, config, version, expectedRows: rows, packageDir, allowHostedCompile: !npm });
  if (reason) throw new Error(reason);
  const hostedCompile = !npm && receipt.mode === "hosted-compile";
  if (hostedCompile) {
    await recheckHostedAdmission({ receipt, root, sha: opts.sha, unit: opts.unit, rows, config, version });
    if (git("rev-parse", "HEAD") !== opts.sha || git("status", "--porcelain", "--untracked-files=no")) throw new Error("checkout changed during hosted admission");
  }
  // The receipt is writable JSON; the broker's signed provenance is not. Ask the broker about every row again, now,
  // and verify each envelope's signature against this host's enrolled key from the committed pipeline config.
  for (const [i, row] of (hostedCompile ? [] : rows).entries()) {
    const got = receipt.rows[i];
    const workspace = npm ? root : resolveRowWorkspace({ root, row, sha: opts.sha });
    const check = verifyAttestation(got.attestation?.requestId, { nonce: got.attestation?.nonce, argv: row.args, pins: pipeline.attestation?.[process.platform], expect, workspace });
    if (!check.verified) throw new Error(`row ${row.id}: broker provenance not verified (${check.reason})`);
  }
  // Registry-consumer stage (rk-consumer-stage.mjs): a crate dispatched from local rows must carry a passed local-source
  // fixture proof, and every brokered build or run in it is asked of the broker again, in its own check directory.
  if (!npm && !hostedCompile) {
    const stage = receipt.registryConsumer;
    if (!stage || stage.ok !== true || stage.unit !== opts.unit || stage.version !== version) throw new Error("registry consumer stage did not pass for this unit and version");
    const prefix = `.consumer-checks${path.sep}${opts.unit}-${opts.sha.slice(0, 12)}-`;
    if (typeof stage.checkDir !== "string" || !stage.checkDir.startsWith(prefix) || stage.checkDir.includes("..")) throw new Error("registry consumer check directory is not this unit's at this commit");
    const workspace = path.join(root, stage.checkDir);
    const brokered = [...(stage.build ?? []).map((b) => ({ label: b.argv, argv: b.argvList, attestation: b.attestation })), ...(stage.runs ?? []).filter((x) => x.via === "broker").map((x) => ({ label: `run ${x.id}`, argv: x.argvList, attestation: x.attestation }))];
    if (!(stage.build ?? []).length && (stage.runs ?? []).some((x) => x.status === "passed")) throw new Error("registry consumer runs passed without a brokered build");
    for (const item of brokered) {
      if (!Array.isArray(item.argv) || !item.argv.length) throw new Error(`registry consumer ${item.label}: no recorded argv to re-verify`);
      const check = verifyAttestation(item.attestation?.requestId, { nonce: item.attestation?.nonce, argv: item.argv, pins: pipeline.attestation?.[process.platform], workspace });
      if (!check.verified) throw new Error(`registry consumer ${item.label}: broker provenance not verified (${check.reason})`);
    }
    if ((stage.runs ?? []).some((x) => !["passed", "not-run-locally"].includes(x.status))) throw new Error("registry consumer stage recorded a failed run");
  }
  const policy = exceptionAllowsets(opts.unit, version, config, pipeline);
  for (const [key, value] of Object.entries(opts.allow)) {
    const supplied = value.split(/\s+/).filter(Boolean);
    if (supplied.some((unit) => !policy[key]?.includes(unit))) throw new Error(`unrecorded qualification exception ${key}`);
  }
}

export async function main(argv, deps = {}) {
  const gh = deps.gh ?? defaultGh;
  const env = deps.env ?? process.env;
  const sleep = deps.sleep ?? ((ms) => new Promise((resolve) => setTimeout(resolve, ms)));
  const now = deps.now ?? (() => Date.now());
  const out = deps.stdout ?? ((line) => process.stdout.write(`${line}\n`));
  const err = deps.stderr ?? ((line) => process.stderr.write(`${line}\n`));
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (error) {
    err(`rightkit-dispatch: ${error.message}`);
    return 2;
  }
  const requestId = env.RUSTRIGHT_JOB_ID;
  if (!requestId) {
    err("rightkit-dispatch: no RUSTRIGHT_JOB_ID; this runs only as the broker's rightkit-dispatch adapter");
    return 2;
  }
  if (opts.probe) {
    const authenticated = (deps.probeGh ?? defaultProbeGh)(env).ok;
    out(JSON.stringify({ authenticated, brokerJobId: requestId }));
    return authenticated ? 0 : 3;
  }
  const head = gh(["api", `repos/${REPO}/commits/main`, "--jq", ".sha"], env);
  if (!head.ok) {
    err("rightkit-dispatch: cannot read origin main through gh");
    return 3;
  }
  const mainSha = head.stdout.trim();
  if (mainSha !== opts.sha) {
    err(`rightkit-dispatch: refused, main is ${mainSha.slice(0, 12)} but this request was admitted for ${opts.sha.slice(0, 12)}`);
    return 2;
  }
  try { await (deps.admission ?? requirePreflight)(opts); }
  catch (error) { err(`rightkit-dispatch: preflight refused${error.code ? ` [${error.code}]` : ""}: ${error.message}`); return 2; }
  let qualification = null;
  if (opts.workflow === "publish") {
    try { qualification = (deps.readQualification ?? readQualification)(opts.qualification, { sha: opts.sha, unit: opts.unit, registry: opts.registry }); }
    catch (error) { err(`rightkit-dispatch: refused, ${error.message}`); return 2; }
  }
  // R3-X04: bind the source before any effect. The commit's real git tree is read from GitHub (the source the workflow will run) and must be the tree the
  // qualification receipt was made for; a mismatch is refused here, before any claim or POST, not left to the hosted admission.
  const treeRead = gh(["api", `repos/${REPO}/git/commits/${opts.sha}`], env);
  const commitBody = treeRead.ok ? parseJson(treeRead.stdout) : null;
  const tree = commitBody?.tree?.sha;
  if (!/^[0-9a-f]{40}$/.test(tree ?? "") || (commitBody.sha !== undefined && commitBody.sha !== opts.sha)) {
    err("rightkit-dispatch: cannot read the requested commit's tree through gh");
    return 3;
  }
  if (qualification && qualification.tree !== tree) {
    err(`rightkit-dispatch: refused [tree-mismatch]: the qualification receipt is for tree ${String(qualification.tree).slice(0, 12)}, but commit ${opts.sha.slice(0, 12)} has tree ${tree.slice(0, 12)}; nothing was claimed or sent`);
    return 2;
  }
  // GB-47 / RA-04 / RA-05: every mode (publish and ci) claims its candidate in the shared GitHub namespace before anything is sent: the same candidate joins
  // its existing run, a different candidate for the same registry/package/version is refused, the send intent is persisted before the POST, and a sent or
  // unknown dispatch is reconciled by its effect token, never re-sent.
  const identity = claimIdentity({ repo: REPO, workflow: WORKFLOWS[opts.workflow], sha: opts.sha, tree, registry: opts.registry ?? "ci", unit: opts.unit ?? "ci", version: qualification?.unit.version ?? opts.sha, artifactSha256: qualification?.artifactSha256 ?? null, allow: opts.allow });
  const discoverRuns = (token) => { const listed = listRuns({ gh, env, repo: REPO, sha: opts.sha }); return { ok: listed.ok, complete: listed.ok && listed.complete, runs: matchingRuns(listed.runs, { workflow: opts.workflow, sha: opts.sha, requestId: token }) }; };
  const verifyRun = (id, token) => {
    const got = gh(["api", `repos/${REPO}/actions/runs/${id}`], env);
    const r = got.ok ? parseJson(got.stdout) : null;
    return r && String(r.id) === String(id) && matchingRuns([r], { workflow: opts.workflow, sha: opts.sha, requestId: token }).length === 1 ? { status: "confirmed", run: r } : { status: "unconfirmed" };
  };
  const record = (extra) => JSON.stringify({ repo: REPO, workflow: WORKFLOWS[opts.workflow], sha: opts.sha, unit: opts.unit ?? null, registry: opts.registry ?? null, allowsets: opts.allow, ...extra });
  const claim = (deps.claim ?? reserveClaim)({ gh, env, repo: REPO, identity, requestId, now, discover: discoverRuns, verifyRun });
  if (claim.status === "joined") {
    claim.adopt?.();
    out(record({ requestId: claim.claim.requestId, runId: claim.run.id, runAttempt: claim.run.run_attempt, runUrl: claim.run.html_url ?? null, joined: true }));
    return 0;
  }
  if (claim.status !== "reserved" || typeof claim.beginSend !== "function") {
    const known = ["unavailable", "conflict", "pending"].includes(claim.status);
    const prefix = { unavailable: "claim unavailable: ", conflict: "refused, conflicting candidate: ", pending: "pending, claimed elsewhere: " }[claim.status] ?? "claim unavailable: ";
    err(`rightkit-dispatch: ${prefix}${claim.code ? `[${claim.code}] ` : ""}${known ? claim.message : "the claim step did not return a reservation; nothing was sent"}`);
    if (claim.status === "pending") out(record({ pending: true, code: claim.code ?? null, requestId: claim.token ?? null }));
    return claim.status === "conflict" || claim.status === "pending" ? 2 : 3;
  }
  // RA-05: persist the send intent BEFORE the POST. From here the dispatch is sent-or-unknown; if the intent cannot be persisted (the claim moved, or the
  // namespace is unreadable) nothing is sent.
  if (!claim.beginSend()) {
    err("rightkit-dispatch: claim unavailable: [claim-intent-unpersisted] the send intent could not be persisted on the claim (it moved or the namespace is unreadable); nothing was sent");
    return 3;
  }
  const inputs = dispatchInputs(opts, requestId, qualification);
  const fields = ["-f", "ref=main"];
  for (const [key, value] of Object.entries(inputs)) fields.push("-f", `inputs[${key}]=${value}`);
  const posted = gh(["api", "-X", "POST", `repos/${REPO}/actions/workflows/${WORKFLOWS[opts.workflow]}/dispatches`, ...fields], env);
  // A POST that did not confirm (error, timeout, dropped response) may still have been accepted: it is never re-sent and its absence of a run is not proof.
  // One listing is still taken, because a dropped response with a visible run is a success; a confirmed POST is polled for until DISCOVERY_MS.
  const deadline = now() + (posted.ok ? DISCOVERY_MS : 0);
  let found = { ok: false, complete: false, runs: [] };
  for (;;) {
    found = discoverRuns(requestId);
    if ((found.complete && found.runs.length > 0) || now() >= deadline) break;
    await sleep(POLL_MS);
  }
  if (found.complete && found.runs.length > 1) {
    err(`rightkit-dispatch: ambiguous, ${found.runs.length} ${WORKFLOWS[opts.workflow]} runs for ${opts.sha.slice(0, 12)} carry the token ${requestId}; nothing is reported as this request's run, and the claim stays pending`);
    return 2;
  }
  if (!(found.complete && found.runs.length === 1)) {
    out(record({ pending: true, code: "claim-sent-or-unknown", requestId, accepted: posted.ok, state: "sending" }));
    if (!posted.ok) {
      err(`rightkit-dispatch: GitHub did not confirm the ${WORKFLOWS[opts.workflow]} dispatch; it may still have been accepted, so the claim stays pending and nothing is re-sent`);
      return 3;
    }
    err(`rightkit-dispatch: pending, dispatched but no ${WORKFLOWS[opts.workflow]} run for ${opts.sha.slice(0, 12)} named ${requestId} is visible after ${DISCOVERY_MS / 1000}s${found.complete ? "" : " (the run listing is unavailable or incomplete)"}; the claim stays pending and nothing is re-sent`);
    return 2;
  }
  const run = found.runs[0];
  if (!claim.record(run)) err(`rightkit-dispatch: the run ${run.id} could not be written onto the claim; the claim stays at its send intent and a retry finds the run by this request's token`);
  out(record({ requestId, runId: run.id, runAttempt: run.run_attempt, runUrl: run.html_url ?? null, dispatchedAt: new Date(now()).toISOString() }));
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(await main(process.argv.slice(2)));
}
