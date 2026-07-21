import assert from "node:assert/strict";
import { lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, symlinkSync, unlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { createTargetBridge } from "./target-bridge.mjs";

function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-target-bridge-"));
  const target = path.join(root, "shared", "target");
  const link = path.join(root, "app", "src-tauri", "target");
  mkdirSync(target, { recursive: true });
  mkdirSync(path.dirname(link), { recursive: true });
  writeFileSync(path.join(target, "keep.txt"), "shared-cache\n");
  return { root, target, link };
}

test("removes an invocation-created bridge after success without touching the shared target", async () => {
  const fx = fixture();
  const bridge = createTargetBridge(fx);
  await bridge.run(async () => {
    bridge.ensure();
    assert.equal(realpathSync(fx.link), realpathSync(fx.target));
  });
  assert.throws(() => lstatSync(fx.link), { code: "ENOENT" });
  assert.equal(readFileSync(path.join(fx.target, "keep.txt"), "utf8"), "shared-cache\n");
});

test("removes an invocation-created bridge after the build throws", async () => {
  const fx = fixture();
  const bridge = createTargetBridge(fx);
  await assert.rejects(
    bridge.run(async () => {
      bridge.ensure();
      throw new Error("package failed");
    }),
    /package failed/,
  );
  assert.throws(() => lstatSync(fx.link), { code: "ENOENT" });
});

test("removes an invocation-created bridge after a signal-style abort", async () => {
  const fx = fixture();
  const bridge = createTargetBridge(fx);
  await assert.rejects(
    bridge.run(async () => {
      bridge.ensure();
      throw new Error("release interrupted by SIGTERM");
    }),
    /SIGTERM/,
  );
  assert.throws(() => lstatSync(fx.link), { code: "ENOENT" });
});

test("accepts but preserves a pre-existing exact shared-target bridge", async () => {
  const fx = fixture();
  symlinkSync(fx.target, fx.link, process.platform === "win32" ? "junction" : "dir");
  await createTargetBridge(fx).run(async (bridge) => {
    assert.deepEqual(bridge.ensure(), { created: false, link: fx.link, target: fx.target });
  });
  assert.equal(realpathSync(fx.link), realpathSync(fx.target));
  assert.equal(readFileSync(path.join(fx.target, "keep.txt"), "utf8"), "shared-cache\n");
});

test("fails closed when the new bridge is replaced immediately after symlink creation", () => {
  const fx = fixture();
  const other = path.join(fx.root, "racing-target");
  mkdirSync(other);
  const bridge = createTargetBridge({
    ...fx,
    symlink(target, link, type) {
      symlinkSync(target, link, type);
      unlinkSync(link);
      symlinkSync(other, link, type);
    },
  });
  assert.throws(() => bridge.ensure(), /owned shared cache target/i);
  assert.equal(realpathSync(fx.link), realpathSync(other));
});

test("fails closed and preserves an unrelated symlink", async () => {
  const fx = fixture();
  const other = path.join(fx.root, "other-target");
  mkdirSync(other);
  symlinkSync(other, fx.link, process.platform === "win32" ? "junction" : "dir");
  await assert.rejects(
    createTargetBridge(fx).run(async (bridge) => bridge.ensure()),
    /not the owned shared cache target/i,
  );
  assert.equal(realpathSync(fx.link), realpathSync(other));
});

test("preserves a same-target symlink that replaces an acquired bridge", () => {
  const fx = fixture();
  let successfulStats = 0;
  const bridge = createTargetBridge({
    ...fx,
    lstat(file) {
      const stats = lstatSync(file);
      successfulStats += 1;
      if (successfulStats === 1) return stats;
      return { dev: stats.dev, ino: stats.ino + 1, isSymbolicLink: () => stats.isSymbolicLink() };
    },
  });
  bridge.ensure();
  unlinkSync(fx.link);
  symlinkSync(fx.target, fx.link, process.platform === "win32" ? "junction" : "dir");
  assert.equal(bridge.release(), false);
  assert.equal(realpathSync(fx.link), realpathSync(fx.target));
});

test("preserves a different-target symlink that replaces an acquired bridge", () => {
  const fx = fixture();
  const other = path.join(fx.root, "replacement-target");
  mkdirSync(other);
  const bridge = createTargetBridge(fx);
  bridge.ensure();
  unlinkSync(fx.link);
  symlinkSync(other, fx.link, process.platform === "win32" ? "junction" : "dir");
  assert.equal(bridge.release(), false);
  assert.equal(realpathSync(fx.link), realpathSync(other));
});

test("fails closed and preserves a real target directory with user content", async () => {
  const fx = fixture();
  mkdirSync(fx.link);
  writeFileSync(path.join(fx.link, "user.txt"), "keep\n");
  await assert.rejects(
    createTargetBridge(fx).run(async (bridge) => bridge.ensure()),
    /not a symbolic link/i,
  );
  assert.equal(readFileSync(path.join(fx.link, "user.txt"), "utf8"), "keep\n");
});

test("cleanup is idempotent and reports process errors without masking a build failure", async () => {
  const fx = fixture();
  const bridge = createTargetBridge(fx);
  bridge.ensure();
  assert.equal(bridge.release(), true);
  assert.equal(bridge.release(), false);

  const cleanupFailure = new Error("unlink denied");
  const failing = createTargetBridge({ ...fx, unlink: () => { throw cleanupFailure; } });
  const buildFailure = new Error("build aborted");
  await assert.rejects(
    failing.run(async () => {
      failing.ensure();
      throw buildFailure;
    }),
    (error) => error === buildFailure && error.cleanupError === cleanupFailure,
  );
  assert.equal(lstatSync(fx.link).isSymbolicLink(), true);
  unlinkSync(fx.link);
});
