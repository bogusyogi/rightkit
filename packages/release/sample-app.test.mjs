import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { sampleAppOf } from "./bin/rightkit-gate.mjs";

const root = path.resolve("/repo");
const git = (record) => (cmd, spec) => (cmd === "show" && spec === "c1:ci/sample-apps.json" && record !== null ? { status: 0, stdout: record } : { status: 128, stdout: "", stderr: "missing" });

test("a run from inside a listed RightKit sample app is that app; anything else is not", () => {
  const listed = git(JSON.stringify({ apps: ["e2e-app"] }));
  assert.equal(sampleAppOf({ o: { cwd: path.join(root, "e2e-app") }, root, git: listed, commit: "c1" }), "e2e-app");
  assert.equal(sampleAppOf({ o: { cwd: path.join(root, "e2e-app", "crates", "engine") }, root, git: listed, commit: "c1" }), "e2e-app");
  assert.equal(sampleAppOf({ o: { cwd: root }, root, git: listed, commit: "c1" }), null, "the repository root is not the app");
  assert.equal(sampleAppOf({ o: { cwd: path.join(root, "e2e-app-other") }, root, git: listed, commit: "c1" }), null, "a prefix is not the app");
  assert.equal(sampleAppOf({ o: { cwd: path.join(root, "e2e-app") }, root, git: git(null), commit: "c1" }), null, "no record, no sample app");
  assert.equal(sampleAppOf({ o: { cwd: path.join(root, "x") }, root, git: git(JSON.stringify({ apps: ["../x"] })), commit: "c1" }), null);
});
