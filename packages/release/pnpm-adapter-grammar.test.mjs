// M12: the door mirrors the broker's rightkit-pnpm adapter grammar (pnpm-adapter-grammar.mjs). The constant is compared with the live
// registration on a host that has it, so the mirror cannot drift from what the broker enforces without this file failing.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import { BUILD_LAUNCH_REFUSED, buildLaunch } from "./build-launch.mjs";
import { resolveManagedRoots } from "./managed-roots.mjs";
import { PNPM_ADAPTER, PNPM_ADAPTER_GRAMMAR, grammarFromSpec, liveAdapterGrammar, pnpmGrammarRefusal } from "./pnpm-adapter-grammar.mjs";

const refusal = (...words) => pnpmGrammarRefusal(words);

test("[M12] the first argument must be a registered subcommand; a leading flag that is not registered, --prefix and --config as global options are refused (the reference registration, GB-03)", () => {
  for (const words of [["run", "build"], ["exec", "tauri", "build", "--config", "{}"], ["pack", "--pack-destination", "/s/p"], ["install"], ["--dir", "hub", "run", "x"], ["-C", "hub", "install"],
    ["--filter", "@rightkit/x", "run", "test"], ["-r", "--workspace-concurrency=1", "run", "test"], ["--workspace-concurrency=1", "run", "t"], ["--dir", "a", "--filter", "b", "run", "t"], ["run", "--if-present", "test"], ["--dir", "hub", "--if-present", "run", "t"]]) {
    assert.equal(refusal(...words), null, words.join(" "));
  }
  for (const words of [[], ["build"], ["test"], ["--if-present", "run", "x"], ["--prefix", "hub", "run", "x"], ["--config", "x=y", "run", "x"], ["--config=x=y", "run", "x"], ["-w", "run", "x"], ["--store-dir", "/s", "install"], ["--frozen-lockfile", "install"],
    ["--help"], ["dlx", "x"], ["add", "x"], ["--dir", "hub", "--prefix", "x", "run", "t"], ["--dir", "hub", "--config", "x", "run", "t"], ["--dir"], ["-r", "-r=1", "run", "t"],
    // The reference registration lists none of these (the earlier mirror did): they are refused with the registration named.
    ["-F", "x", "run", "t"], ["--silent", "run", "t"], ["--recursive", "run", "t"], ["--version"], ["--reporter", "append-only", "run", "t"], ["install", "--frozen-lockfile"], ["exec", "node", "-e", "1"], ["exec", "sh", "-c", "true"]]) {
    assert.notEqual(refusal(...words), null, `${words.join(" ") || "(no arguments)"} is refused`);
  }
  assert.match(refusal("--prefix", "x", "run", "t"), /--prefix, --config|leading flag/);
  assert.match(refusal("--dir", "x", "--config", "y", "run", "t"), /flag "--config" is not registered/);
  // Arguments after the target of run and exec are the target's own: the tail of an exec and a run's own flags pass, unchanged.
  assert.equal(refusal("exec", "tauri", "build", "--config", "{}"), null);
  assert.equal(refusal("run", "test", "--anything"), null);
});

test("[M12] the door refuses a pnpm step by the mirrored grammar, and reads the live registration when the host has one", () => {
  const door = (args, host = {}) => buildLaunch("pnpm", args, { env: { PATH: "/usr/bin" }, platform: "darwin", exists: () => true, which: () => ["/managed/agent-bin/rustright"], readFile: () => "", ...host });
  assert.deepEqual(door(["run", "build"]).args, ["run", "rightkit-pnpm", "--", "run", "build"]);
  for (const args of [["--if-present", "run", "x"], ["--prefix", "hub", "run", "x"], ["--dir", "x", "--config", "y", "run", "t"], ["build"], []]) {
    assert.throws(() => door(args), (e) => e.code === BUILD_LAUNCH_REFUSED && /rightkit-pnpm/.test(e.message), args.join(" "));
  }
  // A live registration that registers another set wins over the constant.
  const spec = { adapters: { [PNPM_ADAPTER]: { subcommands: ["run", "pack"], flags: [{ name: "--dir", value: "relpath" }] } } };
  const readFile = () => JSON.stringify(spec);
  assert.deepEqual(door(["pack"], { readFile }).args, ["run", "rightkit-pnpm", "--", "pack"]);
  assert.throws(() => door(["exec", "tauri"], { readFile }), (e) => e.code === BUILD_LAUNCH_REFUSED && /subcommand "exec" is not registered/.test(e.message));
  // An unreadable or malformed registration falls back to the constant.
  assert.equal(liveAdapterGrammar({ readFile: () => { throw new Error("ENOENT"); } }), null);
  assert.equal(liveAdapterGrammar({ readFile: () => "{}" }), null);
  assert.deepEqual(door(["exec", "tauri", "build"], { readFile: () => "not json" }).args.slice(0, 4), ["run", "rightkit-pnpm", "--", "exec"]);
});

test("[M12] the constant equals the live rightkit-pnpm registration on this host (fails when the broker's spec and the door's mirror diverge)", (t) => {
  const { configPath } = resolveManagedRoots({ env: process.env, platform: process.platform });
  let text;
  try { text = fs.readFileSync(configPath, "utf8"); } catch { return t.skip(`no managed build-control config at ${configPath}: this host holds no rightkit-pnpm registration to compare`); }
  const spec = JSON.parse(text)?.adapters?.[PNPM_ADAPTER];
  if (!spec) return t.skip(`${configPath} registers no ${PNPM_ADAPTER}`);
  const live = grammarFromSpec(spec);
  assert.ok(live, "the live registration has the fields the door reads");
  assert.deepEqual([...PNPM_ADAPTER_GRAMMAR.subcommands], [...live.subcommands], "registered first arguments");
  assert.deepEqual([...PNPM_ADAPTER_GRAMMAR.env].sort(), [...live.env].sort(), "registered request-environment keys");
  assert.deepEqual(JSON.parse(JSON.stringify(PNPM_ADAPTER_GRAMMAR.opaqueTargets)), JSON.parse(JSON.stringify(live.opaqueTargets)), "registered exec targets");
  assert.deepEqual(PNPM_ADAPTER_GRAMMAR.flags, live.flags, "registered flags and their value kinds");
  assert.deepEqual([...PNPM_ADAPTER_GRAMMAR.opaqueAfterTarget], [...live.opaqueAfterTarget]);
  assert.deepEqual([...PNPM_ADAPTER_GRAMMAR.snapshotSubcommands], [...live.snapshotSubcommands]);
});
