import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { claudeDoorDecision, doorDenial, doorViolation, simpleCommands } from "./door-policy.mjs";

test("one door: every way an agent could build an app outside rightkit-gate is named", () => {
  const denied = {
    "cargo build --release": "cargo build", "cargo test -p a": "cargo test", "cargo +1.98.0 clippy --all-targets": "cargo clippy", "cargo check": "cargo check",
    "RUSTFLAGS=-D cargo run": "cargo run", "env CI=1 cargo bench": "cargo bench", "cd app && cargo b": "cargo b", "timeout 600 cargo t": "cargo t",
    "pnpm tauri build": "tauri build", "pnpm exec tauri dev": "tauri dev", "npx tauri build --debug": "tauri build", "yarn tauri bundle": "tauri bundle",
    "pnpm --filter app exec tauri build": "tauri build", "cargo tauri build": "cargo tauri build",
    "right-release build --platform win": "right-release build", "npx right-release upload --release r --platform win --tier patch": "right-release upload",
    "node node_modules/@rightkit/release/cli/right-release.mjs qa --stage installed": "right-release qa", "echo hi; right-release package": "right-release package",
  };
  for (const [command, violation] of Object.entries(denied)) assert.equal(doorViolation(command), violation, command);
});

test("one door: the door itself, the broker, read-only cargo and ordinary commands are allowed", () => {
  for (const command of [
    "rightkit-gate --to compile", "pnpm exec rightkit-gate --to package --platform mac", "npx rightkit-gate check --commit HEAD",
    "rustright cargo build", "rustright cargo test -p a", "cargo fmt --all -- --check", "cargo metadata --no-deps --format-version 1",
    "cargo update -p serde --precise 1.0.1", "cargo tree", "cargo --version", "right-release check",
    "git commit -m 'cargo build fixed'", "echo \"pnpm tauri build\"", "grep -rn 'cargo test' docs", "pnpm install --frozen-lockfile", "pnpm run lint", "",
  ]) assert.equal(doorViolation(command), null, command);
});

test("one door: the denial names the stage to run, and the Claude hook denies through PreToolUse", () => {
  assert.match(doorDenial("cargo test"), /Required: rightkit-gate --to compile /);
  assert.match(doorDenial("tauri build"), /rightkit-gate --to package --platform mac\|win/);
  assert.match(doorDenial("right-release qa"), /--to qualify --platform mac\|win/);
  assert.match(doorDenial("right-release upload"), /--to release --platform mac\|win --tier patch\|update/);
  const deny = claudeDoorDecision({ tool_name: "Bash", tool_input: { command: "cargo build" } });
  assert.equal(deny.hookSpecificOutput.permissionDecision, "deny");
  assert.match(deny.hookSpecificOutput.permissionDecisionReason, /^\[HOOK:one-door:block\]/);
  assert.deepEqual(claudeDoorDecision({ tool_name: "Bash", tool_input: { command: "rightkit-gate --to compile" } }), {});
  assert.deepEqual(claudeDoorDecision({ tool_name: "Read" }), {});
  assert.deepEqual(simpleCommands("a 'b c' && d \"e;f\" | g"), [["a", "b c"], ["d", "e;f"], ["g"]]);
});

test("the agent hook binary: stdin JSON in, the decision out; unreadable input allows", () => {
  const bin = path.join(path.dirname(fileURLToPath(import.meta.url)), "bin", "rightkit-agent-hook.mjs");
  const run = (input) => JSON.parse(spawnSync(process.execPath, [bin], { input, encoding: "utf8" }).stdout);
  assert.equal(run(JSON.stringify({ tool_name: "Bash", tool_input: { command: "pnpm tauri build" } })).hookSpecificOutput.permissionDecision, "deny");
  assert.deepEqual(run(JSON.stringify({ tool_name: "Bash", tool_input: { command: "ls" } })), {});
  assert.deepEqual(run("not json"), {});
});

test("dev and test compile too: right-release dev|test and tauri dev are denied with the --to dev|test door", () => {
  for (const [command, door] of [["pnpm exec right-release dev --platform mac", "--to dev"], ["right-release test", "--to test"], ["npx tauri dev", "--to dev"], ["right-release stage-sidecar", "--to package"]]) {
    const v = doorViolation(command);
    assert.ok(v, command);
    assert.ok(doorDenial(v).includes(`rightkit-gate ${door}`), `${command}: ${doorDenial(v)}`);
  }
  assert.equal(doorViolation("rightkit-gate --to dev -- --platform mac"), null);
});
