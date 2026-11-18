// A best-effort cache of successful snapshot dependency preparations. A miss
// always takes the normal offline install path. Records are same-user local
// state, like the gate's result cache; they are not signed attestations.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { installIdentity } from './dependency-identity.mjs';
import { inheritedEnvShape } from './deterministic-scans.mjs';

const hash = (bytes) => crypto.createHash('sha256').update(bytes).digest('hex');
const inside = (p, dir) => p === dir || p.startsWith(dir + path.sep);
const CONFIG_CODES = new Set(['config-list-failed', 'config-path-query-failed', 'config-list-empty', 'config-json-invalid', 'config-path-unknown', 'config-file-unreadable']);
const safeError = (error, fallback) => CONFIG_CODES.has(error?.reuseCode) ? error.reuseCode : fallback;

// Hash all files, symlink targets, directory names and modes, including pnpm
// state and tool caches. Follow linked targets once; never trust stat/mtime.
export function dependencyDigest(snapshot, dirs, { manifest = null } = {}) {
  const h = crypto.createHash('sha256'), seen = new Set(), pending = [];
  let count = 0;
  const root = fs.realpathSync(snapshot);
  const note = (kind, p, mode = null, value = null) => {
    if (!manifest) return;
    if (manifest.length >= 800000) { manifest.truncated = true; return; }
    manifest.push({ kind, pathHash: hash(p), ...(mode === null ? {} : { mode }), ...(value === null ? {} : { valueHash: value }) });
  };
  const walk = (p) => {
    if (++count > 400000) throw new Error('dependency fingerprint exceeds limit');
    const st = fs.lstatSync(p);
    h.update(`${p}\0${st.mode}\0`);
    if (st.isSymbolicLink()) {
      const target = fs.readlinkSync(p);
      h.update(`link\0${target}\0`); if (manifest) note('link', p, st.mode, hash(target));
      try { pending.push(fs.realpathSync(p)); } catch (e) { if (e.code !== 'ENOENT') throw e; h.update('dangling\0'); note('dangling', p); }
    } else if (st.isDirectory()) {
      h.update('dir\0'); note('dir', p, st.mode);
      for (const name of fs.readdirSync(p).sort()) walk(path.join(p, name));
    } else if (st.isFile()) { const digest = hash(fs.readFileSync(p)); h.update(`file\0${digest}\0`); note('file', p, st.mode, digest); }
    else throw new Error('unsupported dependency entry');
  };
  for (const dir of [...dirs].sort()) {
    h.update(`root\0${dir}\0`); note('root', dir);
    try { walk(path.join(snapshot, dir)); } catch (e) { if (e.code !== 'ENOENT' && e.code !== 'ENOTDIR') throw e; h.update('absent\0'); note('absent', dir); }
  }
  while (pending.length) {
    const p = pending.shift();
    if (seen.has(p)) continue;
    seen.add(p);
    if (p === path.parse(p).root || inside(root, p)) throw new Error('dependency link resolves to snapshot ancestor');
    h.update('target\0'); note('target', p); walk(p);
  }
  return h.digest('hex');
}

// Opt-in diagnostics only. Never used as proof; records contain no paths,
// file contents, link targets or environment values. The directory must
// already exist inside the host's scratch tree.
export function captureDependencyManifest(snapshot, dirs) {
  const entries = [];
  const digest = dependencyDigest(snapshot, dirs, { manifest: entries });
  return { digest, entries, complete: !entries.truncated };
}

// GC-A24 (Adrian, 2026-11-05): the proof is pnpm's own install record (installIdentity), not every installed byte; the inputs above
// (tree, pnpm, node, platform, config, store, environment) already bind what the offline frozen install produces. The byte manifest
// stays an opt-in diagnostic only.
function dependencyProof(snapshot, dirs, diagnosticsEnabled) {
  let capture = null;
  if (diagnosticsEnabled) { try { capture = captureDependencyManifest(snapshot, dirs); } catch { capture = null; } }
  return { digest: installIdentity(snapshot, dirs), capture };
}

function writeDiagnostic(dir, phase, root, inputs, components, capture) {
  if (!dir || !path.isAbsolute(dir) || !capture.complete) return;
  let temp;
  try {
    const scratch = fs.realpathSync(process.platform === 'win32' ? 'D:\\scratch' : '/Volumes/D/scratch');
    const target = fs.realpathSync(dir);
    if (!inside(target, scratch) || !fs.statSync(target).isDirectory()) return;
    const file = path.join(target, `dependency-${phase}-${process.pid}-${crypto.randomBytes(8).toString('hex')}.json`);
    temp = `${file}.tmp`;
    fs.writeFileSync(temp, JSON.stringify({ schema: 'rightkit.dependency-manifest/1', phase, rootHash: hash(root), inputs, components, ...capture }), { flag: 'wx' });
    fs.renameSync(temp, file);
  } catch { if (temp) { try { fs.unlinkSync(temp); } catch {} } /* diagnostics never change a proof */ }
}

// Read-only post-Class-A capture. This never reads or updates the reuse record.
export async function capturePostClassADiagnostics({ checkout, snapshot, commit, roots, diagnosticDir }) {
  if (!diagnosticDir || !Array.isArray(roots)) return;
  try {
    const { treeEntries, dependencyDirs } = await import('./snapshot.mjs');
    const entries = treeEntries(checkout, commit);
    const fold = (s) => process.platform === 'darwin' || process.platform === 'win32' ? s.toLowerCase() : s;
    const dirOf = (p) => p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : '';
    const lockRoots = entries.filter((e) => e.path === 'pnpm-lock.yaml' || e.path.endsWith('/pnpm-lock.yaml')).map((e) => fold(dirOf(e.path))).filter((p) => !p.split('/').includes('node_modules'));
    const nearestLock = (member) => lockRoots.filter((r) => !r || member === r || member.startsWith(`${r}/`)).sort((a, b) => b.length - a.length)[0];
    const generated = [...dependencyDirs(entries, 'linux')];
    for (const root of roots) {
      const owner = fold(path.relative(snapshot, path.resolve(snapshot, root)).split(path.sep).join('/'));
      const dirs = generated.filter((d) => nearestLock(fold(dirOf(d))) === owner);
      writeDiagnostic(diagnosticDir, 'after-a', root, null, null, captureDependencyManifest(snapshot, dirs));
    }
  } catch { /* diagnostics never change the gate result */ }
}

/**
 * NN12, Windows: the program `pnpm` starts, named by its bytes, or null when it cannot be named exactly (then preparation runs, as before).
 * Found as the gate starts it (deterministic-scans.mjs resolveInvocation: PATH and PATHEXT, never the current directory). Recognised:
 *   - a cmd shim whose one `%*` line runs `"%~dp0\<path>"`: the path as written when it has an extension, else what cmd runs for it, the
 *     first of `<path>.exe` / `<path>.com` beside it (pnpm 11+'s global install: `pnpm.CMD` runs `...\node_modules\pnpm\pnpm`, which cmd
 *     completes to the native `pnpm.exe`). A `.js`/`.cjs`/`.mjs` target counts with the node that runs it (this process's).
 *   - a `pnpm.exe` / `pnpm.com` found directly.
 * Anything else (corepack, a version manager, a shim with several commands or a variable) is not named. Every file is hashed in full.
 * Exported for its test; `platform`, `exists`, `readFile` and `execPath` are injectable.
 */
export function windowsPnpmIdentity(env, { exists = (f) => { try { return fs.statSync(f).isFile(); } catch { return false; } }, readFile = (f) => fs.readFileSync(f), execPath = process.execPath } = {}) {
  const w = path.win32;
  const pathKey = Object.keys(env ?? {}).find((k) => k.toLowerCase() === 'path');
  const exts = String(env?.PATHEXT || '.COM;.EXE;.BAT;.CMD').split(';').filter(Boolean).map((e) => e.toLowerCase());
  let found = null;
  for (const dir of String(env?.[pathKey] ?? '').split(';').filter(Boolean)) {
    if (!w.isAbsolute(dir)) continue;
    for (const ext of exts) { const f = w.join(dir, `pnpm${ext}`); if (exists(f)) { found = f; break; } }
    if (found) break;
  }
  if (!found) return null;
  const named = (f) => [f, hash(readFile(f))];
  if (/\.(exe|com)$/i.test(found)) return [named(found)];
  if (!/\.(cmd|bat)$/i.test(found)) return null;
  const text = readFile(found).toString('utf8');
  const runs = text.split(/\r?\n/).filter((l) => l.includes('%*'));
  if (runs.length !== 1) return null;
  const m = /^\s*@?"%~dp0\\?([^"%]+)"\s+%\*\s*$/.exec(runs[0]);
  if (!m) return null;
  const target = w.resolve(w.dirname(found), m[1]);
  if (/\.(m?js|cjs)$/i.test(target)) return exists(target) ? [named(found), named(target), named(execPath)] : null;
  if (w.extname(target)) return null;
  for (const ext of ['.exe', '.com']) {
    if (!exists(target + ext)) continue;
    const bytes = readFile(target + ext);
    return bytes.subarray(0, 2).toString('latin1') === 'MZ' ? [named(found), [target + ext, hash(bytes)]] : null;
  }
  return null;
}

function pnpmIdentity(env) {
  if (process.platform === 'win32') return windowsPnpmIdentity(env);
  for (const dir of String(env.PATH ?? '').split(path.delimiter)) {
    if (!dir || !path.isAbsolute(dir)) continue;
    const p = path.join(dir, 'pnpm');
    let data, real;
    try { real = fs.realpathSync(p); fs.accessSync(real, fs.constants.X_OK); data = fs.readFileSync(real); } catch { continue; }
    const components = [[real, hash(data)]];
    if (data.subarray(0, 2).toString() === '#!') {
      // Recognize only the provisioned pnpm shim's fixed exec target. All
      // other launchers fall back to install, including version managers.
      const text = data.toString('utf8');
      const matches = [...text.matchAll(/^exec "\$basedir\/([^"\n]+)"\s+"\$@"\s*$/gm)];
      if (matches.length !== 1) return null;
      const target = fs.realpathSync(path.resolve(path.dirname(real), matches[0][1]));
      const bytes = fs.readFileSync(target);
      if (bytes.subarray(0, 2).toString() === '#!') return null;
      components.push([target, hash(bytes)]);
    }
    return components;
  }
  return null;
}

function packageIdentity() {
  const root = path.dirname(fileURLToPath(import.meta.url)), rows = [];
  const walk = (dir) => { for (const name of fs.readdirSync(dir).sort()) { if (name === 'node_modules' || name.startsWith('.')) continue; const p = path.join(dir, name), st = fs.lstatSync(p); if (st.isDirectory()) walk(p); else if (st.isFile()) rows.push([path.relative(root, p), st.mode, hash(fs.readFileSync(p))]); else throw new Error('unsupported gate entry'); } };
  walk(root); return hash(JSON.stringify(rows));
}

// Read config in memory only: neither config values nor credentials are stored
// or logged. Unknown config locations or unreadable files disable reuse.
export function canonicalConfigFingerprint(effective, files) {
  const canonical = (v) => Array.isArray(v) ? v.map(canonical) : v && typeof v === 'object' ? Object.fromEntries(Object.keys(v).sort().map((key) => [key, canonical(v[key])])) : v;
  return hash(JSON.stringify({ effective: canonical(effective), files }));
}

export function pnpmConfigIdentity({ dir, env, spawn }) {
  const fail = (code) => { const e = new Error(code); e.reuseCode = code; throw e; };
  const ask = (args) => {
    const r = spawn('pnpm', ['config', ...args], { cwd: dir, env, maxBuffer: 1 << 24 });
    if (r?.status !== 0 || r?.error || r?.signal) fail(args[0] === 'list' ? 'config-list-failed' : 'config-path-query-failed');
    return String(r.stdout ?? '').trim();
  };
  const effective = ask(['list', '--json']);
  if (!effective) fail('config-list-empty');
  let parsed;
  try { parsed = JSON.parse(effective); } catch { fail('config-json-invalid'); }
  const user = ask(['get', 'userconfig']);
  const global = ask(['get', 'globalconfig']);
  // pnpm's default userconfig is ~/.npmrc, home being USERPROFILE on Windows (os.homedir): the Dell gate runs with no HOME (NN12, 2026-11-18: config-path-unknown).
  const home = process.platform === 'win32' ? (env.USERPROFILE ?? env.HOME) : env.HOME;
  const userPath = user && user !== 'undefined' && user !== 'null' ? user : (home ? path.join(home, '.npmrc') : null);
  if (!userPath || !path.isAbsolute(userPath) || !global || global === 'undefined' || global === 'null' || !path.isAbsolute(global)) fail('config-path-unknown');
  const file = (p) => {
    try { return [p, fs.realpathSync(p), hash(fs.readFileSync(p))]; }
    catch (e) { if (e.code === 'ENOENT') return [p, 'absent']; fail('config-file-unreadable'); }
  };
  return canonicalConfigFingerprint(parsed, [file(userPath), file(global)]);
}

export function preparationInputComponents({ source, toolchain, config, store, env }) {
  // The environment is keyed on the build-relevant variables only, by the same rule as every scan row's inherited environment (deterministic-scans.mjs
  // inheritedEnvShape: npm/pnpm config, NODE_OPTIONS, CI, RK_/RIGHTKIT_/RUSTRIGHT_ switches, ...), less the gate's per-run bookkeeping (RK_GATE_*, the
  // ownership handle, nonces). NN12, Dell 2026-11-18: the whole environment was hashed, and every gate launch carries its own RK_GATE_SPAWN_LEDGER and
  // contract step, so no run's key ever matched the last one. pnpm's config files and `config list`, its executable, node, the store and the source are
  // their own components; the installed bytes are checked against the record before any reuse (dependencyProof).
  const environment = Object.entries(inheritedEnvShape(env ?? {}, null));
  return Object.fromEntries(Object.entries({ source, toolchain, config, store, environment }).map(([name, value]) => [name, hash(JSON.stringify(value))]));
}

export function snapshotDependencyReuse({ checkout, snapshot, tree, root, dirs, filters, storeDir, version, env, spawn, diagnostic = {} }) {
  let stage = 'pnpm-identity';
  try {
    const executable = pnpmIdentity(env);
    if (!executable) { diagnostic.setup = 'pnpm-unresolved'; return null; }
    stage = 'git-common';
    const git = spawn('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: checkout, env });
    if (git?.status !== 0) { diagnostic.setup = 'git-common-failed'; return null; }
    const common = String(git.stdout ?? '').trim();
    if (!common) { diagnostic.setup = 'git-common-empty'; return null; }
    const cacheDir = path.join(path.resolve(checkout, common), 'rightkit-gate-dependency-preparation');
    const recordPath = path.join(cacheDir, `${hash(`${fs.realpathSync(snapshot)}\0${root}`)}.json`);
    // Hash values in memory; only the final digest is persisted, never env.
    stage = 'config';
    const config = pnpmConfigIdentity({ dir: path.resolve(snapshot, root), env, spawn });
    stage = 'input-identity';
    const components = preparationInputComponents({ source: { checkout: fs.realpathSync(checkout), snapshot: fs.realpathSync(snapshot), tree, root, dirs: [...dirs].sort(), filters }, toolchain: { pnpm: executable, version, node: [fs.realpathSync(process.execPath), hash(fs.readFileSync(process.execPath)), process.version], platform: [process.platform, process.arch], gate: packageIdentity() }, config, store: fs.realpathSync(storeDir), env });
    const inputs = hash(JSON.stringify(components));
    const diagnosticDir = env.RIGHTKIT_PREPARATION_DIAGNOSTIC_DIR;
    let digestChecks = 0;
    diagnostic.setup = 'ready';
    return {
      get digestChecks() { return digestChecks; },
      matches() {
        let phase = 'config';
        try {
          if (pnpmConfigIdentity({ dir: path.resolve(snapshot, root), env, spawn }) !== config) { diagnostic.match = 'config-changed'; return false; }
          phase = 'record';
          const record = JSON.parse(fs.readFileSync(recordPath, 'utf8'));
          if (record.schema !== 5 || !record.components || typeof record.components !== 'object' || typeof record.dependencies !== 'string') { diagnostic.match = 'record-invalid'; return false; }
          const changed = Object.keys(components).filter((key) => record.components[key] !== components[key]);
          if (changed.length || record.inputs !== inputs) { diagnostic.match = 'record-mismatch'; diagnostic.components = changed.length ? changed.join(',') : 'aggregate'; if (diagnosticDir) { try { writeDiagnostic(diagnosticDir, 'lookup', root, inputs, components, captureDependencyManifest(snapshot, dirs)); } catch {} } return false; }
          phase = 'digest'; digestChecks++;
          const { digest, capture } = dependencyProof(snapshot, dirs, diagnosticDir);
          const ok = record.dependencies === digest;
          if (capture) writeDiagnostic(diagnosticDir, 'lookup', root, inputs, components, capture);
          diagnostic.match = ok ? 'hit' : 'dependency-mismatch'; return ok;
        } catch (e) { diagnostic.match = safeError(e, phase === 'record' && e.code === 'ENOENT' ? 'record-missing' : `${phase}-error`); return false; }
      },
      save() {
        let phase = 'config';
        try {
          if (pnpmConfigIdentity({ dir: path.resolve(snapshot, root), env, spawn }) !== config) { diagnostic.save = 'config-changed'; return; }
          phase = 'digest';
          digestChecks++;
          const { digest, capture } = dependencyProof(snapshot, dirs, diagnosticDir);
          const record = JSON.stringify({ schema: 5, inputs, components, dependencies: digest });
          phase = 'write';
          fs.mkdirSync(cacheDir, { recursive: true });
          const temp = `${recordPath}.${process.pid}.${crypto.randomBytes(8).toString('hex')}.tmp`;
          fs.writeFileSync(temp, record, { flag: 'wx' }); fs.renameSync(temp, recordPath);
          if (capture) writeDiagnostic(diagnosticDir, 'save', root, inputs, components, capture);
          diagnostic.save = 'saved';
        } catch (e) { diagnostic.save = safeError(e, `${phase}-error`); }
      },
    };
  } catch (e) { diagnostic.setup = safeError(e, `${stage}-error`); return null; }
}
