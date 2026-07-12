import assert from "node:assert/strict";
import test from "node:test";
import { isReleaseObject } from "./prune-r2.mjs";

test("pruning only targets release objects for the active OS", () => {
  assert.equal(isReleaseObject("viewright", "mac", "viewright/installers/mac/ViewRight.dmg"), true);
  assert.equal(isReleaseObject("viewright", "mac", "viewright/updates/mac/current/ViewRight.app.tar.gz"), true);
  assert.equal(isReleaseObject("viewright", "mac", "viewright/installers/windows/ViewRight-Setup.exe"), false);
  assert.equal(isReleaseObject("viewright", "windows", "viewright/installers/mac/ViewRight.dmg"), false);
  assert.equal(isReleaseObject("viewright", "mac", "viewright/models/mac/model.bin"), false);
  assert.equal(isReleaseObject("viewright", "mac", "other/installers/mac/Other.dmg"), false);
});
