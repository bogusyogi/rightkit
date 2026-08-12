import { readdirSync, statSync } from 'node:fs';
import { homedir, platform, arch, release } from 'node:os';
import { join } from 'node:path';
import { commandExists, run } from './process.mjs';

const commands = ['cargo', 'rustc', 'swift', 'node', 'pnpm', 'node-gyp', 'cmake', 'make', 'cc'];
function version(command) { if (!commandExists(command)) return null; const result = run(command, ['--version'], process.cwd()); return result.stdout.trim().split('\n')[0] || result.stderr.trim().split('\n')[0]; }
export function inventory() {
  const cargoBin = join(homedir(), '.cargo', 'bin');
  let cargoExtensions = [];
  try { cargoExtensions = readdirSync(cargoBin).filter((entry) => entry.startsWith('cargo-') && statSync(join(cargoBin, entry)).isFile()).sort(); } catch {}
  return { schemaVersion: '1.0.0', generatedAt: new Date().toISOString(), host: { platform: platform(), arch: arch(), release: release() }, tools: Object.fromEntries(commands.map((command) => [command, { available: commandExists(command), version: version(command) }])), cargoExtensions };
}
