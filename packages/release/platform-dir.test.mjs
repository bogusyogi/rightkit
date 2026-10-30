// One platform directory under .right-release/sealed (audit: the gate read `win`, the build sealed into `windows`). The writer here is the real
// release-state.mjs state machine writing to the real filesystem, the reader the real gate-stages.mjs sealedFor: no mocked sealedFor.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { sealedFor } from "./gate-stages.mjs";
import { platformDirOf, platformOfDir } from "./platform-dir.mjs";
import { runBuildStateMachine } from "./release-state.mjs";

const tmp = (t) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-platdir-"))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
const COMMIT = "0123456789abcdef0123456789abcdef01234567";

test("platformDirOf: win and windows are one directory, mac android ios as is, a name that could leave the directory is refused", () => {
  assert.equal(platformDirOf("win"), "windows"); assert.equal(platformDirOf("windows"), "windows");
  for (const p of ["mac", "android", "ios"]) assert.equal(platformDirOf(p), p);
  assert.equal(platformOfDir("windows"), "win"); assert.equal(platformOfDir("mac"), "mac");
  for (const bad of ["../x", "a/b", "", undefined, null, 3, "Win"]) assert.throws(() => platformDirOf(bad), TypeError, String(bad));
});

test("the gate's reader finds what the build's writer sealed, for win and for mac, by the CLI's own platform value", async (t) => {
  for (const [platform, dir] of [["win", "windows"], ["mac", "mac"]]) {
    const root = tmp(t);
    let wrote = null;
    const sealed = await runBuildStateMachine({ root, app: "app", version: "1.0.0", commit: COMMIT, platform, ops: { seal: ({ sealedDir }) => { wrote = sealedDir; fs.mkdirSync(sealedDir, { recursive: true }); fs.writeFileSync(path.join(sealedDir, "release-manifest.json"), JSON.stringify({ app: "app", platform })); } } });
    assert.equal(path.basename(wrote), dir, `${platform}: the writer seals into ${dir}`);
    assert.equal(fs.existsSync(path.join(root, ".right-release", "sealed", sealed.releaseId, platform)), platform === dir, `${platform}: no directory by the CLI spelling unless it is the same`);
    const found = sealedFor({ root, commit: COMMIT, platform });
    assert.ok(found, `${platform}: the reader finds the sealed set`);
    assert.equal(found.id, sealed.releaseId);
    assert.equal(found.manifest, path.join(wrote, "release-manifest.json"));
    assert.match(found.digest, /^[0-9a-f]{64}$/);
    if (platform === "win") assert.deepEqual(sealedFor({ root, commit: COMMIT, platform: "windows" }), found, "either spelling reads the same directory");
    assert.equal(sealedFor({ root, commit: COMMIT, platform: platform === "win" ? "mac" : "win" }), null, "the other platform has no sealed set");
  }
});
