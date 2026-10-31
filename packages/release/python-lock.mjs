// File-only Python lock check (class A scan `python-lock`). Reads committed files in one directory and executes nothing (no pip, uv or poetry).
//  * `*pins.txt` (and `requirements*.txt` compiled from a sibling `<stem>.in`): every entry must be an exact `name==version` pin; when the
//    `<stem>.in` (or `<stem>.txt`) it was pinned from exists, each of its requirements must be satisfied by the pin of that package.
//  * `requirements*.txt` or `pyproject.toml` beside a `uv.lock` / `poetry.lock`: each direct requirement must be satisfied by the locked
//    version of that package, and a requirements file's unpinned entries are fine only where no lock exists (they are then the declaration).
// A requirement line the checker cannot read literally (URL, VCS, editable, unknown specifier) is a finding, never a silent pass. An entry with
// an environment marker is checked when its package is pinned and not required to be pinned (a lock may be for another platform).

export const PYTHON_LOCK_FILE = /(^|\/)(requirements[^/]*\.(txt|in)|[^/]*pins?\.txt|pyproject\.toml|uv\.lock|poetry\.lock)$/;
const join = (dir, f) => (dir === "" || dir === "." ? f : `${dir}/${f}`);
export const normalizeName = (n) => String(n).toLowerCase().replace(/[-_.]+/g, "-");

// ---- versions (PEP 440 subset) --------------------------------------------------------------------------------------------
const PEP440 = /^v?(\d+(?:\.\d+)*)(?:[-_.]?(a|b|rc|alpha|beta|c|pre|preview)[-_.]?(\d*))?(?:[-_.]?(?:post|rev|r)[-_.]?(\d*))?(?:[-_.]?dev[-_.]?(\d*))?(?:\+[A-Za-z0-9.]+)?$/i;
export function parsePyVersion(text) {
  const m = PEP440.exec(String(text).trim());
  if (!m) return null;
  const rank = { a: 0, alpha: 0, b: 1, beta: 1, c: 2, rc: 2, pre: 2, preview: 2 };
  return { rel: m[1].split(".").map(Number), pre: m[2] ? [rank[m[2].toLowerCase()], Number(m[3] || 0)] : null, post: m[4] !== undefined ? Number(m[4] || 0) : null, dev: m[5] !== undefined ? Number(m[5] || 0) : null, text: String(text).trim() };
}
export function comparePyVersions(a, b) {
  for (let i = 0; i < Math.max(a.rel.length, b.rel.length); i++) { const x = a.rel[i] ?? 0; const y = b.rel[i] ?? 0; if (x !== y) return x < y ? -1 : 1; }
  const key = (v) => [v.dev !== null && v.pre === null && v.post === null ? -1 : 0, v.pre ? 0 : 1, ...(v.pre ?? [0, 0]), v.post ?? -1, v.dev === null ? 1 : 0, v.dev ?? 0];
  const ka = key(a); const kb = key(b);
  for (let i = 0; i < ka.length; i++) if (ka[i] !== kb[i]) return ka[i] < kb[i] ? -1 : 1;
  return 0;
}

/** Clauses of a specifier string ("" for any): [{ op, version, wildcard }] or null when not readable. */
export function parseSpecifier(spec) {
  const text = String(spec).trim().replace(/^\(|\)$/g, "");
  if (text === "") return [];
  const out = [];
  for (const part of text.split(",")) {
    const m = /^\s*(===|==|!=|~=|>=|<=|>|<)\s*([^\s,]+?)(\.\*)?\s*$/.exec(part);
    if (!m) return null;
    if (m[3] && !["==", "!="].includes(m[1])) return null;
    const version = parsePyVersion(m[2]);
    if (!version) return null;
    out.push({ op: m[1], version, wildcard: Boolean(m[3]), raw: part.trim() });
  }
  return out;
}
/** true when `version` (parsed) satisfies every clause. */
export function satisfies(version, clauses) {
  return clauses.every(({ op, version: v, wildcard }) => {
    const c = comparePyVersions(version, v);
    switch (op) {
      case "===": case "==": return wildcard ? v.rel.every((n, i) => (version.rel[i] ?? 0) === n) : c === 0;
      case "!=": return wildcard ? !v.rel.every((n, i) => (version.rel[i] ?? 0) === n) : c !== 0;
      case ">=": return c >= 0;
      case "<=": return c <= 0;
      case ">": return c > 0;
      case "<": return c < 0;
      case "~=": { if (c < 0 || v.rel.length < 2) return false; return v.rel.slice(0, -1).every((n, i) => (version.rel[i] ?? 0) === n); }
      default: return false;
    }
  });
}

// ---- requirement lines ----------------------------------------------------------------------------------------------------
/** { reqs: [{ line, name, spec, clauses, marker, pinned }], problems: [{ line, text }] } from requirements-file text. */
export function parseRequirements(text) {
  const reqs = []; const problems = [];
  const lines = String(text).split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    const start = i + 1; let line = lines[i];
    while (/\\\s*$/.test(line) && i + 1 < lines.length) { line = line.replace(/\\\s*$/, " ") + lines[++i]; }
    line = line.replace(/(^|\s)#.*$/, "").trim();
    if (!line) continue;
    if (/^-(r|c|-requirement|-constraint)\b/.test(line) || /^--(index-url|extra-index-url|find-links|trusted-host|no-binary|only-binary|prefer-binary|pre|require-hashes|no-index|use-feature|prefer-binary)\b/.test(line) || /^-[ifz]\s/.test(line)) continue;
    if (/^-e\b|^--editable\b|^-/.test(line)) { problems.push({ line: start, text: `option ${line.slice(0, 50)} cannot be read literally` }); continue; }
    line = line.replace(/\s--hash[=\s]\S+/g, "").trim();
    const [body, ...markerParts] = line.split(";");
    const marker = markerParts.join(";").trim() || null;
    if (/@|:\/\/|^\.{0,2}\//.test(body)) { problems.push({ line: start, text: `requirement ${body.trim().slice(0, 60)} is a URL, VCS or path reference, which cannot be read literally` }); continue; }
    const m = /^([A-Za-z0-9][A-Za-z0-9._-]*)\s*(?:\[[^\]]*\])?\s*(.*)$/.exec(body.trim());
    if (!m) { problems.push({ line: start, text: `requirement ${body.trim().slice(0, 60)} cannot be read literally` }); continue; }
    const clauses = parseSpecifier(m[2]);
    if (clauses === null) { problems.push({ line: start, text: `specifier ${m[2].slice(0, 60)} of ${m[1]} cannot be read literally` }); continue; }
    const pinned = clauses.length === 1 && ["==", "==="].includes(clauses[0].op) && !clauses[0].wildcard;
    reqs.push({ line: start, name: normalizeName(m[1]), display: m[1], spec: m[2].trim(), clauses, marker, pinned });
  }
  return { reqs, problems };
}

// ---- locks ----------------------------------------------------------------------------------------------------------------
/** Map name -> [versions] from a uv.lock / poetry.lock parsed as TOML (`[[package]]` name + version). */
export function lockVersions(doc) {
  const map = new Map();
  for (const p of Array.isArray(doc?.package) ? doc.package : []) {
    if (typeof p?.name !== "string" || typeof p?.version !== "string") continue;
    const k = normalizeName(p.name);
    map.set(k, [...(map.get(k) ?? []), p.version]);
  }
  return map;
}

/** PEP 508 strings or poetry specs from a parsed pyproject.toml -> [{ name, spec, marker, where }], plus problems. */
export function pyprojectRequirements(doc) {
  const out = []; const problems = [];
  const pep = (list, where) => { for (const s of Array.isArray(list) ? list : []) { if (typeof s !== "string") continue; const r = parseRequirements(s); r.reqs.forEach((q) => out.push({ name: q.name, display: q.display, clauses: q.clauses, spec: q.spec, marker: q.marker, where })); r.problems.forEach((p) => problems.push(`${where}: ${p.text}`)); } };
  pep(doc?.project?.dependencies, "[project].dependencies");
  for (const [k, v] of Object.entries(doc?.project?.["optional-dependencies"] ?? {})) pep(v, `[project.optional-dependencies].${k}`);
  for (const [k, v] of Object.entries(doc?.["dependency-groups"] ?? {})) pep(Array.isArray(v) ? v.filter((x) => typeof x === "string") : [], `[dependency-groups].${k}`);
  pep(doc?.tool?.uv?.["dev-dependencies"], "[tool.uv].dev-dependencies");
  const poetry = (table, where) => {
    for (const [name, raw] of Object.entries(table ?? {})) {
      if (name.toLowerCase() === "python") continue;
      const spec = typeof raw === "string" ? raw : typeof raw?.version === "string" ? raw.version : null;
      if (spec === null) { if (!(raw && typeof raw === "object" && (raw.git || raw.path || raw.url))) problems.push(`${where}: ${name} has no literal version`); continue; }
      const clauses = poetrySpec(spec);
      if (clauses === null) problems.push(`${where}: ${name} specifier ${spec} cannot be read literally`);
      else out.push({ name: normalizeName(name), display: name, clauses, spec, marker: raw?.markers ?? (raw?.python ? "python" : null), where });
    }
  };
  poetry(doc?.tool?.poetry?.dependencies, "[tool.poetry.dependencies]");
  poetry(doc?.tool?.poetry?.["dev-dependencies"], "[tool.poetry.dev-dependencies]");
  for (const [g, v] of Object.entries(doc?.tool?.poetry?.group ?? {})) poetry(v?.dependencies, `[tool.poetry.group.${g}.dependencies]`);
  return { reqs: out, problems };
}
/** Poetry version constraint -> clauses (caret and tilde expanded, bare version exact, `*` any); null when not readable. */
export function poetrySpec(spec) {
  const t = String(spec).trim();
  if (t === "*" || t === "") return [];
  if (t.includes("||")) return null;
  const clauses = [];
  for (const part of t.split(",").map((x) => x.trim())) {
    let m;
    if ((m = /^\^\s*(\d+(?:\.\d+)*)$/.exec(part))) {
      const r = m[1].split(".").map(Number); const at = r.findIndex((n) => n !== 0); const i = at < 0 ? r.length - 1 : at;
      const upper = r.slice(0, i); upper.push(r[i] + 1);
      clauses.push({ op: ">=", version: parsePyVersion(m[1]) }, { op: "<", version: parsePyVersion(upper.join(".")) });
    } else if ((m = /^~=?\s*(\d+(?:\.\d+)*)$/.exec(part))) {
      const r = m[1].split(".").map(Number); const upper = r.length === 1 ? [r[0] + 1] : [r[0], r[1] + 1];
      clauses.push({ op: ">=", version: parsePyVersion(m[1]) }, { op: "<", version: parsePyVersion(upper.join(".")) });
    } else if ((m = /^(\d+(?:\.\d+)*)$/.exec(part))) clauses.push({ op: "==", version: parsePyVersion(m[1]) });
    else { const c = parseSpecifier(part); if (c === null || c.length !== 1) return null; clauses.push(c[0]); }
  }
  return clauses.every((c) => c.version) ? clauses : null;
}

// ---- the check ------------------------------------------------------------------------------------------------------------
const describe = (r) => `${r.display ?? r.name}${r.spec ? ` ${r.spec}` : ""}`;
function against(reqs, versionsOf, { file, source, kind }) {
  const out = [];
  for (const r of reqs) {
    const have = versionsOf.get(r.name);
    const at = `${file}${r.line ? `:${r.line}` : ""}${r.where ? ` ${r.where}` : ""}`;
    if (!have || !have.length) { if (!r.marker) out.push(`${at}: ${describe(r)} has no ${kind} in ${source}`); continue; }
    const parsed = have.map(parsePyVersion);
    if (parsed.some((p) => p === null)) { out.push(`${source}: ${r.display ?? r.name} version ${have.join(", ")} is not a readable version`); continue; }
    if (!parsed.some((p) => satisfies(p, r.clauses))) out.push(`${at}: ${describe(r)} is not satisfied by ${source}, which has ${have.join(", ")}`);
  }
  return out;
}

/**
 * Findings for one directory. `names` = the directory's file names; `read(file)` takes repo-relative paths; `parseToml(text)` parses TOML.
 */
export function pythonLockProblems({ dir, names, read, parseToml }) {
  const out = []; const has = (n) => names.includes(n);
  const f = (n) => join(dir, n);
  const isPins = (n) => /pins?\.txt$/.test(n);
  const reqFiles = names.filter((n) => /^requirements[^/]*\.txt$/.test(n) && !isPins(n));
  const pinsFiles = names.filter(isPins);
  const toml = (file) => { try { return parseToml(read(file)); } catch (e) { out.push(`${file}: cannot be parsed as TOML (${String(e.message).split("\n")[0]})`); return null; } };
  const parsedTxt = new Map();
  const reqsOf = (n) => { if (!parsedTxt.has(n)) { const r = parseRequirements(read(f(n))); parsedTxt.set(n, r); } return parsedTxt.get(n); };
  const needPins = (n, r) => { for (const q of r.reqs) if (!q.pinned) out.push(`${f(n)}:${q.line}: ${describe(q)} is not an exact == pin in a lock file`); };
  const pinMap = (r) => { const m = new Map(); for (const q of r.reqs) if (q.pinned) m.set(q.name, [...(m.get(q.name) ?? []), q.clauses[0].version.text]); return m; };
  const problems = (n, r) => r.problems.forEach((p) => out.push(`${f(n)}:${p.line}: ${p.text}`));

  // pins files
  for (const n of pinsFiles) {
    const pins = reqsOf(n); problems(n, pins); needPins(n, pins);
    const stem = n.replace(/[._-]?pins?\.txt$/, "");
    for (const src of [`${stem}.in`, `${stem}.txt`]) {
      if (src === n || !has(src) || isPins(src)) continue;
      const direct = reqsOf(src); problems(src, direct);
      out.push(...against(direct.reqs, pinMap(pins), { file: f(src), source: f(n), kind: "pin" }));
      break;
    }
  }
  // requirements*.txt
  const locks = [["uv.lock"], ["poetry.lock"]].map(([n]) => n).filter(has);
  const lockMaps = locks.map((n) => { const doc = toml(f(n)); return doc ? { n, map: lockVersions(doc) } : null; }).filter(Boolean);
  for (const n of reqFiles) {
    const stem = n.replace(/\.txt$/, ""); const r = reqsOf(n);
    if (has(`${stem}.in`)) {
      problems(n, r); needPins(n, r);
      const direct = reqsOf(`${stem}.in`); problems(`${stem}.in`, direct);
      out.push(...against(direct.reqs, pinMap(r), { file: f(`${stem}.in`), source: f(n), kind: "pin" }));
    } else if (lockMaps.length) {
      problems(n, r);
      for (const l of lockMaps) out.push(...against(r.reqs, l.map, { file: f(n), source: f(l.n), kind: "locked version" }));
    } else if (pinsFiles.some((p) => p.replace(/[._-]?pins?\.txt$/, "") === stem)) { /* its pins file is the lock (checked above) */ }
  }
  // pyproject.toml
  if (has("pyproject.toml") && lockMaps.length) {
    const doc = toml(f("pyproject.toml"));
    if (doc) {
      const p = pyprojectRequirements(doc);
      out.push(...p.problems.map((t) => `${f("pyproject.toml")} ${t}`));
      for (const l of lockMaps) out.push(...against(p.reqs, l.map, { file: f("pyproject.toml"), source: f(l.n), kind: "locked version" }));
    }
  }
  return out;
}
