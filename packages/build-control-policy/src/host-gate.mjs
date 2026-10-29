#!/usr/bin/env node
// Managed-host entry with a distinct name so pnpm's local .bin cannot shadow it.
import { createHash } from 'node:crypto';
import { lstatSync, readFileSync, readlinkSync, readdirSync, realpathSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, posix, win32 } from 'node:path';
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

// Matches RustRight's tree_digest: paths sorted by UTF-8 bytes, files by digest and execute bit, links by target.
export function packageDigest(root, platform = process.platform) {
  const lines = [];
  const walk = (dir, prefix = '') => {
    for (const name of readdirSync(dir)) {
      const full = join(dir, name);
      const relative = prefix ? `${prefix}/${name}` : name;
      const meta = lstatSync(full);
      if (meta.isSymbolicLink()) lines.push(`${relative}\tlink\t-\t${readlinkSync(full)}`);
      else if (meta.isDirectory()) walk(full, relative);
      else if (meta.isFile()) lines.push(`${relative}\tfile\t${platform !== 'win32' && (meta.mode & 0o111) ? 'x' : '-'}\t${createHash('sha256').update(readFileSync(full)).digest('hex')}`);
      else throw new Error(`unsupported package entry: ${relative}`);
    }
  };
  walk(realpathSync(root));
  lines.sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)));
  return createHash('sha256').update(lines.map((line) => `${line}\n`).join('')).digest('hex');
}

export function trustedGate({ platform = process.platform, home = homedir(), read = readFileSync, realpath = realpathSync, treeHash = packageDigest } = {}) {
  const config = platform === 'darwin' ? join(home, 'Library', 'Application Support', 'RightKit', 'build-control', 'config.json')
    : platform === 'win32' ? 'D:\\.rightkit-managed\\config.json' : null;
  if (!config) throw new Error('host gate is available on managed Mac and Windows hosts only');
  // The broker's current schema pins `admission.trustedEntry { path, sha256 }`; a host the new broker has not reached yet still
  // pins `buildRules.checkAdmission.trustedGate { path, sha256, treeSha256 }`. The package digest is checked when the pin names one.
  const parsed = JSON.parse(read(config, 'utf8'));
  const pin = parsed?.admission?.trustedEntry ?? parsed?.buildRules?.checkAdmission?.trustedGate;
  if (typeof pin?.path !== 'string' || !(platform === 'win32' ? win32 : posix).isAbsolute(pin.path) || !/^[a-f0-9]{64}$/.test(pin.sha256 ?? '') || (pin.treeSha256 !== undefined && !/^[a-f0-9]{64}$/.test(pin.treeSha256))) throw new Error('trusted gate pin is missing or invalid');
  const target = realpath(pin.path);
  const digest = createHash('sha256').update(read(target)).digest('hex');
  if (digest !== pin.sha256) throw new Error('trusted gate entry digest does not match host pin');
  const normalized = pin.path.replaceAll('\\', '/');
  const marker = '/node_modules/@rightkit/release/';
  const end = normalized.lastIndexOf(marker);
  if (end < 0) throw new Error('trusted gate is outside its pinned package');
  const packageRoot = normalized.slice(0, end + marker.length - 1);
  if (pin.treeSha256 !== undefined && treeHash(packageRoot, platform) !== pin.treeSha256) throw new Error('trusted gate package digest does not match host pin');
  return target;
}

export function main(args = process.argv.slice(2)) {
  try {
    const gate = trustedGate();
    const child = spawnSync(process.execPath, [gate, ...args], { stdio: 'inherit', env: process.env, windowsHide: true });
    if (child.error) throw child.error;
    return child.status ?? 1;
  } catch (error) {
    console.error(`rightkit-host-gate: ${error.message}`);
    return 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) process.exit(main());
