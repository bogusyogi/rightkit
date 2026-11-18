import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { rememberedVersion, resolveOnPath, versionKey } from "./tool-version-memo.mjs";

test("a tool's version is asked once while its program file is unchanged; a shim, a changed file or a failed answer is asked again (NN01, NN25)", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-ver-"));
  const exe = path.join(dir, process.platform === "win32" ? "actionlint.exe" : "actionlint"); fs.writeFileSync(exe, "binary v1");
  const shim = path.join(dir, "pnpm.cmd"); fs.writeFileSync(shim, "@echo off");
  const env = { PATH: dir, PATHEXT: ".EXE;.CMD" };
  const store = path.join(dir, "store", "tool-versions.json");
  let asked = 0; const run = () => { asked += 1; return "1.7.12"; };
  assert.equal(resolveOnPath("actionlint", { env }), exe);
  assert.equal(rememberedVersion("actionlint", ["-version"], { run, env, storeFile: store }), "1.7.12");
  assert.equal(rememberedVersion("actionlint", ["-version"], { run, env, storeFile: store }), "1.7.12");
  assert.equal(asked, 1, "the second run starts no process");
  // Another argument is another question.
  rememberedVersion("actionlint", ["--version"], { run, env, storeFile: store }); assert.equal(asked, 2);
  // The program replaced (size changes): asked again.
  fs.writeFileSync(exe, "binary v2 is longer"); rememberedVersion("actionlint", ["-version"], { run, env, storeFile: store }); assert.equal(asked, 3);
  // A .cmd shim is never remembered (Windows only: elsewhere a .cmd is not resolved by PATHEXT).
  if (process.platform === "win32") { assert.equal(versionKey("pnpm", ["--version"], resolveOnPath("pnpm", { env })), null); rememberedVersion("pnpm", ["--version"], { run, env, storeFile: store }); rememberedVersion("pnpm", ["--version"], { run, env, storeFile: store }); assert.equal(asked, 5); }
  // A failed answer is not stored.
  let n = 0; const fail = () => { n += 1; return null; };
  rememberedVersion("actionlint", ["-x"], { run: fail, env, storeFile: store }); rememberedVersion("actionlint", ["-x"], { run: fail, env, storeFile: store }); assert.equal(n, 2);
  // Not on PATH, or no store: asked every time.
  assert.equal(resolveOnPath("nope", { env }), null);
  let m = 0; rememberedVersion("actionlint", ["-version"], { run: () => { m += 1; return "x"; }, env, storeFile: null }); rememberedVersion("actionlint", ["-version"], { run: () => { m += 1; return "x"; }, env, storeFile: null }); assert.equal(m, 2);
});
