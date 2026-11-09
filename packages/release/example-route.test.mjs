// rightkit#1543: the example build row (`example:<crate>/<name>` in a --units-file): the planned argv, EXAMPLE_MISSING, and the path/sha256 recorded from cargo's own compiler-artifact record.
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { classAVerdict, writeClassAVerdict } from "./gate-seam.mjs";
import { publisherRequirementScan, resolveExampleArtifact, runScans } from "./deterministic-scans.mjs";
import { exampleRequirementsForRows } from "./local-publish.mjs";
import { parseExampleSelector, readUnitList } from "./bin/rightkit-gate.mjs";

const commit = "a".repeat(40); const tree = "b".repeat(40);
const tmp = () => fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), "rk-example-")));

function fixtureSnapshot() {
  const snap = tmp();
  fs.mkdirSync(path.join(snap, "ci"), { recursive: true }); fs.writeFileSync(path.join(snap, "ci/asset-gated.json"), JSON.stringify({ crates: {}, npm: {} }));
  fs.mkdirSync(path.join(snap, "crates/rightkit-x/examples"), { recursive: true });
  fs.writeFileSync(path.join(snap, "crates/rightkit-x/Cargo.toml"), '[package]\nname = "rightkit-x"\nversion = "0.2.0"\n\n[[example]]\nname = "kws"\npath = "examples/kws.rs"\n');
  return snap;
}
const rustUnits = [{ dir: "crates/rightkit-x", crate: "rightkit-x", name: "rightkit-x", cargoDir: "." }];
const rows = [{ selector: "example:rightkit-x/kws", dir: "crates/rightkit-x", example: { crate: "rightkit-x", name: "kws" } }];
const requirementOf = (snap) => exampleRequirementsForRows({ rows, snapshot: snap, rustUnits, env: { PATH: "/usr/bin" } })[0].requirements[0];
const scanOf = (req) => publisherRequirementScan(req, { platform: "darwin", nativeArch: "arm64", cargoCommand: ["rustright", "cargo"], cargoLiveRoot: "/snap" });

test("the unit-list entry parses, and the plan is ONE cargo build of the named example, with its JSON listing captured and no profile of its own", () => {
  assert.deepEqual(parseExampleSelector("example:rightkit-x/kws"), { crate: "rightkit-x", name: "kws" });
  assert.equal(parseExampleSelector("cargo:rightkit-x"), null);
  const dir = tmp(); const file = path.join(dir, "u.json");
  fs.writeFileSync(file, JSON.stringify({ units: ["example:rightkit-x/kws"] }));
  assert.deepEqual(readUnitList((f) => fs.readFileSync(f, "utf8"), file), ["example:rightkit-x/kws"]);
  fs.writeFileSync(file, JSON.stringify({ units: ["example:rightkit-x/../kws"] }));
  assert.throws(() => readUnitList((f) => fs.readFileSync(f, "utf8"), file), /RK_UNITS_FILE/);
  const req = requirementOf(fixtureSnapshot());
  assert.deepEqual(req.argv, ["build", "-p", "rightkit-x", "--example", "kws", "--locked", "--message-format=json"]);
  assert.equal(req.phase, "compile"); assert.equal(req.cwd, "crates/rightkit-x");
  const scan = scanOf(req);
  assert.equal(scan.tool, "rustright"); assert.equal(scan.root, "cargoLive"); assert.equal(scan.captureJson, true);
  assert.deepEqual(scan.example, { crate: "rightkit-x", name: "kws" });
  assert.equal(scan.args[0], "cargo"); assert.equal(scan.args[1], "build");
  for (const w of ["-p", "rightkit-x", "--example", "kws", "--locked", "--message-format=json"]) assert.ok(scan.args.includes(w), w);
  assert.ok(!scan.batchable && !scan.runOutside, "a build row is neither grouped nor run");
  const snap = fixtureSnapshot(); fs.rmSync(path.join(snap, "crates/rightkit-x/Cargo.toml"));
  fs.writeFileSync(path.join(snap, "crates/rightkit-x/Cargo.toml"), '[package]\nname = "rightkit-x"\nversion = "0.2.0"\n');
  assert.throws(() => requirementOf(snap), /declares no example kws/);
});

// A seam row through the real runScans: the fake client answers with a result line, a receipt built from what it was sent, and cargo's JSON on stdout.
function rig({ withRecord = true, withFile = true, fresh = false }) {
  const data = tmp(); const repoKey = "0123456789abcdef"; const snapshot = path.join(data, "snapshots", repoKey); const verdictDir = path.join(data, "class-a-verdicts");
  fs.mkdirSync(path.join(snapshot, "crates/rightkit-x"), { recursive: true });
  writeClassAVerdict(verdictDir, classAVerdict({ commit, tree, repoKey, workspace: ".", scans: [{ id: "a", status: "pass" }], scope: "full" }));
  const out = path.join(data, "out"); const exe = path.join(out, "aarch64-apple-darwin", "release", "examples", "kws"); const bytes = Buffer.from("binary-bytes");
  fs.mkdirSync(path.dirname(exe), { recursive: true }); if (withFile) fs.writeFileSync(exe, bytes);
  const listing = [...(withRecord ? [{ reason: "compiler-artifact", package_id: "path+file:///w/crates/rightkit-x#rightkit-x@0.2.0", target: { kind: ["example"], name: "kws" }, executable: exe, fresh }] : []), { reason: "build-finished", success: true }];
  const calls = [];
  const spawn = async (cmd, args, opts = {}) => {
    calls.push({ cmd, args: [...args] });
    if (args.includes("--version")) return { status: 0, stdout: "cargo 1.98.0 (test)\n", stderr: "" };
    const receipt = path.join(data, "receipt.json");
    fs.writeFileSync(receipt, JSON.stringify({ id: "q1", status: "SUCCEEDED", terminalAt: "t", gate: { commit, tree, snapshot, classA: opts.env?.RUSTRIGHT_CLASS_A, snapshotUnchanged: true }, provenance: { payload: { jobId: "q1", terminalAt: "t", request: { kind: "cargo", adapter: null, requestedArgv: args.slice(1), workspace: opts.cwd } } } }));
    return { status: 0, stdout: `${listing.map((l) => JSON.stringify(l)).join("\n")}\n`, stderr: `rustright-result ${JSON.stringify({ requestId: "q1", exit: 0, signal: null, outputDir: out, receipt, receiptPersisted: true })}\n` };
  };
  const gateSeam = { commit, tree, repoKey, snapshot, verdictDir, outputRoots: null, scope: null };
  const scan = { ...scanOf(requirementOf(fixtureSnapshot())) }; scan.example = { ...scan.example, commit, tree };
  const plan = { schema: "rightkit.deterministic-scans/1", roots: { contentRoot: snapshot, liveRoot: snapshot, cargoLiveRoot: snapshot }, scans: [scan], notes: [], deferred: [], qualifyOwed: [] };
  return { plan, calls, exe, bytes, out, store: path.join(data, "receipts", "examples"), opts: { classes: ["B"], classAEarlier: true, jobs: 1, classBJobs: 1, spawn, gateSeam, platform: process.platform, exampleStoreDir: path.join(data, "receipts", "examples") } };
}

test("a passed example row records cargo's own artifact path and the sha256 of that file, bound to commit, tree and receipt; the binary is never run", async () => {
  const r = rig({ fresh: true });
  const row = (await runScans(r.plan, r.opts)).scans[0];
  assert.equal(row.status, "pass", JSON.stringify(row));
  const sha = crypto.createHash("sha256").update(r.bytes).digest("hex");
  assert.deepEqual(row.example, { crate: "rightkit-x", version: "0.2.0", name: "kws", kind: ["example"], sha256: sha, fresh: true, path: path.join(r.store, sha, "kws"), builtFrom: r.exe, tests: "not-run", outputDir: r.out, commit, tree, receiptSha256: row.snapshotEvidence.receiptSha256, requestId: "q1" });
  assert.notEqual(row.example.path, r.exe, "the recorded path is the gate's copy, never cargo's mutable path");
  assert.equal(crypto.createHash("sha256").update(fs.readFileSync(row.example.path)).digest("hex"), sha, "the copy's bytes are the recorded sha256");
  assert.equal(fs.statSync(row.example.path).mode & 0o222, 0, "read-only after the copy");
  fs.writeFileSync(r.exe, "another build re-linked the cargo path"); // a later row or run replaces cargo's file: the recorded copy is unaffected
  assert.equal(crypto.createHash("sha256").update(fs.readFileSync(row.example.path)).digest("hex"), sha);
  assert.match(row.example.receiptSha256, /^[0-9a-f]{64}$/);
  assert.equal(r.calls.filter((c) => c.cmd === r.exe || c.args.includes(r.exe)).length, 0, "the example was not run");
  assert.equal(r.calls.filter((c) => !c.args.includes("--version")).length, 1, "one seam request");
});

test("EXAMPLE_MISSING: no compiler-artifact record for the example, or its file is absent", async () => {
  for (const [opts, why] of [[{ withRecord: false }, /no compiler-artifact record/], [{ withFile: false }, /not a readable file/]]) {
    const r = rig(opts);
    const row = (await runScans(r.plan, r.opts)).scans[0];
    assert.equal(row.status, "fail"); assert.equal(row.code, "EXAMPLE_MISSING", JSON.stringify(row)); assert.match(row.detail, why); assert.equal(row.example, undefined);
  }
});

test("no example store directory: EXAMPLE_MISSING, never cargo's path as the record", async () => {
  const r = rig({}); delete r.opts.exampleStoreDir;
  const row = (await runScans(r.plan, r.opts)).scans[0];
  assert.equal(row.status, "fail"); assert.equal(row.code, "EXAMPLE_MISSING"); assert.match(row.detail, /example store/); assert.equal(row.example, undefined);
});

test("resolveExampleArtifact refuses another crate's or another example's record, a cut listing and a failed build", () => {
  const d = tmp(); const exe = path.join(d, "kws"); fs.writeFileSync(exe, "x");
  const rec = (o) => JSON.stringify({ reason: "compiler-artifact", package_id: "path+file:///w#rightkit-x@0.2.0", target: { kind: ["example"], name: "kws" }, executable: exe, ...o });
  const fin = JSON.stringify({ reason: "build-finished", success: true });
  const call = (text, extra = {}) => resolveExampleArtifact({ listingText: text, crate: "rightkit-x", name: "kws", boundDirs: [d], ...extra });
  assert.equal(call(`${rec()}\n${fin}\n`).ok, true);
  assert.equal(call(`${rec({ package_id: "path+file:///w#rightkit-y@0.2.0" })}\n${fin}\n`).ok, false);
  assert.equal(call(`${rec({ target: { kind: ["bin"], name: "kws" } })}\n${fin}\n`).ok, false);
  assert.equal(call(`${rec()}\n${fin}\n`, { listingCut: true }).ok, false);
  assert.equal(call(`${rec()}\n`).ok, false, "no build-finished");
  assert.equal(call(`${rec()}\n${JSON.stringify({ reason: "build-finished", success: false })}\n`).ok, false);
});
