import assert from "node:assert/strict";
import test from "node:test";
import { runConfiguredChecks } from "./release-checks.mjs";

test("configured checks run by default and --skip-checks suppresses only those checks", async () => {
  const fixture = { packageManager: "pnpm", checks: ["lint", "test"], workdir: "/fixture" };
  const defaultCalls = [];
  await runConfiguredChecks({ ...fixture, run: async (...args) => defaultCalls.push(args) });
  assert.deepEqual(defaultCalls, [["pnpm", "lint", "/fixture"], ["pnpm", "test", "/fixture"]]);

  const skippedCalls = [];
  await runConfiguredChecks({ ...fixture, skipChecks: true, run: async (...args) => skippedCalls.push(args) });
  assert.deepEqual(skippedCalls, []);
});
