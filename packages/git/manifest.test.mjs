import test from "node:test";
import assert from "node:assert/strict";
import { validateManifest } from "./manifest.mjs";

const VALID_MINIMAL = {
  schemaVersion: 1,
  lanes: ["ci"],
  matrix: { os: ["ubuntu-latest"], node: ["22.13"] },
  packageManager: "pnpm@11.18.0",
  templateVersion: "1.0.0",
};

test("accepts a minimal valid manifest", () => {
  const { valid, errors } = validateManifest(VALID_MINIMAL);
  assert.equal(valid, true);
  assert.deepEqual(errors, []);
});

test("rejects non-object input", () => {
  for (const bad of [null, undefined, "not an object", 42, ["array"]]) {
    const { valid, errors } = validateManifest(bad);
    assert.equal(valid, false);
    assert.ok(errors.length > 0);
  }
});

test("rejects an unknown top-level field", () => {
  const { valid, errors } = validateManifest({ ...VALID_MINIMAL, extra: true });
  assert.equal(valid, false);
  assert.ok(errors.some((e) => e.includes("unknown top-level field: extra")));
});

test("rejects schemaVersion other than 1", () => {
  const { valid, errors } = validateManifest({ ...VALID_MINIMAL, schemaVersion: 2 });
  assert.equal(valid, false);
  assert.ok(errors.some((e) => e.includes("schemaVersion")));
});

test("rejects an empty or unknown lane list", () => {
  assert.equal(validateManifest({ ...VALID_MINIMAL, lanes: [] }).valid, false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, lanes: ["not-a-real-lane"] }).valid, false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, lanes: ["ci", "ci"] }).valid, false);
});

test("rejects a malformed matrix", () => {
  assert.equal(validateManifest({ ...VALID_MINIMAL, matrix: { os: [] , node: ["22.13"] } }).valid, false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, matrix: { os: ["ubuntu-latest"] } }).valid, false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, matrix: "ubuntu-latest" }).valid, false);
});

test("rejects a malformed packageManager or templateVersion", () => {
  assert.equal(validateManifest({ ...VALID_MINIMAL, packageManager: "pnpm" }).valid, false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, packageManager: "bun@1.0.0" }).valid, false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, templateVersion: "v1" }).valid, false);
});

test("publish-npm lane requires publish.npm with oidc true", () => {
  assert.equal(validateManifest({ ...VALID_MINIMAL, lanes: ["publish-npm"] }).valid, false);
  assert.equal(
    validateManifest({ ...VALID_MINIMAL, lanes: ["publish-npm"], publish: { npm: { oidc: false, provenance: true } } }).valid,
    false,
  );
  assert.equal(
    validateManifest({ ...VALID_MINIMAL, lanes: ["publish-npm"], publish: { npm: { oidc: true, provenance: true } } }).valid,
    true,
  );
});

test("publish-package-managers lane requires both homebrew and winget config", () => {
  const base = { ...VALID_MINIMAL, lanes: ["publish-package-managers"] };
  assert.equal(validateManifest(base).valid, false);
  assert.equal(
    validateManifest({ ...base, publish: { homebrew: { tap: "acme/tap", formulaName: "widgets" } } }).valid,
    false,
  );
  assert.equal(
    validateManifest({
      ...base,
      publish: {
        homebrew: { tap: "acme/tap", formulaName: "widgets" },
        winget: { packageIdentifier: "Acme.Widgets" },
      },
    }).valid,
    true,
  );
});

test("rejects a malformed homebrew tap (must be owner/repo)", () => {
  const result = validateManifest({
    ...VALID_MINIMAL,
    lanes: ["publish-package-managers"],
    publish: {
      homebrew: { tap: "not-owner-slash-repo", formulaName: "widgets" },
      winget: { packageIdentifier: "Acme.Widgets" },
    },
  });
  assert.equal(result.valid, false);
});
