import { spawnSync } from 'node:child_process';

export interface ControlCallOptions {
  /** The control service name the launcher gave the app (`RIGHTKIT_CONTROL_SERVICE`). */
  service: string;
  /** The server method: `health`, `dom`, `click`, `eval`, `screenshot`, ... */
  method: string;
  params?: Record<string, unknown>;
  /** Absolute path of the per-run control token file (default: `RIGHTKIT_QA_CONTROL_TOKEN_FILE`). */
  tokenFile?: string;
  /** The caller label the server's allowlist knows (default `right-qa`). */
  app?: string;
  /** The `rightkit-qa` executable (default `rightkit-qa` on PATH). */
  bin?: string;
  timeoutMs?: number;
}

/**
 * One order to an embedded QA control server that is already running, through `rightkit-qa control
 * call` (the Rust client owns the wire protocol, the token and the same-user socket). Resolves with
 * the server's JSON reply; rejects with the server's error. QA ONLY: a shipped app has no server.
 */
export function controlCall(options: ControlCallOptions): Promise<unknown> {
  const args = ['control', 'call', '--service', options.service, '--method', options.method];
  if (options.params !== undefined) args.push('--params', JSON.stringify(options.params));
  const tokenFile = options.tokenFile ?? process.env.RIGHTKIT_QA_CONTROL_TOKEN_FILE;
  if (tokenFile) args.push('--token-file', tokenFile);
  if (options.app) args.push('--app', options.app);
  const run = spawnSync(options.bin ?? 'rightkit-qa', args, {
    encoding: 'utf8',
    windowsHide: true,
    timeout: options.timeoutMs ?? 120_000,
    maxBuffer: 256 * 1024 * 1024,
  });
  if (run.error) return Promise.reject(run.error);
  let reply: unknown;
  try {
    reply = JSON.parse(run.stdout);
  } catch {
    return Promise.reject(new Error(`rightkit-qa control call ${options.method} failed (exit ${run.status}): ${(run.stderr || run.stdout).trim()}`));
  }
  if (run.status !== 0) {
    const message = typeof reply === 'object' && reply !== null && 'error' in reply ? String((reply as { error: unknown }).error) : run.stdout.trim();
    return Promise.reject(new Error(`${options.method}: ${message}`));
  }
  return Promise.resolve(reply);
}
