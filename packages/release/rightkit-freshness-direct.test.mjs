import assert from "node:assert/strict";
import test from "node:test";
import { evaluateRightKitFreshness, satisfiesRequirement } from "./rightkit-registry-resolver.mjs";

const snapshot = {
  entries: [
    { ecosystem: "cargo", name: "rightkit-daemon", version: "0.5.4" },
    { ecosystem: "cargo", name: "rightkit-fs", version: "0.4.19" },
    { ecosystem: "cargo", name: "rightkit-fs", version: "0.5.0" },
    { ecosystem: "npm", name: "@rightkit/release", version: "0.6.4" },
    { ecosystem: "npm", name: "@rightkit/release", version: "0.6.81" },
  ],
  latest: [
    { ecosystem: "cargo", name: "rightkit-daemon", latest: "0.9.1", via: "live" },
    { ecosystem: "cargo", name: "rightkit-fs", latest: "0.5.1", via: "live" },
    { ecosystem: "npm", name: "@rightkit/release", latest: "0.6.81", via: "live" },
  ],
};

test("admission refuses a stale pin the app declares, and reports a stale transitive lock entry without refusing it", () => {
  const declared = new Map([["cargo\0rightkit-daemon", ["0.5.4"]]]);
  const verdict = evaluateRightKitFreshness(snapshot, null, { enforce: true, declared });
  assert.deepEqual(verdict.refusals.map((r) => r.split(" ")[0]), ["rightkit-daemon"]);
  assert.ok(verdict.warnings.some((w) => /^rightkit-fs 0\.4\.19 is in the lock only through other RightKit units/.test(w)));
});

test("a name the app declares is direct only at a version its own requirement admits (Pulse 0.6.81: rightkit-fs =0.5.0, @rightkit/release 0.6.81)", () => {
  const declared = new Map([["cargo\0rightkit-fs", ["=0.5.0"]], ["npm\0@rightkit/release", ["0.6.81"]]]);
  const verdict = evaluateRightKitFreshness(snapshot, null, { enforce: true, declared });
  // rightkit-fs 0.5.0 is the declared pin and is behind 0.5.1: refused. 0.4.19 and @rightkit/release 0.6.4 are other units' entries: warnings.
  assert.deepEqual(verdict.refusals.map((r) => r.split(" ").slice(0, 3).join(" ")), ["rightkit-fs pinned 0.5.0"]);
  assert.ok(verdict.warnings.some((w) => w.startsWith("rightkit-fs 0.4.19 ")));
  assert.ok(verdict.warnings.some((w) => w.startsWith("@rightkit/release 0.6.4 ")));
});

test("a declared name whose requirement cannot be read stays direct; with no declared set every stale entry is judged", () => {
  const unread = evaluateRightKitFreshness(snapshot, null, { enforce: true, declared: new Map([["cargo\0rightkit-fs", [null]]]) });
  assert.deepEqual(unread.refusals.map((r) => r.split(" ").slice(0, 3).join(" ")), ["rightkit-fs pinned 0.4.19", "rightkit-fs pinned 0.5.0"]);
  const all = evaluateRightKitFreshness(snapshot, null, { enforce: true });
  assert.equal(all.refusals.length, 4);
});

test("requirements are read as each ecosystem reads them", () => {
  assert.equal(satisfiesRequirement("0.5.0", "=0.5.0", "cargo"), true);
  assert.equal(satisfiesRequirement("0.4.19", "=0.5.0", "cargo"), false);
  assert.equal(satisfiesRequirement("0.4.19", "0.4", "cargo"), true); // Cargo bare = caret
  assert.equal(satisfiesRequirement("0.5.0", "0.4", "cargo"), false);
  assert.equal(satisfiesRequirement("1.2.3", "^1.0.0", "cargo"), true);
  assert.equal(satisfiesRequirement("0.6.4", "0.6.81", "npm"), false); // npm bare = exact
  assert.equal(satisfiesRequirement("0.6.81", "0.6.81", "npm"), true);
  assert.equal(satisfiesRequirement("0.6.90", "^0.6.81", "npm"), true);
  assert.equal(satisfiesRequirement("0.6.90", "~0.6.81", "npm"), true);
  assert.equal(satisfiesRequirement("0.7.0", "~0.6.81", "npm"), false);
  assert.equal(satisfiesRequirement("0.5.3", ">=0.5.0, <0.6", "cargo"), true);
  assert.equal(satisfiesRequirement("1.0.0", "1.x || 2.x", "npm"), null);
});
