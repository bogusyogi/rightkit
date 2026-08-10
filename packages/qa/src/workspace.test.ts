import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { createQaWorkspace, disposeQaWorkspace, pruneAbandonedWorkspaces } from './workspace.js';

const roots: string[] = [];

function tempRoot(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'rightkit-qa-ws-'));
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
    expect(workspace.env.RIGHTKIT_QA_DATA_DIR).toBe(workspace.dataDir);
    expect(workspace.env.VIEWRIGHT_DATA_DIR).toBe(workspace.dataDir);
    expect(workspace.env.WEBVIEW2_USER_DATA_FOLDER).toContain(workspace.dataDir);
    expect(workspace.dataDir).not.toBe(os.homedir());
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

    const report = disposeQaWorkspace(workspace, { keepEvidence: true });

    expect(report.retained).toEqual([]);
    expect(fs.existsSync(workspace.dataDir)).toBe(false);
    expect(fs.readFileSync(path.join(workspace.evidenceDir, 'screenshot.png'), 'utf8')).toBe('png');
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
      expect(attempts).toBe(3);
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
    } finally {
      (fs as { rmSync: typeof fs.rmSync }).rmSync = realRm;
    }
  });

  it('sweeps directories left by crashed runs but never a live one', () => {
    const root = tempRoot();
    const stale = createQaWorkspace({ root, runId: 'stale-run', appKey: 'viewright' });
    const live = createQaWorkspace({ root, runId: 'live-run', appKey: 'viewright' });
    const old = Date.now() - 24 * 60 * 60_000;
    fs.utimesSync(stale.root, new Date(old), new Date(old));

    const report = pruneAbandonedWorkspaces(root, { keep: [live.runId] });

    expect(report.removed).toContain(stale.root);
    expect(fs.existsSync(live.root)).toBe(true);
  });

  it('leaves a recent directory alone because it may belong to a concurrent run', () => {
    const root = tempRoot();
    const recent = createQaWorkspace({ root, runId: 'recent-run', appKey: 'viewright' });
    const report = pruneAbandonedWorkspaces(root);
    expect(report.removed).toEqual([]);
    expect(report.retained[0]).toMatchObject({ path: recent.root });
    expect(report.retained[0].reason).toMatch(/may belong to a live run/);
  });

  it('is a no-op when the workspace root has never been created', () => {
    expect(pruneAbandonedWorkspaces(path.join(tempRoot(), 'never-used'))).toEqual({ removed: [], retained: [] });
  });
});
