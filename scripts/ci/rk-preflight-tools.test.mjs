import test from "node:test";
import assert from "node:assert/strict";
import { checkTools } from "./rk-preflight.mjs";

const spawnFn = (cmd) => ({ status: 0, stdout: cmd === "actionlint" ? "1.7.12\n" : "ok\n" });

test("checkTools: an actionlint pin in the object form (gate 0.6.89) compares by its version", () => {
  const { problems } = checkTools({ pins: { actionlint: { version: "1.7.12", linux: {} } }, spawnFn, exists: () => true, platform: "darwin" });
  assert.ok(!problems.some((p) => p.startsWith("actionlint")), problems.join("; "));
});

test("checkTools: the string form still works and a different version is refused", () => {
  assert.ok(!checkTools({ pins: { actionlint: "1.7.12" }, spawnFn, exists: () => true, platform: "darwin" }).problems.some((p) => p.startsWith("actionlint")));
  const { problems } = checkTools({ pins: { actionlint: { version: "1.7.13" } }, spawnFn, exists: () => true, platform: "darwin" });
  assert.ok(problems.includes("actionlint 1.7.12 is not the pinned 1.7.13"), problems.join("; "));
});
