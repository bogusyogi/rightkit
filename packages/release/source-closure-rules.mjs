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
// A digest a producer computes over a projection instead of the raw bytes is declared in .rightkit/source-closures.json by NAME
// only: { "schema": 1, "normalized": [ { "file": "<sealed json>", "transform": "<identity>", "paths"?: [...] } ] }. The identities
// are RightKit's (APPROVED_TRANSFORMS below), never the repository's: each binds the exact source path and record families it
// applies to, the keys it drops, and an input schema of the producer's real semantics, so a field the schema does not know is
// refused rather than dropped. The rule applies the transform to the CURRENT bytes and compares with the sealed digest: a
// deleted source fails, a change outside the dropped keys fails, input outside the schema fails. A repository never supplies
// pointers; an unknown identity, or a declaration that brings its own transform, is a finding and the path is compared raw.
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

// Legion's provider registry, as its producers read it before digesting (Orthic-Labs/legion, scripts/ci). A provider's benchmark
// holds exactly these keys today (src/registry/providers.json at cfe76b4 and d27a3d4); a new key is refused, not dropped.
const LEGION_BENCHMARK_KEYS = Object.freeze(["status", "requiredForCleanClaim", "qualificationDigest", "qualification"]);
const legionRegistryProblems = (value) => {
  if (!isPlain(value) || !Array.isArray(value.providers)) return "the registry is not an object with a providers array";
  for (const [i, provider] of value.providers.entries()) {
    if (!isPlain(provider)) return `providers[${i}] is not an object`;
    if (!isPlain(provider.benchmark)) return `providers[${i}].benchmark is not an object`;
    const unknown = Object.keys(provider.benchmark).filter((k) => !LEGION_BENCHMARK_KEYS.includes(k));
    if (unknown.length) return `providers[${i}].benchmark has key(s) ${unknown.join(", ")} the approved projection does not know`;
  }
  return null;
};

/**
 * The approved transform identities. `path` is the one source each applies to (null: the declaration names text paths, for the
 * lossless line-ending identity only); `families` the record `family` values it applies to (null: any); `schema` refuses input
 * outside the producer's semantics; `project` is the producer's projection.
 */
export const APPROVED_TRANSFORMS = Object.freeze({
  // validate-compatibility-report-accuracy.py:104-111, audit-jvm-sdk-accuracy.py:84-92, audit-reasoning-runtime-registration.py:301-308:
  // each pops providers[].benchmark whole and digests the canonical JSON of the rest.
  "legion-provider-registry/without-benchmark/v1": {
    path: "src/registry/providers.json", families: ["compatibility-report", "code-jvm-sdk", "reasoning-runtime"], json: true,
    schema: legionRegistryProblems,
    project: (v) => ({ ...v, providers: v.providers.map(({ benchmark: _dropped, ...rest }) => rest) }),
  },
  // audit-mobile-native-enrollment.py:74-89 (registry_semantics, used by audit-mobile-sdk-accuracy.py:10-13) and
  // audit-supply-chain-native-accuracy.py:68-73,94-96 (semantics): each drops benchmark status, qualificationDigest and qualification
  // and keeps requiredForCleanClaim.
  "legion-provider-registry/without-qualification-metadata/v1": {
    path: "src/registry/providers.json", families: ["code-mobile-sdk", "supply-chain-native"], json: true,
    schema: legionRegistryProblems,
    project: (v) => ({ ...v, providers: v.providers.map((p) => {
      const { status: _s, qualificationDigest: _d, qualification: _q, ...benchmark } = p.benchmark;
      return { ...p, benchmark };
    }) }),
  },
  // CRLF -> LF only: it drops no field, so the declaration names the text paths.
  "text-lf/v1": { path: null, families: null, json: false },
});

// The canonical JSON form must equal Python's; a non-integer number would print differently, so it is refused.
const numbersExact = (v) => Array.isArray(v) ? v.every(numbersExact) : isPlain(v) ? Object.values(v).every(numbersExact) : typeof v !== "number" || Number.isSafeInteger(v);

/** The digest of `bytes` under an approved identity: { digest } or { problem } when the input is outside the identity's schema. */
export function transformedDigest(bytes, identity) {
  const transform = APPROVED_TRANSFORMS[identity];
  if (!transform) return { problem: `transform "${identity}" is not an approved RightKit identity` };
  if (!transform.json) return { digest: sha256(Buffer.from(bytes.toString("latin1").replace(/\r\n/g, "\n"), "latin1")) };
  let value;
  try { value = JSON.parse(bytes.toString("utf8")); } catch (error) { return { problem: `is not JSON (${error.message})` }; }
  const problem = transform.schema(value) ?? (numbersExact(value) ? null : "holds a non-integer number, whose canonical form is not exact");
  if (problem) return { problem: `${problem}; ${identity} refuses it` };
  return { digest: sha256(Buffer.from(canonicalJson(transform.project(value)), "utf8")) };
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
      const approved = typeof item.transform === "string" ? APPROVED_TRANSFORMS[item.transform] : undefined;
      if (!approved) bad.push(`transform must name an approved RightKit identity (${Object.keys(APPROVED_TRANSFORMS).join(", ")}); a repository never supplies its own transform or pointers`);
      else if (approved.path !== null && (item.paths !== undefined || item.where !== undefined)) bad.push(`${item.transform} binds its own source and families; drop paths and where`);
      else if (approved.path === null && (!Array.isArray(item.paths) || !item.paths.length || !item.paths.every(safeRelative))) bad.push(`${item.transform} needs paths: a non-empty list of safe relative paths`);
      const unknownKeys = isPlain(item) ? Object.keys(item).filter((k) => !["file", "transform", "paths", "where"].includes(k)) : [];
      if (unknownKeys.length) bad.push(`unknown key(s) ${unknownKeys.join(", ")}`);
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
  // { digest } | { problem } | null (not in the tree), per source and identity.
  const hashOf = (rel, identity = null) => {
    const key = `${rel}\0${identity ?? ""}`;
    if (!hashes.has(key)) {
      const bytes = tracked.has(rel) ? fs.readFileSync(path.join(root, rel)) : null;
      hashes.set(key, bytes === null ? null : identity ? transformedDigest(bytes, identity) : { digest: sha256(bytes) });
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
      for (const n of declaration.normalized.filter((d) => d.file === file)) {
        const approved = APPROVED_TRANSFORMS[n.transform];
        if (approved.path === null) { for (const p of n.paths) transforms.set(p, n.transform); continue; }
        if (approved.families.includes(record.family)) transforms.set(approved.path, n.transform);
      }
      const problems = [];
      const malformed = Object.entries(sources).filter(([rel, value]) => !safeRelative(rel) || !DIGEST.test(value)).map(([rel]) => rel);
      if (malformed.length) problems.push(`unsafe path or malformed digest: ${malformed.slice(0, 5).join(", ")}`);
      const missing = [], stale = [], refused = [];
      for (const [rel, value] of Object.entries(sources)) {
        if (malformed.includes(rel)) continue;
        const identity = transforms.get(rel) ?? null;
        const actual = hashOf(rel, identity);
        if (actual === null) missing.push(rel);
        else if (actual.problem) refused.push(`${rel} ${actual.problem}`);
        else if (actual.digest !== value) stale.push(identity ? `${rel} (under ${identity})` : rel);
      }
      if (refused.length) problems.push(`${refused.length} source(s) outside their approved transform: ${refused.slice(0, 3).join("; ")}`);
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
