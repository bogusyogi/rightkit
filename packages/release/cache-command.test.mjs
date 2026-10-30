import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { runCacheCommand } from "./cache-command.mjs";

async function run(args, cacheRoot, env = {}, isBrokerHost = () => false) {
  let stdout = "";
  let stderr = "";
  try {
    await runCacheCommand(args, {
      env: { ...process.env, RIGHT_RELEASE_CACHE_ROOT: cacheRoot, ...env }, isBrokerHost,
      stdout: (value) => { stdout += `${value}\n`; }, stderr: (value) => { stderr += `${value}\n`; },
    });
    return { status: 0, stdout, stderr };
  } catch (error) {
    return { status: 2, stdout, stderr: `${stderr}${error.message}\n` };
  }
}

test("cache status has human and schema-stable JSON output", async () => {
  const cacheRoot = mkdtempSync(path.join(os.tmpdir(), "rightkit-cache-command-"));
  const human = await run(["status"], cacheRoot);
  const json = await run(["status", "--json"], cacheRoot);
  assert.equal(human.status, 0, human.stderr);
  assert.match(human.stdout, /RightKit cache root:/);
  assert.match(human.stdout, /Backups: 0 bytes/);
  assert.equal(json.status, 0, json.stderr);
  assert.equal(JSON.parse(json.stdout).schema, 1);
});

test("cache prune defaults to dry-run and only apply can mutate an admitted temporary entry", async () => {
  const cacheRoot = mkdtempSync(path.join(os.tmpdir(), "rightkit-cache-command-"));
  const target = path.join(cacheRoot, "targets", "mac", "fixture", "1111111111111111");
  mkdirSync(target, { recursive: true });
  writeFileSync(path.join(target, "payload"), "payload");
  writeFileSync(path.join(target, ".rightkit-cache-entry.json"), JSON.stringify({ schema: 1, id: "target:mac:fixture:1111111111111111", kind: "release", platform: "mac", architecture: "aarch64", app: "fixture", fingerprint: "1111111111111111", createdAt: "2020-01-01T00:00:00.000Z", lastUsedAt: "2020-01-01T00:00:00.000Z", lastSuccessfulBuildAt: "2020-01-01T00:00:00.000Z", toolchain: {} }));
  const dry = await run(["prune", "--dry-run", "--json"], cacheRoot);
  assert.equal(dry.status, 0, dry.stderr);
  assert.equal(JSON.parse(dry.stdout).dryRun, true);
  assert.equal((await run(["prune", "--apply", "--json"], cacheRoot)).status, 0);
});

test("cache rejects unknown commands and migration requires a config", async () => {
  const cacheRoot = mkdtempSync(path.join(os.tmpdir(), "rightkit-cache-command-"));
  assert.notEqual((await run(["unknown"], cacheRoot)).status, 0);
  assert.notEqual((await run(["migrate"], cacheRoot)).status, 0);
});

test("status and prune dry-run do not create a cache root or lock file", async () => {
  const parent = mkdtempSync(path.join(os.tmpdir(), "rightkit-cache-command-"));
  const cacheRoot = path.join(parent, "never-created");
  assert.equal((await run(["status", "--json"], cacheRoot)).status, 0);
  assert.equal((await run(["prune", "--dry-run", "--json"], cacheRoot)).status, 0);
  assert.equal(existsSync(cacheRoot), false);
});

test("cache migrate uses the repository vault Cargo home, not an app-local lookalike", async () => {
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
  const result = await run(["migrate", "--config", config, "--apply", "--json"], cacheRoot, { RIGHT_RELEASE_PLATFORM: "mac", RIGHT_RELEASE_RUSTC_VERBOSE: "rustc 1.91.0\nhost: aarch64-apple-darwin", RIGHT_RELEASE_CACHE_KEY: fingerprint });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).cargoHomeMoved, true);
  assert.equal(readFileSync(path.join(cacheRoot, "cargo-home", "registry"), "utf8"), "vault");
  assert.equal(readFileSync(path.join(appCargoHome, "registry"), "utf8"), "app-local");
});

test("status, prune, and migrate short-circuit cleanly on a broker-managed host without proposing a repair", async () => {
  const cacheRoot = mkdtempSync(path.join(os.tmpdir(), "rightkit-cache-command-"));
  const fixtureBrokerHost = () => true;
  const status = await run(["status", "--json"], cacheRoot, {}, fixtureBrokerHost);
  assert.equal(status.status, 0, status.stderr);
  const statusBody = JSON.parse(status.stdout);
  assert.equal(statusBody.brokerManaged, true);
  assert.match(statusBody.message, /broker-managed host/);
  assert.doesNotMatch(status.stdout + status.stderr, /warn|repair|corrupt/i);

  const prune = await run(["prune", "--dry-run", "--json"], cacheRoot, {}, fixtureBrokerHost);
  assert.equal(prune.status, 0, prune.stderr);
  assert.equal(JSON.parse(prune.stdout).brokerManaged, true);

  // migrate short-circuits before even requiring --config, since a broker host has
  // nothing under Cache V2's target layout for it to migrate into.
  const migrate = await run(["migrate", "--json"], cacheRoot, {}, fixtureBrokerHost);
  assert.equal(migrate.status, 0, migrate.stderr);
  assert.equal(JSON.parse(migrate.stdout).brokerManaged, true);

  // Non-broker host: unchanged behaviour.
  const unmanaged = await run(["status", "--json"], cacheRoot);
  assert.equal(unmanaged.status, 0, unmanaged.stderr);
  assert.equal(JSON.parse(unmanaged.stdout).brokerManaged, undefined);
});

test("[GB-65] a broker Mac with RIGHT_RELEASE_CACHE_ROOT unset skips status, prune and migrate with a null root; argv stays validated; a direct host still needs a valid root; nothing is deleted", async () => {
  const brokerMac = { RIGHT_RELEASE_PLATFORM: "mac" };
  const call = async (args, env, isBrokerHost) => {
    let out = ""; let error = null; let result = null;
    try { result = await runCacheCommand(args, { env, isBrokerHost, stdout: (v) => { out += `${v}\n`; }, stderr: () => {} }); } catch (e) { error = e; }
    return { out, error, result };
  };
  for (const args of [["status", "--json"], ["prune", "--apply", "--json"], ["prune", "--dry-run"], ["migrate", "--json"], ["migrate", "--config", "x/right-release.config.json", "--apply"]]) {
    const r = await call(args, brokerMac, () => true);
    assert.equal(r.error, null, `${args.join(" ")}: ${r.error?.message}`);
    assert.equal(r.result.brokerManaged, true); assert.equal(r.result.skipped, true); assert.equal(r.result.cacheRoot, null);
    assert.equal(r.result.removedIds, undefined, "no prune ran");
  }
  assert.match((await call(["status", "--bogus"], brokerMac, () => true)).error?.message, /unknown cache argument: --bogus/, "argv validation is kept on the broker route");
  assert.match((await call(["prune", "--apply", "--dry-run"], brokerMac, () => true)).error?.message, /either --dry-run or --apply/);
  assert.match((await call(["migrate", "--nope"], brokerMac, () => true)).error?.message, /unknown cache argument: --nope/);
  assert.match((await call(["status"], brokerMac, () => false)).error?.message, /RIGHT_RELEASE_CACHE_ROOT is required on macOS/, "a direct Mac host still requires a valid cache policy");
  assert.match((await call(["frobnicate"], brokerMac, () => true)).error?.message, /expected status, prune, or migrate|RIGHT_RELEASE_CACHE_ROOT/);
});
