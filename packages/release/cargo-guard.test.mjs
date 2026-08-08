import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";

import { cargoSubcommand, resolveRealCargo, runCargoGuard, shouldGuardCargo } from "./cargo-guard.mjs";

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

test("Cargo guard resolves real Cargo through rustup or explicit override", () => {
  assert.equal(resolveRealCargo({ env: { RIGHTSUITE_REAL_CARGO: "/opt/toolchain/cargo" } }), path.resolve("/opt/toolchain/cargo"));
  const calls = [];
  const cargo = resolveRealCargo({ env: {}, run: (command, args) => { calls.push([command, args]); return { status: 0, stdout: "/real/cargo\n" }; } });
  assert.equal(cargo, path.resolve("/real/cargo"));
  assert.deepEqual(calls[0][1], ["which", "cargo"]);
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
    env: cacheEnv({ TEST: "1" }),
    resolveCargo: () => "/real/cargo",
    runHeavy: async (args, config) => { calls.push(["heavy", args, config.env]); return 0; },
    runLight: async (command, args, env) => { calls.push(["light", command, args, env]); return 0; },
  };
  assert.equal(await runCargoGuard(["test"], options), 0);
  assert.equal(await runCargoGuard(["fmt", "--check"], options), 0);
  assert.deepEqual(calls[0].slice(0, 2), ["heavy", ["--", "/real/cargo", "test"]]);
  assert.deepEqual(calls[1], ["light", "/real/cargo", ["fmt", "--check"], options.env]);
});
