import { spawnSync } from 'node:child_process';

export function commandExists(command) {
  const probe = spawnSync('/bin/sh', ['-c', 'command -v -- "$1"', 'rightkit-phase0', command], { encoding: 'utf8', timeout: 2_000 });
  return probe.status === 0;
}

export function run(command, args, cwd) {
  const startedAt = new Date().toISOString();
  const start = performance.now();
  const child = spawnSync(command, args, { cwd, encoding: 'utf8', timeout: 120_000, env: { ...process.env, CI: '1' } });
  return {
    command: [command, ...args], cwd, startedAt, durationMs: Math.round(performance.now() - start),
    exitCode: child.status, signal: child.signal, stdout: child.stdout?.slice(-4000) ?? '', stderr: child.stderr?.slice(-4000) ?? '', error: child.error?.message
  };
}

export function passed(step) { return step.exitCode === 0 && !step.error && !step.signal; }
