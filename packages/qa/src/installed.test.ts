import { describe, expect, it } from 'vitest';
import { InstalledAppRefused, assertNotInstalledApp, installedRootOf, installedRoots, isInstalledQaMode } from './installed.js';

const winEnv = { ProgramFiles: 'C:\\Program Files', 'ProgramFiles(x86)': 'C:\\Program Files (x86)', LOCALAPPDATA: 'C:\\Users\\a\\AppData\\Local' };

describe('installed app locations', () => {
  it('lists the macOS roots', () => {
    expect(installedRoots('darwin', {}, '/Users/a')).toEqual(['/Applications', '/Users/a/Applications']);
  });
  it('lists the Windows roots from the environment and skips unset or relative ones', () => {
    expect(installedRoots('win32', winEnv, 'C:\\Users\\a')).toEqual(['C:\\Program Files', 'C:\\Program Files (x86)', 'C:\\Users\\a\\AppData\\Local\\Programs']);
    expect(installedRoots('win32', { ProgramFiles: 'relative' }, '')).toEqual([]);
  });
  it('classifies macOS paths, including case and `..`', () => {
    const roots = installedRoots('darwin', {}, '/Users/a');
    expect(installedRootOf('/Applications/CutRight.app/Contents/MacOS/CutRight', roots, 'darwin')).toBe('/Applications');
    expect(installedRootOf('/applications/CutRight.app', roots, 'darwin')).toBe('/Applications');
    expect(installedRootOf('/Users/a/Applications/X.app', roots, 'darwin')).toBe('/Users/a/Applications');
    expect(installedRootOf('/Users/a/dev/../../../Applications/X.app', roots, 'darwin')).toBe('/Applications');
    expect(installedRootOf('/ApplicationsExtra/X.app', roots, 'darwin')).toBeUndefined();
    expect(installedRootOf('/Users/a/dev/target/debug/X.app', roots, 'darwin')).toBeUndefined();
  });
  it('classifies Windows paths, including case, separators, verbatim prefix and `..`', () => {
    const roots = installedRoots('win32', winEnv, 'C:\\Users\\a');
    expect(installedRootOf('C:\\Program Files\\CutRight\\cutright.exe', roots, 'win32')).toBe('C:\\Program Files');
    expect(installedRootOf('c:/program files (x86)/CutRight/x.exe', roots, 'win32')).toBe('C:\\Program Files (x86)');
    expect(installedRootOf('\\\\?\\C:\\Users\\a\\AppData\\Local\\Programs\\X\\x.exe', roots, 'win32')).toBe('C:\\Users\\a\\AppData\\Local\\Programs');
    expect(installedRootOf('C:\\work\\..\\Program Files\\X\\x.exe', roots, 'win32')).toBe('C:\\Program Files');
    expect(installedRootOf('C:\\Program Files Extra\\x.exe', roots, 'win32')).toBeUndefined();
    expect(installedRootOf('C:\\Users\\a\\AppData\\Local\\Temp\\x.exe', roots, 'win32')).toBeUndefined();
  });
});

describe('installed-QA mode', () => {
  it('reads the stage the release runner sets', () => {
    expect(isInstalledQaMode({ RIGHT_RELEASE_QA_STAGE: 'installed' })).toBe(true);
    expect(isInstalledQaMode({ RIGHT_RELEASE_QA_STAGE: 'source' })).toBe(false);
    expect(isInstalledQaMode({})).toBe(false);
  });
  it('refuses an installed path unless the mode is on, naming the path and the opt-in', () => {
    const options = { platform: 'darwin' as const, roots: ['/Applications'], canonicalize: (p: string) => p };
    expect(() => assertNotInstalledApp('/Applications/CutRight.app', { ...options, env: {} })).toThrow(InstalledAppRefused);
    expect(() => assertNotInstalledApp('/Applications/CutRight.app', { ...options, env: {} })).toThrow(/\/Applications\/CutRight\.app.*RIGHT_RELEASE_QA_STAGE=installed/);
    expect(assertNotInstalledApp('/Applications/CutRight.app', { ...options, env: { RIGHT_RELEASE_QA_STAGE: 'installed' } })).toBe('/Applications/CutRight.app');
    expect(assertNotInstalledApp('/tmp/dev/CutRight.app', { ...options, env: {} })).toBe('/tmp/dev/CutRight.app');
  });
});
