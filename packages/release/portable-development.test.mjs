import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { installPortableDevelopment } from "./portable-development.mjs";

function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rightkit-portable-development-"));
  const source = path.join(root, "dist", "headless", "dev-install");
  const installRoot = path.join(root, "user", "CodeRight");
  fs.mkdirSync(path.join(source, "resources"), { recursive: true });
  fs.writeFileSync(path.join(source, "coderight.exe"), "dev-binary\n");
  fs.writeFileSync(path.join(source, "resources", "data.txt"), "payload\n");
  return {
    root,
    source,
    installRoot,
    descriptor: { kind: "portable", sourceDirectory: "dist/headless/dev-install", installRoot, entrypoint: "coderight.exe", activation: { kind: "current" } },
    cleanup() { fs.rmSync(root, { recursive: true, force: true }); },
  };
}

function currentTarget(current) { return fs.realpathSync(current); }

test("stages a full immutable payload and atomically activates current", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  const result = installPortableDevelopment(fx.descriptor, { root: fx.root });
  assert.equal(fs.readFileSync(path.join(result.entrypoint), "utf8"), "dev-binary\n");
  assert.equal(fs.readFileSync(path.join(result.versionRoot, "resources", "data.txt"), "utf8"), "payload\n");
  assert.equal(path.basename(result.versionRoot), `dev-${result.digest}`);
  assert.equal(currentTarget(result.current), fs.realpathSync(result.versionRoot));
  assert.equal(fs.lstatSync(result.current).isSymbolicLink(), true);
});

test("reuses content-identical version and replaces an owned current pointer", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  const first = installPortableDevelopment(fx.descriptor, { root: fx.root });
  const firstCurrent = currentTarget(first.current);
  fs.writeFileSync(path.join(fx.source, "coderight.exe"), "next-dev-binary\n");
  const second = installPortableDevelopment(fx.descriptor, { root: fx.root });
  assert.notEqual(second.versionRoot, first.versionRoot);
  assert.notEqual(currentTarget(second.current), firstCurrent);
  fs.writeFileSync(path.join(fx.source, "coderight.exe"), "dev-binary\n");
  const reused = installPortableDevelopment(fx.descriptor, { root: fx.root });
  assert.equal(reused.versionRoot, first.versionRoot);
  assert.equal(currentTarget(reused.current), firstCurrent);
  assert.deepEqual(fs.readdirSync(fx.installRoot).filter((name) => name.startsWith(".current-backup-")), []);
});

test("replaces an existing release version junction with development current", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  const releaseVersion = path.join(fx.installRoot, "versions", "0.1.115-releasehash");
  fs.mkdirSync(releaseVersion, { recursive: true });
  fs.writeFileSync(path.join(releaseVersion, "coderight.exe"), "release binary\n");
  fs.symlinkSync(releaseVersion, path.join(fx.installRoot, "current"), process.platform === "win32" ? "junction" : "dir");
  const result = installPortableDevelopment(fx.descriptor, { root: fx.root });
  assert.equal(currentTarget(result.current), fs.realpathSync(result.versionRoot));
  assert.equal(fs.readFileSync(path.join(releaseVersion, "coderight.exe"), "utf8"), "release binary\n");
});

test("rejects a missing source entrypoint before switching current", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  const releaseVersion = path.join(fx.installRoot, "versions", "0.1.115-releasehash");
  fs.mkdirSync(releaseVersion, { recursive: true });
  fs.writeFileSync(path.join(releaseVersion, "coderight.exe"), "release binary\n");
  fs.symlinkSync(releaseVersion, path.join(fx.installRoot, "current"), process.platform === "win32" ? "junction" : "dir");
  fs.rmSync(path.join(fx.source, "coderight.exe"));
  assert.throws(() => installPortableDevelopment(fx.descriptor, { root: fx.root }), /ENOENT/);
  assert.equal(currentTarget(path.join(fx.installRoot, "current")), fs.realpathSync(releaseVersion));
  assert.deepEqual(fs.readdirSync(path.join(fx.installRoot, "versions")), ["0.1.115-releasehash"]);
});

test("refuses a real current directory without modifying it", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  fs.mkdirSync(fx.installRoot, { recursive: true });
  fs.mkdirSync(path.join(fx.installRoot, "current"));
  fs.writeFileSync(path.join(fx.installRoot, "current", "keep.txt"), "user data");
  assert.throws(() => installPortableDevelopment(fx.descriptor, { root: fx.root }), /not a symlink or junction/);
  assert.equal(fs.readFileSync(path.join(fx.installRoot, "current", "keep.txt"), "utf8"), "user data");
});

test("rejects source symlinks that escape the payload", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  const outside = path.join(fx.root, "outside.txt");
  fs.writeFileSync(outside, "outside");
  fs.symlinkSync(outside, path.join(fx.source, "escape.txt"));
  assert.throws(() => installPortableDevelopment(fx.descriptor, { root: fx.root }), /symbolic link/);
  assert.equal(fs.existsSync(fx.installRoot), false);
});

test("restores previous current when pointer activation fails", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  const first = installPortableDevelopment(fx.descriptor, { root: fx.root });
  const prior = currentTarget(first.current);
  fs.writeFileSync(path.join(fx.source, "coderight.exe"), "next-dev-binary\n");
  const originalSymlink = fs.symlinkSync;
  t.mock.method(fs, "symlinkSync", function (target, link, type) {
    if (link === path.join(fx.installRoot, "current")) throw new Error("injected activation failure");
    return originalSymlink.call(fs, target, link, type);
  });
  assert.throws(() => installPortableDevelopment(fx.descriptor, { root: fx.root }), /injected activation failure/);
  assert.equal(currentTarget(first.current), prior);
  assert.equal(fs.readdirSync(fx.installRoot).some((name) => name.startsWith(".current-backup-")), false);
});

test("rejects filesystem roots and absolute payload paths", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  assert.throws(() => installPortableDevelopment({ ...fx.descriptor, installRoot: path.parse(fx.root).root }, { root: fx.root }), /non-filesystem-root/);
  assert.throws(() => installPortableDevelopment({ ...fx.descriptor, entrypoint: path.join(fx.source, "coderight.exe") }, { root: fx.root }), /contained relative path/);
});

test("keeps only the active development version and sweeps stage, backup and prune leftovers", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  let last;
  for (let n = 1; n <= 5; n += 1) {
    fs.writeFileSync(path.join(fx.source, "coderight.exe"), `dev-binary-${n}\n`);
    last = installPortableDevelopment(fx.descriptor, { root: fx.root });
  }
  const versions = path.join(fx.installRoot, "versions");
  assert.deepEqual(fs.readdirSync(versions), [path.basename(last.versionRoot)]);
  fs.mkdirSync(path.join(versions, ".stage-1-old"));
  fs.mkdirSync(path.join(versions, ".prune-old"));
  fs.mkdirSync(path.join(fx.installRoot, ".current-backup-1-old"));
  fs.writeFileSync(path.join(fx.source, "coderight.exe"), "dev-binary-6\n");
  const sixth = installPortableDevelopment(fx.descriptor, { root: fx.root });
  assert.deepEqual(fs.readdirSync(versions), [path.basename(sixth.versionRoot)]);
  assert.deepEqual(fs.readdirSync(fx.installRoot).filter((name) => name.startsWith(".")), []);
});

test("a development version a running process uses survives until the next install", async (t) => {
  const { pruneDevelopmentVersions } = await import("./portable-development.mjs");
  const fx = fixture(); t.after(fx.cleanup);
  const versions = path.join(fx.installRoot, "versions");
  const busy = path.join(versions, "dev-aaaa"); const keep = path.join(versions, "dev-bbbb");
  for (const dir of [busy, keep]) { fs.mkdirSync(dir, { recursive: true }); fs.writeFileSync(path.join(dir, "coderight.exe"), "x"); }
  let result = pruneDevelopmentVersions(fx.installRoot, keep, { platform: "darwin", runningExecutables: [path.join(busy, "coderight.exe")] });
  assert.equal(result.removed, 0);
  assert.deepEqual(result.skipped.map((s) => s.reason), ["in-use"]);
  assert.ok(fs.existsSync(busy));
  result = pruneDevelopmentVersions(fx.installRoot, keep, { platform: "darwin", runningExecutables: [] });
  assert.equal(result.removed, 1);
  assert.ok(result.reclaimedBytes > 0);
  assert.deepEqual(fs.readdirSync(versions), ["dev-bbbb"]);
});
