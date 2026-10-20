import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { productionBuildEnv, productionModeProblems } from "./production-mode.mjs";

const app = () => mkdtempSync(join(tmpdir(), "rk-prod-mode-"));

test("release build children always get NODE_ENV=production", () => {
  assert.equal(productionBuildEnv({ NODE_ENV: "development", A: "1" }).NODE_ENV, "production");
  assert.equal(productionBuildEnv({}).NODE_ENV, "production");
  assert.equal(productionBuildEnv({ A: "1" }).A, "1");
});

test("custom modes and .env NODE_ENV are not refused: the pinned NODE_ENV decides production resolution", () => {
  const root = app();
  writeFileSync(join(root, ".env"), "NODE_ENV=development\n");
  writeFileSync(join(root, ".env.staging"), "VITE_X=1\n");
  writeFileSync(join(root, "vite.config.ts"), "export default { mode: 'staging', resolve: { conditions: ['browser'] } }");
  assert.deepEqual(productionModeProblems({ appRoot: root }), []);
});

test("an explicit development resolve condition is refused, including a nested frontend", () => {
  const root = app(); mkdirSync(join(root, "ui"));
  writeFileSync(join(root, "ui", "vite.config.mts"), 'export default { resolve: { conditions: ["browser", "development"] }, worker: {} }');
  const problems = productionModeProblems({ appRoot: root });
  assert.equal(problems.length, 1); assert.match(problems[0], /development" resolve condition/);
});

test("node_modules and build output are not scanned", () => {
  const root = app(); mkdirSync(join(root, "node_modules", "x"), { recursive: true });
  writeFileSync(join(root, "node_modules", "x", "vite.config.js"), "export default { resolve: { conditions: ['development'] } }");
  assert.deepEqual(productionModeProblems({ appRoot: root }), []);
});
