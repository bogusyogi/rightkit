#!/usr/bin/env node
// The host's pinned gate (`rightkit-host-gate`): the one RightKit gate the RustRight broker trusts on a managed host (Mac, Dell).
// It reads the broker's host config, verifies the pinned entry's sha256 and the whole pinned package tree (treeSha256) exactly as the
// broker does (`buildRules.checkAdmission.trustedGate`), and runs that gate with the caller's arguments. Every app's own copy of
// rightkit-gate hands its run to this pin on a managed host (bin/rightkit-gate.mjs deferToHostGate), so a build script, a hook, a
// person or an agent reaches the same gate whatever copy they typed.
//
// Managed hosts are macOS and Windows, or any host that names its control root in RIGHTKIT_CONTROL_ROOT (how the tests stand one up).
// Self-contained on purpose: it imports nothing of this package, so packages/build-control-policy/src/host-gate.mjs is this file byte
// for byte (host-gate.test.mjs holds the two equal) and either copy can be put on a host's PATH by `rightkit-gate install-host-gate`.
import { createHash } from 'node:crypto';
import { lstatSync, readFileSync, readlinkSync, readdirSync, realpathSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, posix, win32 } from 'node:path';
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

export const PIN_KEY = 'buildRules.checkAdmission.trustedGate';
export const UNMANAGED = 'host gate is available on managed Mac and Windows hosts only (or a host naming RIGHTKIT_CONTROL_ROOT)';

/** The broker's host config, or null on a host that is not broker-managed. RIGHTKIT_CONTROL_ROOT names the control root explicitly. */
export function hostConfigPath({ platform = process.platform, home = homedir(), env = process.env } = {}) {
  if (typeof env.RIGHTKIT_CONTROL_ROOT === 'string' && env.RIGHTKIT_CONTROL_ROOT) return join(env.RIGHTKIT_CONTROL_ROOT, 'config.json');
  if (platform === 'darwin') return join(home, 'Library', 'Application Support', 'RightKit', 'build-control', 'config.json');
  if (platform === 'win32') return 'D:\\.rightkit-managed\\config.json';
  return null;
}

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

/** The pinned package's root (`.../node_modules/@rightkit/release`) for a pinned entry path, or null when the entry is outside one. */
export function pinnedPackageRoot(pinPath) {
  const normalized = String(pinPath).replaceAll('\\', '/');
  const marker = '/node_modules/@rightkit/release/';
  const end = normalized.lastIndexOf(marker);
  return end < 0 ? null : normalized.slice(0, end + marker.length - 1);
}

/**
 * The verified pinned gate: the entry path the host config pins, after its sha256 and its package's treeSha256 both match. Throws
 * when the host is not managed, the pin is missing or malformed, or either digest differs: a pin that does not verify admits nothing.
 */
export function trustedGate({ platform = process.platform, home = homedir(), env = process.env, read = readFileSync, realpath = realpathSync, treeHash = packageDigest } = {}) {
  const config = hostConfigPath({ platform, home, env });
  if (!config) throw new Error(UNMANAGED);
  const pin = JSON.parse(read(config, 'utf8'))?.buildRules?.checkAdmission?.trustedGate;
  if (typeof pin?.path !== 'string' || !(platform === 'win32' ? win32 : posix).isAbsolute(pin.path) || !/^[a-f0-9]{64}$/.test(pin.sha256 ?? '') || !/^[a-f0-9]{64}$/.test(pin.treeSha256 ?? '')) throw new Error('trusted gate pin is missing or invalid');
  const target = realpath(pin.path);
  const digest = createHash('sha256').update(read(target)).digest('hex');
  if (digest !== pin.sha256) throw new Error('trusted gate entry digest does not match host pin');
  const packageRoot = pinnedPackageRoot(pin.path);
  if (!packageRoot) throw new Error('trusted gate is outside its pinned package');
  if (treeHash(packageRoot, platform) !== pin.treeSha256) throw new Error('trusted gate package digest does not match host pin');
  return target;
}

/**
 * What this host pins, for a caller that must decide whether to run itself or hand over: `{ status: "unmanaged" }` (no broker here),
 * `{ status: "none" }` (a managed host whose config holds no pin: the broker judges the repository's own gate, untrusted),
 * `{ status: "pinned", gate }` (the verified pin). A pin that is present but does not verify throws, with the reason.
 */
export function hostPin(options = {}) {
  const { platform = process.platform, home = homedir(), env = process.env, read = readFileSync } = options;
  const config = hostConfigPath({ platform, home, env });
  if (!config) return { status: 'unmanaged' };
  let text;
  try { text = read(config, 'utf8'); } catch { return { status: 'none' }; }
  let pin;
  try { pin = JSON.parse(text)?.buildRules?.checkAdmission?.trustedGate; } catch (error) { throw new Error(`the host config ${config} is not JSON (${error.message})`); }
  if (pin === undefined || pin === null) return { status: 'none' };
  return { status: 'pinned', gate: trustedGate({ ...options, platform, home, env, read }) };
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
