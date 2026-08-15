import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";

import { assertComputePolicyAllowsCargo, cargoCacheEnvironment, cargoProjectRoot, cargoSubcommand, checkHookInput, resolveRealCargo, resolveRealRustc, runCargoGuard, shellRunsCargoTest, shouldGuardCargo } from "./cargo-guard.mjs";

test("Cargo guard serializes compiling commands but bypasses inspection and formatting", () => {
  for (const args of [["build"], ["test"], ["check"], ["clippy"], ["nextest", "run"], ["clean"], ["+stable", "bench"]]) {
    assert.equal(shouldGuardCargo(args), true, args.join(" "));
  }
  for (const args of [["fmt", "--check"], ["metadata"], ["tree"], ["fetch"], ["--version"], []]) {
    assert.equal(shouldGuardCargo(args), false, args.join(" "));
  }
  assert.equal(cargoSubcommand(["+stable", "test"]), "test");
  assert.equal(shouldGuardCargo(["--color", "always", "fmt"]), false);
  assert.equal(shouldGuardCargo(["--config", "net.git-fetch-with-cli=true", "check"]), true);
  assert.equal(shouldGuardCargo(["-Z", "unstable-options", "fmt"]), false);
});

test("global policy denies Cargo test before Cargo resolution", async () => {
  const policy = { exists: () => true, read: () => '{"schemaVersion":1,"cargoTest":"deny"}', env: {} };
  assert.throws(() => assertComputePolicyAllowsCargo(["test", "-p", "app"], policy), /denied by global/);
  assert.doesNotThrow(() => assertComputePolicyAllowsCargo(["check"], policy));
  let resolved = false;
  await assert.rejects(runCargoGuard(["test"], {
    env: { RIGHTSUITE_COMPUTE_POLICY: "/policy.json" },
    policyOptions: { exists: () => true, read: () => '{"schemaVersion":1,"cargoTest":"deny"}' },
    resolveCargo: () => { resolved = true; return "/real/cargo"; },
  }), /denied by global/);
  assert.equal(resolved, false);
});

test("Bash policy detects direct, absolute, rustup and nested Cargo tests only", () => {
  for (const command of ["cargo test -p app", "cd app && /toolchain/bin/cargo +stable test", "rustup run stable cargo test", "bash -c 'cargo test'"]) {
    assert.equal(shellRunsCargoTest(command), true, command);
  }
  for (const command of ["cargo check", "rg 'cargo test' docs", "echo cargo test", "cargo fmt --check"]) {
    assert.equal(shellRunsCargoTest(command), false, command);
  }
  assert.throws(() => checkHookInput(JSON.stringify({ tool_input: { command: "cargo test" } }), {
    exists: () => true, read: () => '{"schemaVersion":1,"cargoTest":"deny"}', env: {},
  }), /denied by global/);
});

test("Cargo guard resolves real Cargo through rustup or explicit override", () => {
  assert.equal(resolveRealCargo({ env: { RIGHTSUITE_REAL_CARGO: "/opt/toolchain/cargo" } }), path.resolve("/opt/toolchain/cargo"));
  const calls = [];
  const cargo = resolveRealCargo({ env: {}, run: (command, args) => { calls.push([command, args]); return { status: 0, stdout: "/real/cargo\n" }; } });
  assert.equal(cargo, path.resolve("/real/cargo"));
  assert.deepEqual(calls[0][1], ["which", "cargo"]);
  assert.equal(resolveRealRustc({ env: { RIGHTSUITE_REAL_RUSTC: "/opt/toolchain/rustc" } }), path.resolve("/opt/toolchain/rustc"));
  assert.equal(resolveRealRustc({ env: {}, run: () => ({ status: 0, stdout: "/real/rustc\n" }) }), path.resolve("/real/rustc"));
});

function cacheEnv(overrides = {}) {
  return { ...(process.platform === "win32" ? { LOCALAPPDATA: "C:\\Users\\test\\AppData\\Local" } : { RIGHT_RELEASE_CACHE_ROOT: "/tmp/rightkit-release-cache" }), ...overrides };
}

test("Cargo guard injects project-keyed target and sccache directories", async () => {
  let guarded;
  await runCargoGuard(["build"], {
    env: cacheEnv(), resolveCargo: () => "/real/cargo", resolveRustc: () => "/real/rustc",
    runHeavy: async (_args, config) => { guarded = config.env; return 0; },
  });
  assert.match(guarded.CARGO_TARGET_DIR, /dev-targets[/\\][^/\\]+-[a-f0-9]{12}$/);
  assert.match(guarded.SCCACHE_DIR, /[/\\]sccache$/);
  assert.equal(guarded.RUSTC_WRAPPER, "sccache");
});

test("Cargo guard rejects cache escapes and alternate compiler wrappers", async () => {
  const run = (args, overrides) => runCargoGuard(args, {
    env: cacheEnv(overrides), resolveCargo: () => "/real/cargo", resolveRustc: () => "/real/rustc",
    runHeavy: async () => 0,
  });
  await assert.rejects(run(["build"], { CARGO_TARGET_DIR: path.resolve("outside-target") }), /CARGO_TARGET_DIR/);
  await assert.rejects(run(["build"], { SCCACHE_DIR: path.resolve("outside-sccache") }), /SCCACHE_DIR/);
  await assert.rejects(run(["build", `--target-dir=${path.resolve("outside-flag")}`]), /--target-dir/);
  await assert.rejects(run(["build"], { RUSTC_WRAPPER: "rustc-wrapper" }), /RUSTC_WRAPPER/);
});

test("Cargo guard routes heavy and light commands without recursion", async () => {
  const calls = [];
  const options = {
    env: { TEST: "1", ...cacheEnv() },
    policyOptions: { exists: () => false },
    resolveCargo: () => "/real/cargo",
    resolveRustc: () => "/real/rustc",
    runHeavy: async (args, config) => { calls.push(["heavy", args, config.env]); return 0; },
    runLight: async (command, args, env) => { calls.push(["light", command, args, env]); return 0; },
  };
  assert.equal(await runCargoGuard(["test"], options), 0);
  assert.equal(await runCargoGuard(["fmt", "--check"], options), 0);
  assert.deepEqual(calls[0].slice(0, 2), ["heavy", ["--", "/real/cargo", "test"]]);
  const cacheRoot = cacheEnv().RIGHT_RELEASE_CACHE_ROOT ?? path.join(cacheEnv().LOCALAPPDATA, "RightSuite", "Cache", "release");
  assert.equal(calls[0][2].CARGO_TARGET_DIR.startsWith(path.join(path.resolve(cacheRoot), "dev-targets") + path.sep), true);
  assert.equal(calls[0][2].RUSTC_WRAPPER, "sccache");
  assert.deepEqual(calls[1], ["light", "/real/cargo", ["fmt", "--check"], options.env]);
});

test("Cargo guard forces target and compiler caches for each project", () => {
  const existing = new Set(["/repo/Cargo.toml", "/repo/crate/Cargo.toml"]);
  assert.equal(cargoProjectRoot(["test"], { cwd: "/repo/crate", platform: "mac", exists: (file) => existing.has(file) }), "/repo");
  const env = cargoCacheEnvironment(["test"], {
    cwd: "/repo/crate", platform: "mac", exists: (file) => existing.has(file),
    env: { RIGHT_RELEASE_CACHE_ROOT: "/cache" },
  });
  assert.match(env.CARGO_TARGET_DIR, /^\/cache\/dev-targets\/repo-[a-f0-9]{12}$/);
  assert.equal(env.RUSTC_WRAPPER, "sccache");
  assert.equal(env.SCCACHE_DIR, "/cache/sccache");
  assert.equal(env.RIGHTSUITE_CARGO_CACHE_GUARD, "1");
});

test("Cargo guard rejects cache bypasses and permits RightKit-owned cache paths", () => {
  const base = { cwd: "/repo", platform: "mac", exists: () => false };
  assert.throws(() => cargoCacheEnvironment(["test"], { ...base, env: { RIGHT_RELEASE_CACHE_ROOT: "/cache", CARGO_TARGET_DIR: "/tmp/target" } }), /CARGO_TARGET_DIR must stay inside/);
  assert.throws(() => cargoCacheEnvironment(["test", "--target-dir", "/tmp/target"], { ...base, env: { RIGHT_RELEASE_CACHE_ROOT: "/cache" } }), /--target-dir must stay inside/);
  assert.throws(() => cargoCacheEnvironment(["test"], { ...base, env: { RIGHT_RELEASE_CACHE_ROOT: "/cache", RUSTC_WRAPPER: "rustc-wrapper" } }), /must be sccache/);
  const env = cargoCacheEnvironment(["test", "--target-dir=/cache/test-targets/app"], {
    ...base,
    env: { RIGHT_RELEASE_CACHE_ROOT: "/cache", CARGO_TARGET_DIR: "/cache/test-targets/app", SCCACHE_DIR: "/cache/sccache", RUSTC_WRAPPER: "/usr/bin/sccache" },
  });
  assert.equal(env.CARGO_TARGET_DIR, "/cache/test-targets/app");
});

test("Cargo guard derives native Windows cache paths", () => {
  const env = cargoCacheEnvironment(["build"], {
    cwd: "D:\\Claude\\citadel", platform: "win", exists: (file) => file === "D:\\Claude\\citadel\\Cargo.toml",
    env: { LOCALAPPDATA: "C:\\Users\\test\\AppData\\Local" },
  });
  assert.match(env.CARGO_TARGET_DIR, /^C:\\Users\\test\\AppData\\Local\\RightSuite\\Cache\\release\\dev-targets\\citadel-[a-f0-9]{12}$/);
  assert.equal(env.SCCACHE_DIR, "C:\\Users\\test\\AppData\\Local\\RightSuite\\Cache\\release\\sccache");
});
