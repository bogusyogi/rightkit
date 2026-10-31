// GB-12 (GATE 36/44/86, E18): Class A admission runs at every stage boundary; an outer receipt boolean or an intact sealed set never
// stands in for it. The deterministic scans reuse through their own complete key; the live scans (never cached) are re-asked.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { doorFixture } from "./test-support/door-fixture.mjs";

const POSIX_SIM = process.platform === "win32" ? "simulates a Linux host, whose ownership reads ps" : false;

test("[GB-12] a second --to compile for the same commit asks Class A again at the stage boundary (the receipt's outer check boolean is no admission)", { skip: POSIX_SIM }, async (t) => {
  const fx = doorFixture(t);
  const ran = [];
  const seams = { runCheck: async () => { ran.push("check"); return 0; }, runCompile: async () => { ran.push("compile"); return 0; } };
  assert.equal((await fx.door(["--to", "compile"], seams)).code, 0);
  const again = await fx.door(["--to", "compile"], seams);
  assert.equal(again.code, 0, again.all);
  assert.deepEqual(ran.filter((s) => s === "check"), ["check", "check"], "Class A ran on both runs, the second one included");
  assert.doesNotMatch(again.out, /\[check\] already passed/, "no outer boolean shortcut for check");
});

test("[GB-12] a red Class A on the repeat run (an advisory that turned up with every deterministic input unchanged) turns the stage red; nothing after it runs", { skip: POSIX_SIM }, async (t) => {
  const fx = doorFixture(t);
  let n = 0; const ran = [];
  const seams = { runCheck: async () => { ran.push("check"); return n++ === 0 ? 0 : 1; }, runCompile: async () => { ran.push("compile"); return 0; } };
  assert.equal((await fx.door(["--to", "compile"], seams)).code, 0);
  const second = await fx.door(["--to", "compile"], seams);
  assert.equal(second.code, 1, "the repeat run is red");
  assert.deepEqual(ran, ["check", "compile", "check"], "the failing admission stops the run before compile");
  assert.match(second.err, /--to compile stopped at check/);
});

test("[GB-12] an intact sealed package set does not stand in for Class A either: --to package again re-asks check, and still reuses the sealed compile and package", { skip: POSIX_SIM }, async (t) => {
  const fx = doorFixture(t);
  const ran = [];
  const sealed = { id: "app-1.0.0-00000000", manifest: "m", digest: "d".repeat(64) };
  // A package run is the native host's: this simulates the Mac, since a Linux host refuses `--platform mac` (E2E L3-03).
  const seams = { platform: "darwin", sealedFor: () => sealed, runCheck: async () => { ran.push("check"); return 0; }, runCompile: async () => { ran.push("compile"); return 0; }, spawnInherit: () => { ran.push("right-release"); return { status: 0 }; } };
  const first = await fx.door(["--to", "package", "--platform", "mac"], seams);
  assert.equal(first.code, 0, first.all);
  const ranFirst = ran.length;
  const again = await fx.door(["--to", "package", "--platform", "mac"], seams);
  assert.equal(again.code, 0, again.all);
  assert.deepEqual(ran.slice(ranFirst), ["check"], "only Class A is asked again; the sealed bytes carry compile and package");
  assert.match(again.out, /\[package\] already passed for the sealed set/);
});
