import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.dirname(fileURLToPath(import.meta.url));
const cli = path.join(root, "cli", "right-release.mjs");

function run(args, cacheRoot, env = {}) {
  return spawnSync(process.execPath, [cli, "cache", ...args], { encoding: "utf8", env: { ...process.env, RIGHT_RELEASE_CACHE_ROOT: cacheRoot, ...env } });
}

test("cache status has human and schema-stable JSON output", () => {
  const cacheRoot = mkdtempSync(path.join(os.tmpdir(), "rightkit-cache-command-"));
  const human = run(["status"], cacheRoot);
  const json = run(["status", "--json"], cacheRoot);
  assert.equal(human.status, 0, human.stderr);
  assert.match(human.stdout, /RightKit cache root:/);
  assert.match(human.stdout, /Backups: 0 bytes/);
  assert.equal(json.status, 0, json.stderr);
  assert.equal(JSON.parse(json.stdout).schema, 1);
});

test("cache prune defaults to dry-run and only apply can mutate an admitted temporary entry", () => {
  const cacheRoot = mkdtempSync(path.join(os.tmpdir(), "rightkit-cache-command-"));
  const target = path.join(cacheRoot, "targets", "mac", "fixture", "1111111111111111");
  mkdirSync(target, { recursive: true });
  writeFileSync(path.join(target, "payload"), "payload");
  writeFileSync(path.join(target, ".rightkit-cache-entry.json"), JSON.stringify({ schema: 1, id: "target:mac:fixture:1111111111111111", kind: "release", platform: "mac", architecture: "aarch64", app: "fixture", fingerprint: "1111111111111111", createdAt: "2020-01-01T00:00:00.000Z", lastUsedAt: "2020-01-01T00:00:00.000Z", lastSuccessfulBuildAt: "2020-01-01T00:00:00.000Z", toolchain: {} }));
  const dry = run(["prune", "--dry-run", "--json"], cacheRoot);
  assert.equal(dry.status, 0, dry.stderr);
  assert.equal(JSON.parse(dry.stdout).dryRun, true);
  assert.equal(run(["prune", "--apply", "--json"], cacheRoot).status, 0);
});

test("cache rejects unknown commands and migration requires a config", () => {
  const cacheRoot = mkdtempSync(path.join(os.tmpdir(), "rightkit-cache-command-"));
  assert.notEqual(run(["unknown"], cacheRoot).status, 0);
  assert.notEqual(run(["migrate"], cacheRoot).status, 0);
});

test("status and prune dry-run do not create a cache root or lock file", () => {
  const parent = mkdtempSync(path.join(os.tmpdir(), "rightkit-cache-command-"));
  const cacheRoot = path.join(parent, "never-created");
  assert.equal(run(["status", "--json"], cacheRoot).status, 0);
  assert.equal(run(["prune", "--dry-run", "--json"], cacheRoot).status, 0);
  assert.equal(existsSync(cacheRoot), false);
});

test("cache migrate uses the repository vault Cargo home, not an app-local lookalike", () => {
  const sandbox = mkdtempSync(path.join(os.tmpdir(), "rightkit-cache-command-"));
  const repo = path.join(sandbox, "repo"); const app = path.join(repo, "app"); const cacheRoot = path.join(sandbox, "shared-cache");
  const config = path.join(app, "right-release.config.mjs"); const fingerprint = "abcdef0123456789";
  const target = path.join(repo, ".right-release", "cache", "cargo-target", "mac", fingerprint);
  const vaultCargoHome = path.join(repo, ".right-release", "cache", "cargo-home");
  const appCargoHome = path.join(app, ".right-release", "cache", "cargo-home");
  mkdirSync(path.join(repo, ".git"), { recursive: true });
  mkdirSync(path.dirname(config), { recursive: true });
  writeFileSync(config, "export default { app: 'fixture', targets: { mac: { targetTriple: 'aarch64-apple-darwin' } } };\n");
  mkdirSync(path.join(app, "src-tauri"), { recursive: true });
  writeFileSync(path.join(app, "src-tauri", "Cargo.lock"), "fixture-lock\n");
  writeFileSync(path.join(app, "src-tauri", "Cargo.toml"), "[package]\nname = 'fixture'\n");
  mkdirSync(target, { recursive: true }); writeFileSync(path.join(target, "artifact"), "target");
  symlinkSync(target, path.join(app, "src-tauri", "target"));
  mkdirSync(vaultCargoHome, { recursive: true }); writeFileSync(path.join(vaultCargoHome, "registry"), "vault");
  mkdirSync(appCargoHome, { recursive: true }); writeFileSync(path.join(appCargoHome, "registry"), "app-local");
  const result = run(["migrate", "--config", config, "--apply", "--json"], cacheRoot, { RIGHT_RELEASE_PLATFORM: "mac", RIGHT_RELEASE_RUSTC_VERBOSE: "rustc 1.91.0\nhost: aarch64-apple-darwin", RIGHT_RELEASE_CACHE_KEY: fingerprint });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).cargoHomeMoved, true);
  assert.equal(readFileSync(path.join(cacheRoot, "cargo-home", "registry"), "utf8"), "vault");
  assert.equal(readFileSync(path.join(appCargoHome, "registry"), "utf8"), "app-local");
});
