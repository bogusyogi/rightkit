import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { receiptRefusal, verifyAttestation, SCHEMA } from "./rk-preflight.mjs";
import { packageRows, readPackages, adapterExpectation } from "./rk-npm-preflight.mjs";

function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-npm-admission-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const unit = "@rightkit/fixture";
  const dir = "packages/fixture";
  fs.mkdirSync(path.join(root, dir), { recursive: true });
  fs.writeFileSync(path.join(root, dir, "package.json"), JSON.stringify({ name: unit, version: "1.2.3", scripts: { build: "tsc", test: "node --test" } }));
  const pkg = readPackages(root).get(unit);
  const rows = packageRows(unit, pkg, { storeDir: path.join(root, "store") });
  const requestId = (index) => `00000000-0000-0000-0000-${String(index + 1).padStart(12, "0")}`;
  const receipt = { schema: SCHEMA, sha: "a".repeat(40), registry: "npm", unit, version: pkg.version, dir: pkg.dir, mode: "local-test", adapter: "rightkit-pnpm", route: "local", ok: true, finishedAt: "2026-10-23T00:00:00Z", scans: { ok: true, exit: 0, timedOut: false }, planChecks: { ok: true, exit: 0, timedOut: false }, requiredRows: rows.map((row) => row.id), rows: rows.map((row, index) => ({ id: row.id, kind: row.kind, cwd: ".", command: `rustright run rightkit-pnpm -- ${row.args.join(" ")}`, exit: 0, timedOut: false, ok: true, brokerRequests: [requestId(index)], attestation: { verified: true, signatureVerified: true, nonce: "b".repeat(64), requestId: requestId(index) } })) };
  const context = { sha: receipt.sha, registry: "npm", unit, version: pkg.version, config: {}, expectedRows: rows, packageDir: pkg.dir };
  const pins = { publicKey: "enrolled-key", keyId: "enrolled-key-id", host: "enrolled-host", brokerDigest: "c".repeat(64), nodeSha256: "d".repeat(64), pnpmScriptPath: path.join(root, "pnpm.mjs"), pnpmScriptSha256: "e".repeat(64) };
  return { root, unit, rows, receipt, context, pins };
}

test("npm local-test receipt follows independently planned install/build/test order", (t) => {
  const f = fixture(t);
  assert.deepEqual(f.rows.map((row) => row.id), ["install", "build-closure", "test"]);
  assert.equal(receiptRefusal(f.receipt, f.context), null);
  assert.match(receiptRefusal(f.receipt, { ...f.context, expectedRows: undefined }), /row plan/);
});

test("npm structural admission rejects altered identity, stages, rows, argv & broker IDs", (t) => {
  const f = fixture(t);
  const mutations = [
    (r) => { r.registry = "crates"; },
    (r) => { r.mode = "local-compile"; },
    (r) => { r.version = "9.9.9"; },
    (r) => { r.sha = "f".repeat(40); },
    (r) => { r.unit = "@rightkit/other"; },
    (r) => { r.dir = "packages/other"; },
    (r) => { r.adapter = "other-adapter"; },
    (r) => { r.scans.ok = false; },
    (r) => { r.planChecks.exit = 1; },
    (r) => { r.rows.reverse(); },
    (r) => { r.requiredRows.reverse(); },
    (r) => { r.rows.pop(); },
    (r) => { r.rows[0].command += " --ignore-scripts"; },
    (r) => { r.rows[0].cwd = "packages/fixture"; },
    (r) => { r.rows[0].kind = "other"; },
    (r) => { r.rows[0].exit = 1; },
    (r) => { r.rows[0].timedOut = true; },
    (r) => { r.rows[0].brokerRequests.push(r.rows[1].brokerRequests[0]); },
    (r) => { r.rows[0].brokerRequests = ["not-a-job"]; },
    (r) => { r.rows[0].attestation.signatureVerified = false; },
    (r) => { r.rows[0].attestation.nonce = "g".repeat(64); },
    (r) => { r.rows[0].attestation.requestId = r.rows[1].brokerRequests[0]; },
  ];
  for (const mutate of mutations) {
    const receipt = structuredClone(f.receipt);
    mutate(receipt);
    assert.notEqual(receiptRefusal(receipt, f.context), null, mutate.toString());
  }
});

test("actual verifier rechecks adapter identity, enrolled pins, exact argv & root cwd", (t) => {
  const f = fixture(t);
  const row = f.rows[0];
  const proof = f.receipt.rows[0].attestation;
  const expect = adapterExpectation(f.pins);
  const payload = { host: f.pins.host, broker: { brokerDigest: f.pins.brokerDigest }, nonce: proof.nonce, jobId: proof.requestId, requestDigest: "f".repeat(64), request: { kind: "adapter", adapter: expect.adapter, requestedArgv: row.args, workspace: f.root }, executable: { kind: "interpreter", sha256: f.pins.nodeSha256, script: { ...expect.script } }, result: { status: "SUCCEEDED", exitCode: 0 } };
  const receipt = { route: "local", provenance: { schema: "rustright.provenance.v1", signature: { alg: "ES256", keyId: f.pins.keyId }, payload } };
  const verify = (options = {}) => verifyAttestation(proof.requestId, { nonce: proof.nonce, argv: row.args, pins: f.pins, expect, workspace: f.root, tmpDir: f.root, spawnFn: (_cmd, args) => args[0] === "status" ? { status: 0, stdout: JSON.stringify(receipt) } : { status: 0 }, ...options });
  assert.equal(verify().verified, true);
  for (const [object, key, bad] of [[payload.request, "adapter", "other"], [payload.request, "requestedArgv", ["install"]], [payload.request, "workspace", path.join(f.root, "wrong")], [payload.executable, "sha256", "0".repeat(64)], [payload.executable.script, "sha256", "0".repeat(64)], [payload.executable.script, "path", `${expect.script.path}-other`], [payload.result, "status", "FAILED"], [payload.result, "exitCode", 5]]) {
    const previous = object[key]; object[key] = bad;
    assert.equal(verify().verified, false, key);
    object[key] = previous;
  }
  assert.equal(verify({ pins: { ...f.pins, nodeSha256: "0".repeat(64) } }).verified, false);
  assert.equal(verify({ expect: { ...expect, executable: { sha256: "0".repeat(64) } } }).verified, false);
});
