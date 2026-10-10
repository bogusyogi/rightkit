import assert from "node:assert/strict";
import test from "node:test";
import { ensureLegacyTargetLink } from "./release-cache-mode.mjs";

function fakeFs(entries) {
  const calls = [];
  return {
    calls,
    existsSync: (p) => p in entries,
    realpathSync: (p) => entries[p],
    renameSync: (a, b) => { calls.push(["rename", a, b]); entries[b] = entries[a]; delete entries[a]; },
    symlinkSync: (t, p, kind) => { calls.push(["link", t, p, kind]); entries[p] = entries[t]; },
  };
}
const base = { targetLink: "/repo/engine/target", cacheTarget: "/vault/target", platform: "darwin", now: () => 42 };

test("missing target is linked; an existing correct link is left alone", () => {
  const fs = fakeFs({ "/vault/target": "/vault/target" });
  assert.equal(ensureLegacyTargetLink({ ...base, env: {}, fs }).action, "linked");
  assert.equal(ensureLegacyTargetLink({ ...base, env: {}, fs }).action, "current");
});

test("a foreign target fails closed on developer hosts", () => {
  const fs = fakeFs({ "/vault/target": "/vault/target", "/repo/engine/target": "/repo/engine/target" });
  const result = ensureLegacyTargetLink({ ...base, env: {}, fs });
  assert.equal(result.action, "refused");
  assert.match(result.reason, /not the release cache link/);
  assert.deepEqual(fs.calls, []);
});

test("hosted CI moves a pre-existing target aside and links the cache", () => {
  const fs = fakeFs({ "/vault/target": "/vault/target", "/repo/engine/target": "/repo/engine/target" });
  const result = ensureLegacyTargetLink({ ...base, env: { GITHUB_ACTIONS: "true" }, fs });
  assert.deepEqual(result, { action: "moved-aside", aside: "/repo/engine/target.pre-right-release-42" });
  assert.deepEqual(fs.calls, [["rename", "/repo/engine/target", "/repo/engine/target.pre-right-release-42"], ["link", "/vault/target", "/repo/engine/target", "dir"]]);
});

test("Windows uses a junction", () => {
  const fs = fakeFs({ "/vault/target": "/vault/target" });
  ensureLegacyTargetLink({ ...base, platform: "win32", env: {}, fs });
  assert.equal(fs.calls[0][3], "junction");
});
