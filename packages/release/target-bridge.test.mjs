import assert from "node:assert/strict";
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, unlinkSync, writeFileSync } from "node:fs";
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

test("Windows creates a directory symlink instead of an untrusted junction", () => {
  const fx = fixture();
  let requestedType;
  const bridge = createTargetBridge({
    ...fx,
    platform: "win32",
    symlink(target, link, type) {
      requestedType = type;
      symlinkSync(target, link, type);
    },
  });
  bridge.ensure();
  assert.equal(requestedType, "dir");
  assert.equal(realpathSync(fx.link), realpathSync(fx.target));
  bridge.release();
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

test("unmanaged target migration rejects a real target without creating the shared cache destination", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-target-bridge-pure-"));
  const target = path.join(root, "shared", "target");
  const link = path.join(root, "app", "src-tauri", "target");
  mkdirSync(link, { recursive: true });

  assert.throws(() => createTargetBridge({ link, target }).ensure(), /not a symbolic link/i);
  assert.equal(existsSync(target), false);
});

test("broker-managed: a real target directory is left untouched instead of treated as corruption", async () => {
  const fx = fixture();
  mkdirSync(fx.link);
  writeFileSync(path.join(fx.link, "user.txt"), "keep\n");
  const result = await createTargetBridge({ ...fx, brokerManaged: true }).run(async (bridge) => bridge.ensure());
  assert.equal(result.brokerManaged, true);
  assert.equal(result.created, false);
  // Nothing was removed, replaced, or relinked: the real directory and its
  // content are exactly as they were, and no shared-cache destination was
  // fabricated in its place.
  assert.equal(readFileSync(path.join(fx.link, "user.txt"), "utf8"), "keep\n");
  assert.equal(lstatSync(fx.link).isSymbolicLink(), false);
});

test("broker-managed: release() is a safe no-op after leaving a real target untouched", () => {
  const fx = fixture();
  mkdirSync(fx.link);
  writeFileSync(path.join(fx.link, "user.txt"), "keep\n");
  const bridge = createTargetBridge({ ...fx, brokerManaged: true });
  bridge.ensure();
  assert.equal(bridge.release(), false);
  assert.equal(readFileSync(path.join(fx.link, "user.txt"), "utf8"), "keep\n");
});

test("broker-managed has no effect on the non-broker path: a symlink bridge still behaves exactly as before", async () => {
  const fx = fixture();
  const result = await createTargetBridge({ ...fx, brokerManaged: true }).run(async (bridge) => {
    const ensured = bridge.ensure();
    assert.equal(lstatSync(fx.link).isSymbolicLink(), true);
    return ensured;
  });
  assert.equal(result.created, true);
  assert.equal(result.brokerManaged, undefined);
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

test("a stale link from an earlier fingerprint is repointed, not a hard stop", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "bridge-stale-"));
  const ownedRoot = path.join(dir, "cache", "heardright");
  const oldTarget = path.join(ownedRoot, "aaaaaaaa");
  const newTarget = path.join(ownedRoot, "bbbbbbbb");
  mkdirSync(oldTarget, { recursive: true });
  const link = path.join(dir, "target");
  symlinkSync(oldTarget, link, "junction");

  const bridge = createTargetBridge({ link, target: newTarget, ownedRoot });
  const result = bridge.ensure();

  assert.equal(result.created, true);
  assert.equal(realpathSync(link), realpathSync(newTarget));
  // The directory the stale link pointed at must survive: it is cache, and the
  // link was only ever a pointer to it.
  assert.equal(existsSync(oldTarget), true);
  rmSync(dir, { recursive: true, force: true });
});

test("a link pointing outside the owned root is still refused", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "bridge-foreign-"));
  const ownedRoot = path.join(dir, "cache", "heardright");
  const foreign = path.join(dir, "somewhere-else");
  const newTarget = path.join(ownedRoot, "bbbbbbbb");
  mkdirSync(foreign, { recursive: true });
  const link = path.join(dir, "target");
  symlinkSync(foreign, link, "junction");

  const bridge = createTargetBridge({ link, target: newTarget, ownedRoot });
  assert.throws(() => bridge.ensure(), /refusing to replace it/);
  assert.equal(existsSync(newTarget), false);
  assert.equal(realpathSync(link), realpathSync(foreign));
  rmSync(dir, { recursive: true, force: true });
});

test("a dangling link whose cache entry was pruned is reclaimed", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "bridge-dangling-"));
  const ownedRoot = path.join(dir, "cache", "heardright");
  const pruned = path.join(ownedRoot, "aaaaaaaa");
  const newTarget = path.join(ownedRoot, "bbbbbbbb");
  mkdirSync(pruned, { recursive: true });
  const link = path.join(dir, "target");
  symlinkSync(pruned, link, "junction");
  rmSync(pruned, { recursive: true, force: true });

  const bridge = createTargetBridge({ link, target: newTarget, ownedRoot });
  assert.equal(bridge.ensure().created, true);
  assert.equal(realpathSync(link), realpathSync(newTarget));
  rmSync(dir, { recursive: true, force: true });
});
