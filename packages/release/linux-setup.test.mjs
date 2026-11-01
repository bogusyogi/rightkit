import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { findLockroots, readPins, resolveNodeVersion, setup } from "./linux-setup.mjs";

const app = () => {
  const r = mkdtempSync(join(tmpdir(), "rk-ls-"));
  writeFileSync(join(r, "package.json"), JSON.stringify({ packageManager: "pnpm@11.25.0+sha512.abc", engines: { node: "26.8.x" } }));
  writeFileSync(join(r, "rust-toolchain.toml"), '[toolchain]\nchannel = "1.91.0"\n');
  writeFileSync(join(r, "pnpm-lock.yaml"), "");
  mkdirSync(join(r, "hub")); writeFileSync(join(r, "hub", "pnpm-lock.yaml"), "");
  mkdirSync(join(r, "node_modules", "x"), { recursive: true }); writeFileSync(join(r, "node_modules", "x", "pnpm-lock.yaml"), "");
  return r;
};

test("reads pins", () => {
  assert.deepEqual(readPins(app()), { node: "26.8.x", pnpm: "11.25.0", rust: "1.91.0" });
});
test("resolves node range to newest patch", () => {
  const idx = [{ version: "v26.8.2" }, { version: "v26.8.10" }, { version: "v26.9.0" }, { version: "v25.1.0" }];
  assert.equal(resolveNodeVersion("26.8.x", idx), "26.8.10");
  assert.equal(resolveNodeVersion("26.8.1", idx), "26.8.1");
});
test("finds lockroots outside node_modules", () => {
  assert.deepEqual(findLockroots(app()), [".", "hub"]);
});
test("setup is a no-op for installed tools and fetches each lockroot with a throwaway virtual store", () => {
  const root = app();
  const calls = [];
  const run = (cmd, args, o) => {
    calls.push({ cmd, args, cwd: o.cwd });
    if (cmd === "node") return { status: 0, stdout: "v26.8.3\n" };
    if (cmd === "pnpm" && args[0] === "--version") return { status: 0, stdout: "11.25.0\n" };
    if (cmd === "rustup" && args[0] === "--version") return { status: 0, stdout: "rustup 1.28\n" };
    return { status: 0, stdout: "" };
  };
  const r = setup({ root, home: root, run, fetchText: () => JSON.stringify([{ version: "v26.8.3" }]) });
  assert.deepEqual(r.lockroots, [".", "hub"]);
  const fetches = calls.filter((c) => c.cmd === "pnpm" && c.args[0] === "fetch");
  assert.equal(fetches.length, 2);
  for (const f of fetches) assert.ok(f.args.includes("--virtual-store-dir"));
  assert.ok(!calls.some((c) => c.cmd === "sh" || c.cmd === "corepack"));
});
test("dry run lists install steps without running", () => {
  const r = setup({ root: app(), home: "/h", dryRun: true, run: () => assert.fail("ran"), fetchText: () => assert.fail("fetched") });
  assert.ok(r.steps.some((s) => s.includes("corepack prepare pnpm@11.25.0")));
  assert.ok(r.steps.some((s) => s.includes("rustup toolchain install")));
});
