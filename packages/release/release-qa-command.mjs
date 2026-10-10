#!/usr/bin/env node
// right-release qa: run RightKit's own QA runner (rightkit-qa) and write the sealed, MAC'd QA receipt.
//
//   right-release qa --stage source    --platform mac|win [--config right-release.config.mjs]
//   right-release qa --stage installed --platform mac|win --release <sealed-id> [--config right-release.config.mjs]
//   right-release qa audit [--app <dir>] [--json] [--since <YYYY-MM-DD>]   (release-qa-audit.mjs; exit 1 when an uploaded build lacks passing QA)
//
// The release config declares the app's QA plan:
//   qa: { version: "1", config: "rightkit-qa.toml", runner?: "<path to rightkit-qa>",
//         args?: { source?: [..], installed?: [..] },                                   // {sealedDir} {releaseId} expand
//         checks: { "smoke.launch": ["app.launch"], journey: ["journey.*"] } }          // floor id -> evidence check names
// Every id in release-qa-floor.json must be mapped; skipped/failed/missing required checks fail the receipt.
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

export function evidencePathFromOutput(stdout) {
  const line = String(stdout).split(/\r?\n/).reverse().find((item) => item.startsWith("[qa]") && /evidence\s+\S/.test(item));
  return line?.match(/evidence\s+(.+?)\s*$/)?.[1] ?? null;
}

/** Run the QA plan through the runner, then seal a receipt. `deps` are test seams; production uses the defaults. */
export async function runQa({ stage, platform, releaseIdArg, configPath, deps = {} }) {
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
  const qaConfig = path.resolve(appRoot, config.qa.config);
  if (!existsSync(qaConfig)) throw new Error(`qa.config does not exist: ${qaConfig}`);
  const expand = (value) => String(value).replaceAll("{sealedDir}", sealed?.sealedDir ?? "").replaceAll("{releaseId}", releaseId);
  const args = ["run", "--config", qaConfig, ...((config.qa.args?.[stage] ?? []).map(expand))];
  const run = (deps.spawnRunner ?? spawnSync)(runnerFile, args, {
    cwd: appRoot, encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024,
    env: { ...process.env, RIGHT_RELEASE_QA_STAGE: stage, RIGHT_RELEASE_QA_COMMIT: source.commit, RIGHT_RELEASE_QA_RELEASE_ID: releaseId, ...(sealed ? { RIGHT_RELEASE_QA_SEALED_DIR: sealed.sealedDir } : {}) },
  });
  if (!deps.quiet) { process.stdout.write(String(run.stdout ?? "")); process.stderr.write(String(run.stderr ?? "")); }
  const evidenceFile = evidencePathFromOutput(run.stdout);
  let evidence = null, evidenceSha256 = null;
  if (evidenceFile && existsSync(evidenceFile)) {
    const bytes = readFileSync(evidenceFile);
    evidenceSha256 = createHash("sha256").update(bytes).digest("hex");
    evidence = JSON.parse(bytes.toString("utf8"));
  }
  const required = evaluateRequiredChecks({ floor, stage, qa: config.qa, evidence });
  // A passing receipt needs the runner to exit cleanly AND an evidence bundle that itself passed.
  if (run.status !== 0 && evidence) evidence = { ...evidence, status: "failed" };
  const receipt = sealQaReceipt(buildQaReceipt({
    stage, app: config.app, platform, releaseId, source, closure: dependencyClosure(repoRoot, source.commit, gitRunner),
    releaseConfigSha256: sha256File(configPath), qa: config.qa, floor, floorDigest,
    policyBundleDigest: deps.policyBundleDigest ?? currentPolicyBundleDigest(deps.floorDir),
    runner: { basename: path.basename(runnerFile).replace(/\.exe$/i, ""), path: runnerFile, sha256: runnerSha },
    evidence, evidenceSha256, required, artifacts, sourceReceiptDigest,
  }), key);
  const file = qaReceiptPath({ repoRoot, releaseId, platform, stage });
  writeQaReceipt(file, receipt);
  return { receipt, file };
}

function usage(code) {
  console.log("usage: right-release qa --stage source|installed --platform mac|win [--release <sealed-id>] [--config right-release.config.mjs]\n       right-release qa audit [--app <dir>] [--json] [--since <YYYY-MM-DD>]");
  process.exit(code);
}

export async function main(args) {
  if (args[0] === "audit") process.exit(auditMain(args.slice(1)));
  let stage = "", platform = process.platform === "win32" ? "win" : "mac", releaseId = "", configName = "right-release.config.mjs";
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--stage") stage = args[++index];
    else if (arg === "--platform") platform = args[++index];
    else if (arg === "--release") releaseId = args[++index];
    else if (arg === "--config") configName = args[++index];
    else if (arg === "-h" || arg === "--help") usage(0);
    else { console.error(`right-release qa: unknown argument: ${arg}`); process.exit(2); }
  }
  if (!QA_STAGES.includes(stage)) { console.error("right-release qa: --stage must be source or installed"); process.exit(2); }
  if (platform !== "win" && platform !== "mac") { console.error("right-release qa: --platform must be win or mac"); process.exit(2); }
  try {
    const { receipt, file } = await runQa({ stage, platform, releaseIdArg: releaseId, configPath: path.resolve(configName) });
    console.log(`right-release qa: ${stage} receipt ${receipt.status} ${receipt.digest.slice(0, 16)} -> ${file}`);
    for (const item of receipt.required) console.log(`  ${item.id}: ${item.status}`);
    process.exit(receipt.status === "passed" ? 0 : 1);
  } catch (error) {
    console.error(`right-release qa: ${error.message}`);
    process.exit(1);
  }
}

if (process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url))) await main(process.argv.slice(2));
