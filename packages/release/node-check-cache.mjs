// Narrow reuse identity for plain Node syntax checks. Admission still binds the
// complete raw Class A environment; the host's runtime libraries remain under
// the same trusted-host assumption as other locally executed tools.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';

const hash = (data) => crypto.createHash('sha256').update(data).digest('hex');

// Pure data transformation: no values are persisted, only the resulting hash.
export function nodeCheckFingerprint(input) {
  return hash(JSON.stringify(input));
}

export function writeNodeCheckDiagnostic(dir, record) {
  let temp;
  try {
    const hex = (s) => typeof s === 'string' && /^[0-9a-f]{64}$/.test(s);
    const scalar = ['key', 'tree', 'source', 'gate', 'gatePackage', 'helper', 'node', 'executable', 'runtime', 'cwd', 'scan', 'normalizedEnvironment'];
    const names = ['schema', ...scalar, 'ancestors', 'environment'];
    if (!record || Object.keys(record).length !== names.length || Object.keys(record).some((k) => !names.includes(k)) || record.schema !== 'rightkit.node-check-components/1' || !scalar.every((k) => hex(record[k]))) return;
    const pairs = (rows, nullable) => Array.isArray(rows) && rows.length <= 4096 && rows.every((r) => Array.isArray(r) && r.length === 2 && hex(r[0]) && (hex(r[1]) || (nullable && r[1] === null)));
    if (!pairs(record.ancestors, true) || !pairs(record.environment, false) || !path.isAbsolute(dir)) return;
    const scratch = fs.realpathSync(process.platform === 'win32' ? 'D:\\scratch' : '/Volumes/D/scratch');
    const target = fs.realpathSync(dir);
    if (!(target === scratch || target.startsWith(scratch + path.sep)) || !fs.statSync(target).isDirectory()) return;
    const file = path.join(target, `node-check-${process.pid}-${crypto.randomBytes(8).toString('hex')}.json`);
    temp = `${file}.tmp`;
    fs.writeFileSync(temp, JSON.stringify(record), { flag: 'wx' });
    fs.renameSync(temp, file);
  } catch { if (temp) { try { fs.unlinkSync(temp); } catch {} } }
}

export function createNodeCheckCacheDigest({ snapshot, tree, files, gateIdentity, gatePackage = path.dirname(fileURLToPath(import.meta.url)), nodePath, onReject = () => {}, onDiagnostic = null, diagnosticScanId = null }) {
  const reject = (reason, id) => { try { onReject(reason, id); } catch {} return null; };
  let captured = false;
  try {
    const root = fs.realpathSync(snapshot);
    const executable = fs.realpathSync(nodePath);
    const nodeHash = hash(fs.readFileSync(executable));
    const helperHash = hash(fs.readFileSync(fileURLToPath(import.meta.url)));
    const walkGate = (dir) => fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => (e.name === 'node_modules' || e.name.startsWith('.') ? [] : e.isDirectory() ? walkGate(path.join(dir, e.name)) : [path.join(dir, e.name)]));
    const gateFiles = walkGate(gatePackage).sort().map((f) => [path.relative(gatePackage, f), hash(fs.readFileSync(f))]);
    const gatePackageHash = nodeCheckFingerprint(gateFiles);
    const committed = new Set(files);
    const ancestors = [];
    for (let dir = path.dirname(root);;) {
      const file = path.join(dir, 'package.json');
      try { ancestors.push([file, hash(fs.readFileSync(file))]); }
      catch (e) { if (e.code !== 'ENOENT' && e.code !== 'ENOTDIR') throw e; ancestors.push([file, null]); }
      const parent = path.dirname(dir); if (parent === dir) break; dir = parent;
    }
    return (scan, { childEnv, cwd, command, shimProof = null }) => {
      try {
        const file = typeof scan.id === 'string' && scan.id.startsWith('node-check:') ? scan.id.slice(11) : null;
        if (!file || !committed.has(file) || scan.class !== 'A' || scan.tool !== 'node' || scan.root !== 'content' || scan.cwd || scan.executes || scan.internal || scan.runOutside || scan.deferred || scan.requires?.length || Object.keys(scan.env ?? {}).some((name) => name !== 'RK_SNAPSHOT_TARGETS') || !Array.isArray(scan.args) || scan.args.length !== 2 || scan.args[0] !== '--check' || scan.args[1] !== file) return reject('row-shape', scan.id);
        if (fs.realpathSync(cwd) !== root || fs.realpathSync(command) !== executable) return reject('runtime-path', scan.id);
        const target = path.resolve(root, file);
        if (!target.startsWith(root + path.sep) || fs.realpathSync(target) !== target || !fs.lstatSync(target).isFile()) return reject('source-path', scan.id);
        const rows = Object.entries(childEnv).filter(([k, v]) => v !== undefined && v !== null && k !== 'RIGHTKIT_BUILD_OWNERSHIP').sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0);
        // Options, preloads, and dynamic-loader overrides can read unbounded
        // external files, so no cache key is issued when any are present.
        const active = (pattern) => rows.some(([k, v]) => pattern.test(k) && String(v) !== '');
        if (active(/^NODE_OPTIONS$/i)) return reject('node-options', scan.id);
        // This host integration variable configures the computer-use REPL,
        // not Node's CLI. Its value still participates in the fingerprint.
        if (rows.some(([k, v]) => /^NODE_/i.test(k) && k !== 'NODE_REPL_TRUSTED_BROWSER_CLIENT_SHA256S' && String(v) !== '')) return reject('node-environment', scan.id);
        if (active(/^(LD_|DYLD_)/i)) return reject('loader-environment', scan.id);
        if (active(/^(OPENSSL_CONF(?:_INCLUDE)?|OPENSSL_MODULES|SSL_CERT_(?:FILE|DIR)|ICU_DATA)$/i)) return reject('runtime-config-environment', scan.id);
        let environment = rows;
        let overridden = [];
        if (shimProof) {
          const { shim, bin, sentinel, originalEnv } = shimProof;
          if (!originalEnv || typeof originalEnv !== 'object' || !path.isAbsolute(shim) || !path.isAbsolute(bin) || path.dirname(shim) !== bin || !sentinel || sentinel !== path.join(path.dirname(bin), 'unbound')) return reject('shim-proof', scan.id);
          const pathKey = Object.keys(originalEnv).find((k) => k.toUpperCase() === 'PATH') ?? 'PATH';
          const originalPath = originalEnv[pathKey] ?? '';
          const expectedSentinel = path.join(sentinel, hash(scan.id).slice(0, 16));
          if (childEnv.CARGO !== shim || childEnv[pathKey] !== `${bin}${path.delimiter}${originalPath}` || childEnv.CARGO_TARGET_DIR !== expectedSentinel || childEnv.CARGO_BUILD_TARGET_DIR !== expectedSentinel) return reject('shim-proof', scan.id);
          environment = rows.map(([k, v]) => k === 'CARGO' ? [k, { generated: 'cargo-wrapper' }] : k === pathKey ? [k, { generated: 'wrapper-path', original: originalPath }] : (k === 'CARGO_TARGET_DIR' || k === 'CARGO_BUILD_TARGET_DIR') ? [k, { generated: 'scan-sentinel' }] : [k, v]);
          overridden = ['CARGO', 'CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR'].map((k) => [k, Object.hasOwn(originalEnv, k), originalEnv[k] ?? null]);
        }
        const source = hash(fs.readFileSync(target));
        const key = nodeCheckFingerprint({ schema: 'rightkit.node-check-cache/2', tree, root, file, source, gateIdentity, gatePackageHash, helperHash, executable, nodeHash, nodeVersion: process.version, platform: process.platform, arch: process.arch, ancestors, argv: scan.args, id: scan.id, cwd: root, environment, overridden });
        if (!captured && typeof onDiagnostic === 'function' && scan.id === diagnosticScanId) {
          captured = true;
          try { onDiagnostic({ schema: 'rightkit.node-check-components/1', key, tree: hash(String(tree)), source, gate: hash(String(gateIdentity)), gatePackage: gatePackageHash, helper: helperHash, node: nodeHash, executable: hash(executable), runtime: nodeCheckFingerprint([process.version, process.platform, process.arch]), cwd: hash(root), scan: nodeCheckFingerprint([scan.id, scan.args]), normalizedEnvironment: nodeCheckFingerprint([environment, overridden]), ancestors: ancestors.map(([name, value]) => [hash(name), value === null ? null : hash(value)]), environment: rows.map(([name, value]) => [hash(name), hash(String(value))]) }); } catch {}
        }
        return key;
      } catch { return reject('row-error', scan?.id); }
    };
  } catch { return (scan) => reject('setup-error', scan?.id); }
}
