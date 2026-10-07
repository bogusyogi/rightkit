import { canonicalJson, sha256 } from "../trust-schema.mjs";

export const RECEIPT_SCHEMA = 1;
export const RECEIPT_TYPE = "rightkit.protected-ownership-receipt";
const HASH = /^[a-f0-9]{64}$/;
const COMMIT = /^[a-f0-9]{40,64}$/;
const SCANNER_VERSION = /^\d+\.\d+\.\d+$/;
const CATALOG_VERSION = /^\d+(?:\.\d+){0,2}$/;

export function catalogDigest(manifest) {
  return sha256(canonicalJson(manifest));
}

export function sourceTreeDigest(treeListing) {
  return sha256(treeListing);
}

export function validateProtectedReceipt(receipt, expected) {
  const problems = [];
  if (!receipt || typeof receipt !== "object" || Array.isArray(receipt)) return { ok: false, problems: ["receipt must be an object"] };
  if (receipt.schema !== RECEIPT_SCHEMA || receipt.type !== RECEIPT_TYPE) problems.push("unsupported ownership receipt type or schema");
  if (receipt.ok !== true) problems.push("protected verifier did not report success");
  if (!COMMIT.test(receipt.commit ?? "") || receipt.commit !== expected.commit) problems.push("receipt commit does not match requested Git commit");
  if (!COMMIT.test(receipt.tree ?? "") || receipt.tree !== expected.tree) problems.push("receipt tree does not match requested Git tree");
  if (receipt.repositoryIdentity !== expected.repositoryIdentity) problems.push("receipt repository identity does not match canonical remote");
  if (!HASH.test(receipt.sourceDigest ?? "") || receipt.sourceDigest !== expected.sourceDigest) problems.push("receipt source-byte digest does not match committed Git tree bytes");
  if (receipt.visibility !== "private") problems.push("private local adapter requires protected private repository registration");
  if (!Array.isArray(receipt.registeredRoots) || receipt.registeredRoots.length === 0 || receipt.registeredRoots.some((root) => typeof root !== "string" || !/^(?:\.|[A-Za-z0-9][A-Za-z0-9._/-]*)$/.test(root) || root.includes("..") || root.includes("\\")) || new Set(receipt.registeredRoots).size !== receipt.registeredRoots.length) problems.push("protected registered roots are missing or malformed");
  if (!SCANNER_VERSION.test(receipt.scannerVersion ?? "") || receipt.scannerVersion !== expected.scannerVersion || !CATALOG_VERSION.test(receipt.catalogVersion ?? "") || receipt.catalogVersion !== expected.catalogVersion) problems.push("scanner or catalog version does not match protected installed source");
  if (!HASH.test(receipt.catalogDigest ?? "") || receipt.catalogDigest !== expected.catalogDigest || !HASH.test(receipt.contentDigest ?? "")) problems.push("catalog digest does not match protected installed catalog or scanned-content digest is malformed");
  if (receipt.floors?.ok !== true || !HASH.test(receipt.floors?.digest ?? "")) problems.push("protected scanner/catalog floors were not satisfied or bound");
  if (receipt.protection?.ok !== true || receipt.protection?.source !== "os-protected-verifier") problems.push("receipt lacks protected verifier execution evidence");
  return { ok: problems.length === 0, problems };
}
