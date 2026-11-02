// GB-13 (GATE E6/E7/E16): a recorded compile is reused only while the outputs it recorded are on disk and the compile environment is the
// one it ran in; the sealed bytes carry compile and package separately.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { createHash } from "node:crypto";
import { compileKeyOf, compileOutputsRecord, compileReuseProblem, compiledBinaries, outputCensus } from "./gate-stages.mjs";
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

test("[rustright#209 item 5] a passed compile records the sha256 of the bin targets it built, and nothing else in debug/, also when its census is truncated", (t) => {
  const out = path.join(tmpDir(t, "rk-bins-"), "target");
  fs.mkdirSync(path.join(out, "debug", "deps"), { recursive: true });
  const exe = process.platform === "win32" ? ".exe" : "";
  fs.writeFileSync(path.join(out, "debug", `rustright${exe}`), "client-bytes", { mode: 0o755 });
  fs.writeFileSync(path.join(out, "debug", `rustright-broker${exe}`), "broker-bytes", { mode: 0o755 });
  fs.writeFileSync(path.join(out, "debug", `old-renamed-bin${exe}`), "stale bytes of a target this commit no longer has", { mode: 0o755 });
  fs.writeFileSync(path.join(out, "debug", "rustright.d"), "dep-info", { mode: 0o644 });
  const sha = (text) => createHash("sha256").update(text).digest("hex");
  assert.deepEqual(compiledBinaries({ [out]: ["rustright-broker", "rustright", "rustright", "not-built-here"] }), [
    { dir: out, path: `debug/not-built-here${exe}`, name: `not-built-here${exe}`, size: null, sha256: null },
    { dir: out, path: `debug/rustright${exe}`, name: `rustright${exe}`, size: 12, sha256: sha("client-bytes") },
    { dir: out, path: `debug/rustright-broker${exe}`, name: `rustright-broker${exe}`, size: 12, sha256: sha("broker-bytes") },
  ], "only the built bin targets, each once; a missing one is recorded as missing; the stale executable is never recorded");

  const truncated = compileOutputsRecord({ dirs: [out], builtBins: { [out]: ["rustright"], "/elsewhere/target": ["x"] }, census: (dirs) => ({ dirs, entries: 250001, digest: "d", truncated: true, missing: [] }) });
  assert.equal(truncated.truncated, true);
  assert.equal(truncated.binariesOf, "built-bin-targets/2");
  assert.deepEqual(truncated.binaries.map((b) => b.name), [`rustright${exe}`], "recorded even when the census proves nothing; only directories the compile reported");
  assert.equal(compileOutputsRecord({ dirs: [out] }).binaries, undefined, "a compile that reported no built targets records no binaries and no marker");
});

test("[rustright#209 item 5] a Windows built target is <bin>.exe", () => {
  const rows = compiledBinaries({ T: ["a"] }, { platform: "win32", stat: () => ({ isFile: () => true, size: 1 }), readFile: () => Buffer.from("x") });
  assert.deepEqual(rows.map((b) => b.name), ["a.exe"]);
});

test("[rustright#209 item 5] a broker --target build is read from <dir>/<triple>/debug, never the stale <dir>/debug beside it", (t) => {
  const out = path.join(tmpDir(t, "rk-bins-triple-"), "target");
  const exe = process.platform === "win32" ? ".exe" : "";
  fs.mkdirSync(path.join(out, "debug"), { recursive: true });
  fs.mkdirSync(path.join(out, "aarch64-apple-darwin", "debug"), { recursive: true });
  fs.writeFileSync(path.join(out, "debug", `rustright${exe}`), "stale-from-october", { mode: 0o755 });
  fs.writeFileSync(path.join(out, "aarch64-apple-darwin", "debug", `rustright${exe}`), "this-compile", { mode: 0o755 });
  const sha = (text) => createHash("sha256").update(text).digest("hex");
  assert.deepEqual(compiledBinaries({ [out]: { profile: "aarch64-apple-darwin/debug", bins: ["rustright"] } }), [
    { dir: out, path: `aarch64-apple-darwin/debug/rustright${exe}`, name: `rustright${exe}`, size: 12, sha256: sha("this-compile") },
  ]);
  assert.deepEqual(compiledBinaries({ [out]: { profile: null, bins: ["rustright"] } }), [], "no known profile (a broker row without a triple): nothing is recorded");
  assert.deepEqual(compiledBinaries({ [out]: { profile: "../escape", bins: ["rustright"] } }), [], "a profile is a plain relative directory");
});
