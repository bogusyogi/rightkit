// Consumer semantic policy (Class A row 13). An app declares, as data in its own commit, the semantic contracts that its own
// scripts used to enforce by running repository code; these rules judge them from committed files alone. Nothing here imports,
// evaluates or spawns repository code: the policy is JSON, the source files are read as text and import edges come from the
// lexer below, which only reads module specifiers.
//
// The declaration is `.rightkit/consumer-policy.json` (CONSUMER_POLICY_FILE). An app without it has nothing to check. Shape:
//
//   { "schema": 1,
//     "retiredTokens":  [{ "id", "tokens": [..], "caseInsensitive"?, "include": [glob], "exclude"?: [glob], "extensions"?: [".ts", ..],
//                          "allowFiles"?: [{ "file", "reason" }], "allowFragments"?: [{ "file", "text", "kind"?: "text" | "identifier" | "string", "reason" }] }],
//     "routeDenyPolicies": [{ "id", "routes": "<route table or OpenAPI json>" | [..], "policy": "<deny policy json>", "prefix": "/v1/admin/" }],
//     "layerBoundaries": [{ "id", "root": "<dir>", "aliases"?: { "~/": "src/" }, "from": { "include", "exclude"? },
//                          "forbid": { "include", "exclude"? }, "stop"?: [glob], "extensions"?: [..] }],
//     "specRouteParity": [{ "id", "spec": "<openapi json>", "routes": "<route table json>" }] }
//
//   route table: { "routes": ["METHOD /path/{param}", ..], "undocumented"?: ["METHOD /path", ..] } (a `:param` is read as `{param}`)
//   deny policy: { "denied": ["METHOD /path", ..] | { "METHOD /path": "<group>" }, "exceptions"?: [..],
//                  "irreversible": { "segments": [..], "fragments": [..] } }
//
//   retired-token      CA-13 a declared retired token in a scoped file, outside a whole-file or exact-fragment exception
//   route-deny-policy  CA-13 the deny-set invariants over the committed route table: a denied or excepted route that is not a route,
//                            a DELETE that is not denied, an irreversible-looking mutation that is neither denied nor excepted, a route
//                            both denied and excepted, and a denied non-DELETE route the irreversible marks do not cover (its removal
//                            would go unnoticed)
//   layer-reach        CA-13 a file reached through value imports from a `from` file that lands on a `forbid` file, or an import in a
//                            reached file that cannot be read statically; a `stop` file (a reviewed public API) is accepted and not walked
//   spec-route-parity  CA-13 the committed API spec and the committed route table disagree: a documented route missing from the spec,
//                            a spec operation that is not a route, or an undocumented route that the spec documents
import fs from "node:fs";
import path from "node:path";

// Not imported from app-check.mjs: app-check imports this module, and a cycle would leave its RULES half-built.
const readText = (root, rel) => { try { return fs.readFileSync(path.join(root, rel), "utf8"); } catch { return null; } };

export const CONSUMER_POLICY_FILE = ".rightkit/consumer-policy.json";
const FIX_POLICY = `fix ${CONSUMER_POLICY_FILE} (see packages/release/consumer-policy.mjs for its shape)`;
const DEFAULT_CODE_EXT = [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"];
const HTTP_METHODS = ["get", "post", "put", "patch", "delete", "head", "options"];

// ---- shared ------------------------------------------------------------------------------------------------------------------

/** The policy, or { error } when it is present but unreadable; null when the app declares none. */
export function loadPolicy(root, files) {
  if (!files.includes(CONSUMER_POLICY_FILE)) return null;
  const text = readText(root, CONSUMER_POLICY_FILE);
  let policy;
  try { policy = JSON.parse(text); } catch (error) { return { error: `${CONSUMER_POLICY_FILE} is not JSON: ${error.message}` }; }
  if (!policy || typeof policy !== "object" || Array.isArray(policy)) return { error: `${CONSUMER_POLICY_FILE} is not a JSON object` };
  if (policy.schema !== 1) return { error: `${CONSUMER_POLICY_FILE} has schema ${JSON.stringify(policy.schema)}; this check reads schema 1` };
  return policy;
}

const policyError = (problem, subject = "policy") => ({ file: CONSUMER_POLICY_FILE, subject, problem, fix: FIX_POLICY });

/** Entries of one section, each checked for an id; a malformed section is one finding. */
function sectionOf(policy, key) {
  if (policy[key] === undefined) return { entries: [] };
  if (!Array.isArray(policy[key])) return { error: policyError(`${CONSUMER_POLICY_FILE} ${key} is not an array`, key) };
  const ids = new Set();
  for (const [i, entry] of policy[key].entries()) {
    if (!entry || typeof entry !== "object" || typeof entry.id !== "string" || !/^[a-z0-9][a-z0-9._-]*$/.test(entry.id)) return { error: policyError(`${CONSUMER_POLICY_FILE} ${key}[${i}] needs an id of lower-case letters, digits, '.', '_' or '-'`, key) };
    if (ids.has(entry.id)) return { error: policyError(`${CONSUMER_POLICY_FILE} ${key} repeats the id ${entry.id}`, key) };
    ids.add(entry.id);
  }
  return { entries: policy[key] };
}

const isStringList = (v) => Array.isArray(v) && v.every((s) => typeof s === "string" && s !== "");

/** A repository-relative posix path: no leading '/', no '.', '..' or empty segment, no backslash. */
const cleanRel = (p) => typeof p === "string" && p !== "" && !p.includes("\\") && !p.includes("\0") && !p.startsWith("/") && p.split("/").every((s) => s !== "" && s !== "." && s !== "..");

const segmentRegex = new Map();
function segmentMatches(pattern, part) {
  let re = segmentRegex.get(pattern);
  if (!re) {
    re = new RegExp(`^${pattern.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, "[^/]*").replace(/\?/g, "[^/]")}$`);
    segmentRegex.set(pattern, re);
  }
  return re.test(part);
}
function matchParts(pattern, pi, parts, si) {
  if (pi === pattern.length) return si === parts.length;
  if (pattern[pi] === "**") {
    for (let k = si; k <= parts.length; k++) if (matchParts(pattern, pi + 1, parts, k)) return true;
    return false;
  }
  return si < parts.length && segmentMatches(pattern[pi], parts[si]) && matchParts(pattern, pi + 1, parts, si + 1);
}
/** Per-segment glob: `*` and `?` stay inside one segment, `**` (a whole segment) is zero or more segments, `[` `]` are literal. */
export function matchGlob(pattern, rel) {
  return matchParts(pattern.split("/"), 0, rel.split("/"), 0);
}
const inSet = (set, rel) => (set.include ?? []).some((g) => matchGlob(g, rel)) && !(set.exclude ?? []).some((g) => matchGlob(g, rel));
const globsOk = (v) => v === undefined || (isStringList(v) && v.every((g) => cleanRel(g.replace(/\*\*?|\?/g, "x"))));

const lineOf = (text, index) => text.slice(0, index).split("\n").length;

// ---- retired-token -------------------------------------------------------------------------------------------------------------

// An exact-fragment exception: "text" is any occurrence; "identifier" an occurrence with no identifier character on either side;
// "string" the whole body of a quoted string ('', "" or ``).
export const FRAGMENT_KINDS = ["text", "identifier", "string"];
const IDENT_CHAR = /[A-Za-z0-9_$]/;
export function blankFragment(text, { text: fragment, kind = "text" }) {
  let out = "";
  let from = 0;
  for (let at = text.indexOf(fragment); at >= 0; at = text.indexOf(fragment, at + 1)) {
    if (at < from) continue;
    const before = text[at - 1] ?? "";
    const after = text[at + fragment.length] ?? "";
    const ok = kind === "identifier" ? !IDENT_CHAR.test(before) && !IDENT_CHAR.test(after)
      : kind === "string" ? ["'", '"', "`"].includes(before) && before === after
      : true;
    if (!ok) continue;
    out += text.slice(from, at) + fragment.replace(/[^\n]/g, " ");
    from = at + fragment.length;
  }
  return out + text.slice(from);
}

function retiredTokens({ root, files }) {
  const policy = loadPolicy(root, files);
  if (!policy) return [];
  if (policy.error) return [policyError(policy.error)];
  const { entries, error } = sectionOf(policy, "retiredTokens");
  if (error) return [error];
  const out = [];
  for (const entry of entries) {
    const bad = (why) => out.push(policyError(`${CONSUMER_POLICY_FILE} retiredTokens ${entry.id}: ${why}`, `retiredTokens:${entry.id}`));
    if (!isStringList(entry.tokens) || entry.tokens.length === 0) { bad("tokens must be a non-empty list of strings"); continue; }
    if (!isStringList(entry.include) || entry.include.length === 0 || !globsOk(entry.include) || !globsOk(entry.exclude)) { bad("include (non-empty) and exclude must be lists of relative globs"); continue; }
    const exts = entry.extensions ?? DEFAULT_CODE_EXT;
    if (!isStringList(exts) || !exts.every((e) => /^\.[A-Za-z0-9.]+$/.test(e))) { bad("extensions must be a list like [\".ts\"]"); continue; }
    const allowFiles = entry.allowFiles ?? [];
    const allowFragments = entry.allowFragments ?? [];
    if (!Array.isArray(allowFiles) || !allowFiles.every((a) => a && cleanRel(a.file) && typeof a.reason === "string" && a.reason.trim())) { bad("each allowFiles entry needs a file and a reason"); continue; }
    if (!Array.isArray(allowFragments) || !allowFragments.every((a) => a && cleanRel(a.file) && typeof a.text === "string" && a.text !== "" && typeof a.reason === "string" && a.reason.trim())) { bad("each allowFragments entry needs a file, a non-empty text and a reason"); continue; }
    if (!allowFragments.every((a) => a.kind === undefined || FRAGMENT_KINDS.includes(a.kind))) { bad(`an allowFragments kind is one of ${FRAGMENT_KINDS.join(", ")}`); continue; }
    const fold = entry.caseInsensitive === true;
    const norm = (s) => (fold ? s.toLowerCase() : s);
    const wholeFile = new Set(allowFiles.map((a) => a.file));
    const fragments = new Map();
    for (const a of allowFragments) fragments.set(a.file, [...(fragments.get(a.file) ?? []), a]);
    for (const file of files) {
      if (!inSet(entry, file) || !exts.includes(path.posix.extname(file).toLowerCase()) || wholeFile.has(file)) continue;
      const original = readText(root, file);
      if (original === null || original.includes("\0")) continue; // binary
      // Each excepted fragment is blanked to spaces of its own length (newlines kept), longest first, so lines stay true and two
      // halves of a token on either side of an excepted fragment never join into one.
      let text = original;
      for (const a of [...(fragments.get(file) ?? [])].sort((x, y) => y.text.length - x.text.length)) text = blankFragment(text, a);
      const haystack = norm(text);
      for (const token of entry.tokens) {
        const at = haystack.indexOf(norm(token));
        if (at < 0) continue;
        out.push({ file, subject: `${entry.id}:${token}`, problem: `${file}:${lineOf(text, at)} contains the retired token ${JSON.stringify(token)} (${CONSUMER_POLICY_FILE} retiredTokens ${entry.id})`, fix: `remove it; a reviewed compatibility use is an allowFragments entry (file, exact text, kind, reason) in ${CONSUMER_POLICY_FILE}` });
      }
    }
  }
  return out;
}

// ---- route tables --------------------------------------------------------------------------------------------------------------

/** "METHOD /a/:b{[0-9]+}/c" -> "METHOD /a/{b}/c". */
export function routeKey(raw) {
  const m = /^([A-Z]+) (\/\S*)$/.exec(String(raw).trim());
  if (!m) return null;
  return `${m[1]} ${m[2].split("/").map((s) => s.replace(/^:([A-Za-z0-9_]+)(?:\{[^}]*\})?$/, "{$1}")).join("/")}`;
}

/**
 * The routes of one or more committed files: a route table ({ routes, undocumented? }) or an OpenAPI document ({ paths }), whose
 * operations are routes. Several sources are a union (an API spec lists the documented routes, a table the rest).
 */
function readRoutes(root, files, sources) {
  const list = typeof sources === "string" ? [sources] : sources;
  if (!isStringList(list) || list.length === 0) return { error: "routes must name a route table or OpenAPI file, or a list of them" };
  const routes = new Set();
  const undocumented = new Set();
  for (const rel of list) {
    if (!cleanRel(rel) || !files.includes(rel)) return { error: `the route source ${JSON.stringify(rel)} is not a file of this commit` };
    let doc;
    try { doc = JSON.parse(readText(root, rel)); } catch (error) { return { error: `the route source ${rel} is not JSON: ${error.message}` }; }
    if (doc && typeof doc.paths === "object" && doc.paths !== null && !Array.isArray(doc.paths)) { for (const k of specOperations(doc)) routes.add(k); continue; }
    const table = readRouteTable(root, files, rel);
    if (table.error) return table;
    for (const k of table.routes) routes.add(k);
    for (const k of table.undocumented) undocumented.add(k);
  }
  return { routes, undocumented };
}

/** "METHOD /path/{param}" for every operation of an OpenAPI document. */
function specOperations(spec) {
  const ops = new Set();
  for (const [p, item] of Object.entries(spec.paths)) for (const m of Object.keys(item ?? {})) if (HTTP_METHODS.includes(m)) { const k = routeKey(`${m.toUpperCase()} ${p}`); if (k) ops.add(k); }
  return ops;
}

function readRouteTable(root, files, rel) {
  if (!cleanRel(rel)) return { error: `the route table path ${JSON.stringify(rel)} is not a relative path` };
  if (!files.includes(rel)) return { error: `the route table ${rel} is not in this commit` };
  let table;
  try { table = JSON.parse(readText(root, rel)); } catch (error) { return { error: `the route table ${rel} is not JSON: ${error.message}` }; }
  if (!table || !isStringList(table.routes) || (table.undocumented !== undefined && !isStringList(table.undocumented))) return { error: `the route table ${rel} needs "routes" (and optional "undocumented") as lists of "METHOD /path"` };
  const parse = (list) => {
    const keys = [];
    for (const r of list) { const k = routeKey(r); if (!k) return { error: `the route table ${rel} has ${JSON.stringify(r)}, which is not "METHOD /path"` }; keys.push(k); }
    return { keys };
  };
  const routes = parse(table.routes);
  if (routes.error) return routes;
  const undocumented = parse(table.undocumented ?? []);
  if (undocumented.error) return undocumented;
  return { routes: new Set(routes.keys), undocumented: new Set(undocumented.keys) };
}

// ---- route-deny-policy --------------------------------------------------------------------------------------------------------

/** True when `routePath` carries an irreversible mark: a segment preceded by '/' or '-' and followed by '/' or the end, or a fragment. */
export function irreversible(marks, routePath) {
  const p = routePath;
  for (const s of marks.segments ?? []) {
    for (let at = p.indexOf(s); at >= 0; at = p.indexOf(s, at + 1)) {
      const before = p[at - 1];
      const after = p[at + s.length];
      if ((before === "/" || before === "-") && (after === undefined || after === "/")) return true;
    }
  }
  return (marks.fragments ?? []).some((f) => p.includes(f));
}

function routeDenyPolicies({ root, files }) {
  const policy = loadPolicy(root, files);
  if (!policy) return [];
  if (policy.error) return [policyError(policy.error)];
  const { entries, error } = sectionOf(policy, "routeDenyPolicies");
  if (error) return [error];
  const out = [];
  for (const entry of entries) {
    const bad = (why) => out.push(policyError(`${CONSUMER_POLICY_FILE} routeDenyPolicies ${entry.id}: ${why}`, `routeDenyPolicies:${entry.id}`));
    if (typeof entry.prefix !== "string" || !entry.prefix.startsWith("/")) { bad("prefix must be a path such as \"/v1/admin/\""); continue; }
    const table = readRoutes(root, files, entry.routes);
    if (table.error) { bad(table.error); continue; }
    if (!cleanRel(entry.policy) || !files.includes(entry.policy)) { bad(`the deny policy ${JSON.stringify(entry.policy)} is not a file of this commit`); continue; }
    let deny;
    try { deny = JSON.parse(readText(root, entry.policy)); } catch (e) { bad(`the deny policy ${entry.policy} is not JSON: ${e.message}`); continue; }
    const deniedRaw = Array.isArray(deny?.denied) ? deny.denied : deny?.denied && typeof deny.denied === "object" ? Object.keys(deny.denied) : null;
    const marks = deny?.irreversible;
    if (!isStringList(deniedRaw) || (deny.exceptions !== undefined && !isStringList(deny.exceptions)) || !marks || !isStringList(marks.segments ?? []) || !isStringList(marks.fragments ?? [])) { bad(`the deny policy ${entry.policy} needs "denied", optional "exceptions" and "irreversible": { "segments", "fragments" }`); continue; }
    const keyed = (list) => list.map((r) => [r, routeKey(r)]);
    const badKey = [...keyed(deniedRaw), ...keyed(deny.exceptions ?? [])].find(([, k]) => !k);
    if (badKey) { bad(`the deny policy ${entry.policy} has ${JSON.stringify(badKey[0])}, which is not "METHOD /path"`); continue; }
    const denied = new Set(deniedRaw.map(routeKey));
    const excepted = new Set((deny.exceptions ?? []).map(routeKey));
    const real = new Set([...table.routes].filter((k) => { const [m, p] = k.split(" "); return m !== "ALL" && p.startsWith(entry.prefix) && !p.includes("*"); }));
    const file = entry.policy;
    const add = (subject, problem, fix) => out.push({ file, subject: `${entry.id}:${subject}`, problem: `${file}: ${problem} (${CONSUMER_POLICY_FILE} routeDenyPolicies ${entry.id})`, fix });
    for (const k of denied) if (!real.has(k)) add(`not-a-route:${k}`, `the denied route ${k} is not a route in ${[entry.routes].flat().join(" + ")}: a renamed route has left the block`, "deny the route as it is registered now");
    for (const k of excepted) {
      if (!real.has(k)) add(`exception-not-a-route:${k}`, `the exception ${k} is not a route in ${[entry.routes].flat().join(" + ")}`, "remove the stale exception");
      if (denied.has(k)) add(`denied-and-excepted:${k}`, `${k} is both denied and excepted`, "keep it in one list");
    }
    for (const k of real) {
      const [m, p] = k.split(" ");
      if (m === "GET" || m === "HEAD") continue;
      if (m === "DELETE" && !denied.has(k)) add(`delete-not-denied:${k}`, `the DELETE route ${k} is not denied`, "add it to denied: every hard delete needs a human");
      if (irreversible(marks, p) && !denied.has(k) && !excepted.has(k)) add(`irreversible-not-denied:${k}`, `the irreversible-looking route ${k} is neither denied nor excepted`, "deny it, or except it with a reason");
    }
    for (const k of denied) {
      const [m, p] = k.split(" ");
      if (m !== "DELETE" && !irreversible(marks, p)) add(`denied-not-covered:${k}`, `the denied route ${k} is not covered by the irreversible marks: its removal from denied would go unnoticed`, "add the segment or fragment that names it to irreversible");
    }
  }
  return out;
}

// ---- spec-route-parity ---------------------------------------------------------------------------------------------------------

function specRouteParity({ root, files }) {
  const policy = loadPolicy(root, files);
  if (!policy) return [];
  if (policy.error) return [policyError(policy.error)];
  const { entries, error } = sectionOf(policy, "specRouteParity");
  if (error) return [error];
  const out = [];
  for (const entry of entries) {
    const bad = (why) => out.push(policyError(`${CONSUMER_POLICY_FILE} specRouteParity ${entry.id}: ${why}`, `specRouteParity:${entry.id}`));
    const table = readRouteTable(root, files, entry.routes);
    if (table.error) { bad(table.error); continue; }
    if (!cleanRel(entry.spec) || !files.includes(entry.spec)) { bad(`the spec ${JSON.stringify(entry.spec)} is not a file of this commit`); continue; }
    let spec;
    try { spec = JSON.parse(readText(root, entry.spec)); } catch (e) { bad(`the spec ${entry.spec} is not JSON: ${e.message}`); continue; }
    if (!spec || typeof spec.paths !== "object" || spec.paths === null || Array.isArray(spec.paths)) { bad(`the spec ${entry.spec} has no "paths" object`); continue; }
    const ops = specOperations(spec);
    const add = (file, subject, problem, fix) => out.push({ file, subject: `${entry.id}:${subject}`, problem: `${problem} (${CONSUMER_POLICY_FILE} specRouteParity ${entry.id})`, fix });
    const regen = `regenerate ${entry.spec} from the API and commit it with the route change`;
    for (const k of table.routes) {
      if (table.undocumented.has(k)) { if (ops.has(k)) add(entry.routes, `undocumented-in-spec:${k}`, `${entry.routes} lists ${k} as undocumented, but ${entry.spec} documents it`, "remove it from undocumented"); continue; }
      if (!ops.has(k)) add(entry.spec, `missing:${k}`, `${entry.spec} has no operation for the documented route ${k} in ${entry.routes}: the committed spec is older than the routes`, regen);
    }
    for (const k of table.undocumented) if (!table.routes.has(k)) add(entry.routes, `undocumented-not-a-route:${k}`, `${entry.routes} lists ${k} as undocumented, but it is not a route`, "remove the stale entry");
    for (const k of ops) if (!table.routes.has(k)) add(entry.spec, `not-a-route:${k}`, `${entry.spec} documents ${k}, which is not a route in ${entry.routes}`, `${regen}, or add the route to ${entry.routes}`);
  }
  return out;
}

// ---- layer-reach ---------------------------------------------------------------------------------------------------------------

const ID_START = /[A-Za-z_$]/;
const ID_PART = /[A-Za-z0-9_$]/;
const REGEX_AFTER = new Set([..."(,=:[!&|?{};+-*%<>~^"]);
const REGEX_AFTER_WORD = new Set(["return", "typeof", "instanceof", "in", "of", "new", "delete", "void", "throw", "case", "do", "else", "yield", "await"]);

/**
 * The module specifiers of a JS/TS source, read lexically: comments, strings, templates and regular expressions are skipped, and
 * `import ... from 'x'`, `import 'x'`, `export ... from 'x'`, `import('x')` and `require('x')` are returned as
 * { spec, typeOnly, via, index }; a dynamic import or require whose argument is not one string literal has spec null. A quote
 * string ends at a newline (JSX text such as Don't cannot swallow the file). The `${...}` holes of a template are code and are
 * read the same way. `import('x').T` and `typeof import('x')` are type-only only in a type position: a `type`, `interface` or
 * `declare` statement, or right after the annotation colon of a `let`, `const`, `var` or `function` before any `=`; anywhere else
 * they load the module and are edges.
 */
export function moduleSpecifiers(text) {
  const out = [];
  const n = text.length;
  let i = 0;
  let prev = ""; // the last significant character, or a word
  let prev2 = ""; // the one before it
  const setPrev = (t) => { prev2 = prev; prev = t; };
  // the statement being read: its first word (`export` skipped), whether its `=` has passed, and whether an enclosing brace
  // belongs to a type statement
  let stmtWord = "";
  let stmtEq = false;
  const typeStack = [false];
  const TYPE_STMT = new Set(["type", "interface", "declare"]);
  const ANNOTATED = new Set(["let", "const", "var", "function"]);
  const inTypeStatement = () => typeStack[typeStack.length - 1] || TYPE_STMT.has(stmtWord);
  const typePosition = (before) => inTypeStatement() || (before === ":" && ANNOTATED.has(stmtWord) && !stmtEq);
  const skipWs = (j) => {
    for (;;) {
      while (j < n && /\s/.test(text[j])) j++;
      if (text.startsWith("//", j)) { while (j < n && text[j] !== "\n") j++; continue; }
      if (text.startsWith("/*", j)) { const e = text.indexOf("*/", j + 2); j = e < 0 ? n : e + 2; continue; }
      return j;
    }
  };
  const readString = (j) => { // j at a quote; returns { value, end } or null
    const q = text[j];
    let v = "";
    for (let k = j + 1; k < n; k++) {
      const c = text[k];
      if (c === "\\") { v += text[k + 1] ?? ""; k++; continue; }
      if (c === q) return { value: v, end: k + 1 };
      if (c === "\n") return null;
      v += c;
    }
    return null;
  };
  // j after the opening backtick; returns the index after the closing one. `holes` collects the [start, end) of each `${...}`.
  const skipTemplate = (j, holes = null) => {
    let depth = 0;
    let hole = 0;
    for (let k = j; k < n; k++) {
      const c = text[k];
      if (c === "\\") { k++; continue; }
      if (depth === 0 && c === "`") return k + 1;
      if (depth === 0 && c === "$" && text[k + 1] === "{") { depth = 1; k++; hole = k + 1; continue; }
      if (depth > 0) {
        if (c === "{") depth++;
        else if (c === "}") { depth--; if (depth === 0 && holes) holes.push([hole, k]); }
        else if (c === "'" || c === '"') { const s = readString(k); if (s) k = s.end - 1; }
        else if (c === "`") k = skipTemplate(k + 1) - 1;
      }
    }
    return n;
  };
  const word = (j) => { let k = j; while (k < n && ID_PART.test(text[k])) k++; return text.slice(j, k); };
  const quoted = (j) => (text[j] === "'" || text[j] === '"' ? readString(j) : null);
  // a call argument: one string literal (or a template without substitutions) followed by ')' or ','
  const literalArg = (j) => {
    j = skipWs(j);
    let s = quoted(j);
    if (!s && text[j] === "`") {
      const end = skipTemplate(j + 1);
      const body = text.slice(j + 1, end - 1);
      if (!body.includes("${")) s = { value: body, end };
    }
    if (!s) return null;
    const after = skipWs(s.end);
    return text[after] === ")" || text[after] === "," ? s.value : null;
  };
  // from j (after `import` or `export`), the clause up to `from` and the string after it, at depth 0 of braces
  const clauseFrom = (j) => {
    let depth = 0;
    for (let k = j; k < n;) {
      k = skipWs(k);
      const c = text[k];
      if (c === undefined) return null;
      if (c === "{") { depth++; k++; continue; }
      if (c === "}") { depth--; k++; continue; }
      if (depth === 0 && (c === ";" || c === "(" || c === "=" || c === "'" || c === '"' || c === "`")) return null;
      if (ID_START.test(c)) {
        const w = word(k);
        if (depth === 0 && w === "from") {
          const s = quoted(skipWs(k + 4));
          return s ? { clause: text.slice(j, k), spec: s.value, end: s.end } : null;
        }
        k += w.length;
        continue;
      }
      k++;
    }
    return null;
  };
  const typeOnlyClause = (clause) => {
    const c = clause.trim();
    if (/^type\s/.test(c) && !/^type\s*,/.test(c) && !/^type\s+from\b/.test(c)) return true; // import type X / export type { X }
    const m = /^\{([\s\S]*)\}$/.exec(c);
    if (!m) return false;
    const names = m[1].split(",").map((s) => s.trim()).filter(Boolean);
    return names.length > 0 && names.every((s) => /^type\s/.test(s));
  };
  while (i < n) {
    const c = text[i];
    if (c === "/" && text[i + 1] === "/") { while (i < n && text[i] !== "\n") i++; continue; }
    if (c === "/" && text[i + 1] === "*") { const e = text.indexOf("*/", i + 2); i = e < 0 ? n : e + 2; continue; }
    if (c === "'" || c === '"') { const s = readString(i); i = s ? s.end : text.indexOf("\n", i) < 0 ? n : text.indexOf("\n", i); setPrev("x"); continue; }
    if (c === "`") {
      const holes = [];
      i = skipTemplate(i + 1, holes);
      for (const [a, b] of holes) for (const s of moduleSpecifiers(text.slice(a, b))) out.push({ ...s, index: a + s.index });
      setPrev("x");
      continue;
    }
    if (c === "/") {
      if (prev === "" || REGEX_AFTER.has(prev) || REGEX_AFTER_WORD.has(prev)) { // a regular expression literal
        let k = i + 1;
        let cls = false;
        for (; k < n && text[k] !== "\n"; k++) {
          if (text[k] === "\\") { k++; continue; }
          if (text[k] === "[") cls = true;
          else if (text[k] === "]") cls = false;
          else if (text[k] === "/" && !cls) break;
        }
        i = k + 1;
        while (i < n && ID_PART.test(text[i])) i++;
        setPrev("x");
        continue;
      }
      setPrev("/"); i++; continue;
    }
    if (ID_START.test(c)) {
      const w = word(i);
      const start = i;
      i += w.length;
      const dotted = text[start - 1] === "." && text[start - 2] !== ".";
      if (!dotted && (w === "import" || w === "export" || w === "require")) {
        const j = skipWs(i);
        if (w === "require" || (w === "import" && text[j] === "(")) {
          if (text[j] === "(") {
            // import('x').Name (Name not then/catch/finally) and typeof import('x') are types only in a type position
            let typeOnly = false;
            if (w === "import") {
              const close = text.indexOf(")", j);
              const after = close < 0 ? "" : text.slice(close + 1, close + 40);
              const member = /^\s*\.\s*([A-Za-z_$][A-Za-z0-9_$]*)/.exec(after);
              if (prev === "typeof") typeOnly = typePosition(prev2);
              else if (member && !["then", "catch", "finally"].includes(member[1])) typeOnly = typePosition(prev);
            }
            out.push({ spec: literalArg(j + 1), typeOnly, via: w === "require" ? "require" : "dynamic-import", index: start });
            i = j + 1;
          }
        } else if (w === "import" && text[j] !== ".") {
          const s = quoted(j);
          if (s) { out.push({ spec: s.value, typeOnly: false, via: "import", index: start }); i = s.end; }
          else {
            const r = clauseFrom(j);
            if (r) { out.push({ spec: r.spec, typeOnly: typeOnlyClause(r.clause), via: "import", index: start }); i = r.end; }
          }
        } else if (w === "export" && (text[j] === "*" || text[j] === "{" || /^type\s*[{*]/.test(text.slice(j, j + 8)))) {
          const r = clauseFrom(j);
          if (r) { out.push({ spec: r.spec, typeOnly: typeOnlyClause(r.clause), via: "export", index: start }); i = r.end; }
        }
      }
      if (stmtWord === "" && w !== "export") stmtWord = w;
      setPrev(w);
      continue;
    }
    if (c === ";") { stmtWord = ""; stmtEq = false; }
    else if (c === "{") { typeStack.push(inTypeStatement()); stmtWord = ""; stmtEq = false; }
    else if (c === "}") { if (typeStack.length > 1) typeStack.pop(); stmtWord = ""; stmtEq = false; }
    else if (c === "=" && !"=>".includes(text[i + 1] ?? "") && !"=!<>".includes(prev)) stmtEq = true;
    if (!/\s/.test(c)) setPrev(c);
    i++;
  }
  return out;
}

const JS_TO_TS = { ".js": [".ts", ".tsx", ".js"], ".jsx": [".tsx", ".jsx"], ".mjs": [".mts", ".mjs"], ".cjs": [".cts", ".cjs"] };

/** The committed file a specifier names, resolved like a bundler (extension and index probing), or null. */
export function resolveSpecifier(spec, fromRel, { root, aliases, fileSet, extensions }) {
  const clean = spec.replace(/[?#].*$/, "");
  let base;
  if (clean.startsWith("./") || clean.startsWith("../") || clean === "." || clean === "..") base = path.posix.join(path.posix.dirname(fromRel), clean);
  else {
    const alias = Object.keys(aliases).sort((a, b) => b.length - a.length).find((a) => clean.startsWith(a));
    if (alias === undefined) return null; // a package
    base = path.posix.join(root, aliases[alias], clean.slice(alias.length));
  }
  if (base.startsWith("../") || base === "..") return null;
  const candidates = [base];
  const ext = path.posix.extname(base);
  for (const alt of JS_TO_TS[ext] ?? []) candidates.push(base.slice(0, -ext.length) + alt);
  for (const e of extensions) candidates.push(base + e);
  for (const e of extensions) candidates.push(`${base}/index${e}`);
  return candidates.find((c) => fileSet.has(c)) ?? null;
}

function layerBoundaries({ root, files }) {
  const policy = loadPolicy(root, files);
  if (!policy) return [];
  if (policy.error) return [policyError(policy.error)];
  const { entries, error } = sectionOf(policy, "layerBoundaries");
  if (error) return [error];
  const out = [];
  const fileSet = new Set(files);
  for (const entry of entries) {
    const bad = (why) => out.push(policyError(`${CONSUMER_POLICY_FILE} layerBoundaries ${entry.id}: ${why}`, `layerBoundaries:${entry.id}`));
    if (!cleanRel(entry.root)) { bad("root must be a relative directory"); continue; }
    const aliases = entry.aliases ?? {};
    if (typeof aliases !== "object" || Array.isArray(aliases) || !Object.entries(aliases).every(([k, v]) => k !== "" && !k.startsWith(".") && typeof v === "string" && (v === "" || cleanRel(v.replace(/\/$/, ""))))) { bad("aliases map a specifier prefix to a directory under root"); continue; }
    const sets = [entry.from, entry.forbid];
    if (!sets.every((s) => s && isStringList(s.include) && s.include.length > 0 && globsOk(s.include) && globsOk(s.exclude)) || !globsOk(entry.stop)) { bad("from and forbid need include (non-empty) and optional exclude lists of globs relative to root; stop is a list of globs"); continue; }
    const exts = entry.extensions ?? DEFAULT_CODE_EXT;
    if (!isStringList(exts)) { bad("extensions must be a list like [\".ts\"]"); continue; }
    const prefix = `${entry.root}/`;
    const local = (f) => (f.startsWith(prefix) ? f.slice(prefix.length) : null);
    const isCode = (f) => exts.includes(path.posix.extname(f).toLowerCase());
    const ctx = { root: entry.root, aliases, fileSet, extensions: exts };
    const forbidden = (f) => { const l = local(f); return l !== null && inSet(entry.forbid, l); };
    const parsed = new Map();
    const specsOf = (f) => {
      if (!parsed.has(f)) { const text = readText(root, f); parsed.set(f, text === null ? [] : moduleSpecifiers(text).map((s) => ({ ...s, line: lineOf(text, s.index) }))); }
      return parsed.get(f);
    };
    const starts = files.filter((f) => { const l = local(f); return l !== null && isCode(f) && inSet(entry.from, l); });
    const reported = new Set();
    for (const start of starts) {
      const seen = new Set([start]);
      const queue = [[start, [start]]];
      while (queue.length) {
        const [file, chain] = queue.shift();
        for (const s of specsOf(file)) {
          if (s.typeOnly) continue;
          const here = chain.map((c) => c.slice(prefix.length)).join(" -> ");
          if (s.spec === null) {
            const key = `dynamic:${file}:${s.line}`;
            if (!reported.has(key)) { reported.add(key); out.push({ file, subject: `${entry.id}:unreadable:${file.slice(prefix.length)}`, problem: `${file}:${s.line} has a ${s.via} whose argument is not a string literal, reached from ${here}: the layer boundary cannot be checked through it (${CONSUMER_POLICY_FILE} layerBoundaries ${entry.id})`, fix: "use a static import with a literal specifier" }); }
            continue;
          }
          const target = resolveSpecifier(s.spec, file, ctx);
          if (!target || !isCode(target) || local(target) === null) continue;
          if ((entry.stop ?? []).some((g) => matchGlob(g, local(target)))) continue;
          if (forbidden(target)) {
            const key = `${start}\0${target}`;
            if (reported.has(key)) continue;
            reported.add(key);
            out.push({ file: start, subject: `${entry.id}:${start.slice(prefix.length)}->${target.slice(prefix.length)}`, problem: `${file}:${s.line} '${s.spec}': ${here} -> ${target.slice(prefix.length)} reaches a file the layer boundary forbids (${CONSUMER_POLICY_FILE} layerBoundaries ${entry.id})`, fix: "import the value from a permitted layer, or move the file there" });
            continue;
          }
          if (!seen.has(target)) { seen.add(target); queue.push([target, [...chain, target]]); }
        }
      }
    }
  }
  return out;
}

export const CONSUMER_POLICY_RULES = Object.freeze([
  { id: "retired-token", gap: "CA-13", run: retiredTokens },
  { id: "route-deny-policy", gap: "CA-13", run: routeDenyPolicies },
  { id: "layer-reach", gap: "CA-13", run: layerBoundaries },
  { id: "spec-route-parity", gap: "CA-13", run: specRouteParity },
]);
