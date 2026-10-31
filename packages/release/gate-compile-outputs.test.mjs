// GB-13 (GATE E6/E7/E16): a recorded compile is reused only while the outputs it recorded are on disk and the compile environment is the
// one it ran in; the sealed bytes carry compile and package separately.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { compileKeyOf, compileOutputsRecord, compileReuseProblem, outputCensus } from "./gate-stages.mjs";
import { doorFixture, tmpDir } from "./test-support/door-fixture.mjs";

const POSIX_SIM = process.platform === "win32" ? "simulates a Linux host, whose ownership reads ps" : false;

/** A compile double that writes two outputs into `out` and reports the directory the way the real compile does. */
function compiler(out, calls) {
  return async ({ reportOutputs } = {}) => {
    calls.push("compile");
    fs.mkdirSync(path.join(out, "debug"), { recursive: true });
    fs.writeFileSync(path.join(out, "debug", "app"), "binary-bytes");
    fs.writeFileSync(path.join(out, "debug", "libx.rlib"), "lib-bytes");
    reportOutputs?.([out]);
    return 0;
  };
}

test("[GB-13] unchanged repeat reuses the compile; a removed or changed output, or a changed profile/target environment, runs it again (warm, no cleared target)", { skip: POSIX_SIM }, async (t) => {
  const fx = doorFixture(t);
  const out = path.join(tmpDir(t, "rk-gb13-out-"), "target");
  const calls = [];
  const seams = { runCompile: compiler(out, calls) };
  assert.equal((await fx.door(["--to", "compile"], seams)).code, 0);
  assert.deepEqual(calls, ["compile"]);
  assert.equal(fx.receipts.get(fx.commit()).stages.compile.outputs.dirs[0], out, "the compile result records its output directory and census");

  let r = await fx.door(["--to", "compile"], seams);
  assert.deepEqual(calls, ["compile"], "an unchanged repeat stays warm");
  assert.match(r.out, /\[compile\] already passed .* outputs are intact/);

  fs.rmSync(path.join(out, "debug", "app"));
  r = await fx.door(["--to", "compile"], seams);
  assert.deepEqual(calls, ["compile", "compile"], "a removed required output is no already-passed compile");
  assert.match(r.out, /no longer stands: the compile outputs on disk differ/);
  assert.ok(fs.existsSync(path.join(out, "debug", "libx.rlib")), "the target was not cleared to get past it");

  await fx.door(["--to", "compile"], seams);
  assert.equal(calls.length, 2, "the re-run recorded the new census");
  fs.writeFileSync(path.join(out, "debug", "libx.rlib"), "tampered-with-another-size");
  await fx.door(["--to", "compile"], seams);
  assert.equal(calls.length, 3, "a changed output");

  const profile = { env: { ...fx.env, CARGO_BUILD_TARGET: "aarch64-apple-darwin" } };
  r = await fx.door(["--to", "compile"], { ...seams, ...profile });
  assert.equal(calls.length, 4, "a changed effective target");
  assert.match(r.out, /profile, target or flags/);
  await fx.door(["--to", "compile"], { ...seams, ...profile });
  assert.equal(calls.length, 4, "and warm again under that target");
  await fx.door(["--to", "compile"], { ...seams, env: { ...profile.env, CARGO_PROFILE_DEV_OPT_LEVEL: "3" } });
  assert.equal(calls.length, 5, "a changed profile");

  fs.rmSync(out, { recursive: true });
  r = await fx.door(["--to", "compile"], { ...seams, env: { ...profile.env, CARGO_PROFILE_DEV_OPT_LEVEL: "3" } });
  assert.equal(calls.length, 6, "a vanished output directory");
});

test("[GB-13] a compile receipt that recorded no outputs (an older receipt, or a hand-edited one) proves nothing and runs again", { skip: POSIX_SIM }, async (t) => {
  const fx = doorFixture(t);
  const out = path.join(tmpDir(t, "rk-gb13-legacy-"), "target");
  const calls = [];
  const seams = { runCompile: compiler(out, calls) };
  await fx.door(["--to", "compile"], seams);
  delete fx.receipts.get(fx.commit()).stages.compile.outputs;
  const r = await fx.door(["--to", "compile"], seams);
  assert.equal(calls.length, 2);
  assert.match(r.out, /records no outputs/);
});

test("[GB-13] sealed bytes carry compile and package on their own: the package stage's later writes to the target do not re-run compile for an intact sealed set", { skip: POSIX_SIM }, async (t) => {
  const fx = doorFixture(t);
  const out = path.join(tmpDir(t, "rk-gb13-sealed-"), "target");
  const calls = [];
  const sealed = { id: "app-1.0.0-00000000", manifest: "m", digest: "d".repeat(64) };
  // A package run is the native host's: this simulates the Mac, since a Linux host refuses `--platform mac` (E2E L3-03).
  const seams = { platform: "darwin", runCompile: compiler(out, calls), sealedFor: () => sealed, spawnInherit: () => { fs.writeFileSync(path.join(out, "debug", "bundle.dmg"), "packaged"); return { status: 0 }; } };
  assert.equal((await fx.door(["--to", "package", "--platform", "mac"], seams)).code, 0);
  const again = await fx.door(["--to", "package", "--platform", "mac"], seams);
  assert.equal(again.code, 0, again.all);
  assert.deepEqual(calls, ["compile"]);
  assert.match(again.out, /\[compile\] already passed for the sealed set/);
});

test("[GB-13] census and reuse helpers: missing directory is not empty; truncated walk proves nothing; env key ignores unrelated variables", (t) => {
  const dir = tmpDir(t, "rk-gb13-census-");
  const a = path.join(dir, "a"); fs.mkdirSync(a); fs.writeFileSync(path.join(a, "f"), "x");
  const rec = { outputs: compileOutputsRecord({ dirs: [a], env: {} }) };
  assert.equal(compileReuseProblem(rec, { env: {} }), null);
  assert.equal(compileReuseProblem(rec, { env: { PATH: "/x", HOME: "/h", CARGO_HOME: "/c", CARGO_TARGET_DIR: "/t" } }), null, "variables that do not change what Cargo builds do not invalidate");
  assert.match(compileReuseProblem(rec, { env: { RUSTFLAGS: "-C debuginfo=2" } }), /environment/);
  fs.rmSync(a, { recursive: true });
  assert.match(compileReuseProblem(rec, { env: {} }), /is gone/);
  assert.equal(outputCensus([path.join(dir, "nope")]).missing.length, 1);
  fs.mkdirSync(a); for (const n of ["1", "2", "3"]) fs.writeFileSync(path.join(a, n), n);
  assert.equal(outputCensus([a], { limit: 2 }).truncated, true);
  assert.match(compileReuseProblem({ outputs: { ...compileOutputsRecord({ dirs: [a] }), truncated: true } }, {}), /incomplete/);
  assert.notEqual(compileKeyOf({ CARGO_BUILD_TARGET: "x" }), compileKeyOf({}));
});
