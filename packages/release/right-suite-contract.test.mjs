import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";

const workspace = path.resolve(new URL("../../../..", import.meta.url).pathname.replace(/^\/(\w:)/, "$1"));
const versionsPath = path.join(path.dirname(new URL(import.meta.url).pathname.replace(/^\/(\w:)/, "$1")), "rightkit-versions.json");
const versions = JSON.parse(readFileSync(versionsPath, "utf8"));
const pubkey = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDI5Mzk1RjlGRjQ2NjI2MUQKUldRZEptYjBuMTg1S1VSUXlBdFM4WmtzaHArYko0U2hRMDVlSDJmSExVZG82Q0hoQ2srUlhqanAK";
const apps = [
  { key: "viewright", root: "viewright", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["scripts/build-mac-notarized.sh"] },
  { key: "scraperight", root: "scraperight", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["package.sh"] },
  { key: "heardright", root: "heardright/tauri-app-next", repoRoot: "heardright", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["scripts/mac-dmg.mjs", "scripts/publish-release.mjs"] },
  { key: "mailright", root: "mailright", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["scripts/build-mac.sh"] },
  { key: "coderight", root: "coderight/apps/coderight-tauri", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["scripts/mac-dmg.mjs"] },
];

test("RightKit exposes one current version manifest", () => {
  assert.equal(existsSync(versionsPath), true, "rightkit-versions.json must be the single current-version source");
  assert.match(versions.packageManager, /^pnpm@\d+\.\d+\.\d+$/);
  assert.equal(versions.npm["@rightkit/release"], "0.2.18");
  assert.equal(versions.npm["@rightkit/license"], "0.1.5");
  assert.equal(versions.npm["@rightkit/logs"], "0.1.3");
  assert.equal(versions.npm["@rightkit/updates"], "0.2.2");
  assert.equal(versions.cargo["rightkit-license"], "0.1.1");
  assert.equal(versions.cargo["rightkit-logs"], "0.1.0");
  for (const [crate, version] of Object.entries(versions.cargo)) {
    const manifest = readFileSync(path.join(workspace, `tools/rightkit/crates/${crate}/Cargo.toml`), "utf8");
    assert.match(manifest, new RegExp(`^version\\s*=\\s*"${version.replaceAll(".", "\\.")}"$`, "m"));
  }
});

test("license v2 public vector is identical at every portable consumer boundary", () => {
  const canonical = readFileSync(
    path.join(workspace, "tools/rightkit/crates/rightkit-license/test-vectors/license-v2.json"),
    "utf8",
  );
  for (const relativePath of [
    "tools/rightkit/packages/license/test-vectors/license-v2.json",
    "rightapps/packages/api/src/licensing/test-vectors/license-v2.json",
    "scraperight/tests/fixtures/license-v2.json",
  ]) {
    assert.equal(
      readFileSync(path.join(workspace, relativePath), "utf8"),
      canonical,
      `${relativePath} must be the canonical public license vector byte-for-byte`,
    );
  }
});

for (const app of apps) {
  test(`${app.key} follows the signed tiered Right Release contract`, async () => {
    const root = path.join(workspace, app.root);
    const pkg = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8"));
    const rightKitDeps = { ...pkg.dependencies, ...pkg.devDependencies };
    for (const [name, specifier] of Object.entries(rightKitDeps).filter(([name]) => name.startsWith("@rightkit/"))) {
      assert.doesNotMatch(specifier, /^(?:git|file|link|workspace):|github/i, `${app.key} ${name} must come from the published npm package`);
      if (versions.npm[name]) assert.equal(specifier, versions.npm[name], `${app.key} ${name} must match rightkit-versions.json exactly`);
    }
    assert.equal(pkg.packageManager, versions.packageManager);
    assert.equal(pkg.devDependencies?.["@rightkit/release"], versions.npm["@rightkit/release"]);
    assert.equal(
      pkg.dependencies?.["@rightkit/updates"],
      versions.npm["@rightkit/updates"],
      `${app.key} must consume the exact published shared updater runtime`,
    );
    assert(!JSON.stringify(pkg).includes("github:adrdsouza/claude#main&path:/tools/right-release"));
    assert(!JSON.stringify(pkg).includes("github:adrdsouza/claude#main&path:/tools/rightkit/packages/release"));
    assert(!JSON.stringify(pkg).includes("git+https://github.com/adrdsouza/rightkit.git"));
    assert.equal(pkg.scripts["release:doctor"], "right-release doctor");
    assert.equal(pkg.scripts["release:mac"], undefined, "tierless release entry points are forbidden");
    assert.equal(pkg.scripts["release:win"], undefined, "tierless release entry points are forbidden");
    assert.equal(pkg.scripts["release:patch:mac"], "right-release --platform mac --tier patch");
    assert.equal(pkg.scripts["release:patch:win"], "right-release --platform win --tier patch");
    assert.equal(pkg.scripts["release:update:mac"], "right-release --platform mac --tier update");
    assert.equal(pkg.scripts["release:update:win"], "right-release --platform win --tier update");
    assert.equal(pkg.scripts["publish:patch:mac"], "right-release publish --platform mac --tier patch");
    assert.equal(pkg.scripts["publish:patch:win"], "right-release publish --platform win --tier patch");
    assert.equal(pkg.scripts["publish:update:mac"], "right-release publish --platform mac --tier update");
    assert.equal(pkg.scripts["publish:update:win"], "right-release publish --platform win --tier update");
    assert.equal(pkg.scripts["deps:check"], "right-release deps --check");
    assert.equal(pkg.scripts["deps:update"], "right-release deps --update");
    assert.ok(!Object.entries(pkg.scripts).some(([name, command]) => /^(?:release|publish):/i.test(name) && /unsigned|--no-sign/i.test(command)), `${app.key} release/publish commands must not expose an unsigned macOS DMG mode`);
    assert.ok(!JSON.stringify(pkg.scripts).includes("../tools/right-release"), `${app.key} scripts must not depend on the parent Claude workspace`);
    assert.ok(!JSON.stringify(pkg.scripts).includes("../tools/rightkit/packages/release"), `${app.key} scripts must not depend on the parent Claude workspace`);

    const config = (await import(`${pathToFileURL(path.join(root, "right-release.config.mjs"))}?contract=${Date.now()}`)).default;
    assert.equal(config.app, app.key);
    assert.ok(config.version);
    for (const platform of ["mac", "win"]) {
      const target = config.targets[platform];
      assert.equal(target.signed, true);
      assert.equal(target.upload, undefined, "generic uploads bypass tier manifest routing");
      assert.equal(target.publish.cmd, "right-release");
      assert.match(target.publish.args.join(" "), /^publish-update\b/);
      assert.ok(new Set(target.updater.artifacts.map((artifact) => artifact.key)).size <= 1, `${app.key} ${platform} must expose at most one current updater R2 object key`);
      assert.ok(new Set(target.installer.artifacts.map((artifact) => artifact.key)).size <= 1, `${app.key} ${platform} must expose at most one current installer R2 object key`);
      for (const installer of target.installer.artifacts) {
        assert.match(installer.key, /\/installers\/(mac|windows)\/current\//, `${app.key} ${platform} installers must replace the stable current R2 object`);
      }
      for (const updater of target.updater.artifacts) {
        assert.match(updater.key, /\/updates\/(mac|windows)\/current\//, `${app.key} ${platform} updaters must replace the stable current R2 object`);
      }
    }
    assert.equal(config.targets.mac.package.cmd, "pnpm", `${app.key} must use the shared pnpm package entry point`);
    assert.deepEqual(config.targets.mac.package.args, ["run", "mac:dmg:notarized"], `${app.key} R2 release must use the signed and notarized macOS package entry point`);
    for (const installer of config.targets.mac.installer.artifacts) {
      assert.equal(path.dirname(installer.file), ".", `${app.key} macOS installer must be copied to the app package root before upload`);
    }
    assert.ok(config.targets.win.sign.files.length);
    const winUpdaterFiles = new Set(config.targets.win.updater.artifacts.map((artifact) => artifact.file));
    for (const signed of config.targets.win.sign.files) assert.ok(winUpdaterFiles.has(signed), `${signed} must be both Azure-signed and updater-signed`);

    const tauri = JSON.parse(readFileSync(path.join(root, app.tauri), "utf8"));
    assert.equal(tauri.plugins.updater.pubkey, pubkey);
    assert.deepEqual(tauri.plugins.updater.endpoints, [
      `https://api.spoares.com/v1/apps/${app.key}/releases/latest.json?platform={{target}}`,
    ]);
    assert.equal(tauri.bundle.createUpdaterArtifacts, true);

    for (const manifestPath of findCargoManifests(root)) {
      const manifest = readFileSync(manifestPath, "utf8");
      for (const [crate, version] of Object.entries(versions.cargo)) {
        const dependency = manifest.match(new RegExp(`^${crate}\\s*=\\s*(.+)$`, "m"));
        if (!dependency) continue;
        assert.equal(
          dependency[1].trim(),
          `"=${version}"`,
          `${path.relative(workspace, manifestPath)} ${crate} must use the exact crates.io version`,
        );
      }
    }

    for (const localCopy of [
      "src-tauri/vendor/rightkit-license",
      "src-tauri/vendor/rightkit-logs",
      "crates/rightkit-license",
    ]) {
      assert.equal(
        existsSync(path.join(root, localCopy)),
        false,
        `${app.key} must not carry an app-local ${localCopy} copy`,
      );
    }

    const repoRoot = path.join(workspace, app.repoRoot ?? app.root);
    for (const packagePath of findFiles(repoRoot, "package.json")) {
      const candidate = JSON.parse(readFileSync(packagePath, "utf8"));
      assert.equal(candidate.scripts?.["release:win"], undefined, `${path.relative(workspace, packagePath)} has a forbidden tierless Windows release script`);
      assert.equal(candidate.scripts?.["release:mac"], undefined, `${path.relative(workspace, packagePath)} has a forbidden tierless macOS release script`);
      for (const command of Object.values(candidate.scripts ?? {})) {
        assert.doesNotMatch(command, /\brelease:(?:win|mac)\b/, `${path.relative(workspace, packagePath)} delegates to a forbidden tierless release command`);
      }
    }
    for (const instructionName of ["AGENTS.md", "CLAUDE.md"]) {
      const instructionPath = path.join(repoRoot, instructionName);
      if (!existsSync(instructionPath)) continue;
      const instruction = readFileSync(instructionPath, "utf8");
      assert.doesNotMatch(instruction, /pnpm(?: run)? release:(?:win|mac)\b/, `${app.key} ${instructionName} documents a forbidden tierless release`);
      assert.doesNotMatch(instruction, /shared tooling at [`']?\.\.\/tools\/right-release/i, `${app.key} ${instructionName} documents a sibling source dependency`);
      assert.doesNotMatch(instruction, /\$env:RIGHTAPPS_ADMIN_TOKEN|pnpm publish:r2/, `${app.key} ${instructionName} documents obsolete release credentials or publishers`);
    }
    for (const releaseFile of app.releaseFiles) {
      const source = readFileSync(path.join(root, releaseFile), "utf8");
      assert.doesNotMatch(source, /(?:\.\.\/)+tools\/right-release|tools\/right-release\//, `${app.key} ${releaseFile} must consume the installed package`);
      assert.match(source, /right-release["']?,?\s*["']mirror-root-artifact|right-release mirror-root-artifact/, `${app.key} ${releaseFile} must mirror the final DMG to the canonical package root`);
    }
  });
}

function findFiles(root, filename) {
  const found = [];
  const visit = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.isDirectory() && [".git", ".claude", ".worktrees", "node_modules", "target", "vendor"].includes(entry.name)) continue;
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) visit(full);
      else if (entry.isFile() && entry.name === filename) found.push(full);
    }
  };
  visit(root);
  return found;
}

const findCargoManifests = (root) => findFiles(root, "Cargo.toml");

test("Right Suite has no hosted workflow files", () => {
  for (const root of ["viewright", "scraperight", "heardright", "mailright", "coderight", "genright", "voiceright", "tools/rightkit"]) {
    const workflowDir = path.join(workspace, root, ".github", "workflows");
    const workflows = existsSync(workflowDir) ? readdirSync(workflowDir) : [];
    assert.deepEqual(workflows, [], `${root} must not contain hosted workflow files`);
  }
});

test("RightApps brand hosts expose app-keyed update manifest proxies", () => {
  for (const app of apps) {
    const siteRoot = path.join(workspace, "rightapps", app.key);
    for (const route of ["patches", "latest"]) {
      const routeFile = path.join(siteRoot, "src", "routes", "releases", `${route}.json`, "index.ts");
      const source = readFileSync(routeFile, "utf8");
      assert.match(source, new RegExp(`APP_KEY\\s*=\\s*["']${app.key}["']`), `${app.key} ${route}.json proxy must set the app key`);
      assert.match(source, /RIGHTAPPS_API_URL/, `${app.key} ${route}.json proxy must use the configurable RightApps API base`);
      assert.match(source, /x-app-key/i, `${app.key} ${route}.json proxy must forward the app key header`);
    }
  }
});
