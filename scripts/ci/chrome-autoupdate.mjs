#!/usr/bin/env node
// Daily: keep Chrome for Testing on Google's current Stable without anyone asking (Adrian, 2026-10-23: "Chrome is always
// at the latest update"). Run by the LaunchAgent dev.rightkit.chrome-update on the Mac (owner host of the RightKit
// primary checkout); every step logs one JSON line to /Volumes/D/scratch/rk-chrome-update/<date>.ndjson.
//
//   1. chrome-update --check. Current: log and exit.
//   2. Outdated: only in an idle, clean primary checkout (HEAD == origin/main, no tracked changes, no rk-pipeline
//      coordinator lock held, no pre-flight running); otherwise log `deferred` with the reason and exit (the next
//      daily run retries; nothing is half-applied).
//   3. chrome-update --apply (all pins, digests from the official archives, patch bumps and the qa cascade).
//   4. Refresh the pnpm lockfile and run the focused checks through the broker (chrome-update + publish contract tests,
//      chrome-pins agreement, qa-chrome tests). A red check reverts every file the update touched and files an issue.
//   5. Commit and push, then run the publication loops for the bumped units: crates rightkit-browser (its pre-flight
//      builds and runs the browser fixture against the new Chrome locally) and npm @rightkit/qa-chrome / @rightkit/qa.
//   6. Any unit that does not publish is reported in a GitHub issue in bogusyogi/rightkit with the loop's lines; the new
//      pin stays on main (being current is the requirement; a red unit is work to do, not a reason to roll back).
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const NODE = process.execPath;
const LOG_DIR = "/Volumes/D/scratch/rk-chrome-update";
const PIPELINE_LOCK = "/Volumes/D/scratch/rk-pipeline/state.json.lock";
const REPO = "bogusyogi/rightkit";
fs.mkdirSync(LOG_DIR, { recursive: true });
const logFile = path.join(LOG_DIR, `${new Date().toISOString().slice(0, 10)}.ndjson`);
const log = (event, fields = {}) => { const line = JSON.stringify({ at: new Date().toISOString(), event, ...fields }); fs.appendFileSync(logFile, `${line}\n`); console.log(line); };
const sh = (command, args, opts = {}) => {
  const r = spawnSync(command, args, { cwd: ROOT, encoding: "utf8", maxBuffer: 256 * 1024 * 1024, timeout: opts.timeoutMs ?? 30 * 60_000, env: { ...process.env, ...(opts.env ?? {}) } });
  return { code: r.status ?? 1, out: `${r.stdout ?? ""}${r.stderr ?? ""}`, error: r.error?.message };
};
const git = (...args) => sh("git", args);
const tail = (text, n = 20) => text.split("\n").filter(Boolean).slice(-n).join("\n");

function fileIssue(title, body) {
  const r = sh("gh", ["issue", "create", "--repo", REPO, "--title", title, "--body", body]);
  log("issue", { title, code: r.code, url: r.out.trim().split("\n").pop() });
}

function idleReason() {
  if (git("status", "--porcelain", "--untracked-files=no").out.trim()) return "tracked files are modified in the primary checkout";
  git("fetch", "-q", "origin");
  if (git("rev-parse", "HEAD").out.trim() !== git("rev-parse", "origin/main").out.trim()) return "HEAD is not origin/main";
  if (fs.existsSync(PIPELINE_LOCK)) {
    try { const pid = JSON.parse(fs.readFileSync(PIPELINE_LOCK, "utf8")).pid; process.kill(pid, 0); return `the publication coordinator (pid ${pid}) is running`; } catch { /* stale lock: the coordinator reclaims it */ }
  }
  if (sh("pgrep", ["-f", "rk-(npm-)?preflight[.]mjs"]).out.trim()) return "a pre-flight is running";
  return null;
}

async function main() {
  const check = sh(NODE, [path.join(ROOT, "scripts", "ci", "chrome-update.mjs"), "--check"], { timeoutMs: 120_000 });
  log("check", { code: check.code, result: check.out.trim() });
  if (check.code === 0) return 0;
  if (check.code !== 3) { fileIssue("Chrome for Testing daily check failed", `\`chrome-update.mjs --check\` exited ${check.code}:\n\n\`\`\`\n${tail(check.out)}\n\`\`\``); return 1; }
  const busy = idleReason();
  if (busy) { log("deferred", { reason: busy }); return 0; }

  const before = git("rev-parse", "HEAD").out.trim();
  const apply = sh(NODE, [path.join(ROOT, "scripts", "ci", "chrome-update.mjs"), "--apply"], { env: { RUNNER_TEMP: "/Volumes/D/scratch/rk-chrome-update" } });
  log("apply", { code: apply.code, out: tail(apply.out, 12) });
  const revert = () => { git("checkout", "--", "."); log("reverted", { to: before }); };
  if (apply.code !== 0) { revert(); fileIssue("Chrome for Testing update could not be applied", `\`chrome-update.mjs --apply\`:\n\n\`\`\`\n${tail(apply.out)}\n\`\`\``); return 1; }
  const version = JSON.parse(fs.readFileSync(path.join(ROOT, "ci", "chrome-for-testing.json"), "utf8")).version;

  // Focused checks, all through the broker or plain node (no compile outside the broker).
  const nonce = () => sh("python3", ["-I", "-c", "import secrets; print(secrets.token_hex(32))"]).out.trim();
  const steps = [
    ["lockfile", "rustright", ["run", "rightkit-pnpm", "--", "install", "--store-dir", "/Volumes/D/.pnpm-store/v11"], true],
    ["pins-agree", NODE, [path.join(ROOT, "scripts", "chrome-pins.mjs")]],
    ["contract-tests", NODE, ["--test", "scripts/ci/chrome-update.test.mjs", "scripts/ci/publish.test.mjs"]],
    ["qa-chrome-tests", "rustright", ["run", "rightkit-pnpm", "--", "--dir", "packages/qa-chrome", "run", "test"], true],
  ];
  for (const [name, command, args, attested] of steps) {
    const r = sh(command, args, { env: attested ? { RUSTRIGHT_ATTEST_NONCE: nonce() } : {} });
    log("step", { name, code: r.code, out: tail(r.out, 6) });
    if (r.code !== 0) { revert(); fileIssue(`Chrome for Testing ${version}: ${name} failed; pin not moved`, `Daily update to Stable ${version} reverted after \`${name}\`:\n\n\`\`\`\n${tail(r.out, 40)}\n\`\`\``); return 1; }
  }
  git("add", "-A");
  const commit = sh("git", ["-c", "user.name=Adrian D'souza", "-c", "user.email=adrian@orthiclabs.com", "commit", "-q", "-m", `chrome: Chrome for Testing ${version} (current Stable), daily update\n\nscripts/ci/chrome-autoupdate.mjs: every pin moved to Google's Stable with archive and executable digests from the\nofficial downloads (chrome-update.mjs --apply); pins agree; contract and qa-chrome tests pass through the broker.\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`]);
  const push = git("push", "origin", "main");
  log("commit", { commit: commit.code, push: push.code, head: git("rev-parse", "--short", "HEAD").out.trim() });
  if (commit.code !== 0 || push.code !== 0) { fileIssue(`Chrome for Testing ${version}: commit or push failed`, `\`\`\`\n${tail(commit.out + push.out, 30)}\n\`\`\``); return 1; }

  // Publication of the units the pin lives in. The dispatch adapter is pinned to the committed dispatcher, which this
  // update does not change, so no re-pin is needed.
  const loops = [["crates", ["rightkit-browser"]], ["npm", ["@rightkit/qa-chrome", "@rightkit/qa"]]];
  const problems = [];
  for (const [registry, units] of loops) {
    const r = sh(NODE, [path.join(ROOT, "scripts", "ci", "rk-pipeline.mjs"), "loop", "--registry", registry, "--jobs", "2", ...units], { timeoutMs: 6 * 60 * 60_000 });
    const lines = r.out.split("\n").filter((l) => /PUBLISHED|failure|RED|held|unfinished|refused/.test(l));
    log("publish", { registry, code: r.code, lines });
    if (r.code !== 0) problems.push(`${registry} (exit ${r.code}):\n${lines.join("\n")}`);
  }
  if (problems.length) fileIssue(`Chrome for Testing ${version}: pinned on main, not every unit published`, `The pin is current on main. These units did not publish:\n\n\`\`\`\n${problems.join("\n\n")}\n\`\`\`\n\nLog: ${logFile}`);
  return problems.length ? 1 : 0;
}

main().then((code) => process.exit(code), (error) => { log("error", { message: error.message }); process.exit(1); });
