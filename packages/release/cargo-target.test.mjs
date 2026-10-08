import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { resolveTargetRoot } from "./cargo-target.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const cli = path.join(here, "cargo-target.mjs");

// CLI tests use a Node-only managed runtime fixture, never a live Cargo toolchain.
function managedCliEnvironment(t) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-target-client-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const client = path.join(root, "runtime-current", "build-control-clients", "src", "rightkit.mjs");
  mkdirSync(path.dirname(client), { recursive: true });
  writeFileSync(client, `import path from 'node:path';
const manifest = process.argv.at(-1);
if (manifest.includes('fail-manifest')) { process.stderr.write('fixture metadata failure'); process.exitCode = 1; }
else process.stdout.write(JSON.stringify({target_directory:path.join(path.dirname(manifest),'target')}));`);
  const socket = path.join(root, "broker.sock"); writeFileSync(socket, "fixture");
  return { ...process.env, RIGHTKIT_CONTROL_ROOT: root, RIGHTKIT_BUILD_BROKER_SOCKET: socket };
}

function fakeCargo() {
  const bin = mkdtempSync(path.join(os.tmpdir(), "rightkit-fake-cargo-"));
  return {
    bin,
    execFileSync(_command, args) {
      const manifest = args.at(-1);
      if (manifest.includes("fail-manifest")) throw new Error("boom");
      if (manifest.includes("bad-target-manifest")) return '{"target_directory":"relative/target"}';
      return JSON.stringify({ target_directory: path.join(bin, "target") });
    },
  };
}

test("resolveTargetRoot returns the absolute target_directory Cargo metadata reports", () => {
  const { bin, execFileSync } = fakeCargo();
  const manifest = path.join(mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-target-fixture-")), "Cargo.toml");
  const result = resolveTargetRoot(manifest, { execFileSync });
  assert.equal(result, path.resolve(path.join(bin, "target")));
});

test("resolveTargetRoot fails closed, naming the manifest, when target_directory is not absolute", () => {
  const { execFileSync } = fakeCargo();
  const manifest = path.join(mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-target-fixture-")), "bad-target-manifest-Cargo.toml");
  assert.throws(
    () => resolveTargetRoot(manifest, { execFileSync }),
    new RegExp(`metadata for .*bad-target-manifest-Cargo\\.toml did not report an absolute target_directory`),
  );
});

test("resolveTargetRoot fails closed, naming the manifest, when the metadata command exits non-zero", () => {
  const { execFileSync } = fakeCargo();
  const manifest = path.join(mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-target-fixture-")), "fail-manifest-Cargo.toml");
  assert.throws(
    () => resolveTargetRoot(manifest, { execFileSync }),
    new RegExp(`metadata failed for .*fail-manifest-Cargo\\.toml`),
  );
});

test("resolveTargetRoot throws when no manifest path is given", () => {
  assert.throws(() => resolveTargetRoot(), /resolveTargetRoot requires a Cargo.toml manifest path/);
  assert.throws(() => resolveTargetRoot(""), /resolveTargetRoot requires a Cargo.toml manifest path/);
});

test("CLI mode writes only the resolved path to stdout on success", (t) => {
  const manifestDir = mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-target-fixture-"));
  const manifest = path.join(manifestDir, "Cargo.toml");
  mkdirSync(path.join(manifestDir, "src"));
  writeFileSync(path.join(manifestDir, "src", "lib.rs"), "");
  writeFileSync(manifest, "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n");
  const result = spawnSync(process.execPath, [cli, manifest], { encoding: "utf8", env: managedCliEnvironment(t) });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(path.isAbsolute(result.stdout.trim()), true);
});

test("CLI mode writes a message to stderr and exits 1 on failure", (t) => {
  const manifestDir = mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-target-fixture-"));
  const manifest = path.join(manifestDir, "fail-manifest-Cargo.toml");
  writeFileSync(manifest, "not valid toml");
  const result = spawnSync(process.execPath, [cli, manifest], { encoding: "utf8", env: managedCliEnvironment(t) });
  assert.equal(result.status, 1);
  assert.equal(result.stdout, "");
  assert.match(result.stderr, /metadata failed for .*fail-manifest-Cargo\.toml/);
});

test("CLI mode requires a manifest argument", () => {
  const result = spawnSync(process.execPath, [cli], { encoding: "utf8" });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /usage: cargo-target\.mjs/);
});
