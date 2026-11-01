// Item 7 row 7.21 (Sage decision A): the gate's own record of the broker it trusts. Pins come from the consumer's enrollment, never
// from the receipt, the request, the project, an environment variable or a flag; the RightKit fallback is the running gate's own
// revision of ci/rk-pipeline.json; an app repository without an enrollment compiles nothing through a broker.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { BrokerPinsError, PINS_SCHEMA, enrollBrokerPins, loadBrokerPins, main, pinsFile, readEnrolledPins, readGateRevisionPins, validatePins } from "./broker-pins.mjs";
import { createSnapshotBroker } from "./snapshot-broker.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";
import { loadVector } from "./test-support/vectors.mjs";
import { signedBytes, verifyProvenance } from "./provenance-verify.mjs";

/** A real P-256 broker key and the pins an enrollment of it records (the key RustRight's attestation signs with). */
function keyPair() {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ec", { namedCurve: "P-256" });
  const jwk = publicKey.export({ format: "jwk" });
  const sec1 = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]).toString("hex");
  return { privateKey, pins: { publicKey: sec1, keyId: crypto.createHash("sha256").update(Buffer.from(sec1, "hex")).digest("hex"), host: "host-1", brokerDigest: "b".repeat(64) } };
}
const subHandleStub = (handle) => (callHandle) => ({ handle, broker: { submitted: true, handles: [callHandle] } });

const POSIX = process.platform === "win32" ? "POSIX file modes" : false;
const tmp = (t) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-pins-"))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
const git = (cwd, ...args) => spawnSync("git", args, { cwd, encoding: "utf8" });
const platformKey = process.platform === "win32" ? "win32" : "darwin";

test("[7.21] enroll writes one owner-only record under the managed data root; a second enrollment is a rotation and must say so", (t) => {
  const dataRoot = tmp(t); const { pins } = keyPair(); const other = keyPair().pins;
  assert.equal(pinsFile({ dataRoot, platform: "darwin" }), path.join(dataRoot, "pins", "broker-darwin.json"));
  assert.equal(readEnrolledPins({ dataRoot, platform: "darwin" }), null, "nothing is enrolled until someone enrolls");
  const done = enrollBrokerPins({ dataRoot, platform: "darwin", ...pins, now: () => new Date("2026-10-29T00:00:00Z") });
  assert.equal(done.rotated, false); assert.equal(done.record.schema, PINS_SCHEMA);
  if (!POSIX) { assert.equal(fs.statSync(done.file).mode & 0o777, 0o600); assert.equal(fs.statSync(path.dirname(done.file)).mode & 0o777, 0o700); }
  const read = readEnrolledPins({ dataRoot, platform: "darwin" });
  assert.deepEqual({ host: read.host, keyId: read.keyId, publicKey: read.publicKey, brokerDigest: read.brokerDigest, source: read.source }, { ...pins, source: "enrolled" });
  // No silent replacement.
  assert.throws(() => enrollBrokerPins({ dataRoot, platform: "darwin", ...other }), (e) => e instanceof BrokerPinsError && e.code === "RK_BROKER_PINS_EXISTS");
  assert.equal(readEnrolledPins({ dataRoot, platform: "darwin" }).keyId, pins.keyId, "the failed enrollment changed nothing");
  const rotated = enrollBrokerPins({ dataRoot, platform: "darwin", ...other, rotate: true });
  assert.equal(rotated.rotated, true); assert.deepEqual(rotated.record.rotatedFrom, { keyId: pins.keyId, brokerDigest: pins.brokerDigest });
  assert.equal(readEnrolledPins({ dataRoot, platform: "darwin" }).keyId, other.keyId);
  // Platforms are separate records; one enrollment never answers for another platform.
  assert.equal(readEnrolledPins({ dataRoot, platform: "win32" }), null);
  assert.equal(readEnrolledPins({ dataRoot, platform: "linux" }), null);
  // Only the three broker platforms have a record; anything else is not a broker host.
  for (const platform of ["freebsd", "aix", ""]) assert.throws(() => pinsFile({ dataRoot, platform }), (e) => e.code === "RK_BROKER_PINS_PLATFORM" && /not a broker host/.test(e.message), platform);
});

test("[7.21] linux is a broker platform (a broker is allowed on Linux): enroll and read round trip under pins/broker-linux.json, separate from darwin and win32", (t) => {
  const dataRoot = tmp(t); const { pins } = keyPair(); const other = keyPair().pins;
  assert.equal(pinsFile({ dataRoot, platform: "linux" }), path.join(dataRoot, "pins", "broker-linux.json"));
  assert.equal(readEnrolledPins({ dataRoot, platform: "linux" }), null, "nothing is enrolled until someone enrolls");
  const done = enrollBrokerPins({ dataRoot, platform: "linux", ...pins, now: () => new Date("2026-10-31T00:00:00Z") });
  assert.equal(done.rotated, false); assert.equal(done.record.platform, "linux"); assert.equal(done.record.schema, PINS_SCHEMA);
  if (process.platform !== "win32") { assert.equal(fs.statSync(done.file).mode & 0o777, 0o600); assert.equal(fs.statSync(path.dirname(done.file)).mode & 0o777, 0o700); }
  const read = readEnrolledPins({ dataRoot, platform: "linux" });
  assert.deepEqual({ host: read.host, keyId: read.keyId, publicKey: read.publicKey, brokerDigest: read.brokerDigest, source: read.source }, { ...pins, source: "enrolled" });
  assert.equal(loadBrokerPins({ dataRoot, platform: "linux" }).keyId, pins.keyId, "the load path returns the enrolled linux record");
  // The linux record is its own: darwin and win32 have none, and the same refusals as darwin apply (no silent replacement, rotation says so).
  assert.equal(readEnrolledPins({ dataRoot, platform: "darwin" }), null); assert.equal(readEnrolledPins({ dataRoot, platform: "win32" }), null);
  assert.throws(() => enrollBrokerPins({ dataRoot, platform: "linux", ...other }), (e) => e.code === "RK_BROKER_PINS_EXISTS");
  const rotated = enrollBrokerPins({ dataRoot, platform: "linux", ...other, rotate: true });
  assert.equal(rotated.rotated, true); assert.deepEqual(rotated.record.rotatedFrom, { keyId: pins.keyId, brokerDigest: pins.brokerDigest });
  // Without any enrollment and outside the RightKit checkout the declared route refuses; nothing is enrolled by the gate.
  const empty = tmp(t);
  assert.throws(() => loadBrokerPins({ dataRoot: empty, platform: "linux" }), (e) => e.code === "RK_BROKER_PINS_MISSING" && /no broker enrollment for linux/.test(e.message));
});

test("[7.21] a linux record is refused under a darwin read and a darwin or win32 record under a linux read (a record is never read as another platform's)", { skip: POSIX }, (t) => {
  const dataRoot = tmp(t); const { pins } = keyPair();
  const linux = enrollBrokerPins({ dataRoot, platform: "linux", ...pins });
  const darwin = enrollBrokerPins({ dataRoot, platform: "darwin", ...pins });
  const win = enrollBrokerPins({ dataRoot, platform: "win32", ...pins });
  const swap = (from, to) => { const text = JSON.parse(fs.readFileSync(from.file, "utf8")); fs.writeFileSync(to.file, JSON.stringify(text)); };
  swap(linux, darwin); // pins/broker-darwin.json now claims to be linux
  assert.throws(() => readEnrolledPins({ dataRoot, platform: "darwin" }), (e) => e.code === "RK_BROKER_PINS_INVALID" && /record of linux, not darwin/.test(e.message));
  assert.throws(() => loadBrokerPins({ dataRoot, platform: "darwin" }), (e) => e.code === "RK_BROKER_PINS_INVALID", "a mismatched record is a refusal, never a missing one that falls back");
  enrollBrokerPins({ dataRoot, platform: "darwin", ...pins, rotate: true }); // repaired
  swap(darwin, linux); // pins/broker-linux.json now claims to be darwin
  assert.throws(() => readEnrolledPins({ dataRoot, platform: "linux" }), (e) => e.code === "RK_BROKER_PINS_INVALID" && /record of darwin, not linux/.test(e.message));
  swap(win, linux); // ... or win32
  assert.throws(() => readEnrolledPins({ dataRoot, platform: "linux" }), (e) => e.code === "RK_BROKER_PINS_INVALID" && /record of win32, not linux/.test(e.message));
  assert.equal(readEnrolledPins({ dataRoot, platform: "darwin" }).keyId, pins.keyId, "the darwin record itself is unaffected");
});

test("[7.21] a linux record that is a link, a non-file, writable by others or behind a linked directory is refused exactly like darwin", { skip: POSIX }, (t) => {
  const dataRoot = tmp(t); const { pins } = keyPair();
  const { file } = enrollBrokerPins({ dataRoot, platform: "linux", ...pins });
  const refuse = (why) => assert.throws(() => readEnrolledPins({ dataRoot, platform: "linux" }), (e) => e.code === "RK_BROKER_PINS_INVALID" && why.test(e.message));
  fs.chmodSync(file, 0o666); refuse(/writable by group or others/); fs.chmodSync(file, 0o600);
  fs.chmodSync(path.dirname(file), 0o777); refuse(/writable by group or others/); fs.chmodSync(path.dirname(file), 0o700);
  const text = fs.readFileSync(file, "utf8");
  fs.rmSync(file); const real = path.join(dataRoot, "real.json"); fs.writeFileSync(real, text, { mode: 0o600 }); fs.symlinkSync(real, file); refuse(/not a regular file/);
  fs.rmSync(file); fs.mkdirSync(file); refuse(/not a regular file/); fs.rmSync(file, { recursive: true });
  fs.rmSync(path.join(dataRoot, "pins"), { recursive: true });
  fs.mkdirSync(path.join(dataRoot, "elsewhere", "d"), { recursive: true }); fs.symlinkSync(path.join(dataRoot, "elsewhere", "d"), path.join(dataRoot, "pins"));
  fs.writeFileSync(path.join(dataRoot, "elsewhere", "d", "broker-linux.json"), text, { mode: 0o600 });
  assert.throws(() => readEnrolledPins({ dataRoot, platform: "linux" }), /not a real directory/, "a record behind a linked directory is refused");
});

test("[7.21] a pin that is not a well-formed enrollment is refused: key id not the key's hash, compressed or wrong-length key, bad digest, no host", (t) => {
  const dataRoot = tmp(t); const { pins } = keyPair();
  const bad = [
    [{ keyId: "0".repeat(64) }, /keyId is not the sha256/], [{ publicKey: `02${pins.publicKey.slice(2, 66)}` }, /uncompressed SEC1/], [{ publicKey: pins.publicKey.slice(0, 128) }, /uncompressed SEC1/],
    [{ publicKey: pins.publicKey.toUpperCase() }, /uncompressed SEC1/], [{ brokerDigest: "bd" }, /brokerDigest is not a sha256/], [{ host: " " }, /host is missing/], [{ host: undefined }, /host is missing/],
  ];
  for (const [over, why] of bad) {
    assert.throws(() => enrollBrokerPins({ dataRoot, platform: "darwin", ...pins, ...over }), (e) => e.code === "RK_BROKER_PINS_INVALID" && why.test(e.message), JSON.stringify(over));
    assert.equal(fs.existsSync(pinsFile({ dataRoot, platform: "darwin" })), false, "nothing was written");
  }
  assert.throws(() => validatePins(null), /not an object/);
});

test("[7.21] a record that is a link, writable by others, another platform's, not JSON or not this schema is refused, never read as missing", { skip: POSIX }, (t) => {
  const dataRoot = tmp(t); const { pins } = keyPair();
  const { file } = enrollBrokerPins({ dataRoot, platform: "darwin", ...pins });
  const refuse = (why) => assert.throws(() => readEnrolledPins({ dataRoot, platform: "darwin" }), (e) => e.code === "RK_BROKER_PINS_INVALID" && why.test(e.message));
  fs.chmodSync(file, 0o666); refuse(/writable by group or others/); fs.chmodSync(file, 0o600);
  const text = fs.readFileSync(file, "utf8");
  fs.writeFileSync(file, "not json"); refuse(/not JSON/);
  fs.writeFileSync(file, JSON.stringify({ ...JSON.parse(text), schema: "other" })); refuse(/is not a rightkit\.broker-pins\/1 record/);
  fs.writeFileSync(file, JSON.stringify({ ...JSON.parse(text), platform: "win32" })); refuse(/record of win32, not darwin/);
  fs.writeFileSync(file, JSON.stringify({ ...JSON.parse(text), keyId: "1".repeat(64) })); refuse(/keyId is not the sha256/);
  fs.rmSync(file); const real = path.join(dataRoot, "real.json"); fs.writeFileSync(real, text, { mode: 0o600 }); fs.symlinkSync(real, file); refuse(/not a regular file/);
  fs.rmSync(file); fs.rmSync(path.join(dataRoot, "pins"), { recursive: true });
  fs.mkdirSync(path.join(dataRoot, "elsewhere", "d"), { recursive: true }); fs.symlinkSync(path.join(dataRoot, "elsewhere", "d"), path.join(dataRoot, "pins"));
  assert.equal(readEnrolledPins({ dataRoot, platform: "darwin" }), null, "no record behind a linked directory");
  fs.writeFileSync(path.join(dataRoot, "elsewhere", "d", "broker-darwin.json"), text, { mode: 0o600 });
  assert.throws(() => readEnrolledPins({ dataRoot, platform: "darwin" }), /not a real directory/, "a record behind a linked directory is refused");
});

/** A fake RightKit checkout whose gate lives in it, with ci/rk-pipeline.json committed. */
function rightkitRepo(t, attestation) {
  const root = tmp(t);
  fs.mkdirSync(path.join(root, "ci")); fs.mkdirSync(path.join(root, "packages", "release", "bin"), { recursive: true });
  fs.writeFileSync(path.join(root, "ci", "rk-pipeline.json"), JSON.stringify({ schema: "rk-pipeline.v1", attestation }));
  const gateFile = path.join(root, "packages", "release", "bin", "rightkit-gate.mjs"); fs.writeFileSync(gateFile, "// gate\n");
  git(root, "init", "-q", "-b", "main"); git(root, "add", "-A"); git(root, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "c");
  return { root, gateFile };
}

test("[7.21] fallback: RightKit's own pins come from the running gate's revision of ci/rk-pipeline.json, never the working tree, and only in the RightKit checkout", (t) => {
  const dataRoot = tmp(t); const mine = keyPair().pins; const evil = keyPair().pins;
  const attestation = { darwin: { ...mine, note: "x" }, win32: { ...mine, note: "y" } };
  const { root, gateFile } = rightkitRepo(t, attestation);
  const read = loadBrokerPins({ dataRoot, platform: "darwin", gateFile, checkout: root });
  assert.equal(read.source, "rk-pipeline"); assert.equal(read.keyId, mine.keyId); assert.match(read.revision, /^[0-9a-f]{40}$/);
  // The working tree (and so any later or selected commit's edit) changes nothing: the pins are the gate's committed revision.
  fs.writeFileSync(path.join(root, "ci", "rk-pipeline.json"), JSON.stringify({ schema: "rk-pipeline.v1", attestation: { darwin: evil, win32: evil } }));
  assert.equal(loadBrokerPins({ dataRoot, platform: "darwin", gateFile, checkout: root }).keyId, mine.keyId, "an edit of ci/rk-pipeline.json in the working tree enrolls nothing");
  // Another repository (an app) never gets the fallback, whatever its own ci/rk-pipeline.json says.
  const app = rightkitRepo(t, { darwin: evil });
  assert.throws(() => loadBrokerPins({ dataRoot, platform: "darwin", gateFile, checkout: app.root }), (e) => e.code === "RK_BROKER_PINS_MISSING" && /enroll the host's broker explicitly/.test(e.message));
  assert.throws(() => loadBrokerPins({ dataRoot, platform: "darwin", gateFile: path.join(app.root, "packages", "release", "bin", "rightkit-gate.mjs"), checkout: root }), (e) => e.code === "RK_BROKER_PINS_MISSING", "the gate is the app's, the checkout RightKit's: no fallback");
  // A gate that is not in a git checkout (an installed package) has no revision to read: missing.
  const loose = tmp(t); fs.writeFileSync(path.join(loose, "rightkit-gate.mjs"), "//\n");
  assert.throws(() => loadBrokerPins({ dataRoot, platform: "darwin", gateFile: path.join(loose, "rightkit-gate.mjs"), checkout: root }), (e) => e.code === "RK_BROKER_PINS_MISSING");
  // A platform the file does not enroll, or an enrollment that is not well-formed, is not a pin.
  assert.equal(readGateRevisionPins({ gateFile, checkout: root, platform: "linux" }), null, "ci/rk-pipeline.json carries no attestation.linux: the fallback is null");
  assert.equal(readGateRevisionPins({ gateFile, checkout: root, platform: "freebsd" }), null);
  const broken = rightkitRepo(t, { darwin: { ...mine, keyId: "0".repeat(64) } });
  assert.throws(() => loadBrokerPins({ dataRoot, platform: "darwin", gateFile: broken.gateFile, checkout: broken.root }), (e) => e.code === "RK_BROKER_PINS_INVALID");
});

test("[7.21] linux has no gate-revision fallback unless ci/rk-pipeline.json carries attestation.linux; an enrolled linux record decides either way, and darwin or win32 entries never answer for linux", (t) => {
  const dataRoot = tmp(t); const mine = keyPair().pins; const enrolled = keyPair().pins; const viaPipeline = keyPair().pins;
  const without = rightkitRepo(t, { darwin: mine, win32: mine });
  assert.equal(readGateRevisionPins({ gateFile: without.gateFile, checkout: without.root, platform: "linux" }), null);
  assert.throws(() => loadBrokerPins({ dataRoot, platform: "linux", gateFile: without.gateFile, checkout: without.root }), (e) => e.code === "RK_BROKER_PINS_MISSING" && /no broker enrollment for linux/.test(e.message), "RightKit's own darwin/win32 pins are no linux pins");
  // Enrolled beats nothing: the record alone satisfies the declared route inside the RightKit checkout.
  enrollBrokerPins({ dataRoot, platform: "linux", ...enrolled });
  const only = loadBrokerPins({ dataRoot, platform: "linux", gateFile: without.gateFile, checkout: without.root });
  assert.equal(only.source, "enrolled"); assert.equal(only.keyId, enrolled.keyId);
  // Were a linux attestation ever committed, the enrolled record would still beat it; without enrollment it is the fallback.
  const withLinux = rightkitRepo(t, { darwin: mine, linux: viaPipeline });
  assert.equal(loadBrokerPins({ dataRoot, platform: "linux", gateFile: withLinux.gateFile, checkout: withLinux.root }).keyId, enrolled.keyId, "enrolled beats fallback");
  const fresh = tmp(t); const fb = loadBrokerPins({ dataRoot: fresh, platform: "linux", gateFile: withLinux.gateFile, checkout: withLinux.root });
  assert.equal(fb.source, "rk-pipeline"); assert.equal(fb.keyId, viaPipeline.keyId);
  // An app repository (not the RightKit checkout) never gets the fallback for linux either.
  const app = rightkitRepo(t, { linux: viaPipeline });
  assert.throws(() => loadBrokerPins({ dataRoot: fresh, platform: "linux", gateFile: withLinux.gateFile, checkout: app.root }), (e) => e.code === "RK_BROKER_PINS_MISSING");
});

test("[7.21] the enrolled record wins over the fallback; nothing in the environment, a flag or the receipt can supply a pin", async (t) => {
  const dataRoot = tmp(t); const mine = keyPair().pins; const enrolled = keyPair().pins;
  const { root, gateFile } = rightkitRepo(t, { darwin: mine, win32: mine });
  enrollBrokerPins({ dataRoot, platform: "darwin", ...enrolled });
  const before = loadBrokerPins({ dataRoot, platform: "darwin", gateFile, checkout: root });
  assert.equal(before.source, "enrolled"); assert.equal(before.keyId, enrolled.keyId);
  const names = ["RIGHTKIT_BROKER_PINS", "RIGHTKIT_BROKER_PUBLIC_KEY", "RIGHTKIT_BROKER_KEY_ID", "RIGHTKIT_BROKER_DIGEST", "RIGHTKIT_ATTEST_PUBLIC_KEY", "RUSTRIGHT_PINS"];
  const saved = Object.fromEntries(names.map((n) => [n, process.env[n]]));
  t.after(() => { for (const n of names) { if (saved[n] === undefined) delete process.env[n]; else process.env[n] = saved[n]; } });
  for (const n of names) process.env[n] = JSON.stringify({ publicKey: "04", keyId: "x" });
  assert.deepEqual(loadBrokerPins({ dataRoot, platform: "darwin", gateFile, checkout: root }), before, "no environment variable changes the pins");
  // The production factory has no pin parameter at all: pins handed to it are ignored, and a missing record refuses the compile.
  const rb = await startRealBroker({ name: "broker-pins" });
  t.after(() => rb.stop());
  const checkout = tmp(t); const snapshotDir = tmp(t); const commit = { sha: "c".repeat(40), tree: "e".repeat(40) };
  const hostPins = keyPair().pins;
  const own = { handle: "h", markBrokerSubmit: subHandleStub("h") };
  const make = (extra = {}) => createSnapshotBroker({ env: { RIGHTKIT_DATA_ROOT: rb.dataRoot, RIGHTKIT_CONTROL_ROOT: rb.controlRoot }, platform: platformKey, checkout, commit, snapshot: { path: snapshotDir, expectedTree: commit.tree }, ownership: own, client: rb.clientPath, gateFile: path.join(checkout, "x", "rightkit-gate.mjs"), ...extra });
  assert.throws(() => make({ pins: enrolled }), (e) => e.code === "RK_BROKER_PINS_MISSING", "an app repository with no enrollment compiles nothing; a pins argument does not help");
  enrollBrokerPins({ dataRoot: rb.dataRoot, platform: platformKey, ...hostPins });
  const sb = make({ pins: enrolled });
  assert.equal(sb.pins.keyId, hostPins.keyId, "the enrolled record decides, whatever was passed");
  assert.equal(sb.pinsSource, "enrolled"); assert.deepEqual(sb.outputRoots, [path.join(rb.dataRoot, "rightkit-build-control", "ws")]);
  assert.equal(sb.checkout, fs.realpathSync.native(checkout)); assert.equal(sb.handle, "h");
  assert.throws(() => make({ ownership: null }), /build ownership/); assert.throws(() => make({ client: "rustright" }), /by absolute path/);
});

test("[7.21] data: a receipt is checked against the enrolled pins only: another key, another broker digest, another host, a pin offered by the receipt, and a legacy receipt are each refused", (t) => {
  const vector = loadVector("receipt-vectors.v1.json"); // RustRight's own production-signed receipt and its pins (rustright-protocol tests/receipt_vectors.rs)
  const receipt = structuredClone(vector.cases.find((c) => c.name === "a successful signed result").receipt);
  const verify = (r, pins) => verifyProvenance(structuredClone(r), pins, vector.expected);
  const resign = (r, { key, mutate = () => {}, keyId = r.provenance.signature.keyId }) => {
    const out = structuredClone(r); const p = out.provenance.payload; p.keyId = keyId; mutate(p);
    out.provenance.signature = { alg: "ES256", keyId, value: crypto.sign("sha256", signedBytes(p), { key, dsaEncoding: "ieee-p1363" }).toString("hex") };
    return out;
  };
  assert.equal(verify(receipt, vector.pins).jobId, vector.expected.jobId, "the enrolled pins verify the broker's own receipt");
  // A changed pin: another enrolled key (a rotation the broker did not follow), or another runtime or host.
  const rotated = keyPair().pins;
  assert.throws(() => verify(receipt, { ...rotated, host: vector.pins.host, brokerDigest: vector.pins.brokerDigest }), (e) => e.code === "key");
  assert.throws(() => verify(receipt, { ...vector.pins, brokerDigest: "f".repeat(64) }), (e) => e.code === "runtime");
  assert.throws(() => verify(receipt, { ...vector.pins, host: "host-9" }), (e) => e.code === "host");
  // A receipt that claims the pinned key id but is signed by another key fails the signature.
  const other = keyPair();
  assert.throws(() => verify(resign(receipt, { key: other.privateKey }), vector.pins), (e) => e.code === "signature");
  // A receipt signed by a runtime other than the enrolled pin's: consistent under its own key, refused when pinned to another broker digest.
  const otherRuntime = resign(receipt, { key: other.privateKey, keyId: other.pins.keyId, mutate: (p) => { p.host = vector.pins.host; p.broker = { brokerDigest: "a".repeat(64) }; } });
  assert.throws(() => verify(otherRuntime, { ...other.pins, host: vector.pins.host, brokerDigest: vector.pins.brokerDigest }), (e) => e.code === "runtime");
  // A pin taken from the receipt: the receipt carries its own public key and key id; the verifier uses neither.
  const forged = resign(receipt, { key: other.privateKey, keyId: other.pins.keyId });
  forged.provenance.publicKey = other.pins.publicKey; forged.provenance.payload.publicKey = other.pins.publicKey; forged.publicKey = other.pins.publicKey;
  assert.throws(() => verify(forged, vector.pins), (e) => e.code === "key", "the receipt's own key is not a pin");
  // A receipt with no provenance at all (legacy) is never attested.
  const legacy = { ...receipt }; delete legacy.provenance;
  assert.throws(() => verify(legacy, vector.pins), (e) => e.code === "legacy");
});

test("[7.21] the enroll command: explicit values only, refuses to replace without --rotate, prints what it wrote", (t) => {
  const dataRoot = tmp(t); const { pins } = keyPair(); const out = []; const err = [];
  const run = (...argv) => main(argv, { out: (m) => out.push(m), err: (m) => err.push(m) });
  const enroll = ["enroll", "--platform", "darwin", "--data-root", dataRoot, "--host", pins.host, "--key-id", pins.keyId, "--public-key", pins.publicKey, "--broker-digest", pins.brokerDigest];
  assert.equal(run("show", "--platform", "darwin", "--data-root", dataRoot), 1); assert.match(out.pop(), /no enrollment/);
  assert.equal(run(...enroll), 0); assert.match(out.pop(), /enrolled darwin host-1 key [0-9a-f]{64}/);
  assert.equal(run(...enroll), 1); assert.match(err.pop(), /RK_BROKER_PINS_EXISTS/);
  assert.equal(run(...enroll, "--rotate"), 0); assert.match(out.pop(), /\(rotation\)/);
  assert.equal(run("show", "--platform", "darwin", "--data-root", dataRoot), 0); assert.equal(JSON.parse(out.pop()).keyId, pins.keyId);
  assert.equal(run("enroll", "--platform", "darwin", "--data-root", dataRoot, "--host", "h"), 1); assert.match(err.pop(), /RK_BROKER_PINS_INVALID/);
  assert.equal(run("nonsense"), 2);
  assert.equal(run("show", "--platform", "darwin"), 1); assert.match(err.pop(), /RK_BROKER_PINS_ROOT/);
});

test("[7.21] the enroll command accepts --platform linux, and a platform that is not a broker host is refused with the platform code", (t) => {
  const dataRoot = tmp(t); const { pins } = keyPair(); const out = []; const err = [];
  const run = (...argv) => main(argv, { out: (m) => out.push(m), err: (m) => err.push(m) });
  const enroll = (platform) => ["enroll", "--platform", platform, "--data-root", dataRoot, "--host", pins.host, "--key-id", pins.keyId, "--public-key", pins.publicKey, "--broker-digest", pins.brokerDigest];
  assert.equal(run("show", "--platform", "linux", "--data-root", dataRoot), 1); assert.match(out.pop(), /no enrollment/);
  assert.equal(run(...enroll("linux")), 0); assert.match(out.pop(), /enrolled linux host-1 key [0-9a-f]{64} in .*broker-linux\.json/);
  assert.equal(run("show", "--platform", "linux", "--data-root", dataRoot), 0); assert.equal(JSON.parse(out.pop()).keyId, pins.keyId);
  assert.equal(run(...enroll("linux")), 1); assert.match(err.pop(), /RK_BROKER_PINS_EXISTS/);
  assert.equal(run("show", "--platform", "darwin", "--data-root", dataRoot), 1, "the linux enrollment is not a darwin one");
  assert.equal(run(...enroll("freebsd")), 1); assert.match(err.pop(), /RK_BROKER_PINS_PLATFORM: freebsd is not a broker host/);
  assert.equal(fs.existsSync(path.join(dataRoot, "pins", "broker-freebsd.json")), false);
});

test("[7.21] RightKit's committed pins, read from this checkout's gate revision, are well-formed for both broker hosts", (t) => {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const top = git(here, "rev-parse", "--show-toplevel");
  const pipeline = top.status === 0 ? git(here, "show", "HEAD:ci/rk-pipeline.json") : null;
  if (!pipeline || pipeline.status !== 0) { t.skip("no git history of ci/rk-pipeline.json here (an export)"); return; }
  for (const platform of ["darwin", "win32"]) {
    const read = readGateRevisionPins({ gateFile: path.join(here, "bin", "rightkit-gate.mjs"), checkout: top.stdout.trim(), platform });
    assert.ok(read, platform); assert.equal(read.source, "rk-pipeline"); assert.match(read.keyId, /^[0-9a-f]{64}$/);
  }
});
