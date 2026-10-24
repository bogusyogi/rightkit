// R01: `targets.<t>.updater.keyFile` is refused at config validation (updater keys come from the OS credential provider).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { ReleaseConfigError, loadReleaseConfigSync, releaseConfigFieldProblems, validateReleaseConfigShape } from "./release-config.mjs";
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
