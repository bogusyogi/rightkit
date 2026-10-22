import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { HOST_UPLOAD_REFUSED, PUBLISH_REPOSITORY, admissionProblems, assertPublishCapability, createPublishCapability } from "./host-upload-refusal.mjs";
import { ciEnvironment, fakeAdmission, fakeApproval, mintTestCapability, withEnvironment } from "./test-support/publish-capability-fixture.mjs";
import { runLocalPublish } from "./local-publish.mjs";
import { runNpmPreflight } from "./publication-gate.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const node = (file, args, env = {}) => spawnSync(process.execPath, [path.join(here, file), ...args], {
  encoding: "utf8", windowsHide: true, env: { ...process.env, ...env },
});

// A caller-set GITHUB_ACTIONS=true (or CI=true) must never lift the refusal: CI uploads use scripts/ci/publish.mjs and
// reviewed-candidate.mjs, not right-release.
for (const env of [{}, { GITHUB_ACTIONS: "true" }, { GITHUB_ACTIONS: "true", CI: "true", RUNNER_ENVIRONMENT: "github-hosted" }]) {
  const label = JSON.stringify(env);
  for (const args of [
    ["publish", "cargo", "--crate", "rightkit-x"],
    ["publish", "cargo", "--crate", "rightkit-x", "--from-evidence", "gate.json"],
    ["publish", "cargo", "--crate", "rightkit-x", "--local"],
    ["publish", "npm", "--package", "@rightkit/x", "--publish", "--tarball", "x.tgz"],
    ["publish", "npm", "--package", "@rightkit/x", "--local"],
  ]) {
    test(`right-release ${args.join(" ")} is refused with env ${label}`, () => {
      const result = node("cli/right-release.mjs", args, env);
      // Without GITHUB_ACTIONS a compile=github-actions-only checkout stops it even earlier (exit 125); either way nothing uploads.
      if (env.GITHUB_ACTIONS === "true") { assert.equal(result.status, 2, result.stderr); assert.match(result.stderr, /host uploads are retired/); }
      else { assert.notEqual(result.status, 0, result.stderr); assert.match(result.stderr, /host uploads are retired|GitHub Actions only/); }
    });
  }
}

test("direct publish-cargo.mjs refuses every non-dry-run call, including --from-evidence, with GITHUB_ACTIONS=true", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-host-upload-"));
  try {
    fs.mkdirSync(path.join(root, "crates", "rightkit-x"), { recursive: true });
    for (const args of [["--crate", "rightkit-x", "--workspace", root], ["--crate", "rightkit-x", "--workspace", root, "--from-evidence", path.join(root, "gate.json")]]) {
      const result = node("publish-cargo.mjs", args, { GITHUB_ACTIONS: "true" });
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, new RegExp(HOST_UPLOAD_REFUSED + "|host uploads are retired"));
    }
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test("direct publication-gate.mjs --publish is refused with GITHUB_ACTIONS=true", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-host-upload-"));
  try {
    fs.mkdirSync(path.join(root, "packages", "x"), { recursive: true });
    fs.writeFileSync(path.join(root, "packages", "x", "package.json"), JSON.stringify({ name: "@rightkit/x", version: "1.0.0" }));
    const result = node("publication-gate.mjs", ["--package", "@rightkit/x", "--workspace", root, "--publish", "--tarball", path.join(root, "x.tgz")], { GITHUB_ACTIONS: "true" });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /host uploads are retired/);
    assert.doesNotMatch(result.stderr, /submitted/);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test("runLocalPublish refuses every upload mode in-process and keeps --qualify-only", async () => {
  await assert.rejects(runLocalPublish("cargo", ["--local", "--crate", "rightkit-x", "--workspace", os.tmpdir()]), { code: HOST_UPLOAD_REFUSED });
  await assert.rejects(runLocalPublish("cargo", ["--local", "--crates", "rightkit-x,rightkit-y", "--workspace", os.tmpdir()]), { code: HOST_UPLOAD_REFUSED });
  await assert.rejects(runLocalPublish("npm", ["--local", "--package", "@rightkit/x", "--workspace", os.tmpdir()]), { code: HOST_UPLOAD_REFUSED });
  // --qualify-only is not refused as an upload; it proceeds to its own preconditions (a missing unit here).
  await assert.rejects(
    runLocalPublish("cargo", ["--local", "--crate", "rightkit-x", "--workspace", path.join(os.tmpdir(), "rk-no-such-ws"), "--qualify-only", "--bundle", path.join(os.tmpdir(), "rk-no-such-bundle")]),
    (error) => error.code !== HOST_UPLOAD_REFUSED,
  );
});

test("runNpmPreflight --publish is refused before any tarball or token is read", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-host-upload-"));
  try {
    fs.mkdirSync(path.join(root, "packages", "x"), { recursive: true });
    fs.writeFileSync(path.join(root, "packages", "x", "package.json"), JSON.stringify({ name: "@rightkit/x", version: "1.0.0" }));
    assert.throws(() => runNpmPreflight(["--package", "@rightkit/x", "--publish", "--tarball", "x.tgz"], root, { readToken: () => assert.fail("token must not be read") }), { code: HOST_UPLOAD_REFUSED });
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

// ---- the publish capability: a direct import of the publisher internals cannot publish --------------------------------------

const ARCHIVES = [{ name: "rightkit-a", version: "1.0.0", sha256: "a".repeat(64) }, { name: "rightkit-b", version: "2.0.0", sha256: "b".repeat(64) }];
const mint = (over = {}) => {
  const admission = over.admission ?? fakeAdmission({ archives: ARCHIVES, mutate: over.mutate });
  return withEnvironment(over.env ?? ciEnvironment(admission), () => createPublishCapability({ admission, approved: over.approved ?? fakeApproval(over.registry ?? "crates", ARCHIVES), registry: over.registry ?? "crates" }));
};
const refuses = (call, pattern) => assert.throws(call, (error) => error.code === HOST_UPLOAD_REFUSED && pattern.test(error.message), String(pattern));

test("a publish capability exists only for a validated admission, an approval entry per archive and the CI run the admission names", () => {
  const capability = mint();
  assert.ok(Object.isFrozen(capability));
  assertPublishCapability(capability, { what: "t", registry: "crates", name: "rightkit-a", version: "1.0.0", sha256: "a".repeat(64) });
  assertPublishCapability(capability, { what: "t", registry: "crates", name: "rightkit-b" });
  assert.deepEqual(admissionProblems(fakeAdmission({ archives: ARCHIVES, kind: "local-admission" }), { registry: "crates", env: ciEnvironment(fakeAdmission({ archives: ARCHIVES })), approved: fakeApproval("crates", ARCHIVES) }), []);
  // The environment alone, a caller-set GITHUB_ACTIONS and the CI variables of any other run lift nothing.
  refuses(() => createPublishCapability({ registry: "crates", approved: fakeApproval("crates", ARCHIVES) }), /validated admission/);
  withEnvironment({ GITHUB_ACTIONS: "true", CI: "true", GITHUB_REF: "refs/heads/main", GITHUB_REPOSITORY: PUBLISH_REPOSITORY }, () => refuses(() => createPublishCapability({ admission: {}, approved: [], registry: "crates" }), /validated admission/));
  const admission = fakeAdmission({ archives: ARCHIVES });
  const cases = {
    "not in GitHub Actions": { env: { ...ciEnvironment(admission), GITHUB_ACTIONS: "false" } },
    "another ref": { env: { ...ciEnvironment(admission), GITHUB_REF: "refs/heads/topic" } },
    "another repository": { env: { ...ciEnvironment(admission), GITHUB_REPOSITORY: "someone/else" } },
    "another verifier commit": { env: { ...ciEnvironment(admission), GITHUB_SHA: "f".repeat(40) } },
    "another run": { env: { ...ciEnvironment(admission), GITHUB_RUN_ID: "1" } },
    "another attempt": { env: { ...ciEnvironment(admission), GITHUB_RUN_ATTEMPT: "7" } },
    "an unknown schema": { mutate: (a) => { a.schema = 2; } },
    "an unknown kind": { mutate: (a) => { a.kind = "self-signed"; } },
    "no original binding": { mutate: (a) => { delete a.original; } },
    "a malformed candidate hash": { mutate: (a) => { a.original.candidateSha256 = "nope"; } },
    "no archives": { mutate: (a) => { a.original.archives = []; a.units = []; } },
    "a verdict that failed": { mutate: (a) => { a.units[0].ok = false; } },
    "a verdict with failures": { mutate: (a) => { a.units[1].failures = ["x"]; } },
    "a verdict for other bytes": { mutate: (a) => { a.units[0].archiveSha256 = "c".repeat(64); } },
    "a verdict for another version": { mutate: (a) => { a.units[0].version = "9.9.9"; } },
    "a missing verdict": { mutate: (a) => { a.units.pop(); } },
    "a duplicate archive": { mutate: (a) => { a.original.archives[1].name = "rightkit-a"; a.units[1].name = "rightkit-a"; } },
    "a malformed source tree": { mutate: (a) => { a.sourceTree = "tree"; } },
    "no run": { mutate: (a) => { delete a.runId; } },
    "no approval": { approved: [] },
    "an approval for other bytes": { approved: fakeApproval("crates", ARCHIVES).map((entry, i) => (i === 0 ? { ...entry, sha256: "d".repeat(64) } : entry)) },
    "an approval for another registry": { approved: fakeApproval("npm", ARCHIVES) },
    "a registry that is neither npm nor crates": { registry: "swift", approved: fakeApproval("swift", ARCHIVES) },
  };
  for (const [label, over] of Object.entries(cases)) {
    // The CI variables follow the (possibly mutated) admission unless the case sets its own.
    const mutated = fakeAdmission({ archives: ARCHIVES, mutate: over.mutate });
    refuses(() => mint({ ...over, admission: mutated, env: over.env ?? ciEnvironment(mutated) }), /validated admission/);
    assert.ok(label);
  }
});

test("a publish capability cannot be forged, copied or reused for another registry, unit, version or bytes", () => {
  const capability = mint();
  const ok = { what: "t", registry: "crates", name: "rightkit-a", version: "1.0.0", sha256: "a".repeat(64) };
  assertPublishCapability(capability, ok);
  for (const [label, forged] of Object.entries({ undefined: undefined, null: null, string: "x", "an empty object": {}, "a literal": { kind: "rightkit-publish-capability", registry: "crates" }, "a spread copy": { ...capability }, "a structured clone": structuredClone(capability), "a prototype clone": Object.create(capability) })) {
    refuses(() => assertPublishCapability(forged, ok), /no publish capability/);
    assert.ok(label);
  }
  refuses(() => assertPublishCapability(capability, { ...ok, registry: "npm" }), /is for crates, not npm/);
  refuses(() => assertPublishCapability(capability, { ...ok, name: "rightkit-c" }), /not an archive of the admission/);
  refuses(() => assertPublishCapability(capability, { ...ok, version: "1.0.1" }), /is not the admitted 1\.0\.0/);
  refuses(() => assertPublishCapability(capability, { ...ok, sha256: "e".repeat(64) }), /not the admitted archive/);
  // A capability of the test fixture is the same object kind, created by the same validator.
  assertPublishCapability(mintTestCapability({ registry: "npm", archives: [{ name: "@rightkit/x", version: "1.0.0", sha256: "f".repeat(64) }] }), { what: "t", registry: "npm", name: "@rightkit/x", version: "1.0.0", sha256: "f".repeat(64) });
});

test("the fixture's environment is restored: no test leaves CI variables behind", () => {
  const before = { GITHUB_ACTIONS: process.env.GITHUB_ACTIONS, GITHUB_SHA: process.env.GITHUB_SHA, GITHUB_RUN_ID: process.env.GITHUB_RUN_ID };
  mint();
  assert.deepEqual({ GITHUB_ACTIONS: process.env.GITHUB_ACTIONS, GITHUB_SHA: process.env.GITHUB_SHA, GITHUB_RUN_ID: process.env.GITHUB_RUN_ID }, before);
});
