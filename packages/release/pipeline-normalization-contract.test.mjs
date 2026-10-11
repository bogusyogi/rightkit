import assert from "node:assert/strict";
import { execFileSync } from "./background-process.mjs";
import { existsSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const liveWorkspace = process.env.RIGHT_SUITE_CONTRACT_WORKSPACE;
// Long-form roots: git lists files under the long path, and labels are taken relative to these.
const fixtureRoot = realpathSync.native(mkdtempSync(path.join(os.tmpdir(), "rightkit-pipeline-contract-")));
const positiveRoot = realpathSync.native(mkdtempSync(path.join(os.tmpdir(), "rightkit-pipeline-positive-")));
const workspace = path.resolve(liveWorkspace ?? positiveRoot);

function writeFixture(relative, contents, root = fixtureRoot) {
  const file = path.join(root, relative);
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, contents);
  execFileSync("git", ["-C", root, "add", "-f", relative], { windowsHide: true });
}

const packageRoots = [
  "coderight/apps/coderight-tauri",
  "cutright/apps/studio",
  "genright",
  "heardright/tauri-app-next",
  "mailright",
  "membrane/apps/membrane-hub",
  "scraperight",
  "viewright",
  "voiceright",
];

const releaseConfigRoots = [
  ...packageRoots,
  "tools/screenright",
];

const rightGitRepos = ["cutright", "legion", "membrane"];
const privateRepos = ["rightsites", "sellright", "workright"];

function initializeFixture(root, compliant) {
  execFileSync("git", ["init", "-q", root], { windowsHide: true });
  for (const productRoot of packageRoots) mkdirSync(path.join(root, productRoot), { recursive: true });
  for (const relative of [
    "tools/pipelines/memory/dispatch/gate.py",
    "tools/pipelines/memory/install-cortex.py",
    "tools/pipelines/memory/verify-source-owner.py",
  ]) writeFixture(relative, "# deterministic compliant fixture\n", root);
  if (!compliant) return;
  for (const productRoot of packageRoots) {
    writeFixture(`${productRoot}/package.json`, JSON.stringify({ scripts: {
      build: "rightkit cargo build",
      verify: "pnpm exec rightkit verify",
    } }), root);
  }
  writeFixture("coderight/apps/coderight-tauri/right-release.config.mjs", 'import { resolveTargetRoot } from "@rightkit/release/cargo-target.mjs";\nresolveTargetRoot();\n', root);
  writeFixture("coderight/apps/coderight-tauri/scripts/build.mjs", 'execFileSync("rightkit", ["cargo", "build"]);\n', root);
  for (const repo of rightGitRepos) writeFixture(`${repo}/.github/workflows/ci.yml`, "# Managed by right-git\nname: CI\n", root);
}

initializeFixture(fixtureRoot, false);
initializeFixture(positiveRoot, true);

test.after(() => {
  rmSync(fixtureRoot, { recursive: true, force: true });
  rmSync(positiveRoot, { recursive: true, force: true });
});

function executableSource(source) {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split("\n")
    .filter((line) => !/^\s*(?:\/\/|#)/.test(line))
    .join("\n");
}

function pipelineScripts(root) {
  const repo = execFileSync("git", ["-C", root, "rev-parse", "--show-toplevel"], { encoding: "utf8", windowsHide: true }).trim();
  // Git prints the resolved root (macOS /private/var); compare against the resolved scan root.
  const prefix = path.relative(realpathSync(repo), realpathSync(root)).replaceAll(path.sep, "/");
  const tracked = execFileSync("git", ["-C", repo, "ls-files", "-z"], { encoding: "utf8", windowsHide: true })
    .split("\0")
    .filter(Boolean);
  return tracked
    .filter((entry) => {
      const local = prefix ? entry.slice(prefix.length + 1) : entry;
      if (prefix && !entry.startsWith(`${prefix}/`)) return false;
      return (
        local.startsWith("scripts/")
        || local === "package.ps1"
        || local === "package.sh"
      ) && /\.(?:mjs|cjs|js|ts|ps1|sh|py)$/i.test(local)
        && !/(?:^|[.-])test(?:[.-]|$)/i.test(path.basename(local));
    })
    // Rebuild paths from the caller's spelling of the root so labels stay workspace-relative.
    .map((entry) => path.join(root, prefix ? entry.slice(prefix.length + 1) : entry))
    .filter((entry) => existsSync(entry));
}

function packagePipelineViolations(workspaceRoot) {
  const violations = [];
  for (const root of packageRoots) {
    const manifestPath = path.join(workspaceRoot, root, "package.json");
    if (!existsSync(manifestPath)) continue;
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    for (const [name, command] of Object.entries(manifest.scripts ?? {})) {
      if (!/^(?:build|package|publish|release|deps|verify:rust|test:rust|lint:rust)/.test(name)) continue;
      if (/(?:\.\.\/[\w./-]*)?tools\/right-release|tools\\right-release/i.test(command)) violations.push(`${root} ${name}: retired workspace-local right-release source`);
      if (/(?:^|&&|\|\||;)\s*cargo\s+/i.test(command)) violations.push(`${root} ${name}: bare cargo`);
      if (/(?:^|&&|\|\||;)\s*npx\s+/i.test(command)) violations.push(`${root} ${name}: npx`);
      if (/(?:^|&&|\|\||;)\s*npm\s+(?:ci|install)\b/i.test(command)) violations.push(`${root} ${name}: npm install`);
    }
  }
  return violations;
}

test("product package pipelines use installed RightKit commands", () => {
  assert.deepEqual(packagePipelineViolations(workspace), []);
});

test("fixture detects retired package pipeline commands", () => {
  writeFixture("coderight/apps/coderight-tauri/package.json", JSON.stringify({ scripts: {
    build: "cargo build && npx right-release && npm install",
  } }));
  const violations = packagePipelineViolations(fixtureRoot);
  assert.ok(violations.includes(`${packageRoots[0]} build: bare cargo`));
  assert.ok(violations.includes(`${packageRoots[0]} build: npx`));
  assert.ok(violations.includes(`${packageRoots[0]} build: npm install`));
});

function releaseConfigViolations(workspaceRoot) {
  const violations = [];
  for (const root of releaseConfigRoots) {
    const configPath = path.join(workspaceRoot, root, "right-release.config.mjs");
    if (!existsSync(configPath)) continue;
    const source = executableSource(readFileSync(configPath, "utf8"));
    const hardcodedTargetRoot = /(?:const|let|var)\s+\w+\s*=\s*["'`]src-tauri[\\/]target/i.test(source);
    const hardcodedTargetArtifact = /(?:file|signature)\s*:\s*["'`]src-tauri[\\/]target/i.test(source);
    if (!hardcodedTargetRoot && !hardcodedTargetArtifact) continue;
    if (!/@rightkit\/release\/cargo-target\.mjs|\.\/scripts\/lib\/target-root\.mjs/.test(source)) violations.push(`${root}: no RightKit target resolver`);
    if (!/resolve(?:TargetRoot|WindowsReleaseDir)\s*\(/.test(source)) violations.push(`${root}: resolver not invoked`);
    if (hardcodedTargetRoot) violations.push(`${root}: hardcoded target root`);
    if (hardcodedTargetArtifact) violations.push(`${root}: hardcoded target artifact`);
  }
  return violations;
}

test("release configs resolve Cargo output through RightKit", () => {
  assert.deepEqual(releaseConfigViolations(workspace), []);
});

test("fixture identifies hardcoded release target paths", () => {
  const root = "coderight/apps/coderight-tauri";
  writeFixture(`${root}/right-release.config.mjs`, 'const target = "src-tauri/target";');
  const violations = releaseConfigViolations(fixtureRoot);
  assert.ok(violations.includes(`${root}: no RightKit target resolver`));
  assert.ok(violations.includes(`${root}: resolver not invoked`));
  assert.ok(violations.includes(`${root}: hardcoded target root`));
});

function pipelineViolations(workspaceRoot) {
  const violations = [];
  for (const root of packageRoots) {
    for (const file of pipelineScripts(path.join(workspaceRoot, root))) {
      const source = executableSource(readFileSync(file, "utf8"));
      const label = path.relative(workspaceRoot, file).replaceAll(path.sep, "/");
      if (/(?:spawnSync|spawn|execFileSync|execFile)\s*\(\s*["'`](?:cargo|rustc|rustdoc)["'`]/.test(source)) violations.push(`${label}: unmanaged JS child`);
      if (/subprocess\.(?:run|Popen)\s*\(\s*\[\s*["'](?:cargo|rustc|rustdoc)["']/.test(source)) violations.push(`${label}: unmanaged Python child`);
      if (/(?:^|\n)\s*(?:&\s*)?(?:cargo|rustc|rustdoc)\s+(?:build|check|clippy|metadata|test|-vV)\b/m.test(source)) violations.push(`${label}: unmanaged shell command`);
    }
  }
  return violations;
}

test("product pipeline children use managed Rust commands", () => {
  assert.deepEqual(pipelineViolations(workspace), []);
});

test("fixture tracks unmanaged Rust child invocations", () => {
  const relative = "coderight/apps/coderight-tauri/scripts/build.mjs";
  writeFixture(relative, 'spawnSync("cargo", ["build"]);');
  assert.ok(pipelineViolations(fixtureRoot).includes(`${relative}: unmanaged JS child`));
});

function workflowViolations(workspaceRoot) {
  const violations = [];
  for (const root of rightGitRepos) {
    const workflowRoot = path.join(workspaceRoot, root, ".github", "workflows");
    if (!existsSync(workflowRoot)) continue;
    for (const entry of readdirSync(workflowRoot, { withFileTypes: true })) {
      if (!entry.isFile() || !/\.ya?ml$/i.test(entry.name)) continue;
      const source = readFileSync(path.join(workflowRoot, entry.name), "utf8");
      if (!/^# Managed by right-git\b/.test(source)) violations.push(`${root}/.github/workflows/${entry.name}: not right-git managed`);
    }
  }
  for (const root of privateRepos) {
    const workflowRoot = path.join(workspaceRoot, root, ".github", "workflows");
    const workflows = existsSync(workflowRoot)
      ? readdirSync(workflowRoot).filter((entry) => /\.ya?ml$/i.test(entry))
      : [];
    if (workflows.length) violations.push(`${root}: private repo has ${workflows.join(", ")}`);
  }
  return violations;
}

test("hosted workflows are generated by right-git", () => {
  assert.deepEqual(workflowViolations(workspace), []);
});

test("fixture identifies unmanaged and private hosted workflows", () => {
  writeFixture("membrane/.github/workflows/ci.yml", "name: CI\n");
  writeFixture("rightsites/.github/workflows/release.yml", "name: Release\n");
  const violations = workflowViolations(fixtureRoot);
  assert.ok(violations.includes("membrane/.github/workflows/ci.yml: not right-git managed"));
  assert.ok(violations.includes("rightsites: private repo has release.yml"));
});

function memoryPipelineViolations(workspaceRoot) {
  const violations = [];
  for (const relative of [
    "tools/pipelines/memory/dispatch/gate.py",
    "tools/pipelines/memory/install-cortex.py",
    "tools/pipelines/memory/verify-source-owner.py",
  ]) {
    const source = executableSource(readFileSync(path.join(workspaceRoot, relative), "utf8"));
    if (/(?:^|&&|\|\||;)\s*cargo\s+(?:metadata|build|check|clippy|test)\b/m.test(source)) violations.push(`${relative}: bare Cargo shell command`);
    if (/subprocess\.run\(\s*\[\s*["']cargo["']/m.test(source)) violations.push(`${relative}: bare Cargo subprocess`);
  }
  return violations;
}

test("workspace memory pipelines route Rust through RightKit", () => {
  assert.deepEqual(memoryPipelineViolations(workspace), []);
});

test("fixture catches bare Cargo in workspace memory pipelines", () => {
  const relative = "tools/pipelines/memory/dispatch/gate.py";
  writeFixture(relative, 'subprocess.run(["cargo", "metadata"])\n');
  assert.ok(memoryPipelineViolations(fixtureRoot).includes(`${relative}: bare Cargo subprocess`));
});
