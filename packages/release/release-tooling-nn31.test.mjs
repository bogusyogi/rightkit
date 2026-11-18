import { test } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { collectTooling, releaseToolingCallsVerify } from "./gate-acceptance.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));

test("NN31: the release check counts in its command form and in process through a module a release path imports", () => {
  const admission = 'import { BUDGETS_FILE, verifyReleaseReceipts } from "./gate-acceptance.mjs";\nexport function gateReleaseGuard() {}\n';
  const publish = 'import { gateReleaseGuard } from "./gate-release-admission.mjs";\n';
  // The command form.
  assert.deepEqual(releaseToolingCallsVerify({ "a/promote.sh": "node packages/release/gate-acceptance.mjs --verify-release r.json --host dell" }).calling, ["a/promote.sh"]);
  // In process: counted through the file that imports the checker, not the checker alone.
  assert.deepEqual(releaseToolingCallsVerify({ "p/gate-release-admission.mjs": admission, "p/publish-local-npm.mjs": publish }).calling, ["p/publish-local-npm.mjs"]);
  assert.deepEqual(releaseToolingCallsVerify({ "p/gate-release-admission.mjs": admission }).calling, [], "a checker nothing imports counts for nothing");
  assert.deepEqual(releaseToolingCallsVerify({ "p/gate-release-admission.mjs": admission, "p/gate-release-admission.test.mjs": publish }).calling, [], "a test importing it is not a release path");
  // A module that imports something else from the runner is not the release check.
  assert.deepEqual(releaseToolingCallsVerify({ "p/x.mjs": 'import { BUDGETS_FILE } from "./gate-acceptance.mjs";\n', "p/y.mjs": 'import { a } from "./x.mjs";\n' }).calling, []);
  // Words that only mention the flag do not count.
  assert.deepEqual(releaseToolingCallsVerify({ "p/doc.mjs": "// see --verify-release\n" }).calling, []);
});

test("NN31: this checkout's real tooling is recognised (publish-local-npm.mjs through gate-release-admission.mjs)", () => {
  const got = collectTooling(path.resolve(here, "..", ".."), path.join(here, "no-managed-root"));
  assert.ok(got.calling.some((f) => f.endsWith("publish-local-npm.mjs")), JSON.stringify(got.calling));
  assert.deepEqual(got.inProcess, ["gate-release-admission.mjs"]);
});
