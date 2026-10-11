import assert from "node:assert/strict";
import test from "node:test";
import { rightKitVersionAllowed } from "./rightkit-version-policy.mjs";

const allowed = new Set(["0.1.0", "0.2.0", "0.4.0", "0.4.1", "0.6.1"]);

test("recorded versions are allowed", () => {
  for (const v of allowed) assert.equal(rightKitVersionAllowed(allowed, v), true, v);
});

test("an unlisted exact version between the recorded minimum and maximum is allowed (republished or skipped patch)", () => {
  for (const v of ["0.3.0", "0.5.0", "0.6.0", "0.1.1"]) assert.equal(rightKitVersionAllowed(allowed, v), true, v);
});

test("a version below the minimum or above everything recorded is refused", () => {
  for (const v of ["0.0.9", "0.6.2", "0.7.0", "1.0.0"]) assert.equal(rightKitVersionAllowed(allowed, v), false, v);
});

test("ranges, tags, prereleases and non-strings are never accepted by the range rule", () => {
  for (const v of ["^0.4.0", "~0.5.0", ">=0.1.0", "latest", "0.5.0-rc.1", "workspace:*", "", undefined, null, 5]) assert.equal(rightKitVersionAllowed(allowed, v), false, String(v));
});

test("a package with no exact recorded versions allows nothing by range", () => {
  assert.equal(rightKitVersionAllowed(new Set(["workspace"]), "0.1.0"), false);
  assert.equal(rightKitVersionAllowed(new Set(), "0.1.0"), false);
});
