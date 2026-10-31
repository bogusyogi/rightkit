// GB-73 (BROKER-CONTRACT section 16): the candidate build receipt `rustright.candidate-build-receipt.v1` the gate writes from its snapshot build, and the two arguments it hands the RustRight
// installer (`--candidate-receipt <file> --candidate-receipt-sha256 <digest>`). The digest is the GATE's record, never read from the file; the receipt is exactly {schema, commit, tree, source:
// "snapshot", artifacts:{broker, client}}; every tamper RustRight's installer refuses (changed file, wrong tree, other binaries, another commit) is refused here before the receipt is handed on.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { CANDIDATE_RECEIPT_SCHEMA, artifactDigest, buildCandidateReceipt, candidateReceiptArgs, candidateReceiptBytes, candidateReceiptProblems, readCandidateRecord, writeCandidateReceipt } from "./candidate-build-receipt.mjs";
import { candidateReceiptCommand } from "./candidate-receipt-command.mjs";
import { main } from "./bin/rightkit-gate.mjs";

const COMMIT = "a".repeat(40), TREE = "b".repeat(40);
const sha = (b) => crypto.createHash("sha256").update(b).digest("hex");
const tmp = (t) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-candidate-receipt-"))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
const binaries = (dir) => { const broker = path.join(dir, "rustright-broker"); const client = path.join(dir, "rustright"); fs.writeFileSync(broker, "broker bytes"); fs.writeFileSync(client, "client bytes"); return { broker, client, brokerSha256: sha("broker bytes"), clientSha256: sha("client bytes") }; };

test("[GB-73] the receipt is exactly RustRight's v1 shape, deterministic, with the source `snapshot`", () => {
  const receipt = buildCandidateReceipt({ commit: COMMIT, tree: TREE, brokerSha256: "1".repeat(64), clientSha256: "2".repeat(64) });
  assert.deepEqual(receipt, { schema: "rustright.candidate-build-receipt.v1", commit: COMMIT, tree: TREE, source: "snapshot", artifacts: { broker: "1".repeat(64), client: "2".repeat(64) } });
  assert.equal(receipt.schema, CANDIDATE_RECEIPT_SCHEMA);
  assert.deepEqual(Object.keys(receipt), ["schema", "commit", "tree", "source", "artifacts"]);
  assert.equal(candidateReceiptBytes(receipt).toString("utf8"), `${JSON.stringify(receipt)}\n`);
  for (const bad of [{ commit: "short" }, { commit: COMMIT.toUpperCase() }, { tree: "" }, { brokerSha256: "x" }, { clientSha256: "F".repeat(64) }]) assert.throws(() => buildCandidateReceipt({ commit: COMMIT, tree: TREE, brokerSha256: "1".repeat(64), clientSha256: "2".repeat(64), ...bad }), (e) => e.code === "RK_CANDIDATE_RECEIPT_INVALID", JSON.stringify(bad));
});

test("[GB-73] the gate records the receipt's digest apart from the file and hands the installer exactly `--candidate-receipt <file> --candidate-receipt-sha256 <digest>`; a receipt is written once", (t) => {
  const dir = tmp(t); const b = binaries(dir);
  const receipt = buildCandidateReceipt({ commit: COMMIT, tree: TREE, brokerSha256: b.brokerSha256, clientSha256: b.clientSha256 });
  const record = writeCandidateReceipt({ dir: path.join(dir, "out"), receipt, now: 1_730_000_000_000 });
  assert.equal(record.sha256, sha(fs.readFileSync(record.file)));
  assert.equal(path.basename(record.file), "candidate-build-receipt.json"); assert.equal(path.basename(record.recordFile), "candidate-build-receipt.record.json");
  assert.deepEqual(candidateReceiptArgs(record), ["--candidate-receipt", record.file, "--candidate-receipt-sha256", record.sha256]);
  const read = readCandidateRecord(record.recordFile);
  assert.deepEqual([read.record.sha256, read.record.commit, read.record.tree, read.record.file], [record.sha256, COMMIT, TREE, record.file]);
  assert.equal(fs.statSync(record.file).mode & 0o077, 0, "owner-only");
  // The same bytes again are the same record; other bytes for the same candidate are refused.
  assert.equal(writeCandidateReceipt({ dir: path.dirname(record.file), receipt }).sha256, record.sha256);
  assert.throws(() => writeCandidateReceipt({ dir: path.dirname(record.file), receipt: { ...receipt, tree: "c".repeat(40) } }), (e) => e.code === "RK_CANDIDATE_RECEIPT_EXISTS");
  assert.throws(() => candidateReceiptArgs({ file: record.file }), /names its file and its sha256/);
  assert.equal(readCandidateRecord(path.join(dir, "missing.json")).problem !== undefined, true);
  fs.writeFileSync(record.recordFile, JSON.stringify({ schema: "other" }));
  assert.match(readCandidateRecord(record.recordFile).problem, /is not a rightkit\.candidate-receipt-record/);
});

test("[GB-73] every tamper RustRight's installer refuses is refused here: a changed file, a wrong tree, another commit, other binaries, a foreign source or schema, extra fields, a digest taken from the file", (t) => {
  const dir = tmp(t); const b = binaries(dir);
  const receipt = buildCandidateReceipt({ commit: COMMIT, tree: TREE, brokerSha256: b.brokerSha256, clientSha256: b.clientSha256 });
  const record = writeCandidateReceipt({ dir: path.join(dir, "out"), receipt });
  const judge = (over = {}) => candidateReceiptProblems({ file: record.file, expectedSha256: record.sha256, commit: COMMIT, tree: TREE, brokerSha256: b.brokerSha256, clientSha256: b.clientSha256, ...over });
  assert.deepEqual(judge(), [], "the untouched receipt is acceptable");
  const rewrite = (patch) => { const doc = { ...JSON.parse(fs.readFileSync(record.file, "utf8")), ...patch }; fs.writeFileSync(record.file, `${JSON.stringify(doc)}\n`); };
  const original = fs.readFileSync(record.file);
  const cases = [
    ["another tree in the file", () => rewrite({ tree: "c".repeat(40) }), /hashes to|names tree/],
    ["another commit in the file", () => rewrite({ commit: "d".repeat(40) }), /hashes to|for commit/],
    ["a foreign source", () => rewrite({ source: "checkout" }), /source/],
    ["another schema", () => rewrite({ schema: "rustright.candidate-build-receipt.v2" }), /schema/],
    ["an extra field", () => rewrite({ extra: 1 }), /exactly/],
    ["other artifacts", () => rewrite({ artifacts: { broker: "9".repeat(64), client: b.clientSha256 } }), /broker binary/],
  ];
  for (const [name, tamper, why] of cases) {
    fs.writeFileSync(record.file, original); tamper();
    const problems = judge();
    assert.ok(problems.length > 0 && problems.some((p) => why.test(p)), `${name}: ${problems.join("; ")}`);
    assert.match(problems.join(" "), /hashes to/, `${name}: a changed file never hashes to the gate's recorded digest`);
  }
  fs.writeFileSync(record.file, original);
  // The digest comes from the gate's record: a digest read from a tampered file would agree with it and prove nothing, so it is never accepted in its place.
  rewrite({ tree: "c".repeat(40) });
  assert.ok(judge({ expectedSha256: sha(fs.readFileSync(record.file)) }).some((p) => /names tree/.test(p)), "even when the file's own digest is supplied, the wrong tree is refused");
  fs.writeFileSync(record.file, original);
  // What is being installed, not what the file says: another commit, tree or binaries refuse.
  assert.ok(judge({ commit: "d".repeat(40) }).some((p) => /for commit/.test(p)));
  assert.ok(judge({ tree: "e".repeat(40) }).some((p) => /names tree/.test(p)));
  assert.ok(judge({ brokerSha256: "8".repeat(64) }).some((p) => /broker binary/.test(p)));
  assert.ok(judge({ clientSha256: "7".repeat(64) }).some((p) => /client binary/.test(p)));
  assert.ok(judge({ expectedSha256: undefined }).some((p) => /recorded digest is missing/.test(p)));
  assert.ok(judge({ file: path.join(dir, "gone.json") })[0].includes("cannot be read"));
  fs.writeFileSync(record.file, "not json\n");
  assert.ok(candidateReceiptProblems({ file: record.file, expectedSha256: sha("not json\n"), commit: COMMIT, tree: TREE, brokerSha256: b.brokerSha256, clientSha256: b.clientSha256 }).includes("the receipt is not JSON"));
});

test("[GB-73] artifactDigest hashes a regular file and refuses a relative path, a missing file and a directory", (t) => {
  const dir = tmp(t); const b = binaries(dir);
  assert.equal(artifactDigest(b.broker), b.brokerSha256);
  for (const bad of ["relative/path", path.join(dir, "missing"), dir]) assert.throws(() => artifactDigest(bad), (e) => e.code === "RK_CANDIDATE_RECEIPT_INVALID", bad);
});

test("[GB-73] `rightkit-gate candidate-receipt` writes the receipt from the commit's own tree and the built binaries' digests, never from flags, and prints the record with the installer's arguments", async (t) => {
  const dir = tmp(t); const b = binaries(dir);
  const out = []; const err = [];
  const spawn = (cmd, args) => (cmd === "git" && args.join(" ") === `rev-parse --verify --quiet ${COMMIT}^{tree}` ? { status: 0, stdout: `${TREE}\n` } : { status: 1, stdout: "" });
  const o = { out: (m) => out.push(m), err: (m) => err.push(m), spawn, cwd: dir, env: { RIGHTKIT_SCRATCH_ROOT: dir }, platform: process.platform };
  assert.equal(await candidateReceiptCommand(o, ["--commit", COMMIT, "--broker", b.broker, "--client", b.client]), 0, err.join("\n"));
  const printed = JSON.parse(out[0]);
  assert.equal(printed.commit, COMMIT); assert.equal(printed.tree, TREE);
  assert.deepEqual(printed.installerArgs, ["--candidate-receipt", printed.file, "--candidate-receipt-sha256", printed.sha256]);
  assert.equal(printed.file, path.join(dir, "candidate-receipts", COMMIT, "candidate-build-receipt.json"));
  assert.deepEqual(JSON.parse(fs.readFileSync(printed.file, "utf8")).artifacts, { broker: b.brokerSha256, client: b.clientSha256 });
  // Refusals: usage, an unknown commit, an unreadable artifact, a relative --out.
  for (const [argv, why] of [[["--commit", COMMIT], /usage/], [["--commit", COMMIT, "--broker", b.broker, "--client", b.client, "--digest", "x"], /usage/], [["--commit", "f".repeat(40), "--broker", b.broker, "--client", b.client], /not in this repository/], [["--commit", COMMIT, "--broker", path.join(dir, "gone"), "--client", b.client], /cannot be read/], [["--commit", COMMIT, "--broker", b.broker, "--client", b.client, "--out", "rel"], /absolute/]]) {
    err.length = 0;
    const code = await candidateReceiptCommand({ ...o, spawn: (cmd, args) => (args.join(" ").includes(`${"f".repeat(40)}^{tree}`) ? { status: 1, stdout: "" } : spawn(cmd, args)) }, argv);
    assert.notEqual(code, 0, JSON.stringify(argv)); assert.match(err.join("\n"), why, JSON.stringify(argv));
  }
  // The entry point dispatches it: no arguments is a usage refusal from the real main.
  const real = []; assert.equal(await main(["candidate-receipt"], { err: (m) => real.push(m), out: () => {} }), 2); assert.match(real.join("\n"), /usage: rightkit-gate candidate-receipt/);
});

test("[T13] with no --out and no RIGHTKIT_SCRATCH_ROOT a direct (Linux) host keeps the receipt under the account's state directory, never /Volumes/D", async () => {
  const home = path.join(path.sep, "home", "gate-user");
  const written = [];
  const spawn = (cmd, args) => (cmd === "git" && args.join(" ") === `rev-parse --verify --quiet ${COMMIT}^{tree}` ? { status: 0, stdout: `${TREE}\n` } : { status: 1, stdout: "" });
  const o = { out: () => {}, err: (m) => assert.fail(m), spawn, cwd: home, env: { HOME: home }, platform: "linux" };
  const files = { broker: "broker bytes", client: "client bytes" };
  const code = await candidateReceiptCommand(o, ["--commit", COMMIT, "--broker", "broker", "--client", "client"], { artifact: (f) => sha(files[f]), write: ({ dir }) => { written.push(dir); return { file: path.join(dir, "candidate-build-receipt.json"), recordFile: path.join(dir, "candidate-build-receipt.record.json"), sha256: "0".repeat(64), commit: COMMIT, tree: TREE }; } });
  assert.equal(code, 0);
  assert.deepEqual(written, [path.join(home, ".local", "state", "rightkit", "runs", "candidate-receipts", COMMIT)]);
  assert.ok(!written[0].includes("Volumes"), "no fixed /Volumes/D fallback on a direct host");
  // An explicit data root still names the runs directory, and RIGHTKIT_SCRATCH_ROOT still wins (gateRunsRoot's own rules).
  written.length = 0;
  await candidateReceiptCommand({ ...o, env: { HOME: home, RIGHTKIT_MANAGED_ROOT: "/data" } }, ["--commit", COMMIT, "--broker", "broker", "--client", "client"], { artifact: (f) => sha(files[f]), write: ({ dir }) => { written.push(dir); return { file: "f", recordFile: "r", sha256: "0".repeat(64) }; } });
  assert.deepEqual(written, [path.join("/data", "runs", "candidate-receipts", COMMIT)]);
});
