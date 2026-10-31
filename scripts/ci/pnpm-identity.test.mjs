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

test("pnpm 12 layout: the native binary copied to node_modules/.bin/bin/pnpm binds to the installed pnpm package", () => {
  const root = tmp();
  write(path.join(root, "node_modules/pnpm/package.json"), JSON.stringify({ name: "pnpm", version: "12.16.1" }));
  write(path.join(root, "node_modules/.bin/bin/pnpm"), "");
  assert.equal(pnpmPackageVersion(path.join(root, "node_modules/.bin/bin/pnpm")), "12.16.1");
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
