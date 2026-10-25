#!/usr/bin/env node
// right-release qa: run RightKit's own QA runner (rightkit-qa) and write the sealed, MAC'd QA receipt.
//
//   right-release qa --stage source    --platform mac|win [--config right-release.config.mjs]
//   right-release qa --stage installed --platform mac|win --release <sealed-id> [--config right-release.config.mjs]
//   right-release qa audit [--app <dir>] [--json] [--since <YYYY-MM-DD>]   (release-qa-audit.mjs; exit 1 when an uploaded build lacks passing QA)
//   right-release qa policy show | set --app <app> --mode warn|refuse --set-by <name> [--stage source|installed]
//                         | approve-runner --runner <file> --version <v> --platform mac|win --set-by <name> [--note <text>]   (release-qa-policy.mjs)
//
// The release config declares the app's QA plan:
//   qa: { version: "1", config: "rightkit-qa.toml", runner?: "<path to rightkit-qa>",
//         args?: { source?: [..], installed?: [..] },                                   // {sealedDir} {releaseId} expand; installed also {installedApp} {installedDir} {qaRoot}
//         installed?: { installDir?: "<abs dir>" },                                       // opt in: mount the sealed DMG (mac) / run the sealed NSIS installer (win, needs RIGHT_RELEASE_QA_ALLOW_INSTALL=1 off CI), see release-qa-installed.mjs
//         journeys?: { manifest?: "journeys.toml", receipt?: "<path to a rightkit.journeys.receipt/1>", args?: { source?: [..], installed?: [..] } },
//         checks: { "smoke.launch": ["app.launch"], journey: ["journey.*"] } }          // floor id -> evidence check names
// Every id in release-qa-floor.json must be mapped; skipped/failed/missing required checks fail the receipt.
// `qa.journeys` maps the floor's `journey` id to "every release-critical journey required on this platform passed":
// `rightkit-qa journeys run` is run (or `receipt` is read), the receipt is validated through `rightkit-qa journeys
// validate-receipt --manifest .. --tree ..`, and the validated summary is sealed into the receipt (release-qa-hardening.mjs).
// This command refuses to seal from inside a QA run (RIGHT_RELEASE_QA_STAGE set): sealing is never test execution.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL, fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { assertPrimaryReleaseCheckout } from "./release-invocation.mjs";
import { assertCleanSource } from "./source-gate.mjs";
import { verifySealedRelease } from "./release-state.mjs";
import { auditMain } from "./release-qa-audit.mjs";
import { journeyRequiredItem, QA_RUN_ENV, relativePosix, validateJourneysReceipt } from "./release-qa-hardening.mjs";
import { prepareInstalledArtifacts } from "./release-qa-installed.mjs";
import { main as policyMain } from "./release-qa-policy.mjs";
import { darwinKeyFile, PROTECTED_SECRETS } from "./protected-credentials.mjs";
import {
  buildQaReceipt, currentPolicyBundleDigest, dependencyClosure, ensureQaReceiptKey, evaluateRequiredChecks, loadQaFloor,
  qaReceiptPath, QA_STAGES, sealQaReceipt, sourceIdentity, validateAppQaConfig, writeQaReceipt,
} from "./release-qa-receipt.mjs";

const sha256File = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");

// First `right-release qa` on a host creates the MAC key as an owner-only file in the per-user RightKit directory.
function provisionKeyOnHost(value, platform = process.platform) {
  const spec = PROTECTED_SECRETS["qa-receipt-key"];
  let file;
  if (platform === "darwin") file = darwinKeyFile(process.env, spec);
  else if (platform === "win32") file = path.join(process.env.APPDATA || path.join(os.homedir(), "AppData", "Roaming"), "RightKit", spec.file);
  else throw new Error("no protected credential store on this platform; set RIGHTKIT_QA_RECEIPT_KEY in hosted CI");
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, value, { mode: 0o600, flag: "wx" });
}

function resolveRunner({ qa, appRoot, platform = process.platform, runner = spawnSync }) {
  if (qa.runner) { const file = path.resolve(appRoot, qa.runner); if (!existsSync(file)) throw new Error(`qa.runner does not exist: ${file}`); return realpathSync(file); }
  const found = runner(platform === "win32" ? "where" : "which", ["rightkit-qa"], { encoding: "utf8", windowsHide: true });
  const file = String(found.stdout ?? "").split(/\r?\n/)[0].trim();
  if (found.status !== 0 || !file) throw new Error("rightkit-qa runner not found on PATH; install it (RightKit crates/rightkit-qa) or set qa.runner in the release config");
  return realpathSync(file);
}

// Windows installs land in `%LOCALAPPDATA%\<Product>`; the runner learns the product folder names from
// RIGHTKIT_QA_INSTALLED_DIRS. Compared as strings with both separators so the answer never depends on the host OS.
const WIN_ABSOLUTE = /^(?:[A-Za-z]:[\\/]|\\\\)/;
const pathParts = (value) => String(value).split(/[\\/]+/).filter(Boolean);
const productDirOk = (name) => name.length <= 255 && ![".", ".."].includes(name) && !/[\u0000-\u001f\u007f;:]/.test(name);

/**
 * Product folder names under %LOCALAPPDATA% that hold the app's installed copy, from the release config's
 * `development.targets.win.install.expectedInstalledPath` (the one place a config names the installed path; the
 * desktop-app-template sets it to `<LOCALAPPDATA>\<Product>\<exe>`). Only an absolute path inside LOCALAPPDATA
 * counts (case-insensitive, either separator); the name is the first component below it and a file must sit under it.
 * `Programs` is already a runner root, so it is dropped. Returns an array of distinct names (today at most one: the
 * config declares one installed path).
 */
export function installedDirNames(config, env = process.env) {
  const localAppData = Object.entries(env ?? {}).find(([key]) => key.toLowerCase() === "localappdata")?.[1];
  const installed = config?.development?.targets?.win?.install?.expectedInstalledPath;
  if (typeof localAppData !== "string" || !WIN_ABSOLUTE.test(localAppData) || typeof installed !== "string" || !WIN_ABSOLUTE.test(installed)) return [];
  const root = pathParts(localAppData).map((part) => part.toLowerCase());
  const parts = pathParts(installed);
  if (parts.some((part) => part === "." || part === "..")) return [];
  if (parts.length < root.length + 2 || !root.every((part, index) => parts[index].toLowerCase() === part)) return [];
  const name = parts[root.length];
  return name.toLowerCase() === "programs" || !productDirOk(name) ? [] : [name];
}

/** `rightkit-qa journeys run` prints `run <id> (<run dir>)` first; the receipt is `<run dir>/receipt.json`. */
export function journeysReceiptFromOutput(stdout) {
  const dir = String(stdout ?? "").match(/^run \S+ \((.+)\)\s*$/m)?.[1];
  return dir ? path.join(dir, "receipt.json") : null;
}

export function evidencePathFromOutput(stdout) {
  const line = String(stdout).split(/\r?\n/).reverse().find((item) => item.startsWith("[qa]") && /evidence\s+\S/.test(item));
  return line?.match(/evidence\s+(.+?)\s*$/)?.[1] ?? null;
}

/** Run the QA plan through the runner, then seal a receipt. `deps` are test seams; production uses the defaults. */
export async function runQa({ stage, platform, releaseIdArg, configPath, carryTo = null, deps = {} }) {
  if ((deps.env ?? process.env)[QA_RUN_ENV]) throw new Error(`refusing to seal a QA receipt inside a QA run (${QA_RUN_ENV} is set): sealing and signing happen outside test execution`);
  const executionStartedAt = new Date();
  const gitRunner = deps.gitRunner ?? spawnSync;
  const appRoot = path.dirname(configPath);
  const repoRoot = String(gitRunner("git", ["-C", appRoot, "rev-parse", "--show-toplevel"], { encoding: "utf8", windowsHide: true }).stdout ?? "").trim();
  if (!repoRoot) throw new Error("release config must live inside a Git repository");
  if (!deps.skipPrimaryCheckout) assertPrimaryReleaseCheckout(repoRoot);
  assertCleanSource({ status: String(gitRunner("git", ["-C", repoRoot, "status", "--porcelain=v1", "-z", "--untracked-files=all"], { encoding: "utf8", windowsHide: true }).stdout ?? ""), commandId: "right-release qa" });
  const source = sourceIdentity(repoRoot, gitRunner);
  const config = (await import(`${pathToFileURL(configPath).href}?qa=${source.commit}`)).default;
  if (!config?.app || !config?.version) throw new Error("release config must expose app and version");
  const { floor, digest: floorDigest } = loadQaFloor(deps.floorDir);
  const problems = validateAppQaConfig(config.qa, floor, stage);
  if (problems.length) throw new Error(`release config QA plan does not meet the protected QA floor:\n- ${problems.join("\n- ")}`);

  let releaseId = releaseIdArg;
  let sealed = null;
  let artifacts = [];
  let sourceReceiptDigest = null;
  if (stage === "source") releaseId = `${config.app}-${config.version}-${source.commit.slice(0, 8)}`;
  else {
    if (!releaseId) throw new Error("--release <sealed-id> is required for the installed stage");
    sealed = verifySealedRelease(path.join(repoRoot, ".right-release", "sealed", releaseId, platform === "win" ? "windows" : "mac"));
    if (sealed.manifest.commit !== source.commit) throw new Error("sealed release was built from a different commit than the current checkout");
    sourceReceiptDigest = sealed.manifest.qa?.sourceReceiptDigest ?? null;
    if (!sourceReceiptDigest) throw new Error("sealed release records no source QA receipt; it was not built through the QA gate; rebuild after `right-release qa --stage source`");
    artifacts = sealed.manifest.files.map((file) => ({ name: file.name, sha256: file.sha256, sizeBytes: file.sizeBytes }));
  }

  const key = ensureQaReceiptKey({ env: deps.env ?? process.env, credentialStore: deps.credentialStore, provision: deps.provision ?? provisionKeyOnHost });
  const runnerFile = deps.runnerFile ?? resolveRunner({ qa: config.qa, appRoot });
  const runnerSha = sha256File(runnerFile);
  const journeysConfig = config.qa.journeys ?? null;
  const qaConfig = path.resolve(appRoot, config.qa.config);
  if (!existsSync(qaConfig)) throw new Error(`qa.config does not exist: ${qaConfig}`);
  // #184: opt in with `qa.installed: { installDir? }`. Mounts the sealed DMG (mac) or runs the sealed NSIS installer into a
  // throwaway directory (win) and hands the runner the opened app; always cleaned up, even when the runner throws.
  let installedQa = null;
  if (stage === "installed" && config.qa.installed && config.qa.installed !== false) {
    installedQa = (deps.prepareInstalled ?? prepareInstalledArtifacts)({
      sealedDir: sealed.sealedDir, manifest: sealed.manifest, platform,
      installDir: typeof config.qa.installed === "object" ? config.qa.installed.installDir ?? null : null,
      deps: { log: (event) => { if (!deps.quiet) console.error(`[qa] ${JSON.stringify(event)}`); } },
    });
  }
  let run, journeysRun = null;
  try {
    const expand = (value) => String(value).replaceAll("{sealedDir}", sealed?.sealedDir ?? "").replaceAll("{releaseId}", releaseId)
      .replaceAll("{installedApp}", installedQa?.installedApp ?? "").replaceAll("{installedDir}", installedQa?.installedDir ?? "").replaceAll("{qaRoot}", installedQa?.qaRoot ?? "");
    const installedDirs = installedDirNames(config, deps.env ?? process.env);
    const runEnv = {
      ...process.env, RIGHT_RELEASE_QA_STAGE: stage, RIGHT_RELEASE_QA_COMMIT: source.commit, RIGHT_RELEASE_QA_RELEASE_ID: releaseId,
      ...(sealed ? { RIGHT_RELEASE_QA_SEALED_DIR: sealed.sealedDir } : {}), ...(installedDirs.length ? { RIGHTKIT_QA_INSTALLED_DIRS: installedDirs.join(";") } : {}), ...(installedQa?.env ?? {}),
    };
    const args = ["run", "--config", qaConfig, ...((config.qa.args?.[stage] ?? []).map(expand))];
    run = (deps.spawnRunner ?? spawnSync)(runnerFile, args, {
      cwd: appRoot, encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024,
      env: runEnv,
    });
    // Journeys lane: the one release proof (ADR-0001 item 10). A receipt named in the config is read as is.
    if (journeysConfig && !journeysConfig.receipt) {
      journeysRun = (deps.spawnJourneys ?? deps.spawnRunner ?? spawnSync)(runnerFile, ["journeys", "run", "--root", appRoot, ...((journeysConfig.args?.[stage] ?? []).map(expand))], {
        cwd: appRoot, encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024, env: runEnv,
      });
    }
  } finally {
    installedQa?.cleanup();
  }
  if (!deps.quiet) { process.stdout.write(String(run.stdout ?? "")); process.stderr.write(String(run.stderr ?? "")); }
  const evidenceFile = evidencePathFromOutput(run.stdout);
  let evidence = null, evidenceSha256 = null;
  if (evidenceFile && existsSync(evidenceFile)) {
    const bytes = readFileSync(evidenceFile);
    evidenceSha256 = createHash("sha256").update(bytes).digest("hex");
    evidence = JSON.parse(bytes.toString("utf8"));
  }
  // The runner binary that produced the evidence is the one that was digested; a swap during the run is refused.
  if (sha256File(runnerFile) !== runnerSha) throw new Error("the rightkit-qa runner changed while QA was running; refusing to seal");
  let journeys = null;
  let required = evaluateRequiredChecks({ floor, stage, qa: config.qa, evidence });
  if (journeysConfig && floor.journeys) {
    if (journeysRun && !deps.quiet) { process.stdout.write(String(journeysRun.stdout ?? "")); process.stderr.write(String(journeysRun.stderr ?? "")); }
    const manifestFile = path.resolve(appRoot, journeysConfig.manifest ?? "journeys.toml");
    const receiptFile = journeysConfig.receipt ? path.resolve(appRoot, String(journeysConfig.receipt)) : journeysReceiptFromOutput(journeysRun?.stdout);
    const validation = receiptFile
      ? validateJourneysReceipt({ runnerFile, runnerSha256: runnerSha, receiptFile, manifestFile, manifestRelPath: relativePosix(repoRoot, manifestFile), platform, spawn: deps.spawnValidator ?? deps.spawnRunner ?? spawnSync })
      : { ok: false, problems: ["rightkit-qa journeys run reported no run directory, so there is no journeys receipt to validate"], block: null };
    if (!deps.quiet) for (const problem of validation.problems) console.error(`[qa] journeys: ${problem}`);
    journeys = validation.block;
    required = required.map((item) => (item.id === floor.journeys.floorId ? journeyRequiredItem({ floorId: item.id, validation }) : item));
  }
  const executionFinishedAt = new Date();
  // A passing receipt needs the runner to exit cleanly AND an evidence bundle that itself passed.
  if (run.status !== 0 && evidence) evidence = { ...evidence, status: "failed" };
  const receipt = sealQaReceipt(buildQaReceipt({
    stage, app: config.app, platform, releaseId, source, closure: dependencyClosure(repoRoot, source.commit, gitRunner),
    releaseConfigSha256: sha256File(configPath), qa: config.qa, floor, floorDigest,
    policyBundleDigest: deps.policyBundleDigest ?? currentPolicyBundleDigest(deps.floorDir),
    runner: { basename: path.basename(runnerFile).replace(/\.exe$/i, ""), path: runnerFile, sha256: runnerSha },
    evidence, evidenceSha256, required, artifacts, sourceReceiptDigest, journeys,
    execution: {
      startedAt: executionStartedAt.toISOString(), finishedAt: executionFinishedAt.toISOString(),
      runnerPids: [run.pid, journeysRun?.pid].filter((pid) => Number.isInteger(pid) && pid > 0), sealerPid: process.pid,
    },
  }), key);
  const file = qaReceiptPath({ repoRoot, releaseId, platform, stage });
  writeQaReceipt(file, receipt);
  // #183: copy the sealed receipt next to the candidate handoff so the signing job (a different runner) can read it.
  let carriedFile = null;
  if (carryTo) {
    carriedFile = path.join(path.resolve(carryTo), `${stage}.receipt.json`);
    writeQaReceipt(carriedFile, receipt);
  }
  return { receipt, file, carriedFile };
}

function usage(code) {
  console.log("usage: right-release qa --stage source|installed --platform mac|win [--release <sealed-id>] [--config right-release.config.mjs] [--carry-to <dir>]\n       (seals the QA receipt; admission is warn or refuse per app in qa-admission-policy.json; refused inside a QA run)\n       right-release qa audit [--app <dir>] [--json] [--since <YYYY-MM-DD>]\n       right-release qa policy show | set --app <app> --mode warn|refuse --set-by <name> [--stage source|installed]\n                             | approve-runner --runner <file> --version <v> --platform mac|win --set-by <name> [--note <text>]");
  process.exit(code);
}

export async function main(args) {
  if (args[0] === "audit") process.exit(auditMain(args.slice(1)));
  if (args[0] === "policy") process.exit(policyMain(args.slice(1), { program: "right-release qa policy" }));
  let stage = "", platform = process.platform === "win32" ? "win" : "mac", releaseId = "", configName = "right-release.config.mjs", carryTo = null;
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--stage") stage = args[++index];
    else if (arg === "--platform") platform = args[++index];
    else if (arg === "--release") releaseId = args[++index];
    else if (arg === "--config") configName = args[++index];
    else if (arg === "--carry-to") carryTo = args[++index];
    else if (arg === "-h" || arg === "--help") usage(0);
    else { console.error(`right-release qa: unknown argument: ${arg}`); process.exit(2); }
  }
  if (!QA_STAGES.includes(stage)) { console.error("right-release qa: --stage must be source or installed"); process.exit(2); }
  if (platform !== "win" && platform !== "mac") { console.error("right-release qa: --platform must be win or mac"); process.exit(2); }
  try {
    const { receipt, file, carriedFile } = await runQa({ stage, platform, releaseIdArg: releaseId, configPath: path.resolve(configName), carryTo });
    console.log(`right-release qa: ${stage} receipt ${receipt.status} ${receipt.digest.slice(0, 16)} -> ${file}`);
    if (carriedFile) console.log(`right-release qa: carried copy -> ${carriedFile}`);
    for (const item of receipt.required) console.log(`  ${item.id}: ${item.status}`);
    process.exit(receipt.status === "passed" ? 0 : 1);
  } catch (error) {
    console.error(`right-release qa: ${error.message}`);
    process.exit(1);
  }
}

if (process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url))) await main(process.argv.slice(2));
