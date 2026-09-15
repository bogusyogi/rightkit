import { readFileSync, realpathSync, statSync, existsSync } from 'node:fs';
import { join, relative, resolve, isAbsolute } from 'node:path';

export const LOCAL_DEVELOPMENT_FILE = '.rightkit-local-development.json';
const allowedRepositories = new Set(['Orthic-Labs/Membrane', 'Orthic-Labs/legion']);
const expected = { schemaVersion: 1, platform: 'win32', purpose: 'unsigned-installer' };

export function localDevelopmentPolicy(root, platform = process.platform) {
  if (!root || platform !== 'win32') return null;
  try {
    const policy = JSON.parse(readFileSync(join(root, LOCAL_DEVELOPMENT_FILE), 'utf8'));
    if (Object.keys(policy).length !== Object.keys(expected).length + 1
      || !allowedRepositories.has(policy.repository)
      || Object.entries(expected).some(([key, value]) => policy[key] !== value)) return null;
    const gitEntry = join(root, '.git');
    let gitDir = gitEntry;
    if (!statSync(gitEntry).isDirectory()) {
      const pointer = /^gitdir:\s*(.+)\s*$/m.exec(readFileSync(gitEntry, 'utf8'));
      if (!pointer) return null;
      gitDir = resolve(root, pointer[1].trim());
    }
    if (existsSync(join(gitDir, 'commondir'))) gitDir = resolve(gitDir, readFileSync(join(gitDir, 'commondir'), 'utf8').trim());
    const config = readFileSync(join(gitDir, 'config'), 'utf8');
    let origin = false;
    for (const raw of config.split(/\r?\n/)) {
      const line = raw.trim();
      if (line.startsWith('[')) { origin = /^\[remote\s+"origin"\]$/i.test(line); continue; }
      const remote = /^url\s*=\s*(?:https:\/\/github\.com\/|git@github\.com:)([^\s]+?)?(?:\.git)?\/?$/i.exec(line);
      if (origin && remote?.[1]?.toLowerCase() === policy.repository.toLowerCase()) return Object.freeze(policy);
    }
  } catch { /* Absent, malformed, or foreign configuration grants nothing. */ }
  return null;
}

export function localDevelopmentCargoAllowed({ root, workspace, operation, argv = [], nested = false, platform = process.platform }) {
  const policy = localDevelopmentPolicy(root, platform);
  if (!policy || !Array.isArray(argv)) return false;
  const targets = argv.flatMap((arg, index) => arg === '--target' ? [argv[index + 1]] : arg.startsWith('--target=') ? [arg.slice(9)] : []);
  if (targets.some((target) => target !== 'x86_64-pc-windows-msvc')) return false;
  // Tauri discovers installed targets before it submits the managed build.
  if (operation === 'rustup') return argv.length === 2 && argv[0] === 'target' && argv[1] === 'list';
  if (['rustc', 'rustdoc'].includes(operation)) {
    return nested || argv.every((arg) => ['-vV', '-V', '--version', '--verbose'].includes(arg)) && argv.length > 0;
  }
  if (operation !== 'cargo') return false;
  if (argv.length === 1 && ['-V', '--version'].includes(argv[0])) return true;
  const args = argv[0] === '--' ? argv.slice(1) : argv;
  const command = args[0];
  const dependencyMaintenance = ['outdated', 'upgrade'].includes(command);
  if (!['check', 'build', 'test', 'metadata', 'outdated', 'upgrade'].includes(command)) return false;
  if (!dependencyMaintenance && command !== 'metadata' && targets.length !== 1) return false;
  const manifestIndex = args.indexOf('--manifest-path');
  const manifestArg = manifestIndex >= 0 ? args[manifestIndex + 1] : args.find((arg) => arg.startsWith('--manifest-path='))?.slice(16);
  try {
    const manifest = realpathSync(resolve(workspace, manifestArg ?? 'Cargo.toml'));
    const path = relative(realpathSync(root), manifest).replaceAll('\\', '/');
    const manifests = policy.repository === 'Orthic-Labs/legion' ? ['engine/Cargo.toml'] : ['engine/Cargo.toml', 'apps/membrane-tray-windows/Cargo.toml', 'apps/membrane-hub/src-tauri/Cargo.toml'];
    if (isAbsolute(path) || !manifests.includes(path)) return false;
  } catch { return false; }
  return true;
}
