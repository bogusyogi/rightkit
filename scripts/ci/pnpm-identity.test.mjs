import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pnpmPackageVersion } from "./exports-import-effects.mjs";

const write = (file, text) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, text); };
const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "rk-pnpm-layout-"));

test("pnpm 11 layout: the binary sits inside the pnpm package", () => {
  const root = tmp();
  write(path.join(root, "node_modules/pnpm/package.json"), JSON.stringify({ name: "pnpm", version: "11.25.0" }));
  write(path.join(root, "node_modules/pnpm/bin/pnpm.cjs"), "");
  assert.equal(pnpmPackageVersion(path.join(root, "node_modules/pnpm/bin/pnpm.cjs")), "11.25.0");
});

test("pnpm 12 self-updated native binary: a bootstrap node_modules/pnpm beside it is not its package", () => {
  const root = tmp();
  write(path.join(root, "node_modules/pnpm/package.json"), JSON.stringify({ name: "pnpm", version: "12.16.1" }));
  write(path.join(root, "node_modules/.bin/bin/pnpm"), "");
  assert.equal(pnpmPackageVersion(path.join(root, "node_modules/.bin/bin/pnpm")), null);
});

test("a native binary inside its @pnpm/exe package binds to that package's version", () => {
  const root = tmp();
  write(path.join(root, "node_modules/@pnpm/exe.linux-x64/package.json"), JSON.stringify({ name: "@pnpm/exe.linux-x64", version: "12.16.1" }));
  write(path.join(root, "node_modules/@pnpm/exe.linux-x64/pnpm"), "");
  assert.equal(pnpmPackageVersion(path.join(root, "node_modules/@pnpm/exe.linux-x64/pnpm")), "12.16.1");
});

test("no pnpm package anywhere: the identity cannot be proven", () => {
  const root = tmp();
  write(path.join(root, "node_modules/.bin/bin/pnpm"), "");
  write(path.join(root, "package.json"), JSON.stringify({ name: "other", version: "1.0.0" }));
  assert.equal(pnpmPackageVersion(path.join(root, "node_modules/.bin/bin/pnpm")), null);
  const bare = tmp(); write(path.join(bare, "bin/pnpm"), "");
  assert.equal(pnpmPackageVersion(path.join(bare, "bin/pnpm")), null);
});

test("a non-pnpm package in node_modules/pnpm is not accepted", () => {
  const root = tmp();
  write(path.join(root, "node_modules/pnpm/package.json"), JSON.stringify({ name: "impostor", version: "12.16.1" }));
  write(path.join(root, "node_modules/.bin/bin/pnpm"), "");
  assert.equal(pnpmPackageVersion(path.join(root, "node_modules/.bin/bin/pnpm")), null);
});

test("a native PE without an .exe extension is run from a content-addressed pnpm.exe copy on Windows only", async () => {
  const { nativeLaunchPath, pnpmLaunch } = await import("./exports-import-effects.mjs");
  const root = tmp(); const tmpdir = path.join(root, "tmp"); fs.mkdirSync(tmpdir);
  const pe = path.join(root, "bin", "pnpm"); fs.mkdirSync(path.dirname(pe)); fs.writeFileSync(pe, Buffer.from([0x4d, 0x5a, 1, 2, 3]));
  const copy = nativeLaunchPath(pe, { platform: "win32", tmpdir });
  assert.match(copy, /rk-pnpm-exe-[0-9a-f]{16}[\\/]pnpm\.exe$/);
  assert.deepEqual([...fs.readFileSync(copy)], [0x4d, 0x5a, 1, 2, 3], "the copy carries the identified bytes");
  assert.equal(nativeLaunchPath(pe, { platform: "win32", tmpdir }), copy, "a second ask reuses the verified copy");
  assert.equal(nativeLaunchPath(pe, { platform: "linux", tmpdir }), pe, "no copy off Windows");
  const exe = path.join(root, "bin", "pnpm.exe"); fs.writeFileSync(exe, Buffer.from([0x4d, 0x5a]));
  assert.equal(nativeLaunchPath(exe, { platform: "win32", tmpdir }), exe, "an .exe is run as it is");
  const text = path.join(root, "bin", "sh-shim"); fs.writeFileSync(text, "#!/bin/sh\n");
  assert.equal(nativeLaunchPath(text, { platform: "win32", tmpdir }), text, "a non-PE file is returned unchanged");
  assert.deepEqual(pnpmLaunch({ path: "/x/pnpm", realPath: "/x/pnpm" }, { platform: "linux" }), { command: "/x/pnpm", args: [] });
  assert.equal(pnpmLaunch({ path: pe, realPath: pe }, { platform: "win32", tmpdir }).command, copy);
  assert.deepEqual(pnpmLaunch({ path: "/x/p", realPath: "/x/pnpm.cjs" }), { command: process.execPath, args: ["/x/pnpm.cjs"] });
});
