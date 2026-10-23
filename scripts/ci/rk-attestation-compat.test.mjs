// Exercises real verifyAttestation decision logic; broker CLI I/O is stubbed.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { crateRows, receiptRefusal, resolveRowWorkspace, verifyAttestation, SCHEMA } from "./rk-preflight.mjs";

function fixture(t) {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-attestation-compat-"));
  t.after(() => fs.rmSync(tmpDir, { recursive: true, force: true }));
  const pins = { publicKey: "pinned-public-key", keyId: "pinned-key", host: "pinned-host", brokerDigest: "b".repeat(64), cargoSha256: "c".repeat(64), nodeSha256: "d".repeat(64) };
  const requestId = "00000000-0000-0000-0000-000000000001";
  const nonce = "a".repeat(64);
  const argv = ["run", "build"];
  const expect = { kind: "adapter", adapter: "rightkit-pnpm", script: { path: path.join(tmpDir, "rightkit-pnpm.mjs"), sha256: "e".repeat(64) }, executable: { sha256: pins.nodeSha256 } };
  const payload = { host: pins.host, broker: { brokerDigest: pins.brokerDigest }, nonce, jobId: requestId, requestDigest: "f".repeat(64), request: { kind: "adapter", adapter: expect.adapter, requestedArgv: argv, manifestDigest: null }, executable: { kind: "interpreter", sha256: pins.nodeSha256, script: { ...expect.script } }, result: { status: "SUCCEEDED", exitCode: 0 } };
  const receipt = { state: "SUCCEEDED", exitCode: 0, route: "local", inputBoundary: { status: "unrequested" }, provenance: { schema: "rustright.provenance.v1", signature: { alg: "ES256", keyId: pins.keyId }, payload } };
  let signatureStatus = 0;
  const calls = [];
  const verify = (options = {}) => verifyAttestation(requestId, { nonce, argv, pins, expect, tmpDir, spawnFn: (cmd, args) => {
    calls.push({ cmd, args });
    return args[0] === "status" ? { status: 0, stdout: JSON.stringify(receipt) } : { status: signatureStatus };
  }, ...options });
  return { pins, nonce, requestId, argv, expect, payload, receipt, verify, calls, failSignature: () => { signatureStatus = 1; } };
}

test("schema stays v2; enrolled adapter succeeds without source-manifest prerequisite", (t) => {
  const f = fixture(t);
  assert.equal(SCHEMA, "rk-preflight.v2");
  const result = f.verify();
  assert.equal(result.verified, true);
  assert.equal(result.manifestDigest, null);
  assert.equal(result.inputBoundary, "unrequested");
  const args = f.calls.find((call) => call.args[0] === "verify-receipt").args;
  assert.equal(args[args.indexOf("--public-key") + 1], f.pins.publicKey);
  assert.equal(args[args.indexOf("--nonce") + 1], f.nonce);
});

test("Cargo default retains enrolled Cargo identity without adapter expectations", (t) => {
  const f = fixture(t);
  f.payload.request = { kind: "cargo", requestedArgv: f.argv, manifestDigest: null };
  f.payload.executable = { kind: "native", sha256: f.pins.cargoSha256 };
  assert.equal(f.verify({ expect: undefined }).verified, true);
  f.payload.executable.sha256 = f.pins.nodeSha256;
  assert.equal(f.verify({ expect: undefined }).verified, false);
});

test("consumer rows use manifest directory cwd without client-rewritten manifest selector", () => {
  const unit = "rightkit-fixture";
  const rows = crateRows(unit, { rust: { [unit]: { paths: ["ci/consumers/fixture"], consumerRows: [{ manifestPath: "ci/consumers/fixture/Cargo.toml", args: ["--no-default-features", "--features", "demo"], targets: ["aarch64-apple-darwin"] }] } } });
  const consumers = rows.filter((row) => row.kind === "consumer");
  assert.equal(consumers.length, 1);
  assert.equal(consumers[0].cwd, "ci/consumers/fixture");
  assert.deepEqual(consumers[0].args, ["check", "--locked", "--target", "aarch64-apple-darwin", "--no-default-features", "--features", "demo"]);
  assert.equal(consumers[0].args.includes("--manifest-path"), false);
});

test("consumer workspace requires present regular manifest matching committed bytes", (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-consumer-workspace-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.writeFileSync(path.join(root, "Cargo.toml"), '[workspace]\nmembers = []\n');
  const row = { kind: "consumer", cwd: "ci/consumers/fixture" };
  const cwd = path.join(root, "ci", "consumers", "fixture");
  const file = path.join(cwd, "Cargo.toml");
  fs.mkdirSync(cwd, { recursive: true });
  const bytes = Buffer.from('[package]\nname = "fixture"\nversion = "0.0.0"\n');
  const calls = [];
  const context = { root, row, sha: "a".repeat(40), spawnFn: (cmd, args, options) => {
    calls.push({ cmd, args, options });
    return { status: 0, stdout: bytes };
  } };
  // Parent manifest must never become fallback when consumer manifest is missing.
  assert.throws(() => resolveRowWorkspace(context), /ENOENT/);
  assert.equal(calls.length, 0);
  fs.writeFileSync(file, bytes);
  assert.equal(resolveRowWorkspace(context), cwd);
  assert.deepEqual(calls[0].args, ["--no-replace-objects", "show", `${context.sha}:ci/consumers/fixture/Cargo.toml`]);
  assert.equal(calls[0].options.cwd, root);
  assert.throws(() => resolveRowWorkspace({ ...context, spawnFn: () => ({ status: 128, stdout: Buffer.alloc(0) }) }), /absent or unreadable/);
  fs.writeFileSync(file, "different");
  assert.throws(() => resolveRowWorkspace(context), /differs from requested commit/);
  fs.unlinkSync(file);
  fs.mkdirSync(file);
  assert.throws(() => resolveRowWorkspace(context), /regular non-symlink/);
  if (process.platform !== "win32") {
    fs.rmdirSync(file);
    fs.symlinkSync(path.join(root, "Cargo.toml"), file);
    assert.throws(() => resolveRowWorkspace(context), /regular non-symlink/);
  }
  // Root rows preserve existing cwd behavior & need no manifest read.
  assert.equal(resolveRowWorkspace({ root, row: { kind: "check" }, spawnFn: () => { throw new Error("unexpected git"); } }), path.resolve(root));
});

test("optional working directory binding uses signed submitted workspace only", (t) => {
  const f = fixture(t);
  const workspace = path.dirname(f.expect.script.path);
  f.payload.request.workspace = workspace;
  // A consumer's resolved independent workspace root need not equal primary checkout.
  f.payload.request.workspaceRoot = path.join(workspace, "independent-consumer");
  assert.equal(f.verify({ workspace }).verified, true);
  assert.equal(f.verify({ workspace: path.join(workspace, "wrong") }).verified, false);
  assert.equal(f.verify({ workspace: "relative" }).verified, false);
  delete f.payload.request.workspace;
  assert.equal(f.verify({ workspace }).verified, false);
});

test("structural receipts bind cwd for root & consumer rows", () => {
  const unit = "rightkit-fixture";
  const config = { rust: { [unit]: { paths: ["ci/consumers/fixture"], consumerRows: [{ manifestPath: "ci/consumers/fixture/Cargo.toml" }] } } };
  const requestId = "00000000-0000-0000-0000-000000000001";
  const expected = crateRows(unit, config);
  const receipt = { schema: SCHEMA, sha: "a".repeat(40), registry: "crates", unit, version: "1.0.0", mode: "local-compile", route: "local", ok: true, finishedAt: "2026-10-23T00:00:00Z", scans: { ok: true, exit: 0, timedOut: false }, planChecks: { ok: true, exit: 0, timedOut: false }, rows: expected.map((row) => ({ id: row.id, cwd: row.cwd ?? ".", command: `rustright cargo ${row.args.join(" ")}`, ok: true, exit: 0, timedOut: false, warnings: 0, brokerRequests: [requestId], attestation: { verified: true, signatureVerified: true, nonce: "b".repeat(64), requestId } })) };
  const context = { sha: receipt.sha, registry: "crates", unit, config, version: receipt.version };
  assert.equal(receiptRefusal(receipt, context), null);
  const consumer = receipt.rows.find((row) => row.id.startsWith("consumer-"));
  consumer.cwd = ".";
  assert.match(receiptRefusal(receipt, context), /working directory/);
  consumer.cwd = "ci/consumers/fixture";
  delete receipt.rows[0].cwd;
  assert.match(receiptRefusal(receipt, context), /working directory/);
});

test("adapter cannot loosen enrolled Node pin, script identity, kind, argv, nonce or broker", (t) => {
  const f = fixture(t);
  const cases = [
    [f.payload.request, "kind", "cargo"],
    [f.payload.request, "adapter", "other-adapter"],
    [f.payload.request, "requestedArgv", ["run", "different"]],
    [f.payload.executable, "kind", "native"],
    [f.payload.executable, "sha256", f.pins.cargoSha256],
    [f.payload.executable.script, "path", `${f.expect.script.path}-other`],
    [f.payload.executable.script, "sha256", "0".repeat(64)],
    [f.payload, "nonce", "0".repeat(64)],
    [f.payload, "jobId", "other-job"],
    [f.payload, "host", "other-host"],
    [f.payload.broker, "brokerDigest", "0".repeat(64)],
  ];
  for (const [object, key, bad] of cases) {
    const previous = object[key]; object[key] = bad;
    assert.equal(f.verify().verified, false, key);
    object[key] = previous;
  }
  assert.equal(f.verify({ expect: { ...f.expect, executable: { sha256: f.pins.cargoSha256 } } }).verified, false);
  assert.equal(f.verify({ pins: { ...f.pins, nodeSha256: undefined } }).verified, false);
  assert.equal(f.verify({ expect: { ...f.expect, script: { ...f.expect.script, path: "relative.mjs" } } }).verified, false);
});

test("unsigned success cannot override failed, missing or nonzero signed result", (t) => {
  const f = fixture(t);
  for (const result of [undefined, { status: "FAILED", exitCode: 0 }, { status: "SUCCEEDED", exitCode: 9 }]) {
    f.payload.result = result;
    assert.equal(f.verify().verified, false);
  }
  f.payload.result = { status: "SUCCEEDED", exitCode: 0 };
  f.receipt.state = "FAILED";
  f.receipt.exitCode = 9;
  assert.equal(f.verify().verified, true);
  f.failSignature();
  assert.equal(f.verify().verified, false);
});
