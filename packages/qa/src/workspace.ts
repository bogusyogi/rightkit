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
import {
  createManagedRun,
  createManagedAppRoot,
  describeKeptHome,
  finishManagedRun,
  isOwnedHome,
  keepFailedHomeRequested,
  preflightQaRoot,
  pruneManagedRuns,
  resolveManagedRoot,
  type ManagedRun,
} from './storage.js';

export interface QaWorkspace {
  runId: string;
  appKey: string;
  managedRoot: string;
  root: string;
  /** Owned app home on internal system temp storage. */
  home: string;
  /** Isolated user-data directory; pass to the app so it takes its own lock. */
  dataDir: string;
  /** Somewhere for screenshots, logs, and other recorded evidence. */
  evidenceDir: string;
  /** Managed capture output directory. */
  captureDir: string;
  /** Environment overrides that point the app at this workspace. */
  env: Record<string, string>;
  marker: ManagedRun['marker'];
}

export interface DisposeReport {
  removed: string[];
  retained: Array<{ path: string; reason: string }>;
  /** Set when a failed run's home was kept on request; also printed to stderr. */
  kept?: { path: string; description: string };
}

export interface CreateQaWorkspaceOptions {
  /** Managed root override; tests may provide a temporary directory. */
  root?: string;
  runId?: string;
  appKey: string;
  leaseMs?: number;
  retention?: import('./storage.js').RetentionPolicy;
  /** Start-of-run sweep and size-cap check of the QA root. Default on (off under vitest). */
  preflight?: import('./storage.js').PreflightOptions;
}

/**
 * Create an isolated run home. `env.RIGHTKIT_RUN_ROOT` is set to `home`, which lives inside the
 * workspace that {@link disposeQaWorkspace} deletes. A host must never map a directory holding
 * artifacts it wants to keep onto RIGHTKIT_RUN_ROOT: disposal removes that tree.
 */
export function createQaWorkspace(options: CreateQaWorkspaceOptions): QaWorkspace {
  const appKey = requireText(options.appKey, 'appKey');
  // Sweep dead homes and refuse to start over the QA root's size cap, before a run exists.
  preflightQaRoot(options.preflight);
  const managed = createManagedRun({ managedRoot: options.root ? requireText(options.root, 'root') : resolveManagedRoot(), app: appKey, runId: options.runId ?? crypto.randomUUID(), leaseMs: options.leaseMs, retention: options.retention });
  const runId = managed.runId;
  const root = managed.root;
  const home = createManagedAppRoot(managed);
  const dataDir = path.join(home, 'data');
  const evidenceDir = path.join(root, 'evidence');
  const captureDir = path.join(root, 'captures');
  const tmpDir = path.join(home, 'tmp');
  const cacheDir = path.join(home, 'cache');
  const appEvidenceDir = path.join(home, 'evidence');
  const appCaptureDir = path.join(home, 'captures');
  fs.mkdirSync(appEvidenceDir, { recursive: true });
  fs.mkdirSync(appCaptureDir, { recursive: true });
  fs.mkdirSync(dataDir, { recursive: true });
  fs.mkdirSync(evidenceDir, { recursive: true });
  fs.mkdirSync(captureDir, { recursive: true });
  fs.mkdirSync(tmpDir, { recursive: true });
  fs.mkdirSync(cacheDir, { recursive: true });

  return {
    runId,
    appKey,
    managedRoot: path.resolve(options.root ? requireText(options.root, 'root') : resolveManagedRoot()),
    root,
    home,
    dataDir,
    evidenceDir,
    captureDir,
    env: {
      RIGHTKIT_QA_NATIVE: '1',
      RIGHTKIT_QA_RUN_ID: runId,
      RIGHTKIT_RUN_ROOT: home,
      RIGHTKIT_SUITE_ROOT: path.join(home, 'suite'),
      HOME: home,
      USERPROFILE: home,
      APPDATA: dataDir,
      LOCALAPPDATA: dataDir,
      XDG_CACHE_HOME: cacheDir,
      RIGHTKIT_QA_DATA_DIR: dataDir,
      RIGHTKIT_QA_EVIDENCE_DIR: appEvidenceDir,
      RIGHTKIT_QA_CAPTURE_DIR: appCaptureDir,
      TMPDIR: tmpDir,
      TMP: tmpDir,
      TEMP: tmpDir,
      // Tauri/webview honour these for profile isolation; setting all three keeps
      // the run off the user's real profile regardless of which the app reads.
      XDG_DATA_HOME: dataDir,
      XDG_CONFIG_HOME: dataDir,
      WEBVIEW2_USER_DATA_FOLDER: path.join(dataDir, 'webview2'),
      [`${appKey.toUpperCase().replace(/[^A-Z0-9]/g, '_')}_DATA_DIR`]: dataDir,
    },
    marker: managed.marker,
  };
}

/**
 * Create a workspace, run `body`, and dispose of it however `body` ends. A throw counts as a failed run
 * (see {@link DisposeOptions.keepFailedHome}). Evidence is kept and the home removed unless `dispose` says otherwise.
 */
export async function withQaWorkspace<T>(options: CreateQaWorkspaceOptions, body: (workspace: QaWorkspace) => Promise<T> | T, dispose: Omit<DisposeOptions, 'failed'> = {}): Promise<T> {
  const workspace = createQaWorkspace(options);
  let failed = true;
  try {
    const result = await body(workspace);
    failed = false;
    return result;
  } finally {
    const report = disposeQaWorkspace(workspace, { keepEvidence: true, ...dispose, failed });
    for (const r of report.retained) if (!report.kept) process.stderr.write(`[qa] WARN could not remove ${r.path}: ${r.reason}\n`);
  }
}

export interface DisposeOptions {
  /** The run failed. With `keepFailedHome` (or RIGHTKIT_QA_KEEP_FAILED_HOME=1) its home is kept and reported. */
  failed?: boolean;
  keepFailedHome?: boolean;
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
 *
 * Deletes `workspace.home`, which is what RIGHTKIT_RUN_ROOT points at, and also
 * `workspace.root` unless `keepEvidence` is set. Anything a host maps under
 * RIGHTKIT_RUN_ROOT is deleted: never map a directory holding artifacts you want
 * to keep onto it.
 */
export function disposeQaWorkspace(workspace: QaWorkspace, options: DisposeOptions = {}): DisposeReport {
  const attempts = Math.max(1, options.attempts ?? 5);
  const retryDelayMs = options.retryDelayMs ?? 200;
  const sleep = options.sleep ?? sleepSync;
  const targets = options.keepEvidence ? [workspace.home] : [workspace.home, workspace.root];
  const report: DisposeReport = { removed: [], retained: [] };

  try { finishManagedRun(workspace.root); } catch (error) {
    report.retained.push({ path: workspace.root, reason: describeError(error) });
    return report;
  }

  if (options.failed && (options.keepFailedHome ?? keepFailedHomeRequested())) {
    const description = describeKeptHome(workspace.home);
    report.kept = { path: workspace.home, description };
    report.retained.push({ path: workspace.home, reason: 'kept: failed run' });
    process.stderr.write(`[qa] kept QA home of the failed run: ${description}\n`);
    return report;
  }

  for (const target of targets) {
    // Only remove the managed run or its registered temp companion.
    const resolved = path.resolve(target);
    if (resolved !== path.resolve(workspace.root) && resolved !== path.resolve(workspace.home)) {
      report.retained.push({ path: resolved, reason: 'outside the QA workspace root' });
      continue;
    }
    if (!fs.existsSync(resolved)) {
      report.removed.push(resolved);
      continue;
    }
    // Deletion happens only inside the QA root this runner owns, never by a caller's path.
    if (resolved === path.resolve(workspace.home) && !isOwnedHome(resolved)) {
      report.retained.push({ path: resolved, reason: 'not a QA home created by rightkit-qa; not deleted' });
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
    else {
      report.retained.push({ path: resolved, reason: describeError(lastError) });
      if (resolved === path.resolve(workspace.home)) {
        if (!options.keepEvidence) report.retained.push({ path: workspace.root, reason: 'app temp cleanup pending' });
        break;
      }
    }
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
  { olderThanMs, now = Date.now(), keep = [] as string[] }: { olderThanMs?: number; now?: number; keep?: string[] } = {},
): DisposeReport {
  const managedRoot = path.resolve(root);
  const report: DisposeReport = { removed: [], retained: [] };
  const runsRoot = path.join(managedRoot, 'runs');
  let apps: fs.Dirent[];
  try { apps = fs.readdirSync(runsRoot, { withFileTypes: true }); } catch { return report; }
  for (const app of apps) {
    if (app.isDirectory() && !app.isSymbolicLink()) {
      const policy = olderThanMs === undefined
        ? { now, keepRunIds: keep }
        : { now, youngMs: olderThanMs, keepCount: 0, maxBytes: 0, keepRunIds: keep };
      const sub = pruneManagedRuns(managedRoot, app.name, policy);
      report.removed.push(...sub.removed);
      report.retained.push(...sub.retained);
    }
  }
  return report;
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
