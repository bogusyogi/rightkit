// Item 7, the direct (Linux) half of the snapshot compile (rows 7.09, 7.12, 7.18, 7.19, 7.30; GATE-CONTRACT "Output directory & cache",
// "Returned evidence", "Linux"; E3, E16):
//   7.09  the output directory is resolved in the primary checkout with the stage's environment, before the source location changes,
//         and the inputs of that resolution are recorded;
//   7.12  no second output directory: a cargo that bypasses the wrapper and writes inside the snapshot fails the after-compile verify,
//         and through the wrapper a redirect is refused (RK_CARGO_REDIRECT);
//   7.18  the direct receipt carries the contract's names minus the broker-only `admission`, `queue` and `reusedFrom`;
//   7.19  the freshness census from Cargo's fingerprint log: the second build of one commit shows reuse, a one-file edit shows that
//         crate and its dependents only, each with a reason;
//   7.30  Linux always runs direct (RustRight absent or first on PATH); a managed Mac or Dell with no broker refuses before compile.
// Pure and fixture tests run everywhere; tests that need a real cargo run only on Linux with one on PATH (WSL2 counts), never as a
// simulated platform (GATE-CONTRACT: a fixture that injects `platform: "linux"` is fixture evidence, not a native Linux row).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { BROKER_ONLY_FIELDS, DIRECT_RECEIPT_REQUIRED, TIMING_FIELDS, buildDirectReceipt, directVerdict, envInputs, isWithin, resolveTargetDirectory, verifyDirectReceipt, writeDirectDiagnostics } from "./direct-receipt.mjs";
import { FINGERPRINT_LOG_FILTER, FingerprintLog, REASONS_CAP, REASON_TEXT_CAP, compilesWorkspace, createFreshnessCensus, selectedFingerprintDirectories, validateFreshness } from "./freshness-census.mjs";
import { main } from "./bin/rightkit-gate.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const POSIX = process.platform === "win32" ? "POSIX scripts: a Windows child finds only an .exe on PATH" : false;
// Linux with a real cargo on PATH (never a RustRight shim): the direct host these rows exist for.
const REAL_CARGO = (() => {
  if (process.platform !== "linux") return null;
  const r = spawnSync("sh", ["-c", "command -v cargo"], { encoding: "utf8" });
  const at = String(r.stdout ?? "").trim();
  return r.status === 0 && at && !at.includes(".rightkit-managed") ? at : null;
})();
const NEEDS_REAL = REAL_CARGO ? false : "needs Linux with a real cargo on PATH (native WSL2 counts); a simulated platform is not evidence";

function tmp(t, prefix = "rk-item7-linux-") {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}
function write(root, files) {
  for (const [rel, body] of Object.entries(files)) {
    const file = path.join(root, ...rel.split("/"));
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, body);
  }
}
const gitIn = (cwd) => (...a) => spawnSync("git", a, { cwd, encoding: "utf8" });
function commitAll(repo, message) {
  const g = gitIn(repo);
  g("add", "-A");
  g("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", message);
  return g("rev-parse", "HEAD").stdout.trim();
}

// ---- 7.19: Cargo's fingerprint log --------------------------------------------------------------------------------------------------

const DIRTY = "  0.123s  INFO prepare_target{force=false package_id=core v0.1.0 (path+file:///snap/crates/core) target=\"core\"}: cargo::core::compiler::fingerprint: fingerprint dirty for core v0.1.0 (/snap/crates/core)/Check { test: false }/TargetInner { name: \"core\", .. }\n";
const REASON = "  0.123s  INFO prepare_target{force=false package_id=core v0.1.0 (path+file:///snap/crates/core) target=\"core\"}: cargo::core::compiler::fingerprint:     dirty: FsStatusOutdated(StaleItem(ChangedFile { reference: \"dep-lib\", stale: \"src/lib.rs\" }))\n";

test("[behavior] item 7 row 7.19: the fingerprint filter is the one the broker sets, and a dirty unit with its reason is recorded and kept out of the output", () => {
  assert.equal(FINGERPRINT_LOG_FILTER, "cargo::core::compiler::fingerprint=info");
  const log = new FingerprintLog({ strip: true });
  // The root's fingerprint lines come before Cargo's first execution status word (GB-68); what follows is a nested Cargo's and is neither counted nor stripped.
  const shown = log.feed(`${DIRTY}${REASON}   Compiling core v0.1.0\n    Finished dev\n`);
  assert.equal(shown, "   Compiling core v0.1.0\n    Finished dev\n");
  assert.equal(log.dirtyUnits, 1);
  assert.equal(log.reasons.length, 1);
  assert.deepEqual([log.reasons[0].unit, log.reasons[0].target, log.reasons[0].kind, log.reasons[0].cause], ["core v0.1.0", "core", "FsStatusOutdated", "source-or-dependency-changed"]);
  assert.match(log.reasons[0].reason, /src\/lib\.rs/);
});

test("[behavior] item 7 row 7.19: lines split across chunks are judged whole, a held ordinary line reaches the caller at the end, a caller's own CARGO_LOG is read but left visible", () => {
  const log = new FingerprintLog({ strip: true });
  const head = DIRTY.slice(0, 40); const tail = DIRTY.slice(40);
  assert.equal(log.feed(head), "", "a partial line is held");
  assert.equal(log.feed(tail), "");
  assert.equal(log.feed("warning: unused\r\n"), "warning: unused\r\n");
  assert.equal(log.feed("no newline yet"), "");
  assert.equal(log.finish(), "no newline yet");
  assert.equal(log.dirtyUnits, 1);
  const visible = new FingerprintLog({ strip: false });
  const chunk = `${DIRTY}${REASON}partial`;
  assert.equal(visible.feed(chunk), chunk, "not stripping: the chunk is returned whole");
  assert.equal(visible.finish(), "");
  assert.equal(visible.reasons.length, 1, "and still read");
});

test("[behavior] item 7 row 7.19: spanless lines, the error form and colour are understood; reasons are capped and counted and long ones cut", () => {
  const log = new FingerprintLog({ strip: true });
  log.feed("\u001b[32m INFO\u001b[0m cargo::core::compiler::fingerprint: fingerprint error for serde v1.0.0 (registry+https://x)/Build/TargetInner { .. }\n");
  log.feed(" INFO cargo::core::compiler::fingerprint:     err: failed to read `fingerprint`\n");
  assert.deepEqual([log.reasons[0].unit, log.reasons[0].target, log.reasons[0].kind], ["serde v1.0.0", null, "failed"]);
  const many = new FingerprintLog({ strip: true });
  for (let i = 0; i < REASONS_CAP + 5; i += 1) {
    many.feed(DIRTY);
    many.feed(` INFO cargo::core::compiler::fingerprint:     dirty: RustflagsChanged { old: [${"x".repeat(2 * REASON_TEXT_CAP)}] }\n`);
  }
  assert.equal(many.reasons.length, REASONS_CAP);
  assert.equal(many.truncated, 5);
  assert.equal(many.dirtyUnits, REASONS_CAP + 5);
  assert.ok(Array.from(many.reasons[0].reason).length <= REASON_TEXT_CAP + 1);
});

test("[behavior] item 7 row 7.19: only a cargo that compiles a workspace gets the fingerprint log", () => {
  for (const [command, args] of [["cargo", ["build", "--locked"]], ["/x/bin/cargo", ["+stable", "clippy", "-p", "a"]], ["cargo", ["--color", "always", "test"]], ["cargo.exe", ["check"]], ["cargo", ["doc", "--no-deps"]]]) assert.equal(compilesWorkspace(command, args), true, `${command} ${args.join(" ")}`);
  for (const [command, args] of [["cargo", ["metadata", "--no-deps"]], ["cargo", ["locate-project"]], ["cargo", ["fmt", "--check"]], ["cargo", ["--version"]], ["cargo", []], ["node", ["build"]], ["rustright", ["cargo", "build"]], ["cargo", ["--", "build"]], ["cargo", ["package", "--list"]]]) assert.equal(compilesWorkspace(command, args), false, `${command} ${args.join(" ")}`);
});

// A target directory as cargo leaves it: <profile>/.fingerprint/<unit>/<files>.
const FP_NOW_S = 1_000_000_000; // the census clock below is this instant, in ms
function unitAt(target, profileDir, name, seconds) {
  const dir = path.join(target, ...profileDir.split("/"), ".fingerprint", name);
  fs.mkdirSync(dir, { recursive: true });
  const file = path.join(dir, "lib-x.json");
  fs.writeFileSync(file, "{}");
  for (const p of [file, dir]) fs.utimesSync(p, seconds, seconds);
}

test("[behavior] item 7 row 7.19: the census counts units at start, fresh and rebuilt (a new unit included) from the SELECTED scope's fingerprints (the build's profile and triple); other profiles, doc output and packaging are not counted", (t) => {
  const target = path.join(tmp(t), "target");
  unitAt(target, "debug", "a-1", FP_NOW_S - 100);
  unitAt(target, "debug", "b-1", FP_NOW_S - 100);
  unitAt(target, "x86_64-unknown-linux-gnu/debug", "c-1", FP_NOW_S - 100);
  unitAt(target, "doc", "ignored-1", FP_NOW_S - 100);
  unitAt(target, "package", "ignored-2", FP_NOW_S - 100);
  assert.deepEqual(selectedFingerprintDirectories(target, { argv: ["build"], triples: ["x86_64-unknown-linux-gnu"] }).map((d) => path.relative(target, d).split(path.sep).join("/")), ["x86_64-unknown-linux-gnu/debug/.fingerprint", "debug/.fingerprint"]);
  const census = createFreshnessCensus({ targetDirs: { ".": target }, now: () => FP_NOW_S * 1000 });
  census.start({ argv: ["build"], triples: ["x86_64-unknown-linux-gnu"] });
  fs.utimesSync(path.join(target, "debug", ".fingerprint", "b-1", "lib-x.json"), FP_NOW_S + 10, FP_NOW_S + 10); // b was rebuilt
  unitAt(target, "debug", "d-1", FP_NOW_S + 20); // d is new
  const freshness = census.finish();
  assert.deepEqual([freshness.unitsAtStart, freshness.fresh, freshness.notFresh, freshness.available], [3, 2, 2, true]);
  assert.deepEqual(validateFreshness(freshness), []);
  assert.equal(freshness.source, "cargo-fingerprint-log+fingerprint-mtime");
});

test("[behavior] item 7 row 7.19: a census with nothing to count, never started or over the entry cap says so instead of reporting zeros as reuse", (t) => {
  const dir = tmp(t);
  const empty = createFreshnessCensus({ targetDirs: { ".": path.join(dir, "no-target") }, now: () => FP_NOW_S * 1000 });
  empty.start({ argv: ["build"] });
  assert.deepEqual([empty.finish().available, empty.finish().reason], [false, "no-fingerprint-units-found"]);
  const unselected = createFreshnessCensus({ targetDirs: { ".": path.join(dir, "no-target") }, now: () => FP_NOW_S * 1000 });
  unselected.start();
  assert.deepEqual([unselected.finish().available, unselected.finish().reason], [false, "fingerprint-census-no-compiling-process"], "no compiling process selected a scope: unavailable, not a census of everything");
  assert.equal(createFreshnessCensus({ targetDirs: {} }).finish().reason, "fingerprint-census-not-started");
  const target = path.join(dir, "target");
  for (const n of ["a-1", "b-1", "c-1"]) unitAt(target, "debug", n, FP_NOW_S - 100);
  const capped = createFreshnessCensus({ targetDirs: { ".": target }, now: () => FP_NOW_S * 1000, cap: 1 });
  capped.start({ argv: ["build"] });
  const f = capped.finish();
  assert.deepEqual([f.available, f.reason, f.fresh, f.notFresh], [false, "fingerprint-census-entry-cap-exceeded", 0, 0]);
  assert.deepEqual(validateFreshness(f), [], "unavailable is still a well formed census: the consumer reads available:false");
});

test("[behavior] item 7 row 7.19: the fingerprint log is turned on for the compiling cargo process only; a caller's own CARGO_LOG stands and stays visible; other commands are untouched", async () => {
  const seen = [];
  const fake = (stderr, extra = {}) => async (command, args, options) => { seen.push({ command, args, env: options.env }); return { status: 0, stdout: "", stderr, ...extra }; };
  const ours = createFreshnessCensus({ targetDirs: {} });
  const r = await ours.wrapSpawn(fake(`warning: x\n${DIRTY}${REASON}`))("cargo", ["build", "--locked"], { env: { PATH: "/bin" } });
  assert.equal(seen[0].env.CARGO_LOG, FINGERPRINT_LOG_FILTER, "turned on for this process");
  assert.equal(seen[0].env.PATH, "/bin", "the rest of the environment is the scan's own");
  assert.equal(r.stderr, "warning: x\n", "its own log lines are kept out of the output");
  assert.equal(ours.finish().invalidationReasons.length, 1);
  assert.equal(ours.finish().cargoProcesses, 1);

  const theirs = createFreshnessCensus({ targetDirs: {} });
  const own = `${DIRTY}${REASON}`;
  const v = await theirs.wrapSpawn(fake(own))("cargo", ["check"], { env: { CARGO_LOG: "info" } });
  assert.equal(seen[1].env.CARGO_LOG, "info", "a caller's own CARGO_LOG is respected");
  assert.equal(v.stderr, own, "and left visible");
  assert.equal(theirs.finish().invalidationReasons.length, 1, "while still being read");

  const other = createFreshnessCensus({ targetDirs: {} });
  await other.wrapSpawn(fake(""))("cargo", ["metadata", "--no-deps"], { env: { PATH: "/bin" } });
  await other.wrapSpawn(fake(""))("node", ["-e", "0"], { env: { PATH: "/bin" } });
  assert.equal(Object.hasOwn(seen[2].env, "CARGO_LOG") || Object.hasOwn(seen[3].env, "CARGO_LOG"), false, "metadata and non-cargo commands get no CARGO_LOG");
  assert.equal(other.finish().cargoProcesses, 0);

  const cut = createFreshnessCensus({ targetDirs: {} });
  await cut.wrapSpawn(fake("", { truncated: true }))("cargo", ["build"], { env: {} });
  assert.equal(cut.finish().outputCut, true, "output cut to the kept tail is recorded, not hidden");
});

test("[behavior] item 7 row 7.19: freshness is refused by the consumer when it is absent or malformed", () => {
  const good = { unitsAtStart: 3, fresh: 2, notFresh: 1, invalidationReasons: [{ unit: "a v0.1.0", target: "a", kind: "FsStatusOutdated", reason: "x" }], available: true };
  assert.deepEqual(validateFreshness(good), []);
  assert.match(validateFreshness(undefined).join(), /absent/);
  assert.match(validateFreshness(null).join(), /absent/);
  for (const [label, bad] of [["no notFresh", { ...good, notFresh: undefined }], ["a negative count", { ...good, fresh: -1 }], ["a fraction", { ...good, unitsAtStart: 1.5 }], ["reasons not a list", { ...good, invalidationReasons: "x" }], ["a reason without a kind", { ...good, invalidationReasons: [{ unit: "a", reason: "x" }] }], ["unavailable without a reason", { ...good, available: false }]]) assert.notEqual(validateFreshness(bad).length, 0, label);
});

// ---- 7.09: the output directory is resolved in the primary checkout ----------------------------------------------------------------

test("[behavior] item 7 row 7.09: the output directory is resolved by cargo metadata in the PRIMARY CHECKOUT with the stage's environment, and the cwd and environment used are recorded", (t) => {
  const dir = tmp(t);
  const checkout = path.join(dir, "checkout"); const snap = path.join(dir, "snap"); const elsewhere = path.join(dir, "elsewhere", "target");
  fs.mkdirSync(checkout, { recursive: true }); fs.mkdirSync(snap, { recursive: true });
  const calls = [];
  const spawn = (command, args, options) => { calls.push({ command, args, options }); return { status: 0, stdout: JSON.stringify({ target_directory: elsewhere }), stderr: "" }; };
  const env = { PATH: "/bin", CARGO_TARGET_DIR: elsewhere, SECRET_TOKEN: "do-not-record" };
  const manifest = path.join(checkout, "Cargo.toml");
  const r = resolveTargetDirectory({ spawn, cargoCommand: ["cargo"], manifest, env, checkoutRoot: checkout, snapshotRoot: snap, workspace: "." });
  assert.equal(r.dir, path.resolve(elsewhere), "an absolute build.target-dir or CARGO_TARGET_DIR resolves to that directory");
  assert.equal(calls.length, 1);
  assert.equal(calls[0].options.cwd, checkout, "run in the checkout, never in the snapshot");
  assert.equal(calls[0].options.env, env, "with the stage's own environment");
  assert.deepEqual(calls[0].args, ["metadata", "--offline", "--format-version", "1", "--no-deps", "--manifest-path", manifest]);
  assert.equal(r.inputs.cwd, checkout);
  assert.deepEqual(r.inputs.command, ["cargo", ...calls[0].args]);
  assert.equal(r.inputs.env.CARGO_TARGET_DIR, elsewhere);
  assert.match(r.inputs.env.digest, /^[0-9a-f]{64}$/);
  assert.equal(JSON.stringify(r.inputs).includes("do-not-record"), false, "the rest of the environment is recorded by digest only");
  assert.notEqual(envInputs({ ...env, SECRET_TOKEN: "other" }).digest, r.inputs.env.digest, "a changed environment is a changed digest");
  assert.equal(resolveTargetDirectory({ spawn, cargoCommand: ["rustright", "cargo"], manifest, env, checkoutRoot: checkout }).inputs.command[1], "cargo", "a broker-style cargo command keeps its words");
});

test("[behavior] item 7 row 7.09: a resolution run in the snapshot, with no absolute directory, or landing inside the snapshot, is refused before anything compiles", (t) => {
  const dir = tmp(t);
  const checkout = path.join(dir, "checkout"); const snap = path.join(dir, "snap");
  fs.mkdirSync(checkout, { recursive: true }); fs.mkdirSync(path.join(snap, "crates"), { recursive: true });
  const reply = (target) => () => ({ status: 0, stdout: JSON.stringify({ target_directory: target }), stderr: "" });
  const refuses = (options, message) => assert.throws(() => resolveTargetDirectory({ cargoCommand: ["cargo"], env: {}, workspace: "crates", ...options }), (e) => e.code === "RK_TARGET_RESOLUTION" && message.test(e.message), String(message));
  // The resolution test double that would run in the snapshot: its cwd is the snapshot's.
  refuses({ spawn: reply(path.join(checkout, "target")), manifest: path.join(snap, "crates", "Cargo.toml"), checkoutRoot: checkout, snapshotRoot: snap }, /must be resolved in the primary checkout/);
  refuses({ spawn: reply(path.join(checkout, "target")), manifest: path.join(checkout, "Cargo.toml"), checkoutRoot: checkout, snapshotRoot: checkout }, /before the source location changes, not in the snapshot/);
  refuses({ spawn: reply(path.join(snap, "target")), manifest: path.join(checkout, "Cargo.toml"), checkoutRoot: checkout, snapshotRoot: snap }, /inside the snapshot/);
  refuses({ spawn: reply("target"), manifest: path.join(checkout, "Cargo.toml"), checkoutRoot: checkout, snapshotRoot: snap }, /no absolute target_directory for crates/);
  refuses({ spawn: () => ({ status: 101, stdout: "", stderr: "error: could not find `Cargo.toml`\n" }), manifest: path.join(checkout, "Cargo.toml"), checkoutRoot: checkout, snapshotRoot: snap }, /no absolute target_directory for crates \(error: could not find/);
  refuses({ spawn: () => ({ status: 0, stdout: "not json", stderr: "" }), manifest: path.join(checkout, "Cargo.toml"), checkoutRoot: checkout, snapshotRoot: snap }, /no absolute target_directory/);
  assert.equal(isWithin(snap, path.join(snap, "target")), true);
  assert.equal(isWithin(snap, `${snap}-other`), false, "a sibling that shares a prefix is not inside");
  assert.equal(isWithin(snap, path.join(snap, "..", "x")), false);
});

// ---- 7.18: the receipt -----------------------------------------------------------------------------------------------------------------

const TREE = "b".repeat(40);
const goodFreshness = () => ({ unitsAtStart: 3, fresh: 2, notFresh: 1, invalidationReasons: [{ unit: "a v0.1.0", target: "a", kind: "FsStatusOutdated", reason: "x" }], available: true });
const goodReceipt = (over = {}) => buildDirectReceipt({
  requestId: "5d4f1bf7-0000-4000-8000-000000000000", issuedAt: new Date().toISOString(), commit: { sha: "a".repeat(40), tree: TREE },
  snapshotPath: path.resolve(path.sep, "snaps", "k"), treeAtStart: TREE, treeAtExit: TREE, ownershipHandle: "h-1", declaredInputDigests: [],
  verdict: directVerdict({ scans: [{ id: "x", class: "A", status: "pass" }, { id: "y", class: "B", status: "pass" }] }), freshness: goodFreshness(),
  timingsMs: { bind: 1, cargo: 2, afterExitBind: 3, diagnostics: 4 }, exit: 0, diagnosticsPath: path.resolve(path.sep, "d", "diagnostics.json"),
  outputDirs: { ".": path.resolve(path.sep, "checkout", "target") }, outputResolution: {}, ...over,
});

test("[behavior] item 7 row 7.18: the direct receipt carries every contract name with the right type and never admission, queue or reusedFrom", () => {
  const receipt = goodReceipt();
  assert.deepEqual(verifyDirectReceipt(receipt), []);
  assert.deepEqual(DIRECT_RECEIPT_REQUIRED.filter((name) => !Object.hasOwn(receipt, name)), []);
  assert.equal(typeof receipt.requestId, "string");
  assert.equal(receipt.brokerVersion, null);
  assert.equal(receipt.executor, "direct");
  assert.deepEqual(Object.keys(receipt.commit).sort(), ["sha", "tree"]);
  assert.deepEqual(Object.keys(receipt.snapshot).sort(), ["path", "treeAtExit", "treeAtStart"]);
  assert.ok(Array.isArray(receipt.declaredInputDigests));
  assert.equal(receipt.verdict.result, "PASSED");
  for (const k of ["unitsAtStart", "fresh", "notFresh"]) assert.equal(typeof receipt.freshness[k], "number", k);
  assert.ok(Array.isArray(receipt.freshness.invalidationReasons));
  assert.deepEqual(Object.keys(receipt.timingsMs).sort(), [...TIMING_FIELDS].sort());
  for (const k of TIMING_FIELDS) assert.equal(typeof receipt.timingsMs[k], "number", k);
  assert.equal(typeof receipt.exit, "number");
  assert.equal(typeof receipt.diagnosticsPath, "string");
  assert.equal(path.isAbsolute(receipt.outputDir), true);
  assert.deepEqual(receipt.refreshed, [], "nothing is reused on a direct host");
  for (const name of BROKER_ONLY_FIELDS) assert.equal(Object.hasOwn(receipt, name), false, name);
  assert.equal(directVerdict({ failures: ["x"] }).result, "FAILED");
});

test("[behavior] item 7 row 7.18: a receipt that drops any required name, carries a broker-only one, or holds a malformed census is refused", () => {
  for (const name of DIRECT_RECEIPT_REQUIRED) {
    const receipt = goodReceipt(); delete receipt[name];
    assert.ok(verifyDirectReceipt(receipt).some((p) => p.startsWith(`${name} is absent`)), `${name} dropped`);
  }
  for (const [name, value] of [["admission", {}], ["queue", {}], ["reusedFrom", "r"]]) assert.ok(verifyDirectReceipt({ ...goodReceipt(), [name]: value }).some((p) => p.includes(`${name} is a broker-only field`)), name);
  assert.ok(verifyDirectReceipt({ ...goodReceipt(), timingsMs: { bind: 1, cargo: 1, afterExitBind: 1, diagnostics: 1, admission: 1 } }).some((p) => /timingsMs\.admission/.test(p)));
  for (const k of TIMING_FIELDS) assert.ok(verifyDirectReceipt({ ...goodReceipt(), timingsMs: { ...goodReceipt().timingsMs, [k]: "1" } }).some((p) => p.includes(`timingsMs.${k}`)), k);
  for (const k of ["path", "treeAtStart", "treeAtExit"]) { const r = goodReceipt(); r.snapshot[k] = ""; assert.notEqual(verifyDirectReceipt(r).length, 0, `snapshot.${k}`); }
  const moved = goodReceipt(); moved.snapshot.treeAtExit = "c".repeat(40);
  assert.ok(verifyDirectReceipt(moved).some((p) => /not the commit's tree/.test(p)));
  assert.notEqual(verifyDirectReceipt(goodReceipt({ freshness: undefined })).length, 0, "census absent");
  assert.notEqual(verifyDirectReceipt(goodReceipt({ freshness: { unitsAtStart: 1, fresh: 1 } })).length, 0, "census malformed");
  assert.ok(verifyDirectReceipt({ ...goodReceipt(), exit: "0" }).some((p) => /exit is not/.test(p)));
  assert.ok(verifyDirectReceipt({ ...goodReceipt(), executor: "broker" }).some((p) => /executor is/.test(p)));
  assert.ok(verifyDirectReceipt({ ...goodReceipt(), brokerVersion: "1.0" }).some((p) => /brokerVersion/.test(p)));
  assert.ok(verifyDirectReceipt({ ...goodReceipt(), refreshed: "none" }).some((p) => /refreshed/.test(p)));
  assert.ok(verifyDirectReceipt({ ...goodReceipt(), outputDir: "target" }).some((p) => /outputDir is not absolute/.test(p)));
  assert.deepEqual(verifyDirectReceipt(goodReceipt({ outputDirs: {} })), [], "a commit with no Cargo workspace has no output directory");
  const two = goodReceipt({ outputDirs: { ".": path.resolve(path.sep, "c", "target"), native: path.resolve(path.sep, "c", "native", "target") } });
  assert.equal(two.outputDir, null);
  assert.deepEqual(verifyDirectReceipt(two), [], "several workspaces name several directories");
  assert.notEqual(verifyDirectReceipt({ ...goodReceipt(), schema: "other" }).length, 0);
  assert.deepEqual(verifyDirectReceipt(null), ["the receipt is not an object"]);
});

test("[behavior] item 7 row 7.18: the structured diagnostics are written as one file the receipt names", (t) => {
  const dir = tmp(t);
  const file = writeDirectDiagnostics({ dir: path.join(dir, "d"), requestId: "rid", commit: { sha: "a".repeat(40), tree: TREE }, exit: 1, scans: [{ id: "x", class: "B", status: "fail", code: "EXIT", detail: "boom".repeat(1000) }], failures: ["x"], deferrals: [], notes: ["n"], freshness: goodFreshness() });
  assert.equal(path.isAbsolute(file), true);
  const body = JSON.parse(fs.readFileSync(file, "utf8"));
  assert.equal(body.schema, "rightkit.gate-direct-diagnostics/1");
  assert.equal(body.scans[0].detail.length, 1000, "one scan's detail is cut");
  assert.equal(body.freshness.notFresh, 1);
});

// ---- the gate, end to end on a fixture (simulated platform: fixture evidence) ----------------------------------------------------------

test("[behavior] item 7 rows 7.09, 7.18, 7.19 (fixture): the door resolves the output directory in the checkout before the snapshot is used, and its receipt carries the matrix, the resolution inputs and a census over the checkout's fingerprints", { skip: POSIX }, async (t) => {
  const dir = tmp(t);
  const repo = path.join(dir, "repo"); const snaps = path.join(dir, "snaps");
  write(repo, { "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n", "README.md": "r\n" });
  const g = gitIn(repo); g("init", "-q", "-b", "main");
  const sha = commitAll(repo, "c");
  const repoReal = fs.realpathSync(repo);
  const checkoutTarget = path.join(repoReal, "target");
  // One unit already in the checkout's target (older than the stage): reused. The fake scan runner below "rebuilds" a second one.
  unitAt(checkoutTarget, "debug", "old-1", Math.floor(Date.now() / 1000) - 3600);
  const fakeCargo = path.join(dir, "toolchain", "cargo");
  const metadataCalls = [];
  const spawn = (c, a, opts) => {
    if (c === "git") return spawnSync(c, a, { ...opts, encoding: "utf8" });
    if (c === "which" && a[0] === "cargo") return { status: 0, stdout: `${fakeCargo}\n`, stderr: "" };
    if (c === "cargo" && a.includes("metadata")) {
      metadataCalls.push({ cwd: opts.cwd, args: a, env: opts.env });
      return { status: 0, stdout: JSON.stringify({ target_directory: checkoutTarget, workspace_root: opts.cwd, packages: [{ name: "a", manifest_path: path.join(opts.cwd, "crates", "a", "Cargo.toml") }] }), stderr: "" };
    }
    return { status: 1, stdout: "", stderr: "not in this fixture" };
  };
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir, RIGHTKIT_SCRATCH_ROOT: path.join(dir, "scratch"), CARGO_TARGET_DIR: undefined };
  delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE; delete env.CARGO_TARGET_DIR; delete env.CARGO_BUILD_TARGET_DIR; delete env.CARGO_LOG;
  const out = []; const seen = [];
  const code = await main(["--to", "compile", "--commit", sha], {
    cwd: repo, platform: "linux", snapshotRoot: snaps, env, spawn, out: (m) => out.push(m), err: (m) => out.push(m), appCheck: async () => 0, runCheck: async () => 0, readReceipt: () => null, writeReceipt: () => {},
    runScans: async (plan, options) => {
      seen.push({ spawnIsFunction: typeof options.spawn === "function" });
      // Not cargo: passed through untouched, with its stderr intact.
      const through = await options.spawn(process.execPath, ["-e", "console.error('through')"], { env: process.env });
      seen[0].through = through.stderr;
      // The census reads the scope a compiling cargo process selects (its profile, here the default `debug`): `cargo check --help` is such a process and compiles nothing.
      await options.spawn("cargo", ["check", "--help"], { env: process.env, cwd: repo });
      await new Promise((resolve) => setTimeout(resolve, 40)); // the file system's clock is coarser than Date.now()
      unitAt(checkoutTarget, "debug", "a-1", Math.floor(Date.now() / 1000) + 5); // a unit "rebuilt" during the stage
      return { scans: [], notes: [], cacheHits: [] };
    },
  });
  const text = out.join("\n");
  assert.equal(code, 0, text);
  assert.equal(seen[0].spawnIsFunction, true, "the scan runner is given the census's spawner");
  assert.match(seen[0].through, /through/);
  // 7.09: resolved in the checkout, with the stage's environment, never in the snapshot.
  const resolution = metadataCalls.filter((c) => c.args.includes("--offline"));
  assert.ok(resolution.length >= 1, "the output directory was resolved");
  for (const c of resolution) {
    assert.equal(isWithin(repoReal, c.cwd), true, `resolved in the checkout (${c.cwd})`);
    assert.equal(isWithin(snaps, c.cwd), false, `never in the snapshot (${c.cwd})`);
    assert.equal(c.env.RIGHTKIT_SCRATCH_ROOT, env.RIGHTKIT_SCRATCH_ROOT, "with the stage's environment");
  }
  const receiptLine = out.find((l) => l.startsWith("rightkit-gate: direct receipt "));
  assert.ok(receiptLine, text);
  const receipt = JSON.parse(receiptLine.slice("rightkit-gate: direct receipt ".length));
  assert.deepEqual(verifyDirectReceipt(receipt), []);
  assert.equal(receipt.outputDir, checkoutTarget);
  assert.equal(isWithin(receipt.snapshot.path, receipt.outputDir), false, "the output directory is outside the snapshot");
  const recorded = receipt.outputResolution["."];
  assert.equal(isWithin(repoReal, recorded.cwd), true, "the receipt records the cwd of the resolution");
  assert.equal(recorded.command.includes("metadata"), true);
  assert.match(recorded.env.digest, /^[0-9a-f]{64}$/);
  assert.equal(recorded.targetDirectory, checkoutTarget);
  // 7.19 over the checkout's own fingerprints: one reused (old-1), one rebuilt (a-1).
  assert.deepEqual([receipt.freshness.unitsAtStart, receipt.freshness.fresh, receipt.freshness.notFresh, receipt.freshness.available], [1, 1, 1, true], JSON.stringify(receipt.freshness));
  // 7.18
  for (const name of BROKER_ONLY_FIELDS) assert.equal(Object.hasOwn(receipt, name), false, name);
  assert.equal(receipt.executor, "direct"); assert.equal(receipt.brokerVersion, null); assert.equal(receipt.exit, 0);
  assert.deepEqual(receipt.refreshed, []);
  assert.deepEqual(receipt.declaredInputDigests, []);
  assert.equal(fs.existsSync(receipt.diagnosticsPath), true, "the structured diagnostics exist");
  assert.equal(isWithin(path.join(dir, "scratch"), receipt.diagnosticsPath), true, "on the host's scratch root, never in the repository");
  assert.equal(fs.existsSync(path.join(receipt.snapshot.path, "target")), false, "no target/ in the snapshot");
});

// ---- real cargo (Linux only) -------------------------------------------------------------------------------------------------------------

const crateFiles = (name, deps = []) => ({
  [`crates/${name}/Cargo.toml`]: `[package]\nname = "${name}"\nversion = "0.1.0"\nedition = "2021"\npublish = false\n${deps.length ? `\n[dependencies]\n${deps.map((d) => `${d} = { path = "../${d}" }`).join("\n")}\n` : ""}`,
  [`crates/${name}/src/lib.rs`]: `/// Adds one.\npub fn one(x: u32) -> u32 { x + 1 }\n${deps.map((d) => `pub use ${d}::one as from_${d};\n`).join("")}#[cfg(test)]\nmod tests { #[test] fn adds() { assert_eq!(super::one(1), 2); } }\n`,
});
const lockOf = (packages) => `# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n${packages.map(({ name, deps = [] }) => `[[package]]\nname = "${name}"\nversion = "0.1.0"\n${deps.length ? `dependencies = [\n${deps.map((d) => ` "${d}",`).join("\n")}\n]\n` : ""}`).join("\n")}`;
// A repository of path crates: { name, deps } in lock order (alphabetical), committed on main. The lockfile is written here because the
// test runner's broker guard refuses cargo in-process.
function cargoRepo(dir, crates, { config = null } = {}) {
  const repo = path.join(dir, "repo");
  fs.mkdirSync(repo, { recursive: true });
  const files = { "Cargo.toml": `[workspace]\nmembers = [${crates.map((c) => `"crates/${c.name}"`).join(", ")}]\nresolver = "2"\n`, "Cargo.lock": lockOf(crates), ...(config ? { ".cargo/config.toml": config } : {}) };
  for (const c of crates) Object.assign(files, crateFiles(c.name, c.deps));
  write(repo, files);
  gitIn(repo)("init", "-q", "-b", "main");
  return { repo, sha: commitAll(repo, "c") };
}

// The gate's own door run in a child process (the broker guard is off there), with the same real class B cargo scans the gate plans plus
// any extra scan rows, which run in the COMPILE stage only (the stage whose scans carry the runtime cargo wrapper: `options.cargoLedger`); the check
// stage also calls runScans (class A over its own snapshot, no wrapper, so no CARGO), and an extra row there judges the check, not the compile.
// `onlyExtra` runs the extra rows alone. Returns { code, out, receipt }.
function runDoor(dir, { repo, sha, extra = [], onlyExtra = false, forceCompile = false, env: envOver = {} }) {
  const driver = path.join(dir, `driver-${Math.random().toString(16).slice(2)}.mjs`);
  fs.writeFileSync(driver, `const { main } = await import(${JSON.stringify(pathToFileURL(path.join(HERE, "bin", "rightkit-gate.mjs")).href)});
const { runScans } = await import(${JSON.stringify(pathToFileURL(path.join(HERE, "deterministic-scans.mjs")).href)});
const out = [];
const keep = (s) => (s.class === "B" ? s.tool === "cargo" : s.internal || s.tool === "node" || (s.tool === "cargo" && s.args.includes("metadata")));
const extra = ${JSON.stringify(extra)};
const code = await main(["--to", "compile", "--commit", ${JSON.stringify(sha)}], { cwd: ${JSON.stringify(repo)}, platform: "linux", snapshotRoot: ${JSON.stringify(path.join(dir, "snaps"))},
  out: (m) => out.push(m), err: (m) => out.push(m), appCheck: async () => 0,${forceCompile ? " compileReuseProblem: () => \"the test measures the census of this run's own compile (GB-13 would otherwise reuse the recorded one)\"," : ""}
  runScans: (plan, options) => runScans({ ...plan, scans: [...(${onlyExtra ? "true" : "false"} ? [] : plan.scans.filter(keep)), ...(options.cargoLedger ? extra : [])] }, options) });
const line = out.find((l) => l.startsWith("rightkit-gate: direct receipt "));
const snapLine = out.map((l) => / snapshot (\\S+) is commit /.exec(l)).find(Boolean);
console.log(JSON.stringify({ code, out: out.join("\\n").slice(-30000), snapshot: snapLine ? snapLine[1] : null, receipt: line ? JSON.parse(line.slice("rightkit-gate: direct receipt ".length)) : null }));\n`);
  const env = { ...process.env, NODE_OPTIONS: "", RK_GATE_CACHE: "0", RIGHTKIT_SCRATCH_ROOT: path.join(dir, "scratch"), ...envOver };
  delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.CARGO_BUILD_TARGET_DIR; delete env.CARGO_LOG;
  if (!Object.hasOwn(envOver, "CARGO_TARGET_DIR")) delete env.CARGO_TARGET_DIR;
  const r = spawnSync(process.execPath, [driver], { env, encoding: "utf8", cwd: repo, timeout: 900_000, maxBuffer: 256 * 1024 * 1024 });
  try { return JSON.parse(String(r.stdout).trim().split(/\r?\n/).pop()); } catch { return { code: null, out: `${r.stdout}\n${r.stderr}`, receipt: null }; }
}
const snapshotOf = (res) => res.snapshot ?? null;
const libArtifacts = (target, name) => {
  const deps = path.join(target, "debug", "deps");
  return fs.existsSync(deps) ? fs.readdirSync(deps).filter((n) => new RegExp(`^lib${name}-.*\\.(rlib|rmeta)$`).test(n)) : [];
};

test("[behavior] item 7 row 7.09 real cargo: with no config, a relative build.target-dir, an absolute build.target-dir and CARGO_TARGET_DIR in the stage environment, every build writes to the checkout's resolved directory, the receipt names it, and no output directory exists under the snapshot", { skip: NEEDS_REAL }, (t) => {
  for (const variant of ["no config (default target/)", "relative build.target-dir", "absolute build.target-dir", "CARGO_TARGET_DIR in the stage environment"]) {
    const dir = tmp(t);
    const absolute = path.join(dir, "absolute-out"); const fromEnv = path.join(dir, "env-out");
    const config = variant === "relative build.target-dir" ? '[build]\ntarget-dir = "out"\n' : variant === "absolute build.target-dir" ? `[build]\ntarget-dir = ${JSON.stringify(absolute)}\n` : null;
    const { repo, sha } = cargoRepo(dir, [{ name: "core" }], { config });
    const expected = { "no config (default target/)": path.join(repo, "target"), "relative build.target-dir": path.join(repo, "out"), "absolute build.target-dir": absolute, "CARGO_TARGET_DIR in the stage environment": fromEnv }[variant];
    const res = runDoor(dir, { repo, sha, env: variant === "CARGO_TARGET_DIR in the stage environment" ? { CARGO_TARGET_DIR: fromEnv } : {} });
    assert.equal(res.code, 0, `${variant}: ${res.out.slice(-4000)}`);
    assert.ok(libArtifacts(expected, "core").length > 0, `${variant}: the build wrote the crate's outputs to ${expected}`);
    const snap = snapshotOf(res);
    assert.ok(snap, `${variant}: the door synced a snapshot`);
    for (const stray of ["target", "out", "absolute-out", "env-out"]) assert.equal(fs.existsSync(path.join(snap, stray)), false, `${variant}: no ${stray}/ under the snapshot`);
    for (const other of ["target", "out"]) if (path.join(repo, other) !== expected) assert.equal(fs.existsSync(path.join(repo, other)), false, `${variant}: nothing in the checkout's ${other}/`);
    assert.ok(res.receipt, `${variant}: the compile printed its direct receipt`);
    assert.equal(res.receipt.outputDir, expected, `${variant}: the receipt names the resolved absolute directory`);
    const recorded = res.receipt.outputResolution["."];
    assert.equal(recorded.cwd, repo, `${variant}: resolved with the checkout as the working directory`);
    assert.equal(recorded.env.CARGO_TARGET_DIR, variant === "CARGO_TARGET_DIR in the stage environment" ? fromEnv : null, `${variant}: the environment used is recorded`);
    assert.equal(recorded.targetDirectory, expected);
    assert.equal(isWithin(snap, recorded.cwd), false);
  }
});

test("[behavior] item 7 rows 7.18 and 7.19 real cargo: the receipt of a real direct compile passes the field matrix, the second build of the same commit shows reuse, and a one-file committed edit rebuilds that crate and its dependents only, each with a reason", { skip: NEEDS_REAL }, (t) => {
  const dir = tmp(t);
  const crates = [{ name: "a" }, { name: "b", deps: ["a"] }, { name: "c" }];
  const { repo, sha } = cargoRepo(dir, crates);
  const first = runDoor(dir, { repo, sha });
  assert.equal(first.code, 0, first.out.slice(-4000));
  assert.deepEqual(verifyDirectReceipt(first.receipt), [], "the field matrix");
  const r1 = first.receipt;
  assert.equal(r1.executor, "direct"); assert.equal(r1.brokerVersion, null); assert.equal(r1.exit, 0); assert.deepEqual(r1.refreshed, []);
  for (const name of BROKER_ONLY_FIELDS) assert.equal(Object.hasOwn(r1, name), false, name);
  for (const k of TIMING_FIELDS) assert.ok(Number.isFinite(r1.timingsMs[k]) && r1.timingsMs[k] >= 0, k);
  assert.ok(fs.existsSync(r1.diagnosticsPath), "the diagnostics file exists");
  for (const name of DIRECT_RECEIPT_REQUIRED) { const bad = { ...r1 }; delete bad[name]; assert.ok(verifyDirectReceipt(bad).length > 0, `${name} dropped`); }
  assert.equal(r1.freshness.available, true, JSON.stringify(r1.freshness));
  assert.equal(r1.freshness.unitsAtStart, 0, "a cold target counts no unit at start");
  assert.ok(r1.freshness.notFresh > 0, "the first build rebuilds everything");
  assert.equal(path.isAbsolute(r1.outputDir), true);

  // The same commit again: reuse. notFresh is 0, or each rebuilt unit has a reason.
  const second = runDoor(dir, { repo, sha, forceCompile: true });
  assert.equal(second.code, 0, second.out.slice(-4000));
  const f2 = second.receipt.freshness;
  assert.ok(f2.unitsAtStart > 0 && f2.fresh > 0, `the second build sees the first build's units: ${JSON.stringify(f2)}`);
  // GATE-CONTRACT E16 / map row 7.19: notFresh is 0, or every rebuilt unit has a reason (a dirty line in Cargo's log).
  assert.ok(f2.notFresh === 0 || (f2.invalidationReasons.length > 0 && f2.dirtyUnits >= f2.notFresh), `the second build of the same commit rebuilt ${f2.notFresh} unit(s) without a reason for each: ${JSON.stringify(f2)}`);

  // One committed edit to crate a: a and its dependent b rebuild, each with a reason, and c does not.
  fs.appendFileSync(path.join(repo, "crates", "a", "src", "lib.rs"), "// edited\n");
  const sha2 = commitAll(repo, "edit a");
  const third = runDoor(dir, { repo, sha: sha2, forceCompile: true });
  assert.equal(third.code, 0, third.out.slice(-4000));
  const f3 = third.receipt.freshness;
  const names = new Set(f3.invalidationReasons.map((r) => r.unit.split(" ")[0]));
  assert.ok(f3.notFresh > 0, JSON.stringify(f3));
  assert.ok(names.has("a") && names.has("b"), `the edited crate and its dependent each carry a reason: ${[...names].join(", ")}`);
  assert.equal(names.has("c"), false, `the independent crate is not rebuilt: ${[...names].join(", ")}`);
  for (const r of f3.invalidationReasons) assert.ok(r.kind && r.reason, `a reason for ${r.unit}`);
});

test("[behavior] item 7 row 7.12 real cargo: a cargo that bypasses the wrapper and writes inside the snapshot (--target-dir, CARGO_TARGET_DIR or --config build.target-dir, cwd in the snapshot) fails the after-compile verify", { skip: NEEDS_REAL }, (t) => {
  const bypass = (args, env = {}) => ({
    id: `bypass:${args.join("_")}`, class: "A", tool: "node", root: "content", cwd: "crates/core", env: {}, command: "bypass",
    args: ["-e", `const { spawnSync } = require("node:child" + "_process"); const env = { ...process.env, ...${JSON.stringify(env)} }; delete env.CARGO_TARGET_DIR; delete env.CARGO_BUILD_TARGET_DIR; delete env.CARGO; Object.assign(env, ${JSON.stringify(env)}); const r = spawnSync(${JSON.stringify(REAL_CARGO)}, ${JSON.stringify(args)}, { stdio: "inherit", env }); process.exit(r.status ?? 1);`],
  });
  const variants = [
    ["--target-dir target", bypass(["build", "--locked", "--target-dir", "target"])],
    ["CARGO_TARGET_DIR=target", bypass(["build", "--locked"], { CARGO_TARGET_DIR: "target" })],
    ["--config build.target-dir", bypass(["build", "--locked", "--config", 'build.target-dir="target"'])],
  ];
  for (const [label, scan] of variants) {
    const dir = tmp(t);
    const { repo, sha } = cargoRepo(dir, [{ name: "core" }]);
    const res = runDoor(dir, { repo, sha, extra: [scan], onlyExtra: true });
    assert.equal(res.code, 2, `${label}: the stage fails the after-compile verify\n${res.out.slice(-3000)}`);
    assert.match(res.out, /RK_CHECK_ERROR/, label);
    assert.match(res.out, /the compile covers no known bytes/, label);
    assert.equal(res.receipt, null, `${label}: no receipt for a compile that covers no known bytes`);
  }
});

test("[behavior] item 7 row 7.12 real cargo: through the wrapper, each of --target-dir target, CARGO_TARGET_DIR=target and --config build.target-dir is refused as RK_CARGO_REDIRECT", { skip: NEEDS_REAL }, (t) => {
  const wrapped = (args, env = {}) => ({
    id: `wrapped:${args.join("_")}${Object.keys(env).join("")}`, class: "A", tool: "node", root: "content", cwd: "crates/core", env: {}, command: "wrapped",
    args: ["-e", `const { spawnSync } = require("node:child" + "_process"); const r = spawnSync(process.env.CARGO, ${JSON.stringify(args)}, { stdio: "inherit", env: { ...process.env, ...${JSON.stringify(env)} } }); process.exit(r.status ?? 1);`],
  });
  const variants = [
    ["--target-dir target", wrapped(["build", "--locked", "--target-dir", "target"])],
    ["--config build.target-dir", wrapped(["build", "--locked", "--config", 'build.target-dir="target"'])],
    ["CARGO_TARGET_DIR=target", wrapped(["build", "--locked"], { CARGO_TARGET_DIR: "target" })],
  ];
  for (const [label, scan] of variants) {
    const dir = tmp(t);
    const { repo, sha } = cargoRepo(dir, [{ name: "core" }]);
    const res = runDoor(dir, { repo, sha, extra: [scan], onlyExtra: true });
    assert.notEqual(res.code, 0, `${label}: refused\n${res.out.slice(-3000)}`);
    assert.match(res.out, /RK_CARGO_REDIRECT/, `${label}: ${res.out.slice(-3000)}`);
    const snap = snapshotOf(res);
    for (const stray of ["target", path.join("crates", "core", "target")]) assert.equal(fs.existsSync(path.join(snap, stray)), false, `${label}: nothing written under the snapshot (${stray})`);
  }
});

// ---- 7.30: Linux always direct; a managed host with no broker refuses --------------------------------------------------------------

test("[behavior] item 7 row 7.30 (native Linux): with RustRight absent and with a RustRight shim first on PATH the door runs the same direct compile; no broker, browser or Mac-path launch is attempted", { skip: NEEDS_REAL }, (t) => {
  for (const shimmed of [false, true]) {
    const dir = tmp(t);
    const { repo, sha } = cargoRepo(dir, [{ name: "core" }]);
    const trace = path.join(dir, "shim-trace.log");
    const shim = path.join(dir, "shim", "bin");
    // A RustRight shim first on PATH: `rustright` and a `cargo` that names it. Either one running is a broker launch, and says so in the trace.
    write(shim, {
      rustright: `#!/bin/sh\necho "rustright $*" >> '${trace}'\nexit 99\n`,
      cargo: `#!/bin/sh\n# RustRight managed shim (rustright cargo-guard.mjs)\necho "cargo-shim $*" >> '${trace}'\nexit 98\n`,
    });
    for (const name of ["rustright", "cargo"]) fs.chmodSync(path.join(shim, name), 0o755);
    const compile = { id: "direct-build", class: "A", tool: "node", root: "content", cwd: ".", env: {}, command: "direct-build",
      args: ["-e", 'const { spawnSync } = require("node:child" + "_process"); const r = spawnSync(process.env.CARGO, ["build", "--locked"], { stdio: "inherit" }); process.exit(r.status ?? 1);'] };
    const realBin = path.dirname(REAL_CARGO);
    const res = runDoor(dir, { repo, sha, extra: [compile], onlyExtra: true, env: { PATH: `${shimmed ? `${shim}${path.delimiter}` : ""}${realBin}${path.delimiter}${process.env.PATH}` } });
    const label = shimmed ? "RustRight shim first on PATH" : "RustRight absent";
    assert.equal(res.code, 0, `${label}: ${res.out.slice(-3000)}`);
    assert.match(res.out, /executor direct/, label);
    assert.ok(res.receipt, `${label}: a direct receipt`);
    assert.equal(res.receipt.executor, "direct", label);
    assert.equal(res.receipt.brokerVersion, null, label);
    assert.ok(libArtifacts(path.join(repo, "target"), "core").length > 0, `${label}: the compile ran, direct`);
    assert.equal(fs.existsSync(trace), false, `${label}: no broker or shim launch: ${fs.existsSync(trace) ? fs.readFileSync(trace, "utf8") : ""}`);
    const everything = JSON.stringify(res);
    for (const forbidden of ["/Volumes", "xdg-open", "chrome", "firefox", "msedge", "Applications"]) assert.equal(everything.includes(forbidden), false, `${label}: no ${forbidden} in the run`);
  }
});

test("[behavior] item 7 row 7.30 (native Mac and Dell): with the broker unreachable the door refuses before any compile, launches no cargo and never falls back to direct", { skip: process.platform === "linux" && "native on the Mac and the Dell; Linux always runs direct (the test above)" }, async (t) => {
  const dir = tmp(t);
  const repo = path.join(dir, "repo");
  write(repo, { "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n", "README.md": "r\n" });
  gitIn(repo)("init", "-q", "-b", "main");
  const sha = commitAll(repo, "c");
  const launches = []; const out = []; const err = [];
  // The host is managed (a broker socket is configured) and the broker cannot be reached: nothing answers `rustright` or `cargo`.
  const spawn = (command, args, options) => {
    launches.push([path.basename(String(command)).replace(/\.exe$/i, ""), ...args]);
    if (command === "git") return spawnSync(command, args, { ...options, encoding: "utf8" });
    return { status: 1, stdout: "", stderr: "not reachable" };
  };
  const code = await main(["--to", "compile", "--commit", sha], {
    cwd: repo, platform: process.platform, snapshotRoot: path.join(dir, "snaps"), env: { PATH: "", RIGHTKIT_BUILD_BROKER_SOCKET: path.join(dir, "no-broker.sock"), RIGHTKIT_DATA_ROOT: dir },
    exists: () => false, spawn, out: (m) => out.push(m), err: (m) => err.push(m), appCheck: async () => 0, runCheck: async () => 0, readReceipt: () => null, writeReceipt: () => {},
  });
  const text = `${out.join("\n")}\n${err.join("\n")}`;
  assert.notEqual(code, 0, text);
  assert.match(text, /BLOCKED/, text);
  assert.match(text, /broker|RustRight|carrier/i, text);
  assert.deepEqual(launches.filter((l) => l[0] === "cargo" || (l[0] === "rustright" && l[1] === "cargo")), [], "no cargo was launched");
  assert.doesNotMatch(text, /executor direct|direct receipt/, "no direct fallback");
});
