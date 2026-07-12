import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";

const workspace = path.resolve(new URL("../../../..", import.meta.url).pathname.replace(/^\/(\w:)/, "$1"));
const pubkey = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDI5Mzk1RjlGRjQ2NjI2MUQKUldRZEptYjBuMTg1S1VSUXlBdFM4WmtzaHArYko0U2hRMDVlSDJmSExVZG82Q0hoQ2srUlhqanAK";
const licensePackage = "^0.1.5";
const logsPackage = "^0.1.3";
const releasePackage = "^0.2.10";
const apps = [
  { key: "viewright", root: "viewright", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["scripts/build-mac-notarized.sh"] },
  { key: "scraperight", root: "scraperight", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["package.sh"] },
  { key: "heardright", root: "heardright/tauri-app-next", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["scripts/mac-dmg.mjs", "scripts/publish-release.mjs"] },
  { key: "mailright", root: "mailright", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["scripts/mac-dmg.mjs"] },
  { key: "coderight", root: "coderight/apps/coderight-tauri", tauri: "src-tauri/tauri.conf.json", releaseFiles: ["scripts/mac-dmg.mjs"] },
];

for (const app of apps) {
  test(`${app.key} follows the signed tiered Right Release contract`, async () => {
    const root = path.join(workspace, app.root);
    const pkg = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8"));
    const rightKitDeps = { ...pkg.dependencies, ...pkg.devDependencies };
    for (const [name, specifier] of Object.entries(rightKitDeps).filter(([name]) => name.startsWith("@rightkit/"))) {
      assert.doesNotMatch(specifier, /^(?:git|file|link|workspace):|github/i, `${app.key} ${name} must come from the published npm package`);
    }
    if (rightKitDeps["@rightkit/license"]) assert.equal(rightKitDeps["@rightkit/license"], licensePackage);
    if (rightKitDeps["@rightkit/logs"]) assert.equal(rightKitDeps["@rightkit/logs"], logsPackage);
    assert.equal(pkg.devDependencies?.["@rightkit/release"], releasePackage);
    assert(!JSON.stringify(pkg).includes("github:adrdsouza/claude#main&path:/tools/right-release"));
    assert(!JSON.stringify(pkg).includes("github:adrdsouza/claude#main&path:/tools/rightkit/packages/release"));
    assert(!JSON.stringify(pkg).includes("git+https://github.com/adrdsouza/rightkit.git"));
    assert.equal(pkg.scripts["release:doctor"], "right-release doctor");
    assert.equal(pkg.scripts["release:mac"], "right-release --platform mac");
    assert.equal(pkg.scripts["release:win"], "right-release --platform win");
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
    assert.ok(!Object.entries(pkg.scripts).some(([name, command]) => /mac|dmg/i.test(name) && /unsigned|--no-sign/i.test(command)), `${app.key} must not expose an unsigned macOS DMG mode`);
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
    assert.match(config.targets.mac.package.args.join(" "), /mac:dmg:notarized|--notarize/, `${app.key} publish packaging must use the notarized macOS lane`);
    assert.ok(config.targets.win.sign.files.length);
    const winUpdaterFiles = new Set(config.targets.win.updater.artifacts.map((artifact) => artifact.file));
    for (const signed of config.targets.win.sign.files) assert.ok(winUpdaterFiles.has(signed), `${signed} must be both Azure-signed and updater-signed`);

    const tauri = JSON.parse(readFileSync(path.join(root, app.tauri), "utf8"));
    assert.equal(tauri.plugins.updater.pubkey, pubkey);
    assert.deepEqual(tauri.plugins.updater.endpoints, [
      `https://api.spoares.com/v1/apps/${app.key}/releases/latest.json?platform={{target}}`,
    ]);
    assert.equal(tauri.bundle.createUpdaterArtifacts, true);
    for (const releaseFile of app.releaseFiles) {
      const source = readFileSync(path.join(root, releaseFile), "utf8");
      assert.doesNotMatch(source, /(?:\.\.\/)+tools\/right-release|tools\/right-release\//, `${app.key} ${releaseFile} must consume the installed package`);
    }
    for (const forbidden of ["scripts/build-mac-unsigned.sh", "package-unsigned.sh"]) {
      assert.equal(existsSync(path.join(root, forbidden)), false, `${app.key} must not ship ${forbidden}`);
    }
  });
}

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
