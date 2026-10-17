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

const read = (...parts) => fs.readFileSync(path.join(...parts), "utf8");

test("installs in place at the fixed current directory with no versions folder", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  const result = installPortableDevelopment(fx.descriptor, { root: fx.root });
  assert.equal(result.current, path.join(fx.installRoot, "current"));
  assert.equal(fs.lstatSync(result.current).isDirectory(), true);
  assert.equal(fs.lstatSync(result.current).isSymbolicLink(), false);
  assert.equal(fs.readFileSync(result.entrypoint, "utf8"), "dev-binary\n");
  assert.equal(read(result.current, "resources", "data.txt"), "payload\n");
  assert.deepEqual(fs.readdirSync(fx.installRoot), ["current"]);
});

test("a second install replaces current in place and keeps NO current.previous (a stale one is removed)", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  const first = installPortableDevelopment(fx.descriptor, { root: fx.root });
  fs.mkdirSync(path.join(fx.installRoot, "current.previous"), { recursive: true });
  fs.writeFileSync(path.join(fx.installRoot, "current.previous", "coderight.exe"), "stale\n");
  fs.writeFileSync(path.join(fx.source, "coderight.exe"), "second\n");
  const second = installPortableDevelopment(fx.descriptor, { root: fx.root });
  assert.equal(second.current, first.current);
  assert.equal("previous" in second, false);
  fs.writeFileSync(path.join(fx.source, "coderight.exe"), "third\n");
  installPortableDevelopment(fx.descriptor, { root: fx.root });
  assert.deepEqual(fs.readdirSync(fx.installRoot), ["current"]);
  assert.equal(read(fx.installRoot, "current", "coderight.exe"), "third\n");
});

test("user data beside current (coderight.db, WAL, models) is never copied, moved or changed, on success and on a failed install", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  installPortableDevelopment(fx.descriptor, { root: fx.root });
  const data = new Map([["coderight.db", "db-bytes"], ["coderight.db-wal", "wal-bytes"], [path.join("models", "x.bin"), "model-bytes"]]);
  for (const [name, text] of data) { fs.mkdirSync(path.dirname(path.join(fx.installRoot, name)), { recursive: true }); fs.writeFileSync(path.join(fx.installRoot, name), text); }
  const snapshot = () => [...data.keys()].map((name) => [name, read(fx.installRoot, name)]);
  const before = snapshot();
  installPortableDevelopment(fx.descriptor, { root: fx.root });
  assert.deepEqual(snapshot(), before);
  assert.deepEqual(fs.readdirSync(fx.installRoot).sort(), ["coderight.db", "coderight.db-wal", "current", "models"], "no .previous or backup beside the data");
  const current = path.join(fx.installRoot, "current");
  const originalRename = fs.renameSync;
  t.mock.method(fs, "renameSync", function (from, to) {
    if (to === current) throw new Error("injected swap failure");
    return originalRename.call(fs, from, to);
  });
  assert.throws(() => installPortableDevelopment(fx.descriptor, { root: fx.root }), /injected swap failure/);
  assert.deepEqual(snapshot(), before);
  assert.deepEqual(fs.readdirSync(fx.installRoot).sort(), ["coderight.db", "coderight.db-wal", "models"], "the failed install left current absent and kept no old copy");
});

test("removes a legacy current junction without touching its target and keeps no .previous", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  const legacy = path.join(fx.installRoot, "versions", "0.1.115-releasehash");
  fs.mkdirSync(legacy, { recursive: true });
  fs.writeFileSync(path.join(legacy, "coderight.exe"), "release binary\n");
  fs.symlinkSync(legacy, path.join(fx.installRoot, "current"), process.platform === "win32" ? "junction" : "dir");
  const result = installPortableDevelopment(fx.descriptor, { root: fx.root });
  assert.equal(fs.lstatSync(result.current).isSymbolicLink(), false);
  assert.equal(read(legacy, "coderight.exe"), "release binary\n");
  assert.equal(fs.existsSync(path.join(fx.installRoot, "current.previous")), false);
});

test("rejects a missing source entrypoint before touching current", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  installPortableDevelopment(fx.descriptor, { root: fx.root });
  fs.rmSync(path.join(fx.source, "coderight.exe"));
  assert.throws(() => installPortableDevelopment(fx.descriptor, { root: fx.root }), /ENOENT/);
  assert.equal(read(fx.installRoot, "current", "coderight.exe"), "dev-binary\n");
});

test("refuses a current that is a file", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  fs.mkdirSync(fx.installRoot, { recursive: true });
  fs.writeFileSync(path.join(fx.installRoot, "current"), "x");
  assert.throws(() => installPortableDevelopment(fx.descriptor, { root: fx.root }), /not a directory/);
  assert.equal(read(fx.installRoot, "current"), "x");
});

test("rejects source symlinks that escape the payload", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  const outside = path.join(fx.root, "outside.txt");
  fs.writeFileSync(outside, "outside");
  fs.symlinkSync(outside, path.join(fx.source, "escape.txt"));
  assert.throws(() => installPortableDevelopment(fx.descriptor, { root: fx.root }), /symbolic link/);
  assert.equal(fs.existsSync(fx.installRoot), false);
});

test("a failed swap leaves current absent and keeps NO old copy", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  installPortableDevelopment(fx.descriptor, { root: fx.root });
  fs.writeFileSync(path.join(fx.source, "coderight.exe"), "next-dev-binary\n");
  const staging = path.join(fx.installRoot, "current.staging");
  const current = path.join(fx.installRoot, "current");
  const originalRename = fs.renameSync;
  t.mock.method(fs, "renameSync", function (from, to) {
    if (from === staging && to === current) throw new Error("injected swap failure");
    return originalRename.call(fs, from, to);
  });
  assert.throws(() => installPortableDevelopment(fx.descriptor, { root: fx.root }), /injected swap failure/);
  assert.equal(fs.existsSync(current), false, "reinstall from an older build");
  assert.deepEqual(fs.readdirSync(fx.installRoot), []);
});

test("rejects filesystem roots and absolute payload paths", (t) => {
  const fx = fixture(); t.after(fx.cleanup);
  assert.throws(() => installPortableDevelopment({ ...fx.descriptor, installRoot: path.parse(fx.root).root }, { root: fx.root }), /non-filesystem-root/);
  assert.throws(() => installPortableDevelopment({ ...fx.descriptor, entrypoint: path.join(fx.source, "coderight.exe") }, { root: fx.root }), /contained relative path/);
});
