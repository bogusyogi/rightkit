// File-only SwiftPM lockfile check (class A scan `swift-lock`; Adrian 2026-11-01: "the build should not be able to proceed if the lockfile
// isn't a match"). Like `cargo --locked` and `pnpm --frozen-lockfile`, a committed Package.resolved that disagrees with the declared
// dependencies must fail before any compile. This module reads committed files and executes nothing: Package.swift is code, so SwiftPM is
// never run; the `.package(...)` declarations are read literally, and a declaration that cannot be read literally is a finding (never a
// silent pass). It checks the ROOT package's own declarations against the pins: transitive pins and the pins' own consistency with
// dependency manifests are SwiftPM's (`--disable-automatic-resolution` in the class B swift-build scan).
import path from "node:path";

// ---- versions -------------------------------------------------------------------------------------------------------------
const SEMVER = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/;
export function parseVersion(text) {
  const m = SEMVER.exec(String(text ?? "").trim());
  if (!m) return null;
  return { major: Number(m[1]), minor: Number(m[2]), patch: Number(m[3]), pre: m[4] ? m[4].split(".") : [], text: String(text).trim() };
}
const cmpIdent = (a, b) => {
  const an = /^\d+$/.test(a); const bn = /^\d+$/.test(b);
  if (an && bn) return Math.sign(Number(a) - Number(b));
  if (an) return -1;
  if (bn) return 1;
  return a < b ? -1 : a > b ? 1 : 0;
};
export function compareVersions(a, b) {
  for (const k of ["major", "minor", "patch"]) if (a[k] !== b[k]) return a[k] < b[k] ? -1 : 1;
  if (!a.pre.length && !b.pre.length) return 0;
  if (!a.pre.length) return 1;
  if (!b.pre.length) return -1;
  for (let i = 0; i < Math.max(a.pre.length, b.pre.length); i++) {
    if (a.pre[i] === undefined) return -1;
    if (b.pre[i] === undefined) return 1;
    const c = cmpIdent(a.pre[i], b.pre[i]);
    if (c) return c;
  }
  return 0;
}
const V = (major, minor, patch) => ({ major, minor, patch, pre: [], text: `${major}.${minor}.${patch}` });

/** Requirement as { type, ... } -> readable text. */
export function requirementText(r) {
  switch (r.type) {
    case "exact": return `exact ${r.version}`;
    case "from": case "upToNextMajor": return `from ${r.version} (up to next major)`;
    case "upToNextMinor": return `from ${r.version} (up to next minor)`;
    case "range": return `${r.lower}${r.closed ? "..." : "..<"}${r.upper}`;
    case "branch": return `branch ${r.branch}`;
    case "revision": return `revision ${r.revision}`;
    default: return JSON.stringify(r);
  }
}

/** null when the pin ({ version, branch, revision }) satisfies the requirement, else the reason. */
export function requirementProblem(req, pin) {
  const pinned = pin.version ? `version ${pin.version}` : pin.branch ? `branch ${pin.branch}` : pin.revision ? `revision ${pin.revision}` : "no version";
  if (req.type === "branch") return pin.branch === req.branch ? null : `pins ${pinned}, not branch ${req.branch}`;
  if (req.type === "revision") return pin.revision && String(pin.revision).toLowerCase() === req.revision.toLowerCase() ? null : `pins ${pinned}, not revision ${req.revision}`;
  const v = parseVersion(pin.version);
  if (!v) return `pins ${pinned}, which is not a semantic version`;
  const lo = (text) => parseVersion(text);
  switch (req.type) {
    case "exact": { const want = lo(req.version); return compareVersions(v, want) === 0 ? null : `pins version ${pin.version}, not ${req.version}`; }
    case "from": case "upToNextMajor": { const a = lo(req.version); return compareVersions(v, a) >= 0 && compareVersions(v, V(a.major + 1, 0, 0)) < 0 ? null : `pins version ${pin.version}, outside ${req.version} up to next major`; }
    case "upToNextMinor": { const a = lo(req.version); return compareVersions(v, a) >= 0 && compareVersions(v, V(a.major, a.minor + 1, 0)) < 0 ? null : `pins version ${pin.version}, outside ${req.version} up to next minor`; }
    case "range": {
      const a = lo(req.lower); const b = lo(req.upper);
      const upper = compareVersions(v, b);
      return compareVersions(v, a) >= 0 && (req.closed ? upper <= 0 : upper < 0) ? null : `pins version ${pin.version}, outside ${req.lower}${req.closed ? "..." : "..<"}${req.upper}`;
    }
    default: return `has an unknown requirement kind ${req.type}`;
  }
}

// ---- locations ------------------------------------------------------------------------------------------------------------
/** SwiftPM's package identity of a URL or path: its last path component without `.git`, lower-cased. */
export function identityOf(location) {
  const parts = String(location).replace(/[\\/]+$/, "").split(/[\\/:]/);
  return (parts[parts.length - 1] ?? "").replace(/\.git$/i, "").toLowerCase();
}
/** A comparable form of a repository location: no scheme, user, `.git`, trailing slash or case; scp form equals URL form. */
export function normalizeLocation(location) {
  let t = String(location).trim().toLowerCase().replace(/^[a-z][a-z0-9+.-]*:\/\//, "").replace(/^[^@/]+@/, "");
  t = t.replace(/^([^/:]+):(?!\d+\/)/, "$1/").replace(/\/+$/, "").replace(/\.git$/, "").replace(/\/+$/, "");
  return t;
}

// ---- Package.swift --------------------------------------------------------------------------------------------------------
/** The text with comments blanked (newlines kept, offsets unchanged); string literals are left whole. */
function maskComments(text) {
  let out = ""; let i = 0;
  while (i < text.length) {
    const c = text[i]; const two = text.slice(i, i + 2);
    if (text.startsWith('"""', i)) { const end = text.indexOf('"""', i + 3); const stop = end < 0 ? text.length : end + 3; out += text.slice(i, stop); i = stop; continue; }
    if (c === '"') { let j = i + 1; while (j < text.length && text[j] !== '"' && text[j] !== "\n") j += text[j] === "\\" ? 2 : 1; out += text.slice(i, j + 1); i = j + 1; continue; }
    if (two === "//") { while (i < text.length && text[i] !== "\n") { out += " "; i++; } continue; }
    if (two === "/*") {
      let depth = 0;
      while (i < text.length) {
        if (text.startsWith("/*", i)) { depth++; out += "  "; i += 2; } else if (text.startsWith("*/", i)) { depth--; out += "  "; i += 2; if (!depth) break; } else { out += text[i] === "\n" ? "\n" : " "; i++; }
      }
      continue;
    }
    out += c; i++;
  }
  return out;
}

/** Index just past the `)` that closes the `(` at `open`, or -1. Strings are skipped whole. */
function closeParen(text, open) {
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    const c = text[i];
    if (c === '"') { if (text.startsWith('"""', i)) { const e = text.indexOf('"""', i + 3); if (e < 0) return -1; i = e + 2; continue; } i++; while (i < text.length && text[i] !== '"' && text[i] !== "\n") i += text[i] === "\\" ? 2 : 1; continue; }
    if (c === "(" || c === "[" || c === "{") depth++;
    else if (c === ")" || c === "]" || c === "}") { depth--; if (depth === 0) return i + 1; if (depth < 0) return -1; }
  }
  return -1;
}

function splitArgs(body) {
  const args = []; let depth = 0; let start = 0;
  for (let i = 0; i < body.length; i++) {
    const c = body[i];
    if (c === '"') { i++; while (i < body.length && body[i] !== '"') i += body[i] === "\\" ? 2 : 1; continue; }
    if (c === "(" || c === "[" || c === "{") depth++;
    else if (c === ")" || c === "]" || c === "}") depth--;
    else if (c === "," && depth === 0) { args.push(body.slice(start, i)); start = i + 1; }
  }
  const last = body.slice(start);
  if (last.trim() !== "" || args.length) args.push(last);
  return args.map((a) => a.trim());
}

const STR = '"((?:[^"\\\\\\n]|\\\\[^(\\n])*)"';
const strOf = (expr) => { const m = new RegExp(`^${STR}$`).exec(expr.trim()); return m ? m[1] : null; };
const RANGE = new RegExp(`^${STR}\\s*(\\.\\.<|\\.\\.\\.)\\s*${STR}$`);
const FUNC = new RegExp(`^(?:Package\\.Dependency\\.Requirement)?\\.(upToNextMajor|upToNextMinor|exact|branch|revision)\\(\\s*(?:(?:from|_)\\s*:\\s*)?${STR}\\s*\\)$`);

function versionOrNull(s) { return parseVersion(s) ? s : null; }

/** Parse one `.package(...)` argument list: { kind: "remote"|"local", url, path, requirement } or { problem }. */
function parseDependency(args) {
  let url = null; let local = null; let requirement = null;
  const fail = (problem) => ({ problem });
  for (const arg of args) {
    const lab = /^([A-Za-z_]\w*)\s*:\s*([\s\S]*)$/.exec(arg);
    if (lab) {
      const [, label, expr] = lab;
      if (label === "name" || label === "traits") continue;
      if (label === "url") { url = strOf(expr); if (url === null) return fail(`url is not a string literal (${arg.slice(0, 60)})`); continue; }
      if (label === "path") { local = strOf(expr); if (local === null) return fail(`path is not a string literal (${arg.slice(0, 60)})`); continue; }
      if (label === "exact" || label === "from") {
        const v = strOf(expr);
        if (v === null || !versionOrNull(v)) return fail(`${label}: is not a literal semantic version (${expr.slice(0, 60)})`);
        if (requirement) return fail("more than one requirement");
        requirement = label === "exact" ? { type: "exact", version: v } : { type: "from", version: v };
        continue;
      }
      if (label === "branch" || label === "revision") {
        const v = strOf(expr);
        if (v === null || v === "") return fail(`${label}: is not a literal string (${expr.slice(0, 60)})`);
        if (requirement) return fail("more than one requirement");
        requirement = label === "branch" ? { type: "branch", branch: v } : { type: "revision", revision: v };
        continue;
      }
      return fail(`unsupported argument label ${label}:`);
    }
    if (requirement) return fail("more than one requirement");
    const range = RANGE.exec(arg);
    if (range) {
      if (!versionOrNull(range[1]) || !versionOrNull(range[3])) return fail(`range bounds are not literal semantic versions (${arg.slice(0, 60)})`);
      requirement = { type: "range", lower: range[1], upper: range[3], closed: range[2] === "..." };
      continue;
    }
    const fn = FUNC.exec(arg);
    if (fn) {
      const [, kind, v] = fn;
      if ((kind === "exact" || kind === "upToNextMajor" || kind === "upToNextMinor") && !versionOrNull(v)) return fail(`${kind} is not a literal semantic version (${arg.slice(0, 60)})`);
      requirement = kind === "branch" ? { type: "branch", branch: v } : kind === "revision" ? { type: "revision", revision: v } : { type: kind, version: v };
      continue;
    }
    return fail(`requirement is not a literal the checker reads (${arg.slice(0, 60)})`);
  }
  if (local !== null && url !== null) return fail("declares both url: and path:");
  if (local !== null) return { kind: "local", path: local };
  if (url === null) return fail("has no url: or path: (registry or computed dependency)");
  if (!requirement) return fail("remote dependency has no requirement");
  return { kind: "remote", url, requirement };
}

/** { deps: [{ line, kind, url, path, identity, requirement }], problems: [{ line, text }] } from Package.swift source, read literally. */
export function parsePackageSwift(source) {
  const text = maskComments(String(source));
  const deps = []; const problems = [];
  const lineAt = (idx) => text.slice(0, idx).split("\n").length;
  // Lines inside an `#if` ... `#endif` region cannot be evaluated without Swift's compiler conditions.
  const conditional = new Set(); let depth = 0;
  text.split("\n").forEach((l, i) => {
    const t = l.trim();
    if (/^#if\b/.test(t)) depth++;
    if (depth > 0) conditional.add(i + 1);
    if (/^#endif\b/.test(t)) depth = Math.max(0, depth - 1);
  });
  const re = /\.package\s*\(/g; let m;
  while ((m = re.exec(text))) {
    const open = m.index + m[0].length - 1;
    const line = lineAt(m.index);
    const end = closeParen(text, open);
    if (end < 0) { problems.push({ line, text: "unbalanced .package( declaration" }); break; }
    const parsed = parseDependency(splitArgs(text.slice(open + 1, end - 1)));
    re.lastIndex = end;
    if (parsed.problem) { problems.push({ line, text: `.package declaration cannot be read literally: ${parsed.problem}` }); continue; }
    if (conditional.has(line)) { problems.push({ line, text: `.package declaration is inside an #if region, which cannot be evaluated literally (${parsed.kind === "remote" ? parsed.url : parsed.path})` }); continue; }
    deps.push({ line, ...parsed, identity: identityOf(parsed.kind === "remote" ? parsed.url : parsed.path) });
  }
  return { deps, problems };
}

// ---- Package.resolved -----------------------------------------------------------------------------------------------------
/** { pins: [{ identity, location, version, branch, revision }], problems: [text] } for Package.resolved versions 1, 2 and 3. */
export function parseResolved(source) {
  let doc;
  try { doc = JSON.parse(String(source).replace(/^﻿/, "")); } catch (e) { return { pins: [], problems: [`not valid JSON: ${e.message}`] }; }
  const v = doc?.version;
  let raw;
  if (v === 1) raw = doc?.object?.pins;
  else if (v === 2 || v === 3) raw = doc?.pins;
  else return { pins: [], problems: [`unsupported Package.resolved version ${JSON.stringify(v)} (the checker reads 1, 2 and 3)`] };
  if (!Array.isArray(raw)) return { pins: [], problems: [`Package.resolved version ${v} has no pins array`] };
  const pins = [];
  for (const p of raw) {
    const location = v === 1 ? p?.repositoryURL : p?.location;
    if (typeof location !== "string") return { pins: [], problems: [`a Package.resolved version ${v} pin has no ${v === 1 ? "repositoryURL" : "location"}`] };
    const state = p.state ?? {};
    pins.push({ identity: identityOf(location), location, version: state.version ?? null, branch: state.branch ?? null, revision: state.revision ?? null });
  }
  return { pins, problems: [] };
}

/** Findings for the declared remote dependencies against the pins. `label(dep)` prefixes each finding with its file (and line). */
function matchPins(deps, pins, { label, resolvedFile }) {
  const out = [];
  for (const d of deps) {
    const named = pins.filter((p) => p.identity === d.identity);
    const where = `${label(d)}: package ${d.identity} requires ${requirementText(d.requirement)}`;
    if (!named.length) { out.push(`${where} but ${resolvedFile} has no pin for it`); continue; }
    const pin = named.find((p) => normalizeLocation(p.location) === normalizeLocation(d.url));
    if (!pin) { out.push(`${where} from ${d.url} but ${resolvedFile} pins ${named.map((p) => p.location).join(", ")}`); continue; }
    const why = requirementProblem(d.requirement, pin);
    if (why) out.push(`${where} but ${resolvedFile} ${why}`);
  }
  return out;
}

const join = (dir, f) => (dir === "" || dir === "." ? f : `${dir}/${f}`);
const tryRead = (read, exists, file) => (exists(file) ? { text: read(file) } : null);

/** Findings for the Swift package at `dir` (repo-relative, "." for the root). `read`/`exists` take repo-relative paths. */
export function packageLockProblems({ dir, read, exists }) {
  const manifest = join(dir, "Package.swift"); const resolvedFile = join(dir, "Package.resolved");
  const src = tryRead(read, exists, manifest);
  if (!src) return [];
  const { deps, problems } = parsePackageSwift(src.text);
  const out = problems.map((p) => `${manifest}:${p.line}: ${p.text}`);
  const remote = deps.filter((d) => d.kind === "remote");
  const resolved = tryRead(read, exists, resolvedFile);
  if (!resolved) {
    if (remote.length) out.push(`${manifest}: ${remote.length} remote ${remote.length === 1 ? "dependency" : "dependencies"} (${remote.map((d) => d.identity).join(", ")}) and no ${resolvedFile}; commit the lockfile`);
    return out;
  }
  const parsed = parseResolved(resolved.text);
  if (parsed.problems.length) return [...out, ...parsed.problems.map((p) => `${resolvedFile}: ${p}`)];
  out.push(...matchPins(remote, parsed.pins, { label: (d) => `${manifest}:${d.line}`, resolvedFile }));
  return out;
}

// ---- Xcode projects -------------------------------------------------------------------------------------------------------
const unquote = (v) => { const t = v.trim(); return t.startsWith('"') && t.endsWith('"') ? t.slice(1, -1).replace(/\\"/g, '"') : t; };
const field = (block, name) => { const m = new RegExp(`(?:^|[\\s;{])${name}\\s*=\\s*("(?:[^"\\\\]|\\\\.)*"|[^;\\s]+)\\s*;`).exec(block); return m ? unquote(m[1]) : null; };

/** { deps, problems } for the XCRemoteSwiftPackageReference objects of a project.pbxproj. */
export function parsePbxproj(source) {
  const text = String(source); const deps = []; const problems = [];
  const re = /isa\s*=\s*XCRemoteSwiftPackageReference\s*;/g; let m;
  while ((m = re.exec(text))) {
    const line = text.slice(0, m.index).split("\n").length;
    const open = text.lastIndexOf("{", m.index);
    let depth = 0; let end = -1;
    for (let i = open; i < text.length; i++) { if (text[i] === "{") depth++; else if (text[i] === "}") { depth--; if (depth === 0) { end = i; break; } } }
    if (open < 0 || end < 0) { problems.push({ line, text: "unbalanced XCRemoteSwiftPackageReference object" }); continue; }
    const block = text.slice(open + 1, end);
    const url = field(block, "repositoryURL");
    const reqOpen = block.search(/requirement\s*=\s*\{/);
    if (!url || reqOpen < 0) { problems.push({ line, text: "XCRemoteSwiftPackageReference has no repositoryURL or requirement the checker reads" }); continue; }
    const req = block.slice(block.indexOf("{", reqOpen) + 1, block.indexOf("}", reqOpen));
    const kind = field(req, "kind");
    let requirement = null;
    if (kind === "exactVersion") { const v = field(req, "version"); if (v && parseVersion(v)) requirement = { type: "exact", version: v }; }
    else if (kind === "upToNextMajorVersion") { const v = field(req, "minimumVersion"); if (v && parseVersion(v)) requirement = { type: "upToNextMajor", version: v }; }
    else if (kind === "upToNextMinorVersion") { const v = field(req, "minimumVersion"); if (v && parseVersion(v)) requirement = { type: "upToNextMinor", version: v }; }
    else if (kind === "versionRange") { const a = field(req, "minimumVersion"); const b = field(req, "maximumVersion"); if (a && b && parseVersion(a) && parseVersion(b)) requirement = { type: "range", lower: a, upper: b, closed: false }; }
    else if (kind === "branch") { const b = field(req, "branch"); if (b) requirement = { type: "branch", branch: b }; }
    else if (kind === "revision") { const r = field(req, "revision"); if (r) requirement = { type: "revision", revision: r }; }
    if (!requirement) { problems.push({ line, text: `requirement of ${url} cannot be read literally (kind ${kind ?? "missing"})` }); continue; }
    deps.push({ line, kind: "remote", url, identity: identityOf(url), requirement });
  }
  return { deps, problems };
}

/** Findings for one `<name>.xcodeproj` directory (repo-relative). */
export function xcodeProjectLockProblems({ project, read, exists }) {
  const pbx = `${project}/project.pbxproj`;
  const resolvedFile = `${project}/project.xcworkspace/xcshareddata/swiftpm/Package.resolved`;
  const src = tryRead(read, exists, pbx);
  if (!src) return [];
  const { deps, problems } = parsePbxproj(src.text);
  const out = problems.map((p) => `${pbx}:${p.line}: ${p.text}`);
  const resolved = tryRead(read, exists, resolvedFile);
  if (!resolved) {
    if (deps.length) out.push(`${pbx}: ${deps.length} remote Swift ${deps.length === 1 ? "package" : "packages"} (${deps.map((d) => d.identity).join(", ")}) and no ${resolvedFile}; commit the lockfile`);
    return out;
  }
  const parsed = parseResolved(resolved.text);
  if (parsed.problems.length) return [...out, ...parsed.problems.map((p) => `${resolvedFile}: ${p}`)];
  out.push(...matchPins(deps, parsed.pins, { label: (d) => `${pbx}:${d.line}`, resolvedFile }));
  return out;
}

/** The scan's repository-relative lock owners for a changed file: a Swift package directory or an `.xcodeproj` directory. */
export function lockOwnerOf(file, { exists, ancestorDirs }) {
  const parts = file.split("/");
  const xi = parts.findIndex((p) => p.endsWith(".xcodeproj"));
  if (xi >= 0) return { kind: "xcode", dir: parts.slice(0, xi + 1).join("/") };
  const owner = ancestorDirs(path.posix.dirname(file)).reverse().find((d) => exists(join(d, "Package.swift")));
  return owner === undefined ? null : { kind: "package", dir: owner === "" ? "." : owner };
}
