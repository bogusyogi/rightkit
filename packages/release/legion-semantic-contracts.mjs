// Legion's semantic catalog and provenance contracts, as RightKit-owned class A rules for the app check. Legion (Orthic-Labs/legion)
// enforces each of these with its own code only after its CI has built `legion-dev` (minutes into a Windows or macOS job); the
// relations themselves are pure data over committed files and git history, so they are read here before any build, by trusted
// adapters: no Legion code runs, no network, no clock. Each relation is copied from the producer that owns its meaning, cited by
// path:line at Orthic-Labs/legion 26459d5 (unchanged at the historical commits below unless stated), and every exemption it honours
// is that producer's, fixed here, never one a repository chooses.
//
//   legion-schema-projection  LG-02  a JSON schema that Legion's schema generator byte-compares (`generate-schemas --check`) is not
//                                    pinned against line-ending conversion in .gitattributes (a Windows checkout rewrites it to
//                                    CRLF and the check reports SCHEMA DRIFT: run 31302567212, jobs 93217599020 and 93217599034),
//                                    or one of its `enum` arrays is not the code-owned string list it is generated from
//   legion-legacy-tokens      LG-03  a legacy token (seer, nemesis, forge, sentinel, sorcerer) in a tracked path or file that
//                                    src/config/naming-legacy-allowlist.json does not classify, or classifies with a different exact
//                                    occurrence count (jobs 120707319434/491, 118763807969/988)
//   legion-retirement-ledger  LG-04  a skill, script, hook, doctrine, agent-rule or native command file deleted after c1d80c9d with no
//                                    row in docs/provenance/retirements.md (job 119412709257/288)
//   legion-authority-body     LG-05  an agent card that does not carry its doctrine body verbatim (job 117161756781/757149), a
//                                    variant card without its roster bodies, or a council-seat card whose body differs from doctrine
//
// A contract applies when its producer or its data file is in the tree (ANCHORS); once it applies, a missing input is a finding.
// What stays with Legion's own gate: anything its generators compute (the rest of a schema's shape, the plugin surface, skill
// catalogs and manifests: canonical regeneration runs `legion-dev`, class B), and the naming check's semantic half (authority sets
// read from Rust and README), which is not this ledger.
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";

// git's repository-location variables are dropped, as app-check.mjs does: a hook runs with some of them set.
const GIT_LOCATION = /^GIT_(?:DIR|WORK_TREE|INDEX_FILE|COMMON_DIR|PREFIX|OBJECT_DIRECTORY|ALTERNATE_OBJECT_DIRECTORIES|NAMESPACE)$/i;
const gitEnv = () => Object.fromEntries(Object.entries(process.env).filter(([key]) => !GIT_LOCATION.test(key)));
function git(repo, args, { input } = {}) {
  const r = spawnSync("git", ["-c", "core.quotepath=off", ...args], { cwd: repo, env: gitEnv(), encoding: "utf8", input, maxBuffer: 256 * 1024 * 1024 });
  return { ok: !r.error && r.status === 0, out: String(r.stdout ?? ""), err: r.error?.message ?? String(r.stderr ?? "").trim() };
}

const readBytes = (root, rel) => { try { return fs.readFileSync(path.join(root, rel)); } catch { return null; } };
const isFile = (root, rel) => { try { return fs.statSync(path.join(root, rel)).isFile(); } catch { return false; } };
/** Legion's read_text (checks/mod.rs:117-123): null for a NUL byte or invalid UTF-8; a BOM is kept, as Rust keeps it. */
export function decodeText(bytes) {
  if (bytes === null || bytes.includes(0)) return null;
  try { return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes); } catch { return null; }
}
const readText = (root, rel) => decodeText(readBytes(root, rel));

/** The files and paths each contract is anchored to: any of them in the tree makes the contract apply. */
export const ANCHORS = Object.freeze({
  "legion-schema-projection": ["engine/bins/legion-dev/src/generators/schemas.rs", "scripts/generate-schemas.mjs"],
  "legion-legacy-tokens": ["engine/bins/legion-dev/src/checks/canonical_names.rs", "src/config/naming-legacy-allowlist.json"],
  "legion-retirement-ledger": ["engine/bins/legion-dev/src/checks/retirements.rs", "docs/provenance/retirements.md"],
  "legion-authority-body": ["engine/bins/legion-dev/src/checks/authority_parity.rs", "src/roster/alchemist.md"],
});
const applies = (id, files) => ANCHORS[id].some((anchor) => files.has(anchor));

// ---- LG-02 schema projection ------------------------------------------------------------------------------------------------------

/**
 * The schemas each generator writes and compares byte for byte, and the enum arrays it projects from code. Current producer:
 * engine/bins/legion-dev/src/generators/schemas.rs: build_schemas() :202-216 names the targets, run() :224-238 compares the committed
 * text with the rendering, :31 projects PROVIDER_STATUS, :81 EVIDENCE_STRENGTH and :82 SECURITY_VERDICTS, all from
 * engine/crates/legion-contracts/src/enums.rs (:15, :83, :160). Historical producer (Orthic-Labs/legion 8c275f0c, the failing
 * run): scripts/generate-schemas.mjs buildSchemas() :157-163 and main() :189-216 (`committed !== generated`, :195); its enums are
 * JavaScript modules, which a class A rule does not evaluate, so only its line-ending pin is checked here.
 */
export const SCHEMA_PROJECTIONS = Object.freeze([
  Object.freeze({
    producer: "engine/bins/legion-dev/src/generators/schemas.rs",
    targets: ["src/schemas/provider-result-v1.schema.json", "src/schemas/security-verdict-v1.schema.json", "src/schemas/platform/web-actor-fixture-v1.schema.json"],
    enumSource: "engine/crates/legion-contracts/src/enums.rs",
    enums: [
      { target: "src/schemas/provider-result-v1.schema.json", pointer: "/properties/status/enum", constant: "PROVIDER_STATUS" },
      { target: "src/schemas/security-verdict-v1.schema.json", pointer: "/properties/evidenceStrength/enum", constant: "EVIDENCE_STRENGTH" },
      { target: "src/schemas/security-verdict-v1.schema.json", pointer: "/properties/verdict/enum", constant: "SECURITY_VERDICTS" },
    ],
  }),
  Object.freeze({
    producer: "scripts/generate-schemas.mjs",
    targets: ["schemas/provider-result-v1.schema.json", "schemas/security-verdict-v1.schema.json", "schemas/platform/web-actor-fixture-v1.schema.json"],
    enumSource: null,
    enums: [],
  }),
]);

/**
 * The string list of `pub const NAME: &[&str] = &[ "a", "b", ... ];` in Rust source, read as data: string literals with the simple
 * escapes, commas, whitespace and line comments only. Returns { values } or { problem }; anything else is refused, never evaluated.
 */
export function rustStrList(source, name) {
  const head = new RegExp(`(?:^|\\n)[ \\t]*pub(?:\\([^)\\n]*\\))?[ \\t]+const[ \\t]+${name}[ \\t]*:[ \\t]*&(?:'static[ \\t]+)?\\[[ \\t]*&(?:'static[ \\t]+)?str[ \\t]*\\][ \\t]*=[ \\t\\n]*&\\[`, "g");
  const starts = [...source.matchAll(head)].map((m) => m.index + m[0].length);
  if (starts.length !== 1) return { problem: starts.length ? `declares ${name} ${starts.length} times` : `declares no \`pub const ${name}: &[&str] = &[...]\`` };
  const token = /\s+|\/\/[^\n]*|"((?:[^"\\\n]|\\["\\nt])*)"|,|\]/y;
  token.lastIndex = starts[0];
  const values = [];
  for (;;) {
    const at = token.lastIndex;
    const m = token.exec(source);
    if (!m || m.index !== at) return { problem: `${name} is not a plain list of string literals (at offset ${at})` };
    if (m[0] === "]") return { values };
    if (m[1] !== undefined) values.push(m[1].replace(/\\(["\\nt])/g, (_, c) => ({ n: "\n", t: "\t" })[c] ?? c));
  }
}

const pointerGet = (value, pointer) => pointer.split("/").slice(1).map((part) => part.replace(/~1/g, "/").replace(/~0/g, "~"))
  .reduce((node, key) => (node !== null && typeof node === "object" && Object.hasOwn(node, key) ? node[key] : undefined), value);

/** git's text and eol attributes of `rels`, as the exported tree's .gitattributes (and the repository's info/attributes) set them. */
function attributesOf(repo, root, rels) {
  const r = git(repo, ["--work-tree", root, "check-attr", "-z", "--stdin", "text", "eol"], { input: rels.join("\0") });
  if (!r.ok) throw new Error(`git check-attr failed: ${r.err}`);
  const fields = r.out.split("\0");
  const seen = new Map();
  for (let i = 0; i + 2 < fields.length; i += 3) seen.set(fields[i], { ...seen.get(fields[i]), [fields[i + 1]]: fields[i + 2] });
  return seen;
}

export function legionSchemaProjection({ root, files, repo = process.cwd() }) {
  const tracked = new Set(files ?? []);
  if (!applies("legion-schema-projection", tracked)) return [];
  const out = [];
  for (const projection of SCHEMA_PROJECTIONS) {
    if (!tracked.has(projection.producer)) continue;
    const present = projection.targets.filter((t) => tracked.has(t));
    for (const target of projection.targets.filter((t) => !tracked.has(t))) {
      out.push({ file: target, subject: "missing", problem: `${target} is missing: ${projection.producer} generates it and its --check reads it`, fix: `regenerate it with Legion's schema generator and commit it` });
    }
    const attrs = present.length ? attributesOf(repo, root, present) : new Map();
    for (const target of present) {
      const { text = "unspecified", eol = "unspecified" } = attrs.get(target) ?? {};
      if (text === "unset" || eol === "lf") continue;
      out.push({
        file: target, subject: "eol",
        problem: `${target} is byte-compared by ${projection.producer} --check but not pinned against line-ending conversion (text: ${text}, eol: ${eol}): a Windows checkout with core.autocrlf writes it as CRLF and the check reports "SCHEMA DRIFT: ${target} is not up to date with the code-owned enums" though nothing drifted`,
        fix: `pin it in .gitattributes: \`* -text\` (Legion's choice) or \`${target} -text\` / \`eol=lf\``,
      });
    }
    if (!projection.enumSource || !projection.enums.length) continue;
    const source = tracked.has(projection.enumSource) ? readText(root, projection.enumSource) : null;
    if (source === null) {
      out.push({ file: projection.enumSource, subject: "missing", problem: `${projection.enumSource} is missing or not UTF-8 text: the code-owned enums ${projection.enums.map((e) => e.constant).join(", ")} of ${projection.producer} cannot be read`, fix: "restore it, or retire the projection in Legion and in RightKit's contract together" });
      continue;
    }
    for (const { target, pointer, constant } of projection.enums) {
      if (!present.includes(target)) continue;
      const list = rustStrList(source, constant);
      if (list.problem) { out.push({ file: projection.enumSource, subject: `enum:${constant}`, problem: `${projection.enumSource}: ${list.problem}; the projection into ${target}${pointer} cannot be checked`, fix: `keep ${constant} a literal \`&[&str]\` list` }); continue; }
      let schema;
      try { schema = JSON.parse(readText(root, target) ?? ""); } catch (error) { out.push({ file: target, subject: "json", problem: `${target} is not JSON (${error.message})`, fix: "regenerate it" }); continue; }
      const actual = pointerGet(schema, pointer);
      if (JSON.stringify(actual) === JSON.stringify(list.values)) continue;
      out.push({
        file: target, subject: `enum:${pointer}`,
        problem: `${target}${pointer} is ${JSON.stringify(actual)} but ${projection.enumSource} ${constant} is ${JSON.stringify(list.values)}: the schema is not the projection of its code-owned enum`,
        fix: `regenerate the schemas with Legion's generator (generate-schemas) and commit them with the enum change`,
      });
    }
  }
  return out;
}

// ---- LG-03 legacy occurrence ledger -----------------------------------------------------------------------------------------------

// engine/bins/legion-dev/src/checks/canonical_names.rs: TOKENS :17, SKIP_PREFIXES :18-28 (the `records/*/evidence/` and
// `evidence/` rows arrived after 118763807969 failed on immutable evidence at b797eec5), is_active_source :46-52, occurrences :56-82,
// matching_rule :84-99, allowlist_issues :101-171, the scan :388-458, skipped_naming_path :494-510.
export const LEGACY_TOKENS = Object.freeze(["seer", "nemesis", "forge", "sentinel", "sorcerer"]);
export const LEGACY_SKIP_PREFIXES = Object.freeze([
  ".git/", ".agent/", ".audit/", ".cache/", "docs/foundation/", "node_modules/",
  "qualification/provider-accuracy/logs/", "qualification/provider-accuracy/evidence/", "qualification/provider-accuracy/records/*/evidence/",
]);
export const LEGACY_ALLOWLIST = "src/config/naming-legacy-allowlist.json";
const ACTIVE_SOURCE = /\.(?:cjs|js|json|jsx|md|mjs|py|sh|toml|ts|tsx|ya?ml)$/i;

/** skipped_naming_path: a `*` covers exactly one non-empty directory, never its siblings. */
export function legacySkipped(rel) {
  const p = `${rel}/`;
  return LEGACY_SKIP_PREFIXES.some((prefix) => {
    const star = prefix.indexOf("*/");
    if (star < 0) return p.startsWith(prefix);
    const parent = prefix.slice(0, star), child = prefix.slice(star + 2);
    if (!p.startsWith(parent)) return false;
    const rest = p.slice(parent.length), slash = rest.indexOf("/");
    return slash > 0 && rest.slice(slash + 1).startsWith(child);
  });
}

const isAlnum = (b) => (b >= 48 && b <= 57) || (b >= 65 && b <= 90) || (b >= 97 && b <= 122);
/**
 * The line of every whole-word, ASCII case-insensitive occurrence of each token (canonical_names.rs occurrences, byte for byte):
 * { token: [line, ...] } for every token in `tokens`.
 */
export function tokenLinesAll(text, tokens = LEGACY_TOKENS) {
  const found = Object.fromEntries(tokens.map((t) => [t, []]));
  const folded = text.toLowerCase(); // a superset filter only: ASCII matches survive Unicode lowercasing
  const wanted = tokens.filter((t) => folded.includes(t.toLowerCase()));
  if (!wanted.length) return found;
  const bytes = Buffer.from(text, "utf8");
  const lower = new Uint8Array(bytes.length);
  for (let i = 0; i < bytes.length; i++) { const b = bytes[i]; lower[i] = b >= 65 && b <= 90 ? b + 32 : b; }
  const low = Buffer.from(lower.buffer, lower.byteOffset, lower.length);
  for (const token of wanted) {
    const needle = Buffer.from(token.toLowerCase(), "utf8");
    let line = 1, counted = 0;
    for (let at = low.indexOf(needle, 0); at >= 0; at = low.indexOf(needle, at + 1)) {
      const end = at + needle.length;
      if ((at === 0 || !isAlnum(bytes[at - 1])) && (end >= bytes.length || !isAlnum(bytes[end]))) {
        for (; counted < at; counted++) if (bytes[counted] === 10) line++;
        found[token].push(line);
      }
    }
  }
  return found;
}
export const tokenLines = (text, token) => tokenLinesAll(text, [token])[token];

const ruleTargets = (rule, rel) => (typeof rule.path === "string" && rule.path === rel) || (typeof rule.pathPrefix === "string" && rel.startsWith(rule.pathPrefix));
const matchingRule = (rules, rel, token) => rules.find((rule) => rule && typeof rule === "object" && ruleTargets(rule, rel) && Array.isArray(rule.tokens) && rule.tokens.includes(token));
const legacyFinding = (rel, line, token, reason, key) => ({
  file: rel, subject: `${token ?? "-"}:${key}`,
  problem: `${rel}${line ? `:${line}` : ""}: ${reason}${token ? ` (${token})` : ""}`,
  fix: `classify it in ${LEGACY_ALLOWLIST} (path, tokens, exact occurrences, class, reason) or rename the occurrence`,
});

export function legionLegacyTokens({ root, files }) {
  const tracked = new Set(files ?? []);
  if (!applies("legion-legacy-tokens", tracked)) return [];
  let allowlist;
  try { allowlist = JSON.parse(readText(root, LEGACY_ALLOWLIST) ?? ""); }
  catch (error) { return [{ file: LEGACY_ALLOWLIST, subject: "ledger", problem: `${LEGACY_ALLOWLIST} is missing or not JSON (${error.message}): the legacy-token ledger cannot be read`, fix: "restore the ledger; Legion's canonical-names check fails without it too" }]; }
  const rules = Array.isArray(allowlist?.rules) ? allowlist.rules : [];
  const scanned = [...tracked].filter((rel) => !legacySkipped(rel) && isFile(root, rel)).sort();
  // One read per file: the occurrences of every token in its path and its text (null text: binary or not UTF-8).
  const seen = new Map(scanned.map((rel) => {
    const text = readText(root, rel);
    return [rel, { name: tokenLinesAll(rel), text: text === null ? null : tokenLinesAll(text) }];
  }));
  const ledger = [];
  // The ledger's own shape: every rule has a target, tokens, class and reason; names known tokens; matches files; is used.
  rules.forEach((rule, index) => {
    const at = `${LEGACY_ALLOWLIST}#rules[${index}]`;
    const target = typeof rule?.path === "string" ? rule.path : typeof rule?.pathPrefix === "string" ? rule.pathPrefix : null;
    const shaped = target !== null && Array.isArray(rule.tokens) && rule.tokens.length && typeof rule.class === "string" && typeof rule.reason === "string";
    if (!shaped) { ledger.push({ ...legacyFinding(at, null, null, "legacy allowlist rule lacks target, tokens, class, or reason", "shape"), file: LEGACY_ALLOWLIST, subject: `rules[${index}]:shape` }); return; }
    const candidates = scanned.filter((rel) => ruleTargets(rule, rel));
    if (!candidates.length) { ledger.push({ ...legacyFinding(target, null, null, "legacy allowlist target does not exist or matches no files", "no-target"), file: LEGACY_ALLOWLIST, subject: `${target}:no-target` }); return; }
    for (const token of rule.tokens) {
      if (typeof token !== "string") continue;
      if (!LEGACY_TOKENS.includes(token)) { ledger.push({ ...legacyFinding(at, null, token, "legacy allowlist names unknown token", "unknown-token"), file: LEGACY_ALLOWLIST, subject: `rules[${index}]:${token}:unknown-token` }); continue; }
      const observed = candidates.reduce((sum, rel) => sum + seen.get(rel).name[token].length + (seen.get(rel).text?.[token].length ?? 0), 0);
      if (!observed) ledger.push({ ...legacyFinding(target, null, token, "legacy allowlist token exemption is unused", "unused"), file: LEGACY_ALLOWLIST, subject: `${target}:${token}:unused` });
    }
  });
  const out = [];
  for (const rel of scanned) {
    const { name, text } = seen.get(rel);
    for (const token of LEGACY_TOKENS) if (name[token].length && !matchingRule(rules, rel, token)) out.push(legacyFinding(rel, null, token, "unclassified legacy filename", "filename"));
    if (text === null) { if (ACTIVE_SOURCE.test(rel)) out.push(legacyFinding(rel, null, null, "active source cannot be decoded for naming scan", "decode")); continue; }
    for (const token of LEGACY_TOKENS) {
      const lines = text[token];
      const rule = matchingRule(rules, rel, token);
      if (!lines.length && !rule) continue;
      const exact = rule?.occurrences && Number.isInteger(rule.occurrences[token]) ? rule.occurrences[token] : null;
      if (lines.length && !rule) out.push(legacyFinding(rel, lines[0], token, "unclassified legacy token", "unclassified"));
      else if (lines.length && typeof rule.path === "string" && rule.class !== "R5" && exact === null) out.push(legacyFinding(rel, lines[0], token, "active exact-path allowlist lacks occurrence count", "no-count"));
      else if (exact !== null && lines.length !== exact) out.push(legacyFinding(rel, lines[0] ?? null, token, `legacy token occurrence count differs: expected ${exact}, found ${lines.length}`, "count"));
    }
  }
  return [...ledger, ...out];
}

// ---- LG-04 retirement ledger ------------------------------------------------------------------------------------------------------

// engine/bins/legion-dev/src/checks/retirements.rs: BASELINE :20 (c1d80c9d190e0f45dfd630d4d65e8bed748381ce), LEDGER :21,
// EXEMPT_PREFIXES :22, is_ledger_surface :25-40, deleted_paths :44-54 (still absent at HEAD), ledger_entries :58-77, glob_matches
// :80-100, is_covered :102-111, the history read :160-178 (`git log -M --diff-filter=D --name-only --format= BASELINE..HEAD`).
// Legion skips locally when history is shallow and fails in CI; a class A rule cannot tell, so it always fails: unverified is not clean.
export const RETIREMENT_BASELINE = "c1d80c9d";
const RETIREMENT_BASELINE_DEFAULT = RETIREMENT_BASELINE;
export const RETIREMENT_LEDGER = "docs/provenance/retirements.md";
const RETIREMENT_EXEMPT = Object.freeze(["skills/manifests/"]);

export function onLedgerSurface(rel) {
  if (RETIREMENT_EXEMPT.some((p) => rel.startsWith(p))) return false;
  return /^(?:skills\/|scripts\/|hooks\/|doctrine\/|docs\/agent-rules)/.test(rel) || /^engine\/bins\/[^/]+\/src\/commands\//.test(rel);
}

/** Backtick spans in the first column of every table row. */
export function ledgerEntries(text) {
  const entries = [];
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line.startsWith("|")) continue;
    const cell = line.split("|")[1];
    if (cell === undefined) continue;
    for (const m of cell.matchAll(/`([^`]+)`/g)) {
      const entry = m[1].trim().replace(/^(?:\.\/)+/, "");
      if (entry) entries.push(entry);
    }
  }
  return entries;
}

const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\/-]/g, "\\$&");
/** `*` within a segment, `**` across segments, then the path or anything below it. */
export function ledgerGlob(pattern, rel) {
  let re = "^";
  for (let i = 0; i < pattern.length;) {
    if (pattern[i] === "*") { if (pattern[i + 1] === "*") { re += ".*"; i += 2; } else { re += "[^/]*"; i += 1; } }
    else re += escapeRe(pattern[i++]);
  }
  return new RegExp(`${re}(?:/.*)?$`, "s").test(rel);
}
export const ledgerCovers = (rel, entries) => entries.some((entry) => {
  if (entry.includes("*")) return ledgerGlob(entry, rel);
  const e = entry.replace(/\/+$/, "");
  return rel === e || rel.startsWith(`${e}/`);
});

/** The LG-04 rule bound to a baseline prefix: RETIREMENT_BASELINE in the app check; a fixture's own baseline in tests. */
export const retirementLedgerFor = (baseline) => ({ root, commit, files, repo }) => legionRetirementLedger({ root, commit, files, repo, baseline });

export function legionRetirementLedger({ root, commit, files, repo = process.cwd(), baseline: RETIREMENT_BASELINE = RETIREMENT_BASELINE_DEFAULT }) {
  const tracked = new Set(files ?? []);
  if (!applies("legion-retirement-ledger", tracked)) return [];
  const unverified = (why) => [{ file: RETIREMENT_LEDGER, subject: "history", problem: `cannot enumerate files deleted since ${RETIREMENT_BASELINE}: ${why}; a ledger that is not checked is not clean`, fix: `check with full history (actions/checkout fetch-depth: 0, or git fetch --unshallow)` }];
  const shallow = git(repo, ["rev-parse", "--is-shallow-repository"]);
  if (!shallow.ok) return unverified(`not a git repository (${shallow.err})`);
  if (shallow.out.trim() !== "false") return unverified("the clone is shallow");
  // The baseline is the one commit object whose id starts with c1d80c9d, looked up among objects only: a ref someone names
  // c1d80c9d (which git would otherwise prefer) never moves the range.
  const candidates = git(repo, ["rev-parse", `--disambiguate=${RETIREMENT_BASELINE}`]).out.split("\n").map((l) => l.trim())
    .filter((oid) => /^[0-9a-f]{40,64}$/.test(oid) && git(repo, ["cat-file", "-t", oid]).out.trim() === "commit");
  if (candidates.length !== 1) return unverified(candidates.length ? `${RETIREMENT_BASELINE} names ${candidates.length} commits here` : `${RETIREMENT_BASELINE} is not a commit here`);
  const base = { out: candidates[0] };
  const isCommit = commit && git(repo, ["cat-file", "-t", commit]).out.trim() === "commit";
  const end = isCommit ? commit : "HEAD";
  if (!git(repo, ["merge-base", "--is-ancestor", base.out.trim(), end]).ok) return unverified(`${RETIREMENT_BASELINE} is not an ancestor of ${end === "HEAD" ? "HEAD" : end.slice(0, 12)}`);
  const log = git(repo, ["log", "-M", "--diff-filter=D", "--name-only", "--format=", `${base.out.trim()}..${end}`]);
  if (!log.ok) throw new Error(`git log failed: ${log.err}`);
  let names = log.out;
  // --worktree checks a tree: what HEAD has and the tree lacks is deleted too.
  if (!isCommit && commit) {
    const diff = git(repo, ["diff", "-M", "--diff-filter=D", "--name-only", "HEAD", commit]);
    if (!diff.ok) throw new Error(`git diff failed: ${diff.err}`);
    names += `\n${diff.out}`;
  }
  const dirs = new Set();
  for (const rel of tracked) for (let i = rel.indexOf("/"); i > 0; i = rel.indexOf("/", i + 1)) dirs.add(rel.slice(0, i));
  const exists = (rel) => tracked.has(rel) || dirs.has(rel);
  const deleted = [...new Set(names.split("\n").map((l) => l.trim()).filter((l) => l && onLedgerSurface(l) && !exists(l)))].sort();
  if (!deleted.length) return [];
  const ledger = tracked.has(RETIREMENT_LEDGER) ? readText(root, RETIREMENT_LEDGER) : null;
  if (ledger === null) return [{ file: RETIREMENT_LEDGER, subject: "ledger", problem: `${RETIREMENT_LEDGER} is missing or not UTF-8 text, and ${deleted.length} path(s) were deleted since ${RETIREMENT_BASELINE}`, fix: "restore the ledger" }];
  const entries = ledgerEntries(ledger);
  return deleted.filter((rel) => !ledgerCovers(rel, entries)).map((rel) => ({
    file: rel, subject: "row",
    problem: `${rel}: deleted since ${RETIREMENT_BASELINE} with no row in ${RETIREMENT_LEDGER}`,
    fix: `add a row whose first column names the path, a parent directory or a glob that matches it, with its successor or why it was dropped`,
  }));
}

// ---- LG-05 authority body parity --------------------------------------------------------------------------------------------------

// engine/bins/legion-dev/src/checks/authority_parity.rs: ROLES :14, VARIANTS :18-19 (card, base role), cards() :23-29,
// body_after_frontmatter :262-267, the council seat's body equality :289-301, check_method_embedded :336-379 (the doctrine body
// verbatim in the card, a variant's two roster bodies too, and no `doctrine/` or `src/roster/` outside the embedded method).
export const AUTHORITY_CARDS = Object.freeze([["sage", "sage"], ["alchemist", "alchemist"], ["oracle", "oracle"], ["sentry", "sentry"], ["alchemist-specialist", "alchemist"]]);
const AUTHORITY_VARIANTS = Object.freeze({ "alchemist-specialist": "alchemist" });

/** The text after a leading `---` frontmatter block, CRLF as LF, trimmed. */
export const bodyAfterFrontmatter = (text) => text.replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n/, "").replace(/\r\n/g, "\n").trim();

export function legionAuthorityBody({ root, files }) {
  const tracked = new Set(files ?? []);
  if (!applies("legion-authority-body", tracked)) return [];
  const out = [];
  const need = (rel) => {
    const text = tracked.has(rel) ? readText(root, rel) : null;
    if (text === null) out.push({ file: rel, subject: "missing", problem: `${rel} is missing or not UTF-8 text; the authority cards cannot be checked against it`, fix: "restore it, or retire the role in Legion and in RightKit's contract together" });
    return text;
  };
  for (const [card, doctrineId] of AUTHORITY_CARDS) {
    const doctrine = need(`doctrine/${doctrineId}.md`);
    const raw = need(`agents/${card}.md`);
    if (doctrine === null || raw === null) continue;
    const body = bodyAfterFrontmatter(doctrine);
    const text = raw.replace(/\r\n/g, "\n");
    if (!text.includes(body)) out.push({ file: `agents/${card}.md`, subject: `method:${doctrineId}`, problem: `${card}: agents/${card}.md does not contain the doctrine/${doctrineId}.md body verbatim under ## Method`, fix: `copy the doctrine/${doctrineId}.md body (after its frontmatter) into agents/${card}.md under ## Method` });
    const base = AUTHORITY_VARIANTS[card];
    if (base) {
      for (const rosterId of [base, card]) {
        const roster = need(`src/roster/${rosterId}.md`);
        if (roster !== null && !text.includes(bodyAfterFrontmatter(roster))) out.push({ file: `agents/${card}.md`, subject: `roster:${rosterId}`, problem: `${card}: agents/${card}.md does not contain the src/roster/${rosterId}.md body verbatim under ## Roster`, fix: `copy the src/roster/${rosterId}.md body into agents/${card}.md under ## Roster` });
      }
    }
    const outside = text.replace(body, () => "");
    for (const forbidden of ["doctrine/", "src/roster/"]) if (outside.includes(forbidden)) out.push({ file: `agents/${card}.md`, subject: `outside:${forbidden}`, problem: `${card}: agents/${card}.md refers to \`${forbidden}\` outside its embedded method; the installed card cannot read that path`, fix: "name the rule itself, not the repository path" });
  }
  const seatDoctrine = need("doctrine/council-seat.md");
  const seatCard = need("agents/council-seat.md");
  if (seatDoctrine !== null && seatCard !== null && bodyAfterFrontmatter(seatDoctrine) !== bodyAfterFrontmatter(seatCard)) {
    out.push({ file: "agents/council-seat.md", subject: "body", problem: "council-seat: agents/council-seat.md body differs from doctrine/council-seat.md body; the card must carry the doctrine body verbatim", fix: "copy the doctrine/council-seat.md body into the card" });
  }
  return out;
}

export const LEGION_SEMANTIC_RULES = Object.freeze([
  { id: "legion-schema-projection", gap: "LG-02", run: legionSchemaProjection },
  { id: "legion-legacy-tokens", gap: "LG-03", run: legionLegacyTokens },
  { id: "legion-retirement-ledger", gap: "LG-04", run: retirementLedgerFor(RETIREMENT_BASELINE) },
  { id: "legion-authority-body", gap: "LG-05", run: legionAuthorityBody },
]);
