// GB-48 (GATE 34/36/44, E17): `check --commit <sha> --full-a` reads the selected commit's snapshot under ownership on a managed host; it
// never converts to `--worktree` and runs the live checkout; hosted CI keeps its in-place full check (its runner checkout is the commit).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { doorFixture } from "./test-support/door-fixture.mjs";

const POSIX_SIM = process.platform === "win32" ? "simulates a Linux host, whose ownership reads ps" : false;

test("[GB-48] direct full-A entry with a dirty checkout, HEAD ahead and an untracked file reads the selected commit's snapshot only: no --worktree, no refusal", { skip: POSIX_SIM }, async (t) => {
  const fx = doorFixture(t, { "README.md": "committed\n", "package.json": '{"name":"x","private":true}\n' }, "rk-gb48-");
  const selected = fx.commit();
  fx.write("README.md", "second commit\n"); fx.git("commit", "-qam", "ahead");
  fx.write("README.md", "UNCOMMITTED EDIT\n"); fx.write("untracked.txt", "u\n");
  const roots = [];
  const runScans = async (plan) => { roots.push(plan.roots.contentRoot, plan.roots.liveRoot); return { scans: [], notes: [], cacheHits: [], deferred: [] }; };
  const r = await fx.door(["check", "--commit", selected, "--full-a"], { runScans });
  assert.equal(r.code, 0, r.all);
  assert.doesNotMatch(r.all, /--worktree|working tree|--full-a checks a commit only when it is HEAD|needs a clean checkout/);
  assert.match(r.all, /build ownership held/);
  assert.match(r.all, /snapshot .* is commit /);
  assert.ok(roots.length > 0);
  for (const root of roots) assert.ok(!root.startsWith(fx.repo), `class A read ${root}, which is the checkout`);
  assert.equal(fs.readFileSync(path.join(roots[0], "README.md"), "utf8"), "committed\n", "the scans saw the selected commit's bytes, not HEAD's or the checkout's");
  assert.match(r.all, /class A full pass/);
});

test("[GB-48] a concurrent edit while the check runs cannot reach it, and the entry still refuses nothing for a clean HEAD", { skip: POSIX_SIM }, async (t) => {
  const fx = doorFixture(t, { "README.md": "committed\n" }, "rk-gb48b-");
  const selected = fx.commit();
  const runScans = async (plan) => { fx.write("README.md", "EDITED DURING THE CHECK\n"); return { scans: [], notes: [], cacheHits: [], deferred: [] }; };
  const r = await fx.door(["check", "--commit", selected, "--full-a"], { runScans });
  assert.equal(r.code, 0, r.all);
  assert.equal(fs.readFileSync(path.join(fx.snapshot(), "README.md"), "utf8"), "committed\n");
});

test("[GB-48] hosted CI keeps the in-place full check of its runner checkout (the commit); a differing checkout is still refused there", { skip: POSIX_SIM }, async (t) => {
  const fx = doorFixture(t, { "README.md": "committed\n" }, "rk-gb48c-");
  const hosted = { env: { ...fx.env, GITHUB_ACTIONS: "true" } };
  fx.write("README.md", "dirty\n");
  const r = await fx.door(["check", "--commit", "HEAD", "--full-a"], { ...hosted, runScans: async () => ({ scans: [], notes: [], cacheHits: [], deferred: [] }) });
  assert.equal(r.code, 2, r.all);
  assert.match(r.err, /needs a clean checkout/);
});
