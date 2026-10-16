import assert from "node:assert/strict";
import test from "node:test";
import { isReleaseObject, previousVersionKeys } from "./prune-r2.mjs";

test("pruning only targets release objects for the active OS", () => {
  assert.equal(isReleaseObject("viewright", "mac", "viewright/installers/mac/ViewRight.dmg"), true);
  assert.equal(isReleaseObject("viewright", "mac", "viewright/updates/mac/current/ViewRight.app.tar.gz"), true);
  assert.equal(isReleaseObject("viewright", "mac", "viewright/installers/windows/ViewRight-Setup.exe"), false);
  assert.equal(isReleaseObject("viewright", "windows", "viewright/installers/mac/ViewRight.dmg"), false);
  assert.equal(isReleaseObject("viewright", "mac", "viewright/models/mac/model.bin"), false);
  assert.equal(isReleaseObject("viewright", "mac", "other/installers/mac/Other.dmg"), false);
});

test("pruning retains the newest previous versioned install source beyond the kept keys", () => {
  const k = (v) => `app/installers/windows/versions/${v}/App_${v}.msix`;
  const keys = [k("0.1.0.0"), k("0.1.1.0"), k("0.1.9.0"), k("0.1.10.0"), "app/installers/windows/current/App.msix"];
  const kept = previousVersionKeys({ app: "app", platform: "windows", keys, keep: [k("0.1.10.0")], previous: 1 });
  assert.deepEqual([...kept], [k("0.1.9.0")]);
  assert.deepEqual([...previousVersionKeys({ app: "app", platform: "windows", keys: [k("0.1.0.0")], keep: [k("0.1.0.0")] })], []);
});
