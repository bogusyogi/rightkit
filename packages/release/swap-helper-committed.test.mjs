import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { committedSwapHelperDigest, prepareSwapHelper } from "./swap-helper-bundle.mjs";

const pkg = (t, bytes, digestOf = bytes) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-swap-committed-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  fs.mkdirSync(path.join(dir, "bin", "darwin"), { recursive: true });
  fs.writeFileSync(path.join(dir, "bin", "darwin", "rightkit-swap"), bytes);
  const hex = crypto.createHash("sha256").update(digestOf).digest("hex");
  fs.writeFileSync(path.join(dir, "bin", "darwin", "rightkit-swap.sha256"), `${hex}  rightkit-swap\n`);
  return dir;
};

test("the committed helper's digest is read from bin/darwin/rightkit-swap.sha256", (t) => {
  const dir = pkg(t, "helper");
  assert.equal(committedSwapHelperDigest(dir), crypto.createHash("sha256").update("helper").digest("hex"));
  assert.equal(committedSwapHelperDigest(path.join(dir, "absent")), null);
});

test("on the Mac a committed helper that matches its digest is used as it is: nothing is built or signed", (t) => {
  const dir = pkg(t, "helper");
  const calls = [];
  const prepare = () => prepareSwapHelper({ workspaceRoot: dir, packageDir: dir, platform: "darwin", run: (...a) => { calls.push(a); return { status: 1 }; } });
  // The signature check runs codesign: where it cannot run (off a Mac, or an unsigned test file) the committed path refuses as unsigned, never builds.
  assert.throws(prepare, (e) => e.code === "SWAP_HELPER_UNSIGNED");
  assert.equal(calls.length, 0, "no Cargo build");
});

test("on the Mac a committed helper whose bytes differ from its digest is rebuilt, never used", (t) => {
  const dir = pkg(t, "edited", "original");
  const calls = [];
  assert.throws(() => prepareSwapHelper({ workspaceRoot: dir, packageDir: dir, platform: "darwin", run: (...a) => { calls.push(a); return { status: 1, stdout: "", stderr: "no cargo here" }; } }), (e) => e.code === "SWAP_HELPER_BUILD_FAILED");
  assert.equal(calls[0][0], "cargo");
});

test("off the Mac the committed helper is taken when its bytes match the committed digest, and refused when they do not", (t) => {
  assert.equal(prepareSwapHelper({ workspaceRoot: ".", packageDir: pkg(t, "helper"), platform: "win32", run: () => assert.fail("no build off the Mac") }).built, false);
  assert.throws(() => prepareSwapHelper({ workspaceRoot: ".", packageDir: pkg(t, "edited", "original"), platform: "linux", run: () => assert.fail("no build") }), (e) => e.code === "SWAP_HELPER_MISSING");
});

test("a helper that arrives without its executable bit runs as a private executable copy; an executable one runs as it is", async (t) => {
  const { runnableHelper } = await import("./install-app.mjs");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-swap-runnable-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const helper = path.join(dir, "rightkit-swap");
  fs.writeFileSync(helper, "signed bytes", { mode: 0o644 });
  if (process.platform === "win32") { assert.equal(runnableHelper(helper, { tmp: dir }), helper); return; }
  fs.chmodSync(helper, 0o644);
  const copy = runnableHelper(helper, { tmp: dir });
  assert.notEqual(copy, helper);
  assert.equal(fs.readFileSync(copy, "utf8"), "signed bytes");
  assert.ok((fs.statSync(copy).mode & 0o111) !== 0);
  fs.chmodSync(helper, 0o755);
  assert.equal(runnableHelper(helper, { tmp: dir }), helper);
});
