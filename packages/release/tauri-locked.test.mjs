// rightkit#1301: --locked reaches Cargo through the Tauri CLI in a carried stage (the exact forwarding form is PENDING RustRight's
// confirmation; tauri-cli 2.12.4 source, per the coordinator), and a configured Cargo step is never sent around an unreadable carrier.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { BUILD_LAUNCH_REFUSED, buildLaunch } from "./build-launch.mjs";
import { SHIM_MAP_SCHEMA } from "./cargo-target-shim.mjs";
import { CARRIED_CARGO_MAP_ENV } from "./carried-cargo.mjs";
import { lockedTauriArgs } from "./tauri-locked.mjs";

function carrierEnv(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-tl-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const map = path.join(dir, "map.json");
  fs.writeFileSync(map, JSON.stringify({ schema: SHIM_MAP_SCHEMA, mode: "broker", broker: {}, ledgerDir: path.join(dir, "ledger"), workspaces: {} }));
  return { dir, env: { [CARRIED_CARGO_MAP_ENV]: map } };
}

test("a tauri build or dev step in a carried stage gets --locked right after the first --, added with its own -- when there is none; a locked or frozen step is left alone", (t) => {
  const { env } = carrierEnv(t);
  assert.deepEqual(lockedTauriArgs(["exec", "tauri", "build", "--no-bundle", "--target", "x"], env), ["exec", "tauri", "build", "--no-bundle", "--target", "x", "--", "--locked"]);
  assert.deepEqual(lockedTauriArgs(["exec", "tauri", "build", "--", "--profile", "fast-dev"], env), ["exec", "tauri", "build", "--", "--locked", "--profile", "fast-dev"]);
  assert.deepEqual(lockedTauriArgs(["tauri", "dev"], env), ["tauri", "dev", "--", "--locked"]);
  assert.deepEqual(lockedTauriArgs(["tauri", "dev", "--", "--features", "a", "--", "appArg"], env), ["tauri", "dev", "--", "--locked", "--features", "a", "--", "appArg"], "--locked stays before the second --");
  assert.deepEqual(lockedTauriArgs(["exec", "tauri", "build", "--", "--locked"], env), ["exec", "tauri", "build", "--", "--locked"]);
  assert.deepEqual(lockedTauriArgs(["tauri", "build", "--", "--frozen", "-j", "2"], env), ["tauri", "build", "--", "--frozen", "-j", "2"]);
  // Only a Tauri build or dev; and only in a carried stage.
  assert.deepEqual(lockedTauriArgs(["exec", "tauri", "signer", "sign", "x"], env), ["exec", "tauri", "signer", "sign", "x"]);
  assert.deepEqual(lockedTauriArgs(["run", "build"], env), ["run", "build"]);
  assert.deepEqual(lockedTauriArgs(["exec", "tauri", "build"], {}), ["exec", "tauri", "build"], "a stage with no carrier (a direct host) is unchanged");
});

test("M1: a configured cargo or rustright cargo step whose carrier map cannot be read is refused, never sent to the uncarried client", () => {
  const CLIENT = "/managed/agent-bin/rustright";
  const host = { env: { PATH: "/managed/agent-bin", [CARRIED_CARGO_MAP_ENV]: "/no/such/map.json" }, platform: "darwin", exists: (p) => p === CLIENT, which: (c) => (c === "rustright" ? [CLIENT] : []) };
  for (const [command, args] of [["cargo", ["build"]], ["rustright", ["cargo", "test"]], ["/x/cargo", ["check"]]]) {
    assert.throws(() => buildLaunch(command, args, host), (e) => e.code === BUILD_LAUNCH_REFUSED && /cannot be read/.test(e.message) && /never sent to the client/.test(e.message), command);
  }
  // A pnpm, node or QA step is carried the same way: a map that cannot be read refuses it, never the client's adapter job around the carrier.
  for (const [command, args] of [["pnpm", ["run", "x"]], ["node", ["scripts/x.mjs"]]]) {
    assert.throws(() => buildLaunch(command, args, { ...host, cwd: "/app" }), (e) => e.code === BUILD_LAUNCH_REFUSED && /cannot be read/.test(e.message) && /never sent to the client/.test(e.message), command);
  }
  // A QA step runs outside the broker (it compiles nothing), but a stage whose carrier map cannot be read still refuses it.
  assert.throws(() => buildLaunch("rightkit-qa", ["--all"], { ...host, cwd: "/app" }), (e) => e.code === BUILD_LAUNCH_REFUSED && /cannot be read/.test(e.message) && /never run around its carrier/.test(e.message));
  // A missing map variable is no carrier.
  assert.equal(buildLaunch("pnpm", ["run", "x"], { ...host, env: { PATH: "/managed/agent-bin" } }).command, CLIENT);
  assert.deepEqual(buildLaunch("cargo", ["build"], { ...host, env: { PATH: "/managed/agent-bin" } }), { command: CLIENT, args: ["cargo", "build"] });
});

test("the sources of the Tauri builds a stage runs apply the rule: recipe-build and development", () => {
  for (const file of ["recipe-build.mjs", "development.mjs"]) assert.match(fs.readFileSync(new URL(`./${file}`, import.meta.url), "utf8"), /lockedTauriArgs\(/, file);
});
