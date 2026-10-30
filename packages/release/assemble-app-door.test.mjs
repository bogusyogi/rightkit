import "./test-support/broker-guard.mjs";
// M27 (gate-vs-broker audit 2026-10-30): `assemble-app --build` prepare steps go through the one door, never straight from the checkout.
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { runPrepare } from "./assemble-app-command.mjs";
import { BUILD_LAUNCH_REFUSED, buildLaunch } from "./build-launch.mjs";
import { SHIM_MAP_SCHEMA } from "./cargo-target-shim.mjs";
import { CARRIED_CARGO_MAP_ENV } from "./carried-cargo.mjs";

const CLIENT = "/managed/agent-bin/rustright";
const hostOf = (env) => ({ platform: "darwin", exists: (p) => p === CLIENT, which: (c) => (c === "rustright" ? [CLIENT] : []), env });
const specOf = (...steps) => ({ prepare: { release: steps } });

test("M27: a cargo prepare step is one carried door launch (the carrier's run entry, not the checkout's cargo); an own right-release step keeps resolveCommand", (t) => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-assemble-door-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const mapFile = path.join(dir, "map.json");
  writeFileSync(mapFile, JSON.stringify({ schema: SHIM_MAP_SCHEMA, mode: "broker", broker: { checkout: "/primary", snapshot: { path: "/snap" } }, ledgerDir: dir, workspaces: {} }));
  const env = { PATH: "/managed/agent-bin", [CARRIED_CARGO_MAP_ENV]: mapFile };
  const doorCalls = [];
  const door = (cmd, args, host) => { doorCalls.push({ cmd, args, stepEnv: host.stepEnv, cwd: host.cwd }); return buildLaunch(cmd, args, { ...hostOf(host.env), cwd: host.cwd }); };
  const runs = [];
  const run = (command, args, options) => { runs.push({ command, args, env: options.env, cwd: options.cwd }); return { status: 0 }; };
  const resolved = [];
  const resolveCommand = (cmd, args) => { resolved.push([cmd, ...args]); return { command: process.execPath, args: ["own-right-release.mjs", ...args] }; };
  runPrepare({
    root: "/snap/app", env, run, resolveCommand, door, log: () => {},
    spec: specOf({ cmd: "cargo", args: ["build", "-p", "helper", "--release"], cwd: "tools" }, { cmd: "pnpm", args: ["exec", "right-release", "stage-sidecar"] }),
  });
  assert.equal(doorCalls.length, 1, "the cargo step went through the door; the own right-release step did not");
  assert.deepEqual(doorCalls[0].args, ["build", "-p", "helper", "--release"]);
  assert.equal(doorCalls[0].cwd, path.resolve("/snap/app", "tools"));
  assert.deepEqual(resolved, [["pnpm", "exec", "right-release", "stage-sidecar"]]);
  assert.equal(runs[0].command, process.execPath);
  assert.ok(runs[0].args[0].endsWith("carried-cargo-run.mjs"), "one carried (ledgered) call, not a bare cargo from the checkout");
  assert.deepEqual(runs[0].args.slice(1), ["build", "-p", "helper", "--release"]);
  assert.equal(runs[0].cwd, path.resolve("/snap/app", "tools"));
  assert.equal(runs[1].args[0], "own-right-release.mjs");
});

test("M27: a prepare command the door cannot carry fails ASSEMBLY_BUILD_FAILED naming the door, and runs nothing; a step env of signing secrets is refused", () => {
  const env = { PATH: "/managed/agent-bin" };
  const door = (cmd, args, host) => buildLaunch(cmd, args, { ...hostOf(host.env), cwd: host.cwd });
  const runs = [];
  const run = (...a) => { runs.push(a); return { status: 0 }; };
  assert.throws(
    () => runPrepare({ root: "/w/app", env, run, resolveCommand: () => assert.fail("not an own step"), door, log: () => {}, spec: specOf({ cmd: "make", args: ["all"] }) }),
    (e) => e.code === "ASSEMBLY_BUILD_FAILED" && /refused by the build door/.test(e.message),
  );
  assert.equal(runs.length, 0);
  assert.equal(BUILD_LAUNCH_REFUSED, "RK_BUILD_LAUNCH_REFUSED");
  assert.throws(
    () => runPrepare({ root: "/w/app", env, run, resolveCommand: () => assert.fail("not an own step"), door, log: () => {}, spec: specOf({ cmd: "pnpm", args: ["run", "build"], env: { TAURI_SIGNING_PRIVATE_KEY: "s3cr3t" } }) }),
    (e) => e.code === "ASSEMBLY_BUILD_FAILED" && !e.message.includes("s3cr3t"),
  );
  assert.equal(runs.length, 0);
});
