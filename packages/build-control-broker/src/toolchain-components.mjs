import { execFile as nodeExecFile } from 'node:child_process';
import { promisify } from 'node:util';

const execFile = promisify(nodeExecFile);

export class TargetStdUnavailableError extends Error {
  constructor({ target, toolchain }) {
    super(`managed Rust target std is unavailable: ${target} (${toolchain})`);
    this.name = 'TargetStdUnavailableError'; this.code = 'RK_TARGET_STD_UNAVAILABLE'; this.target = target; this.toolchain = toolchain;
  }
}

export function rustupToolchainSelector(pinnedToolchain) {
  if (typeof pinnedToolchain !== 'string' || !pinnedToolchain) throw new TypeError('pinned broker toolchain is required');
  const versioned = /^stable-(\d+\.\d+\.\d+-.+)$/.exec(pinnedToolchain);
  return versioned?.[1] ?? pinnedToolchain;
}

export async function inspectManagedTargetStd({ rustupPath, rustupHome, pinnedToolchain, target, run = execFile } = {}) {
  if (![rustupPath, rustupHome, pinnedToolchain, target].every((value) => typeof value === 'string' && value)) throw new TypeError('rustup path, managed Rustup home, pinned toolchain, & target are required');
  const toolchain = rustupToolchainSelector(pinnedToolchain);
  const env = { ...process.env, RUSTUP_HOME: rustupHome };
  let compiler;
  try {
    const result = await run(rustupPath, ['run', toolchain, 'rustc', '-vV'], { env, encoding: 'utf8' });
    compiler = String(result.stdout ?? result).trim();
  } catch (cause) {
    throw Object.assign(new Error(`managed pinned compiler identity is unavailable: ${toolchain}`), { code: 'RK_TOOLCHAIN_IDENTITY_UNAVAILABLE', cause });
  }
  const release = /^release:\s*(.+)$/m.exec(compiler)?.[1] ?? null;
  const host = /^host:\s*(.+)$/m.exec(compiler)?.[1] ?? null;
  const expected = /^(\d+\.\d+\.\d+)-(.+)$/.exec(toolchain);
  if (expected && (release !== expected[1] || host !== expected[2])) throw Object.assign(new Error(`managed pinned compiler identity mismatch: ${toolchain}`), { code: 'RK_TOOLCHAIN_IDENTITY_MISMATCH' });
  let installed;
  try {
    const result = await run(rustupPath, ['target', 'list', '--installed', '--toolchain', toolchain], { env, encoding: 'utf8' });
    installed = String(result.stdout ?? result).split(/\r?\n/).map((value) => value.trim()).filter(Boolean);
  } catch (cause) {
    throw Object.assign(new Error(`managed target inventory is unavailable: ${toolchain}`), { code: 'RK_TARGET_INVENTORY_UNAVAILABLE', cause });
  }
  return Object.freeze({ target, pinnedToolchain, toolchain, rustupHome, compiler: Object.freeze({ release, host }), available: installed.includes(target) });
}

export function requireManagedTargetStd(evidence) {
  if (!evidence?.available) throw new TargetStdUnavailableError({ target: evidence?.target, toolchain: evidence?.toolchain ?? evidence?.pinnedToolchain });
  return evidence;
}
