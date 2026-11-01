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

// Linux (WSL2, VMs, standalone hosts, cloud containers) runs direct with no broker (docs/GATE-CONTRACT.md section 1), so there is
// no host pin. The entry is the checkout's own `node_modules/@rightkit/release/bin/rightkit-gate.mjs`, accepted only when it sits
// inside the git checkout, resolves inside an `@rightkit/release` package, and that package's version is the one the checkout's
// committed pnpm-lock.yaml pins. Anything else refuses: the gate never falls back to a package the lockfile does not name.
export function linuxGate({ cwd = process.cwd(), read = readFileSync, realpath = realpathSync, gitTop = defaultGitTop } = {}) {
  const top = gitTop(cwd);
  if (!top) throw new Error('not inside a git checkout; run the gate from the repository');
  const entry = posix.join(top, 'node_modules', '@rightkit', 'release', 'bin', 'rightkit-gate.mjs');
  let target;
  try { target = realpath(entry); } catch { throw new Error(`no ${entry}; install the checkout's dependencies (pnpm install) first`); }
  const marker = '/@rightkit/release/bin/rightkit-gate.mjs';
  if (!target.endsWith(marker)) throw new Error('gate entry resolves outside an @rightkit/release package');
  const packageRoot = target.slice(0, target.length - '/bin/rightkit-gate.mjs'.length);
  const installed = JSON.parse(read(posix.join(packageRoot, 'package.json'), 'utf8')).version;
  const lock = String(read(posix.join(top, 'pnpm-lock.yaml'), 'utf8'));
  const pinned = /^\s+'?@rightkit\/release'?:\n\s+specifier: [^\n]+\n\s+version: ([^\s(]+)/m.exec(lock)?.[1];
  if (!pinned) throw new Error('the checkout\'s pnpm-lock.yaml does not pin @rightkit/release');
  if (installed !== pinned) throw new Error(`installed @rightkit/release ${installed} is not the locked ${pinned}; run pnpm install`);
  return target;
}

function defaultGitTop(cwd) {
  const r = spawnSync('git', ['rev-parse', '--show-toplevel'], { cwd, encoding: 'utf8' });
  return r.status === 0 ? r.stdout.trim() : null;
}

export function trustedGate({ platform = process.platform, home = homedir(), read = readFileSync, realpath = realpathSync, treeHash = packageDigest, cwd, gitTop } = {}) {
  if (platform === 'linux') return linuxGate({ cwd, read, realpath, ...(gitTop ? { gitTop } : {}) });
  const config = platform === 'darwin' ? join(home, 'Library', 'Application Support', 'RightKit', 'build-control', 'config.json')
    : platform === 'win32' ? 'D:\\.rightkit-managed\\config.json' : null;
  if (!config) throw new Error('host gate is available on managed Mac and Windows hosts and on Linux only');
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
