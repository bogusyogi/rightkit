import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const release = fileURLToPath(new URL("./release.mjs", import.meta.url));
const cli = fileURLToPath(new URL("./cli/right-release.mjs", import.meta.url));
const versions = JSON.parse(readFileSync(new URL("./rightkit-versions.json", import.meta.url), "utf8"));
const defaultBuildInputs = { include: ["package.json", "pnpm-lock.yaml", "src-tauri/**"] };

function git(cwd, ...args) {
  return execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}

function fixture({ signed = true, publish = false, packageJson, cargoManifest, cargoConfig, repoCargoConfig, siblingCargoManifest, buildInputs = defaultBuildInputs } = {}) {
  const fixtureRoot = mkdtempSync(path.join(os.tmpdir(), "right-release-test-"));
  const dir = repoCargoConfig ? path.join(fixtureRoot, "apps", "fixture") : fixtureRoot;
  mkdirSync(dir, { recursive: true });
  const config = path.join(dir, "right-release.config.mjs");
  writeFileSync(
    path.join(dir, "package.json"),
    JSON.stringify(
      packageJson ?? {
        name: "fixture",
        scripts: { "release:patch:win": "right-release --platform win --tier patch" },
        packageManager: versions.packageManager,
        dependencies: {
          "@rightkit/license": versions.npm["@rightkit/license"],
          "@rightkit/logs": versions.npm["@rightkit/logs"],
        },
        devDependencies: { "@rightkit/release": versions.npm["@rightkit/release"] },
      },
      null,
      2,
    ),
  );
  writeFileSync(
    config,
    `export default ${JSON.stringify({
      schema: 1,
      app: "fixture",
      packageManager: "pnpm",
      checks: [],
      targets: {
        win: {
          ...(buildInputs ? { buildInputs } : {}),
          signed,
          package: { cmd: "node", args: ["-e", "process.exit(0)"] },
          ...(publish ? { publish: { cmd: "node", args: ["publish-update.mjs"] } } : {}),
          artifacts: [],
          sign: { files: ["fixture.exe"] },
          updater: { artifacts: [{ file: "fixture.exe", signature: "fixture.exe.sig", platform: "windows-x86_64", key: "fixture/fixture.exe" }] },
          hardening: [],
        },
      },
    })};\n`,
  );
  writeFileSync(path.join(dir, "pnpm-lock.yaml"), "lockfileVersion: '9.0'\n", "utf8");
  const tauriDir = path.join(dir, "src-tauri");
  mkdirSync(tauriDir, { recursive: true });
  writeFileSync(path.join(tauriDir, "Cargo.toml"), `[package]\nname = "fixture-tauri"\nversion = "0.0.0"\nedition = "2021"\n[lib]\npath = "lib.rs"\n`, "utf8");
  writeFileSync(path.join(tauriDir, "Cargo.lock"), "# fixture lock\n", "utf8");
  writeFileSync(path.join(tauriDir, "lib.rs"), "pub fn fixture() {}\n", "utf8");
  if (cargoManifest) {
    writeFileSync(path.join(dir, "Cargo.toml"), `${cargoManifest}\n[lib]\npath = "fixture.rs"\n`, "utf8");
    writeFileSync(path.join(dir, "fixture.rs"), "", "utf8");
  }
  if (cargoConfig) {
    const cargoDir = path.join(dir, ".cargo");
    mkdirSync(cargoDir, { recursive: true });
    writeFileSync(path.join(cargoDir, "config.toml"), cargoConfig, "utf8");
  }
  if (repoCargoConfig) {
    const cargoDir = path.join(fixtureRoot, ".cargo");
    mkdirSync(cargoDir, { recursive: true });
    writeFileSync(path.join(cargoDir, "config.toml"), repoCargoConfig, "utf8");
  }
  if (siblingCargoManifest) {
    const sibling = path.join(fixtureRoot, "docs", "archived-workspace");
    mkdirSync(sibling, { recursive: true });
    writeFileSync(path.join(sibling, "Cargo.toml"), siblingCargoManifest, "utf8");
  }
  git(fixtureRoot, "init", "--initial-branch", "main");
  git(fixtureRoot, "config", "user.email", "release-test@example.com");
  git(fixtureRoot, "config", "user.name", "Release Test");
  git(fixtureRoot, "add", ".");
  git(fixtureRoot, "commit", "-m", "initial");
  return config;
}

function lockFixture() {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-release-lock-test-"));
  const config = path.join(dir, "right-release.config.mjs");
  writeFileSync(
    path.join(dir, "package.json"),
    JSON.stringify(
      {
        name: "fixture-lock",
        scripts: { "release:patch:mac": "right-release --platform mac --tier patch" },
        packageManager: versions.packageManager,
        dependencies: {
          "@rightkit/license": versions.npm["@rightkit/license"],
          "@rightkit/logs": versions.npm["@rightkit/logs"],
        },
        devDependencies: { "@rightkit/release": versions.npm["@rightkit/release"] },
      },
      null,
      2,
    ),
  );
  writeFileSync(
    config,
    `export default ${JSON.stringify({
      schema: 1,
      app: "fixture-lock",
      packageManager: "pnpm",
      checks: [],
      targets: {
        mac: {
          signed: true,
          package: { cmd: "node", args: ["-e", "process.exit(0)"] },
          artifacts: [],
          hardening: [],
          installer: { artifacts: [] },
          updater: { artifacts: [] },
        },
      },
    })};\n`,
  );
  return { dir, config };
}

function run(config, ...args) {
  return spawnSync(process.execPath, [release, "--config", config, "--platform", "win", "--dry-run", ...args], {
    encoding: "utf8",
  });
}

function runRaw(config, ...args) {
  return spawnSync(process.execPath, [release, "--config", config, ...args], {
    encoding: "utf8",
  });
}

function runDoctor(config, ...args) {
  return spawnSync(process.execPath, [cli, "doctor", "--config", config, "--platform", "win", ...args], {
    encoding: "utf8",
  });
}

test("accepts a tier-neutral internal build", () => {
  const result = run(fixture());
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /tier=neutral/);
  assert.doesNotMatch(result.stdout, /RIGHT_RELEASE_TIER=/);
});

test("accepts patch and exposes it to the signed package command", () => {
  const result = run(fixture(), "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /tier=patch/);
  assert.match(result.stdout, /RIGHT_RELEASE_TIER=patch/);
});

test("accepts update as the Pro-gated feature tier", () => {
  const result = run(fixture(), "--tier", "update");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /tier=update/);
  assert.match(result.stdout, /RIGHT_RELEASE_TIER=update/);
});

test("rejects an invalid entitlement tier", () => {
  const result = run(fixture(), "--tier=minor");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /invalid --tier.*patch\|update/i);
});

test("rejects targets that do not explicitly declare signed release output", () => {
  const result = run(fixture({ signed: false }), "--tier=patch");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /signed: true/);
});

test("rejects Git, path, or link RightKit app dependencies before release work starts", () => {
  const result = run(
    fixture({
      packageJson: {
        name: "fixture",
        packageManager: versions.packageManager,
        scripts: { "release:patch:win": "node ../tools/right-release/release.mjs --tier patch" },
        dependencies: {
          "@rightkit/license": "git+https://github.com/adrdsouza/rightkit.git#main",
          "@rightkit/logs": "link:../../../rightkit/packages/logs",
        },
        devDependencies: {
          "@rightkit/release": "github:adrdsouza/claude#main&path:/tools/right-release",
        },
      },
    }),
    "--tier=patch",
  );
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /right-release bin|published npm package/i);
});

test("rejects hosted workflow files before release work starts", () => {
  const config = fixture();
  const workflowDir = path.join(path.dirname(config), ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(workflowDir, "ci.yml"), "name: forbidden\n");

  const result = run(config, "--tier=patch");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /hosted workflow.*forbidden/i);
});

test("doctor accepts an exact crates.io RightKit pin with benign Cargo config", () => {
  const config = fixture({
    cargoManifest: `[package]\nname = "fixture"\nversion = "0.0.0"\nedition = "2021"\n[dependencies]\nrightkit-license = "=0.1.2"`,
    cargoConfig: `[net]\nretry = 2\n[registries.private]\nindex = "https://example.test/index"`,
  });
  const result = run(config, "--doctor");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /right-release/);
});

test("doctor rejects a selected target with missing or empty buildInputs.include", () => {
  for (const buildInputs of [null, { include: [] }]) {
    const result = run(fixture({ buildInputs }), "--doctor");
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /non-empty buildInputs\.include paths/i);
  }
});

test("doctor passes a clean selected target", () => {
  const result = runDoctor(fixture());
  assert.equal(result.status, 0, result.stderr);
  // Derived from the version manifest, not hardcoded: a literal here has to be
  // hand-edited on every bump and fails the suite when someone forgets.
  const expected = versions.stagedNpm?.["@rightkit/release"] ?? versions.npm["@rightkit/release"];
  assert.match(result.stdout, new RegExp(`right-release ${expected.replace(/\./g, "\\.")}`));
});

test("doctor permits unrelated dirt", () => {
  const config = fixture();
  const root = path.dirname(config);
  mkdirSync(path.join(root, "notes"));
  writeFileSync(path.join(root, "notes", "scratch.md"), "unrelated\n");
  const result = run(config, "--doctor");
  assert.equal(result.status, 0, result.stderr);
});

test("doctor rejects tracked dirt inside declared buildInputs with the exact path", () => {
  const config = fixture();
  writeFileSync(path.join(path.dirname(config), "src-tauri", "lib.rs"), "pub fn changed() {}\n");
  const result = run(config, "--doctor");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /dirty files can change the packaged app[\s\S]*- src-tauri\/lib\.rs/i);
});

test("doctor rejects untracked dirt inside declared buildInputs with the exact path", () => {
  const config = fixture();
  writeFileSync(path.join(path.dirname(config), "src-tauri", "new-runtime.rs"), "pub fn new_runtime() {}\n");
  const result = run(config, "--doctor");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /dirty files can change the packaged app[\s\S]*- src-tauri\/new-runtime\.rs/i);
});

test("doctor rejects dirty required release metadata even when it is not declared", () => {
  const config = fixture({ buildInputs: { include: ["src-tauri/lib.rs"] } });
  writeFileSync(path.join(path.dirname(config), "pnpm-lock.yaml"), "lockfileVersion: '9.1'\n");
  const result = run(config, "--doctor");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /dirty files can change the packaged app[\s\S]*- pnpm-lock\.yaml/i);
});

test("doctor rejects a dirty release config before importing it", () => {
  const config = fixture();
  writeFileSync(config, `${readFileSync(config, "utf8")}\n// dirty config\n`);
  const result = run(config, "--doctor");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /dirty release config cannot be executed[\s\S]*- right-release\.config\.mjs/i);
});

test("doctor rejects linked and detached checkouts like build runtime", () => {
  const primaryConfig = fixture();
  const primary = path.dirname(primaryConfig);
  const linked = `${primary}-linked`;
  git(primary, "worktree", "add", "--detach", linked);
  const linkedResult = run(path.join(linked, "right-release.config.mjs"), "--doctor");
  assert.notEqual(linkedResult.status, 0);
  assert.match(linkedResult.stderr, /must be invoked from the primary Git worktree/i);

  git(primary, "checkout", "--detach");
  const detachedResult = run(primaryConfig, "--doctor");
  assert.notEqual(detachedResult.status, 0);
  assert.match(detachedResult.stderr, /detached HEAD is forbidden/i);
});

test("doctor scopes Cargo inspection to the configured app inside a monorepo", () => {
  const config = fixture({
    cargoManifest: `[package]\nname = "fixture"\nversion = "0.0.0"\nedition = "2021"\n[dependencies]\nrightkit-license = "=0.1.2"`,
    repoCargoConfig: `[net]\nretry = 2`,
    siblingCargoManifest: `[package]\nname = "archived-copy"\nversion = "0.0.0"\n[dependencies]\nrightkit-license = { path = "../../rightkit-license" }`,
  });
  const result = run(config, "--doctor");
  assert.equal(result.status, 0, result.stderr);
});

test("doctor rejects a repo-local RightKit Cargo override", () => {
  const config = fixture({
    cargoManifest: `[package]\nname = "fixture"\nversion = "0.0.0"\nedition = "2021"\n[dependencies]\nrightkit-license = "=0.1.1"`,
    cargoConfig: `[patch.crates-io]\nrightkit-license = { path = "../rightkit-license" }`,
  });
  const result = run(config, "--doctor");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /forbidden RightKit Cargo override/i);
});

test("doctor rejects a RightKit override at an ancestor repository root", () => {
  const config = fixture({
    cargoManifest: `[package]\nname = "fixture"\nversion = "0.0.0"\nedition = "2021"\n[dependencies]\nrightkit-license = "=0.1.1"`,
    repoCargoConfig: `[patch.crates-io]\nrightkit-license = { git = "https://example.test/rightkit-license.git" }`,
  });
  const result = run(config, "--doctor");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /forbidden RightKit Cargo override/i);
});

test("combined build and upload is rejected before the package step", () => {
  const result = run(fixture({ publish: true }), "--tier=update", "--upload");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /combined build\+upload was removed/i);
  assert.doesNotMatch(result.stdout, /node -e process\.exit\(0\)/);
});

test("Windows tier-neutral build signs the installer then minisigns the updater payload", () => {
  const result = run(fixture({ publish: true }));
  assert.equal(result.status, 0, result.stderr);
  const azureAt = result.stdout.indexOf("sign-windows.mjs");
  const updaterAt = result.stdout.indexOf("sign-updater.mjs");
  assert.ok(azureAt >= 0, result.stdout);
  assert.ok(updaterAt > azureAt, result.stdout);
  assert.doesNotMatch(result.stdout, /publish-update\.mjs/);
});

test("legacy internal tier does not make the build worker upload", () => {
  const result = run(fixture({ publish: true }), "--tier=update");
  assert.equal(result.status, 0, result.stderr);
  const azureAt = result.stdout.indexOf("sign-windows.mjs");
  const updaterAt = result.stdout.indexOf("sign-updater.mjs");
  assert.ok(azureAt >= 0, result.stdout);
  assert.ok(updaterAt > azureAt, result.stdout);
  assert.doesNotMatch(result.stdout, /publish-update\.mjs/);
});

test("release sweeps stale rust artifacts when a target dir exists", () => {
  const config = fixture();
  const dir = path.dirname(config);
  mkdirSync(path.join(dir, "src-tauri", "target"), { recursive: true });
  const result = run(config, "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /dry-run: cargo sweep --installed .*src-tauri/);
});

test("release skips the sweep when no target dir exists", () => {
  const result = run(fixture(), "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
  assert.doesNotMatch(result.stdout, /cargo sweep/);
});

test("release lock is single-flight and is cleaned after success", () => {
  const { dir, config } = lockFixture();
  const lockPath = path.join(dir, ".cache", "right-release", "mac.lock.json");
  const result = runRaw(config, "--platform", "mac", "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /right-release: done/);
  assert.equal(existsSync(lockPath), false);
});
