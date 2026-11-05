// rightkit#1511: the signed exact-sub-handle revocation in a dead owner's reclaim; positive and negative cases with a fixture key (no production key).
import test from "node:test";
import assert from "node:assert";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { judgeOwner, proveOwnerProcessesGone } from "./build-ownership.mjs";
import { REVOCATION_SCHEMA, revocationFile, revocationSignedBytes } from "./handle-revocation.mjs";

const OWNER = "a1b2";
const LOST = `${OWNER}#lost`;
const DONE = `${OWNER}#done`;

function keyPair() {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ec", { namedCurve: "P-256" });
  const jwk = publicKey.export({ format: "jwk" });
  const hex = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]).toString("hex");
  const keyId = crypto.createHash("sha256").update(Buffer.from(hex, "hex")).digest("hex");
  return { privateKey, pins: { host: "broker-host", keyId, publicKey: hex, brokerDigest: "b".repeat(64) } };
}

function setup() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-revoke-"));
  const file = path.join(dir, "build-ownership", "k", "owner.json");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const owner = { schema: "rightkit.build-ownership/1", handle: OWNER, host: os.hostname(), pid: 4242, startedMs: 1, containment: {}, snapshot: "/snap/x", acquiredAt: "2026-11-04T23:56:10.802Z", broker: { submitted: true, handles: [DONE, LOST] } };
  const observed = JSON.stringify(owner);
  fs.writeFileSync(file, observed);
  return { file, owner, observed };
}

const probe = { identity: () => ({ alive: false }), bootTime: () => Date.now() };
const leaseWith = (lostState = { state: "UNKNOWN", members: [] }) => (q) => {
  if (q.handle === OWNER) return { state: "REAPED", why: "", members: [{ handle: DONE, state: "REAPED" }] };
  if (q.handle === DONE) return { state: "REAPED", why: "", members: [{ handle: DONE, state: "REAPED" }] };
  return { why: "no record", ...lostState };
};

function payloadFor({ file, owner, observed }, pins, change = {}) {
  return {
    schema: REVOCATION_SCHEMA, keyId: pins.keyId, host: pins.host, runtime: { brokerDigest: pins.brokerDigest }, snapshot: owner.snapshot, ownership: LOST,
    ownerFile: fs.realpathSync.native(file), dataRoot: fs.realpathSync.native(path.dirname(path.dirname(path.dirname(file)))),
    ownerSha256: crypto.createHash("sha256").update(observed).digest("hex"), proofSha256: "d".repeat(64), ...change,
  };
}

function writeRevocation(ctx, { privateKey, pins }, change = {}, { signWith = privateKey, tamper = null, sub = LOST } = {}) {
  const payload = payloadFor(ctx, pins, change);
  const value = crypto.sign("sha256", revocationSignedBytes(payload), { key: signWith, dsaEncoding: "ieee-p1363" }).toString("hex");
  const body = { schema: REVOCATION_SCHEMA, payload: tamper ? { ...payload, ...tamper } : payload, signature: { alg: "ES256", keyId: pins.keyId, value } };
  const at = revocationFile(fs.realpathSync.native(path.dirname(path.dirname(path.dirname(ctx.file)))), OWNER, sub, ctx.owner.snapshot);
  fs.mkdirSync(path.dirname(at), { recursive: true });
  fs.writeFileSync(at, JSON.stringify(body));
  return at;
}

const judge = (ctx, pins, lease = leaseWith()) => judgeOwner(ctx.observed, { file: ctx.file, probe, lease, pins });

test("without a revocation an UNKNOWN sub-handle keeps the record", () => {
  const ctx = setup(); const k = keyPair();
  assert.throws(() => judge(ctx, k.pins), /sub-handle a1b2#lost and its lease is UNKNOWN/);
});

test("a valid signed revocation settles exactly that sub-handle", () => {
  const ctx = setup(); const k = keyPair();
  writeRevocation(ctx, k);
  const judged = judge(ctx, k.pins);
  assert.ok(judged.proofs.some((p) => /a1b2#lost is UNKNOWN to the broker and revoked/.test(p)));
  assert.ok(judged.proofs.some((p) => /1 REAPED, 0 proven never started, 1 revoked/.test(p)));
  assert.strictEqual(judged.revocations.length, 1);
});

test("no enrolled pin: the revocation is not honoured", () => {
  const ctx = setup(); const k = keyPair();
  writeRevocation(ctx, k);
  assert.throws(() => judge(ctx, null), /lease is UNKNOWN/);
});

for (const [name, opts, pattern] of [
  ["another key", (k) => ({ signWith: keyPair().privateKey }), /signature does not verify/],
  ["a tampered payload", () => ({ tamper: { proofSha256: "e".repeat(64) } }), /signature does not verify/],
  ["other owner bytes", () => ({ change: { ownerSha256: "0".repeat(64) } }), /other owner record bytes/],
  ["another sub-handle in the payload", () => ({ change: { ownership: DONE } }), /revokes a1b2#done/],
  ["another host", () => ({ change: { host: "elsewhere" } }), /another host/],
  ["another broker runtime", () => ({ change: { runtime: { brokerDigest: "c".repeat(64) } } }), /another broker runtime/],
  ["another owner file", () => ({ change: { ownerFile: "/other/owner.json" } }), /another ownership record/],
  ["another data root", () => ({ change: { dataRoot: "/other" } }), /another managed data root/],
  ["another snapshot", () => ({ change: { snapshot: "/snap/y" } }), /another snapshot/],
  ["a malformed proof digest", () => ({ change: { proofSha256: "XYZ" } }), /proofSha256/],
  ["an extra unsigned-shape field", () => ({ change: { revokedAt: "2026-11-05T05:00:00Z" } }), /not exactly/],
]) {
  test(`a revocation signed over ${name} refuses the reclaim`, () => {
    const ctx = setup(); const k = keyPair(); const o = opts(k);
    writeRevocation(ctx, k, o.change ?? {}, o);
    assert.throws(() => judge(ctx, k.pins), pattern);
  });
}

test("a revocation never overrides an ACTIVE lease", () => {
  const ctx = setup(); const k = keyPair();
  writeRevocation(ctx, k);
  assert.throws(() => judge(ctx, k.pins, leaseWith({ state: "ACTIVE", members: [{ handle: LOST, state: "ACTIVE" }] })), /lease is ACTIVE/);
});

test("a revocation never overrides a member row the broker lists", () => {
  const ctx = setup(); const k = keyPair();
  writeRevocation(ctx, k);
  assert.throws(() => judge(ctx, k.pins, leaseWith({ state: "UNKNOWN", members: [{ handle: LOST, state: "UNKNOWN" }] })), /lease is UNKNOWN/);
});

test("a link at the revocation path refuses", () => {
  const ctx = setup(); const k = keyPair();
  const real = writeRevocation(ctx, k);
  const moved = `${real}.real`; fs.renameSync(real, moved); fs.symlinkSync(moved, real);
  assert.throws(() => judge(ctx, k.pins), /not a plain file/);
});

test("revocationFile: the f227 completion key <dataRoot>/snapshot-leases/completed/<sha256 canonical [snapshot, ownership]>, no suffix", () => {
  assert.throws(() => revocationFile("/root", OWNER, "zz#1", "/snap/x"), /not a sub-handle/);
  assert.throws(() => revocationFile("/root", OWNER, `${OWNER}#../x`, "/snap/x"), /not a sub-handle/);
  assert.throws(() => revocationFile("rel", OWNER, LOST, "/snap/x"), /not absolute/);
  const name = crypto.createHash("sha256").update('["/snap/x","a1b2#lost"]').digest("hex");
  assert.strictEqual(revocationFile("/root", OWNER, LOST, "/snap/x"), `/root/snapshot-leases/completed/${name}`);
});

test("a legacy completion record at the key is not a revocation: ignored, the UNKNOWN sub-handle still holds", () => {
  const ctx = setup(); const k = keyPair();
  const at = writeRevocation(ctx, k);
  fs.writeFileSync(at, JSON.stringify({ schema: "rustright.snapshot-completion/1", handle: LOST }));
  assert.throws(() => judge(ctx, k.pins), /sub-handle a1b2#lost and its lease is UNKNOWN/);
});

test("the shared golden fixture verifies: signed bytes, file name, signature", async () => {
  const v = JSON.parse(fs.readFileSync(new URL("./handle-revocation.vectors.json", import.meta.url), "utf8"));
  const { verifyRevocation, revocationName, revocationSignedBytes: bytesOf } = await import("./handle-revocation.mjs");
  assert.strictEqual(bytesOf(v.payload).toString("utf8"), v.signedBytesUtf8);
  assert.strictEqual(crypto.createHash("sha256").update(bytesOf(v.payload)).digest("hex"), v.signedBytesSha256);
  assert.strictEqual(revocationName(v.payload.snapshot, v.payload.ownership), v.fileName);
  const owner = { handle: v.payload.ownership.split("#")[0], snapshot: v.payload.snapshot };
  const expected = { ownerFile: v.payload.ownerFile, dataRoot: v.payload.dataRoot, ownerBytes: Buffer.from(v.ownerBytes), owner, sub: v.payload.ownership };
  assert.deepStrictEqual(verifyRevocation(Buffer.from(JSON.stringify(v.envelope)), v.pins, expected), v.payload);
});

test("process-only proof: judges the owner and processes, never the broker, writes nothing", () => {
  const ctx = setup();
  const before = fs.readFileSync(ctx.file, "utf8");
  const proof = proveOwnerProcessesGone({ file: ctx.file, probe });
  assert.strictEqual(proof.schema, "rightkit.owner-process-proof/1");
  assert.strictEqual(proof.ownerSha256, crypto.createHash("sha256").update(ctx.observed).digest("hex"));
  assert.strictEqual(proof.ownerFile, fs.realpathSync.native(ctx.file));
  assert.ok(proof.proofs.some((p) => /broker work NOT judged/.test(p)));
  assert.strictEqual(proof.probe.package, "@rightkit/release");
  assert.match(proof.probe.moduleSha256, /^[0-9a-f]{64}$/);
  assert.strictEqual(fs.readFileSync(ctx.file, "utf8"), before);
});

test("process-only proof refuses a live owner", () => {
  const ctx = setup();
  const live = { identity: () => ({ alive: true, startedMs: 1 }), bootTime: () => 0 };
  assert.throws(() => proveOwnerProcessesGone({ file: ctx.file, probe: live }), /held by pid 4242/);
});

test("reclaim: an unreadable mark lock left by a crash is removed, a live-looking one is honoured", async () => {
  const { acquireBuildOwnership } = await import("./build-ownership.mjs");
  const { markLockFile } = await import("./ownership-mark.mjs");
  const ctx = setup();
  for (const d of [path.dirname(path.dirname(ctx.file)), path.dirname(ctx.file)]) fs.chmodSync(d, 0o700);
  fs.writeFileSync(markLockFile(ctx.file), "");
  const { processProbe } = await import("./build-ownership.mjs");
  const real = processProbe();
  const done = { ...real, identity: (pid) => (pid === 4242 ? { alive: false } : real.identity(pid)), bootTime: () => Date.now() };
  let r;
  r = acquireBuildOwnership({ file: ctx.file, stage: "t", snapshot: "/snap/x", commit: "c", probe: done, lease: leaseWith({ state: "REAPED", members: [{ handle: LOST, state: "REAPED" }] }), exportEnv: false });
  assert.ok(r.reclaimed);
  assert.ok(!fs.existsSync(markLockFile(ctx.file)) || JSON.parse(fs.readFileSync(markLockFile(ctx.file), "utf8")).schema);
  r.release?.();
});
