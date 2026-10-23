// Shared builders for the QA admission hardening tests (release-qa-hardening.test.mjs, release-qa-carried.test.mjs).
// Test support only: nothing in the release path imports this file.
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { journeyRequiredItem, validateJourneysReceipt } from "./release-qa-hardening.mjs";
import {
  buildQaReceipt, dependencyClosure, evaluateRequiredChecks, loadQaFloor, qaReceiptPath, sealQaReceipt, sourceIdentity, writeQaReceipt,
} from "./release-qa-receipt.mjs";

export const here = path.dirname(fileURLToPath(import.meta.url));
export const KEY = "unit-test-key";
export const store = { read: () => KEY };
export const BUNDLE = "b".repeat(64);
export const SHA = (c) => c.repeat(64);
export const RUNNER_SHA = SHA("e");
export const OTHER_RUNNER_SHA = SHA("9");
export const policyWith = ({ apps = {}, approvedRunners = [{ sha256: RUNNER_SHA, version: "0.8.0" }] } = {}) => ({ policy: { schema: 1, apps, approvedRunners }, digest: SHA("a"), baseline: "none" });

// J-001 and J-002 are release-critical on macos and windows, J-003 is not critical, J-004 is critical on windows only.
export const MANIFEST_TOML = `version = 1
app = "demo"

[[journey]]
id = "J-001"
platforms = ["macos", "windows"]
release_critical = true

[[journey]]
id = "J-002"
platforms = ["macos", "windows"]
release_critical = true

[[journey]]
id = "J-003"
platforms = ["macos", "windows"]
release_critical = false

[[journey]]
id = "J-004"
platforms = ["windows"]
release_critical = true
`;

export const sha256 = (value) => createHash("sha256").update(value).digest("hex");

function sh(cwd, args) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8" });
  if (result.status !== 0) throw new Error(`git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** A git repo with a lockfile and a committed journeys.toml. */
export function gitFixture(manifestText = MANIFEST_TOML) {
  const root = mkdtempSync(path.join(tmpdir(), "rk-qa-hard-"));
  sh(root, ["init", "-q", "-b", "main"]);
  sh(root, ["config", "user.email", "t@example.com"]); sh(root, ["config", "user.name", "t"]);
  sh(root, ["remote", "add", "origin", "https://github.com/bogusyogi/example-app.git"]);
  writeFileSync(path.join(root, "Cargo.lock"), "lock-1\n");
  writeFileSync(path.join(root, ".gitignore"), ".right-release/\n");
  writeFileSync(path.join(root, "journeys.toml"), manifestText);
  sh(root, ["add", "-A"]); sh(root, ["commit", "-q", "-m", "init"]);
  return root;
}

export const OS = { mac: "macos", win: "windows" };

/** A journeys receipt as rightkit-qa writes it (only the fields the release side reads). */
export function journeysReceiptJson({ root, platform = "mac", manifestText = MANIFEST_TOML, statuses = {}, hostOs, startedAt, endedAt }) {
  const os = hostOs ?? OS[platform];
  const critical = (id) => ["J-001", "J-002", "J-004"].includes(id);
  const ids = ["J-001", "J-002", "J-003", ...(platform === "win" ? ["J-004"] : [])];
  return {
    schema: "rightkit.journeys.receipt/1", run_id: "run-1", runner: "rightkit-qa", runner_version: "0.8.0", verdict: "passed",
    started_at: startedAt, ended_at: endedAt, host: { os, arch: "x86_64", session: "interactive" },
    source: { head: sourceIdentity(root).commit, tree: sourceIdentity(root).tree, dirty: false, diff_sha256: null },
    manifest: { path: "journeys.toml", sha256: sha256(manifestText) }, selection: { ids, platform: os, tier: "fast" },
    journeys: ids.map((id) => ({ id, status: statuses[id] ?? "passed", release_critical: critical(id) })),
  };
}

export const accepting = () => ({ status: 0, stdout: "receipt claims passed, recomputed passed: ACCEPTED\n", stderr: "" });
export const refusing = () => ({ status: 1, stdout: "- journey_failed [J-001]: journey failed\nreceipt claims passed, recomputed failed: REFUSED\n", stderr: "" });

const qa = { version: "1", config: "rightkit-qa.toml", journeys: {}, checks: { "smoke.launch": ["app.launch"] } };
const evidence = () => ({ schemaVersion: 4, runId: "r1", status: "passed", finishedAt: "2026-10-09T00:00:00Z", checks: [{ name: "app.launch", status: "passed" }] });

/**
 * A sealed source (or installed) QA receipt written where the gate reads it, as `right-release qa` would write it
 * with qa.journeys configured. Timestamps are relative to now so the execution window precedes admission.
 */
export function writeHardenedReceipt(root, {
  stage = "source", platform = "mac", releaseId, receiptPlatform = platform, receiptReleaseId = releaseId, statuses = {}, runnerSha = RUNNER_SHA,
  spawn = accepting, execution, tweakBlock, nowMs = Date.now(), artifacts = [], sourceReceiptDigest = null,
} = {}) {
  const at = (offsetSec) => new Date(nowMs + offsetSec * 1000).toISOString();
  const dir = mkdtempSync(path.join(tmpdir(), "rk-qa-jr-"));
  const receiptFile = path.join(dir, "receipt.json");
  writeFileSync(receiptFile, JSON.stringify(journeysReceiptJson({ root, platform: receiptPlatform, statuses, startedAt: at(-110), endedAt: at(-20) })));
  const manifestFile = path.join(root, "journeys.toml");
  const validation = validateJourneysReceipt({ runnerFile: "rightkit-qa", runnerSha256: runnerSha, receiptFile, manifestFile, manifestRelPath: "journeys.toml", platform: receiptPlatform, spawn });
  const { floor, digest } = loadQaFloor(here);
  const source = sourceIdentity(root);
  // `tweakBlock` forges only the receipt's journeys block; the sealed required item stays the honest one, so the
  // admission side's recompute is what must catch the forgery.
  const block = tweakBlock ? tweakBlock(validation.block) : validation.block;
  const required = evaluateRequiredChecks({ floor, stage, qa, evidence: evidence() }).map((item) => (item.id === floor.journeys.floorId ? journeyRequiredItem({ floorId: item.id, validation }) : item));
  const receipt = sealQaReceipt(buildQaReceipt({
    stage, app: "demo", platform: receiptPlatform, releaseId: receiptReleaseId, source, closure: dependencyClosure(root, source.commit), releaseConfigSha256: SHA("d"),
    qa, floor, floorDigest: digest, policyBundleDigest: BUNDLE, runner: { basename: "rightkit-qa", path: "/x/rightkit-qa", sha256: runnerSha },
    evidence: evidence(), evidenceSha256: SHA("f"), required, artifacts, sourceReceiptDigest, journeys: block, now: new Date(nowMs - 5000),
    execution: execution === false ? null : (execution ?? { startedAt: at(-120), finishedAt: at(-10), runnerPids: [4242], sealerPid: 4343 }),
  }), KEY);
  writeQaReceipt(qaReceiptPath({ repoRoot: root, releaseId, platform, stage }), receipt);
  return { receipt, validation };
}

export const gateOpts = (root, releaseId, platform = "mac", extra = {}) => ({
  repoRoot: root, releaseId, platform, configName: "right-release.config.mjs", releaseConfigSha256: SHA("d"),
  credentialStore: store, policyBundleDigest: BUNDLE, floorDir: here, ...extra,
});

export const releaseIdOf = (root) => `demo-1.0.0-${sourceIdentity(root).commit.slice(0, 8)}`;
export const makeDir = (root, sub) => { const dir = path.join(root, sub); mkdirSync(dir, { recursive: true }); return dir; };
