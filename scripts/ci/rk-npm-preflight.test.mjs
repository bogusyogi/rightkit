import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { adapterExpectation, ADAPTER, packageRows, parseArgs, readPackages } from "./rk-npm-preflight.mjs";

test("arguments: npm registry and one @rightkit package only", () => {
  const opts = parseArgs(["--registry", "npm", "--unit", "@rightkit/legal"]);
  assert.equal(opts.unit, "@rightkit/legal");
  assert.throws(() => parseArgs(["--registry", "crates", "--unit", "rightkit-fs"]), /must be npm/);
  assert.throws(() => parseArgs(["--registry", "npm", "--unit", "left-pad"]), /@rightkit/);
  assert.throws(() => parseArgs(["--registry", "npm", "--unit", "@rightkit/legal", "--skip-tests"]), /unknown argument/);
});

test("rows mirror the hosted npm verify: frozen install pinned to the store, the build closure, then the package's test script", () => {
  const pkg = { name: "@rightkit/legal", dir: "packages/legal", version: "0.6.2", scripts: { test: "vitest run" } };
  const rows = packageRows("@rightkit/legal", pkg, { storeDir: "/Volumes/D/.pnpm-store/v11" });
  assert.deepEqual(rows.map((r) => r.id), ["install", "build-closure", "test"]);
  assert.deepEqual(rows[0].args, ["install", "--frozen-lockfile", "--store-dir", "/Volumes/D/.pnpm-store/v11"]);
  assert.ok(rows[1].args.includes("--filter") && rows[1].args.includes("@rightkit/legal...") && rows[1].args.slice(-2).join(" ") === "run build");
  assert.deepEqual(rows[2].args, ["--dir", "packages/legal", "run", "test"]);
  assert.throws(() => packageRows("@rightkit/legal", pkg, {}), /store pin/);
  assert.throws(() => packageRows("@rightkit/x", { ...pkg, scripts: {} }, { storeDir: "/s" }), /no test script/);
});

test("the attestation expectation binds the adapter id, this host's node and the pinned pnpm script", () => {
  const expect = adapterExpectation({ nodeSha256: "aa", pnpmScriptSha256: "bb", pnpmScriptPath: "/p/pnpm.mjs" });
  assert.deepEqual(expect, { kind: "adapter", adapter: ADAPTER, executable: { sha256: "aa" }, script: { path: "/p/pnpm.mjs", sha256: "bb" } });
});

test("workspace packages are read from packages/*/package.json with their directory", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-npm-"));
  fs.mkdirSync(path.join(root, "packages", "legal"), { recursive: true });
  fs.mkdirSync(path.join(root, "packages", "no-manifest"), { recursive: true });
  fs.writeFileSync(path.join(root, "packages", "legal", "package.json"), JSON.stringify({ name: "@rightkit/legal", version: "0.6.2", scripts: { test: "x" } }));
  const packages = readPackages(root);
  assert.deepEqual([...packages.keys()], ["@rightkit/legal"]);
  assert.equal(packages.get("@rightkit/legal").dir, "packages/legal");
  assert.equal(packages.get("@rightkit/legal").version, "0.6.2");
});
