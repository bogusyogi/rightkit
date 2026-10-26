// Sealed source-closure freshness for the app check. A committed qualification record that claims "these source files, at these
// digests, produced me" is stale the moment one of those files changes: the claim no longer describes the tree it ships in. Legion's
// qualification/provider-accuracy/index.json failed exactly so, only after a full Windows build (Orthic-Labs/legion runs 40149636641
// job 121038474276 and 40158717069 job 121065535787: "AssertionError: source closure stale/incomplete"). This rule reads the same
// data before any build, as a trusted JSON adapter: no repository code, no network, no clock.
//
//   source-closure  SF-01  in any committed *.json, an object { sourceClosure: { sources: { "<path>": "sha256:<hex>" }, digest } }
//                          (a sealed record) where a listed source is missing from the tree, its bytes no longer hash to the recorded
//                          digest, or `digest` is not sha256 over the canonical JSON of `sources` (sorted keys, no spaces, UTF-8).
//
// What it cannot see: a source the producer would now ADD to the closure. The closure's membership is computed by the producing
// repository's own code, which a Class A rule never runs; that half stays with the repository's own gate after the build.
//
// A digest a producer computes over a normalised projection instead of the raw bytes is declared in .rightkit/source-closures.json
// with one of the transforms below. The rule applies that transform to the CURRENT bytes and compares the result with the sealed
// digest, so a declaration never skips a comparison: a deleted source still fails, and a changed one fails unless the change lies
// wholly inside what the named transform discards.
//   { "schema": 1, "normalized": [ { "file": "<sealed json>", "where"?: { "<entry key>": "<scalar>" }, "paths": ["<source>", ...],
//                                    "transform": <TRANSFORMS>, "reason": "<at least 10 characters>" } ] }
//   { "kind": "lf" }                                       CRLF -> LF, then sha256 of the bytes
//   { "kind": "json-drop", "drop": ["/a/*/b", ...] }        parse JSON, delete each key the pointer names ("*" = every array item
//                                                          or object member), sha256 of the canonical JSON (sorted keys, no spaces)
// Any other transform, or a malformed declaration, is a finding, and the path is compared on its raw bytes.
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";

export const SOURCE_CLOSURE_DECLARATION = ".rightkit/source-closures.json";
const DIGEST = /^sha256:[0-9a-f]{64}$/;
const SKIPPED_TREE = /(^|\/)(node_modules|vendor|target)\//;
const MAX_BYTES = 16 * 1024 * 1024;

const sha256 = (bytes) => `sha256:${createHash("sha256").update(bytes).digest("hex")}`;
const isPlain = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

/** JSON with every object's keys sorted, no whitespace, non-ASCII kept: Python's json.dumps(sort_keys=True, ensure_ascii=False, separators=(",", ":")). */
export function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (isPlain(value)) return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(",")}}`;
  return JSON.stringify(value);
}

/** A relative, forward-slash, non-escaping path; anything else is never read. */
export function safeRelative(value) {
  if (typeof value !== "string" || !value || value.length > 240 || value.includes("\\") || value.includes(":") || value.startsWith("/")) return false;
  return value.split("/").every((part) => part && part !== "." && part !== "..");
}

/** Every sealed record in a parsed JSON value, depth first: [{ pointer, record }]. */
export function* sealedRecords(node, pointer = "") {
  if (Array.isArray(node)) { for (let i = 0; i < node.length; i++) yield* sealedRecords(node[i], `${pointer}/${i}`); return; }
  if (!isPlain(node)) return;
  const closure = node.sourceClosure;
  if (isPlain(closure) && isPlain(closure.sources) && "digest" in closure) yield { pointer, record: node };
  for (const [key, value] of Object.entries(node)) if (key !== "sourceClosure") yield* sealedRecords(value, `${pointer}/${key.replace(/~/g, "~0").replace(/\//g, "~1")}`);
}

// Delete every key a "/a/*/b" pointer names from a parsed JSON value, in place.
function dropPointer(node, parts) {
  if (!parts.length || node === null || typeof node !== "object") return;
  const [head, ...rest] = parts;
  const keys = head === "*" ? Object.keys(node) : [head];
  for (const key of keys) {
    if (!Object.hasOwn(node, key)) continue;
    if (!rest.length) { if (Array.isArray(node)) continue; delete node[key]; } else dropPointer(node[key], rest);
  }
}

const TRANSFORM_KINDS = Object.freeze(["lf", "json-drop"]);
const validTransform = (t) => isPlain(t) && TRANSFORM_KINDS.includes(t.kind)
  && (t.kind !== "json-drop" || (Array.isArray(t.drop) && t.drop.length > 0 && t.drop.every((p) => typeof p === "string" && /^(\/[^/]+)+$/.test(p) && !p.endsWith("/*"))));

/** The digest of `bytes` under a declared transform (validated), or null when the bytes do not fit it (JSON that does not parse). */
export function transformedDigest(bytes, transform) {
  if (transform.kind === "lf") return sha256(Buffer.from(bytes.toString("latin1").replace(/\r\n/g, "\n"), "latin1"));
  let value;
  try { value = JSON.parse(bytes.toString("utf8")); } catch { return null; }
  for (const pointer of transform.drop) dropPointer(value, pointer.slice(1).split("/").map((p) => p.replace(/~1/g, "/").replace(/~0/g, "~")));
  return sha256(Buffer.from(canonicalJson(value), "utf8"));
}

/** Parse and validate the declaration. Returns { normalized: [...], problems: [string] }. */
export function readDeclaration(text) {
  if (text === null) return { normalized: [], problems: [] };
  let doc;
  try { doc = JSON.parse(text); } catch (error) { return { normalized: [], problems: [`is not valid JSON (${error.message})`] }; }
  const problems = [];
  if (!isPlain(doc) || (doc.schema !== undefined && doc.schema !== 1) || !Array.isArray(doc.normalized ?? [])) return { normalized: [], problems: ['expected { "schema": 1, "normalized": [...] }'] };
  const normalized = [];
  (doc.normalized ?? []).forEach((item, index) => {
    const bad = [];
    if (!isPlain(item)) bad.push("is not an object");
    else {
      if (!safeRelative(item.file)) bad.push("file must be a safe relative path");
      if (!Array.isArray(item.paths) || !item.paths.length || !item.paths.every(safeRelative)) bad.push("paths must be a non-empty list of safe relative paths");
      if (typeof item.reason !== "string" || item.reason.trim().length < 10) bad.push("reason must say why the digest is not over raw bytes (10+ characters)");
      if (!validTransform(item.transform)) bad.push(`transform must be one of ${TRANSFORM_KINDS.join(", ")} (json-drop with a non-empty drop list of /pointers); a reason alone exempts nothing`);
      if (item.where !== undefined && (!isPlain(item.where) || !Object.values(item.where).every((v) => v === null || typeof v !== "object"))) bad.push("where must map entry keys to scalars");
    }
    if (bad.length) problems.push(`normalized #${index}: ${bad.join("; ")}`);
    else normalized.push(item);
  });
  return { normalized, problems };
}

const labelOf = (record, pointer) => {
  for (const key of ["provider", "id", "name"]) if (typeof record[key] === "string" && record[key]) return `${record[key]} (${pointer || "/"})`;
  return pointer || "/";
};

/** The source-closure rule. */
export function sourceClosure({ root, files }) {
  const tracked = new Set(files ?? []);
  const out = [];
  const declarationText = tracked.has(SOURCE_CLOSURE_DECLARATION) ? fs.readFileSync(path.join(root, SOURCE_CLOSURE_DECLARATION), "utf8") : null;
  const declaration = readDeclaration(declarationText);
  for (const problem of declaration.problems) out.push({ file: SOURCE_CLOSURE_DECLARATION, subject: "declaration", problem: `${SOURCE_CLOSURE_DECLARATION} ${problem}`, fix: "repair the declaration; an unreadable exemption exempts nothing" });
  const hashes = new Map();
  const hashOf = (rel, transform = null) => {
    const key = `${rel}\0${transform ? JSON.stringify(transform) : ""}`;
    if (!hashes.has(key)) {
      const bytes = tracked.has(rel) ? fs.readFileSync(path.join(root, rel)) : null;
      hashes.set(key, bytes === null ? null : transform ? (transformedDigest(bytes, transform) ?? "unparseable") : sha256(bytes));
    }
    return hashes.get(key);
  };
  for (const file of [...tracked].filter((f) => f.endsWith(".json") && !SKIPPED_TREE.test(f)).sort()) {
    const full = path.join(root, file);
    let stat;
    try { stat = fs.statSync(full); } catch { continue; }
    if (!stat.isFile() || stat.size > MAX_BYTES) continue;
    const text = fs.readFileSync(full, "utf8");
    if (!text.includes('"sourceClosure"')) continue;
    let doc;
    try { doc = JSON.parse(text); } catch { continue; } // json-parse rules own syntax
    for (const { pointer, record } of sealedRecords(doc)) {
      const label = labelOf(record, pointer);
      const { sources, digest } = record.sourceClosure;
      const transforms = new Map();
      for (const n of declaration.normalized.filter((d) => d.file === file && Object.entries(d.where ?? {}).every(([k, v]) => record[k] === v))) {
        for (const p of n.paths) transforms.set(p, n.transform);
      }
      const problems = [];
      const malformed = Object.entries(sources).filter(([rel, value]) => !safeRelative(rel) || !DIGEST.test(value)).map(([rel]) => rel);
      if (malformed.length) problems.push(`unsafe path or malformed digest: ${malformed.slice(0, 5).join(", ")}`);
      const missing = [], stale = [];
      for (const [rel, value] of Object.entries(sources)) {
        if (malformed.includes(rel)) continue;
        const actual = hashOf(rel, transforms.get(rel) ?? null);
        if (actual === null) missing.push(rel);
        else if (actual !== value) stale.push(rel);
      }
      if (missing.length) problems.push(`${missing.length} source(s) no longer in the tree: ${missing.slice(0, 5).join(", ")}${missing.length > 5 ? ", ..." : ""}`);
      if (stale.length) problems.push(`${stale.length} source(s) changed since the record was sealed: ${stale.slice(0, 5).join(", ")}${stale.length > 5 ? ", ..." : ""}`);
      if (digest !== sha256(Buffer.from(canonicalJson(sources), "utf8"))) problems.push("sourceClosure.digest is not sha256 over the canonical JSON of sourceClosure.sources");
      if (problems.length) out.push({
        file, subject: label,
        problem: `${file} ${label}: sealed source closure stale/incomplete: ${problems.join("; ")}`,
        fix: "re-run the qualification that produced this record on the exact source and commit its output (never rewrite recorded digests by hand); or revert the source change",
      });
    }
  }
  return out;
}

export const SOURCE_CLOSURE_RULES = Object.freeze([{ id: "source-closure", gap: "SF-01", run: sourceClosure }]);
