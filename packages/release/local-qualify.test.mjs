import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import zlib from "node:zlib";
import { spawnSync } from "node:child_process";
import * as upload from "../../scripts/ci/upload.mjs";
import * as reviewed from "../../scripts/ci/reviewed-candidate.mjs";
import {
  HOSTS, QualifyError, buildManifest, buildReceipt, candidateDocument, candidateIdFor, checkoutState, exportBundle, hostArgs, importBundle, parseHostArgs, parseQualifyArgs, planUnits,
  qualifyHost, reachArgs, remoteCommand, runQualify, sealCandidate, verifyBundle, writeBundleManifest, writeStoredZip,
} from "./local-qualify.mjs";

const COMMIT = "a".repeat(40); const TREE = "c".repeat(40);
const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const clone = (x) => structuredClone(x);
const temp = (t) => { const dir = fs.realpathSync.native(fs.mkdtempSync(path.join(process.env.RIGHTKIT_TEST_SCRATCH ?? os.tmpdir(), "rk-lq-"))); t.after(() => fs.rmSync(dir, { recursive: true, force: true })); return dir; };
const refused = (fn, pattern) => assert.throws(fn, (error) => error instanceof QualifyError && pattern.test(error.message), String(pattern));
const rejected = (promise, pattern) => assert.rejects(promise, (error) => error instanceof QualifyError && pattern.test(error.message), String(pattern));

// ---- arguments ----
test("qualify arguments: defaults, hosts, and every refusal", () => {
  const parsed = parseQualifyArgs(["cargo", "--crates", "rightkit-a,rightkit-b"], "win32");
  assert.deepEqual(parsed.crates, ["rightkit-a", "rightkit-b"]); assert.deepEqual(parsed.hosts, ["win", "mac"]); assert.equal(parsed.outRoot, "D:/scratch/rk-localq"); assert.equal(parsed.release, true);
  assert.equal(parseQualifyArgs(["cargo", "--crate", "rightkit-a"], "darwin").outRoot, "/Volumes/D/scratch/rk-localq");
  const full = parseQualifyArgs(["cargo", "--crates", "rightkit-a", "--hosts", "mac", "--id", "my-id", "--allow-ignored-units", "rightkit-a", "--jobs", "8", "--concurrency", "3", "--out", "D:/x", "--mac-root", "/m/rk", "--no-release"], "win32");
  assert.deepEqual([full.hosts, full.id, full.allowIgnoredUnits, full.jobs, full.concurrency, full.outRoot, full.roots.mac, full.release], [["mac"], "my-id", ["rightkit-a"], 8, 3, "D:/x", "/m/rk", false]);
  refused(() => parseQualifyArgs(["npm", "--crates", "x"], "win32"), /npm has no local qualification/);
  refused(() => parseQualifyArgs([], "win32"), /usage/);
  refused(() => parseQualifyArgs(["cargo"], "win32"), /requires --crates/);
  refused(() => parseQualifyArgs(["cargo", "--crates", "Bad_Name"], "win32"), /invalid crate name/);
  refused(() => parseQualifyArgs(["cargo", "--crates", "rightkit-a,rightkit-a"], "win32"), /more than once/);
  refused(() => parseQualifyArgs(["cargo", "--crates", "rightkit-a", "--allow-ignored-units", "rightkit-b"], "win32"), /not in --crates/);
  refused(() => parseQualifyArgs(["cargo", "--crates", "rightkit-a", "--hosts", "linux"], "win32"), /--hosts takes/);
  refused(() => parseQualifyArgs(["cargo", "--crates", "rightkit-a", "--hosts", "win,win"], "win32"), /--hosts takes/);
  refused(() => parseQualifyArgs(["cargo", "--crates", "rightkit-a", "--id", "Bad Id"], "win32"), /--id must match/);
  refused(() => parseQualifyArgs(["cargo", "--crates", "rightkit-a", "--jobs", "0"], "win32"), /--jobs/);
  refused(() => parseQualifyArgs(["cargo", "--crates", "rightkit-a", "--concurrency", "9"], "win32"), /--concurrency/);
  refused(() => parseQualifyArgs(["cargo", "--crates", "rightkit-a", "--bogus"], "win32"), /unknown qualify option/);
  refused(() => parseQualifyArgs(["cargo", "--crates", "rightkit-a", "--mac-root", "/x; rm -rf /"], "win32"), /plain path/);
  refused(() => parseQualifyArgs(["cargo", "--crates", "rightkit-a"], "linux"), /not linux/);
});

test("host arguments round trip, and the remote command only ever carries plain tokens", () => {
  const args = hostArgs({ root: "/Volumes/D/claude/rightkit", id: "local-a-aaaaaa", commit: COMMIT, crates: ["rightkit-a", "rightkit-b"], out: "/Volumes/D/scratch/rk-localq/local-a-aaaaaa/mac", exclude: ["rightkit-b"], allowIgnoredUnits: ["rightkit-a"], jobs: 6, concurrency: 3, canonical: true });
  assert.equal(args[0], "host");
  assert.deepEqual(parseHostArgs(args.slice(1)), { root: "/Volumes/D/claude/rightkit", id: "local-a-aaaaaa", commit: COMMIT, crates: ["rightkit-a", "rightkit-b"], out: "/Volumes/D/scratch/rk-localq/local-a-aaaaaa/mac", exclude: ["rightkit-b"], allowIgnoredUnits: ["rightkit-a"], jobs: 6, concurrency: 3, canonical: true });
  assert.equal(remoteCommand({ root: "/Volumes/D/claude/rightkit", args }), `node /Volumes/D/claude/rightkit/packages/release/local-qualify.mjs ${args.join(" ")}`);
  for (const bad of ["a b", "a;b", "$(x)", "a`b`", "a|b", "a&b", "a\"b", "a'b", "", "a\nb", "a>b"]) refused(() => remoteCommand({ root: "/r", args: ["host", bad] }), /refusing to send/);
  refused(() => remoteCommand({ root: "/r", args: ["host"], node: "node; evil" }), /refusing to send/);
  refused(() => parseHostArgs(["--root", "/r"]), /host needs/);
  refused(() => parseHostArgs([...args.slice(1), "--bogus"]), /unknown host option/);
  refused(() => parseHostArgs(args.slice(1).map((value) => (value === COMMIT ? "short" : value))), /host needs/);
  assert.deepEqual(reachArgs("win", "node x"), { command: "ssh", args: ["mac", "node x"] });
  assert.deepEqual(reachArgs("mac", "node x"), { command: "win", args: ["node x"] });
});

// ---- checkout and plan ----
const gitFake = ({ head = COMMIT, tree = TREE, dirty = "", remote = COMMIT, known = true, ancestor = true } = {}) => (args) => {
  const key = args.join(" ");
  if (key === "rev-parse HEAD") return head; if (key === "rev-parse HEAD^{tree}") return tree; if (key === "status --porcelain") return dirty;
  if (key === "ls-remote origin refs/heads/main") return remote ? `${remote}\trefs/heads/main` : "";
  if (args[0] === "cat-file") return known ? "" : null; if (args[0] === "merge-base") return ancestor ? "" : null;
  throw new Error(`unexpected git ${key}`);
};

test("the checkout must be clean with HEAD on origin/main", () => {
  assert.deepEqual(checkoutState({ git: gitFake(), root: "/r" }), { head: COMMIT, tree: TREE });
  refused(() => checkoutState({ git: gitFake({ dirty: " M crates/x/src/lib.rs" }), root: "/r" }), /not clean/);
  refused(() => checkoutState({ git: gitFake({ dirty: "?? crates/x/new.rs" }), root: "/r" }), /not clean/);
  refused(() => checkoutState({ git: gitFake({ remote: "b".repeat(40), ancestor: false }), root: "/r" }), /not pushed to origin\/main/);
  refused(() => checkoutState({ git: gitFake({ remote: "b".repeat(40), known: false }), root: "/r" }), /not pushed to origin\/main/);
  refused(() => checkoutState({ git: gitFake({ remote: "" }), root: "/r" }), /cannot read origin\/main/);
  refused(() => checkoutState({ git: gitFake({ head: "nope" }), root: "/r" }), /cannot resolve HEAD/);
  assert.deepEqual(checkoutState({ git: gitFake({ remote: "b".repeat(40) }), root: "/r" }).head, COMMIT, "HEAD behind a newer origin/main is still on main");
});

function workspace(t, { crates = ["rightkit-a"], gate = {}, assetGated = {}, linux = [] } = {}) {
  const root = temp(t);
  for (const name of crates) fs.mkdirSync(path.join(root, "crates", name), { recursive: true });
  const files = { "ci/gate.config.json": { rust: gate }, "ci/asset-gated.json": { crates: assetGated }, "ci/linux-required.json": { schema: 1, npm: [], crates: linux } };
  return { root, readJson: (file) => files[path.relative(root, file).split(path.sep).join("/")] ?? null, readPackage: (dir) => ({ name: path.basename(dir), version: "0.1.0" }) };
}

test("units are planned dependency-first with the allowsets the request implies, and unqualifiable units refuse", (t) => {
  const w = workspace(t, { crates: ["rightkit-a", "rightkit-b", "rightkit-c"], gate: { "rightkit-b": { os: ["macos"] } }, assetGated: { "rightkit-c": "needs assets" } });
  const plan = planUnits({ root: w.root, crates: ["rightkit-a", "rightkit-b", "rightkit-c"], readJson: w.readJson, readPackage: w.readPackage, dependsOn: (name) => (name === "rightkit-a" ? ["rightkit-c"] : []) });
  assert.deepEqual(plan.units.map((unit) => unit.name), ["rightkit-b", "rightkit-c", "rightkit-a"]);
  assert.deepEqual(plan.allow, { hostExclusion: ["rightkit-b"], noTest: [], ignored: [], missingAssets: ["rightkit-c"] });
  assert.deepEqual(plan.units.find((unit) => unit.name === "rightkit-b").excludedOn, ["win"]); assert.deepEqual(plan.units.find((unit) => unit.name === "rightkit-a").excludedOn, []);
  const linux = workspace(t, { linux: ["rightkit-a"] });
  refused(() => planUnits({ root: linux.root, crates: ["rightkit-a"], readJson: linux.readJson, readPackage: linux.readPackage, dependsOn: () => [] }), /requires a Linux receipt/);
  const runtime = workspace(t, { crates: ["rightkit-logs"] });
  refused(() => planUnits({ root: runtime.root, crates: ["rightkit-logs"], readJson: runtime.readJson, readPackage: runtime.readPackage, dependsOn: () => [] }), /continuous runtime owner/);
  const winOnly = workspace(t, { gate: { "rightkit-a": { os: ["windows"] } } });
  refused(() => planUnits({ root: winOnly.root, crates: ["rightkit-a"], readJson: winOnly.readJson, readPackage: winOnly.readPackage, dependsOn: () => [] }), /crates are packed on macOS/);
  refused(() => planUnits({ root: w.root, crates: ["rightkit-missing"], readJson: w.readJson, readPackage: w.readPackage, dependsOn: () => [] }), /unit directory not found/);
  refused(() => planUnits({ root: w.root, crates: ["rightkit-a"], readJson: w.readJson, readPackage: () => ({ name: "other", version: "0.1.0" }), dependsOn: () => [] }), /differs from --crates/);
  refused(() => planUnits({ root: w.root, crates: ["rightkit-a"], readJson: w.readJson, readPackage: () => ({ name: "rightkit-a", version: "0.1.0-rc1" }), dependsOn: () => [] }), /plain x\.y\.z/);
  refused(() => planUnits({ root: w.root, crates: ["rightkit-a"], readJson: () => null, readPackage: w.readPackage, dependsOn: () => [] }), /linux-required\.json is missing/);
  assert.match(candidateIdFor({ crates: ["rightkit-fs"], version: "0.3.2", head: COMMIT }), /^local-fs-032-aaaaaa$/);
  assert.match(candidateIdFor({ crates: ["rightkit-fs", "rightkit-job-queue"], head: COMMIT }), /^local-fs-job-queue-aaaaaa$/);
});

// ---- bundles ----
test("a host bundle round trips through export/import and any alteration, addition or removal is caught", (t) => {
  const dir = path.join(temp(t), "bundle"); fs.mkdirSync(path.join(dir, "rightkit-a"), { recursive: true });
  fs.writeFileSync(path.join(dir, "host-result.json"), "{}"); fs.writeFileSync(path.join(dir, "rightkit-a", "rightkit-a-0.1.0.crate"), Buffer.from([0, 1, 2, 255]));
  writeBundleManifest(dir); verifyBundle(dir);
  const exported = exportBundle(dir); const dest = path.join(temp(t), "pulled"); importBundle(exported, dest);
  assert.deepEqual(fs.readFileSync(path.join(dest, "rightkit-a", "rightkit-a-0.1.0.crate")), Buffer.from([0, 1, 2, 255]));
  refused(() => importBundle(exported, dest), /already exists/);
  const doc = JSON.parse(exported);
  const mutate = (fn) => { const copy = clone(doc); fn(copy); return JSON.stringify(copy); };
  for (const [name, text, pattern] of [
    ["altered bytes", mutate((d) => { d.files[0].b64 = Buffer.from("evil").toString("base64"); }), /altered .* in transit/],
    ["path escape", mutate((d) => { d.files[0].path = "../escape"; }), /malformed file row/], ["absolute", mutate((d) => { d.files[0].path = "/etc/x"; }), /malformed file row/], ["backslash", mutate((d) => { d.files[0].path = "a\\b"; }), /malformed file row/],
    ["duplicate", mutate((d) => { d.files.push(d.files[0]); }), /malformed file row/], ["not json", "nope", /not JSON/], ["empty", JSON.stringify({ schema: 1, files: [] }), /empty/], ["schema", mutate((d) => { d.schema = 2; }), /empty or of an unknown schema/],
  ]) refused(() => importBundle(text, path.join(temp(t), "x")), pattern);
  fs.appendFileSync(path.join(dir, "host-result.json"), " "); refused(() => verifyBundle(dir), /changed after the host wrote it/);
  writeBundleManifest(dir); fs.writeFileSync(path.join(dir, "extra.txt"), "x"); refused(() => verifyBundle(dir), /unlisted file extra\.txt/);
  fs.rmSync(path.join(dir, "extra.txt")); fs.rmSync(path.join(dir, "host-result.json")); refused(() => verifyBundle(dir), /host-result\.json is missing/);
  refused(() => verifyBundle(path.join(dir, "nowhere")), /no readable bundle\.manifest\.json/);
});

// ---- zip ----
function readStoredZip(bytes) {
  const end = bytes.length - 22; assert.equal(bytes.readUInt32LE(end), 0x06054b50);
  const count = bytes.readUInt16LE(end + 10); let cursor = bytes.readUInt32LE(end + 16); const out = {};
  for (let i = 0; i < count; i += 1) {
    assert.equal(bytes.readUInt32LE(cursor), 0x02014b50); assert.equal(bytes.readUInt16LE(cursor + 10), 0, "stored");
    const crc = bytes.readUInt32LE(cursor + 16), size = bytes.readUInt32LE(cursor + 24), nameLength = bytes.readUInt16LE(cursor + 28), offset = bytes.readUInt32LE(cursor + 42);
    const name = bytes.subarray(cursor + 46, cursor + 46 + nameLength).toString(); assert.equal(bytes.readUInt32LE(offset), 0x04034b50);
    const start = offset + 30 + bytes.readUInt16LE(offset + 26) + bytes.readUInt16LE(offset + 28); const data = bytes.subarray(start, start + size);
    assert.equal(zlib.crc32(data) >>> 0, crc); assert.equal(((bytes.readUInt32LE(cursor + 38) >>> 16) & 0o170000), 0o100000); out[name] = Buffer.from(data); cursor += 46 + nameLength;
  }
  return out;
}

test("the stored ZIP is flat, deterministic and readable by a strict reader (and by unzip when installed)", (t) => {
  const entries = [{ name: "manifest.json", data: Buffer.from("{}") }, { name: "rightkit-a-0.1.0.crate", data: crypto.randomBytes(4096) }, { name: "empty.json", data: Buffer.alloc(0) }];
  const zip = writeStoredZip(entries); assert.deepEqual(zip, writeStoredZip(entries));
  const read = readStoredZip(zip); assert.deepEqual(Object.keys(read), entries.map((e) => e.name)); for (const e of entries) assert.deepEqual(read[e.name], e.data);
  for (const name of ["../x", "a/b", "a\\b", ".hidden", "", "x".repeat(300), "a b"]) refused(() => writeStoredZip([{ name, data: Buffer.from("x") }]), /unsafe ZIP entry name/);
  const probe = spawnSync("unzip", ["-v"], { encoding: "utf8" });
  if (!probe.error && probe.status === 0) {
    const file = path.join(temp(t), "x.zip"); fs.writeFileSync(file, zip);
    assert.deepEqual(spawnSync("unzip", ["-Z1", file], { encoding: "utf8" }).stdout.split(/\r?\n/).filter(Boolean), entries.map((e) => e.name));
    assert.deepEqual(spawnSync("unzip", ["-p", file, "rightkit-a-0.1.0.crate"], { encoding: null }).stdout, entries[1].data);
  }
});

// ---- host results, receipts and the seal ----
const OS = { win: "win32", mac: "darwin" };
function hostBundle(dir, key, { id, units, canonical, bytes = {}, excluded = [], ignored = {}, commit = COMMIT, tree = TREE, mutate = () => {} }) {
  fs.mkdirSync(dir, { recursive: true });
  const rows = units.map((unit) => {
    if (excluded.includes(unit.name)) return { name: unit.name, version: unit.version, file: `${unit.name}-${unit.version}.crate`, sha256: null, bytesVerified: false, status: "allowed-exclusion", executed: 0, ignored: 0, skipped: 0, steps: [] };
    const crate = bytes[unit.name]; const sha = sha256(crate); const ign = ignored[unit.name] ?? 0;
    fs.mkdirSync(path.join(dir, unit.name), { recursive: true }); fs.writeFileSync(path.join(dir, unit.name, `${unit.name}-${unit.version}.crate`), crate); fs.writeFileSync(path.join(dir, unit.name, "gate.json"), "{}");
    if (canonical) fs.writeFileSync(path.join(dir, unit.name, `${unit.name}-${unit.version}.upload.json`), JSON.stringify({ name: unit.name, vers: unit.version, deps: [] }));
    const evidence = { schema: 1, kind: "cargo", name: unit.name, version: unit.version, commit, artifactSha256: sha, hostPolicy: `${OS[key] === "darwin" ? "darwin" : "win32"}-only`, hostPolicyReason: "fixture", runs: [{ host: { os: OS[key], arch: "x64" }, commit, features: ["default"], artifactSha256: sha, command: `cargo test -p ${unit.name}`, operator: "agent (local-build grant)", timestamp: "2026-10-22T00:00:00Z", passed: 3, failed: 0, skipped: 0, ignored: ign, steps: [] }] };
    return { name: unit.name, version: unit.version, file: `${unit.name}-${unit.version}.crate`, sha256: sha, bytesVerified: true, archiveScan: { schema: 1, scanner: "publish-cargo/scanCrateForSecrets", archiveSha256: sha, status: "pass", files: 4, violations: [], problems: [] }, status: ign > 0 ? "allowed-skips" : "pass", executed: 3, ignored: ign, skipped: 0, steps: [], evidence };
  });
  const scans = { schema: upload.LOCAL_SCAN_SCHEMA, passed: true, problems: [], sourceCommit: commit, headCommit: commit, cargoLockSha256: "d".repeat(64), scope: { units: units.map((u) => u.name), files: 9 }, tools: { pinned: {}, observed: {} }, classes: ["A", "B"], scanSetDigest: "e".repeat(64), classDigests: { A: "e".repeat(64), B: "f".repeat(64) }, inputDigest: null, deferred: [], scans: [{ id: "cargo-metadata", class: "A", command: "cargo metadata", status: "pass" }, { id: "clippy", class: "B", command: "cargo clippy", status: "pass" }] };
  const result = { schema: 1, kind: "local-host-result", host: HOSTS[key].name, hostOs: OS[key], arch: "x64", candidateId: id, commit, tree, broker: { name: "rustright", client: "C:/.rustright/runtime/rustright.exe", sha256: "9".repeat(64) }, qualifiedAt: "2026-10-22T00:00:00Z", scans, units: rows }; mutate(result);
  fs.writeFileSync(path.join(dir, "host-result.json"), JSON.stringify(result)); writeBundleManifest(dir);
  return dir;
}
const units = [{ name: "rightkit-a", version: "0.1.0", dir: "crates/rightkit-a" }, { name: "rightkit-b", version: "0.2.0", dir: "crates/rightkit-b" }];
const crateBytes = Object.fromEntries(units.map((u) => [u.name, crypto.randomBytes(256)]));
const plan = { units, allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] } };

function seal(t, { id = "local-ab-aaaaaa", win = {}, mac = {}, planOverride = plan } = {}) {
  const root = temp(t);
  const bundles = { win: hostBundle(path.join(root, "win"), "win", { id, units, bytes: crateBytes, ...win }), mac: hostBundle(path.join(root, "mac"), "mac", { id, units, bytes: crateBytes, canonical: true, ...mac }) };
  const staged = []; const stageDir = path.join(root, "stage");
  const stage = ({ files, receipts, allow }) => {
    fs.mkdirSync(path.join(stageDir, "packed"), { recursive: true });
    for (const { name, data } of files) fs.writeFileSync(path.join(stageDir, "packed", name), data);
    for (const [host, data] of Object.entries(receipts)) { fs.mkdirSync(path.join(stageDir, "receipts", host), { recursive: true }); fs.writeFileSync(path.join(stageDir, "receipts", host, `${host}.json`), data); }
    const sealed = upload.loadSealed({ packDir: path.join(stageDir, "packed"), registry: "crates", unitsText: planOverride.units.map((u) => u.name).join(" ") });
    staged.push(...sealed.problems, ...upload.checkLocalReceipts({ manifest: sealed.manifest, manifestSha256: sealed.manifestSha256, receiptsDir: path.join(stageDir, "receipts"), allow, candidate: { id } }));
  };
  const sealed = sealCandidate({ id, commit: COMMIT, tree: TREE, plan: planOverride, bundles, stage });
  return { sealed, staged, root, id };
}

test("receipt mapping round trip: host results become receipts that the promotion's own checker accepts, bound to the sealed manifest", (t) => {
  const { sealed, staged, id } = seal(t);
  assert.deepEqual(staged, []);
  assert.deepEqual(sealed.manifest.units.map((u) => [u.name, u.file, u.sha256, u.dir]), units.map((u) => [u.name, `${u.name}-${u.version}.crate`, sha256(crateBytes[u.name]), u.dir]));
  assert.equal(sealed.manifest.source, "local"); assert.equal(sealed.manifest.candidateId, id); assert.equal(sealed.manifest.commit, COMMIT);
  const windows = JSON.parse(sealed.receipts.windows); const macos = JSON.parse(sealed.receipts.macos);
  for (const doc of [windows, macos]) { assert.equal(doc.kind, "local-verify"); assert.equal(doc.manifestSha256, sealed.manifestSha256); assert.equal(doc.candidateId, id); assert.equal(doc.commit, COMMIT); assert.equal(doc.tree, TREE); assert.equal(doc.broker.name, "rustright"); assert.deepEqual(doc.scans.deferred, []); assert.equal(doc.units.length, 2); }
  assert.deepEqual([windows.host, windows.hostOs, macos.host, macos.hostOs], ["windows", "win32", "macos", "darwin"]);
  // The packed asset is the manifest and the exact canonical bytes.
  const zip = readStoredZip(sealed.zip); assert.deepEqual(Object.keys(zip).sort(), ["manifest.json", "rightkit-a-0.1.0.crate", "rightkit-a-0.1.0.upload.json", "rightkit-b-0.2.0.crate", "rightkit-b-0.2.0.upload.json"]);
  assert.equal(sha256(zip["manifest.json"]), sealed.manifestSha256); assert.deepEqual(zip["rightkit-a-0.1.0.crate"], crateBytes["rightkit-a"]);
  const standalone = buildReceipt({ result: JSON.parse(fs.readFileSync(path.join(seal(t).root, "mac", "host-result.json"))), candidateId: id, manifestSha256: sealed.manifestSha256, allow: sealed.allow });
  assert.equal(standalone.units.length, 2); assert.deepEqual(standalone.allowHostExclusion, []);
  const manifest = buildManifest({ id, commit: COMMIT, units: sealed.units }); assert.deepEqual(manifest, sealed.manifest);
});

test("sealing refuses hosts that qualified different bytes, trees, commits, candidates or a host that is not the packer", (t) => {
  const other = crypto.randomBytes(256);
  assert.throws(() => seal(t, { win: { bytes: { ...crateBytes, "rightkit-b": other } } }), (e) => e instanceof QualifyError && /rightkit-b: win packed .* canonical macOS archive .* different bytes/.test(e.message));
  assert.throws(() => seal(t, { mac: { tree: "d".repeat(40) } }), /host result is not for/);
  assert.throws(() => seal(t, { win: { commit: "b".repeat(40) } }), /host result is not for/);
  assert.throws(() => seal(t, { win: { id: "other" } }), /host result is not for/);
  assert.throws(() => seal(t, { win: { mutate: (r) => { r.hostOs = "darwin"; } } }), /host result is not for/);
  assert.throws(() => seal(t, { win: { mutate: (r) => { r.units.pop(); } } }), /no result for rightkit-b/);
  const root = temp(t); const win = hostBundle(path.join(root, "win"), "win", { id: "x", units, bytes: crateBytes });
  refused(() => sealCandidate({ id: "x", commit: COMMIT, tree: TREE, plan, bundles: { win }, stage: () => {} }), /canonical packer/);
  // The promotion checker has the last word: a receipt that fails it stops the seal before any upload.
  assert.match(seal(t, { win: { mutate: (r) => { r.scans.deferred = ["clippy"]; } } }).staged.join("\n"), /windows receipt: scans record has a deferred scan/);
});

test("ignored tests and host exclusions flow into the candidate allowsets and still pass the checker", (t) => {
  const { sealed, staged } = seal(t, { win: { ignored: { "rightkit-a": 2 } }, mac: { ignored: { "rightkit-a": 2 } } });
  assert.deepEqual(sealed.allow.ignored, ["rightkit-a"]); assert.deepEqual(staged, []);
  const excludedPlan = { units, allow: { hostExclusion: ["rightkit-b"], noTest: [], ignored: [], missingAssets: [] } };
  const out = seal(t, { planOverride: excludedPlan, win: { excluded: ["rightkit-b"] } });
  assert.deepEqual(out.staged, []); assert.equal(JSON.parse(out.sealed.receipts.windows).units.find((u) => u.name === "rightkit-b").status, "allowed-exclusion");
  const without = seal(t, { win: { excluded: ["rightkit-b"] } });
  assert.match(without.staged.join("\n"), /excluded from this host without allow_host_exclusion/);
});

test("the candidate document validates, pins the asset hashes, and leaves reviewer and reviewRef null so the approval loader refuses it until a human fills them", (t) => {
  const { sealed, id } = seal(t);
  const names = { packed: reviewed.localAssetName(id, "packed"), windows: reviewed.localAssetName(id, "windows"), macos: reviewed.localAssetName(id, "macos") };
  const contents = { packed: sealed.zip, windows: sealed.receipts.windows, macos: sealed.receipts.macos };
  const hashes = Object.fromEntries(Object.entries(contents).map(([kind, data]) => [kind, sha256(data)]));
  const doc = candidateDocument({ id, commit: COMMIT, units: sealed.units, allow: sealed.allow, names, hashes });
  assert.equal(reviewed.validateCandidate(clone(doc.candidate)).source, "local");
  assert.deepEqual(doc.candidate.units, ["rightkit-a", "rightkit-b"]); assert.equal(doc.candidate.release.tag, `candidate-${id}`);
  assert.deepEqual(doc.approval.artifacts.map((a) => [a.name, a.version, a.sha256, a.reviewer, a.reviewRef]), units.map((u) => [u.name, u.version, sha256(crateBytes[u.name]), null, null]));
  const file = path.join(temp(t), "approved-artifacts.json"); fs.writeFileSync(file, JSON.stringify({ schema: 2, candidates: [doc.candidate], artifacts: doc.approval.artifacts }));
  const loaded = upload.loadApprovedArtifacts(file); assert.ok(loaded.problems.some((p) => /reviewer must be a non-empty string/.test(p)) && loaded.problems.some((p) => /reviewRef must be a non-empty string/.test(p)));
  const ready = clone(doc.approval.artifacts).map((a) => ({ ...a, reviewer: "Astra", reviewRef: "https://example.test/review" })); fs.writeFileSync(file, JSON.stringify({ schema: 2, candidates: [doc.candidate], artifacts: ready }));
  assert.deepEqual(upload.loadApprovedArtifacts(file).problems, []);
  assert.equal(reviewed.selectCandidate(file, id).id, id);
});

// ---- one host ----
function hostRig(t, over = {}) {
  const root = temp(t); const out = path.join(temp(t), "out"); fs.writeFileSync(path.join(root, "Cargo.lock"), "# lock\n");
  const calls = { run: [], spawn: [], scans: [], meta: [] };
  const deps = {
    platform: "win32", arch: "x64", git: (args) => (args.join(" ") === "ls-files -z" ? "Cargo.lock\0crates/rightkit-a/src/lib.rs\0crates/other/src/lib.rs\0" : gitFake(over.git)(args)),
    managedRuntimeClient: () => "C:\\.rustright\\runtime\\rustright.exe", readFile: (file) => (String(file).includes("rustright") ? Buffer.from("broker bytes") : fs.readFileSync(file)),
    readPackage: (dir) => ({ name: path.basename(dir), version: "0.1.0" }), readJson: () => ({ rust: {} }), now: () => "2026-10-22T01:00:00Z",
    run: (command, args) => { calls.spawn.push([command, ...args]); return { status: 0, stdout: command === "where" ? "C:\\bin\\rightkit.cmd\n" : "", stderr: "" }; },
    loadScanModules: async () => ({ observeToolVersions: () => ({ "cargo-deny": "cargo-deny 0.20.2" }), buildLocalScanRecord: (input) => { calls.scans.push(input); return { schema: upload.LOCAL_SCAN_SCHEMA, passed: over.scanFail ? false : true, problems: over.scanFail ? ["clippy: fail"] : [], deferred: [], units: input.units, files: input.files }; } }),
    planScans: (options) => { calls.plan = options; return { scans: [{ id: "cargo-metadata" }, { id: "clippy" }] }; }, runScans: async () => ({ scans: [] }),
    loadArchiveScan: async () => ({ scanArchiveForSecrets: async ({ file }) => ({ schema: 1, scanner: "publish-cargo/scanCrateForSecrets", archiveSha256: sha256(fs.readFileSync(file)), status: "pass", files: 2, violations: [], problems: [] }) }),
    uploadMetadata: async ({ name, version }) => { calls.meta.push(name); return { name, vers: version, deps: [] }; },
    runAsync: async (command, args) => {
      calls.run.push([command, ...args]);
      const bundle = args[args.indexOf("--bundle") + 1]; const name = args[args.indexOf("--crate") + 1];
      if (over.failUnit === name) return { status: 1, stdout: "", stderr: "tests failed" };
      const crate = Buffer.from(`crate ${name}`); fs.mkdirSync(bundle);
      fs.writeFileSync(path.join(bundle, `${name}-0.1.0.crate`), crate);
      const sha = sha256(crate); fs.writeFileSync(path.join(bundle, "gate.json"), JSON.stringify({ schema: 1, kind: "cargo", name, version: "0.1.0", commit: COMMIT, artifactSha256: over.wrongEvidence ? "1".repeat(64) : sha, runs: [{ host: { os: "win32", arch: "x64" }, commit: COMMIT, passed: 3, failed: 0, skipped: 0, ignored: args.includes("--allow-ignored") ? 1 : 0, steps: [] }] }));
      return { status: 0, stdout: "", stderr: "" };
    },
    log: () => {},
  };
  const options = { root, id: "local-a-aaaaaa", commit: COMMIT, crates: ["rightkit-a", "rightkit-b"], out, exclude: [], allowIgnoredUnits: [], concurrency: 2, canonical: false, ...over.options };
  return { root, out, calls, deps: { ...deps, ...over.deps }, options };
}

test("one host: scans first, then qualify-only per unit, a verified bundle, honest rows, nothing partial", async (t) => {
  const rig = hostRig(t, { options: { allowIgnoredUnits: ["rightkit-a"], jobs: 5, canonical: true, exclude: ["rightkit-b"] } });
  const { out, result } = await qualifyHost(rig.options, rig.deps);
  assert.equal(out, path.resolve(rig.options.out)); verifyBundle(out);
  assert.equal(rig.calls.run.length, 1, "the excluded unit is never qualified");
  const cli = path.join(rig.root, "packages", "release", "cli", "right-release.mjs");
  assert.deepEqual(rig.calls.run[0], [process.execPath, cli, "publish", "cargo", "--crate", "rightkit-a", "--local", "--qualify-only", "--bundle", path.join(path.resolve(rig.options.out), "rightkit-a"), "--workspace", rig.root, "--allow-ignored", "--jobs", "5"]);
  assert.deepEqual(rig.calls.meta, ["rightkit-a"], "canonical host writes upload metadata");
  assert.ok(fs.existsSync(path.join(out, "rightkit-a", "rightkit-a-0.1.0.upload.json")));
  assert.deepEqual([result.host, result.hostOs, result.commit, result.tree, result.candidateId, result.broker.name], ["windows", "win32", COMMIT, TREE, "local-a-aaaaaa", "rustright"]);
  assert.equal(result.broker.sha256, sha256(Buffer.from("broker bytes"))); assert.equal(result.broker.client, "C:/.rustright/runtime/rustright.exe");
  const [a, b] = result.units; assert.deepEqual([a.name, a.status, a.executed, a.ignored], ["rightkit-a", "allowed-skips", 3, 1]); assert.equal(a.sha256, sha256(Buffer.from("crate rightkit-a"))); assert.equal(a.archiveScan.status, "pass");
  assert.deepEqual([b.name, b.status, b.sha256, b.bytesVerified], ["rightkit-b", "allowed-exclusion", null, false]);
  assert.equal(result.scans.passed, true);
  // Scans: whole-workspace scans plus only the units' crates, class A then B, nothing deferred, through the plan builder.
  assert.deepEqual(rig.calls.plan.changed, ["Cargo.lock", "crates/rightkit-a/src/lib.rs"]); assert.deepEqual(rig.calls.plan.cargoCommand, ["rightkit", "cargo"]); assert.equal(rig.calls.plan.includeTests, false); assert.equal(rig.calls.plan.liveRoot, rig.root);
  assert.deepEqual(rig.calls.scans[0].units, ["rightkit-a", "rightkit-b"]); assert.equal(rig.calls.scans[0].sourceCommit, COMMIT); assert.equal(rig.calls.scans[0].headCommit, COMMIT);
  assert.ok(fs.existsSync(path.join(out, "host-result.json")) && !fs.readdirSync(out).some((name) => /^(export|meta)-/.test(name)), "no scratch is left in the bundle");
});

test("one host refuses a moved or dirty HEAD, a missing broker, failed scans, bad evidence, a failed unit or an existing output", async (t) => {
  const moved = hostRig(t, { git: { head: "b".repeat(40) } }); await rejected(qualifyHost(moved.options, moved.deps), /not the candidate commit/);
  const dirty = hostRig(t, { git: { dirty: " M x" } }); await rejected(qualifyHost(dirty.options, dirty.deps), /not clean/);
  const noBroker = hostRig(t, { deps: { managedRuntimeClient: () => undefined } }); await rejected(qualifyHost(noBroker.options, noBroker.deps), /never plain cargo/);
  const scans = hostRig(t, { scanFail: true }); await rejected(qualifyHost(scans.options, scans.deps), /deterministic scans did not pass:\n  - clippy: fail/); assert.equal(scans.calls.run.length, 0, "no unit starts after failed scans");
  const evidence = hostRig(t, { wrongEvidence: true }); await rejected(qualifyHost(evidence.options, evidence.deps), /gate evidence does not describe the bundled archive/);
  const failing = hostRig(t, { failUnit: "rightkit-b" }); await rejected(qualifyHost(failing.options, failing.deps), /rightkit-b: qualify-only failed .*exit 1\)\n+tests failed/);
  const existing = hostRig(t); fs.mkdirSync(existing.options.out); await rejected(qualifyHost(existing.options, existing.deps), /host output already exists/);
  const linuxHost = hostRig(t, { deps: { platform: "linux" } }); await rejected(qualifyHost(linuxHost.options, linuxHost.deps), /not linux/);
  const noCargoShim = hostRig(t, { deps: { run: () => ({ status: 1, stdout: "", stderr: "" }) } }); await rejected(qualifyHost(noCargoShim.options, noCargoShim.deps), /export|never used|cannot export/);
});

// ---- the driver ----
function driverRig(t, { platform = "win32", extra = [], over = {} } = {}) {
  const here = platform === "win32" ? "win" : "mac"; const there = here === "win" ? "mac" : "win";
  const root = temp(t); const outRoot = temp(t); const remoteDisk = temp(t);
  for (const name of ["rightkit-a", "rightkit-b"]) fs.mkdirSync(path.join(root, "crates", name), { recursive: true });
  const files = { "ci/gate.config.json": { rust: {} }, "ci/asset-gated.json": { crates: {} }, "ci/linux-required.json": { schema: 1, npm: [], crates: [] } };
  const log = []; const execs = []; const spawns = []; const printed = [];
  let startedHere; let startedThere; const here$ = new Promise((resolve) => { startedHere = resolve; }); const there$ = new Promise((resolve) => { startedThere = resolve; });
  const guard = (promise) => Promise.race([promise, new Promise((_, reject) => setTimeout(() => reject(new Error("hosts did not run at the same time")), 3000))]);
  const id = "local-ab-aaaaaa";
  const remoteDir = (arg) => path.join(remoteDisk, ...arg.split(`${HOSTS[there].scratch}/`)[1].split("/"));
  const deps = {
    platform, root, git: gitFake(), readJson: (file) => files[path.relative(root, file).split(path.sep).join("/")] ?? null, readPackage: (dir) => ({ name: path.basename(dir), version: path.basename(dir) === "rightkit-a" ? "0.1.0" : "0.2.0" }), dependsOn: () => [],
    remoteNode: "node", loadUpload: async () => upload, loadReviewed: async () => reviewed, log: (line) => printed.push(line),
    qualifyHost: async (options) => { log.push(`${here} start`); startedHere(); await guard(there$); hostBundle(options.out, here, { id, units, bytes: crateBytes, canonical: here === "mac", ...over.here }); log.push(`${here} end`); return { out: options.out }; },
    exec: async (command, argv) => {
      execs.push({ command, argv });
      const cmd = argv.at(-1);
      if (/ host /.test(cmd)) { log.push(`${there} start`); startedThere(); await guard(here$); if (over.remoteFail) return { status: 3, stdout: "", stderr: "remote exploded" }; hostBundle(remoteDir(argv.at(-1).split("--out ")[1].split(" ")[0]), there, { id, units, bytes: crateBytes, canonical: there === "mac", ...over.there }); log.push(`${there} end`); return { status: 0, stdout: "ok", stderr: "" }; }
      if (/ export /.test(cmd)) return { status: 0, stdout: exportBundle(remoteDir(cmd.split("--dir ")[1])), stderr: "" };
      throw new Error(`unexpected exec ${cmd}`);
    },
    spawn: (command, argv) => {
      spawns.push([command, ...argv]);
      if (argv[0] === "release") return { status: over.releaseFail ? 1 : 0, stdout: "", stderr: over.releaseFail ? "HTTP 422" : "" };
      const names = spawns.find((call) => call[1] === "release").filter((a) => /\.(zip|json)$/.test(a) && fs.existsSync(a)); const assets = names.map((file) => ({ name: path.basename(file), size: fs.statSync(file).size }));
      return { status: 0, stdout: JSON.stringify({ prerelease: true, assets: over.extraAsset ? [...assets, { name: "x", size: 1 }] : assets }), stderr: "" };
    },
  };
  return { deps, root, outRoot, log, execs, spawns, printed, id, here, there, args: ["cargo", "--crates", "rightkit-a,rightkit-b", "--id", id, ...extra, "--out", outRoot] };
}

test("driver (Dell): both hosts run at once, the Mac over ssh, the bundle comes back by export, and one candidate is cut and read back from its prerelease", async (t) => {
  const rig = driverRig(t); const result = await runQualify(rig.args, rig.deps);
  assert.deepEqual(new Set(rig.log.slice(0, 2)), new Set(["win start", "mac start"]), "neither host waits for the other");
  assert.equal(rig.execs.length, 2);
  const [run, pull] = rig.execs;
  assert.equal(run.command, "ssh"); assert.equal(run.argv[0], "mac");
  assert.equal(run.argv[1], `node /Volumes/D/claude/rightkit/packages/release/local-qualify.mjs host --root /Volumes/D/claude/rightkit --id ${rig.id} --commit ${COMMIT} --crates rightkit-a,rightkit-b --out /Volumes/D/scratch/rk-localq/${rig.id}/mac --concurrency 2 --canonical`);
  assert.equal(pull.command, "ssh"); assert.equal(pull.argv[1], `node /Volumes/D/claude/rightkit/packages/release/local-qualify.mjs export --dir /Volumes/D/scratch/rk-localq/${rig.id}/mac`);
  const tag = `candidate-${rig.id}`; const assets = path.join(rig.outRoot, rig.id, "assets");
  const create = rig.spawns[0];
  assert.deepEqual(create.slice(0, 10), ["gh", "release", "create", tag, "--repo", "bogusyogi/rightkit", "--prerelease", "--target", COMMIT, "--title"]);
  assert.deepEqual(create.slice(-3), ["packed", "windows", "macos"].map((kind) => path.join(assets, reviewed.localAssetName(rig.id, kind))));
  assert.deepEqual(rig.spawns[1].slice(0, 3), ["gh", "api", `repos/bogusyogi/rightkit/releases/tags/${tag}`]);
  const { candidate, document } = result;
  assert.deepEqual([candidate.id, candidate.source, candidate.sourceSha, candidate.registry, candidate.units, candidate.release], [rig.id, "local", COMMIT, "crates", ["rightkit-a", "rightkit-b"], { tag }]);
  for (const kind of ["packed", "windows", "macos"]) assert.equal(candidate.assets[kind].sha256, sha256(fs.readFileSync(path.join(assets, candidate.assets[kind].name))), kind);
  reviewed.validateCandidate(clone(candidate));
  assert.deepEqual(document.approval.artifacts.map((a) => a.reviewer), [null, null]);
  assert.equal(JSON.parse(fs.readFileSync(path.join(rig.outRoot, rig.id, "candidate.json"), "utf8")).candidate.id, rig.id);
  assert.ok(rig.printed.join("\n").includes(`"id": "${rig.id}"`) && /Add this to ci\/approved-artifacts\.json/.test(rig.printed.join("\n")));
  // It never writes the approval: no approved-artifacts.json anywhere it wrote.
  const written = []; const walk = (dir) => { for (const entry of fs.readdirSync(dir, { withFileTypes: true })) { if (entry.isDirectory()) walk(path.join(dir, entry.name)); else written.push(entry.name); } }; walk(rig.outRoot); walk(rig.root);
  assert.ok(!written.includes("approved-artifacts.json"));
  // The staged candidate is the exact layout the promotion rebuilds, and the pinned assets pass it end to end.
  const sealed = upload.loadSealed({ packDir: path.join(rig.outRoot, rig.id, "stage", "packed"), registry: "crates", unitsText: "rightkit-b rightkit-a".split(" ").sort().join(" ") });
  assert.ok(sealed.manifest);
});

test("driver (Mac): the Dell is reached with `win \"<cmd>\"`, and the Mac is the local canonical packer", async (t) => {
  const rig = driverRig(t, { platform: "darwin" }); const result = await runQualify(rig.args, rig.deps);
  assert.equal(rig.execs[0].command, "win"); assert.equal(rig.execs[0].argv.length, 1);
  assert.match(rig.execs[0].argv[0], new RegExp(`^node D:/Claude/rightkit/packages/release/local-qualify\\.mjs host --root D:/Claude/rightkit --id ${rig.id} --commit ${COMMIT} --crates rightkit-a,rightkit-b --out D:/scratch/rk-localq/${rig.id}/win --concurrency 2$`));
  assert.equal(rig.execs[1].command, "win"); assert.ok(result.candidate);
});

test("driver refusals: a failing host, different bytes, an unpublished HEAD, a partial host set and a release that does not read back all stop before a candidate exists", async (t) => {
  const remote = driverRig(t, { over: { remoteFail: true } }); await rejected(runQualify(remote.args, remote.deps), /qualification failed:\nmac: remote qualification failed \(exit 3\)\n+remote exploded/);
  assert.equal(remote.spawns.length, 0, "no release after a failed host");
  const bytes = driverRig(t, { over: { there: { bytes: { ...crateBytes, "rightkit-a": crypto.randomBytes(256) } } } }); await rejected(runQualify(bytes.args, bytes.deps), /canonical macOS archive .* different bytes/); assert.equal(bytes.spawns.length, 0);
  const dirty = driverRig(t); dirty.deps.git = gitFake({ dirty: " M x" }); await rejected(runQualify(dirty.args, dirty.deps), /not clean/); assert.equal(dirty.execs.length, 0);
  const unpushed = driverRig(t); unpushed.deps.git = gitFake({ remote: "b".repeat(40), ancestor: false }); await rejected(runQualify(unpushed.args, unpushed.deps), /not pushed/);
  const release = driverRig(t, { over: { releaseFail: true } }); await rejected(runQualify(release.args, release.deps), /gh release create failed:\s+HTTP 422/);
  const extra = driverRig(t, { over: { extraAsset: true } }); await rejected(runQualify(extra.args, extra.deps), /does not hold exactly the three assets/);
  const partial = driverRig(t, { extra: ["--hosts", "win"] });
  partial.deps.qualifyHost = async (options) => { hostBundle(options.out, "win", { id: partial.id, units, bytes: crateBytes }); return { out: options.out }; };
  const outcome = await runQualify(partial.args, partial.deps); assert.equal(outcome.candidate, null); assert.equal(partial.execs.length, 0); assert.equal(partial.spawns.length, 0);
  const none = driverRig(t, { extra: ["--no-release"] }); const noRelease = await runQualify(none.args, none.deps);
  assert.equal(none.spawns.length, 0); assert.ok(noRelease.candidate, "--no-release still seals and prints the candidate");
});
