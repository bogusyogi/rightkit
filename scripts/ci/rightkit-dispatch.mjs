#!/usr/bin/env node
// rightkit-dispatch: the broker-admitted entry for hosted qualification (Adrian, 2026-10-22: every build enters
// through the RustRight broker; deterministic checks first; per-unit qualification on GitHub; direct publish).
// Registered as the `rightkit-dispatch` adapter (docs/contracts/attested-runs.md §2): the broker pins this file by
// digest, clears the environment (no token is forwarded; gh resolves its own host and keyring configuration under the
// broker-injected HOME), stamps RIGHTKIT_JOB_ID and writes the receipt. This script proves the commit, dispatches one workflow run bound to that request, and
// prints one JSON record line with the hosted run id. It compiles nothing, waits for nothing after discovery, and
// never prints an environment value.
//
//   rightkit-dispatch --probe
//   rightkit-dispatch --workflow ci --sha <40 hex>
//   rightkit-dispatch --workflow publish --registry crates|npm --unit <one name> --sha <40 hex>
//                     [--allow-host-exclusion <s>] [--allow-no-test <s>] [--allow-ignored <s>] [--allow-missing-assets <s>]
//
// Exit 0: dispatched and discovered. Exit 2: refused (usage, main moved, no broker request id, run not found). Exit 3: gh failed.
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
    else if (arg.startsWith("--") && ALLOW_FLAGS.includes(arg.slice(2))) opts.allow[arg.slice(2).replaceAll("-", "_")] = take();
    else throw new Error(`unknown argument ${arg}`);
  }
  if (opts.probe) return opts;
  if (typeof opts.workflow !== "string" || !Object.hasOwn(WORKFLOWS, opts.workflow)) throw new Error("--workflow must be ci or publish");
  if (!/^[0-9a-f]{40}$/.test(opts.sha ?? "")) throw new Error("--sha must be the full 40-hex commit this request was admitted for");
  if (opts.workflow === "publish") {
    if (!["crates", "npm"].includes(opts.registry)) throw new Error("--registry must be crates or npm");
    if (!/^[@a-z0-9][a-z0-9._/-]*$/i.test(opts.unit ?? "") || /\s/.test(opts.unit)) throw new Error("--unit names exactly one crate or package");
  } else if (opts.registry || opts.unit || Object.keys(opts.allow).length) throw new Error("--workflow ci takes only --sha");
  return opts;
}

/** The workflow_dispatch inputs for this request: correlation and the admitted commit are always present. */
export function dispatchInputs(opts, requestId) {
  const inputs = { request_id: requestId, expected_sha: opts.sha };
  if (opts.workflow === "publish") {
    inputs.registry = opts.registry;
    inputs.units = opts.unit;
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

// Broker pins this adapter. Pin its admission implementation too: importing mutable helpers would defeat that pin.
const PREFLIGHT_SHA256 = "875e20177064209879b0a2a438381024c5d01809fa20b843d64dcfc5e6781f9e";
export async function requirePreflight(opts) {
  if (opts.workflow !== "publish" || opts.registry !== "crates") throw new Error("only scoped crates publication has local admission; general CI/npm dispatch is not admitted");
  // The broker runs the digest-pinned copy of this file from its policy directory, so the repository is the adapter's
  // working directory (the request's workspace), never a path relative to this file.
  const root = process.cwd();
  const helper = path.join(root, "scripts", "ci", "rk-preflight.mjs");
  if (!fs.existsSync(helper) || !fs.existsSync(path.join(root, ".git"))) throw new Error(`working directory ${root} is not the RightKit checkout`);
  if (crypto.createHash("sha256").update(fs.readFileSync(helper)).digest("hex") !== PREFLIGHT_SHA256) throw new Error("preflight implementation digest changed; repin adapter with its reviewed helper");
  const { receiptRefusal, exceptionAllowsets, crateRows, verifyAttestation, DEFAULT_OUT } = await import(pathToFileURL(helper));
  const git = (...args) => {
    const r = spawnSync("git", args, { cwd: root, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
    if (r.status !== 0) throw new Error("cannot prove committed admission inputs");
    return r.stdout.trim();
  };
  if (git("rev-parse", "HEAD") !== opts.sha || git("status", "--porcelain", "--untracked-files=no")) throw new Error("checkout changed since local admission");
  const config = JSON.parse(git("show", `${opts.sha}:ci/gate.config.json`));
  const pipeline = JSON.parse(git("show", `${opts.sha}:ci/rk-pipeline.json`));
  const manifest = git("show", `${opts.sha}:crates/${opts.unit}/Cargo.toml`);
  const version = manifest.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const receipt = JSON.parse(fs.readFileSync(path.join(DEFAULT_OUT, opts.sha, `${opts.unit}.json`), "utf8"));
  const reason = receiptRefusal(receipt, { ...opts, config, version });
  if (reason) throw new Error(reason);
  // The receipt is writable JSON; the broker's signed provenance is not. Ask the broker about every row again, now,
  // and verify each envelope's signature against this host's enrolled key from the committed pipeline config.
  for (const [i, row] of crateRows(opts.unit, config).entries()) {
    const got = receipt.rows[i];
    const check = verifyAttestation(got.attestation?.requestId, { nonce: got.attestation?.nonce, argv: row.args, pins: pipeline.attestation?.[process.platform] });
    if (!check.verified) throw new Error(`row ${row.id}: broker provenance not verified (${check.reason})`);
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
  const requestId = env.RIGHTKIT_JOB_ID;
  if (!requestId) {
    err("rightkit-dispatch: no RIGHTKIT_JOB_ID; this runs only as the broker's rightkit-dispatch adapter");
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
  catch (error) { err(`rightkit-dispatch: preflight refused: ${error.message}`); return 2; }
  const inputs = dispatchInputs(opts, requestId);
  const fields = ["-f", "ref=main"];
  for (const [key, value] of Object.entries(inputs)) fields.push("-f", `inputs[${key}]=${value}`);
  const posted = gh(["api", "-X", "POST", `repos/${REPO}/actions/workflows/${WORKFLOWS[opts.workflow]}/dispatches`, ...fields], env);
  if (!posted.ok) {
    err(`rightkit-dispatch: GitHub refused the ${WORKFLOWS[opts.workflow]} dispatch`);
    return 3;
  }
  const deadline = now() + DISCOVERY_MS;
  let matches = [];
  for (;;) {
    const listed = gh(["api", `repos/${REPO}/actions/runs?event=workflow_dispatch&head_sha=${opts.sha}&per_page=30`], env);
    if (listed.ok) {
      try {
        matches = matchingRuns(JSON.parse(listed.stdout).workflow_runs ?? [], { workflow: opts.workflow, sha: opts.sha, requestId });
      } catch {
        matches = [];
      }
    }
    if (matches.length > 0 || now() >= deadline) break;
    await sleep(POLL_MS);
  }
  if (matches.length > 1) {
    err(`rightkit-dispatch: ambiguous, ${matches.length} ${WORKFLOWS[opts.workflow]} runs for ${opts.sha.slice(0, 12)} carry the token ${requestId}; nothing is reported as this request's run`);
    return 2;
  }
  if (matches.length === 0) {
    err(`rightkit-dispatch: dispatched but no ${WORKFLOWS[opts.workflow]} run for ${opts.sha.slice(0, 12)} named ${requestId} appeared within ${DISCOVERY_MS / 1000}s`);
    return 2;
  }
  const run = matches[0];
  out(JSON.stringify({ repo: REPO, workflow: WORKFLOWS[opts.workflow], sha: opts.sha, unit: opts.unit ?? null, registry: opts.registry ?? null, allowsets: opts.allow, requestId, runId: run.id, runAttempt: run.run_attempt, runUrl: run.html_url ?? null, dispatchedAt: new Date(now()).toISOString() }));
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(await main(process.argv.slice(2)));
}
