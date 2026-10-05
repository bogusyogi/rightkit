#!/usr/bin/env node
// Release preflight for the Orthic-Labs release-candidate pipeline (legion, membrane).
// Each check below is derived from a real failure observed 2026-09-02; see the cited run ids.
//
// Usage:
//   node preflight.mjs <product> <version> <sha> [--checkout <path>] [--json]
//
// Exit code 0 on PREFLIGHT PASS, 1 on PREFLIGHT FAIL. Read-only: never mutates
// the checkout, the GitHub repo, or credentials, and never prints token values.

import { spawnSync } from "node:child_process";
import { readFileSync, existsSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

// ---------------------------------------------------------------------------
// Product registry
// ---------------------------------------------------------------------------

const PRODUCTS = {
  legion: {
    // Legion's release chain calls right-release, whose admitted-dispatch exemption needs refs/remotes/origin/main.
    usesRightReleaseGuard: true,
    repo: "Orthic-Labs/legion",
    defaultCheckout: "D:\\Claude\\legion",
    workflowFile: ".github/workflows/release-candidate.yml",
    assetPattern: (v) => `Legion-${v}-windows-x86_64-setup.exe`,
    versionFile: "release/version.json",
    readVersion: (checkout) => {
      const p = join(checkout, "release/version.json");
      const json = JSON.parse(readFileSync(p, "utf8"));
      // find the key holding the version — schema uses "version", but stay
      // defensive in case the key name drifts.
      if (typeof json.version === "string") return json.version;
      for (const [k, v] of Object.entries(json)) {
        if (typeof v === "string" && /^\d+\.\d+\.\d+$/.test(v)) return v;
      }
      throw new Error(`no version-shaped string field found in ${p}`);
    },
  },
  membrane: {
    // Membrane's chain uses right-git-candidate; its protected jobs signed without fetch-depth on four runs (2026-09-02).
    usesRightReleaseGuard: false,
    repo: "Orthic-Labs/Membrane",
    defaultCheckout: "D:\\Claude\\membrane",
    workflowFile: ".github/workflows/release-candidate.yml",
    assetPattern: (v) => `Membrane_Hub_${v}_x64-setup.exe`,
    versionFile: "apps/membrane-hub/package.json",
    readVersion: (checkout) => {
      const p = join(checkout, "apps/membrane-hub/package.json");
      const json = JSON.parse(readFileSync(p, "utf8"));
      if (typeof json.version !== "string") {
        throw new Error(`no .version field in ${p}`);
      }
      return json.version;
    },
  },
};

// ---------------------------------------------------------------------------
// gh / git helpers
// ---------------------------------------------------------------------------

function runGh(args) {
  const res = spawnSync("gh", args, { encoding: "utf8", windowsHide: true });
  return {
    ok: res.status === 0,
    status: res.status,
    stdout: (res.stdout || "").trim(),
    stderr: (res.stderr || "").trim(),
  };
}

function runGit(args, cwd) {
  const res = spawnSync("git", args, { encoding: "utf8", windowsHide: true, cwd });
  return {
    ok: res.status === 0,
    status: res.status,
    stdout: (res.stdout || "").trim(),
    stderr: (res.stderr || "").trim(),
  };
}

function runNode(args, cwd) {
  const res = spawnSync(process.execPath, args, { encoding: "utf8", windowsHide: true, cwd });
  return {
    ok: res.status === 0,
    status: res.status,
    stdout: (res.stdout || "").trim(),
    stderr: (res.stderr || "").trim(),
  };
}

// ---------------------------------------------------------------------------
// Pure parsers (unit-tested in preflight.test.mjs — no gh/git calls here)
// ---------------------------------------------------------------------------

/**
 * Extract `${{ secrets.X }}` and `${{ vars.X }}` references from workflow YAML
 * text. Returns { secrets: string[], vars: string[] } deduplicated. A `vars.X`
 * reference that carries a `||` fallback default (e.g. `vars.X || 'default'`)
 * is reported separately in `varsWithDefault` and excluded from `vars`, per
 * check 8 (defaulted vars need no environment/repo variable to exist).
 */
export function extractSecretsAndVars(yamlText) {
  const secrets = new Set();
  const vars = new Set();
  const varsWithDefault = new Set();

  const secretRe = /\$\{\{\s*secrets\.([A-Za-z_][A-Za-z0-9_]*)\s*\}\}/g;
  for (const m of yamlText.matchAll(secretRe)) secrets.add(m[1]);

  // Match the whole ${{ ... }} expression so we can detect a `||` default
  // inside the same expression as the vars.X reference.
  const exprRe = /\$\{\{([^}]*)\}\}/g;
  for (const m of yamlText.matchAll(exprRe)) {
    const expr = m[1];
    const varRe = /vars\.([A-Za-z_][A-Za-z0-9_]*)/g;
    let vm;
    while ((vm = varRe.exec(expr))) {
      const name = vm[1];
      if (/\|\|/.test(expr)) {
        varsWithDefault.add(name);
      } else {
        vars.add(name);
      }
    }
  }

  return {
    secrets: [...secrets].sort(),
    vars: [...vars].sort(),
    varsWithDefault: [...varsWithDefault].sort(),
  };
}

/**
 * Split workflow YAML into top-level job blocks under `jobs:`. Returns
 * Map<jobName, blockText>. Minimal indentation-based parser; no yaml dep.
 */
export function splitJobBlocks(yamlText) {
  const lines = yamlText.split(/\r?\n/);
  const jobsIdx = lines.findIndex((l) => /^jobs:\s*$/.test(l));
  if (jobsIdx === -1) return new Map();

  const jobs = new Map();
  let currentName = null;
  let currentLines = [];

  const flush = () => {
    if (currentName) jobs.set(currentName, currentLines.join("\n"));
    currentLines = [];
  };

  for (let i = jobsIdx + 1; i < lines.length; i++) {
    const line = lines[i];
    const jobHeaderMatch = /^ {2}([A-Za-z0-9_-]+):\s*$/.exec(line);
    if (jobHeaderMatch) {
      flush();
      currentName = jobHeaderMatch[1];
      continue;
    }
    if (currentName) currentLines.push(line);
  }
  flush();

  return jobs;
}

/**
 * Check 9: within one job block, does it declare `environment: release`, a
 * checkout step with `ref: ${{ needs.admission.outputs.source_revision }}`,
 * and `fetch-depth: 0`? Returns { hasReleaseEnv, hasAdmittedRef, hasFetchDepthZero }.
 */
export function analyzeJobForFetchDepth(jobBlockText) {
  const hasReleaseEnv = /^\s*environment:\s*release\s*$/m.test(jobBlockText);
  const hasAdmittedRef = /ref:\s*\$\{\{\s*needs\.admission\.outputs\.source_revision\s*\}\}/.test(
    jobBlockText
  );
  const hasFetchDepthZero = /fetch-depth:\s*0\s*$/m.test(jobBlockText);
  return { hasReleaseEnv, hasAdmittedRef, hasFetchDepthZero };
}

/**
 * Check 11: parse git hook file content for lines that declare a path
 * dependency (`checker=`, `gate=`, or an absolute path token) and return the
 * list of referenced paths to validate for existence.
 */
export function extractHookPathReferences(hookText) {
  const refs = [];
  const lines = hookText.split(/\r?\n/);
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;

    const kv = /^(?:export\s+)?(?:checker|gate)=["']?([^"'\s]+)["']?/.exec(trimmed);
    if (kv) {
      refs.push(kv[1]);
      continue;
    }

    // Absolute path token anywhere on the line (POSIX or Windows-drive style).
    const absRe = /(?:[A-Za-z]:[\\/][^\s"'`]+|\/(?:[^\s"'`]+\/)*[^\s"'`]+\.[a-zA-Z0-9]+)/g;
    let m;
    while ((m = absRe.exec(trimmed))) {
      refs.push(m[0]);
    }
  }
  return [...new Set(refs)];
}

/** Check 12: does the diff-name list include a workflow file? */
export function touchesWorkflows(changedPaths) {
  return changedPaths.some((p) => p.startsWith(".github/workflows/"));
}

/**
 * Check 14 (local-ci-command): from a `scripts/ci/run-ci.sh`-style one-CI-command
 * script, return the lines to run locally before a push — those starting with
 * `node ` or `pnpm test` (an optional `VAR=value ` prefix is allowed). Lines that
 * install dependencies or invoke `cargo`/`rightkit` are skipped: the local
 * preflight has no toolchain and RightKit forbids cargo on the public repos.
 * Comments and blank lines are ignored.
 */
export function parseCiCommandLines(scriptText) {
  const lines = [];
  for (const raw of scriptText.split(/\r?\n/)) {
    const line = raw.replace(/#.*$/, "").trim();
    if (!line) continue;
    if (/\b(?:cargo|rightkit)\b/.test(line)) continue;
    if (/\b(?:pnpm|npm|yarn)\s+(?:install|i|add)\b/.test(line)) continue;
    const body = line.replace(/^(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)+/, "");
    if (/^node\s/.test(body) || /^pnpm\s+test\b/.test(body)) lines.push(line);
  }
  return lines;
}

// ---------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------

function check(id, status, detail) {
  return { id, status, detail };
}

// 1. sha-format
function checkShaFormat(sha) {
  const ok = /^[0-9a-f]{40}$/.test(sha);
  return check(
    "sha-format",
    ok ? "PASS" : "FAIL",
    ok ? "40 lowercase hex characters" : `not 40 lowercase hex characters: "${sha}"`
  );
}

// 2. sha-is-branch-head
function checkShaIsBranchHead(repo, sha) {
  const res = runGh(["api", `repos/${repo}/branches/main`, "--jq", ".commit.sha"]);
  if (!res.ok) {
    return check("sha-is-branch-head", "FAIL", `gh api branches/main failed: ${res.stderr}`);
  }
  const headSha = res.stdout.trim();
  const ok = headSha === sha;
  return check(
    "sha-is-branch-head",
    ok ? "PASS" : "FAIL",
    ok
      ? `sha matches main HEAD (${headSha})`
      : `sha ${sha} does not match main HEAD ${headSha} — source_revision would lag main (cf. Membrane run 33584512141)`
  );
}

// 3. local-checkout-matches
function checkLocalCheckoutMatches(checkout, sha) {
  const rev = runGit(["rev-parse", "HEAD"], checkout);
  if (!rev.ok) {
    return check("local-checkout-matches", "FAIL", `git rev-parse HEAD failed in ${checkout}: ${rev.stderr}`);
  }
  const localHead = rev.stdout.trim();
  if (localHead !== sha) {
    return check(
      "local-checkout-matches",
      "FAIL",
      `local HEAD ${localHead} != requested sha ${sha} in ${checkout}`
    );
  }
  const status = runGit(["status", "--porcelain"], checkout);
  if (!status.ok) {
    return check("local-checkout-matches", "FAIL", `git status failed in ${checkout}: ${status.stderr}`);
  }
  if (status.stdout.length > 0) {
    return check(
      "local-checkout-matches",
      "WARN",
      `local HEAD matches sha but checkout is dirty (${status.stdout.split(/\r?\n/).length} entries)`
    );
  }
  return check("local-checkout-matches", "PASS", `local HEAD matches sha and checkout is clean`);
}

// 4. ci-green-at-sha
function checkCiGreenAtSha(repo, sha) {
  const res = runGh([
    "run",
    "list",
    "-R",
    repo,
    "--commit",
    sha,
    "--json",
    "workflowName,status,conclusion",
  ]);
  if (!res.ok) {
    return check("ci-green-at-sha", "FAIL", `gh run list failed: ${res.stderr}`);
  }
  let runs;
  try {
    runs = JSON.parse(res.stdout);
  } catch (e) {
    return check("ci-green-at-sha", "FAIL", `could not parse gh run list output: ${e.message}`);
  }
  const ciRuns = runs.filter((r) => (r.workflowName || "").includes("-ci"));
  const green = ciRuns.find((r) => r.status === "completed" && r.conclusion === "success");
  if (green) {
    return check("ci-green-at-sha", "PASS", `${green.workflowName} completed/success at ${sha}`);
  }
  if (ciRuns.length === 0) {
    return check("ci-green-at-sha", "FAIL", `no CI (*-ci) workflow run found at sha ${sha}`);
  }
  const summary = ciRuns.map((r) => `${r.workflowName}:${r.status}/${r.conclusion}`).join(", ");
  return check("ci-green-at-sha", "FAIL", `no green CI run at sha ${sha} — found: ${summary}`);
}

// 5. no-existing-release
function checkNoExistingRelease(repo, version) {
  const res = runGh(["release", "view", `v${version}`, "-R", repo]);
  if (res.ok) {
    return check("no-existing-release", "FAIL", `release v${version} already exists in ${repo}`);
  }
  const notFound = /release not found/i.test(res.stderr) || /release not found/i.test(res.stdout);
  if (notFound) {
    return check("no-existing-release", "PASS", `no existing release v${version} in ${repo}`);
  }
  return check(
    "no-existing-release",
    "FAIL",
    `gh release view failed unexpectedly (not a "release not found" error): ${res.stderr}`
  );
}

// 6. version-matches-source
function checkVersionMatchesSource(product, version) {
  try {
    const sourceVersion = product.readVersion(product._checkout);
    const ok = sourceVersion === version;
    return check(
      "version-matches-source",
      ok ? "PASS" : "FAIL",
      ok
        ? `${product.versionFile} version ${sourceVersion} matches requested ${version}`
        : `${product.versionFile} version ${sourceVersion} != requested ${version}`
    );
  } catch (e) {
    return check("version-matches-source", "FAIL", `could not read source version: ${e.message}`);
  }
}

// 7. release-env-allows-main
function checkReleaseEnvAllowsMain(repo) {
  const res = runGh([
    "api",
    `repos/${repo}/environments/release/deployment-branch-policies`,
    "--jq",
    ".branch_policies[].name",
  ]);
  if (!res.ok) {
    return check("release-env-allows-main", "FAIL", `gh api deployment-branch-policies failed: ${res.stderr}`);
  }
  const names = res.stdout.split(/\r?\n/).map((s) => s.trim()).filter(Boolean);
  const ok = names.includes("main");
  return check(
    "release-env-allows-main",
    ok ? "PASS" : "FAIL",
    ok
      ? `release environment allows branch "main"`
      : `release environment branch policies do not include "main" (got: ${names.join(", ") || "<none>"}) — cf. Legion run 33584504054`
  );
}

// 8. release-env-secrets
function checkReleaseEnvSecrets(repo, workflowText) {
  const { secrets, vars, varsWithDefault } = extractSecretsAndVars(workflowText);

  const secretsRes = runGh([
    "api",
    `repos/${repo}/environments/release/secrets`,
    "--jq",
    ".secrets[].name",
  ]);
  if (!secretsRes.ok) {
    return check("release-env-secrets", "FAIL", `gh api environments/release/secrets failed: ${secretsRes.stderr}`);
  }
  const availableSecrets = new Set(
    secretsRes.stdout.split(/\r?\n/).map((s) => s.trim()).filter(Boolean)
  );

  const envVarsRes = runGh([
    "api",
    `repos/${repo}/environments/release/variables`,
    "--jq",
    ".variables[].name",
  ]);
  const repoVarsRes = runGh(["api", `repos/${repo}/actions/variables`, "--jq", ".variables[].name"]);
  const availableVars = new Set(
    [
      ...(envVarsRes.ok ? envVarsRes.stdout.split(/\r?\n/) : []),
      ...(repoVarsRes.ok ? repoVarsRes.stdout.split(/\r?\n/) : []),
    ]
      .map((s) => s.trim())
      .filter(Boolean)
  );

  const missingSecrets = secrets.filter((s) => !availableSecrets.has(s));
  const missingVars = vars.filter((v) => !availableVars.has(v));

  if (missingSecrets.length === 0 && missingVars.length === 0) {
    return check(
      "release-env-secrets",
      "PASS",
      `all ${secrets.length} secrets and ${vars.length} non-defaulted vars present (${varsWithDefault.length} defaulted vars skipped)`
    );
  }
  const parts = [];
  if (missingSecrets.length) parts.push(`missing secrets: ${missingSecrets.join(", ")}`);
  if (missingVars.length) parts.push(`missing vars: ${missingVars.join(", ")}`);
  return check("release-env-secrets", "FAIL", parts.join("; "));
}

// 9. protected-jobs-fetch-depth
function checkProtectedJobsFetchDepth(workflowText, product = { usesRightReleaseGuard: true }) {
  const jobs = splitJobBlocks(workflowText);
  const violations = [];
  let checked = 0;
  for (const [name, block] of jobs) {
    const { hasReleaseEnv, hasAdmittedRef, hasFetchDepthZero } = analyzeJobForFetchDepth(block);
    if (hasReleaseEnv && hasAdmittedRef) {
      checked++;
      if (!hasFetchDepthZero) violations.push(name);
    }
  }
  if (violations.length === 0) {
    return check(
      "protected-jobs-fetch-depth",
      "PASS",
      `${checked} protected admitted-ref job(s) all declare fetch-depth: 0`
    );
  }
  return check(
    "protected-jobs-fetch-depth",
    product.usesRightReleaseGuard ? "FAIL" : "WARN",
    `job(s) missing fetch-depth: 0 despite environment:release + admitted ref checkout: ${violations.join(", ")}` +
      (product.usesRightReleaseGuard ? " — cf. Legion run 33585519212" : " — advisory: this product's chain does not invoke the right-release guard")
  );
}

// 10. manifest-digests
function checkManifestDigests(checkout) {
  const scriptPath = join(checkout, "scripts/ci/check-runtime-language-manifest.mjs");
  if (!existsSync(scriptPath)) {
    return check("manifest-digests", "PASS", `no scripts/ci/check-runtime-language-manifest.mjs in ${checkout}; skipped`);
  }
  const res = runNode([scriptPath], checkout);
  if (res.ok) {
    return check("manifest-digests", "PASS", `check-runtime-language-manifest.mjs exited 0`);
  }
  return check(
    "manifest-digests",
    "FAIL",
    `check-runtime-language-manifest.mjs exited ${res.status}: ${res.stderr || res.stdout}`
  );
}

// 10b. repo-governance-ledgers: the scripts/ci/check-*.mjs ledger checks CI runs. Membrane CI failed
// twice on 2026-09-02 (UNCLASSIFIED_EXECUTABLE, STALE_GRAPH_MISSING_NODE) after a new tracked file;
// only toolchain-free checks run here.
function checkGovernanceLedgers(checkout) {
  const scripts = [
    "scripts/ci/check-invocation-graph.mjs",
    "scripts/ci/check-generated.mjs",
    "scripts/ci/check-adapt-ontology.mjs",
    "scripts/ci/check-native-contract-fixtures.mjs",
  ].filter((rel) => existsSync(join(checkout, rel)));
  if (scripts.length === 0) return check("repo-governance-ledgers", "PASS", "no ledger checks in this repo; skipped");
  const failed = [];
  for (const rel of scripts) {
    const res = runNode([join(checkout, rel)], checkout);
    if (!res.ok) {
      const firstError = String(res.stderr || res.stdout).split(String.fromCharCode(10)).map((l) => l.trimEnd()).find((l) => /ERROR/.test(l));
      failed.push(`${rel}: ${firstError || `exit ${res.status}`}`);
    }
  }
  if (failed.length === 0) return check("repo-governance-ledgers", "PASS", `${scripts.length} ledger check(s) exited 0`);
  return check("repo-governance-ledgers", "FAIL", failed.join(" | "));
}

// 11. git-hooks-resolve
function checkGitHooksResolve(checkout) {
  const hooksDirRes = runGit(["rev-parse", "--git-path", "hooks"], checkout);
  if (!hooksDirRes.ok) {
    return check("git-hooks-resolve", "FAIL", `git rev-parse --git-path hooks failed: ${hooksDirRes.stderr}`);
  }
  const hooksDir = join(checkout, hooksDirRes.stdout.trim());
  const hookNames = ["pre-commit", "pre-push", "commit-msg"];
  const problems = [];
  let inspected = 0;

  for (const name of hookNames) {
    const hookPath = join(hooksDir, name);
    if (!existsSync(hookPath)) continue;
    let st;
    try {
      st = statSync(hookPath);
    } catch {
      continue;
    }
    // On win32 there is no execute bit; treat any present hook file as active.
    const isExecutableOrWindows = process.platform === "win32" || (st.mode & 0o111) !== 0;
    if (!isExecutableOrWindows) continue;

    inspected++;
    const text = readFileSync(hookPath, "utf8");
    const refs = extractHookPathReferences(text);
    for (const ref of refs) {
      if (!existsSync(ref)) {
        problems.push(`${name}: missing referenced path "${ref}"`);
      }
    }
  }

  if (problems.length === 0) {
    return check("git-hooks-resolve", "PASS", `${inspected} hook(s) inspected, all referenced paths resolve`);
  }
  return check(
    "git-hooks-resolve",
    "FAIL",
    `${problems.join("; ")} — cf. stray Cortex pre-push hooks blocking pushes`
  );
}

// 12. workflow-scope-token
function checkWorkflowScopeToken(checkout, repo, sha) {
  const headRes = runGh(["api", `repos/${repo}/branches/main`, "--jq", ".commit.sha"]);
  if (!headRes.ok) {
    return check("workflow-scope-token", "WARN", `could not determine remote head to diff against: ${headRes.stderr}`);
  }
  const remoteHead = headRes.stdout.trim();
  if (remoteHead === sha) {
    return check("workflow-scope-token", "PASS", `sha is remote head; no diff range to inspect`);
  }
  const diffRes = runGit(["diff", "--name-only", `${remoteHead}..${sha}`], checkout);
  if (!diffRes.ok) {
    return check("workflow-scope-token", "WARN", `git diff failed: ${diffRes.stderr}`);
  }
  const changed = diffRes.stdout.split(/\r?\n/).filter(Boolean);
  const touches = touchesWorkflows(changed);
  if (touches && process.platform === "win32") {
    return check(
      "workflow-scope-token",
      "WARN",
      `diff touches .github/workflows/ and platform is win32 — the Windows GH_TOKEN lacks "workflow" scope; push must go through the Mac`
    );
  }
  return check("workflow-scope-token", "PASS", `no workflow-scope concern (touches=${touches}, platform=${process.platform})`);
}

// 13. dry-run-receipt: rule 3 (release workflows have a dry run). The first real
// dispatch must never be the first execution — a completed, successful
// release-candidate run with a `-dry-run` artifact must exist within 7 days for
// the EXACT candidate workflow-file blob, so a workflow edit forces a fresh
// rehearsal.
function checkDryRunReceipt(spec, checkout, sha) {
  const workflowRel = ".github/workflows/release-candidate.yml";
  const candidateBlob = runGit(["rev-parse", `${sha}:${workflowRel}`], checkout);
  if (!candidateBlob.ok) {
    return check("dry-run-receipt", "FAIL", `could not resolve candidate workflow blob at ${sha}: ${candidateBlob.stderr}`);
  }
  const wantBlob = candidateBlob.stdout.trim();
  const res = runGh([
    "run", "list", "-R", spec.repo, "--workflow", "release-candidate.yml", "--limit", "30",
    "--json", "databaseId,conclusion,status,headSha,createdAt",
  ]);
  if (!res.ok) {
    return check("dry-run-receipt", "FAIL", `gh run list --workflow release-candidate.yml failed: ${res.stderr}`);
  }
  let runs;
  try {
    runs = JSON.parse(res.stdout);
  } catch (e) {
    return check("dry-run-receipt", "FAIL", `could not parse gh run list output: ${e.message}`);
  }
  const cutoff = Date.now() - 7 * 24 * 60 * 60 * 1000;
  const candidates = runs.filter(
    (r) => r.status === "completed" && r.conclusion === "success" && Date.parse(r.createdAt) >= cutoff
  );
  for (const r of candidates) {
    let blob = runGit(["rev-parse", `${r.headSha}:${workflowRel}`], checkout);
    if (!blob.ok) {
      runGit(["fetch", "origin", r.headSha], checkout);
      blob = runGit(["rev-parse", `${r.headSha}:${workflowRel}`], checkout);
    }
    if (!blob.ok || blob.stdout.trim() !== wantBlob) continue;
    const arts = runGh(["api", `repos/${spec.repo}/actions/runs/${r.databaseId}/artifacts`]);
    if (!arts.ok) continue;
    let parsed;
    try {
      parsed = JSON.parse(arts.stdout);
    } catch {
      continue;
    }
    if ((parsed.artifacts || []).some((a) => String(a.name).includes("-dry-run"))) {
      return check("dry-run-receipt", "PASS", `run ${r.databaseId} is a completed dry run for candidate workflow blob ${wantBlob}`);
    }
  }
  return check(
    "dry-run-receipt",
    "FAIL",
    `no completed successful release-candidate run within 7 days matches candidate workflow blob ${wantBlob} with a "-dry-run" artifact — dispatch release-candidate with dry_run=true from this sha's branch first`
  );
}

// 14. local-ci-command: rule 1 (one CI command, identical everywhere). Every
// `node`/`pnpm test` line in scripts/ci/run-ci.sh must exit 0 locally, with
// RIGHT_RELEASE_OFFLINE=1, so green locally means green in CI.
function checkLocalCiCommand(checkout) {
  const scriptPath = join(checkout, "scripts/ci/run-ci.sh");
  if (!existsSync(scriptPath)) {
    return check("local-ci-command", "PASS", `no scripts/ci/run-ci.sh in ${checkout}; skipped`);
  }
  const lines = parseCiCommandLines(readFileSync(scriptPath, "utf8"));
  if (lines.length === 0) {
    return check("local-ci-command", "FAIL", "scripts/ci/run-ci.sh yielded no runnable node/pnpm test lines");
  }
  const failed = [];
  for (const line of lines) {
    const res = spawnSync("bash", ["-c", line], {
      cwd: checkout,
      encoding: "utf8",
      windowsHide: true,
      env: { ...process.env, RIGHT_RELEASE_OFFLINE: "1" },
    });
    if (res.status !== 0) {
      failed.push(`"${line}" exited ${res.status}: ${String(res.stderr || res.stdout).split(/\r?\n/).slice(-3).join(" ")}`);
    }
  }
  if (failed.length) {
    return check("local-ci-command", "FAIL", failed.join(" | "));
  }
  return check("local-ci-command", "PASS", `${lines.length} CI command line(s) from run-ci.sh exited 0`);
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const args = { _: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--checkout") {
      args.checkout = argv[++i];
    } else if (a === "--json") {
      args.json = true;
    } else {
      args._.push(a);
    }
  }
  return args;
}

function main() {
  const argv = process.argv.slice(2);
  const args = parseArgs(argv);
  const [product, version, sha] = args._;

  if (!product || !version || !sha || !(product in PRODUCTS)) {
    process.stderr.write(
      "usage: node preflight.mjs <legion|membrane> <version> <sha> [--checkout <path>] [--json]\n"
    );
    process.exit(1);
  }

  const spec = PRODUCTS[product];
  const checkout = args.checkout || spec.defaultCheckout;
  spec._checkout = checkout;

  const workflowPath = join(checkout, spec.workflowFile);
  let workflowText = "";
  let workflowReadError = null;
  try {
    workflowText = readFileSync(workflowPath, "utf8");
  } catch (e) {
    workflowReadError = e;
  }

  const results = [];

  results.push(checkShaFormat(sha));

  // sha-format gates every check that assumes a well-formed sha.
  if (results[0].status === "PASS") {
    results.push(checkShaIsBranchHead(spec.repo, sha));
    results.push(checkLocalCheckoutMatches(checkout, sha));
    results.push(checkCiGreenAtSha(spec.repo, sha));
  } else {
    results.push(check("sha-is-branch-head", "FAIL", "skipped: sha-format failed"));
    results.push(check("local-checkout-matches", "FAIL", "skipped: sha-format failed"));
    results.push(check("ci-green-at-sha", "FAIL", "skipped: sha-format failed"));
  }

  results.push(checkNoExistingRelease(spec.repo, version));
  results.push(checkVersionMatchesSource(spec, version));
  results.push(checkReleaseEnvAllowsMain(spec.repo));

  if (workflowReadError) {
    results.push(check("release-env-secrets", "FAIL", `could not read ${workflowPath}: ${workflowReadError.message}`));
    results.push(check("protected-jobs-fetch-depth", "FAIL", `could not read ${workflowPath}: ${workflowReadError.message}`));
  } else {
    results.push(checkReleaseEnvSecrets(spec.repo, workflowText));
    results.push(checkProtectedJobsFetchDepth(workflowText, product));
  }

  results.push(checkManifestDigests(checkout));
  results.push(checkGovernanceLedgers(checkout));
  results.push(checkGitHooksResolve(checkout));
  results.push(checkWorkflowScopeToken(checkout, spec.repo, sha));

  if (results[0].status === "PASS") {
    results.push(checkDryRunReceipt(spec, checkout, sha));
  } else {
    results.push(check("dry-run-receipt", "FAIL", "skipped: sha-format failed"));
  }
  results.push(checkLocalCiCommand(checkout));

  const failures = results.filter((r) => r.status === "FAIL").length;
  const passed = failures === 0;

  if (args.json) {
    process.stdout.write(
      JSON.stringify(
        {
          product,
          version,
          sha,
          checkout,
          results,
          summary: passed ? "PREFLIGHT PASS" : `PREFLIGHT FAIL (${failures} failures)`,
        },
        null,
        2
      ) + "\n"
    );
  } else {
    for (const r of results) {
      process.stdout.write(`${r.status} ${r.id} ${r.detail}\n`);
    }
    process.stdout.write(passed ? "PREFLIGHT PASS\n" : `PREFLIGHT FAIL (${failures} failures)\n`);
  }

  process.exit(passed ? 0 : 1);
}

// Only run when invoked directly (not when imported by the test file).
const isMain = process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1];
if (isMain) {
  main();
}
