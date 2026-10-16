import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync, realpathSync } from 'node:fs';
import { dirname, extname, isAbsolute, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { sourceNonCode } from './ownership-source.mjs';
import { evaluateDependency, validateAllowlist } from './ownership-dependency-allowlist.mjs';

// Packaged snapshot keeps installed adapters self-contained. Focused test
// asserts this snapshot matches release catalog before any catalog changes ship.
const catalogPath = fileURLToPath(new URL('./ownership-fingerprints.json', import.meta.url));
let catalog; let catalogError;
try { catalog = JSON.parse(readFileSync(catalogPath, 'utf8')); } catch (error) { catalogError = error; catalog = { capabilities: [] }; }
let fingerprints = [];
try { fingerprints = catalog.capabilities.flatMap((capability) => capability.fingerprints
  .filter((entry) => entry.type === 'source' && typeof entry.pattern === 'string')
  .map((entry) => ({ ...entry, package: capability.package, regex: new RegExp(entry.pattern, entry.flags ?? '') }))).sort((a, b) => Number(b.tier === 'block') - Number(a.tier === 'block')); } catch (error) { catalogError = error; }
const sourceExtensions = new Set(['.rs', '.ts', '.tsx', '.js', '.mjs', '.cjs', '.jsx', '.swift', '.m', '.mm', '.c', '.cc', '.cpp', '.h', '.py', '.go', '.kt', '.java', '.cs']);

function normalizedPath(value, cwd = process.cwd()) {
  if (typeof value !== 'string') return null;
  const slash = value.replace(/\\/g, '/').replace(/^file:\/\//i, '');
  const absolute = resolve(cwd, slash);
  return absolute.toLowerCase().replace(/\\/g, '/');
}

// Product-repository scope: screening applies only to repositories registered in the ownership
// repository registry (ownership-repositories.json, byte copy of release/ownership-repositories.json).
// Everything else is allowed untouched; protected-record denials stay global.
const registryUrl = fileURLToPath(new URL('./ownership-repositories.json', import.meta.url));
let registry; let registryError;
try { registry = new Set(JSON.parse(readFileSync(registryUrl, 'utf8')).repositories.map((entry) => entry.repositoryIdentity)); } catch (error) { registryError = error; registry = new Set(); }

// Mirrors canonicalRepositoryIdentity in packages/release/ownership-approval.mjs exactly.
export function canonicalRepositoryIdentity(value) {
  if (typeof value !== 'string' || !value.trim()) throw new Error('repository identity is required');
  let raw = value.trim();
  const scp = raw.match(/^([^@/:]+)@([^:]+):(.+)$/);
  if (scp) raw = `ssh://${scp[2]}/${scp[3]}`;
  let url;
  try { url = new URL(raw); } catch { throw new Error('repository identity must be an absolute HTTPS/SSH URL or SSH scp remote'); }
  if (!['https:', 'ssh:'].includes(url.protocol) || !url.hostname || (url.protocol === 'https:' && (url.username || url.password)) || url.password || url.search || url.hash) throw new Error('unsupported repository identity URL');
  const hostname = url.hostname.toLowerCase();
  let repoPath = url.pathname.replace(/^\/+/, '').replace(/\/+$/, '').replace(/\.git$/i, '');
  if (hostname === 'github.com') repoPath = repoPath.toLowerCase();
  if (!repoPath || repoPath.split('/').some((x) => !x || x === '.' || x === '..')) throw new Error('invalid repository identity path');
  return `https://${hostname}${url.port ? `:${url.port}` : ''}/${repoPath}`;
}

// Default-deny dependency allowlist, loaded the same way as the registry. After trust-sync the protected trust
// carries the activated copy (and its repository registry): new dependencies outside it are denied. Before
// trust-sync the shipped byte copy is used in advisory mode only.
const allowlistUrl = fileURLToPath(new URL('./ownership-dependency-allowlist.json', import.meta.url));
let shippedAllowlist = null;
try { const parsed = JSON.parse(readFileSync(allowlistUrl, 'utf8')); if (validateAllowlist(parsed).ok) shippedAllowlist = parsed; } catch { shippedAllowlist = null; }
const protectedTrustPath = () => process.platform === 'win32' ? 'C:\\ProgramData\\Orthic Labs\\RightKit\\ownership\\trust.json'
  : process.platform === 'darwin' ? '/Library/Application Support/Orthic Labs/RightKit/ownership/trust.json' : null;
const defaultReadTrust = () => { const file = protectedTrustPath(); return file && existsSync(file) ? JSON.parse(readFileSync(file, 'utf8')) : null; };
let allowlistSources = { readTrust: defaultReadTrust, shipped: shippedAllowlist };
/** Test seam: inject the protected trust reader and the shipped allowlist. */
export function setAllowlistSources(sources = {}) { allowlistSources = { readTrust: sources.readTrust ?? defaultReadTrust, shipped: sources.shipped === undefined ? shippedAllowlist : sources.shipped }; }
function activeAllowlist() {
  try {
    const trust = allowlistSources.readTrust();
    if (trust?.dependencyAllowlist && validateAllowlist(trust.dependencyAllowlist).ok && Array.isArray(trust.repositories)) {
      return { allowlist: trust.dependencyAllowlist, trusted: true, guarded: (identity) => trust.repositories.some((entry) => entry.repositoryIdentity === identity) };
    }
  } catch { /* unreadable or malformed trust: fall back to the advisory shipped copy */ }
  const shipped = allowlistSources.shipped;
  return shipped ? { allowlist: shipped, trusted: false, guarded: (identity) => shipped.repositories.some((entry) => entry.repositoryIdentity === identity) } : null;
}

function defaultGit(args) {
  const result = spawnSync('git', args, { encoding: 'utf8', windowsHide: true, timeout: 5_000 });
  return result.status === 0 ? String(result.stdout ?? '').trim() : null;
}
// Workspace roots are always in scope by path. Override with RIGHTKIT_WORKSPACE_ROOTS (path-delimited).
const defaultRoots = () => process.env.RIGHTKIT_WORKSPACE_ROOTS ? process.env.RIGHTKIT_WORKSPACE_ROOTS.split(process.platform === 'win32' ? ';' : ':').filter(Boolean)
  : process.platform === 'win32' ? ['D:\\Claude'] : ['/Volumes/D/claude'];
const defaultRealpath = (value) => { try { return realpathSync(value); } catch { return value; } };
let scopeDeps = { git: defaultGit, exists: existsSync, roots: defaultRoots, realpath: defaultRealpath, caseInsensitive: process.platform === 'win32' };
const dirScope = new Map(); const topScope = new Map(); const topIdentity = new Map();
/** Test seam: inject the git runner (args => stdout|null) and existence probe; clears caches. */
export function setScopeDependencies(deps = {}) {
  scopeDeps = { git: deps.git ?? defaultGit, exists: deps.exists ?? existsSync, roots: deps.roots ?? defaultRoots, realpath: deps.realpath ?? defaultRealpath, caseInsensitive: deps.caseInsensitive ?? process.platform === 'win32' };
  dirScope.clear(); topScope.clear(); topIdentity.clear();
}

/** Canonical origin identity of the repository containing `dir`, or null. */
function repositoryIdentityOf(dir) {
  try {
    const top = scopeDeps.git(['-C', dir, 'rev-parse', '--show-toplevel']);
    if (!top) return null;
    if (!topIdentity.has(top)) {
      const origin = scopeDeps.git(['-C', top, 'remote', 'get-url', 'origin']);
      let identity = null; try { identity = origin ? canonicalRepositoryIdentity(origin) : null; } catch { identity = null; }
      topIdentity.set(top, identity);
    }
    return topIdentity.get(top);
  } catch { return null; }
}

function repositoryInScope(dir) {
  if (dirScope.has(dir)) return dirScope.get(dir);
  let result = false;
  try {
    const top = scopeDeps.git(['-C', dir, 'rev-parse', '--show-toplevel']);
    if (top) {
      if (!topScope.has(top)) {
        const identity = repositoryIdentityOf(dir);
        topScope.set(top, identity !== null && registry.has(identity));
      }
      result = topScope.get(top);
    }
  } catch { result = false; }
  dirScope.set(dir, result);
  return result;
}

const driveAbsolute = (value) => /^[A-Za-z]:[\\/]/.test(value) || /^\\\\/.test(value);
function parentOf(value) {
  if (!driveAbsolute(value)) return dirname(value);
  const trimmed = value.replace(/[\\/]+$/, ''); const cut = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'));
  return cut <= 2 && /^[A-Za-z]:/.test(trimmed) ? `${trimmed.slice(0, 2)}/` : cut > 0 ? trimmed.slice(0, cut) : trimmed;
}
function nearestExisting(start) {
  let current = start;
  for (let depth = 0; depth < 64; depth += 1) {
    if (scopeDeps.exists(current)) return current;
    const parent = parentOf(current); if (parent === current) return null;
    current = parent;
  }
  return null;
}

function underWorkspace(existing) {
  const fold = (value) => { const slashed = String(value).replaceAll('\\', '/').replace(/\/+$/, ''); return scopeDeps.caseInsensitive ? slashed.toLowerCase() : slashed; };
  const target = fold(scopeDeps.realpath(existing)); const rawTarget = fold(existing);
  const roots = typeof scopeDeps.roots === 'function' ? scopeDeps.roots() : scopeDeps.roots;
  return roots.some((root) => [fold(root), fold(scopeDeps.realpath(root))].some((base) => base && [target, rawTarget].some((candidate) => candidate === base || candidate.startsWith(`${base}/`))));
}
function inScopeDirectory(dir) {
  if (typeof dir !== 'string' || !dir) return false;
  const existing = nearestExisting(dir); if (!existing) return false;
  try { if (underWorkspace(existing)) return true; } catch { /* fall through to registry */ }
  return registryError ? false : repositoryInScope(existing);
}
function inScopeFile(file, cwd) {
  if (typeof file !== 'string') return false;
  if (driveAbsolute(file)) return inScopeDirectory(parentOf(file));
  if (!isAbsolute(file) && typeof cwd !== 'string') return false;
  return inScopeDirectory(dirname(isAbsolute(file) ? file : resolve(cwd, file)));
}

function isExceptionPath(value, cwd) {
  const normalized = normalizedPath(value, cwd);
  return Boolean(normalized && normalized.split('/').some((part, index, parts) =>
    part === 'ownership-exceptions.json' && parts[index - 1] === '.rightkit'));
}

function isTrustPath(value, cwd) {
  const normalized = normalizedPath(value, cwd);
  return Boolean(normalized && normalized.includes('/orthic labs/rightkit/ownership/trust.json'));
}

function isProtectedPath(value, cwd) { return isExceptionPath(value, cwd) || isTrustPath(value, cwd); }

function writeTargetException(command, cwd) {
  if (typeof command !== 'string') return false;
  // This hook is advisory feedback. Any command naming the protected record is
  // denied; broker/build admission remains authoritative for opaque shell IO.
  if (!/(?:ownership-exceptions\.json|(?:Orthic Labs[\\/]RightKit|RightKit[\\/]ownership)[\\/]trust\.json)/i.test(command)) return false;
  const hasWriteIntent = /(?:>|\btee\b|\b(?:mv|cp|copy|move|del|rm|remove-item|set-content|add-content|out-file|apply_patch|sed\s+-i|perl\s+-i|createWriteStream)\b|\bfs(?:\.promises)?\.(?:writeFile|appendFile|rename|rm|unlink|open)|writeFileSync|writeFile\s*\()/i.test(command);
  return hasWriteIntent && (isProtectedPath(command, cwd) || /\.rightkit[\\/]ownership-exceptions\.json/i.test(command) || /(?:Orthic Labs[\\/]RightKit|RightKit[\\/]ownership)[\\/]trust\.json/i.test(command));
}

function applyEdit(oldText, oldString, newString, replaceAll = false) {
  if (typeof oldString !== 'string' || typeof newString !== 'string' || oldString.length === 0) return null;
  const first = oldText.indexOf(oldString);
  if (first < 0) return null;
  if (replaceAll) return oldText.split(oldString).join(newString);
  if (oldText.indexOf(oldString, first + oldString.length) >= 0) return null;
  return oldText.slice(0, first) + newString + oldText.slice(first + oldString.length);
}

function reconstructClaude(input, current) {
  const tool = input?.tool_name ?? input?.toolName;
  const value = input?.tool_input ?? input?.toolInput ?? {};
  const manifest = isManifest(value.file_path ?? value.filePath ?? value.path ?? value.file);
  if (tool === 'Write' && typeof value.content === 'string') return value.content;
  if (tool === 'Edit') return applyEdit(current, value.old_string ?? value.oldString, value.new_string ?? value.newString, manifest && value.replace_all === true);
  if (tool === 'MultiEdit' && Array.isArray(value.edits)) {
    let result = current;
    for (const edit of value.edits) {
      result = applyEdit(result, edit.old_string ?? edit.oldString, edit.new_string ?? edit.newString, manifest && edit.replace_all === true);
      if (result === null) return null;
    }
    return result;
  }
  return null;
}

function patchFiles(patch) {
  if (typeof patch !== 'string' || !patch.includes('*** Begin Patch')) return null;
  const files = []; let active = null; let hunk = false;
  for (const line of patch.replace(/\r/g, '').split('\n')) {
    const header = /^\*\*\* (Update|Add|Delete) File: (.+)$/.exec(line);
    if (header) {
      if (active) files.push(active);
    active = { kind: header[1], path: header[2], hunks: [], added: [] }; hunk = false;
      continue;
    }
    const move = /^\*\*\* Move to: (.+)$/.exec(line);
    if (move && active) { active.path = `${active.path}\n${move[1]}`; continue; }
    if (!active || /^\*\*\* (?:Begin|End) Patch$/.test(line) || /^\*\*\* End of File$/.test(line)) continue;
    if (line.startsWith('@@')) {
      hunk = true;
      active.hunks.push({ anchor: line.slice(2).replace(/^@+\s*/, '').trim(), lines: [] });
      continue;
    }
    if (active.kind === 'Add') {
      if (line.startsWith('+')) active.added.push(line.slice(1));
      continue;
    }
    if (hunk && /^[ +\\-]/.test(line)) active.hunks.at(-1)?.lines.push(line);
  }
  if (active) files.push(active);
  return files.length ? files : null;
}

function reconstructPatch(current, file) {
  if (file.kind !== 'Update') {
    if (file.kind === 'Delete') return '';
    return file.added.join('\n');
  }
  const output = current.replace(/\r/g, '').split('\n'); let cursor = 0;
  for (const hunk of file.hunks) {
    const numbered = isManifest(file.path.split('\n').at(-1)) && /^-\d+(?:,\d+)? \+(\d+)(?:,\d+)?(?: @@)?$/.exec(hunk.anchor);
    if (numbered) cursor = Math.max(0, Number(numbered[1]) - 1);
    else if (hunk.anchor) {
      const anchorIndex = output.findIndex((line, index) => index >= cursor && line.includes(hunk.anchor));
      if (anchorIndex < 0) return null;
      cursor = anchorIndex + 1;
    } else if (hunk.lines[0]?.[0] !== '+') {
      const firstOldLine = hunk.lines.find((line) => line[0] === ' ' || line[0] === '-')?.slice(1);
      if (firstOldLine === undefined) return null;
      const firstIndex = output.findIndex((line, index) => index >= cursor && line === firstOldLine);
      if (firstIndex < 0) return null;
      cursor = firstIndex;
    } else return null;
    for (const line of hunk.lines) {
      const marker = line[0]; const text = line.slice(1);
      if (marker === ' ') { if (output[cursor] !== text) return null; cursor += 1; }
      else if (marker === '-') { if (output[cursor] !== text) return null; output.splice(cursor, 1); }
      else if (marker === '+') { output.splice(cursor, 0, text); cursor += 1; }
      else if (marker !== '\\') return null;
    }
  }
  return file.hunks.length ? output.join('\n') : null;
}

function stripComments(text, extension) {
  const hashComments = ['.py', '.ps1', '.rb', '.sh'].includes(extension);
  let result = ''; let quote = ''; let escaped = false; let lineComment = false; let blockComment = false;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i]; const next = text[i + 1];
    if (lineComment) { if (ch === '\n') { lineComment = false; result += '\n'; } else result += ' '; continue; }
    if (blockComment) { if (ch === '*' && next === '/') { result += '  '; i += 1; blockComment = false; } else result += ch === '\n' ? '\n' : ' '; continue; }
    if (quote) { result += ch; if (escaped) escaped = false; else if (ch === '\\') escaped = true; else if (ch === quote) quote = ''; continue; }
    if (ch === '"' || ch === "'" || ch === '`') { quote = ch; result += ch; continue; }
    if (ch === '/' && next === '/') { result += '  '; i += 1; lineComment = true; continue; }
    if (ch === '/' && next === '*') { result += '  '; i += 1; blockComment = true; continue; }
    if (hashComments && ch === '#') { result += ' '; lineComment = true; continue; }
    result += ch;
  }
  return result;
}

function scannedText(filePath, text, cwd) {
  const extension = extname(resolve(cwd, filePath)).toLowerCase();
  return sourceExtensions.has(extension) ? stripComments(text, extension) : '';
}

// RightKit's own tree implements the owned capabilities, so owner source and manifests are never
// screened as duplicates. Advisory only; protected gates scan registered app repositories.
// Only the event's own cwd counts; the hook process cwd says nothing about the edited file.
let explicitCwd;
function isRightKitTree(target, cwd) {
  if (typeof target !== 'string' || (!isAbsolute(target) && typeof cwd !== 'string')) return false;
  const normalized = (isAbsolute(target) ? target : resolve(cwd, target)).replaceAll('\\', '/').toLowerCase();
  return normalized.includes('/claude/rightkit/');
}
function hasFingerprintIncrease(beforePath, before, after, cwd) {
  if (isRightKitTree(beforePath, explicitCwd)) return undefined;
  const oldText = scannedText(beforePath, before, cwd);
  const newText = scannedText(beforePath, after, cwd);
  return fingerprints.find((entry) => occurrenceCount(newText, entry, beforePath) > occurrenceCount(oldText, entry, beforePath));
}

function fallbackPatchDecision(patch, cwd) {
  if (typeof patch !== 'string') return null;
  const paths = [...patch.matchAll(/^\*\*\* (?:Update|Add|Delete) File: (.+)$/gm)].map((match) => match[1]);
  if (paths.some((path) => isProtectedPath(path, cwd))) return denial('Agent changes to ownership exception or protected trust records are blocked.');
  const added = patch.split(/\r?\n/).filter((line) => line.startsWith('+') && !line.startsWith('+++')).map((line) => line.slice(1)).join('\n');
  if (paths.some((path) => sourceExtensions.has(extname(resolve(cwd, path)).toLowerCase()))
    && paths.some((file) => fingerprints.some((entry) => occurrenceCount(added, entry, file)))) {
    const match = fingerprints.find((entry) => paths.some((file) => occurrenceCount(added, entry, file)));
    return fingerprintDecision(match, 'Could not safely reconstruct patch containing a RightKit source fingerprint.');
  }
  return null;
}

function occurrenceCount(text, entry, filePath) {
  const extension = extname(filePath).toLowerCase();
  if (entry.extensions && !entry.extensions.includes(extension)) return 0;
  const { regex } = entry;
  const flags = regex.flags.includes('g') ? regex.flags : `${regex.flags}g`;
  const matches = [...text.matchAll(new RegExp(regex.source, flags))];
  if (!entry.codeOnly) return matches.length;
  const nonCode = sourceNonCode(text, extension === '.rs');
  return matches.filter((match) => !nonCode[match.index]).length;
}

function denial(reason, required = 'use the published RightKit capability; exception records require separate owner approval.') {
  return { hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: `[HOOK:rightkit-ownership:block]\nWhy: ${reason}\nRequired: ${required}` } };
}

export function fingerprintDecision(entry, reason) {
  if (entry.tier === 'block') return denial(reason);
  return { decision: 'allow', hookSpecificOutput: { hookEventName: 'PreToolUse', additionalContext: `[HOOK:rightkit-ownership:advise] ${reason} Usage context & replacement qualification require review.` } };
}

const manifestName = (path) => path.replace(/\\/g, '/').split('/').at(-1);
const isManifest = (path) => typeof path === 'string' && ['Cargo.toml', 'package.json', 'Package.swift'].includes(manifestName(path));
const advice = (reason) => fingerprintDecision({ tier: 'advise' }, reason);
const manifestFailure = (path) => advice(`could not screen manifest edit: ${path}. Ownership hooks are advisory; protected gates remain authoritative.`);
const object = (value) => value !== null && typeof value === 'object' && !Array.isArray(value);

// Small self-contained TOML reader: unsupported syntax throws into advisory
// feedback, never a guessed dependency verdict. No installed npm closure needed.
function cargoManifest(text) {
  let at = 0; let table = []; const records = []; const seen = new Set(); const tables = new Set();
  const fail = () => { throw new Error('invalid or unsupported TOML'); };
  const space = (lines = false) => {
    while (at < text.length) {
      if ((lines ? /\s/ : /[ \t\r]/).test(text[at])) { at += 1; continue; }
      if (text[at] === '#') { while (at < text.length && text[at] !== '\n') at += 1; continue; }
      break;
    }
  };
  const string = () => {
    const quote = text[at]; const triple = text.slice(at, at + 3) === quote.repeat(3);
    const end = triple ? quote.repeat(3) : quote; at += end.length; let result = '';
    if (triple && text[at] === '\n') at += 1;
    while (at < text.length) {
      if (text.slice(at, at + end.length) === end) { at += end.length; return result; }
      const ch = text[at++];
      if (!triple && /[\r\n]/.test(ch)) fail();
      if (ch !== '\\' || quote === "'") { result += ch; continue; }
      if (triple && /\s/.test(text[at] ?? '')) { while (at < text.length && /\s/.test(text[at])) at += 1; continue; }
      const escape = text[at++]; const escapes = { b: '\b', t: '\t', n: '\n', f: '\f', r: '\r', '"': '"', '\\': '\\' };
      if (Object.hasOwn(escapes, escape)) result += escapes[escape];
      else if (escape === 'u' || escape === 'U') {
        const count = escape === 'u' ? 4 : 8; const hex = text.slice(at, at + count);
        if (!new RegExp(`^[0-9a-fA-F]{${count}}$`).test(hex)) fail();
        result += String.fromCodePoint(parseInt(hex, 16)); at += count;
      } else fail();
    }
    return fail();
  };
  const keys = () => {
    const parts = [];
    do {
      space(); let key;
      if (text[at] === '"' || text[at] === "'") key = string();
      else { key = /^[A-Za-z0-9_-]+/.exec(text.slice(at))?.[0]; if (!key) fail(); at += key.length; }
      parts.push(key); space();
      if (text[at] !== '.') return parts;
      at += 1;
    } while (at < text.length);
    return fail();
  };
  const value = (depth = 0) => {
    if (depth > 64) fail(); space(true);
    if (text[at] === '"' || text[at] === "'") return string();
    if (text[at] === '[') {
      at += 1; const list = []; space(true);
      while (text[at] !== ']') {
        list.push(value(depth + 1)); space(true);
        if (text[at] === ']') break;
        if (text[at++] !== ',') fail(); space(true);
      }
      at += 1; return list;
    }
    if (text[at] === '{') {
      at += 1; const fields = Object.create(null); space(true);
      while (text[at] !== '}') {
        const key = keys().join('.'); if (Object.hasOwn(fields, key) || text[at++] !== '=') fail();
        fields[key] = value(depth + 1); space(true);
        if (text[at] === '}') break;
        if (text[at++] !== ',') fail(); space(true);
      }
      at += 1; return fields;
    }
    const token = /^[^\s,#\]}]+/.exec(text.slice(at))?.[0]; if (!token) fail(); at += token.length;
    if (token === 'true' || token === 'false') return token === 'true';
    if (/^[+-]?(?:\d[\d_]*(?:\.[\d_]+)?(?:[eE][+-]?[\d_]+)?|inf|nan)$/.test(token) || /^0[xob][0-9a-fA-F_]+$/.test(token)) return Number(token.replace(/_/g, ''));
    if (/^\d{4}-\d{2}-\d{2}(?:T[^\s]+)?$/.test(token)) return new Date(token);
    return fail();
  };
  while (at < text.length) {
    space(true); if (at >= text.length) break;
    if (text[at] === '[') {
      at += 1; const array = text[at] === '['; if (array) at += 1;
      table = keys(); if (text[at++] !== ']' || array && text[at++] !== ']') fail();
      const id = JSON.stringify(table); if (!array && tables.has(id)) fail(); tables.add(id);
      if (array) table = [...table, `#${records.length}`];
    } else {
      const key = keys(); if (text[at++] !== '=') fail();
      const full = [...table, ...key]; const id = JSON.stringify(full); if (seen.has(id)) fail(); seen.add(id);
      records.push({ path: full, value: value() });
    }
    space(); if (at < text.length && text[at++] !== '\n') fail();
  }
  const dependencies = new Map(); const kinds = new Map(); const metadata = {};
  const rank = { registry: 0, any: 1, git: 2, path: 3 };
  const upgrade = (id, kind) => { if (rank[kind] > rank[kinds.get(id) ?? 'registry']) kinds.set(id, kind); };
  for (const entry of records) {
    const parts = entry.path;
    if (parts.length === 2 && parts[0] === 'package' && ['name', 'description'].includes(parts[1]) && typeof entry.value === 'string') metadata[parts[1]] = entry.value;
    const group = parts.findIndex((part, index) => ['dependencies', 'dev-dependencies', 'build-dependencies'].includes(part)
      && (index === 0 || index === 1 && parts[0] === 'workspace' || index === 2 && parts[0] === 'target'));
    if (group < 0 || !parts[group + 1]) continue;
    const alias = parts[group + 1]; const id = JSON.stringify(parts.slice(0, group + 2));
    if (parts.length === group + 2) {
      if (typeof entry.value !== 'string' && !object(entry.value)) fail();
      const name = object(entry.value) ? entry.value.package ?? alias : alias; if (typeof name !== 'string') fail();
      dependencies.set(id, name);
      if (object(entry.value)) upgrade(id, entry.value.path !== undefined ? 'path' : entry.value.git !== undefined ? 'git' : entry.value.workspace === true ? 'any' : 'registry');
    } else if (parts.length === group + 3) {
      if (!dependencies.has(id)) dependencies.set(id, alias);
      if (parts.at(-1) === 'path') upgrade(id, 'path'); else if (parts.at(-1) === 'git') upgrade(id, 'git'); else if (parts.at(-1) === 'workspace' && entry.value === true) upgrade(id, 'any');
      if (parts.at(-1) === 'package') { if (typeof entry.value !== 'string') fail(); dependencies.set(id, entry.value); }
    } else fail();
  }
  return { ecosystem: 'cargo', names: new Set(dependencies.values()), entries: [...dependencies].map(([id, name]) => ({ name, kind: kinds.get(id) ?? 'registry' })), ...metadata };
}

function npmName(value) {
  return /^(?:[^@]+@)?npm:(@[^/]+\/[^@]+|[^@]+)(?:@|$)/.exec(value)?.[1]
    ?? /^(@[^/]+\/[^@]+|[^@]+)(?:@|$)/.exec(value)?.[1];
}

function parseManifest(path, text) {
  const base = manifestName(path);
  if (base === 'Cargo.toml') return cargoManifest(text);
  const names = new Set(); const entries = [];
  if (base === 'package.json') {
    const pkg = JSON.parse(text); if (!object(pkg)) throw new Error('invalid package.json');
    for (const section of ['dependencies', 'devDependencies', 'optionalDependencies', 'peerDependencies']) {
      if (pkg[section] === undefined) continue;
      if (!object(pkg[section])) throw new Error('invalid dependency section');
      for (const [name, spec] of Object.entries(pkg[section])) {
        if (typeof spec !== 'string') throw new Error('invalid dependency declaration');
        const actual = spec.startsWith('npm:') ? npmName(spec) : name; if (!actual) throw new Error('invalid npm alias');
        names.add(actual);
        entries.push({ name: actual, kind: /^(?:file:|link:|workspace:)/.test(spec) ? 'path' : /^(?:git\+|github:|https?:.*\.git)/i.test(spec) ? 'git' : 'registry' });
      }
    }
    return { ecosystem: 'npm', names, entries, name: pkg.name, description: pkg.description };
  }
  const code = stripComments(text, '.swift');
  for (const match of code.matchAll(/\.package\s*\(/g)) {
    let at = match.index + match[0].length; const start = at; let depth = 1; let quote = false;
    for (; at < code.length && depth; at += 1) {
      if (quote && code[at] === '\\') { at += 1; continue; }
      if (code[at] === '"') quote = !quote;
      else if (!quote && code[at] === '(') depth += 1;
      else if (!quote && code[at] === ')') depth -= 1;
    }
    if (depth || quote) throw new Error('invalid Swift package declaration');
    const args = code.slice(start, at - 1); const field = (key) => new RegExp(`\\b${key}\\s*:\\s*"([^"]+)"`).exec(args)?.[1];
    const source = field('url') ?? field('path');
    const name = field('name') ?? field('id') ?? source?.replace(/\.git$/i, '').split(/[\\/]/).at(-1)?.toLowerCase();
    if (!name) throw new Error('unsupported Swift package identity');
    names.add(name);
    entries.push({ name, kind: field('path') ? 'path' : field('id') && !field('url') ? 'registry' : 'git' });
  }
  return { ecosystem: 'swiftpm', names, entries };
}

function dependencyFeedback(dependencies) {
  const matches = catalog.capabilities.flatMap((capability) => capability.fingerprints
    .filter((entry) => entry.type === 'dependency' && dependencies.some((dep) => dep.ecosystem === entry.ecosystem && dep.name === entry.name))
    .map((entry) => ({ ...entry, capability: capability.capability, package: capability.package })));
  if (!matches.length) return null;
  const block = matches.some((entry) => entry.tier === 'block' || entry.policy === 'banned');
  const reason = matches.map((entry) => `Dependency ${entry.name} matches capability ${entry.capability}; RightKit owner: ${entry.package}. Use ${entry.package} instead or request a signed exception.`).join('\n');
  return block ? denial(reason) : advice(reason);
}

const dirOfFile = (file, cwd) => driveAbsolute(file) ? parentOf(file) : dirname(isAbsolute(file) ? file : resolve(cwd, file));
/** Default-deny allowlist screening of NEW direct dependencies in a guarded (registered) repository. */
function allowlistFeedback(dependencies, dir) {
  if (!dependencies.length || typeof dir !== 'string') return null;
  const active = activeAllowlist(); if (!active) return null;
  const identity = repositoryIdentityOf(nearestExisting(dir) ?? dir);
  if (!identity || !active.guarded(identity)) return null;
  const owners = catalog.capabilities.map((capability) => capability.package);
  const unlisted = dependencies.filter((dependency) => !evaluateDependency(active.allowlist, identity, dependency, { owners }).allowed);
  if (!unlisted.length) return null;
  const reason = `New dependency ${unlisted.map((dependency) => `${dependency.ecosystem}:${dependency.name}`).join(', ')} is not in the ownership dependency allowlist for ${identity}.`;
  const required = 'propose it in ownership-dependency-allowlist.json (advisory until protected trust carries it; trust-sync is retired).';
  return active.trusted ? denial(reason, required) : advice(`${reason} Propose it in ownership-dependency-allowlist.json; it stays advisory until protected trust carries the allowlist (trust-sync is retired).`);
}
/** Catalog capability screening first (block-tier always wins), then the allowlist. */
function dependencyDecision(dependencies, dir) {
  const catalogFeedback = dependencyFeedback(dependencies);
  if (catalogFeedback?.hookSpecificOutput.permissionDecision === 'deny') return catalogFeedback;
  const listed = allowlistFeedback(dependencies, dir);
  if (listed?.hookSpecificOutput.permissionDecision === 'deny') return listed;
  if (catalogFeedback && listed) return advice(`${catalogFeedback.hookSpecificOutput.additionalContext}\n${listed.hookSpecificOutput.additionalContext}`);
  return catalogFeedback ?? listed;
}

// Fixed discovery synonyms are bound to owners, rather than every capability.
const keywordOwners = {
  oauth: ['rightkit-oauth'], 'model-store': ['rightkit-models'], models: ['rightkit-models'],
  supervisor: ['rightkit-child-process'], ocr: ['rightkit-ocr'], speech: ['rightkit-speech-client'],
  asr: ['rightkit-asr'], tts: ['rightkit-tts'], sync: ['rightkit-sync'],
  secrets: ['rightkit-credentials'], credential: ['rightkit-credentials', 'RightKitSwift'], keychain: ['rightkit-credentials', 'RightKitSwift'],
  watcher: ['rightkit-file-watch'], updater: ['@rightkit/app-update'], onboarding: ['@rightkit/onboarding'],
  settings: ['rightkit-app-settings', '@rightkit/settings-ui'], telemetry: ['rightkit-logs', '@rightkit/logs'],
  logs: ['rightkit-logs', '@rightkit/logs'], crash: ['rightkit-crash'], sandbox: ['rightkit-sandbox'],
  browser: ['rightkit-browser'], mcp: ['rightkit-mcp'], pdf: ['rightkit-pdf'],
  media: ['rightkit-ffmpeg'], audio: ['rightkit-audio'], hotkeys: ['rightkit-hotkeys'],
  overlay: ['rightkit-overlay'], license: ['rightkit-license', '@rightkit/license'],
};

function manifestFeedback(path, before, after, created, cwd) {
  if (isRightKitTree(path, explicitCwd)) return null;
  try {
    if (after === null) return manifestFailure(path);
    const post = parseManifest(path, after);
    const previous = created ? new Set() : parseManifest(path, before).names;
    const feedback = dependencyDecision((post.entries ?? [...post.names].map((name) => ({ name, kind: 'registry' }))).filter((entry) => !previous.has(entry.name)).map((entry) => ({ ecosystem: post.ecosystem, name: entry.name, kind: entry.kind })), dirOfFile(path, cwd));
    if (feedback) return feedback;
    if (!created || manifestName(path) === 'Package.swift' || /(?:^|\/)rightkit(?:\/|$)/i.test(normalizedPath(path, cwd))) return null;
    const tokens = [post.name, post.description].filter((value) => typeof value === 'string').join(' ').toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);
    const hasKeyword = (keyword) => {
      const parts = keyword.toLowerCase().split('-');
      return tokens.some((_, index) => parts.every((part, offset) => tokens[index + offset] === part));
    };
    const owners = catalog.capabilities.filter((capability) => !post.names.has(capability.package)
      && [...capability.capability.split('-'), ...Object.keys(keywordOwners).filter((keyword) => keywordOwners[keyword].includes(capability.package))].some(hasKeyword));
    return owners.length ? advice(owners.map((entry) => `New manifest suggests capability ${entry.capability}; declare RightKit owner ${entry.package} as a dependency.`).join('\n')) : null;
  } catch { return manifestFailure(path); }
}

function commandDependencies(command) {
  if (typeof command !== 'string') return [];
  const segments = []; let words = []; let word = ''; let quote = ''; let quoted = false; let skipWord = false;
  // A finished word is dropped when it is the target of a redirection (`> file`, `2>&1`'s `1`).
  const flush = () => { if (word || quoted) { if (skipWord) skipWord = false; else if (word) words.push(word); } word = ''; quoted = false; };
  const split = () => { flush(); skipWord = false; segments.push(words); words = []; };
  for (let at = 0; at < command.length; at += 1) {
    const ch = command[at];
    if (quote) { if (ch === quote) quote = ''; else if (ch === '\\' && quote === '"' && command[at + 1] === '"') word += command[++at]; else word += ch; continue; }
    if (ch === "'" || ch === '"') { quote = ch; quoted = true; continue; }
    if (ch === '\\' && at + 1 < command.length && /[\s|&;<>()$`"'#\\]/.test(command[at + 1])) { word += command[++at]; continue; }
    if (ch === '#' && !word) { while (at < command.length && command[at] !== '\n') at += 1; split(); continue; }
    // Command substitution and grouping: `$(`, `(`, `)` and backticks start or end a command.
    if (ch === '$' && command[at + 1] === '(') { split(); at += 1; continue; }
    if (ch === '(' || ch === ')' || ch === '`') { split(); continue; }
    // Redirections: `>`, `>>`, `>&`, `>|`, `<`, `<<`, `<<<`, `<&`, `<>`, `&>`, `&>>`, with an optional leading fd number.
    if (ch === '>' || ch === '<' || (ch === '&' && command[at + 1] === '>')) {
      if (/^\d+$/.test(word) && !quoted) word = ''; else flush();
      if (ch === '&') at += 1;
      const operator = command[at];
      while (command[at + 1] === operator || (operator === '>' && command[at + 1] === '|') || ((operator === '>' || operator === '<') && command[at + 1] === '&') || (operator === '<' && command[at + 1] === '>')) {
        at += 1; if (command[at] === '&' || command[at] === '|' || command[at] === '>' && operator === '<') break;
      }
      skipWord = true; continue;
    }
    if (ch === ';' || ch === '\n' || ch === '|' || ch === '&') {
      split(); if ((ch === '&' || ch === '|') && command[at + 1] === ch) at += 1;
    } else if (/\s/.test(ch)) flush();
    else word += ch;
  }
  if (quote) throw new Error('unclosed shell quote');
  flush(); segments.push(words);
  const dependencies = [];
  const valueFlags = new Set(['--package', '-p', '--rename', '--features', '-F', '--manifest-path', '--target', '--registry', '--git', '--branch', '--tag', '--rev', '--path', '--workspace', '-w', '--filter', '--dir', '-C', '--prefix', '--cache', '--userconfig', '--globalconfig']);
  for (const words of segments) {
    const program = words[0]?.replace(/\\/g, '/').split('/').at(-1)?.replace(/\.exe$/i, '');
    if (!['cargo', 'pnpm', 'npm', 'yarn'].includes(program)) continue;
    const verbs = program === 'cargo' || program === 'yarn' ? ['add'] : program === 'pnpm' ? ['add', 'install'] : ['i', 'install', 'add'];
    if (!verbs.includes(words[1])) continue;
    let kind = 'registry'; const found = [];
    for (let index = 2; index < words.length; index += 1) {
      const word = words[index];
      if (word === '--path') kind = 'path'; else if (word === '--git' && kind !== 'path') kind = 'git';
      if (word.startsWith('-')) { if (valueFlags.has(word) && !(program === 'pnpm' && ['-w', '--workspace'].includes(word))) index += 1; continue; }
      const name = npmName(word); if (name) found.push({ ecosystem: program === 'cargo' ? 'cargo' : 'npm', name });
    }
    dependencies.push(...found.map((dependency) => ({ ...dependency, kind })));
  }
  return dependencies;
}

export function ownershipWriteDecision(input, options = {}) {
  try {
    if (catalogError) return advice('Could not load ownership catalog; protected ownership gates remain authoritative.');
    return writeDecision(input, options);
  } catch {
    const data = input?.tool_input ?? input?.toolInput; const path = data?.file_path ?? data?.filePath ?? data?.path ?? data?.file;
    return isManifest(path) ? manifestFailure(path) : advice('Could not screen ownership write input; protected ownership gates remain authoritative.');
  }
}

function writeDecision(input, { readFile = readFileSync, cwd = input?.cwd ?? process.cwd() } = {}) {
  explicitCwd = typeof input?.cwd === 'string' ? input.cwd : undefined;
  const tool = input?.tool_name ?? input?.toolName ?? '';
  const data = input?.tool_input ?? input?.toolInput ?? {};
  const command = tool === 'exec_command' ? data.cmd ?? input?.cmd : data.command ?? data.cmd ?? data.code ?? data.input ?? input?.command ?? input?.cmd ?? input?.code;
  if (writeTargetException(command, cwd)) return denial('Agent changes to ownership exception or protected trust records are blocked.');
  if (['Bash', 'bash', 'shell', 'Shell', 'exec_command', 'shell_command'].includes(tool)) {
    const feedback = inScopeDirectory(cwd) ? dependencyDecision(commandDependencies(command), cwd) : null; if (feedback) return feedback;
  }
  const path = data.file_path ?? data.filePath ?? data.path ?? data.file;
  const patch = data.patch ?? data.input ?? input?.patch ?? (tool === 'apply_patch' ? data.code : undefined);
  if (tool === 'apply_patch') {
    const files = patchFiles(patch);
    if (!files && typeof patch === 'string') {
      const paths = [...patch.matchAll(/^\*\*\* (?:Update|Add|Delete) File: (.+)$/gm)].map((match) => match[1]);
      if (!paths.some((file) => inScopeFile(file, cwd))) return paths.some((file) => isProtectedPath(file, cwd)) ? denial('Agent changes to ownership exception or protected trust records are blocked.') : { decision: 'allow' };
    }
    if (!files) return fallbackPatchDecision(patch, cwd) ?? (typeof patch === 'string' && [...patch.matchAll(/^\*\*\* (?:Update|Add|Delete) File: (.+)$/gm)].some((match) => isManifest(match[1])) ? manifestFailure('apply_patch') : { decision: 'allow' });
    let feedback = null; const manifests = new Set();
    for (const file of files) {
      const [oldPath, newPath] = file.path.split('\n'); const target = newPath ?? oldPath;
      if (!isManifest(target) || file.kind === 'Delete' || !inScopeFile(target, cwd)) continue;
      manifests.add(file);
      let current = ''; let created = false; let post = null;
      try { current = readFile(resolve(cwd, oldPath), 'utf8'); post = reconstructPatch(current, file); }
      catch (error) { if (error?.code === 'ENOENT') { created = !newPath; post = reconstructPatch(current, file); } }
      const result = manifestFeedback(target, current, post, created, cwd);
      if (result?.hookSpecificOutput.permissionDecision === 'deny') return result;
      if (result) feedback = feedback ? advice(`${feedback.hookSpecificOutput.additionalContext}\n${result.hookSpecificOutput.additionalContext}`) : result;
    }
    for (const file of files) {
      const [oldPath, newPath] = file.path.split('\n');
      if (isProtectedPath(oldPath, cwd) || (newPath && isProtectedPath(newPath, cwd))) return denial('Agent changes to ownership exception or protected trust records are blocked.');
      if (manifests.has(file) || !inScopeFile(newPath ?? oldPath, cwd)) continue;
      const absolute = resolve(cwd, oldPath); let current = '';
      try { current = readFile(absolute, 'utf8'); } catch (error) { if (error?.code !== 'ENOENT') return { decision: 'allow' }; }
      const post = reconstructPatch(current, file);
      if (post === null) {
        const addedText = file.kind === 'Add' ? file.added.join('\n') : file.hunks.flatMap((hunk) => hunk.lines.filter((line) => line.startsWith('+')).map((line) => line.slice(1))).join('\n');
        const likelySource = sourceExtensions.has(extname(resolve(cwd, oldPath)).toLowerCase());
        const addedMatch = likelySource && fingerprints.find((entry) => occurrenceCount(addedText, entry, oldPath));
        if (addedMatch) return fingerprintDecision(addedMatch, 'Could not reconstruct patch adding a RightKit source fingerprint.');
        continue;
      }
      const match = hasFingerprintIncrease(oldPath, current, post, cwd);
      if (match) {
        return fingerprintDecision(match, `Post-edit content adds RightKit source fingerprint ${match.id} (${match.package}).`);
      }
    }
    return feedback ?? { decision: 'allow' };
  }
  if (!['Write', 'Edit', 'MultiEdit'].includes(tool) || typeof path !== 'string') return { decision: 'allow' };
  if (isProtectedPath(path, cwd)) return denial('Agent changes to ownership exception or protected trust records are blocked.');
  const absolute = isAbsolute(path) ? path : resolve(cwd, path);
  if (!inScopeFile(path, cwd)) return { decision: 'allow' };
  let current = ''; let created = false;
  try { current = readFile(absolute, 'utf8'); } catch (error) {
    if (error?.code !== 'ENOENT') return isManifest(path) ? manifestFailure(path) : { decision: 'allow' };
    created = true;
  }
  const post = reconstructClaude(input, current);
  if (isManifest(path)) return manifestFeedback(path, current, post, created, cwd) ?? { decision: 'allow' };
  if (post === null) return { decision: 'allow' };
  const match = hasFingerprintIncrease(path, current, post, cwd);
  return match ? fingerprintDecision(match, `Post-edit content adds RightKit source fingerprint ${match.id} (${match.package}).`) : { decision: 'allow' };
}

export function handleOwnershipWriteHook(input) { return ownershipWriteDecision(input); }
