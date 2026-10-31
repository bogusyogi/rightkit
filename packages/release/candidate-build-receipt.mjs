// GB-73 (BROKER-CONTRACT section 16): the candidate build receipt the gate writes from its SNAPSHOT build, which RustRight's installer takes instead of a clean primary checkout:
//   rustright-install stage|cutover ... --candidate-receipt <file> --candidate-receipt-sha256 <digest>
// where the file is exactly
//   { "schema": "rustright.candidate-build-receipt.v1", "commit", "tree", "source": "snapshot", "artifacts": { "broker", "client" } }
// (full lowercase commit and tree ids, the sha256 of the broker and client binaries being installed). The installer accepts it only when the file hashes to the DIGEST THE GATE RECORDED (the
// digest comes from the gate's record, never from the file), the commit is the asked-for one, the tree is that commit's own tree in the checkout's object database, the artifact digests are the
// ones being installed and the commit is on origin/main; a tampered receipt, a wrong tree, other binaries and an off-origin commit each refuse before effects. So this module writes the receipt
// deterministically, records its digest apart from it, hands the installer its two arguments, and verifies a receipt the way the installer does before the gate hands it on.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { writeFileAtomic } from "./atomic-write.mjs";

export const CANDIDATE_RECEIPT_SCHEMA = "rustright.candidate-build-receipt.v1";
export const CANDIDATE_RECORD_SCHEMA = "rightkit.candidate-receipt-record/1";
export const CANDIDATE_RECEIPT_FILE = "candidate-build-receipt.json";
export const CANDIDATE_RECORD_FILE = "candidate-build-receipt.record.json";
const OBJECT_ID = /^[0-9a-f]{40}$/;
const DIGEST = /^[0-9a-f]{64}$/;
const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

/** The receipt object for a snapshot build of `commit` (whose tree is `tree`): exactly RustRight's fields, in a fixed order. Refuses anything that is not a full lowercase id or digest. */
export function buildCandidateReceipt({ commit, tree, brokerSha256, clientSha256 }) {
  const problems = [];
  if (!OBJECT_ID.test(commit ?? "")) problems.push("commit must be a full lowercase object id");
  if (!OBJECT_ID.test(tree ?? "")) problems.push("tree must be a full lowercase object id");
  if (!DIGEST.test(brokerSha256 ?? "")) problems.push("the broker artifact digest must be 64 lowercase hex characters");
  if (!DIGEST.test(clientSha256 ?? "")) problems.push("the client artifact digest must be 64 lowercase hex characters");
  if (problems.length) throw Object.assign(new Error(`the candidate build receipt cannot be written: ${problems.join("; ")}`), { code: "RK_CANDIDATE_RECEIPT_INVALID" });
  return { schema: CANDIDATE_RECEIPT_SCHEMA, commit, tree, source: "snapshot", artifacts: { broker: brokerSha256, client: clientSha256 } };
}

/** The bytes the receipt is written as and hashed over: its JSON, one line, a final newline. */
export const candidateReceiptBytes = (receipt) => Buffer.from(`${JSON.stringify(receipt)}\n`, "utf8");

/** The digest of one built artifact (a regular file), or a typed refusal. */
export function artifactDigest(file, { readFile = fs.readFileSync, stat = fs.statSync } = {}) {
  if (typeof file !== "string" || !path.isAbsolute(file)) throw Object.assign(new Error(`the artifact ${JSON.stringify(file)} must be an absolute path`), { code: "RK_CANDIDATE_RECEIPT_INVALID" });
  let meta; try { meta = stat(file); } catch (e) { throw Object.assign(new Error(`the artifact ${file} cannot be read (${e.code ?? e.message})`), { code: "RK_CANDIDATE_RECEIPT_INVALID" }); }
  if (!meta.isFile()) throw Object.assign(new Error(`the artifact ${file} is not a regular file`), { code: "RK_CANDIDATE_RECEIPT_INVALID" });
  return sha256(readFile(file));
}

/**
 * Writes the receipt and, apart from it, the gate's record of its digest, into `dir` (created, owner-only, each written atomically). Returns the record: `{ schema, file, sha256, commit, tree,
 * artifacts, recordFile }`. A directory that already holds a receipt for other bytes is refused: a candidate's receipt is written once.
 */
export function writeCandidateReceipt({ dir, receipt, now = Date.now(), write = writeFileAtomic, read = (f) => fs.readFileSync(f), exists = fs.existsSync }) {
  const file = path.join(dir, CANDIDATE_RECEIPT_FILE); const recordFile = path.join(dir, CANDIDATE_RECORD_FILE);
  const bytes = candidateReceiptBytes(receipt);
  if (exists(file) && sha256(read(file)) !== sha256(bytes)) throw Object.assign(new Error(`${file} already holds a different candidate build receipt; a candidate's receipt is written once`), { code: "RK_CANDIDATE_RECEIPT_EXISTS" });
  write(file, bytes, { mode: 0o600 });
  const record = { schema: CANDIDATE_RECORD_SCHEMA, file, sha256: sha256(bytes), commit: receipt.commit, tree: receipt.tree, artifacts: { ...receipt.artifacts }, recordedAt: new Date(now).toISOString() };
  write(recordFile, Buffer.from(`${JSON.stringify(record)}\n`, "utf8"), { mode: 0o600 });
  return { ...record, recordFile };
}

/** The installer's two arguments for a receipt record: the file and the digest the GATE recorded. */
export function candidateReceiptArgs(record) {
  if (!record || typeof record.file !== "string" || !DIGEST.test(record.sha256 ?? "")) throw Object.assign(new Error("a candidate receipt record names its file and its sha256"), { code: "RK_CANDIDATE_RECEIPT_INVALID" });
  return ["--candidate-receipt", record.file, "--candidate-receipt-sha256", record.sha256];
}

/**
 * Whether the receipt at `file` is acceptable for installing `commit` / `tree` with the binaries whose digests are `brokerSha256` and `clientSha256`, judged as RustRight's installer judges it
 * (provenance.rs): the file hashes to `expectedSha256` (the gate's recorded digest, never a value read from the file), it is exactly {schema, commit, tree, source, artifacts:{broker, client}} with the
 * v1 schema and source "snapshot", the commit and tree are the asked-for ones and the artifact digests are the ones being installed. Returns the problems; none means acceptable. (That the commit is on
 * origin/main and the tree is the commit's own is the installer's check, in the checkout's object database.)
 */
export function candidateReceiptProblems({ file, expectedSha256, commit, tree, brokerSha256, clientSha256, readFile = fs.readFileSync }) {
  let bytes; try { bytes = readFile(file); } catch (e) { return [`the receipt ${file} cannot be read (${e.code ?? e.message})`]; }
  const problems = [];
  if (!DIGEST.test(expectedSha256 ?? "")) return ["the gate's recorded digest is missing or malformed"];
  if (sha256(bytes) !== expectedSha256) problems.push(`the file hashes to ${sha256(bytes)}, not the digest the gate recorded (${expectedSha256}): it was changed after it was written`);
  let doc; try { doc = JSON.parse(Buffer.from(bytes).toString("utf8")); } catch { return [...problems, "the receipt is not JSON"]; }
  if (!isObject(doc) || Object.keys(doc).sort().join() !== "artifacts,commit,schema,source,tree") problems.push("the receipt is not exactly {schema, commit, tree, source, artifacts}");
  if (doc?.schema !== CANDIDATE_RECEIPT_SCHEMA) problems.push(`the schema is ${JSON.stringify(doc?.schema)}, not ${CANDIDATE_RECEIPT_SCHEMA}`);
  if (doc?.source !== "snapshot") problems.push(`the source is ${JSON.stringify(doc?.source)}, not "snapshot"`);
  if (doc?.commit !== commit) problems.push(`the receipt is for commit ${String(doc?.commit).slice(0, 12)}, not ${String(commit).slice(0, 12)}`);
  if (doc?.tree !== tree) problems.push(`the receipt names tree ${String(doc?.tree).slice(0, 12)}, not the commit's ${String(tree).slice(0, 12)}`);
  const artifacts = doc?.artifacts;
  if (!isObject(artifacts) || Object.keys(artifacts).sort().join() !== "broker,client") problems.push("artifacts is not exactly {broker, client}");
  else {
    if (artifacts.broker !== brokerSha256) problems.push("the broker binary being installed is not the one the receipt names");
    if (artifacts.client !== clientSha256) problems.push("the client binary being installed is not the one the receipt names");
  }
  return problems;
}

/** The gate's record read back (never the receipt's own digest): `{ record }` or `{ problem }`. */
export function readCandidateRecord(recordFile, { readFile = fs.readFileSync } = {}) {
  let doc; try { doc = JSON.parse(readFile(recordFile, "utf8")); } catch (e) { return { problem: `the candidate receipt record ${recordFile} cannot be read (${e.code ?? e.message})` }; }
  if (doc?.schema !== CANDIDATE_RECORD_SCHEMA || typeof doc.file !== "string" || !DIGEST.test(doc.sha256 ?? "") || !OBJECT_ID.test(doc.commit ?? "") || !OBJECT_ID.test(doc.tree ?? "")) return { problem: `${recordFile} is not a ${CANDIDATE_RECORD_SCHEMA}` };
  return { record: doc };
}
