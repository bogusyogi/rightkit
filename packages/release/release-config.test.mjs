// R01: `targets.<t>.updater.keyFile` is refused at config validation (updater keys come from the OS credential provider).
// Suite trust roots: a Right Suite app's `distribution.updaterKeyId` and declared updater public key are judged against the contract.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { ReleaseConfigError, loadReleaseConfigSync, releaseConfigFieldProblems, validateReleaseConfigShape } from "./release-config.mjs";
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
