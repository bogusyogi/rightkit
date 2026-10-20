import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { createQaWorkspace, disposeQaWorkspace } from './workspace.js';
import { QaLaunchGuard, UserDataViolation, assertNoPermissionDialog, assertQaLaunchSafe, isSystemDialogOwner, permissionDialogIn, seedFrom, spawnQaApp, type UserDataRule } from './userdata.js';

const cleanups: Array<() => void> = [];
afterEach(() => { for (const fn of cleanups.splice(0)) fn(); });

function fixture() {
  const root = fs.mkdtempSync(path.join(fs.realpathSync.native(os.tmpdir()), 'rkqa-ud-'));
  const fakeHome = path.join(root, 'fakehome');
  for (const d of ['Documents', 'Desktop', 'Downloads', 'Library/Mobile Documents/com~apple~CloudDocs', 'Library/CloudStorage/OneDrive-Acme', 'Library/Application Support/ViewRight/db', 'AppData/Roaming/ViewRight', 'Dropbox', 'Elsewhere']) {
    fs.mkdirSync(path.join(fakeHome, d), { recursive: true });
  }
  const workspace = createQaWorkspace({ root: path.join(root, 'managed'), appKey: 'viewright' });
  cleanups.push(() => { disposeQaWorkspace(workspace); fs.rmSync(root, { recursive: true, force: true }); });
  return { root, fakeHome, workspace, guard: QaLaunchGuard.forWorkspace(workspace, { userHomes: [fakeHome] }) };
}

function ruleOf(fn: () => unknown): UserDataRule {
  try { fn(); } catch (error) {
    expect(error).toBeInstanceOf(UserDataViolation);
    return (error as UserDataViolation).rule;
  }
  throw new Error('expected a UserDataViolation');
}

function linkDirectory(target: string, link: string): void {
  fs.symlinkSync(target, link, process.platform === 'win32' ? 'junction' : 'dir');
}

describe('QA runs never touch real user data', () => {
  it('refuses every forbidden location with its rule', () => {
    const { fakeHome: h, guard } = fixture();
    const cases: Array<[string, UserDataRule]> = [
      [path.join(h, 'Documents'), 'user-folder'],
      [path.join(h, 'Documents/sub/dir'), 'user-folder'],
      [path.join(h, 'Desktop'), 'user-folder'],
      [path.join(h, 'Downloads'), 'user-folder'],
      [h, 'user-folder'],
      [path.join(h, 'Library/Mobile Documents/com~apple~CloudDocs'), 'cloud-storage'],
      [path.join(h, 'Library/CloudStorage/OneDrive-Acme'), 'cloud-storage'],
      [path.join(h, 'Dropbox'), 'cloud-storage'],
      [path.join(h, 'Library/Application Support/ViewRight/db'), 'real-app-data'],
      [path.join(h, 'AppData/Roaming/ViewRight'), 'real-app-data'],
      [path.join(h, 'Elsewhere'), 'outside-qa-root'],
    ];
    for (const [p, rule] of cases) expect(ruleOf(() => guard.checkDir(p)), p).toBe(rule);
    const error = (() => { try { guard.checkDir(path.join(h, 'Documents')); } catch (e) { return e as UserDataViolation; } })();
    expect(error?.message).toContain(path.join(h, 'Documents'));
    expect(error?.message).toContain('user-folder');
  });

  if (process.platform !== 'win32') it('refuses external volumes and other accounts', () => {
    const { guard } = fixture();
    expect(ruleOf(() => guard.checkDir('/Volumes/Backup/qa-home'))).toBe('external-volume');
    expect(ruleOf(() => guard.checkDir('/Users/someone-else/Documents/x'))).toBe('user-folder');
    expect(ruleOf(() => guard.checkDir('/Users/someone-else/Library/Application Support/ViewRight'))).toBe('real-app-data');
  });

  it('refuses relative paths on every host', () => {
    const { guard } = fixture();
    expect(ruleOf(() => guard.checkDir('relative/path'))).toBe('unresolvable');
  });

  it('accepts a created QA root and its directories', () => {
    const { guard, workspace } = fixture();
    for (const dir of [workspace.home, workspace.dataDir, path.join(workspace.home, 'tmp'), path.join(workspace.home, 'not/created/yet')]) guard.checkDir(dir);
    expect(() => guard.checkEnv(workspace.env)).not.toThrow();
    expect(() => assertQaLaunchSafe(workspace, { userHomes: [] })).not.toThrow();
  });

  it('does not treat an arbitrary directory as a QA root', () => {
    const { root } = fixture();
    expect(ruleOf(() => QaLaunchGuard.forWorkspace({ home: path.join(root, 'fakehome/Elsewhere') }))).toBe('not-a-qa-root');
    expect(ruleOf(() => QaLaunchGuard.forWorkspace({ home: root }))).toBe('not-a-qa-root');
  });

  it('refuses a symlink out of the QA root', () => {
    const { fakeHome, guard, workspace } = fixture();
    const intoDocuments = path.join(workspace.home, 'looks-internal');
    linkDirectory(path.join(fakeHome, 'Documents'), intoDocuments);
    expect(ruleOf(() => guard.checkDir(intoDocuments))).toBe('user-folder');
    expect(ruleOf(() => guard.checkDir(path.join(intoDocuments, 'deeper/file')))).toBe('user-folder');
    const plain = path.join(workspace.home, 'to-plain');
    linkDirectory(path.join(fakeHome, 'Elsewhere'), plain);
    expect(ruleOf(() => guard.checkDir(plain))).toBe('outside-qa-root');
  });

  it('refuses an environment path or working directory outside the root, and spawns nothing', () => {
    const { fakeHome, workspace } = fixture();
    const real = path.join(fakeHome, 'Library/Application Support/ViewRight');
    expect(ruleOf(() => assertQaLaunchSafe(workspace, { userHomes: [fakeHome], env: { VIEWRIGHT_DATA_DIR: real } }))).toBe('real-app-data');
    expect(ruleOf(() => assertQaLaunchSafe(workspace, { userHomes: [fakeHome], cwd: path.join(fakeHome, 'Documents') }))).toBe('user-folder');
    expect(ruleOf(() => spawnQaApp(workspace, process.execPath, ['-e', '0'], { userHomes: [fakeHome], env: { HOME: path.join(fakeHome, 'Desktop') } }))).toBe('user-folder');
  });

  it('seedFrom refuses real app data without a verified scrub', async () => {
    const { fakeHome, workspace } = fixture();
    const real = path.join(fakeHome, 'Library/Application Support/ViewRight/db');
    fs.writeFileSync(path.join(real, 'app.sqlite'), 'user rows');
    const copy = (from: string, to: string) => { fs.mkdirSync(to, { recursive: true }); fs.copyFileSync(path.join(from, 'app.sqlite'), path.join(to, 'app.sqlite')); };
    const base = { userHomes: [fakeHome], destRel: 'seed', transform: copy };
    const dest = path.join(workspace.dataDir, 'seed');
    await expect(seedFrom(workspace, real, base)).rejects.toMatchObject({ rule: 'unscrubbed-real-data' });
    expect(fs.existsSync(dest)).toBe(false);
    await expect(seedFrom(workspace, real, { ...base, scrub: { declaredScrubbed: false, verify: () => true } })).rejects.toMatchObject({ rule: 'unscrubbed-real-data' });
    await expect(seedFrom(workspace, real, { ...base, scrub: { declaredScrubbed: true, verify: () => false } })).rejects.toMatchObject({ rule: 'unscrubbed-real-data' });
    expect(fs.existsSync(dest)).toBe(false);
    const out = await seedFrom(workspace, real, { ...base, scrub: { declaredScrubbed: true, verify: (d) => { fs.writeFileSync(path.join(d, 'app.sqlite'), ''); return true; } } });
    expect(fs.readFileSync(path.join(out, 'app.sqlite'), 'utf8')).toBe('');
    // A generated fixture needs no scrub.
    const generated = path.join(workspace.dataDir, 'generated');
    fs.mkdirSync(generated);
    fs.writeFileSync(path.join(generated, 'fixture.txt'), 'fixture');
    const ok = await seedFrom(workspace, generated, { userHomes: [fakeHome], destRel: 'seed2', transform: (f, t) => { fs.mkdirSync(t, { recursive: true }); fs.copyFileSync(path.join(f, 'fixture.txt'), path.join(t, 'fixture.txt')); } });
    expect(fs.existsSync(path.join(ok, 'fixture.txt'))).toBe(true);
    await expect(seedFrom(workspace, generated, { ...base, destRel: '../x' })).rejects.toThrow(/relative/);
  });

  it('matches permission-dialog owners and treats the live check as unsupported', () => {
    expect(isSystemDialogOwner('UserNotificationCenter')).toBe(true);
    expect(isSystemDialogOwner('Finder')).toBe(false);
    expect(permissionDialogIn(['Finder', 'CoreServicesUIAgent'])).toBe('CoreServicesUIAgent');
    expect(() => assertNoPermissionDialog(1, ['Finder'])).not.toThrow();
    expect(() => assertNoPermissionDialog(1, ['CoreServicesUIAgent'])).toThrow(/permission dialog/);
  });
});
