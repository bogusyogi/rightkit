// GB-68 (BROKER-CONTRACT section 16; GATE warm rule, E16, B9): the direct freshness census (freshness-census.mjs) is held to the same inputs and results as RustRight's
// census. The vector is RustRight's docs/contracts/freshness-evidence-vectors.v1.json (rustright.freshness-evidence-vectors.v1), copied byte for byte into
// test-support/vectors/ and pinned in vectors-source.json; the Rust side is rustright-broker job_service/freshness_vectors.rs. Every case is judged here through the production
// code: the reason-to-cause table, the profile directory a command line selects, the root-execution cutoff, the FingerprintMissing cause chain, the caps, the selected scope
// (never every profile or triple) and the census states (a read error is unavailable, never empty and never a first build).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import * as census from "./freshness-census.mjs";
import { loadVector, vectorsSource } from "./test-support/vectors.mjs";

const FILE = "freshness-evidence-vectors.v1.json";
// The digest of RustRight's committed blob (docs/contracts/freshness-evidence-vectors.v1.json at commit ddfb07b, unchanged at 8a97e08). Changing the copy means recopying it from a
// named RustRight commit AND changing this constant and vectors-source.json together: a deliberate update, never a silent one.
const PINNED_SHA256 = "13a87427d659d33fb586dd284d652280663c02c437d4ad89cf440a0cd036b5a8";

const vector = loadVector(FILE);
const tmp = (t) => { const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-fresh-vec-"))); t.after(() => fs.rmSync(dir, { recursive: true, force: true })); return dir; };
const layOut = (root, files) => { for (const file of files) { const target = path.join(root, ...file.split("/")); fs.mkdirSync(path.dirname(target), { recursive: true }); fs.writeFileSync(target, "x"); } };
const posix = (root, p) => path.relative(root, p).split(path.sep).join("/");

test("[GB-68] drift: the copy of RustRight's freshness vector is the pinned bytes, the pin names its source commit, and a changed copy fails here until it is deliberately re-pinned", () => {
  const bytes = fs.readFileSync(new URL(`./test-support/vectors/${FILE}`, import.meta.url));
  const digest = crypto.createHash("sha256").update(bytes).digest("hex");
  const pin = vectorsSource().files[FILE];
  assert.ok(pin, "the vector is pinned in vectors-source.json");
  assert.equal(pin.schema, "rustright.freshness-evidence-vectors.v1");
  assert.match(pin.commit, /^[0-9a-f]{40}$/, "the pin names the RustRight commit the bytes were copied from");
  assert.equal(pin.sha256, digest, "vectors-source.json pins the bytes of the copy");
  assert.equal(digest, PINNED_SHA256, "the copy changed: recopy it from a named RustRight commit and update PINNED_SHA256, vectors-source.json and the commit together");
  assert.equal(vector.schema, "rustright.freshness-evidence-vectors.v1");
  assert.ok(/^[0-9a-f]{40}$/.test(vectorsSource().source.commit));
});

test("[GB-68] the vector states the caps and the reason-to-cause table the census uses (every Cargo dirty-reason kind, FingerprintMissing named apart, the rest unexplained)", () => {
  for (const [kind, cause] of Object.entries(vector.causes)) assert.equal(census.causeOf(kind), cause, kind);
  assert.equal(census.REASONS_CAP, vector.caps.reasons);
  assert.equal(census.REASON_TEXT_CAP, vector.caps.reasonTextChars);
  // The kinds Cargo's DirtyReason also has, classified as RustRight classifies them (cargo_evidence.rs cause_of).
  for (const kind of ["NumberOfDependenciesChanged", "UnitDependencyNameChanged", "UnitDependencyInfoChanged", "LocalLengthsChanged", "PrecalculatedComponentsChanged", "DepInfoOutputChanged", "RerunIfChangedOutputFileChanged", "RerunIfChangedOutputPathsChanged"]) assert.equal(census.causeOf(kind), "source-or-dependency-changed", kind);
  for (const kind of ["EnvVarsChanged", "DeclaredFeaturesChanged", "TargetConfigurationChanged", "LocalFingerprintTypeChanged", "FingerprintMethodChanged", "ConfigSettingsChanged"]) assert.equal(census.causeOf(kind), "env-rustflags-profile-features-or-target-changed", kind);
  for (const kind of ["failed", "unknown", "", "SomethingElse"]) assert.equal(census.causeOf(kind), "unexplained", `unclassified ${JSON.stringify(kind)} stays unexplained`);
});

test("[GB-68] the profile directory a command line selects and whether it asks for JSON messages match the vector", () => {
  assert.ok(vector.profileDirectory.length >= 11 && vector.wantsJson.length >= 5);
  for (const c of vector.profileDirectory) assert.equal(census.cargoProfileDirectory(c.argv), c.directory, JSON.stringify(c.argv));
  for (const c of vector.wantsJson) assert.equal(census.wantsCargoJson(c.argv), c.expect, JSON.stringify(c.argv));
});

const readLog = (c) => {
  const log = new census.FingerprintLog({ strip: c.strip ?? false });
  let shown = "";
  if (c.repeat) {
    for (let i = 0; i < c.repeat.count; i += 1) {
      const unit = `${c.repeat.unitPrefix}${i}`;
      shown += log.feed(` INFO cargo::core::compiler::fingerprint: fingerprint dirty for ${unit} v0.1.0 (/w/${unit})/Check { test: false }/TargetInner { .. }\n`);
      shown += log.feed(" INFO cargo::core::compiler::fingerprint:     dirty: FreshBuild\n");
    }
  }
  for (const chunk of c.chunks ?? []) shown += log.feed(chunk);
  shown += log.finish();
  return { log, shown };
};

test("[GB-68] every log vector yields RustRight's root units, kinds, causes, counts and caps: the root-execution cutoff, the FingerprintMissing cause chain, colour and split chunks", () => {
  assert.equal(vector.logs.length, 8);
  for (const c of vector.logs) {
    const { log, shown } = readLog(c);
    const expect = c.expect;
    assert.equal(log.dirtyUnits, expect.dirtyUnits, c.name);
    assert.equal(log.truncated, expect.truncated, c.name);
    if (expect.reasonCount !== undefined) assert.equal(log.reasons.length, expect.reasonCount, c.name);
    if (expect.reasons) {
      assert.equal(log.reasons.length, expect.reasons.length, `${c.name}: no reason beyond the expected ones`);
      expect.reasons.forEach((wanted, i) => {
        for (const field of ["unit", "target", "kind", "cause"]) assert.equal(log.reasons[i][field], wanted[field], `${c.name}: reason ${i} ${field}`);
        if (wanted.reasonChars !== undefined) assert.equal(Array.from(log.reasons[i].reason).length, wanted.reasonChars, c.name);
      });
    }
    for (const want of expect.shownContains ?? []) assert.ok(shown.includes(want), `${c.name}: a line that is not the root's reaches the output untouched: ${JSON.stringify(shown)}`);
    for (const lack of expect.shownLacks ?? []) assert.ok(!shown.includes(lack), `${c.name}: the root's own log lines stay out of the output: ${JSON.stringify(shown)}`);
    if (expect.stripped) assert.ok(!shown.includes("fingerprint") && !shown.includes("Caused by"), `${c.name}: the log's own lines stay out of the output: ${JSON.stringify(shown)}`);
  }
});

test("[GB-68] negative: every Cargo execution status word ends the root window (and a pre-resolution word does not); nested chatter after it is neither counted nor stripped, a failed read with another cause stays unexplained", () => {
  const dirty = (name) => ` INFO cargo::core::compiler::fingerprint: fingerprint dirty for ${name} v0.1.0 (/w/${name})/Check { test: false }/TargetInner { .. }\n INFO cargo::core::compiler::fingerprint:     dirty: FsStatusOutdated { stale: x }\n`;
  for (const word of ["Compiling", "Checking", "Documenting", "Fresh", "Dirty", "Running", "Finished", "Scraping"]) {
    const log = new census.FingerprintLog({ strip: true });
    const shown = log.feed(`${dirty("root")}   ${word} thing\n${dirty("nested")}`);
    assert.equal(log.dirtyUnits, 1, `${word}: only the lines before the first status word are the root's`);
    assert.deepEqual(log.reasons.map((r) => r.unit), ["root v0.1.0"], word);
    assert.ok(shown.includes("fingerprint dirty for nested") && !shown.includes("fingerprint dirty for root"), `${word}: nested lines are left in the output`);
  }
  for (const word of ["Updating", "Locking", "Downloading", "Downloaded", "Blocking", "warning:"]) {
    const log = new census.FingerprintLog({ strip: true });
    log.feed(`   ${word} something\n${dirty("late")}`);
    assert.equal(log.dirtyUnits, 1, `${word} does not end the root window`);
  }
  // A cause chain that does not follow the heading, or follows another reason, never rewrites an unrelated reason.
  const log = new census.FingerprintLog({ strip: true });
  log.feed(" INFO cargo::core::compiler::fingerprint: fingerprint error for x v0.1.0 (/w/x)/Build/TargetInner { .. }\n INFO cargo::core::compiler::fingerprint:     err: failed to read `/t/debug/.fingerprint/x-1/lib-x`\nCaused by:\n    Permission denied (os error 13)\n");
  assert.deepEqual(log.reasons.map((r) => [r.kind, r.cause]), [["failed", "unexplained"]]);
  const other = new census.FingerprintLog({ strip: true });
  other.feed(" INFO cargo::core::compiler::fingerprint: fingerprint error for y v0.1.0 (/w/y)/Build/TargetInner { .. }\n INFO cargo::core::compiler::fingerprint:     err: failed to parse manifest `/w/y/Cargo.toml`\nCaused by:\n    No such file or directory (os error 2)\n");
  assert.deepEqual(other.reasons.map((r) => [r.kind, r.cause]), [["failed", "unexplained"]], "only a `.fingerprint` read with a not-found cause is a missing fingerprint");
});

test("[GB-68] the selected scope is the selected profile's directory (plus the selected triple's), never every profile or triple, never doc or package", (t) => {
  assert.equal(vector.scope.length, 4);
  for (const c of vector.scope) {
    const root = tmp(t);
    layOut(root, c.files);
    const selected = census.selectedFingerprintDirectories(root, { argv: c.argv, triples: c.triple ? [c.triple] : [] });
    assert.deepEqual(selected.map((d) => posix(root, d)), c.directories, c.name);
    const counter = census.createFreshnessCensus({ targetDirs: { ".": root }, now: () => 0 });
    counter.start({ argv: c.argv, triples: c.triple ? [c.triple] : [] });
    const scope = counter.scope();
    assert.equal(scope.unitsAtStart, c.unitsAtStart, c.name);
    assert.equal(scope.readError, null, c.name);
    assert.equal(scope.capped, false, c.name);
    assert.deepEqual(scope.directories.map((d) => posix(root, d)), c.directories, c.name);
  }
});

test("[GB-68] every census vector has RustRight's outcome: counts by the start time, an empty profile, a cap overflow and an unreadable directory are unavailable, never a census of zero or a first build", (t) => {
  assert.equal(vector.census.length, 5);
  for (const c of vector.census) {
    const root = tmp(t);
    layOut(root, c.files);
    const since = c.since === "future" ? 4e18 : 0;
    const counter = census.createFreshnessCensus({ targetDirs: { ".": root }, now: () => since, cap: c.cap ?? 1_000_000 });
    counter.start({ argv: c.argv });
    const got = counter.finish();
    if (c.expect.outcome === "available") {
      assert.equal(got.available, true, `${c.name}: ${JSON.stringify(got)}`);
      assert.deepEqual([got.fresh, got.notFresh], [c.expect.fresh, c.expect.notFresh], c.name);
    } else {
      assert.equal(got.available, false, `${c.name}: ${JSON.stringify(got)}`);
      assert.equal(got.reason, c.expect.reason, c.name);
      assert.deepEqual([got.fresh, got.notFresh, got.unitsAtStart], [0, 0, 0], `${c.name}: an unavailable census reports no counts`);
    }
  }
});

test("[GB-68] negative: a fingerprint directory or unit that exists and cannot be read is unavailable at the start and in the walk (a fake read error), never empty; only an absent directory is empty", (t) => {
  const root = tmp(t);
  layOut(root, ["debug/.fingerprint/a-1/x"]);
  const denied = (target) => Object.assign(new Error("EACCES: permission denied"), { code: "EACCES", path: target });
  const failing = (match) => ({ ...fs, readdirSync: (p, ...rest) => { if (match(String(p))) throw denied(p); return fs.readdirSync(p, ...rest); } });
  // The fingerprint directory itself cannot be read: at the start.
  const atStart = census.createFreshnessCensus({ targetDirs: { ".": root }, now: () => 0, fsApi: failing((p) => p.endsWith(`${path.sep}.fingerprint`)) });
  atStart.start({ argv: ["build"] });
  assert.equal(atStart.scope().readError !== null, true, "the start records the read error");
  assert.deepEqual([atStart.finish().available, atStart.finish().reason], [false, "fingerprint-census-read-failed"]);
  // The directory read at the start, then a unit that cannot be read in the walk.
  const inWalk = census.createFreshnessCensus({ targetDirs: { ".": root }, now: () => 0, fsApi: failing((p) => p.endsWith(`${path.sep}a-1`)) });
  inWalk.start({ argv: ["build"] });
  assert.deepEqual([inWalk.finish().available, inWalk.finish().reason], [false, "fingerprint-census-read-failed"]);
  // A unit that vanished (ENOENT) is not a read failure; an absent directory is an empty profile (no units found), not a read failure.
  const vanished = census.createFreshnessCensus({ targetDirs: { ".": root }, now: () => 0, fsApi: { ...fs, readdirSync: (p, ...rest) => { if (String(p).endsWith(`${path.sep}a-1`)) throw Object.assign(new Error("ENOENT"), { code: "ENOENT" }); return fs.readdirSync(p, ...rest); } } });
  vanished.start({ argv: ["build"] });
  assert.equal(vanished.finish().available, true, "a vanished unit holds no files");
  const absent = census.createFreshnessCensus({ targetDirs: { ".": root }, now: () => 0 });
  absent.start({ argv: ["build", "--release"] });
  assert.equal(absent.finish().reason, "no-fingerprint-units-found");
});

test("[GB-68] through the spawner the census selects what each compiling cargo process builds: a release build reads release only, a --target build adds its triple, other commands select nothing", async (t) => {
  const root = tmp(t);
  const old = (name) => { const dir = path.join(root, ...name.split("/")); fs.mkdirSync(dir, { recursive: true }); fs.writeFileSync(path.join(dir, "x"), "x"); fs.utimesSync(path.join(dir, "x"), 1_000, 1_000); };
  for (const unit of ["debug/.fingerprint/a-1", "debug/.fingerprint/b-1", "release/.fingerprint/c-1", "x86_64-unknown-linux-gnu/release/.fingerprint/d-1", "aarch64-apple-darwin/release/.fingerprint/e-1"]) old(unit);
  const run = async (argv, env = {}) => {
    const counter = census.createFreshnessCensus({ targetDirs: { ".": root }, now: () => 5_000_000 });
    counter.start();
    await counter.wrapSpawn(async () => ({ status: 0, stdout: "", stderr: "" }))("cargo", argv, { cwd: root, env: { PATH: "/bin", ...env } });
    return counter;
  };
  const release = await run(["build", "--release"]);
  assert.deepEqual(release.scope().directories.map((d) => posix(root, d)), ["release/.fingerprint"]);
  assert.deepEqual([release.finish().unitsAtStart, release.finish().fresh], [1, 1]);
  const triple = await run(["build", "--release", "--target", "x86_64-unknown-linux-gnu"]);
  assert.deepEqual(triple.scope().directories.map((d) => posix(root, d)), ["x86_64-unknown-linux-gnu/release/.fingerprint", "release/.fingerprint"]);
  assert.equal(triple.finish().unitsAtStart, 2);
  const fromEnv = await run(["build", "--release"], { CARGO_BUILD_TARGET: "aarch64-apple-darwin" });
  assert.deepEqual(fromEnv.scope().directories.map((d) => posix(root, d)), ["aarch64-apple-darwin/release/.fingerprint", "release/.fingerprint"], "CARGO_BUILD_TARGET selects the triple as the flag does");
  const none = await run(["metadata", "--no-deps"]);
  const nothing = none.finish();
  assert.deepEqual([nothing.available, nothing.reason], [false, "fingerprint-census-no-compiling-process"], "no compiling process: unavailable, never a census of everything and never green");
  // Two compiling processes of different profiles: the union of their scopes, each baseline taken before its own process.
  const both = createTwo(root);
  async function createTwo(dir) {
    const counter = census.createFreshnessCensus({ targetDirs: { ".": dir }, now: () => 5_000_000 });
    counter.start();
    const wrapped = counter.wrapSpawn(async () => ({ status: 0, stdout: "", stderr: "" }));
    await wrapped("cargo", ["build"], { cwd: dir, env: {} });
    await wrapped("cargo", ["build", "--release"], { cwd: dir, env: {} });
    return counter;
  }
  const union = (await both).finish();
  assert.deepEqual([union.available, union.unitsAtStart, union.fresh, union.notFresh], [true, 3, 3, 0]);
});

test("[GB-68] a build.target in the cargo configuration selects its triple as --target does (a compile's output sits under <target>/<triple>/<profile>), and one the command line names wins", async (t) => {
  const root = tmp(t);
  const layout = (rel) => { const dir = path.join(root, ...rel.split("/")); fs.mkdirSync(dir, { recursive: true }); fs.writeFileSync(path.join(dir, "x"), "x"); fs.utimesSync(path.join(dir, "x"), 1_000, 1_000); };
  layout("debug/.fingerprint/host-1"); layout("wasm32-wasip1/debug/.fingerprint/guest-1"); layout("wasm32-wasip1/debug/.fingerprint/guest-2");
  const work = path.join(root, "work", "app"); fs.mkdirSync(path.join(work, ".cargo"), { recursive: true });
  fs.writeFileSync(path.join(work, ".cargo", "config.toml"), '# a comment\n[net]\noffline = true\n[build]\njobs = 4\ntarget = "wasm32-wasip1" # the guest\n');
  assert.deepEqual(census.configuredBuildTargets(work), ["wasm32-wasip1"]);
  assert.deepEqual(census.configuredBuildTargets(path.join(root, "debug")), [], "no configuration, no triple");
  const counter = census.createFreshnessCensus({ targetDirs: { ".": root }, now: () => 5_000_000 });
  counter.start();
  await counter.wrapSpawn(async () => ({ status: 0, stdout: "", stderr: "" }))("cargo", ["build"], { cwd: work, env: {} });
  assert.deepEqual(counter.scope().directories.map((d) => posix(root, d)), ["wasm32-wasip1/debug/.fingerprint", "debug/.fingerprint"]);
  assert.equal(counter.finish().unitsAtStart, 3, "the configured triple's units are counted, not missed");
  const flagged = census.createFreshnessCensus({ targetDirs: { ".": root }, now: () => 5_000_000 });
  flagged.start();
  await flagged.wrapSpawn(async () => ({ status: 0, stdout: "", stderr: "" }))("cargo", ["build", "--target", "x86_64-unknown-linux-gnu"], { cwd: work, env: {} });
  assert.deepEqual(flagged.scope().directories.map((d) => posix(root, d)), ["x86_64-unknown-linux-gnu/debug/.fingerprint", "debug/.fingerprint"]);
});
