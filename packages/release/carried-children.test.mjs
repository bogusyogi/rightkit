// rightkit#1301 F3 and F5: configured cargo steps are carried through the seam on every platform, and a carried stage's child gets no
// output-directory variable.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { carriedEnv } from "./bin/rightkit-gate.mjs";
import { buildLaunch } from "./build-launch.mjs";
import { SHIM_MAP_SCHEMA } from "./cargo-target-shim.mjs";
import { CARRIED_CARGO_MAP_ENV } from "./carried-cargo.mjs";

const CLIENT = "/managed/agent-bin/rustright";
const managed = { env: { PATH: "/managed/agent-bin" }, platform: "darwin", exists: (p) => p === CLIENT, which: (c) => (c === "rustright" ? [CLIENT] : []) };
const direct = { env: { PATH: "/usr/bin" }, platform: "linux", exists: () => false, which: () => [] };

test("F3: in a snapshot stage with a carrier a configured cargo or rustright cargo step is carried through the seam, on darwin and win32; without one it goes by the client; a direct host never carries", (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-bl-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const map = path.join(dir, "map.json");
  fs.writeFileSync(map, JSON.stringify({ schema: SHIM_MAP_SCHEMA, mode: "broker", broker: {}, ledgerDir: path.join(dir, "ledger"), workspaces: {} }));
  const runner = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "carried-cargo-run.mjs");
  for (const platform of ["darwin", "win32"]) {
    const host = { ...managed, platform, env: { ...managed.env, [CARRIED_CARGO_MAP_ENV]: map } };
    assert.deepEqual(buildLaunch("cargo", ["test", "--locked"], host), { command: process.execPath, args: [runner, "test", "--locked"] }, platform);
    assert.deepEqual(buildLaunch("rustright", ["cargo", "build"], host), { command: process.execPath, args: [runner, "build"] }, platform);
    assert.deepEqual(buildLaunch("/x/cargo.exe", ["check"], host).args, [runner, "check"], "named by path, still carried");
    if (platform !== "darwin") continue; // the client lookup below is the POSIX fixture
    // A pnpm, node or QA step of a snapshot stage is a carried ADAPTER job (carried-adapter-run.mjs), never the client's `run`.
    const adapterRunner = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "carried-adapter-run.mjs");
    assert.deepEqual(buildLaunch("pnpm", ["run", "x"], host), { command: process.execPath, args: [adapterRunner, "rightkit-pnpm", "--", "run", "x"] });
    assert.deepEqual(buildLaunch("rightkit-qa", ["--all"], host), { command: process.execPath, args: [adapterRunner, "rightkit-qa", "--", "--all"] });
    assert.deepEqual(buildLaunch("node", ["scripts/x.mjs", "a"], { ...host, cwd: "/app" }), { command: process.execPath, args: [adapterRunner, "rightkit-node", "--", "scripts/x.mjs", "a"] });
    // A rustright command that is not cargo is not the client's in a snapshot stage (door-closure.test.mjs): `run <adapter> -- <args>` is that adapter's carried step, anything else is refused.
    assert.deepEqual(buildLaunch("rustright", ["run", "rightkit-qa", "--", "--all"], host), { command: process.execPath, args: [adapterRunner, "rightkit-qa", "--", "--all"] });
    assert.throws(() => buildLaunch("rustright", ["run", "rightkit-qa"], host), (e) => e.code === "RK_BUILD_LAUNCH_REFUSED");
  }
  assert.deepEqual(buildLaunch("cargo", ["build"], managed), { command: CLIENT, args: ["cargo", "build"] }, "no carrier in the environment");
  assert.deepEqual(buildLaunch("pnpm", ["run", "x"], managed), { command: CLIENT, args: ["run", "rightkit-pnpm", "--", "run", "x"] }, "no carrier in the environment: the client's adapter job");
  assert.deepEqual(buildLaunch("cargo", ["build"], { ...direct, env: { ...direct.env, [CARRIED_CARGO_MAP_ENV]: map } }), { command: "cargo", args: ["build"] }, "a direct host runs the toolchain");
});

test("F5: a carried stage's child gets no CARGO_TARGET_DIR or CARGO_BUILD_TARGET_DIR (any case), so the Tauri CLI does not look for bundles in the wrong place", () => {
  const carrier = { carried: true, env: { RIGHTKIT_CARRIED_CARGO_MAP: "/m", PATH: "/shim:/bin" } };
  const out = carriedEnv(carrier, { PATH: "/bin", CARGO_TARGET_DIR: "/t", Cargo_Build_Target_Dir: "/u", KEEP: "1" }, { CARGO_BUILD_TARGET_DIR: "/v", X: "2" });
  assert.deepEqual(out, { PATH: "/shim:/bin", RIGHTKIT_CARRIED_CARGO_MAP: "/m", KEEP: "1", X: "2" });
  // A stage with no carrier (a direct host) keeps what it pinned.
  assert.deepEqual(carriedEnv({ carried: false, env: {} }, { CARGO_TARGET_DIR: "/t" }, { A: "1" }), { CARGO_TARGET_DIR: "/t", A: "1" });
});

test("F3: carried-cargo-run.mjs is the seam as a process: one carried call from the working directory, its status the exit status", () => {
  const source = fs.readFileSync(new URL("./carried-cargo-run.mjs", import.meta.url), "utf8");
  assert.match(source, /runCargoSync\(process\.argv\.slice\(2\), \{ cwd: process\.cwd\(\), env: process\.env, stdio: "inherit"/);
  assert.match(source, /process\.exit\(result\.status \?\? 1\)/);
});
