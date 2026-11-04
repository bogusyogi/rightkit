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

export function createNodeCheckCacheDigest({ snapshot, tree, files, gateIdentity, gatePackage = path.dirname(fileURLToPath(import.meta.url)), nodePath }) {
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
    return (scan, { childEnv, cwd, command }) => {
      try {
        const file = typeof scan.id === 'string' && scan.id.startsWith('node-check:') ? scan.id.slice(11) : null;
        if (!file || !committed.has(file) || scan.class !== 'A' || scan.tool !== 'node' || scan.root !== 'content' || scan.cwd || scan.executes || scan.internal || scan.runOutside || scan.deferred || scan.requires?.length || Object.keys(scan.env ?? {}).length || !Array.isArray(scan.args) || scan.args.length !== 2 || scan.args[0] !== '--check' || scan.args[1] !== file) return null;
        if (fs.realpathSync(cwd) !== root || fs.realpathSync(command) !== executable) return null;
        const target = path.resolve(root, file);
        if (!target.startsWith(root + path.sep) || fs.realpathSync(target) !== target || !fs.lstatSync(target).isFile()) return null;
        const rows = Object.entries(childEnv).filter(([k, v]) => v !== undefined && v !== null && k !== 'RIGHTKIT_BUILD_OWNERSHIP').sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0);
        // Options, preloads, and dynamic-loader overrides can read unbounded
        // external files, so no cache key is issued when any are present.
        if (rows.some(([k, v]) => /^(NODE_|LD_|DYLD_|OPENSSL_CONF(?:_INCLUDE)?$|OPENSSL_MODULES$|SSL_CERT_(?:FILE|DIR)$|ICU_DATA$)/i.test(k) && String(v) !== '')) return null;
        return nodeCheckFingerprint({ schema: 'rightkit.node-check-cache/1', tree, root, file, source: hash(fs.readFileSync(target)), gateIdentity, gatePackageHash, helperHash, executable, nodeHash, nodeVersion: process.version, platform: process.platform, arch: process.arch, ancestors, argv: scan.args, id: scan.id, cwd: root, environment: rows });
      } catch { return null; }
    };
  } catch { return () => null; }
}
