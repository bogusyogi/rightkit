import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { HOME_MARKER, QaRootOverCapError, checkQaRootCap, cleanQaHomes, isOwnedHome } from './storage.js';
import { createQaWorkspace, disposeQaWorkspace, withQaWorkspace } from './workspace.js';

const roots: string[] = [];
afterEach(() => { for (const r of roots.splice(0)) fs.rmSync(r, { recursive: true, force: true }); });

function qaParent(): { base: string; parent: string } {
  const base = fs.mkdtempSync(path.join(fs.realpathSync.native(os.tmpdir()), 'rkqa-root-'));
  roots.push(base);
  const parent = path.join(base, 'rightkit-qa');
  fs.mkdirSync(parent);
  return { base, parent };
}

function home(parent: string, name: string, pid: number, startedAtMs: number, payload = 10): string {
  const dir = path.join(parent, name);
  fs.mkdirSync(dir);
  fs.writeFileSync(path.join(dir, '.rightkit-app.json'), JSON.stringify({ runRoot: path.join(parent, 'gone'), app: 'app', runId: name }));
  fs.writeFileSync(path.join(dir, HOME_MARKER), JSON.stringify({ schemaVersion: 1, pid, startedAtMs, app: 'app', runId: name }));
  fs.writeFileSync(path.join(dir, 'payload.bin'), Buffer.alloc(payload));
  return dir;
}

describe('QA root hygiene', () => {
  const hours = 3600_000;

  it('removes dead old homes and never a live, young or unbound one', () => {
    const { parent } = qaParent();
    const now = Date.now();
    const deadOld = home(parent, 'dead-old', 111, now - 3 * hours);
    const deadYoung = home(parent, 'dead-young', 111, now);
    const aliveOld = home(parent, 'alive-old', 222, now - 3 * hours);
    const stranger = path.join(parent, 'stranger');
    fs.mkdirSync(stranger);
    const isAlive = (pid: number) => pid === 222;
    const dry = cleanQaHomes({ parent, maxAgeMs: 2 * hours, dryRun: true, now, isAlive });
    expect(dry.removed).toHaveLength(1);
    expect(fs.existsSync(deadOld)).toBe(true);
    const report = cleanQaHomes({ parent, maxAgeMs: 2 * hours, now, isAlive });
    expect(report.removed.map((r) => r.path)).toEqual([deadOld]);
    expect(fs.existsSync(deadOld)).toBe(false);
    for (const kept of [deadYoung, aliveOld, stranger]) expect(fs.existsSync(kept), kept).toBe(true);
  });

  it('refuses a parent that is not the QA root', () => {
    const { base } = qaParent();
    const victim = path.join(base, 'elsewhere');
    fs.mkdirSync(path.join(victim, 'h'), { recursive: true });
    expect(cleanQaHomes({ parent: victim, maxAgeMs: 0 }).removed).toEqual([]);
    expect(fs.existsSync(path.join(victim, 'h'))).toBe(true);
  });

  it.skipIf(process.platform === 'win32')('never follows a symlink out of the root', () => {
    const { base, parent } = qaParent();
    const outside = path.join(base, 'outside');
    fs.mkdirSync(outside);
    fs.writeFileSync(path.join(outside, 'precious'), 'keep');
    const doomed = home(parent, 'doomed', 111, 1);
    fs.symlinkSync(outside, path.join(doomed, 'link-out'));
    fs.symlinkSync(outside, path.join(parent, 'link-home'));
    cleanQaHomes({ parent, maxAgeMs: 0, isAlive: () => false });
    expect(fs.existsSync(doomed)).toBe(false);
    expect(fs.readFileSync(path.join(outside, 'precious'), 'utf8')).toBe('keep');
    expect(fs.lstatSync(path.join(parent, 'link-home')).isSymbolicLink()).toBe(true);
  });

  it('names the largest homes when the root is over its cap', () => {
    const { parent } = qaParent();
    const big = home(parent, 'big', 1, Date.now(), 3_000_000);
    home(parent, 'small', 1, Date.now(), 1_000_000);
    expect(() => checkQaRootCap(parent, 10_000_000)).not.toThrow();
    expect(() => checkQaRootCap(parent, 0)).not.toThrow();
    try { checkQaRootCap(parent, 2_000_000); throw new Error('expected over cap'); } catch (error) {
      expect(error).toBeInstanceOf(QaRootOverCapError);
      const detail = (error as QaRootOverCapError).detail;
      expect(detail.largest[0].path).toBe(big);
      expect((error as Error).message).toContain('big');
    }
  });

  it('writes the owner marker and counts only a runner-created home as owned', () => {
    const base = fs.mkdtempSync(path.join(fs.realpathSync.native(os.tmpdir()), 'rkqa-own-'));
    roots.push(base);
    const ws = createQaWorkspace({ root: base, appKey: 'ownapp' });
    roots.push(ws.home);
    const marker = JSON.parse(fs.readFileSync(path.join(ws.home, HOME_MARKER), 'utf8'));
    expect(marker.pid).toBe(process.pid);
    expect(Date.now() - marker.startedAtMs).toBeLessThan(60_000);
    expect(isOwnedHome(ws.home)).toBe(true);
    expect(isOwnedHome(base)).toBe(false);
    // A caller-supplied path that is not a QA home is never deleted.
    const foreign = path.join(base, 'precious');
    fs.mkdirSync(foreign);
    fs.writeFileSync(path.join(foreign, 'keep'), 'keep');
    const report = disposeQaWorkspace({ ...ws, home: foreign }, { keepEvidence: true });
    expect(fs.existsSync(path.join(foreign, 'keep'))).toBe(true);
    expect(report.retained.some((r) => r.path === foreign)).toBe(true);
    // The genuine home goes.
    expect(disposeQaWorkspace(ws).retained).toEqual([]);
    expect(fs.existsSync(ws.home)).toBe(false);
  });

  it('removes the home when the body fails, or keeps it only when asked', async () => {
    const base = fs.mkdtempSync(path.join(fs.realpathSync.native(os.tmpdir()), 'rkqa-with-'));
    roots.push(base);
    let seen = '';
    await expect(withQaWorkspace({ root: base, appKey: 'withapp' }, (ws) => { seen = ws.home; throw new Error('scenario failed'); })).rejects.toThrow('scenario failed');
    expect(fs.existsSync(seen)).toBe(false);
    await expect(withQaWorkspace({ root: base, appKey: 'withapp' }, (ws) => { seen = ws.home; throw new Error('boom'); }, { keepFailedHome: true })).rejects.toThrow('boom');
    expect(fs.existsSync(seen)).toBe(true);
    roots.push(seen);
    await withQaWorkspace({ root: base, appKey: 'withapp' }, (ws) => { seen = ws.home; }, { keepFailedHome: true });
    expect(fs.existsSync(seen)).toBe(false);
  });
});
