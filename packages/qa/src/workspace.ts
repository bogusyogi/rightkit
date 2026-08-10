// An isolated, reliably-removable home for one QA run.
//
// Two failures this addresses:
//
//   Single instance. The apps hold a single-instance lock keyed on their user
//   data directory. A harness that launches the app with the real data directory
//   either loses to an already-running copy (the launch silently hands off to the
//   existing window and the driver attaches to nothing) or steals the user's
//   session. Giving the run its own data directory makes the harness a genuinely
//   separate instance instead of a competitor for the same lock.
//
//   Cleanup. Best-effort `rm -rf` in a finally block is not rollback. On Windows a
//   file still mapped by an exiting process fails with EBUSY/EPERM for a short
//   window, so a single attempt leaves directories behind — which is exactly the
//   litter found in packages/legal-ui/.cache. Removal retries, and whatever
//   genuinely could not be removed is REPORTED rather than swallowed.

import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';

export interface QaWorkspace {
  runId: string;
  root: string;
  /** Isolated user-data directory; pass to the app so it takes its own lock. */
  dataDir: string;
  /** Somewhere for screenshots, logs, and other recorded evidence. */
  evidenceDir: string;
  /** Environment overrides that point the app at this workspace. */
  env: Record<string, string>;
}

export interface DisposeReport {
  removed: string[];
  retained: Array<{ path: string; reason: string }>;
}

export interface CreateQaWorkspaceOptions {
  root: string;
  runId?: string;
  appKey: string;
}

export function createQaWorkspace(options: CreateQaWorkspaceOptions): QaWorkspace {
  const appKey = requireText(options.appKey, 'appKey');
  const runId = options.runId ?? crypto.randomUUID();
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(runId)) throw new Error(`unsafe QA run id: ${runId}`);
  const root = path.resolve(requireText(options.root, 'root'), 'runs', runId);
  const dataDir = path.join(root, 'data');
  const evidenceDir = path.join(root, 'evidence');
  fs.mkdirSync(dataDir, { recursive: true });
  fs.mkdirSync(evidenceDir, { recursive: true });

  return {
    runId,
    root,
    dataDir,
    evidenceDir,
    env: {
      RIGHTKIT_QA_NATIVE: '1',
      RIGHTKIT_QA_RUN_ID: runId,
      RIGHTKIT_QA_DATA_DIR: dataDir,
      RIGHTKIT_QA_EVIDENCE_DIR: evidenceDir,
      // Tauri/webview honour these for profile isolation; setting all three keeps
      // the run off the user's real profile regardless of which the app reads.
      XDG_DATA_HOME: dataDir,
      XDG_CONFIG_HOME: dataDir,
      WEBVIEW2_USER_DATA_FOLDER: path.join(dataDir, 'webview2'),
      [`${appKey.toUpperCase().replace(/[^A-Z0-9]/g, '_')}_DATA_DIR`]: dataDir,
    },
  };
}

export interface DisposeOptions {
  /** Keep the recorded evidence and remove only the volatile profile. */
  keepEvidence?: boolean;
  attempts?: number;
  retryDelayMs?: number;
  sleep?: (ms: number) => void;
}

/**
 * Remove the workspace, retrying through the brief window in which an exiting
 * app still holds its files open. Anything still present afterwards is returned,
 * never silently ignored.
 */
export function disposeQaWorkspace(workspace: QaWorkspace, options: DisposeOptions = {}): DisposeReport {
  const attempts = Math.max(1, options.attempts ?? 5);
  const retryDelayMs = options.retryDelayMs ?? 200;
  const sleep = options.sleep ?? sleepSync;
  const targets = options.keepEvidence ? [workspace.dataDir] : [workspace.root];
  const report: DisposeReport = { removed: [], retained: [] };

  for (const target of targets) {
    // Only ever remove inside the workspace root we created.
    const resolved = path.resolve(target);
    if (resolved !== path.resolve(workspace.root) && !isInside(workspace.root, resolved)) {
      report.retained.push({ path: resolved, reason: 'outside the QA workspace root' });
      continue;
    }
    if (!fs.existsSync(resolved)) {
      report.removed.push(resolved);
      continue;
    }

    let lastError: unknown;
    for (let attempt = 1; attempt <= attempts; attempt += 1) {
      try {
        fs.rmSync(resolved, { recursive: true, force: true });
        lastError = undefined;
        break;
      } catch (error) {
        lastError = error;
        if (attempt < attempts) sleep(retryDelayMs);
      }
    }
    if (lastError === undefined && !fs.existsSync(resolved)) report.removed.push(resolved);
    else report.retained.push({ path: resolved, reason: describeError(lastError) });
  }
  return report;
}

/**
 * Sweep run directories left behind by earlier crashed runs. Bounded to the
 * workspace's own `runs` directory and to entries older than `olderThanMs`, so a
 * concurrent run is never removed out from under itself.
 */
export function pruneAbandonedWorkspaces(
  root: string,
  { olderThanMs = 6 * 60 * 60_000, now = Date.now(), keep = [] as string[] }: { olderThanMs?: number; now?: number; keep?: string[] } = {},
): DisposeReport {
  const runsRoot = path.resolve(root, 'runs');
  const report: DisposeReport = { removed: [], retained: [] };
  let entries: fs.Dirent[];
  try {
    entries = fs.readdirSync(runsRoot, { withFileTypes: true });
  } catch {
    return report;
  }

  for (const entry of entries) {
    if (!entry.isDirectory() || entry.isSymbolicLink()) continue;
    if (keep.includes(entry.name)) continue;
    const target = path.join(runsRoot, entry.name);
    let ageMs: number;
    try {
      ageMs = now - fs.statSync(target).mtimeMs;
    } catch {
      continue;
    }
    if (ageMs < olderThanMs) {
      report.retained.push({ path: target, reason: `only ${Math.round(ageMs / 1000)}s old; may belong to a live run` });
      continue;
    }
    try {
      fs.rmSync(target, { recursive: true, force: true });
      report.removed.push(target);
    } catch (error) {
      report.retained.push({ path: target, reason: describeError(error) });
    }
  }
  return report;
}

function isInside(root: string, candidate: string): boolean {
  const relative = path.relative(path.resolve(root), path.resolve(candidate));
  return relative !== '' && !relative.startsWith('..') && !path.isAbsolute(relative);
}

function describeError(error: unknown): string {
  if (error === undefined) return 'still present after removal reported success';
  const code = (error as NodeJS.ErrnoException).code;
  return code ? `${code}: ${(error as Error).message}` : String((error as Error)?.message ?? error);
}

function sleepSync(ms: number): void {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function requireText(value: string, name: string): string {
  const normalized = String(value ?? '').trim();
  if (!normalized) throw new Error(`${name} is required`);
  return normalized;
}
