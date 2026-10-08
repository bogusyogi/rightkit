import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { appTempDir, createManagedRun, finishManagedRun, pruneManagedRuns } from './storage.js';
import { createQaWorkspace as createWorkspace, disposeQaWorkspace, pruneAbandonedWorkspaces } from './workspace.js';

const roots: string[] = [];
function createQaWorkspace(options: Parameters<typeof createWorkspace>[0]) {
  const workspace = createWorkspace(options);
  roots.push(workspace.home);
  return workspace;
}

function tempRoot(): string {
  const root = fs.mkdtempSync(path.join(fs.realpathSync.native(os.tmpdir()), 'rightkit-qa-ws-'));
  roots.push(root);
  return root;
}

afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

describe('QA run workspace', () => {
  it('gives the run its own data directory so it does not fight the installed instance', () => {
    const workspace = createQaWorkspace({ root: tempRoot(), runId: 'run-1', appKey: 'viewright' });
    expect(fs.existsSync(workspace.dataDir)).toBe(true);
    expect(fs.existsSync(path.join(workspace.root, 'data'))).toBe(false);
    expect(workspace.env.RIGHTKIT_QA_DATA_DIR).toBe(workspace.dataDir);
    expect(workspace.env.VIEWRIGHT_DATA_DIR).toBe(workspace.dataDir);
    expect(workspace.env.WEBVIEW2_USER_DATA_FOLDER).toContain(workspace.dataDir);
    expect(workspace.env.RIGHTKIT_RUN_ROOT).toBe(workspace.home);
    expect(workspace.env.TMPDIR).toContain(workspace.home);
    expect(workspace.home).not.toContain(workspace.root);
    expect(workspace.home.startsWith(path.join(appTempDir(), 'rightkit-qa') + path.sep)).toBe(true);
    expect(workspace.evidenceDir).toContain(workspace.root);
    for (const key of ['RIGHTKIT_RUN_ROOT', 'RIGHTKIT_SUITE_ROOT', 'HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'XDG_DATA_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'WEBVIEW2_USER_DATA_FOLDER', 'VIEWRIGHT_DATA_DIR', 'RIGHTKIT_QA_DATA_DIR', 'RIGHTKIT_QA_EVIDENCE_DIR', 'RIGHTKIT_QA_CAPTURE_DIR', 'TMPDIR', 'TMP', 'TEMP']) expect(workspace.env[key]).toContain(workspace.home);
    expect(workspace.captureDir).toContain(workspace.root);
    expect(workspace.dataDir).not.toBe(os.homedir());
  });

  it.skipIf(process.platform !== 'darwin')('ignores external broker TMPDIR for app-side paths', () => {
    const root = tempRoot();
    const spy = vi.spyOn(os, 'tmpdir').mockReturnValue('/Volumes/D');
    try {
      const workspace = createQaWorkspace({ root, appKey: 'viewright' });
      expect(workspace.home.startsWith('/private/tmp/rightkit-qa/')).toBe(true);
      expect(workspace.env.RIGHTKIT_QA_EVIDENCE_DIR).toContain(workspace.home);
      for (const [key, value] of Object.entries(workspace.env)) {
        if (path.isAbsolute(value)) expect(value, key).toMatch(/^\/private\/tmp\/rightkit-qa\//);
      }
    } finally { spy.mockRestore(); }
  });

  it.skipIf(process.platform !== 'darwin')('keeps fallback control socket paths short even with long app keys', () => {
    const workspace = createQaWorkspace({ root: tempRoot(), appKey: 'a'.repeat(100) });
    const socket = path.join(workspace.env.TMPDIR, 'rk-12345678-control-12345678.sock');
    expect(Buffer.byteLength(socket)).toBeLessThanOrEqual(100);
  });

  it('refuses an unsafe run id rather than building a path from it', () => {
    expect(() => createQaWorkspace({ root: tempRoot(), runId: '../escape', appKey: 'viewright' }))
      .toThrow(/unsafe QA run id/);
  });

  it('removes the whole workspace and says so', () => {
    const workspace = createQaWorkspace({ root: tempRoot(), runId: 'run-2', appKey: 'viewright' });
    fs.writeFileSync(path.join(workspace.dataDir, 'profile.db'), 'x');
    const report = disposeQaWorkspace(workspace);
    expect(report.retained).toEqual([]);
    expect(report.removed).toContain(workspace.root);
    expect(fs.existsSync(workspace.root)).toBe(false);
  });

  it('keeps recorded evidence while discarding the volatile profile', () => {
    const workspace = createQaWorkspace({ root: tempRoot(), runId: 'run-3', appKey: 'viewright' });
    fs.writeFileSync(path.join(workspace.evidenceDir, 'screenshot.png'), 'png');
    fs.writeFileSync(path.join(workspace.dataDir, 'profile.db'), 'x');
    fs.writeFileSync(path.join(workspace.home, 'app-launch.log'), 'live log');
    fs.writeFileSync(path.join(workspace.env.RIGHTKIT_QA_EVIDENCE_DIR, 'app.json'), 'evidence');
    fs.writeFileSync(path.join(workspace.env.RIGHTKIT_QA_CAPTURE_DIR, 'frame.png'), 'frame');

    const report = disposeQaWorkspace(workspace, { keepEvidence: true });

    expect(report.retained).toEqual([]);
    expect(fs.existsSync(workspace.dataDir)).toBe(false);
    expect(fs.readFileSync(path.join(workspace.evidenceDir, 'screenshot.png'), 'utf8')).toBe('png');
    expect(fs.existsSync(workspace.home)).toBe(false);
    expect(fs.readFileSync(path.join(workspace.evidenceDir, 'app-launch.log'), 'utf8')).toBe('live log');
    expect(fs.readFileSync(path.join(workspace.evidenceDir, 'app', 'app.json'), 'utf8')).toBe('evidence');
    expect(fs.readFileSync(path.join(workspace.captureDir, 'frame.png'), 'utf8')).toBe('frame');
  });

  it('retries a removal that fails while the app is still exiting', () => {
    const workspace = createQaWorkspace({ root: tempRoot(), runId: 'run-4', appKey: 'viewright' });
    let attempts = 0;
    const realRm = fs.rmSync;
    const spy = ((target: fs.PathLike, options?: fs.RmOptions) => {
      attempts += 1;
      if (attempts < 3) throw Object.assign(new Error('resource busy or locked'), { code: 'EBUSY' });
      return realRm(target, options);
    }) as typeof fs.rmSync;
    (fs as { rmSync: typeof fs.rmSync }).rmSync = spy;
    try {
      const report = disposeQaWorkspace(workspace, { retryDelayMs: 0, sleep: () => {} });
      expect(attempts).toBe(4);
      expect(report.removed).toContain(workspace.root);
    } finally {
      (fs as { rmSync: typeof fs.rmSync }).rmSync = realRm;
    }
  });

  it('reports what it could not remove instead of silently leaving litter', () => {
    const workspace = createQaWorkspace({ root: tempRoot(), runId: 'run-5', appKey: 'viewright' });
    const realRm = fs.rmSync;
    (fs as { rmSync: typeof fs.rmSync }).rmSync = (() => {
      throw Object.assign(new Error('resource busy or locked'), { code: 'EBUSY' });
    }) as typeof fs.rmSync;
    try {
      const report = disposeQaWorkspace(workspace, { attempts: 2, retryDelayMs: 0, sleep: () => {} });
      expect(report.removed).toEqual([]);
      expect(report.retained[0].reason).toMatch(/EBUSY/);
      expect(fs.existsSync(workspace.root)).toBe(true);
      expect(JSON.parse(fs.readFileSync(path.join(workspace.root, '.rightkit-run.json'), 'utf8')).appRoot).toBe(workspace.home);
    } finally {
      (fs as { rmSync: typeof fs.rmSync }).rmSync = realRm;
    }
  });

  it('preserves ownership & outputs when finish copying fails', () => {
    const workspace = createQaWorkspace({ root: tempRoot(), appKey: 'viewright' });
    fs.symlinkSync(workspace.root, path.join(workspace.env.RIGHTKIT_QA_EVIDENCE_DIR, 'outside'));
    const report = disposeQaWorkspace(workspace);
    expect(report.retained[0].reason).toMatch(/symlink/);
    expect(fs.existsSync(workspace.home)).toBe(true);
    expect(JSON.parse(fs.readFileSync(path.join(workspace.root, '.rightkit-run.json'), 'utf8')).ownerPid).toBe(process.pid);
  });

  it('sweeps directories left by crashed runs but never a live one', () => {
    const root = tempRoot();
    const stale = createQaWorkspace({ root, runId: 'stale-run', appKey: 'viewright' });
    const live = createQaWorkspace({ root, runId: 'live-run', appKey: 'viewright' });
    const old = Date.now() - 48 * 60 * 60_000;
    finishManagedRun(stale.root);
    const markerPath = path.join(stale.root, '.rightkit-run.json');
    const marker = JSON.parse(fs.readFileSync(markerPath, 'utf8')) as Record<string, unknown>;
    fs.writeFileSync(markerPath, JSON.stringify({ ...marker, createdAt: old }));

    const report = pruneAbandonedWorkspaces(root, { olderThanMs: 6 * 60 * 60_000, keep: [live.runId] });

    expect(report.removed).toContain(stale.root);
    expect(fs.existsSync(live.root)).toBe(true);
    expect(fs.existsSync(live.home)).toBe(true);
    expect(fs.existsSync(stale.home)).toBe(false);
  });

  it('leaves a recent directory alone because it may belong to a concurrent run', () => {
    const root = tempRoot();
    const recent = createQaWorkspace({ root, runId: 'recent-run', appKey: 'viewright' });
    const report = pruneAbandonedWorkspaces(root);
    expect(report.removed).toEqual([]);
    expect(report.retained[0]).toMatchObject({ path: recent.root });
    expect(report.retained[0].reason).toMatch(/may belong to a live run/);
  });

  it('keeps one completed run older than six hours under default retention', () => {
    const root = tempRoot();
    const completed = createManagedRun({ managedRoot: root, app: 'viewright', runId: 'completed-old', leaseMs: 1 });
    finishManagedRun(completed);
    const markerPath = path.join(completed.root, '.rightkit-run.json');
    const marker = JSON.parse(fs.readFileSync(markerPath, 'utf8')) as Record<string, unknown>;
    fs.writeFileSync(markerPath, JSON.stringify({ ...marker, createdAt: Date.now() - 8 * 60 * 60_000 }));

    const report = pruneAbandonedWorkspaces(root);

    expect(report.removed).toEqual([]);
    expect(fs.existsSync(completed.root)).toBe(true);
  });

  it('is a no-op when the workspace root has never been created', () => {
    expect(pruneAbandonedWorkspaces(path.join(tempRoot(), 'never-used'))).toEqual({ removed: [], retained: [] });
  });

  it('keeps newest five under count retention even when byte cap is not exceeded', () => {
    const root = tempRoot();
    const old = Date.now() - 48 * 60 * 60_000;
    for (let i = 0; i < 7; i += 1) {
      const run = createManagedRun({ managedRoot: root, app: 'viewright', runId: `count-${i}`, leaseMs: 1, retention: { keepCount: 100 } });
      finishManagedRun(run);
      const markerPath = path.join(run.root, '.rightkit-run.json');
      const marker = JSON.parse(fs.readFileSync(markerPath, 'utf8')) as Record<string, unknown>;
      fs.writeFileSync(markerPath, JSON.stringify({ ...marker, createdAt: old + i }));
    }
    const report = pruneManagedRuns(root, 'viewright', { maxBytes: 1024 ** 3, now: Date.now() });
    expect(report.removed).toHaveLength(2);
    expect(fs.existsSync(path.join(root, 'runs', 'viewright', 'count-5'))).toBe(true);
    expect(fs.existsSync(path.join(root, 'runs', 'viewright', 'count-6'))).toBe(true);
  });
});
