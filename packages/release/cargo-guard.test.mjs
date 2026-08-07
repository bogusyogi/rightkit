import assert from "node:assert/strict";
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
  assert.equal(resolveRealCargo({ env: { RIGHTSUITE_REAL_CARGO: "/opt/toolchain/cargo" } }), "/opt/toolchain/cargo");
  const calls = [];
  const cargo = resolveRealCargo({ env: {}, run: (command, args) => { calls.push([command, args]); return { status: 0, stdout: "/real/cargo\n" }; } });
  assert.equal(cargo, "/real/cargo");
  assert.deepEqual(calls[0][1], ["which", "cargo"]);
});

test("Cargo guard routes heavy and light commands without recursion", async () => {
  const calls = [];
  const options = {
    env: { TEST: "1" },
    resolveCargo: () => "/real/cargo",
    runHeavy: async (args, config) => { calls.push(["heavy", args, config.env]); return 0; },
    runLight: async (command, args, env) => { calls.push(["light", command, args, env]); return 0; },
  };
  assert.equal(await runCargoGuard(["test"], options), 0);
  assert.equal(await runCargoGuard(["fmt", "--check"], options), 0);
  assert.deepEqual(calls[0], ["heavy", ["--", "/real/cargo", "test"], options.env]);
  assert.deepEqual(calls[1], ["light", "/real/cargo", ["fmt", "--check"], options.env]);
});
