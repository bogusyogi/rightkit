import { spawnSync } from 'node:child_process';

/** Injectable git access so census, gate and run are testable without a repository. */
export interface Git {
  /** stdout of `git <args>` in the repository, or null on a non-zero exit. */
  run(args: string[]): string | null;
}

export function systemGit(cwd: string): Git {
  return {
    run(args) {
      const r = spawnSync('git', args, { cwd, encoding: 'utf8', maxBuffer: 256 * 1024 * 1024, windowsHide: true });
      return r.status === 0 ? r.stdout : null;
    },
  };
}

export function showAt(git: Git, ref: string, file: string): string | null {
  return git.run(['show', `${ref}:${file}`]);
}

export function changedPaths(git: Git, from: string, to?: string): string[] | null {
  const out = git.run(to ? ['diff', '--name-only', from, to] : ['diff', '--name-only', from]);
  if (out === null) return null;
  const paths = new Set(out.split('\n').map((l) => l.trim()).filter(Boolean));
  if (!to) {
    const untracked = git.run(['ls-files', '--others', '--exclude-standard']);
    for (const l of (untracked ?? '').split('\n')) if (l.trim()) paths.add(l.trim());
  }
  return [...paths].sort();
}
