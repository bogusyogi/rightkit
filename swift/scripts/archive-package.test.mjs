import assert from "node:assert/strict";
import test from "node:test";

import { buildArchive, checksum, packageVersion } from "./archive-package.mjs";

test("the same commit always produces the same archive bytes", () => {
  const first = buildArchive();
  const second = buildArchive();

  assert.ok(first.length > 0, "archive is empty");
  assert.equal(checksum(first), checksum(second));
  assert.deepEqual(first, second);
});

test("the archive is cut from the commit, not the working tree", () => {
  // A scratch edit must not reach a release. Same tree-ish, same bytes; a different one differs.
  const head = checksum(buildArchive("HEAD"));
  const parent = checksum(buildArchive("HEAD~1"));

  assert.notEqual(head, parent, "HEAD and HEAD~1 hashed the same — the tree-ish is being ignored");
});

test("the version is semver", () => {
  assert.match(packageVersion(), /^\d+\.\d+\.\d+$/);
});
