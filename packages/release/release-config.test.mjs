// R01: `targets.<t>.updater.keyFile` is refused at config validation (updater keys come from the OS credential provider).
// Suite trust roots: a Right Suite app's `distribution.updaterKeyId` and declared updater public key are judged against the contract.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { LEGACY_RELEASE_CONFIG_FILE, RELEASE_CONFIG_FILE, ReleaseConfigError, defaultReleaseConfigFile, importReleaseConfig, isJsonReleaseConfig, loadReleaseConfigSync, releaseConfigFieldProblems, validateReleaseConfigShape } from "./release-config.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";
import { RIGHT_SUITE_APP_KEYS, RIGHT_SUITE_UPDATER_PUBLIC_KEYS, RIGHT_SUITE_UPDATER_SIGNING_KEY_ID } from "./suite-api.generated.mjs";

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

const CONTRACT_KEY_TEXT = RIGHT_SUITE_UPDATER_PUBLIC_KEYS.find((key) => key.kid === RIGHT_SUITE_UPDATER_SIGNING_KEY_ID).publicKey;
const FOREIGN_KEY_TEXT = Buffer.from("untrusted comment: minisign public key: 0123456789ABCDEF\nRWTvzSuJRWMBEUQdEHqbc7lbKiBHjtsGTNtGJHUTyDSkMmfbgoQCaeCl\n").toString("base64");
const problemsOf = (config) => releaseConfigFieldProblems({ schema: 1, ...config }).map(({ name, code, pointer }) => ({ name, code, pointer }));

test("distribution.updaterKeyId must be 16 upper-case hex digits for every app", () => {
  for (const app of ["demo", "pulse"]) {
    for (const updaterKeyId of ["abc", RIGHT_SUITE_UPDATER_SIGNING_KEY_ID.toLowerCase(), `${RIGHT_SUITE_UPDATER_SIGNING_KEY_ID}0`, "", 123, null, false]) {
      assert.deepEqual(problemsOf({ app, distribution: { updaterKeyId } }), [{ name: "updater-key-id", code: "RELEASE_CONFIG_UPDATER_KEY_ID", pointer: "/distribution/updaterKeyId" }], `${app} ${JSON.stringify(updaterKeyId)}`);
    }
  }
  const [problem] = releaseConfigFieldProblems({ schema: 1, app: "demo", distribution: { updaterKeyId: "abc" } });
  assert.match(problem.detail, /distribution\.updaterKeyId must be the 16 upper-case hex digits of a minisign key id, got "abc"/);
});

test("an app outside the suite may name any well-formed key id and any key", () => {
  assert.deepEqual(problemsOf({ app: "ember", distribution: { updaterKeyId: "0123456789ABCDEF", updaterPublicKey: FOREIGN_KEY_TEXT, updaterPubkey: FOREIGN_KEY_TEXT }, targets: { win: { updater: { pubkey: FOREIGN_KEY_TEXT, publicKey: FOREIGN_KEY_TEXT } } } }), []);
});

test("a suite app's updaterKeyId must be a key the contract lists", () => {
  for (const app of RIGHT_SUITE_APP_KEYS) {
    assert.deepEqual(problemsOf({ app, distribution: { updaterKeyId: RIGHT_SUITE_UPDATER_SIGNING_KEY_ID } }), [], app);
    assert.deepEqual(problemsOf({ app, distribution: {} }), [], app);
    assert.deepEqual(problemsOf({ app, distribution: { updaterKeyId: "0123456789ABCDEF" } }), [{ name: "updater-key-id", code: "RELEASE_CONFIG_UPDATER_KEY_ID", pointer: "/distribution/updaterKeyId" }], app);
  }
  assert.deepEqual(problemsOf({ app: "MailRight", distribution: { updaterKeyId: "0123456789ABCDEF" } }), [{ name: "updater-key-id", code: "RELEASE_CONFIG_UPDATER_KEY_ID", pointer: "/distribution/updaterKeyId" }], "the app name's case does not take an app out of the suite");
  const [problem] = releaseConfigFieldProblems({ schema: 1, app: "pulse", distribution: { updaterKeyId: "0123456789ABCDEF" } });
  assert.match(problem.detail, /0123456789ABCDEF is not an updater key in the Right Suite contract \(accepted: 29395F9FF466261D\)/);
});

test("a suite app may repeat a contract key text but declares no other updater public key, in any of the four fields", () => {
  const declared = (value) => ({ app: "mailright", distribution: { updaterPublicKey: value, updaterPubkey: value }, targets: { win: { updater: { pubkey: value, publicKey: value } } } });
  assert.deepEqual(problemsOf(declared(CONTRACT_KEY_TEXT)), []);
  for (const blank of [undefined, null, "", "  \n"]) assert.deepEqual(problemsOf(declared(blank)), [], JSON.stringify(blank));
  const publicKeyProblem = (pointer) => ({ name: "updater-public-key", code: "RELEASE_CONFIG_UPDATER_PUBLIC_KEY", pointer });
  for (const value of [FOREIGN_KEY_TEXT, `${CONTRACT_KEY_TEXT}\n`, CONTRACT_KEY_TEXT.slice(1), 123]) {
    assert.deepEqual(problemsOf(declared(value)), [
      publicKeyProblem("/distribution/updaterPublicKey"),
      publicKeyProblem("/distribution/updaterPubkey"),
      publicKeyProblem("/targets/win/updater/pubkey"),
      publicKeyProblem("/targets/win/updater/publicKey"),
    ], JSON.stringify(value));
  }
  const [problem] = releaseConfigFieldProblems({ schema: 1, app: "mailright", distribution: { updaterPublicKey: FOREIGN_KEY_TEXT } });
  assert.match(problem.detail, /distribution\.updaterPublicKey is not a Right Suite contract key: mailright is a suite app and takes its updater keys from the contract/);
});

test("the updater.keyFile refusal still comes first, so its code is the one a config that also sets other fields throws", () => {
  const config = { schema: 1, app: "pulse", distribution: { updaterKeyId: "abc", updaterPublicKey: FOREIGN_KEY_TEXT }, targets: { win: { updater: { keyFile: "a" } } } };
  assert.deepEqual(releaseConfigFieldProblems(config).map((problem) => problem.name), ["updater-key-file", "updater-key-id", "updater-public-key"]);
  assert.throws(() => validateReleaseConfigShape(config), (error) => error instanceof ReleaseConfigError && error.code === "RELEASE_CONFIG_UPDATER_KEY_FILE" && /updater\.keyFile[\s\S]*updaterKeyId[\s\S]*Right Suite contract key/.test(error.message));
});

test("the loader refuses a suite app's foreign updater key, also when a template supplies it", () => {
  const load = (distribution, tauriPubkey) => {
    const root = tempRoot("right-release-config-");
    mkdirSync(path.join(root, "src-tauri"));
    writeFileSync(path.join(root, "src-tauri", "tauri.conf.json"), JSON.stringify({ plugins: { updater: { pubkey: tauriPubkey } } }));
    const file = path.join(root, "right-release.config.json");
    writeFileSync(file, JSON.stringify({ schema: 1, app: "pulse", distribution }));
    return loadReleaseConfigSync(file, { validatePipeline: false });
  };
  const fromTauri = { $fromJson: { file: "src-tauri/tauri.conf.json", pointer: "/plugins/updater/pubkey" } };
  assert.equal(load({ updaterPublicKey: CONTRACT_KEY_TEXT }, "").config.distribution.updaterPublicKey, CONTRACT_KEY_TEXT);
  assert.equal(load({ updaterPublicKey: fromTauri }, CONTRACT_KEY_TEXT).config.distribution.updaterPublicKey, CONTRACT_KEY_TEXT);
  for (const [distribution, tauriPubkey] of [[{ updaterPublicKey: FOREIGN_KEY_TEXT }, ""], [{ updaterPublicKey: fromTauri }, FOREIGN_KEY_TEXT], [{ updaterKeyId: "0123456789ABCDEF" }, ""]]) {
    assert.throws(() => load(distribution, tauriPubkey), (error) => error instanceof ReleaseConfigError && /^RELEASE_CONFIG_UPDATER_(PUBLIC_KEY|KEY_ID)$/.test(error.code), JSON.stringify(distribution));
  }
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

test("{{localAppData}} loads on every host: %LOCALAPPDATA% when set, the Windows profile default under the home directory otherwise", () => {
  const file = writeConfig({
    schema: 1, app: "demo",
    development: { targets: { win: { install: { kind: "tauri", productName: "Demo", targetTriple: "x86_64-pc-windows-msvc", expectedInstalledPath: "{{localAppData}}/Demo/demo.exe" } } } },
  });
  const installPath = (env) => loadReleaseConfigSync(file, { validatePipeline: false, env, homedir: () => "/home/someone" }).config.development.targets.win.install.expectedInstalledPath;
  assert.equal(installPath({}), `${path.join("/home/someone", "AppData", "Local")}/Demo/demo.exe`);
  assert.equal(installPath({ LOCALAPPDATA: "" }), `${path.join("/home/someone", "AppData", "Local")}/Demo/demo.exe`);
  assert.equal(installPath({ LOCALAPPDATA: "C:\\Users\\a\\AppData\\Local" }), "C:\\Users\\a\\AppData\\Local/Demo/demo.exe");
});

test("a template token inside $comment is expanded like any other string, so a comment must not name one", () => {
  const file = writeConfig({ schema: 1, app: "demo", $comment: "use {{version}} here", version: "1.2.3" });
  assert.equal(loadReleaseConfigSync(file, { validatePipeline: false }).config.$comment, "use 1.2.3 here");
  const unknown = writeConfig({ schema: 1, app: "demo", $comment: "tokens like {{nope}} are errors" });
  assert.throws(() => loadReleaseConfigSync(unknown, { validatePipeline: false }), (error) => error.code === "RELEASE_CONFIG_TEMPLATE" && /unknown template token \{\{nope\}\}/.test(error.message));
});
