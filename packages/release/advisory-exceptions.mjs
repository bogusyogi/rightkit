// Advisory exception gate. ci/advisory-exceptions.json is the single source; deny.toml
// [advisories].ignore and .cargo/audit.toml must match it exactly, every entry needs an owner,
// evidence and an unexpired absolute reviewBy date, and the entry's versions must match what
// Cargo.lock actually holds, `dependents` must equal the exact set of direct reverse
// dependencies of the affected versions in Cargo.lock, and every hop of `path` must be a real
// dependency edge (scope drift fails in both directions). Policy: Astra 2026-10-22.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const EXCEPTIONS_FILE = 'ci/advisory-exceptions.json';
export const DENY_FILE = 'deny.toml';
export const AUDIT_FILE = '.cargo/audit.toml';
export const LOCK_FILE = 'Cargo.lock';

const REQUIRED = ['id', 'crate', 'path', 'owner', 'rationale', 'evidence', 'reviewBy', 'approvedBy'];
const ID_RE = /^RUSTSEC-\d{4}-\d{4}$/;
const DATE_RE = /^(\d{4})-(\d{2})-(\d{2})$/;

// ---- minimal TOML subset: tables, key = value, strings, arrays, inline tables, bare scalars ----

export function parseToml(text) {
  let i = 0;
  const n = text.length;
  const root = {};
  let table = root;
  const fail = msg => { throw new Error(`TOML parse error at offset ${i}: ${msg}`); };
  const skipWs = (newlines) => {
    for (;;) {
      const c = text[i];
      if (c === ' ' || c === '\t' || c === '\r') i++;
      else if (c === '\n' && newlines) i++;
      else if (c === '#') { while (i < n && text[i] !== '\n') i++; }
      else break;
    }
  };
  const parseString = () => {
    const q = text[i++];
    let out = '';
    if (q === "'") {
      while (i < n && text[i] !== "'") { if (text[i] === '\n') fail('newline in string'); out += text[i++]; }
      if (i >= n) fail('unterminated string');
      i++;
      return out;
    }
    if (text.startsWith('""', i)) fail('multiline strings are not supported');
    while (i < n && text[i] !== '"') {
      const c = text[i++];
      if (c === '\n') fail('newline in string');
      if (c !== '\\') { out += c; continue; }
      const e = text[i++];
      const map = { n: '\n', t: '\t', r: '\r', '"': '"', '\\': '\\', b: '\b', f: '\f' };
      if (e in map) out += map[e];
      else if (e === 'u' || e === 'U') {
        const len = e === 'u' ? 4 : 8;
        const hex = text.slice(i, i + len);
        if (!/^[0-9a-fA-F]+$/.test(hex) || hex.length !== len) fail('bad unicode escape');
        out += String.fromCodePoint(parseInt(hex, 16));
        i += len;
      } else fail(`bad escape \\${e}`);
    }
    if (i >= n) fail('unterminated string');
    i++;
    return out;
  };
  const parseKey = () => {
    if (text[i] === '"' || text[i] === "'") return parseString();
    const m = /^[A-Za-z0-9_-]+/.exec(text.slice(i, i + 200));
    if (!m) fail('bad key');
    i += m[0].length;
    return m[0];
  };
  const parseValue = () => {
    skipWs(false);
    const c = text[i];
    if (c === '"' || c === "'") return parseString();
    if (c === '[') {
      i++;
      const arr = [];
      for (;;) {
        skipWs(true);
        if (text[i] === ']') { i++; return arr; }
        arr.push(parseValue());
        skipWs(true);
        if (text[i] === ',') i++;
        else if (text[i] !== ']') fail('expected , or ] in array');
      }
    }
    if (c === '{') {
      i++;
      const obj = {};
      for (;;) {
        skipWs(false);
        if (text[i] === '}') { i++; return obj; }
        const k = parseKey();
        skipWs(false);
        if (text[i++] !== '=') fail('expected = in inline table');
        obj[k] = parseValue();
        skipWs(false);
        if (text[i] === ',') i++;
        else if (text[i] !== '}') fail('expected , or } in inline table');
      }
    }
    const m = /^[^\s,\]}#]+/.exec(text.slice(i, i + 200));
    if (!m) fail('bad value');
    i += m[0].length;
    const raw = m[0];
    if (raw === 'true') return true;
    if (raw === 'false') return false;
    if (/^[+-]?\d[\d_]*(\.\d+)?$/.test(raw)) return Number(raw.replace(/_/g, ''));
    return fail(`unsupported value ${raw}`);
  };
  for (;;) {
    skipWs(true);
    if (i >= n) break;
    if (text[i] === '[') {
      const array = text[i + 1] === '[';
      i += array ? 2 : 1;
      const parts = [];
      for (;;) {
        skipWs(false);
        parts.push(parseKey());
        skipWs(false);
        if (text[i] === '.') { i++; continue; }
        break;
      }
      const close = array ? ']]' : ']';
      if (!text.startsWith(close, i)) fail('bad table header');
      i += close.length;
      let t = root;
      parts.forEach((p, idx) => {
        const last = idx === parts.length - 1;
        if (last && array) { (t[p] ||= []).push({}); t = t[p][t[p].length - 1]; }
        else { const next = t[p] ||= {}; t = Array.isArray(next) ? next[next.length - 1] : next; }
      });
      table = t;
    } else {
      const k = parseKey();
      skipWs(false);
      if (text[i++] !== '=') fail('expected =');
      if (Object.hasOwn(table, k)) fail(`duplicate key ${k}`);
      table[k] = parseValue();
    }
    skipWs(false);
    if (i < n && text[i] !== '\n') fail('unexpected trailing characters');
  }
  return root;
}

// ---- Cargo.lock ----

export function lockedVersions(lockText) {
  const map = new Map();
  for (const block of lockText.split('[[package]]').slice(1)) {
    const name = /^name = "([^"]+)"/m.exec(block)?.[1];
    const version = /^version = "([^"]+)"/m.exec(block)?.[1];
    if (!name || !version) continue;
    if (!map.has(name)) map.set(name, new Set());
    map.get(name).add(version);
  }
  return map;
}

// Full package graph: Map "name version" -> { name, version, deps: Set of "name version" }.
// A dependency ref is "name" (unambiguous) or "name version" [(source)]; an ambiguous bare
// name or an unknown ref is recorded in `unresolved` so the check fails closed.
export function lockedGraph(lockText) {
  const pkgs = [];
  for (const block of lockText.split('[[package]]').slice(1)) {
    const name = /^name = "([^"]+)"/m.exec(block)?.[1];
    const version = /^version = "([^"]+)"/m.exec(block)?.[1];
    if (!name || !version) continue;
    const dm = /^dependencies = \[([\s\S]*?)^\]/m.exec(block);
    const refs = dm ? [...dm[1].matchAll(/"([^"]+)"/g)].map(m => m[1]) : [];
    pkgs.push({ name, version, refs });
  }
  const byName = new Map();
  for (const p of pkgs) {
    if (!byName.has(p.name)) byName.set(p.name, []);
    byName.get(p.name).push(p.version);
  }
  const graph = new Map();
  const unresolved = [];
  for (const p of pkgs) {
    const deps = new Set();
    for (const ref of p.refs) {
      const [dn, dv] = ref.split(' ');
      const versions = byName.get(dn) ?? [];
      const v = dv ?? (versions.length === 1 ? versions[0] : undefined);
      if (!v || !versions.includes(v)) { unresolved.push(`${p.name} ${p.version} -> ${ref}`); continue; }
      deps.add(`${dn} ${v}`);
    }
    graph.set(`${p.name} ${p.version}`, { name: p.name, version: p.version, deps });
  }
  return { graph, unresolved };
}

// A path hop is "name" or "name version"; returns the matching graph node ids.
function hopNodes(graph, hop) {
  const [n, v] = hop.trim().split(/\s+/);
  return [...graph.keys()].filter(id => {
    const node = graph.get(id);
    return node.name === n && (v === undefined || node.version === v);
  });
}

// ---- the check ----

const isNonEmptyString = v => typeof v === 'string' && v.trim() !== '';
const isStringArray = v => Array.isArray(v) && v.length > 0 && v.every(isNonEmptyString);

function dateProblem(entryLabel, value, now) {
  const m = DATE_RE.exec(value);
  if (!m) return `${entryLabel}: reviewBy ${JSON.stringify(value)} is not an absolute YYYY-MM-DD date`;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  const dt = new Date(Date.UTC(y, mo - 1, d));
  if (dt.getUTCFullYear() !== y || dt.getUTCMonth() !== mo - 1 || dt.getUTCDate() !== d) {
    return `${entryLabel}: reviewBy ${value} is not a real calendar date`;
  }
  const today = new Date(now);
  if (Number.isNaN(today.getTime())) return `${entryLabel}: the gate clock is invalid`;
  const todayUtc = Date.UTC(today.getUTCFullYear(), today.getUTCMonth(), today.getUTCDate());
  if (todayUtc > dt.getTime()) return `${entryLabel}: reviewBy ${value} has expired; renew only with fresh evidence`;
  return null;
}

export function checkAdvisoryExceptions({ root = process.cwd(), now = new Date(), readFile, allowPending = false } = {}) {
  const read = readFile ?? (p => fs.readFileSync(path.join(root, p), 'utf8'));
  const problems = [];
  const load = (file, parse) => {
    try { return parse(read(file)); } catch (err) { problems.push(`${file}: cannot read or parse (${err.message})`); return null; }
  };

  const doc = load(EXCEPTIONS_FILE, JSON.parse);
  const deny = load(DENY_FILE, parseToml);
  const audit = load(AUDIT_FILE, parseToml);
  const lockPair = load(LOCK_FILE, t => ({ versions: lockedVersions(t), ...lockedGraph(t) }));
  if (!doc || !deny || !audit || !lockPair) return problems;
  const lock = lockPair.versions;
  const { graph } = lockPair;
  for (const u of lockPair.unresolved) problems.push(`${LOCK_FILE}: dependency ${u} does not resolve to exactly one locked package`);

  if (doc.schema !== 1) problems.push(`${EXCEPTIONS_FILE}: schema must be 1`);
  if (!Array.isArray(doc.exceptions)) {
    problems.push(`${EXCEPTIONS_FILE}: exceptions must be an array`);
    return problems;
  }

  // Entries.
  const byId = new Map();
  const valid = [];
  doc.exceptions.forEach((e, idx) => {
    const label = `${EXCEPTIONS_FILE} entry ${e?.id ?? `#${idx}`}`;
    if (!e || typeof e !== 'object' || Array.isArray(e)) { problems.push(`${label}: not an object`); return; }
    let ok = true;
    for (const f of REQUIRED) {
      if (!isNonEmptyString(e[f])) { problems.push(`${label}: missing field ${f}`); ok = false; }
    }
    if (!isStringArray(e.affectedVersions)) { problems.push(`${label}: missing field affectedVersions (non-empty array of exact versions)`); ok = false; }
    if (e.unaffectedVersions !== undefined && !isStringArray(e.unaffectedVersions)) { problems.push(`${label}: unaffectedVersions must be a non-empty array when present`); ok = false; }
    if (isNonEmptyString(e.id)) {
      if (!ID_RE.test(e.id)) { problems.push(`${label}: id is not a RUSTSEC id`); ok = false; }
      if (byId.has(e.id)) { problems.push(`${label}: duplicate id`); ok = false; } else byId.set(e.id, e);
    }
    if (isNonEmptyString(e.reviewBy)) {
      const p = dateProblem(label, e.reviewBy, now);
      if (p) { problems.push(p); ok = false; }
    }
    if (isNonEmptyString(e.approvedBy) && !allowPending && /pending/i.test(e.approvedBy)) {
      problems.push(`${label}: approval is pending (${e.approvedBy})`);
      ok = false;
    }
    if (ok) valid.push(e);
    // Consumer scope: the exact reverse-dependency set, machine-checked from Cargo.lock.
    if (isNonEmptyString(e.crate) && Array.isArray(e.affectedVersions)) {
      if (!isStringArray(e.dependents)) {
        problems.push(`${label}: missing field dependents (non-empty array of "name version" direct reverse dependencies)`);
      } else {
        const affected = new Set(e.affectedVersions.map(v => `${e.crate} ${v}`));
        const actual = new Set();
        for (const [id, node] of graph) for (const d of node.deps) if (affected.has(d)) actual.add(id);
        const declared = new Set(e.dependents);
        if (declared.size !== e.dependents.length) problems.push(`${label}: dependents has duplicates`);
        for (const id of actual) if (!declared.has(id)) problems.push(`${label}: ${id} depends on ${e.crate} in ${LOCK_FILE} but is not in dependents (new consumer widens the suppression; needs fresh approval)`);
        for (const id of declared) if (!actual.has(id)) problems.push(`${label}: dependents lists ${id}, which does not depend on an affected ${e.crate} in ${LOCK_FILE} (stale)`);
      }
    }
    // Path documentation: every hop must be a real dependency edge, ending at the advisory crate.
    if (isNonEmptyString(e.path) && isNonEmptyString(e.crate)) {
      const hops = e.path.split('->').map(s => s.trim());
      if (hops.length < 2 || hops.some(h => h === '')) problems.push(`${label}: path must have at least two hops separated by "->"`);
      else {
        if (hops[hops.length - 1].split(/\s+/)[0] !== e.crate) problems.push(`${label}: path must end at ${e.crate}`);
        for (let h = 0; h + 1 < hops.length; h++) {
          const from = hopNodes(graph, hops[h]);
          const to = new Set(hopNodes(graph, hops[h + 1]));
          if (!from.some(id => [...graph.get(id).deps].some(d => to.has(d)))) {
            problems.push(`${label}: path hop "${hops[h]} -> ${hops[h + 1]}" is not a dependency edge in ${LOCK_FILE}`);
          }
        }
      }
    }
    // Version scope applies even to an entry that failed another rule.
    if (isNonEmptyString(e.crate) && Array.isArray(e.affectedVersions)) {
      const locked = lock.get(e.crate);
      const listed = new Set([...e.affectedVersions, ...(Array.isArray(e.unaffectedVersions) ? e.unaffectedVersions : [])]);
      for (const v of listed) {
        if (!locked || !locked.has(v)) problems.push(`${label}: version ${e.crate} ${v} is not in ${LOCK_FILE} (scope drift)`);
      }
      for (const v of locked ?? []) {
        if (!listed.has(v)) problems.push(`${label}: ${LOCK_FILE} holds ${e.crate} ${v}, which the entry does not list (scope drift; the advisory may apply to it)`);
      }
      for (const v of e.affectedVersions) {
        if ((e.unaffectedVersions ?? []).includes(v)) problems.push(`${label}: version ${v} is both affected and unaffected`);
      }
    }
  });

  // deny.toml.
  const adv = deny.advisories;
  const denyIgnore = adv && Array.isArray(adv.ignore) ? adv.ignore : null;
  if (!denyIgnore) problems.push(`${DENY_FILE}: [advisories].ignore is missing`);
  else {
    const seen = new Set();
    for (const item of denyIgnore) {
      const id = typeof item === 'string' ? item : item?.id;
      const reason = typeof item === 'string' ? undefined : item?.reason;
      if (!isNonEmptyString(id)) { problems.push(`${DENY_FILE}: ignore item without an id`); continue; }
      if (seen.has(id)) problems.push(`${DENY_FILE}: duplicate ignore ${id}`);
      seen.add(id);
      const entry = byId.get(id);
      if (!entry) { problems.push(`${DENY_FILE}: ignore ${id} has no entry in ${EXCEPTIONS_FILE}`); continue; }
      if (reason !== entry.rationale) problems.push(`${DENY_FILE}: ignore ${id} reason differs from the rationale in ${EXCEPTIONS_FILE}`);
    }
    for (const id of byId.keys()) if (!seen.has(id)) problems.push(`${EXCEPTIONS_FILE}: entry ${id} has no ignore in ${DENY_FILE}`);
  }
  if (adv) {
    if (adv.yanked !== 'deny') problems.push(`${DENY_FILE}: [advisories] yanked must be "deny"`);
    if (adv.unmaintained !== 'workspace') problems.push(`${DENY_FILE}: [advisories] unmaintained must be "workspace"`);
  } else problems.push(`${DENY_FILE}: [advisories] section is missing`);

  // audit.toml.
  const auditIgnore = audit.advisories?.ignore;
  if (!Array.isArray(auditIgnore) || !auditIgnore.every(x => typeof x === 'string')) {
    problems.push(`${AUDIT_FILE}: [advisories].ignore must be an array of string ids`);
  } else {
    const set = new Set();
    for (const id of auditIgnore) {
      if (set.has(id)) problems.push(`${AUDIT_FILE}: duplicate ignore ${id}`);
      set.add(id);
      if (!byId.has(id)) problems.push(`${AUDIT_FILE}: ignore ${id} has no entry in ${EXCEPTIONS_FILE}`);
    }
    for (const id of byId.keys()) if (!set.has(id)) problems.push(`${AUDIT_FILE}: entry ${id} from ${EXCEPTIONS_FILE} is not ignored here (sets differ)`);
  }

  return problems;
}

// ---- CLI ----

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const root = path.resolve(import.meta.dirname, '..', '..');
  const problems = checkAdvisoryExceptions({ root, allowPending: args.includes('--allow-pending') });
  if (problems.length) {
    for (const p of problems) console.error(`advisory-exceptions: ${p}`);
    console.error(`advisory-exceptions: ${problems.length} problem(s)`);
    process.exit(1);
  }
  console.log('advisory-exceptions: ok');
}
