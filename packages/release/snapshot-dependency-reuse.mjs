// A best-effort cache of successful snapshot dependency preparations. A miss
// always takes the normal offline install path. Records are same-user local
// state, like the gate's result cache; they are not signed attestations.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';

const hash = (bytes) => crypto.createHash('sha256').update(bytes).digest('hex');
const inside = (p, dir) => p === dir || p.startsWith(dir + path.sep);
const CONFIG_CODES = new Set(['config-list-failed', 'config-path-query-failed', 'config-list-empty', 'config-json-invalid', 'config-path-unknown', 'config-file-unreadable']);
const safeError = (error, fallback) => CONFIG_CODES.has(error?.reuseCode) ? error.reuseCode : fallback;

// Hash all files, symlink targets, directory names and modes, including pnpm
// state and tool caches. Follow linked targets once; never trust stat/mtime.
export function dependencyDigest(snapshot, dirs) {
  const h = crypto.createHash('sha256'), seen = new Set(), pending = [];
  let count = 0;
  const root = fs.realpathSync(snapshot);
  const walk = (p) => {
    if (++count > 400000) throw new Error('dependency fingerprint exceeds limit');
    const st = fs.lstatSync(p);
    h.update(`${p}\0${st.mode}\0`);
    if (st.isSymbolicLink()) {
      h.update(`link\0${fs.readlinkSync(p)}\0`);
      try { pending.push(fs.realpathSync(p)); } catch (e) { if (e.code !== 'ENOENT') throw e; h.update('dangling\0'); }
    } else if (st.isDirectory()) {
      h.update('dir\0');
      for (const name of fs.readdirSync(p).sort()) walk(path.join(p, name));
    } else if (st.isFile()) h.update(`file\0${hash(fs.readFileSync(p))}\0`);
    else throw new Error('unsupported dependency entry');
  };
  for (const dir of [...dirs].sort()) {
    h.update(`root\0${dir}\0`);
    try { walk(path.join(snapshot, dir)); } catch (e) { if (e.code !== 'ENOENT' && e.code !== 'ENOTDIR') throw e; h.update('absent\0'); }
  }
  while (pending.length) {
    const p = pending.shift();
    if (seen.has(p)) continue;
    seen.add(p);
    if (p === path.parse(p).root || inside(root, p)) throw new Error('dependency link resolves to snapshot ancestor');
    h.update('target\0'); walk(p);
  }
  return h.digest('hex');
}

function pnpmIdentity(env) {
  // Do not infer Windows cmd/corepack dispatch or arbitrary shell programs.
  if (process.platform === 'win32') return null;
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

function pnpmConfigIdentity({ dir, env, spawn }) {
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
  const userPath = user && user !== 'undefined' && user !== 'null' ? user : (env.HOME ? path.join(env.HOME, '.npmrc') : null);
  if (!userPath || !path.isAbsolute(userPath) || !global || global === 'undefined' || global === 'null' || !path.isAbsolute(global)) fail('config-path-unknown');
  const file = (p) => {
    try { return [p, fs.realpathSync(p), hash(fs.readFileSync(p))]; }
    catch (e) { if (e.code === 'ENOENT') return [p, 'absent']; fail('config-file-unreadable'); }
  };
  return canonicalConfigFingerprint(parsed, [file(userPath), file(global)]);
}

export function snapshotDependencyReuse({ checkout, snapshot, tree, root, dirs, filters, storeDir, version, env, spawn, diagnostic = {} }) {
  let stage = 'pnpm-identity';
  try {
    const executable = pnpmIdentity(env);
    if (!executable) { diagnostic.setup = 'pnpm-unresolved'; return null; }
    stage = 'git-common';
    const git = spawn('git', ['rev-parse', '--git-common-dir'], { cwd: checkout, env });
    if (git?.status !== 0) { diagnostic.setup = 'git-common-failed'; return null; }
    const common = String(git.stdout ?? '').trim();
    if (!common) { diagnostic.setup = 'git-common-empty'; return null; }
    const cacheDir = path.join(path.resolve(checkout, common), 'rightkit-gate-dependency-preparation');
    const recordPath = path.join(cacheDir, `${hash(`${fs.realpathSync(snapshot)}\0${root}`)}.json`);
    // Hash values in memory; only the final digest is persisted, never env.
    stage = 'config';
    const config = pnpmConfigIdentity({ dir: path.resolve(snapshot, root), env, spawn });
    // These gate-only controls select scan concurrency; the install ignores
    // scripts and pnpm's effective configuration is bound separately above.
    const schedule = new Set(['RK_GATE_JOBS', 'RK_GATE_TEST_JOBS', 'RK_GATE_CLASS_B_JOBS']);
    stage = 'input-identity';
    const inputs = hash(JSON.stringify({ schema: 3, checkout: fs.realpathSync(checkout), snapshot: fs.realpathSync(snapshot), tree, root, dirs: [...dirs].sort(), filters, store: fs.realpathSync(storeDir), pnpm: executable, version, node: [fs.realpathSync(process.execPath), hash(fs.readFileSync(process.execPath)), process.version], platform: [process.platform, process.arch], gate: packageIdentity(), config, env: Object.entries(env).filter(([name]) => !schedule.has(name)).sort(([a], [b]) => a.localeCompare(b)) }));
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
          if (record.schema !== 3 || record.inputs !== inputs || typeof record.dependencies !== 'string') { diagnostic.match = 'record-mismatch'; return false; }
          phase = 'digest'; digestChecks++;
          const ok = record.dependencies === dependencyDigest(snapshot, dirs);
          diagnostic.match = ok ? 'hit' : 'dependency-mismatch'; return ok;
        } catch (e) { diagnostic.match = safeError(e, phase === 'record' && e.code === 'ENOENT' ? 'record-missing' : `${phase}-error`); return false; }
      },
      save() {
        let phase = 'config';
        try {
          if (pnpmConfigIdentity({ dir: path.resolve(snapshot, root), env, spawn }) !== config) { diagnostic.save = 'config-changed'; return; }
          phase = 'digest';
          digestChecks++;
          const record = JSON.stringify({ schema: 3, inputs, dependencies: dependencyDigest(snapshot, dirs) });
          phase = 'write';
          fs.mkdirSync(cacheDir, { recursive: true });
          const temp = `${recordPath}.${process.pid}.${crypto.randomBytes(8).toString('hex')}.tmp`;
          fs.writeFileSync(temp, record, { flag: 'wx' }); fs.renameSync(temp, recordPath);
          diagnostic.save = 'saved';
        } catch (e) { diagnostic.save = safeError(e, `${phase}-error`); }
      },
    };
  } catch (e) { diagnostic.setup = safeError(e, `${stage}-error`); return null; }
}
