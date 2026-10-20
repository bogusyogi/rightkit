/**
 * The default profile remover: a separate Node process runs `rmSync`. It lives in its own file with no imports
 * from the rest of the package, and uses only syntax Node can run by stripping types, so a test can start it
 * in a real child process and watch that process exit.
 */
import { spawn } from 'node:child_process';

/**
 * Longest the close waits, after a removal's bound is spent, for the aborted remover to confirm it has stopped.
 * {@link removeDirInChild} gives up its own handles after the same time.
 */
export const REMOVER_STOP_WAIT_MS = 1_000;

/** Runs in the remover process: removes the directory named by the environment; a failure prints its errno code. */
const REMOVER_SCRIPT =
  "try{require('node:fs').rmSync(process.env.RK_QA_REMOVE_DIR,{recursive:true,force:true})}" +
  "catch(e){process.stdout.write(String(e&&e.code||'ERR'));process.exit(1)}";

/** `code` of the error {@link removeDirInChild} rejects with once it has stopped its remover on abort. */
export const REMOVE_ABORTED_CODE = 'RK_REMOVE_ABORTED';

/**
 * Default removal: a separate Node process runs `rmSync`, so a stalled or huge recursive removal blocks nothing
 * in this process and can be stopped. When `signal` aborts (the bound is spent) the remover is killed through
 * the ChildProcess handle this call owns (never by a saved pid), and the call settles only after that process
 * has exited: it rejects with `code: REMOVE_ABORTED_CODE` and the remover's `pid`. If the exit never comes the
 * call never settles (a failed kill, reported by the child's 'error' event while the remover is alive, is no
 * exit either), and the caller reports the removal as still running. Otherwise it rejects with the remover's
 * errno `code`.
 *
 * An exit that never comes must not keep this process alive: {@link REMOVER_STOP_WAIT_MS} after the abort, when
 * the remover has still not been seen to exit, this call stops waiting for it. It unreferences the child and
 * destroys the pipe to it, so neither holds the event loop open. That releases handles only; it claims nothing
 * about the remover, which may still be running, and the call stays unsettled unless the exit is recorded later.
 * `script` is for tests.
 */
export function removeDirInChild(dir: string, _timeoutMs: number, signal: AbortSignal, script: string = REMOVER_SCRIPT): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const aborted = (pid?: number) => Object.assign(new Error('profile removal aborted: its bound was spent'), { code: REMOVE_ABORTED_CODE, pid });
    if (signal.aborted) {
      reject(aborted());
      return;
    }
    const child = spawn(process.execPath, ['-e', script], {
      stdio: ['ignore', 'pipe', 'ignore'],
      windowsHide: true,
      env: { ...process.env, RK_QA_REMOVE_DIR: dir, ELECTRON_RUN_AS_NODE: '1' },
    });
    let out = '';
    let stopping = false;
    let release: ReturnType<typeof setTimeout> | undefined;
    const onAbort = () => {
      stopping = true;
      // The handle is live until Node records the exit, so this signals our remover and nothing else.
      if (child.exitCode === null && child.signalCode === null) {
        try {
          child.kill('SIGKILL');
        } catch { /* already gone: 'close' follows */ }
      }
      // Abandon the remover's handles if its exit is still not recorded when the stop wait is over. This timer is
      // bounded and cleared on 'close' or 'error', so it never outlives the child it watches.
      release = setTimeout(() => {
        child.unref();
        child.stdout!.destroy();
      }, REMOVER_STOP_WAIT_MS);
    };
    const settleHandles = () => {
      signal.removeEventListener('abort', onAbort);
      if (release !== undefined) clearTimeout(release);
    };
    signal.addEventListener('abort', onAbort, { once: true });
    child.stdout!.setEncoding('utf8');
    child.stdout!.on('data', (chunk: string) => { out += chunk; });
    // 'error' also reports a failed kill, with the remover still alive: that is no exit, so it settles nothing and
    // the exit stays awaited ('close', bounded by the release timer). Only a spawn failure (no pid: no process
    // ever existed) settles here; a plain removal failure, or a stop when the abort won the race.
    child.on('error', (error) => {
      if (child.pid !== undefined) return;
      settleHandles();
      reject(stopping ? aborted() : error);
    });
    child.once('close', (code) => {
      settleHandles();
      if (stopping) reject(aborted(child.pid));
      else if (code === 0) resolve();
      else reject(Object.assign(new Error(`profile removal failed (${out.trim() || `exit ${code}`})`), out.trim() ? { code: out.trim() } : {}));
    });
  });
}
