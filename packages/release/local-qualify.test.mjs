import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import zlib from "node:zlib";
import { spawnSync } from "./background-process.mjs";
import * as upload from "../../scripts/ci/upload.mjs";
import * as reviewed from "../../scripts/ci/reviewed-candidate.mjs";
import {
  DEFAULT_QUALIFY_CONFIG, GATE_CONFIG_FILE, HOSTS, QualifyError, buildManifest, buildReceipt, candidateDocument, candidateIdFor, checkoutState, ciDispatchArgs, dispatchCi, exportBundle, hostArgs, hostRoot, importBundle, loadQualifyConfig, parseHostArgs, parseQualifyArgs, planUnits,
  qualifyHost, reachArgs, remoteCommand, remoteScriptPath, repositoryFromOrigin, runQualify, sealCandidate, verifyBundle, writeBundleManifest, writeStoredZip,
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
  const args = hostArgs({ root: "/Volumes/D/claude/rightkit", id: "local-a-aaaaaa", commit: COMMIT, crates: ["rightkit-a", "rightkit-b"], out: "/Volumes/D/scratch/rk-localq/local-a-aaaaaa/mac", exclude: ["rightkit-b"], allowIgnoredUnits: ["rightkit-a"], jobs: 6, concurrency: 3, runtimeRepeats: 4, waitIdleMin: 10, canonical: true });
  assert.equal(args[0], "host");
  assert.deepEqual(parseHostArgs(args.slice(1)), { root: "/Volumes/D/claude/rightkit", id: "local-a-aaaaaa", commit: COMMIT, crates: ["rightkit-a", "rightkit-b"], out: "/Volumes/D/scratch/rk-localq/local-a-aaaaaa/mac", exclude: ["rightkit-b"], allowIgnoredUnits: ["rightkit-a"], jobs: 6, concurrency: 3, runtimeRepeats: 4, waitIdleMin: 10, canonical: true });
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
const gitFake = ({ head = COMMIT, tree = TREE, dirty = "", remote = COMMIT, known = true, ancestor = true, origin = "https://github.com/bogusyogi/rightkit.git" } = {}) => (args) => {
  const key = args.join(" ");
  if (key === "remote get-url origin") return origin;
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
  // A runtime owner with trusted coverage is planned like any other unit; one without has no workload to run and refuses.
  const runtime = workspace(t, { crates: ["rightkit-logs", "rightkit-fsindex"] });
  const planned = planUnits({ root: runtime.root, crates: ["rightkit-logs"], readJson: runtime.readJson, readPackage: runtime.readPackage, dependsOn: () => [], runtimeCoverage: ["rightkit-logs"] });
  assert.deepEqual(planned.units.map((unit) => unit.name), ["rightkit-logs"]);
  refused(() => planUnits({ root: runtime.root, crates: ["rightkit-logs"], readJson: runtime.readJson, readPackage: runtime.readPackage, dependsOn: () => [] }), /no trusted runtime coverage/);
  refused(() => planUnits({ root: runtime.root, crates: ["rightkit-fsindex"], readJson: runtime.readJson, readPackage: runtime.readPackage, dependsOn: () => [], runtimeCoverage: ["rightkit-logs"] }), /no trusted runtime coverage/);
  const macOnlyRuntime = workspace(t, { crates: ["rightkit-logs"], gate: { "rightkit-logs": { os: ["macos"] } } });
  refused(() => planUnits({ root: macOnlyRuntime.root, crates: ["rightkit-logs"], readJson: macOnlyRuntime.readJson, readPackage: macOnlyRuntime.readPackage, dependsOn: () => [], runtimeCoverage: ["rightkit-logs"] }), /needs its runtime proof on both native hosts/);
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
  assert.equal(run.argv[1], `node /Volumes/D/claude/rightkit/packages/release/local-qualify.mjs host --root /Volumes/D/claude/rightkit --id ${rig.id} --commit ${COMMIT} --crates rightkit-a,rightkit-b --out /Volumes/D/scratch/rk-localq/${rig.id}/mac --concurrency 2 --runtime-repeats 3 --wait-idle-min 30 --canonical`);
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
  assert.match(rig.execs[0].argv[0], new RegExp(`^node D:/Claude/rightkit/packages/release/local-qualify\\.mjs host --root D:/Claude/rightkit --id ${rig.id} --commit ${COMMIT} --crates rightkit-a,rightkit-b --out D:/scratch/rk-localq/${rig.id}/win --concurrency 2 --runtime-repeats 3 --wait-idle-min 30$`));
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

// ---- runtime owners ----
test("a runtime owner is measured after every unit finished, its entry rides in the host result, and the driver binds it to the sealed manifest", async (t) => {
  const order = [];
  const rig = hostRig(t, { options: { crates: ["rightkit-a", "rightkit-logs"], canonical: true }, deps: {
    measureRuntime: async (options) => { order.push(["measure", options.crate, options.repeats, options.waitIdleMs, options.archiveSha256]); return { schema: 1, kind: "local-runtime-crate", crate: options.crate, manifestSha256: null, workloads: [{ manifestSha256: null, artifactDigest: null }] }; },
  } });
  const baseRun = rig.deps.runAsync;
  rig.deps.runAsync = async (command, args) => { order.push(["qualify", args[args.indexOf("--crate") + 1]]); return baseRun(command, args); };
  const { result } = await qualifyHost({ ...rig.options, runtimeRepeats: 4, waitIdleMin: 5 }, rig.deps);
  assert.deepEqual(order.map((row) => row[0]), ["qualify", "qualify", "measure"], "measurement starts only after every unit qualified");
  assert.deepEqual(order[2].slice(1, 4), ["rightkit-logs", 4, 300_000]); assert.equal(order[2][4], sha256(Buffer.from("crate rightkit-logs")), "the entry names the archive this host packed");
  assert.deepEqual(Object.keys(result.runtime.crates), ["rightkit-logs"]); assert.equal(result.runtime.kind, "local-runtime");
  const receipt = buildReceipt({ result, candidateId: "local-a-aaaaaa", manifestSha256: "7".repeat(64), allow: plan.allow });
  assert.equal(receipt.runtime.crates["rightkit-logs"].manifestSha256, "7".repeat(64)); assert.equal(receipt.runtime.crates["rightkit-logs"].workloads[0].artifactDigest, `local:${"7".repeat(64)}`);
  assert.equal(result.runtime.crates["rightkit-logs"].manifestSha256, null, "the host result itself stays unbound");
  const plain = hostRig(t); const { result: plainResult } = await qualifyHost(plain.options, plain.deps);
  assert.equal(Object.hasOwn(plainResult, "runtime"), false); assert.equal(Object.hasOwn(buildReceipt({ result: plainResult, candidateId: "x", manifestSha256: "7".repeat(64), allow: plan.allow }), "runtime"), false);
  const failing = hostRig(t, { options: { crates: ["rightkit-logs"] }, deps: { measureRuntime: async () => { throw new Error("the host is not quiet; no measurement was taken: before: broker inFlight is 2, not 0"); } } });
  await rejected(qualifyHost(failing.options, failing.deps), /win: runtime proof for rightkit-logs refused:\n  - the host is not quiet/);
  assert.equal(fs.existsSync(path.join(failing.options.out, "host-result.json")), false, "no result is written for a refused runtime proof");
  const excluded = hostRig(t, { options: { crates: ["rightkit-logs"], exclude: ["rightkit-logs"] }, deps: { measureRuntime: async () => assert.fail("never measured") } });
  await rejected(qualifyHost(excluded.options, excluded.deps), /is a runtime owner; the runtime proof needs both native hosts/);
});

test("runtime options: repeats 3 to 9 and a bounded idle wait reach the host", () => {
  const parsed = parseQualifyArgs(["cargo", "--crates", "rightkit-logs", "--runtime-repeats", "5", "--wait-idle-min", "0"], "win32");
  assert.deepEqual([parsed.runtimeRepeats, parsed.waitIdleMin], [5, 0]);
  assert.deepEqual([parseQualifyArgs(["cargo", "--crates", "rightkit-logs"], "win32").runtimeRepeats, parseQualifyArgs(["cargo", "--crates", "rightkit-logs"], "win32").waitIdleMin], [3, 30]);
  for (const bad of [["--runtime-repeats", "2"], ["--runtime-repeats", "10"], ["--runtime-repeats", "x"], ["--wait-idle-min", "-1"], ["--wait-idle-min", "241"]]) refused(() => parseQualifyArgs(["cargo", "--crates", "rightkit-logs", ...bad], "win32"), /--runtime-repeats|--wait-idle-min/);
  refused(() => parseHostArgs([...hostArgs({ root: "/r", id: "x-aaaaaa", commit: COMMIT, crates: ["rightkit-a"], out: "/o" }).slice(1), "--runtime-repeats", "2"]), /--runtime-repeats/);
});

// ---- any repository, not only RightKit (adoption gap 1) ----

const ACME_CONFIG = { hosts: { win: { root: "D:/work/widgets" }, mac: { root: "/Volumes/D/work/widgets" } }, approvedArtifacts: "release/approved.json", promotionWorkflow: "promote.yml" };
const fakeRoot = (files) => ({ readText: (file) => files[path.basename(file)], exists: (file) => Object.hasOwn(files, path.basename(file)) });
const configFor = (origin, files = {}) => loadQualifyConfig({ root: "/r", git: gitFake({ origin }), ...fakeRoot(files) });

test("the repository is owner/name from the origin remote, in every URL form git uses", () => {
  for (const [url, expected] of [
    ["https://github.com/bogusyogi/rightkit.git", "bogusyogi/rightkit"], ["https://github.com/bogusyogi/rightkit", "bogusyogi/rightkit"], ["https://github.com/Bogusyogi/RightKit.git\n", "bogusyogi/rightkit"],
    ["git@github.com:acme/widgets.git", "acme/widgets"], ["git@github.com-work:acme/widgets.git", "acme/widgets"], ["ssh://git@github.com/acme/widgets.git", "acme/widgets"],
    ["https://x-access-token:abc@github.com/acme/widgets.git/", "acme/widgets"], ["https://github.com/acme/wid.gets", "acme/wid.gets"],
  ]) assert.equal(repositoryFromOrigin(url), expected, url);
  for (const bad of [null, undefined, "", "https://gitlab.com/acme/widgets.git", "https://github.com/acme", "https://github.com/acme/widgets/extra", "file:///c/repo", "https://github.com/../widgets", "git@github.com:acme/widg ets.git"]) assert.equal(repositoryFromOrigin(bad), null, String(bad));
});

test("config: RightKit's own values are the defaults only when the origin is bogusyogi/rightkit; a committed file overrides them", () => {
  const own = configFor("https://github.com/bogusyogi/rightkit.git");
  assert.deepEqual(own, { repository: "bogusyogi/rightkit", hosts: { win: { root: "D:/Claude/rightkit" }, mac: { root: "/Volumes/D/claude/rightkit" } }, approvedArtifacts: "ci/approved-artifacts.json", promotionWorkflow: "publish-reviewed.yml" });
  assert.deepEqual(DEFAULT_QUALIFY_CONFIG.hosts.win, { root: HOSTS.win.root });
  const partial = configFor("git@github.com:bogusyogi/rightkit.git", { [GATE_CONFIG_FILE]: JSON.stringify({ hosts: { mac: { root: "/m/rk" } }, promotionWorkflow: "other.yml" }) });
  assert.deepEqual([partial.hosts.win.root, partial.hosts.mac.root, partial.approvedArtifacts, partial.promotionWorkflow], ["D:/Claude/rightkit", "/m/rk", "ci/approved-artifacts.json", "other.yml"]);
  const acme = configFor("git@github.com:acme/widgets.git", { [GATE_CONFIG_FILE]: JSON.stringify(ACME_CONFIG) });
  assert.deepEqual(acme, { repository: "acme/widgets", ...ACME_CONFIG });
  assert.equal(hostRoot({ key: "mac", config: acme }), "/Volumes/D/work/widgets");
  assert.equal(hostRoot({ key: "mac", flags: { mac: "/x/y" }, config: acme }), "/x/y", "a flag wins");
});

test("config: any other repository refuses by name for every missing value, and for a missing or malformed file", () => {
  const origin = "https://github.com/acme/widgets.git";
  refused(() => configFor(origin), /acme\/widgets has no \.rightkit-gate\.json at its root and no built-in values.*"approvedArtifacts".*"promotionWorkflow"/);
  const without = (key) => { const { [key]: _drop, ...rest } = ACME_CONFIG; return JSON.stringify(rest); };
  refused(() => configFor(origin, { [GATE_CONFIG_FILE]: without("approvedArtifacts") }), /does not set approvedArtifacts, and only bogusyogi\/rightkit has a default/);
  refused(() => configFor(origin, { [GATE_CONFIG_FILE]: without("promotionWorkflow") }), /does not set promotionWorkflow/);
  const noHosts = configFor(origin, { [GATE_CONFIG_FILE]: without("hosts") });
  assert.deepEqual(noHosts.hosts, {}, "the config loads; a host root is required only when that host runs");
  refused(() => hostRoot({ key: "mac", config: noHosts }), /no checkout root for the macos host: set hosts\.mac\.root in \.rightkit-gate\.json or pass --mac-root/);
  refused(() => hostRoot({ key: "win", config: configFor(origin, { [GATE_CONFIG_FILE]: JSON.stringify({ ...ACME_CONFIG, hosts: { mac: { root: "/m" } } }) }) }), /no checkout root for the windows host/);
  for (const [text, pattern] of [
    ["{", /is not valid JSON/], ["[]", /must be a JSON object/], ['{"hostz":{}}', /unknown key "hostz"/],
    [JSON.stringify({ ...ACME_CONFIG, hosts: { win: { root: "D:/x; rm -rf /" } } }), /hosts\.win\.root must be a plain checkout path/], [JSON.stringify({ ...ACME_CONFIG, hosts: { win: "D:/x" } }), /hosts\.win\.root/],
    [JSON.stringify({ ...ACME_CONFIG, hosts: { linux: { root: "/x" } } }), /unknown host "linux"/], [JSON.stringify({ ...ACME_CONFIG, hosts: [] }), /hosts must be an object/],
    [JSON.stringify({ ...ACME_CONFIG, approvedArtifacts: "../x.json" }), /approvedArtifacts must be a plain/], [JSON.stringify({ ...ACME_CONFIG, approvedArtifacts: "/abs/x.json" }), /approvedArtifacts must be a plain/],
    [JSON.stringify({ ...ACME_CONFIG, promotionWorkflow: "a/b.yml" }), /promotionWorkflow must be a workflow file name/], [JSON.stringify({ ...ACME_CONFIG, promotionWorkflow: "x; y.yml" }), /promotionWorkflow must be a workflow file name/],
  ]) refused(() => configFor(origin, { [GATE_CONFIG_FILE]: text }), pattern);
  refused(() => loadQualifyConfig({ root: "/r", git: gitFake({ origin: "" }), ...fakeRoot({}) }), /cannot derive owner\/name.*no origin remote/);
  refused(() => loadQualifyConfig({ root: "/r", git: () => null, ...fakeRoot({}) }), /no origin remote/);
  refused(() => configFor("https://gitlab.example/acme/widgets.git"), /cannot derive owner\/name.*GitHub origin/);
  assert.ok(!/s3cret/.test((() => { try { configFor("https://user:s3cret@gitlab.example/a/b.git"); } catch (error) { return error.message; } return ""; })()), "a credential in the URL is never echoed");
});

test("the other host finds this script in the source layout or the installed package; --repository reaches a host", () => {
  assert.equal(remoteScriptPath("/r", "/r/packages/release/local-qualify.mjs"), "packages/release/local-qualify.mjs");
  assert.equal(remoteScriptPath("/r", "/r/node_modules/.pnpm/@rightkit+release@0.5.0/node_modules/@rightkit/release/local-qualify.mjs"), "node_modules/@rightkit/release/local-qualify.mjs");
  assert.equal(remoteScriptPath("/r", "/elsewhere/node_modules/@rightkit/release/local-qualify.mjs"), "node_modules/@rightkit/release/local-qualify.mjs");
  assert.equal(remoteScriptPath("/r", "/elsewhere/rightkit/packages/release/local-qualify.mjs"), "packages/release/local-qualify.mjs");
  assert.equal(remoteCommand({ root: "/m/w", args: ["host"], script: "node_modules/@rightkit/release/local-qualify.mjs" }), "node /m/w/node_modules/@rightkit/release/local-qualify.mjs host");
  const args = hostArgs({ root: "/m/w", id: "x-aaaaaa", commit: COMMIT, crates: ["rightkit-a"], out: "/o", repository: "acme/widgets" });
  assert.deepEqual(args.slice(-2), ["--repository", "acme/widgets"]);
  assert.equal(parseHostArgs(args.slice(1)).repository, "acme/widgets");
  assert.equal(Object.hasOwn(parseHostArgs(hostArgs({ root: "/m/w", id: "x-aaaaaa", commit: COMMIT, crates: ["rightkit-a"], out: "/o" }).slice(1)), "repository"), false);
  refused(() => parseHostArgs([...args.slice(1, -1), "not a repo"]), /--repository must be owner\/name/);
});

test("driver for another repository: the origin names it, the config places the hosts, the approvals file and the promotion workflow", async (t) => {
  const rig = driverRig(t);
  rig.deps.git = gitFake({ origin: "git@github.com:acme/widgets.git" });
  rig.deps.script = "node_modules/@rightkit/release/local-qualify.mjs";
  fs.writeFileSync(path.join(rig.root, GATE_CONFIG_FILE), JSON.stringify(ACME_CONFIG));
  const result = await runQualify(rig.args, rig.deps);
  const [run, pull] = rig.execs;
  assert.equal(run.argv[1], `node /Volumes/D/work/widgets/node_modules/@rightkit/release/local-qualify.mjs host --root /Volumes/D/work/widgets --id ${rig.id} --commit ${COMMIT} --crates rightkit-a,rightkit-b --out /Volumes/D/scratch/rk-localq/${rig.id}/mac --concurrency 2 --runtime-repeats 3 --wait-idle-min 30 --canonical --repository acme/widgets`);
  assert.equal(pull.argv[1], `node /Volumes/D/work/widgets/node_modules/@rightkit/release/local-qualify.mjs export --dir /Volumes/D/scratch/rk-localq/${rig.id}/mac`);
  const tag = `candidate-${rig.id}`;
  const create = rig.spawns[0];
  assert.deepEqual(create.slice(0, 7), ["gh", "release", "create", tag, "--repo", "acme/widgets", "--prerelease"]);
  assert.match(create[create.indexOf("--notes") + 1], /promoted only by promote\.yml after its pins are committed/);
  assert.deepEqual(rig.spawns[1].slice(0, 3), ["gh", "api", `repos/acme/widgets/releases/tags/${tag}`]);
  assert.equal(JSON.parse(fs.readFileSync(path.join(rig.outRoot, rig.id, "stage", "packed", "manifest.json"), "utf8")).repository, "acme/widgets");
  assert.match(rig.printed.join("\n"), /Add this to release\/approved\.json \(.*promote\.yml then promotes it/);
  assert.ok(!rig.printed.join("\n").includes("ci/approved-artifacts.json"));
  assert.ok(rig.printed[0].includes("(acme/widgets)"));
  assert.equal(result.candidate.id, rig.id);
});

test("driver for another repository refuses before anything runs when its config or checkout lacks what qualification needs", async (t) => {
  const none = driverRig(t); none.deps.git = gitFake({ origin: "https://github.com/acme/widgets.git" });
  await rejected(runQualify(none.args, none.deps), /acme\/widgets has no \.rightkit-gate\.json/);
  assert.deepEqual([none.execs.length, none.spawns.length], [0, 0]);
  const noMac = driverRig(t); noMac.deps.git = gitFake({ origin: "https://github.com/acme/widgets.git" });
  fs.writeFileSync(path.join(noMac.root, GATE_CONFIG_FILE), JSON.stringify({ ...ACME_CONFIG, hosts: { win: { root: "D:/work/widgets" } } }));
  await rejected(runQualify(noMac.args, noMac.deps), /no checkout root for the macos host/);
  assert.deepEqual([noMac.execs.length, noMac.spawns.length], [0, 0]);
  const flagged = driverRig(t, { extra: ["--mac-root", "/m/flag"] }); flagged.deps.git = gitFake({ origin: "https://github.com/acme/widgets.git" });
  fs.writeFileSync(path.join(flagged.root, GATE_CONFIG_FILE), JSON.stringify({ ...ACME_CONFIG, hosts: { win: { root: "D:/work/widgets" } } }));
  await runQualify(flagged.args, flagged.deps);
  assert.match(flagged.execs[0].argv[1], /node \/m\/flag\/packages\/release\/local-qualify\.mjs host --root \/m\/flag /);
  const noScripts = driverRig(t); noScripts.deps.git = gitFake({ origin: "https://github.com/acme/widgets.git" });
  fs.writeFileSync(path.join(noScripts.root, GATE_CONFIG_FILE), JSON.stringify(ACME_CONFIG));
  delete noScripts.deps.loadReviewed;
  await rejected(runQualify(noScripts.args, noScripts.deps), /has no scripts\/ci\/reviewed-candidate\.mjs; local qualification runs the repository's own CI scripts/);
  assert.equal(noScripts.execs.length, 0);
  const noOrigin = driverRig(t); noOrigin.deps.git = gitFake({ origin: "" });
  await rejected(runQualify(noOrigin.args, noOrigin.deps), /cannot derive owner\/name/);
});

test("a host measures the runtime proof for the repository the driver names, else for its own origin", async (t) => {
  const seen = [];
  const measureRuntime = async (options) => { seen.push(options.repository); return { schema: 1, kind: "local-runtime-crate", crate: options.crate, manifestSha256: null, workloads: [{ manifestSha256: null, artifactDigest: null }] }; };
  const named = hostRig(t, { options: { crates: ["rightkit-logs"], canonical: true, repository: "acme/widgets" }, deps: { measureRuntime } });
  await qualifyHost(named.options, named.deps);
  const derived = hostRig(t, { options: { crates: ["rightkit-logs"], canonical: true }, deps: { measureRuntime } });
  await qualifyHost(derived.options, derived.deps);
  assert.deepEqual(seen, ["acme/widgets", "bogusyogi/rightkit"]);
  const lost = hostRig(t, { options: { crates: ["rightkit-logs"] }, git: { origin: "" }, deps: { measureRuntime } });
  await rejected(qualifyHost(lost.options, lost.deps), /cannot derive owner\/name from the origin remote; pass --repository owner\/name/);
});

// ---- --with-ci: the GitHub qualification that rides with the local one ----
const CI_RUN_ID = 7001; const CI_WORKFLOW_ID = 4242;
/** A fake `gh`: records every call, answers `run list` from a state the dispatch changes, and answers the rest like the release rig. */
function ghFake({ before = [], created = "2026-10-22T10:00:05Z", delayPolls = 0, extra = [], failDispatch = false, failList = false, headSha = COMMIT, branch = "main", event = "workflow_dispatch", log = [] } = {}) {
  const state = { dispatched: false, polls: 0, calls: [] };
  const rows = (list) => JSON.stringify(list);
  const spawn = (command, argv) => {
    state.calls.push([command, ...argv]);
    if (command === "gh" && argv[0] === "run" && argv[1] === "list") {
      if (failList) return { status: 1, stdout: "", stderr: "HTTP 502" };
      const old = before.map((databaseId) => ({ databaseId, headSha: COMMIT, headBranch: "main", event: "workflow_dispatch", createdAt: "2026-10-21T00:00:00Z", workflowDatabaseId: CI_WORKFLOW_ID, status: "completed" }));
      if (!state.dispatched) return { status: 0, stdout: rows(old), stderr: "" };
      state.polls += 1;
      if (state.polls <= delayPolls) return { status: 0, stdout: rows(old), stderr: "" };
      const fresh = [{ databaseId: CI_RUN_ID, headSha, headBranch: branch, event, createdAt: created, workflowDatabaseId: CI_WORKFLOW_ID, status: "queued" }, ...extra];
      return { status: 0, stdout: rows([...fresh, ...old]), stderr: "" };
    }
    if (command === "gh" && argv[0] === "workflow" && argv[1] === "run") {
      if (failDispatch) return { status: 1, stdout: "", stderr: "HTTP 422: Workflow does not have 'workflow_dispatch' trigger" };
      state.dispatched = true; log.push("ci dispatched"); return { status: 0, stdout: "", stderr: "" };
    }
    return undefined;
  };
  return { spawn, state };
}
const noSleep = async () => {}; const NOW = "2026-10-22T10:00:00Z";

test("--with-ci is a flag of the driver only, and it needs both hosts because the pin rides in a candidate", () => {
  assert.equal(parseQualifyArgs(["cargo", "--crates", "rightkit-a", "--with-ci"], "win32").withCi, true);
  assert.equal(parseQualifyArgs(["cargo", "--crates", "rightkit-a"], "win32").withCi, false);
  refused(() => parseQualifyArgs(["cargo", "--crates", "rightkit-a", "--with-ci", "--hosts", "win"], "win32"), /--with-ci rides with a candidate, which needs both hosts/);
  assert.equal(parseQualifyArgs(["cargo", "--crates", "rightkit-a", "--with-ci", "--hosts", "mac,win"], "win32").withCi, true);
});

test("a Linux-required unit is planned only under --with-ci, and nothing else is relaxed by it", (t) => {
  const linux = workspace(t, { crates: ["rightkit-a", "rightkit-b"], linux: ["rightkit-a"] });
  const plan = (over = {}) => planUnits({ root: linux.root, crates: ["rightkit-a", "rightkit-b"], readJson: linux.readJson, readPackage: linux.readPackage, dependsOn: () => [], ...over });
  refused(() => plan(), /rightkit-a requires a Linux receipt.*--with-ci/);
  const withCi = plan({ withCi: true });
  assert.deepEqual(withCi.units.map((unit) => unit.name), ["rightkit-a", "rightkit-b"]); assert.deepEqual(withCi.ciRequired, ["rightkit-a"]);
  assert.deepEqual(plan({ crates: ["rightkit-b"], withCi: true }).ciRequired, [], "units local qualification proves need no run");
  // A runtime owner with no trusted local workload stays refused even with --with-ci: publish.yml has no runtime workload, so its success is no runtime proof.
  const runtime = workspace(t, { crates: ["rightkit-fsindex", "rightkit-logs"] });
  const owners = (over = {}) => planUnits({ root: runtime.root, crates: ["rightkit-fsindex", "rightkit-logs"], readJson: runtime.readJson, readPackage: runtime.readPackage, dependsOn: () => [], runtimeCoverage: ["rightkit-logs"], ...over });
  refused(() => owners(), /rightkit-fsindex is a continuous runtime owner with no trusted runtime coverage/);
  refused(() => owners({ withCi: true }), /rightkit-fsindex is a continuous runtime owner with no trusted runtime coverage.*--with-ci does not help/);
  assert.deepEqual(owners({ crates: ["rightkit-logs"], withCi: true }).ciRequired, [], "a covered owner is proven locally on both hosts and needs no run");
  // A covered owner still proves itself locally on both hosts; a run does not excuse that.
  const macOnly = workspace(t, { crates: ["rightkit-logs"], gate: { "rightkit-logs": { os: ["macos"] } } });
  refused(() => planUnits({ root: macOnly.root, crates: ["rightkit-logs"], readJson: macOnly.readJson, readPackage: macOnly.readPackage, dependsOn: () => [], runtimeCoverage: ["rightkit-logs"], withCi: true }), /needs its runtime proof on both native hosts/);
  // Nothing else relaxes: a unit packed away from macOS still refuses.
  const winOnly = workspace(t, { gate: { "rightkit-a": { os: ["windows"] } } });
  refused(() => planUnits({ root: winOnly.root, crates: ["rightkit-a"], readJson: winOnly.readJson, readPackage: winOnly.readPackage, dependsOn: () => [], withCi: true }), /crates are packed on macOS/);
});

test("the dispatch starts publish.yml for exactly these units on main and pins the run that appeared after it", async (t) => {
  const lockDir = temp(t); const log = []; const gh = ghFake({ before: [6000, 6001], delayPolls: 2 });
  const slept = []; const ci = await dispatchCi({ repository: "bogusyogi/rightkit", sourceSha: COMMIT, units: ["rightkit-a", "rightkit-b"], allow: { hostExclusion: ["rightkit-b"], ignored: ["rightkit-a"], missingAssets: [] }, lockDir, deps: { spawn: gh.spawn, sleep: async (ms) => { slept.push(ms); }, ciPollMs: 10, now: () => NOW }, log: (line) => log.push(line) });
  assert.deepEqual(ci, { workflowId: String(CI_WORKFLOW_ID), runId: String(CI_RUN_ID), sourceSha: COMMIT, units: ["rightkit-a", "rightkit-b"], dispatchedAt: "2026-10-22T10:00:00Z" });
  const dispatch = gh.state.calls.find((call) => call[1] === "workflow");
  assert.deepEqual(dispatch, ["gh", "workflow", "run", "publish.yml", "-R", "bogusyogi/rightkit", "--ref", "main", "-f", "registry=crates", "-f", "units=rightkit-a rightkit-b", "-f", "allow_host_exclusion=rightkit-b", "-f", "allow_ignored=rightkit-a"]);
  assert.deepEqual(ciDispatchArgs({ repository: "o/r", units: ["rightkit-a"] }), ["workflow", "run", "publish.yml", "-R", "o/r", "--ref", "main", "-f", "registry=crates", "-f", "units=rightkit-a"]);
  const lists = gh.state.calls.filter((call) => call[1] === "run");
  assert.ok(lists.length >= 3 && lists.every((call) => call.includes("--commit") && call[call.indexOf("--commit") + 1] === COMMIT && call.includes("--branch") && call.includes("workflow_dispatch")), "runs are looked up by workflow, branch, event and commit");
  assert.equal(gh.state.calls.indexOf(dispatch), 1, "the baseline listing comes before the dispatch");
  assert.deepEqual(slept, [10, 10, 10], "the poll is bounded and waits between listings");
  assert.match(log.join("\n"), /ci: publish\.yml run 7001 \(workflow 4242\) dispatched for aaaaaaaaaaaa: rightkit-a rightkit-b/);
  assert.equal(fs.existsSync(path.join(lockDir, ".ci-dispatch.lock")), false, "the capture lock is released");
  reviewed.validateCandidate(clone({ id: "local-a-aaaaaa", source: "local", sourceSha: COMMIT, registry: "crates", units: ["rightkit-a", "rightkit-b"], allow: { hostExclusion: ["rightkit-b"], noTest: [], ignored: ["rightkit-a"], missingAssets: [] }, release: { tag: "candidate-local-a-aaaaaa" }, assets: Object.fromEntries(["packed", "macos", "windows"].map((kind) => [kind, { name: reviewed.localAssetName("local-a-aaaaaa", kind), sha256: "1".repeat(64) }])), ci }));
});

test("the dispatch never guesses: an unrelated, older, foreign-branch or other-commit run is not ours, two new runs are ambiguous, and a run that never appears is a bounded refusal", async (t) => {
  const run = (over, deps = {}) => dispatchCi({ repository: "bogusyogi/rightkit", sourceSha: COMMIT, units: ["rightkit-a"], lockDir: temp(t), deps: { sleep: noSleep, ciPollMs: 1, ciPolls: 3, now: () => NOW, ...ghFake(over), ...deps }, log: () => {} });
  const ours = await run({}); assert.equal(ours.runId, String(CI_RUN_ID));
  await rejected(run({ created: "2026-10-01T00:00:00Z" }), /did not appear within/);
  await rejected(run({ branch: "dev" }), /did not appear within/);
  await rejected(run({ headSha: "b".repeat(40) }), /did not appear within/);
  await rejected(run({ event: "push" }), /did not appear within/);
  await rejected(run({ delayPolls: 99 }), /did not appear within 0s/);
  await rejected(run({ extra: [{ databaseId: 7002, headSha: COMMIT, headBranch: "main", event: "workflow_dispatch", createdAt: "2026-10-22T10:00:06Z", workflowDatabaseId: CI_WORKFLOW_ID, status: "queued" }] }), /2 new publish\.yml runs appeared for aaaaaaaaaaaa \(7001, 7002\).*cancel the extras/);
  await rejected(run({ failDispatch: true }), /ci: gh workflow run failed:.*workflow_dispatch/s);
  await rejected(run({ failList: true }), /ci: gh run list failed:.*502/s);
  const text = ghFake({}); await rejected(dispatchCi({ repository: "o/r", sourceSha: COMMIT, units: ["rightkit-a"], lockDir: temp(t), deps: { sleep: noSleep, now: () => NOW, spawn: (command, argv) => (argv[1] === "list" ? { status: 0, stdout: "not json", stderr: "" } : text.spawn(command, argv)) }, log: () => {} }), /did not print JSON/);
  const noId = ghFake({}); const noIdSpawn = (command, argv) => { const out = noId.spawn(command, argv); return argv[1] === "list" ? { ...out, stdout: out.stdout.replaceAll(`"workflowDatabaseId":${CI_WORKFLOW_ID}`, '"workflowDatabaseId":null') } : out; };
  await rejected(dispatchCi({ repository: "o/r", sourceSha: COMMIT, units: ["rightkit-a"], lockDir: temp(t), deps: { sleep: noSleep, now: () => NOW, spawn: noIdSpawn, ciPollMs: 1, ciPolls: 3 }, log: () => {} }), /no run or workflow id/);
});

test("batches in flight on one machine take turns only for the capture window: a held lock waits, a stale one is taken over, and the runs themselves are never serialised", async (t) => {
  const lockDir = temp(t); const lock = path.join(lockDir, ".ci-dispatch.lock"); fs.writeFileSync(lock, "999 held\n");
  let waits = 0; const gh = ghFake({});
  const sleep = async () => { waits += 1; if (waits === 2) fs.unlinkSync(lock); };
  const ci = await dispatchCi({ repository: "bogusyogi/rightkit", sourceSha: COMMIT, units: ["rightkit-a"], lockDir, deps: { spawn: gh.spawn, sleep, ciPollMs: 1, now: () => NOW }, log: () => {} });
  assert.equal(ci.runId, String(CI_RUN_ID)); assert.ok(waits >= 2, "waited for the holder"); assert.equal(fs.existsSync(lock), false);
  fs.writeFileSync(lock, "999 stale\n"); const old = new Date(Date.now() - 10 * 60 * 1000); fs.utimesSync(lock, old, old);
  const again = await dispatchCi({ repository: "bogusyogi/rightkit", sourceSha: COMMIT, units: ["rightkit-a"], lockDir, deps: { ...ghFake({}), sleep: noSleep, ciPollMs: 1, now: () => NOW }, log: () => {} });
  assert.equal(again.runId, String(CI_RUN_ID)); assert.equal(fs.existsSync(lock), false);
  fs.writeFileSync(lock, "999 held\n"); let tries = 0;
  await rejected(dispatchCi({ repository: "o/r", sourceSha: COMMIT, units: ["rightkit-a"], lockDir, deps: { ...ghFake({}), now: () => NOW, sleep: async () => { tries += 1; } }, log: () => {} }), /held .*ci-dispatch\.lock for two minutes/);
  assert.equal(tries, 120); assert.equal(fs.existsSync(lock), true, "a lock this process did not take is never removed");
  // The lock is released on failure too.
  fs.unlinkSync(lock);
  await rejected(dispatchCi({ repository: "o/r", sourceSha: COMMIT, units: ["rightkit-a"], lockDir, deps: { ...ghFake({ failDispatch: true }), now: () => NOW, sleep: noSleep }, log: () => {} }), /ci: gh workflow run failed/);
  assert.equal(fs.existsSync(lock), false);
});

// The driver with --with-ci: the release rig plus a fake gh for the run list and the dispatch.
function ciRig(t, { platform = "win32", extra = [], over = {}, gh = {}, linux = [], ci = true } = {}) {
  const rig = driverRig(t, { platform, extra: [...(ci ? ["--with-ci"] : []), ...extra], over });
  const fake = ghFake({ ...gh, log: rig.log }); const releaseSpawn = rig.deps.spawn; rig.deps.spawn = (command, argv) => fake.spawn(command, argv) ?? releaseSpawn(command, argv);
  rig.deps.sleep = noSleep; rig.deps.ciPollMs = 1; rig.deps.now = () => NOW;
  const readJson = rig.deps.readJson; rig.deps.readJson = (file) => (/linux-required\.json$/.test(file) ? { schema: 1, npm: [], crates: linux } : readJson(file));
  return { ...rig, gh: fake };
}

test("driver --with-ci: both hosts and the GitHub run start together, the run is pinned in the candidate, and nothing else about the candidate changes", async (t) => {
  const rig = ciRig(t); const result = await runQualify(rig.args, rig.deps);
  const first = rig.log.indexOf("ci dispatched"); assert.ok(first >= 0 && first < rig.log.indexOf("win end") && first < rig.log.indexOf("mac end"), "the run is dispatched before either host finishes");
  assert.deepEqual(new Set(rig.log.filter((line) => /start$/.test(line))), new Set(["win start", "mac start"]));
  const dispatch = rig.gh.state.calls.find((call) => call[1] === "workflow");
  assert.deepEqual(dispatch, ["gh", "workflow", "run", "publish.yml", "-R", "bogusyogi/rightkit", "--ref", "main", "-f", "registry=crates", "-f", "units=rightkit-a rightkit-b"]);
  assert.deepEqual(result.candidate.ci, { workflowId: String(CI_WORKFLOW_ID), runId: String(CI_RUN_ID), sourceSha: COMMIT, units: ["rightkit-a", "rightkit-b"], dispatchedAt: "2026-10-22T10:00:00Z" });
  assert.deepEqual(Object.keys(result.candidate), ["id", "source", "sourceSha", "registry", "units", "allow", "release", "assets", "ci"]);
  reviewed.validateCandidate(clone(result.candidate)); assert.deepEqual(result.document.approval.candidates[0].ci, result.candidate.ci);
  assert.equal(JSON.parse(fs.readFileSync(path.join(rig.outRoot, rig.id, "candidate.json"), "utf8")).candidate.ci.runId, "7001");
  assert.match(rig.printed.join("\n"), /ci: run 7001 is pinned in the candidate; promotion admits it as clean-room proof when it lands \(pending proceeds on the local bytes, a failure needs allow\.ciFailure\)\. Watch it: gh run watch 7001 -R bogusyogi\/rightkit/);
  assert.ok(rig.spawns.some((call) => call[1] === "release" && call[2] === "create"), "the prerelease is still created and read back");
  assert.equal(rig.execs.length, 2, "the other host is driven exactly as without --with-ci");
  // Without the flag nothing about GitHub runs is touched and the candidate has no pin.
  const plain = ciRig(t, { ci: false }); const plainResult = await runQualify(plain.args, plain.deps);
  assert.equal(plain.gh.state.calls.filter((call) => call[1] === "workflow" || call[1] === "run").length, 0, "no GitHub run is listed or dispatched without the flag"); assert.equal(Object.hasOwn(plainResult.candidate, "ci"), false);
});

test("driver --with-ci on the Mac: the same pin, and the dispatch comes from the Mac's own gh", async (t) => {
  const rig = ciRig(t, { platform: "darwin" }); const result = await runQualify(rig.args, rig.deps);
  assert.equal(result.candidate.ci.runId, String(CI_RUN_ID)); assert.equal(rig.execs[0].command, "win"); assert.ok(rig.gh.state.calls.some((call) => call[1] === "workflow"));
});

test("driver --with-ci: a Linux-required unit is qualified only with the run, which becomes its Linux proof; the allowsets the plan carries ride in the dispatch", async (t) => {
  const refusedRig = ciRig(t, { ci: false, linux: ["rightkit-a"] }); await rejected(runQualify(refusedRig.args, refusedRig.deps), /rightkit-a requires a Linux receipt.*--with-ci/);
  assert.equal(refusedRig.execs.length, 0); assert.equal(refusedRig.gh.state.calls.length, 0);
  const rig = ciRig(t, { linux: ["rightkit-a"], extra: ["--allow-ignored-units", "rightkit-b"] });
  const result = await runQualify(rig.args, rig.deps);
  assert.deepEqual(rig.gh.state.calls.find((call) => call[1] === "workflow").slice(-4), ["-f", "units=rightkit-a rightkit-b", "-f", "allow_ignored=rightkit-b"]);
  assert.equal(result.candidate.ci.units.length, 2); assert.equal(Object.hasOwn(result.candidate.assets, "linux"), false, "no local Linux receipt exists to pin");
  assert.match(rig.printed.join("\n"), /its success is REQUIRED for rightkit-a/);
  // The checkout's own promotion code accepts it only with the pin: the same candidate without `ci` needs a Linux receipt it cannot have.
  const linuxFile = path.join(temp(t), "linux-required.json"); fs.writeFileSync(linuxFile, JSON.stringify({ schema: 1, npm: [], crates: ["rightkit-a"] }));
  reviewed.validateCandidate(clone(result.candidate), { linuxFile });
  const stripped = clone(result.candidate); delete stripped.ci; assert.throws(() => reviewed.validateCandidate(stripped, { linuxFile }), /refused/);
});

test("driver --with-ci refusals: HEAD behind origin/main, a failed dispatch, an ambiguous run and a failed host all stop before a candidate exists", async (t) => {
  const behind = ciRig(t); behind.deps.git = gitFake({ remote: "b".repeat(40) }); await rejected(runQualify(behind.args, behind.deps), /--with-ci: HEAD aaaaaaaaaaaa is not origin\/main's tip \(bbbbbbbbbbbb\).*qualify from the tip/);
  assert.equal(behind.execs.length, 0); assert.equal(behind.gh.state.calls.length, 0, "no run is started for other bytes than the hosts qualify");
  const failing = ciRig(t, { gh: { failDispatch: true } }); await rejected(runQualify(failing.args, failing.deps), /qualification failed:\nci: gh workflow run failed/);
  assert.equal(failing.spawns.filter((call) => call[1] === "release").length, 0, "no prerelease without the run it pins");
  const ambiguous = ciRig(t, { gh: { extra: [{ databaseId: 7002, headSha: COMMIT, headBranch: "main", event: "workflow_dispatch", createdAt: "2026-10-22T10:00:06Z", workflowDatabaseId: CI_WORKFLOW_ID, status: "queued" }] } });
  await rejected(runQualify(ambiguous.args, ambiguous.deps), /2 new publish\.yml runs appeared/);
  const hostFails = ciRig(t, { over: { remoteFail: true } }); await rejected(runQualify(hostFails.args, hostFails.deps), /qualification failed:\nmac: remote qualification failed/);
  assert.ok(hostFails.log.includes("ci dispatched"), "the run was already started; it is logged with its id so it can be cancelled");
  assert.equal(hostFails.spawns.filter((call) => call[1] === "release").length, 0);
});
