import { readFileSync, realpathSync } from 'node:fs';
import { join, relative, resolve, isAbsolute } from 'node:path';

export const LOCAL_DEVELOPMENT_FILE = '.rightkit-local-development.json';
const expected = { schemaVersion: 1, repository: 'Orthic-Labs/Membrane', platform: 'win32', purpose: 'unsigned-installer' };

export function localDevelopmentPolicy(root, platform = process.platform) {
  if (!root || platform !== 'win32') return null;
  try {
    const policy = JSON.parse(readFileSync(join(root, LOCAL_DEVELOPMENT_FILE), 'utf8'));
    if (Object.keys(policy).length !== Object.keys(expected).length
      || Object.entries(expected).some(([key, value]) => policy[key] !== value)) return null;
    const config = readFileSync(join(root, '.git/config'), 'utf8');
    let origin = false;
    for (const raw of config.split(/\r?\n/)) {
      const line = raw.trim();
      if (line.startsWith('[')) { origin = /^\[remote\s+"origin"\]$/i.test(line); continue; }
      if (origin && /^url\s*=\s*(?:https:\/\/github\.com\/|git@github\.com:)Orthic-Labs\/Membrane(?:\.git)?\/?$/i.test(line)) return Object.freeze(policy);
    }
  } catch { /* Absent, malformed, or foreign configuration grants nothing. */ }
  return null;
}

export function localDevelopmentCargoAllowed({ root, workspace, operation, argv = [], nested = false, platform = process.platform }) {
  if (!localDevelopmentPolicy(root, platform) || !Array.isArray(argv)) return false;
  const targets = argv.flatMap((arg, index) => arg === '--target' ? [argv[index + 1]] : arg.startsWith('--target=') ? [arg.slice(9)] : []);
  if (targets.some((target) => target !== 'x86_64-pc-windows-msvc')) return false;
  if (['rustc', 'rustdoc'].includes(operation)) {
    return nested || argv.every((arg) => ['-vV', '-V', '--version', '--verbose'].includes(arg)) && argv.length > 0;
  }
  if (operation !== 'cargo') return false;
  if (argv.length === 1 && ['-V', '--version'].includes(argv[0])) return true;
  const args = argv[0] === '--' ? argv.slice(1) : argv;
  const command = args[0];
  if (!['check', 'build', 'test', 'metadata'].includes(command)) return false;
  if (command !== 'metadata' && targets.length !== 1) return false;
  const manifestIndex = args.indexOf('--manifest-path');
  const manifestArg = manifestIndex >= 0 ? args[manifestIndex + 1] : args.find((arg) => arg.startsWith('--manifest-path='))?.slice(16);
  try {
    const manifest = realpathSync(resolve(workspace, manifestArg ?? 'Cargo.toml'));
    const path = relative(realpathSync(root), manifest).replaceAll('\\', '/');
    if (isAbsolute(path) || !['engine/Cargo.toml', 'apps/membrane-tray-windows/Cargo.toml', 'apps/membrane-hub/src-tauri/Cargo.toml'].includes(path)) return false;
  } catch { return false; }
  return true;
}
