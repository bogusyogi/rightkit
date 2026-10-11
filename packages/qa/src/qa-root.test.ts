import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import {
  DEFAULT_ROOT_CAP_BYTES,
  DEFAULT_ROOT_CEILING_BYTES,
  HOME_MARKER,
  QaRootOverCapError,
  ROOT_CAP_ENV,
  ROOT_CEILING_ENV,
  RUN_MARKER,
  checkQaRootCap,
  checkQaRootLimits,
  cleanQaHomes,
  isOwnedHome,
  parseQaHomeName,
  preflightQaRoot,
  qaHomeProduct,
  resolveQaRootLimits,
  type PreflightOptions,
} from './storage.js';
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

function home(parent: string, name: string, pid: number, startedAtMs: number, payload = 10, app = 'app'): string {
  const dir = path.join(parent, name);
  fs.mkdirSync(dir);
  fs.writeFileSync(path.join(dir, '.rightkit-app.json'), JSON.stringify({ runRoot: path.join(parent, 'gone'), app, runId: name }));
  fs.writeFileSync(path.join(dir, HOME_MARKER), JSON.stringify({ schemaVersion: 1, pid, startedAtMs, app, runId: name }));
  fs.writeFileSync(path.join(dir, 'payload.bin'), Buffer.alloc(payload));
  return dir;
}

/** A home of `app` named the way the Rust crate names homes: `<app>-<16 hex digits>`. `tag` is lowercase hex. */
function sizedHome(parent: string, app: string, tag: string, payload: number): string {
  return home(parent, `${app}-${tag.padStart(16, '0')}`, 111, Date.now(), payload, app);
}

/**
 * A home written before the owner marker existed: a binding and optionally a run record, no
 * `.rightkit-home.json`. The run record's lease is a day long, as it was.
 */
function unmarkedHome(base: string, parent: string, app: string, tag: string, options: { ownerPid?: number; binding?: boolean; payload?: number } = {}): string {
  const dir = path.join(parent, `${app}-${tag.padStart(16, '0')}`);
  fs.mkdirSync(dir);
  const runRoot = path.join(base, 'runs', path.basename(dir));
  if (options.ownerPid !== undefined) {
    fs.mkdirSync(runRoot, { recursive: true });
    fs.writeFileSync(path.join(runRoot, RUN_MARKER), JSON.stringify({ schemaVersion: 1, app, runId: path.basename(dir), createdAt: Date.now(), ownerPid: options.ownerPid, leaseUntil: Date.now() + 24 * 3600_000 }));
  }
  if (options.binding !== false) fs.writeFileSync(path.join(dir, '.rightkit-app.json'), JSON.stringify({ runRoot, app, runId: path.basename(dir) }));
  fs.writeFileSync(path.join(dir, 'payload.bin'), Buffer.alloc(options.payload ?? 10));
  return dir;
}

/** Set the modification time of everything under `dir`, `dir` included, to `ageMs` ago. Symlinks are left alone. */
function ageTree(dir: string, ageMs: number): void {
  const when = (Date.now() - ageMs) / 1000;
  const all = [dir];
  const stack = [dir];
  while (stack.length) {
    const current = stack.pop() as string;
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      if (entry.isSymbolicLink()) continue;
      const p = path.join(current, entry.name);
      all.push(p);
      if (entry.isDirectory()) stack.push(p);
    }
  }
  for (const p of all) fs.utimesSync(p, when, when);
}

/** Run `body`, which must throw the QA root refusal, and return it. */
function overCap(body: () => void): QaRootOverCapError {
  try { body(); } catch (error) {
    if (error instanceof QaRootOverCapError) return error;
    throw error;
  }
  throw new Error('expected a QaRootOverCapError');
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

describe('QA root limits', () => {
  const hours = 3600_000;

  it('never counts another product against a run', () => {
    const { parent } = qaParent();
    for (const tag of ['1', '2', '3']) sizedHome(parent, 'genright', tag, 1_000_000);
    sizedHome(parent, 'cutright', '1', 1_000_000);
    sizedHome(parent, 'postright', '1', 100_000);
    // genright and cutright hold 4 MB between them; none of it counts against postright.
    expect(() => checkQaRootCap(parent, 'postright', 2_000_000)).not.toThrow();
    expect(() => preflightQaRoot({ enabled: true, parent, app: 'postright', capBytes: 2_000_000, ceilingBytes: 0, env: {} })).not.toThrow();
  });

  it('refuses the product that is over its own cap and names its homes', () => {
    const { parent } = qaParent();
    const big = sizedHome(parent, 'genright', '1', 3_000_000);
    const small = sizedHome(parent, 'genright', '2', 1_000_000);
    const other = sizedHome(parent, 'cutright', '1', 9_000_000);
    expect(() => checkQaRootCap(parent, 'genright', 10_000_000)).not.toThrow();
    expect(() => checkQaRootCap(parent, 'genright', 0)).not.toThrow();
    const error = overCap(() => checkQaRootCap(parent, 'genright', 2_000_000));
    expect(error.detail.kind).toBe('product-cap');
    expect(error.detail.app).toBe('genright');
    expect(error.detail.homeCount).toBe(2);
    expect(error.detail.limitBytes).toBe(2_000_000);
    expect(error.detail.totalBytes).toBeGreaterThanOrEqual(4_000_000);
    expect(error.detail.totalBytes).toBeLessThan(5_000_000);
    expect(error.detail.largest.map((h) => h.path)).toEqual([big, small]);
    expect(error.detail.largest.every((h) => h.product === 'genright')).toBe(true);
    expect(error.message).toContain('genright');
    expect(error.message).toContain(big);
    expect(error.message).toContain(ROOT_CAP_ENV);
    expect(error.message).toContain('rightkit-qa clean');
    expect(error.message).toContain('cleanQaHomes()');
    expect(error.message).not.toContain(other);
  });

  it('counts a home with no binding for the product in its name', () => {
    const { parent } = qaParent();
    const unbound = path.join(parent, 'cutright-0000000000000001');
    fs.mkdirSync(unbound);
    fs.writeFileSync(path.join(unbound, 'blob'), Buffer.alloc(3_000_000));
    expect(() => checkQaRootCap(parent, 'genright', 1_000_000)).not.toThrow();
    const error = overCap(() => checkQaRootCap(parent, 'cutright', 1_000_000));
    expect(error.detail.largest).toEqual([{ path: unbound, bytes: 3_000_000, product: 'cutright' }]);
  });

  it('fails every run above the root ceiling and names the largest homes with their product', () => {
    const { parent } = qaParent();
    const g = sizedHome(parent, 'genright', '1', 3_000_000);
    const c = sizedHome(parent, 'cutright', '1', 2_000_000);
    sizedHome(parent, 'viewright', '1', 1_000_000);
    expect(() => checkQaRootLimits(parent, 'viewright', { capBytes: 0, ceilingBytes: 10_000_000 })).not.toThrow();
    expect(() => checkQaRootLimits(parent, 'viewright', { capBytes: 0, ceilingBytes: 0 })).not.toThrow();
    const error = overCap(() => checkQaRootLimits(parent, 'viewright', { capBytes: 0, ceilingBytes: 5_000_000 }));
    expect(error.detail.kind).toBe('root-ceiling');
    expect(error.detail.homeCount).toBe(3);
    expect(error.detail.totalBytes).toBeGreaterThanOrEqual(6_000_000);
    expect(error.detail.largest.map((h) => [h.path, h.product])).toEqual([[g, 'genright'], [c, 'cutright'], [expect.stringContaining('viewright'), 'viewright']]);
    expect(error.message).toContain('genright');
    expect(error.message).toContain('cutright');
    expect(error.message).toContain(ROOT_CEILING_ENV);
    expect(error.message).toContain('rightkit-qa clean');
    // A product over its own cap is told about its cap first.
    expect(overCap(() => checkQaRootLimits(parent, 'genright', { capBytes: 2_000_000, ceilingBytes: 5_000_000 })).detail.kind).toBe('product-cap');
    // Without a product only the ceiling applies.
    expect(() => checkQaRootLimits(parent, undefined, { capBytes: 1, ceilingBytes: 0 })).not.toThrow();
    expect(overCap(() => checkQaRootLimits(parent, undefined, { capBytes: 1, ceilingBytes: 5_000_000 })).detail.kind).toBe('root-ceiling');
  });

  it('lets limits set in code win over the environment, and the environment over the default', () => {
    const { parent } = qaParent();
    sizedHome(parent, 'genright', '1', 3_000_000);
    const tiny = { [ROOT_CAP_ENV]: '1', [ROOT_CEILING_ENV]: '1' };
    const run = (options: Partial<PreflightOptions>, env: NodeJS.ProcessEnv = tiny) => preflightQaRoot({ enabled: true, parent, app: 'genright', env, ...options });
    // The environment alone (1 byte) refuses.
    expect(() => run({})).toThrow(QaRootOverCapError);
    // A limit set in code wins over it, in either direction.
    expect(() => run({ capBytes: 10_000_000, ceilingBytes: 10_000_000 })).not.toThrow();
    expect(() => run({ capBytes: 0, ceilingBytes: 0 })).not.toThrow();
    expect(() => run({ capBytes: 1_000_000 }, { [ROOT_CAP_ENV]: '0', [ROOT_CEILING_ENV]: '0' })).toThrow(QaRootOverCapError);
    // One limit set in code leaves the other to the environment.
    expect(overCap(() => run({ capBytes: 0 })).detail.kind).toBe('root-ceiling');
    // Defaults when nothing says anything, or the environment is not a number.
    const defaults = { capBytes: DEFAULT_ROOT_CAP_BYTES, ceilingBytes: DEFAULT_ROOT_CEILING_BYTES };
    expect(defaults).toEqual({ capBytes: 4_000_000_000, ceilingBytes: 32_000_000_000 });
    expect(resolveQaRootLimits({}, {})).toEqual(defaults);
    expect(resolveQaRootLimits({}, { [ROOT_CAP_ENV]: 'lots', [ROOT_CEILING_ENV]: '-5' })).toEqual(defaults);
    expect(resolveQaRootLimits({ capBytes: 7 }, tiny)).toEqual({ capBytes: 7, ceilingBytes: 1 });
    // A value that is not a non-negative number is a mistake, not a silent default.
    expect(() => resolveQaRootLimits({ capBytes: -1 })).toThrow(/capBytes/);
    expect(() => resolveQaRootLimits({ ceilingBytes: Number.NaN })).toThrow(TypeError);
  });

  it('passes the limits given to createQaWorkspace on to the preflight', () => {
    const { parent } = qaParent();
    sizedHome(parent, 'postright', '1', 3_000_000);
    sizedHome(parent, 'genright', '1', 3_000_000);
    const base = fs.mkdtempSync(path.join(fs.realpathSync.native(os.tmpdir()), 'rkqa-lim-'));
    roots.push(base);
    const preflight = { enabled: true, parent, env: {} as NodeJS.ProcessEnv };
    // Other products' 6 MB is no business of viewright.
    const viewright = createQaWorkspace({ root: base, appKey: 'viewright', rootCapBytes: 1_000_000, preflight });
    roots.push(viewright.home);
    // postright is over a 1 MB cap, and the option beats preflight.capBytes.
    expect(() => createQaWorkspace({ root: base, appKey: 'postright', rootCapBytes: 1_000_000, preflight })).toThrow(QaRootOverCapError);
    expect(() => createQaWorkspace({ root: base, appKey: 'postright', rootCapBytes: 1_000_000, preflight: { ...preflight, capBytes: 0 } })).toThrow(QaRootOverCapError);
    // Lifted in code, it starts even though the environment says 1 byte.
    const lifted = createQaWorkspace({ root: base, appKey: 'postright', rootCapBytes: 0, preflight: { ...preflight, env: { [ROOT_CAP_ENV]: '1' } } });
    roots.push(lifted.home);
    // The ceiling option works the same way.
    expect(() => createQaWorkspace({ root: base, appKey: 'viewright', rootCapBytes: 0, rootCeilingBytes: 5_000_000, preflight })).toThrow(QaRootOverCapError);
  });

  it('matches the home names both runners write and no others', () => {
    expect(parseQaHomeName('genright-0123456789abcdef')).toBe('genright');
    expect(parseQaHomeName('scrape-right-0123456789abcdef')).toBe('scrape-right');
    expect(parseQaHomeName('viewright-aB3dE9')).toBe('viewright');
    for (const bad of ['stranger', 'precious-data', 'x-0123456789ABCDEF', 'x-0123456789abcde', '-0123456789abcdef', 'a-b-c', '.hidden-0123456789abcdef', 'waytoolonglabelname-0123456789abcdef', 'my dir-0123456789abcdef']) {
      expect(parseQaHomeName(bad), bad).toBeUndefined();
    }
    const base = fs.mkdtempSync(path.join(fs.realpathSync.native(os.tmpdir()), 'rkqa-name-'));
    roots.push(base);
    const ws = createQaWorkspace({ root: base, appKey: 'longappnamewithmorethan16chars' });
    roots.push(ws.home);
    expect(parseQaHomeName(path.basename(ws.home))).toBe('longappnamewithm');
  });

  it('takes a home product from its binding, then from its name', () => {
    expect(qaHomeProduct('cutright-0123456789abcdef', { app: 'genright' })).toBe('genright');
    expect(qaHomeProduct('cutright-0123456789abcdef')).toBe('cutright');
    expect(qaHomeProduct('cutright-0123456789abcdef', { app: '' })).toBe('cutright');
    expect(qaHomeProduct('stranger')).toBe('stranger');
  });

  describe('homes without an owner marker', () => {
    const isAlive = (pid: number) => pid === 222;

    it('sweeps a dead one, with or without a run record, and unblocks its product', () => {
      const { base, parent } = qaParent();
      // The run record names a dead owner but its lease still runs: that lease kept cancelled runs alive.
      const withRecord = unmarkedHome(base, parent, 'genright', '1', { ownerPid: 111, payload: 1_000_000 });
      const noRecord = unmarkedHome(base, parent, 'genright', '2', { payload: 1_000_000 });
      const noBinding = unmarkedHome(base, parent, 'cutright', '1', { binding: false, payload: 1_000_000 });
      for (const dir of [withRecord, noRecord, noBinding]) ageTree(dir, 3 * hours);
      const dry = cleanQaHomes({ parent, maxAgeMs: 2 * hours, dryRun: true, isAlive });
      expect(dry.removed).toHaveLength(3);
      for (const dir of [withRecord, noRecord, noBinding]) expect(fs.existsSync(dir), dir).toBe(true);
      // They count against genright until swept, and not after.
      expect(() => checkQaRootCap(parent, 'genright', 1_500_000)).toThrow(QaRootOverCapError);
      const report = cleanQaHomes({ parent, maxAgeMs: 2 * hours, isAlive });
      expect(report.removed.map((r) => r.path).sort()).toEqual([withRecord, noRecord, noBinding].sort());
      expect(report.removed.every((r) => r.reason.includes('no owner marker'))).toBe(true);
      for (const dir of [withRecord, noRecord, noBinding]) expect(fs.existsSync(dir), dir).toBe(false);
      expect(() => checkQaRootCap(parent, 'genright', 1_500_000)).not.toThrow();
    });

    it('keeps a young one, and one with a recently written file deep inside', () => {
      const { base, parent } = qaParent();
      const fresh = unmarkedHome(base, parent, 'genright', '1', { ownerPid: 111 });
      const busy = unmarkedHome(base, parent, 'genright', '2', { ownerPid: 111 });
      fs.mkdirSync(path.join(busy, 'data', 'deep'), { recursive: true });
      const log = path.join(busy, 'data', 'deep', 'app.log');
      fs.writeFileSync(log, 'x');
      ageTree(busy, 3 * hours);
      fs.writeFileSync(log, 'y');
      const report = cleanQaHomes({ parent, maxAgeMs: 2 * hours, isAlive });
      expect(report.removed).toEqual([]);
      for (const dir of [fresh, busy]) {
        expect(fs.existsSync(dir), dir).toBe(true);
        expect(report.retained.some((r) => r.path === dir && r.reason.includes('modified within the window')), dir).toBe(true);
      }
    });

    it('keeps one whose recorded pid is alive, and sweeps it once that pid is gone', () => {
      const { base, parent } = qaParent();
      const live = unmarkedHome(base, parent, 'genright', '1', { ownerPid: 222 });
      ageTree(live, 3 * hours);
      const kept = cleanQaHomes({ parent, maxAgeMs: 2 * hours, isAlive });
      expect(fs.existsSync(live)).toBe(true);
      expect(kept.retained.some((r) => r.path === live && r.reason.includes('is alive'))).toBe(true);
      cleanQaHomes({ parent, maxAgeMs: 2 * hours, isAlive: () => false });
      expect(fs.existsSync(live)).toBe(false);
      // A pid in a home marker that has no binding beside it counts the same way.
      const orphan = unmarkedHome(base, parent, 'cutright', '1', { binding: false });
      fs.writeFileSync(path.join(orphan, HOME_MARKER), JSON.stringify({ schemaVersion: 1, pid: 222, startedAtMs: 1, app: 'cutright', runId: 'x' }));
      ageTree(orphan, 3 * hours);
      cleanQaHomes({ parent, maxAgeMs: 2 * hours, isAlive });
      expect(fs.existsSync(orphan), 'live pid in the home marker').toBe(true);
      cleanQaHomes({ parent, maxAgeMs: 2 * hours, isAlive: () => false });
      expect(fs.existsSync(orphan), 'dead pid in the home marker').toBe(false);
    });

    it('keeps a directory that is not named like a home', () => {
      const { parent } = qaParent();
      const names = ['precious-data', 'stranger', 'x-0123456789ABCDEF'];
      for (const name of names) {
        const dir = path.join(parent, name);
        fs.mkdirSync(path.join(dir, 'sub'), { recursive: true });
        fs.writeFileSync(path.join(dir, 'sub', 'f'), 'keep');
        ageTree(dir, 3 * hours);
      }
      const report = cleanQaHomes({ parent, maxAgeMs: 2 * hours, isAlive: () => false });
      expect(report.removed).toEqual([]);
      for (const name of names) expect(fs.existsSync(path.join(parent, name, 'sub', 'f')), name).toBe(true);
      expect(report.retained.some((r) => r.path.endsWith('precious-data') && r.reason.includes('not a QA home name'))).toBe(true);
    });

    it.skipIf(process.platform === 'win32')('skips a symlink named like a home and never follows one out of the root', () => {
      const { base, parent } = qaParent();
      const outside = path.join(base, 'outside');
      fs.mkdirSync(path.join(outside, 'sub'), { recursive: true });
      fs.writeFileSync(path.join(outside, 'sub', 'precious'), 'keep');
      ageTree(outside, 3 * hours);
      const link = path.join(parent, 'genright-0000000000000001');
      fs.symlinkSync(outside, link);
      const doomed = unmarkedHome(base, parent, 'genright', '2', { ownerPid: 111 });
      fs.symlinkSync(outside, path.join(doomed, 'link-out'));
      // A zero window and a little slack: a link's own modification time cannot be aged here.
      const report = cleanQaHomes({ parent, maxAgeMs: 0, now: Date.now() + 1000, isAlive: () => false });
      expect(fs.existsSync(doomed), JSON.stringify(report)).toBe(false);
      expect(fs.readFileSync(path.join(outside, 'sub', 'precious'), 'utf8')).toBe('keep');
      expect(fs.lstatSync(link).isSymbolicLink()).toBe(true);
      expect(report.retained.some((r) => r.path === link && r.reason.includes('symlink'))).toBe(true);
    });
  });
});
