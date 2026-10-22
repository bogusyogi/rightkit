import assert from "node:assert/strict";
import test from "node:test";
import { verifyAttestation, brokerRequestIds, checkTools, countDocWarnings, crateRows, parseArgs, receiptRefusal, SCHEMA } from "./rk-preflight.mjs";

const sha = "a".repeat(40), unit = "rightkit-example", version = "1.0.0", config = {};
const expected = { sha, registry: "crates", unit, version, config };
export function greenReceipt() {
  return { schema: SCHEMA, mode: "local-compile", sha, registry: "crates", unit, version, route: "local", ok: true, finishedAt: new Date().toISOString(), scans: { ok: true, exit: 0, timedOut: false }, planChecks: { ok: true, exit: 0, timedOut: false }, rows: crateRows(unit, config).map((r) => ({ attestation: { verified: true, signatureVerified: true, nonce: "f".repeat(64), requestId: "11111111-1111-4111-8111-111111111111" }, id: r.id, command: `rustright cargo ${r.args.join(" ")}`, exit: 0, timedOut: false, warnings: 0, brokerRequests: ["11111111-1111-4111-8111-111111111111"], ok: true })) };
}

test("publication preflight refuses scan/compile shortcuts & unbrokered npm", () => {
  for (const flag of ["--skip-tests", "--rows-only"]) assert.throws(() => parseArgs(["--registry", "crates", "--unit", unit, flag]), /partial/);
  assert.throws(() => parseArgs(["--registry", "npm", "--unit", "@rightkit/legal"]), /broker-admitted/);
  assert.throws(() => parseArgs(["--registry", "crates", "--unit", unit, "--budget-min", "0"]), /positive/);
});

test("compile plan preserves feature coverage, compiles tests without running them & covers local-path consumers", () => {
  const config = { rust: { [unit]: { featureSets: [[], ["--features", "remote"]], alsoSerial: true, paths: ["ci/consumers/example"], consumerRows: [{ manifestPath: "ci/consumers/example/Cargo.toml", args: ["--features", "engine"], locked: true }] } } };
  const rows = crateRows(unit, config);
  assert.equal(rows.filter((r) => r.kind === "doc").length, 3);
  assert.equal(rows.filter((r) => r.kind === "test").length, 2);
  assert.ok(rows.filter((r) => r.kind === "test").every((r) => r.args.includes("--no-run") && !r.args.includes("--test-threads=1")));
  assert.ok(rows.every((r) => r.args.includes("--locked")));
  assert.ok(rows.some((r) => r.kind === "consumer" && r.args.includes("ci/consumers/example/Cargo.toml")));
  config.rust[unit].consumerRows[0].args = ["--manifest-path", "/tmp/foreign/Cargo.toml"];
  assert.throws(() => crateRows(unit, config), /may not contain/);
});

test("admission rejects forged shapes, stale identity, absent scans, omitted rows & unbrokered rows", () => {
  assert.equal(receiptRefusal(greenReceipt(), expected), null);
  const mutations = [
    (r) => { r.schema = "rk-preflight.v1"; }, (r) => { r.route = "ci"; },
    (r) => { r.sha = "b".repeat(40); }, (r) => { r.unit = "rightkit-other"; },
    (r) => { r.registry = "npm"; }, (r) => { r.version = "1.0.1"; },
    (r) => { r.scans = null; }, (r) => { r.scans.timedOut = true; },
    (r) => { r.planChecks = null; }, (r) => { r.planChecks.exit = 1; },
    (r) => { r.rows.pop(); }, (r) => { r.rows[0].brokerRequests = []; },
    (r) => { r.rows[0].command = "cargo check"; }, (r) => { delete r.finishedAt; },
    (r) => { r.rows[0].attestation.verified = false; }, (r) => { r.rows[0].attestation.signatureVerified = false; }, (r) => { r.rows[0].attestation.requestId = "22222222-2222-4222-8222-222222222222"; }, (r) => { delete r.rows[0].attestation; },
  ];
  for (const mutate of mutations) { const r = greenReceipt(); mutate(r); assert.ok(receiptRefusal(r, expected)); }
});

test("tool parity resolves pinned rustfmt & rejects prefix-only actionlint match", () => {
  const pins = { actionlint: "1.7.12", rustfmt: { toolchain: "1.98.0", version: "rustfmt pinned", darwin: { path: "/tools/1.98.0/rustfmt" } }, shellcheck: { darwin: { path: "/tools/shellcheck", sha256: "aa" } } };
  const calls = [];
  const spawnFn = (cmd, args) => { calls.push([cmd, args]); return { status: 0, stdout: cmd === "actionlint" ? "1.7.12\n" : cmd === "rustup" ? "/tools/1.98.0/rustfmt\n" : cmd.endsWith("rustfmt") ? "rustfmt pinned\n" : "ShellCheck\nversion: 0.10.0\n" }; };
  const base = { pins, spawnFn, exists: () => true, digest: () => "aa", platform: "darwin" };
  const good = checkTools(base);
  assert.deepEqual(good.problems, []);
  assert.equal(good.observed.rustfmt.path, "/tools/1.98.0/rustfmt");
  assert.ok(calls.some(([cmd]) => cmd === "/tools/1.98.0/rustfmt"));
  assert.match(checkTools({ ...base, spawnFn: (cmd, args) => cmd.endsWith("rustfmt") ? { status: 0, stdout: "wrong formatter" } : spawnFn(cmd, args) }).problems.join(" "), /pinned/);
  assert.match(checkTools({ ...base, digest: () => "bb" }).problems.join(" "), /sha256/);
  assert.match(checkTools({ ...base, spawnFn: (cmd, args) => cmd === "actionlint" ? { status: 0, stdout: "1.7.120" } : spawnFn(cmd, args) }).problems.join(" "), /not the pinned/);
});

test("rustdoc diagnostics & request IDs are captured", () => {
  assert.equal(countDocWarnings("warning: private intra-doc link\nwarning: `crate` generated 1 warning\n"), 1);
  assert.deepEqual(brokerRequestIds("rustright: request 6891efd1-6e8b-4ef4-9600-613527d4515e\n"), ["6891efd1-6e8b-4ef4-9600-613527d4515e"]);
});


test("per-row attestation: broker status fields must match, then the envelope signature is verified against the enrolled key", () => {
  const nonce = "a".repeat(64), job = "11111111-1111-4111-8111-111111111111";
  const pins = { host: "mac.local", keyId: "b".repeat(64), publicKey: "c".repeat(64), brokerDigest: "d".repeat(64) };
  const status = (over = {}) => ({ receipt: { state: "SUCCEEDED", exitCode: 0, inputBoundary: { status: "unrequested" }, provenance: { schema: "rustright.provenance.v1", signature: { alg: "ES256", keyId: pins.keyId }, payload: { host: pins.host, broker: { brokerDigest: pins.brokerDigest }, nonce, jobId: job, requestDigest: "e".repeat(64), request: { kind: "cargo", requestedArgv: ["check", "-p", "x", "--locked"], manifestDigest: null } } }, ...over } });
  const spawnFor = (doc, verifyStatus = 0) => (cmd, args) => (args[0] === "status" ? { status: 0, stdout: JSON.stringify(doc) } : { status: verifyStatus, stdout: "" });
  const calls = [];
  const good = verifyAttestation(job, { nonce, argv: ["check", "-p", "x", "--locked"], pins, tmpDir: process.env.TMPDIR ?? "/tmp", spawnFn: (cmd, args) => { calls.push(args); return spawnFor(status())(cmd, args); } });
  assert.equal(good.verified, true);
  assert.equal(good.signatureVerified, true);
  const verify = calls.find((a) => a[0] === "verify-receipt");
  assert.ok(verify.includes("--public-key") && verify[verify.indexOf("--public-key") + 1] === pins.publicKey, "verifier gets the enrolled public key");
  assert.ok(verify[verify.indexOf("--broker-digest") + 1] === pins.brokerDigest && verify[verify.indexOf("--nonce") + 1] === nonce && verify[verify.indexOf("--job-id") + 1] === job);
  const bad = (over, n = nonce, argv = ["check", "-p", "x", "--locked"], verifyStatus = 0) => verifyAttestation(job, { nonce: n, argv, pins, tmpDir: process.env.TMPDIR ?? "/tmp", spawnFn: spawnFor(status(over), verifyStatus) }).reason;
  assert.equal(bad({}, "c".repeat(64)), "nonce mismatch");
  assert.equal(bad({}, nonce, ["check", "-p", "y", "--locked"]), "argv mismatch");
  assert.equal(bad({ exitCode: 1 }), "job did not succeed");
  assert.equal(bad({}, nonce, ["check", "-p", "x", "--locked"], 1), "signature verification failed");
  const foreignKey = status(); foreignKey.receipt.provenance.signature.keyId = "9".repeat(64);
  assert.equal(verifyAttestation(job, { nonce, argv: ["check", "-p", "x", "--locked"], pins, spawnFn: spawnFor(foreignKey) }).reason, "signature key is not the enrolled key");
  const unsigned = status(); delete unsigned.receipt.provenance;
  assert.equal(verifyAttestation(job, { nonce, argv: ["check", "-p", "x", "--locked"], pins, spawnFn: spawnFor(unsigned) }).reason, "no signed provenance envelope");
  assert.equal(verifyAttestation(job, { nonce, argv: [], pins: {} }).reason, "no enrollment pins for this host");
  assert.equal(verifyAttestation(null, { nonce, argv: [], pins }).reason, "no broker request or nonce");
});
