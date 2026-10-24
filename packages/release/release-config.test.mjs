// R01: `targets.<t>.updater.keyFile` is refused at config validation (updater keys come from the OS credential provider).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { LEGACY_RELEASE_CONFIG_FILE, RELEASE_CONFIG_FILE, ReleaseConfigError, defaultReleaseConfigFile, importReleaseConfig, isJsonReleaseConfig, loadReleaseConfigSync, releaseConfigFieldProblems, validateReleaseConfigShape } from "./release-config.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

function writeConfig(config) {
  const file = path.join(tempRoot("right-release-config-"), "right-release.config.json");
  writeFileSync(file, JSON.stringify(config));
  return file;
}

test("updater.keyFile is refused with the field named and the OS credential provider as the reason", () => {
  const file = writeConfig({ schema: 1, app: "demo", targets: { win: { updater: { keyFile: "%APPDATA%/RightKit/updater.key", artifacts: [] } } } });
  assert.throws(() => loadReleaseConfigSync(file, { validatePipeline: false }), (error) => {
    assert.ok(error instanceof ReleaseConfigError);
    assert.equal(error.code, "RELEASE_CONFIG_UPDATER_KEY_FILE");
    assert.match(error.message, /targets\.win\.updater\.keyFile is not supported/);
    assert.match(error.message, /OS credential provider/);
    assert.match(error.message, /protected-credentials\.mjs/);
    assert.match(error.message, /Remove updater\.keyFile/);
    return true;
  });
});

test("every target that sets updater.keyFile is listed, whatever the value", () => {
  const config = { schema: 1, app: "demo", targets: { win: { updater: { keyFile: "a" } }, mac: { updater: { keyFile: null } }, linux: { updater: { artifacts: [] } } } };
  const problems = releaseConfigFieldProblems(config);
  assert.deepEqual(problems.map((problem) => problem.pointer), ["/targets/win/updater/keyFile", "/targets/mac/updater/keyFile"]);
  assert.ok(problems.every((problem) => problem.name === "updater-key-file" && /OS credential provider/.test(problem.detail)));
  assert.throws(() => validateReleaseConfigShape(config), /targets\.win\.updater\.keyFile[\s\S]*targets\.mac\.updater\.keyFile/);
});

test("a config without updater.keyFile loads", () => {
  const file = writeConfig({ schema: 1, app: "demo", targets: { win: { updater: { artifacts: [] } } } });
  assert.equal(loadReleaseConfigSync(file, { validatePipeline: false }).config.app, "demo");
  assert.deepEqual(releaseConfigFieldProblems({ schema: 1, app: "demo" }), []);
  assert.deepEqual(releaseConfigFieldProblems(null), []);
});

// ---- command wiring: default name, strict JSON branch, legacy module branch ----------------------------------------------------------
const appDir = (files = {}) => {
  const root = tempRoot("right-release-config-app-");
  for (const [name, content] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(root, name)), { recursive: true });
    writeFileSync(path.join(root, name), typeof content === "string" ? content : JSON.stringify(content));
  }
  return root;
};
const refusal = async (promise, code) => assert.rejects(promise, (error) => { assert.ok(error instanceof ReleaseConfigError, `expected a ReleaseConfigError, got ${error?.name}: ${error?.message}`); assert.equal(error.code, code); return true; });

test("defaultReleaseConfigFile picks right-release.config.json when it exists and the legacy module otherwise", () => {
  assert.equal(isJsonReleaseConfig("a/right-release.config.json"), true);
  assert.equal(isJsonReleaseConfig("right-release.config.JSON"), true);
  assert.equal(isJsonReleaseConfig("right-release.config.mjs"), false);
  assert.equal(isJsonReleaseConfig("right-release.config.json.mjs"), false);
  assert.equal(defaultReleaseConfigFile(appDir({ [RELEASE_CONFIG_FILE]: { schema: 1, app: "demo" } })), RELEASE_CONFIG_FILE);
  assert.equal(defaultReleaseConfigFile(appDir({ [RELEASE_CONFIG_FILE]: { schema: 1, app: "demo" }, [LEGACY_RELEASE_CONFIG_FILE]: "export default {};" })), RELEASE_CONFIG_FILE, "the JSON config wins when both exist");
  assert.equal(defaultReleaseConfigFile(appDir({ [LEGACY_RELEASE_CONFIG_FILE]: "export default {};" })), LEGACY_RELEASE_CONFIG_FILE);
  assert.equal(defaultReleaseConfigFile(appDir()), LEGACY_RELEASE_CONFIG_FILE, "an app with neither keeps the name it always had, so the error text is unchanged");
  const probed = [];
  assert.equal(defaultReleaseConfigFile("/app", { exists: (file) => { probed.push(file); return true; } }), RELEASE_CONFIG_FILE);
  assert.deepEqual(probed, [path.join("/app", RELEASE_CONFIG_FILE)]);
});

test("importReleaseConfig loads a JSON config strictly and returns the expanded config", async () => {
  const root = appDir({
    "src-tauri/tauri.conf.json": { version: "1.2.3" },
    [RELEASE_CONFIG_FILE]: { schema: 1, app: "demo", version: { $fromJson: { file: "src-tauri/tauri.conf.json", pointer: "/version" } }, targets: { mac: { artifact: "target/{{app}}-{{version}}.dmg" } } },
  });
  const config = await importReleaseConfig(path.join(root, RELEASE_CONFIG_FILE));
  assert.equal(config.version, "1.2.3");
  assert.equal(config.targets.mac.artifact, "target/demo-1.2.3.dmg");
  assert.deepEqual(await importReleaseConfig(path.join(root, RELEASE_CONFIG_FILE), { query: "ignored=1", lenient: true }), config, "the legacy-only options change nothing for a JSON config");
});

test("importReleaseConfig refuses an invalid JSON config with a ReleaseConfigError", async () => {
  const comment = appDir({ [RELEASE_CONFIG_FILE]: '{\n  // not data\n  "schema": 1, "app": "demo"\n}\n' });
  await refusal(importReleaseConfig(path.join(comment, RELEASE_CONFIG_FILE)), "RELEASE_CONFIG_INVALID_JSON");
  const unknownKey = appDir({ [RELEASE_CONFIG_FILE]: { schema: 1, app: "demo", hooks: { pre: "x" } } });
  await refusal(importReleaseConfig(path.join(unknownKey, RELEASE_CONFIG_FILE)), "RELEASE_CONFIG_KEY");
  const missing = appDir();
  await refusal(importReleaseConfig(path.join(missing, RELEASE_CONFIG_FILE)), "RELEASE_CONFIG_NOT_FOUND");
});

test("importReleaseConfig refuses a JSON config whose dev build command is not an admitted invocation (M24)", async () => {
  const bad = appDir({ [RELEASE_CONFIG_FILE]: { schema: 1, app: "demo", development: { targets: { mac: { build: [{ cmd: "bash", args: ["x.sh"] }] } } } } });
  await assert.rejects(importReleaseConfig(path.join(bad, RELEASE_CONFIG_FILE)), (error) => {
    assert.ok(error instanceof ReleaseConfigError);
    assert.equal(error.code, "RELEASE_CONFIG_M24");
    assert.match(error.message, /\/development\/targets\/mac\/build\/0/);
    return true;
  });
  const good = appDir({ [RELEASE_CONFIG_FILE]: { schema: 1, app: "demo", development: { targets: { mac: { build: [{ cmd: "rustright", args: ["cargo", "build"] }] } } } } });
  assert.equal((await importReleaseConfig(path.join(good, RELEASE_CONFIG_FILE))).development.targets.mac.build[0].cmd, "rustright");
});

test("importReleaseConfig still imports a legacy .mjs module, with its cache-busting query and its lenient fallbacks", async () => {
  const root = appDir({ [LEGACY_RELEASE_CONFIG_FILE]: 'export default { schema: 1, app: "legacy-one" };\n', "named.mjs": 'export const config = { app: "named" };\n', [RELEASE_CONFIG_FILE]: { schema: 1, app: "json-beside-it" } });
  const legacy = path.join(root, LEGACY_RELEASE_CONFIG_FILE);
  assert.equal((await importReleaseConfig(legacy, { query: "upload=1" })).app, "legacy-one", "an explicit .mjs path takes the legacy branch even beside a JSON config");
  writeFileSync(legacy, 'export default { schema: 1, app: "legacy-two" };\n');
  assert.equal((await importReleaseConfig(legacy, { query: "upload=1" })).app, "legacy-one", "the same query is the same module instance");
  assert.equal((await importReleaseConfig(legacy, { query: "upload=2" })).app, "legacy-two", "a new query re-evaluates the module");
  assert.equal(await importReleaseConfig(path.join(root, "named.mjs")), undefined, "the strict branch is default-export only");
  assert.equal((await importReleaseConfig(path.join(root, "named.mjs"), { lenient: true })).app, "named");
});

// Every command resolves its default config through defaultReleaseConfigFile. A literal default sends a JSON-only app to a missing .mjs.
const COMMAND_MODULES = [
  "release.mjs", "build-release.mjs", "development.mjs", "upload-release.mjs", "github-upload-gate.mjs", "android-play-upload.mjs", "ios-upload.mjs",
  "github-release.mjs", "publish-update.mjs", "release-qa-command.mjs", "command-spec.mjs", "test-command.mjs", "web-build.mjs",
  "npm-license-notices.mjs", "cargo-license-notices.mjs", "binary-license-notices.mjs", "deps.mjs", "release-dependencies-command.mjs",
  "stage-sidecar-command.mjs", "cache-command.mjs", "mirror-root-artifact.mjs", "apple-profile-create.mjs", "apple-profile-renew.mjs", "cargo-profile-contract.mjs",
];
test("no release command hard-codes the legacy config name or imports a release config directly", () => {
  const here = fileURLToPath(new URL(".", import.meta.url));
  for (const name of COMMAND_MODULES) {
    const text = readFileSync(path.join(here, name), "utf8");
    assert.ok(!text.includes(`"${LEGACY_RELEASE_CONFIG_FILE}"`), `${name} hard-codes ${LEGACY_RELEASE_CONFIG_FILE}; use defaultReleaseConfigFile()`);
    // github-release.mjs imports the --addon file (an add-on config is a different contract); its release config goes through github-upload-gate.mjs.
    if (name !== "github-release.mjs") assert.ok(!/await import\(`?\$?\{?pathToFileURL\((?:configPath|absoluteConfig|configFile|absolute|file)\b/.test(text), `${name} imports the release config directly; use importReleaseConfig()`);
  }
});
