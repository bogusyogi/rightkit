import assert from "node:assert/strict";
import test from "node:test";
import { evaluateRightKitFreshness } from "./rightkit-registry-resolver.mjs";

const snapshot = {
  entries: [
    { ecosystem: "cargo", name: "rightkit-daemon", version: "0.5.4" },
    { ecosystem: "cargo", name: "rightkit-fs", version: "0.4.19" },
    { ecosystem: "npm", name: "@rightkit/license", version: "0.3.4" },
  ],
  latest: [
    { ecosystem: "cargo", name: "rightkit-daemon", latest: "0.9.1", via: "live" },
    { ecosystem: "cargo", name: "rightkit-fs", latest: "0.5.0", via: "live" },
    { ecosystem: "npm", name: "@rightkit/license", latest: "0.3.4", via: "live" },
  ],
};

test("admission refuses a stale pin the app declares, and reports a stale transitive lock entry without refusing it", () => {
  const declared = new Set(["cargo\0rightkit-daemon", "npm\0@rightkit/license"]);
  const verdict = evaluateRightKitFreshness(snapshot, null, { enforce: true, declared });
  assert.equal(verdict.refusals.length, 1);
  assert.match(verdict.refusals[0], /^rightkit-daemon pinned 0\.5\.4 -> latest 0\.9\.1/);
  assert.ok(verdict.warnings.some((w) => /^rightkit-fs 0\.4\.19 is in the lock only through other RightKit units \(latest 0\.5\.0\)/.test(w)));
});

test("with no declared set every stale entry is judged, as before", () => {
  const verdict = evaluateRightKitFreshness(snapshot, null, { enforce: true });
  assert.deepEqual(verdict.refusals.map((r) => r.split(" ")[0]), ["rightkit-daemon", "rightkit-fs"]);
});
