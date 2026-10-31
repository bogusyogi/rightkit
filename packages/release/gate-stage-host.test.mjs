// E2E L3-03: a Mac or Windows artifact is constructed on its native host and never cross-built from another platform (GATE-CONTRACT s1 "Native build
// target"; PLATFORM-MATRIX). On a Linux host `--to package|qualify --platform win|mac|ios` used to be accepted by `--route` and by a real run, which took
// build ownership, synced the snapshot, ran Class A and started the compile before failing late; `--to release` took ownership and the snapshot before it
// said the release runs only in the trusted publisher, without a typed code. Both are now typed refusals from routePlan: the probe and the real run say the
// same thing, and nothing starts (no ownership, snapshot, check, receipt or process but git and which).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { ROUTE_REFUSAL_CODES, main, releaseRouteRefusal } from "./bin/rightkit-gate.mjs";

const tmp = (t, prefix) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
function fixture(t) {
  const dir = tmp(t, "rk-stage-host-");
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  const git = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
  git("init", "-q", "-b", "main"); git("config", "user.email", "t@t"); git("config", "user.name", "t");
  fs.writeFileSync(path.join(repo, "README.md"), "r\n"); fs.writeFileSync(path.join(repo, "right-release.config.json"), "{}\n");
  git("add", "-A"); git("commit", "-q", "-m", "c");
  const sha = git("rev-parse", "HEAD").stdout.trim();
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  for (const k of ["GITHUB_ACTIONS", "TAURI_CONFIG", "RIGHTKIT_PUBLISHER"]) delete env[k];
  const run = async (args, { platform }) => {
    const out = [], err = [], touched = [];
    const spawn = (c, a, opts) => { touched.push(c); return c === "git" ? spawnSync(c, a, { ...opts, encoding: "utf8" }) : { status: 1, stdout: "", stderr: "x" }; };
    const trap = (name) => () => { touched.push(name); throw new Error(`called ${name}`); };
    const code = await main(args, { cwd: repo, platform, env, spawn, snapshotRoot: path.join(dir, "snaps"), out: (m) => out.push(m), err: (m) => err.push(m), resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }),
      acquireOwnership: trap("acquireOwnership"), syncSnapshot: trap("syncSnapshot"), runCheck: trap("runCheck"), runCompile: trap("runCompile"), spawnInherit: trap("spawnInherit"), appCheck: trap("appCheck"), writeReceipt: trap("writeReceipt") });
    const line = out.find((l) => l.startsWith("{"));
    return { code, out: out.join("\n"), err: err.join("\n"), touched, json: line ? JSON.parse(line) : null };
  };
  return { sha, run };
}
const quiet = (touched) => touched.filter((c) => !["git", "which", "where"].includes(c));

test("RK_STAGE_HOST_MISMATCH is a code a probe can print; the release refusal is a real-run code (the probe has no release route)", () => {
  assert.ok(ROUTE_REFUSAL_CODES.includes("RK_STAGE_HOST_MISMATCH"));
  assert.equal(ROUTE_REFUSAL_CODES.includes("RK_RELEASE_PUBLISHER_ONLY"), false);
  assert.equal(releaseRouteRefusal({ env: {} }, { stages: ["check", "release"], platform: "mac" }).code, "RK_RELEASE_PUBLISHER_ONLY");
  assert.equal(releaseRouteRefusal({ env: {} }, { stages: ["check", "release"], platform: "ios" }), null, "a mobile upload has its own route");
  assert.equal(releaseRouteRefusal({ env: { GITHUB_ACTIONS: "true", RIGHTKIT_PUBLISHER: "github-environment" } }, { stages: ["release"], platform: "mac" }), null, "the trusted publisher");
  assert.equal(releaseRouteRefusal({ env: {} }, { stages: ["check", "compile"], platform: "mac" }), null);
});

for (const [platform, stage, target, wantRefusal] of [
  ["linux", "package", "win", true], ["linux", "package", "mac", true], ["linux", "package", "ios", true], ["linux", "qualify", "win", true], ["linux", "qualify", "mac", true],
  ["linux", "package", "android", false],
  ["darwin", "package", "mac", false], ["darwin", "package", "ios", false], ["darwin", "qualify", "mac", false], ["darwin", "package", "win", true], ["darwin", "qualify", "win", true], ["darwin", "package", "android", false],
  ["win32", "package", "win", false], ["win32", "qualify", "win", false], ["win32", "package", "mac", true], ["win32", "package", "ios", true],
]) {
  test(`--to ${stage} --platform ${target} --route on a ${platform} host ${wantRefusal ? "answers RK_STAGE_HOST_MISMATCH" : "has no refusal"}, and a real run agrees before anything starts`, async (t) => {
    const { sha, run } = fixture(t);
    const probe = await run(["--to", stage, "--platform", target, "--route"], { platform });
    assert.equal(probe.code, 0, probe.err);
    assert.equal(probe.json.refusal?.code ?? null, wantRefusal ? "RK_STAGE_HOST_MISMATCH" : null, JSON.stringify(probe.json.refusal));
    assert.deepEqual(quiet(probe.touched), []);
    if (!wantRefusal) return;
    assert.match(probe.json.refusal.message, new RegExp(`--platform ${target}`));
    const real = await run(["--to", stage, "--platform", target, "--commit", sha], { platform });
    assert.equal(real.code, 1, real.err);
    assert.match(real.err, /BLOCKED: RK_STAGE_HOST_MISMATCH: /);
    assert.match(real.err, /nothing ran/);
    assert.deepEqual(quiet(real.touched), [], "no ownership, snapshot, check, receipt or process");
  });
}

test("check, compile, dev and test have no host routing: any --platform on any host", async (t) => {
  const { run } = fixture(t);
  for (const stage of ["check", "compile", "dev", "test"]) for (const platform of ["linux", "darwin", "win32"]) {
    const r = await run(["--to", stage, "--platform", "win", "--route"], { platform });
    assert.equal(r.json.refusal, null, `${stage} on ${platform}`);
  }
});

test("--to release outside the trusted publisher is a typed refusal before ownership and the snapshot (the native host is asked first)", async (t) => {
  const { sha, run } = fixture(t);
  for (const [platform, args, code] of [
    ["darwin", ["--platform", "mac", "--tier", "patch"], "RK_RELEASE_PUBLISHER_ONLY"], ["darwin", ["--platform", "mac", "--tier", "update"], "RK_RELEASE_PUBLISHER_ONLY"],
    ["win32", ["--platform", "win", "--tier", "patch"], "RK_RELEASE_PUBLISHER_ONLY"], ["linux", ["--platform", "win", "--tier", "patch"], "RK_STAGE_HOST_MISMATCH"], ["linux", ["--platform", "mac", "--tier", "update"], "RK_STAGE_HOST_MISMATCH"],
  ]) {
    const r = await run(["--to", "release", ...args, "--commit", sha], { platform });
    assert.equal(r.code, 1, r.err);
    assert.match(r.err, new RegExp(`BLOCKED: ${code}: `));
    if (code === "RK_RELEASE_PUBLISHER_ONLY") assert.match(r.err, /--to release runs only in the trusted publisher/);
    assert.deepEqual(quiet(r.touched), [], `${platform} ${args.join(" ")}`);
  }
});
