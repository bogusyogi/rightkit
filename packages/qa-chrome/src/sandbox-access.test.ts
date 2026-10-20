import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  ALL_APPLICATION_PACKAGES,
  ALL_RESTRICTED_APPLICATION_PACKAGES,
  ChromeSandboxAccessError,
  grantSandboxAccess,
  missingSandboxGrants,
  sandboxAccessProblem,
  sandboxGrantCommand,
} from './sandbox-access.js';

const BOTH = [ALL_APPLICATION_PACKAGES, ALL_RESTRICTED_APPLICATION_PACKAGES];
// As Windows prints them: a file unpacked into a user folder, and the same file after the grant.
const UNPACKED = 'D:AI(A;ID;0x1301bf;;;S-1-5-21-1-2-3-1016)(A;ID;FA;;;S-1-5-21-1-2-3-1001)';
const GRANTED = `D:AI(A;ID;0x1200a9;;;S-1-15-2-2)(A;ID;0x1200a9;;;AC)${UNPACKED.slice(4)}`;

describe('Chrome sandbox access on Windows', () => {
  it('reads the access list: both grants, one, partial, inherit-only, deny', () => {
    expect(missingSandboxGrants(UNPACKED)).toEqual(BOTH);
    expect(missingSandboxGrants('')).toEqual(BOTH);
    expect(missingSandboxGrants(GRANTED)).toEqual([]);
    expect(missingSandboxGrants('D:(A;;GRGX;;;S-1-15-2-1)(A;;FA;;;S-1-15-2-2)')).toEqual([]);
    expect(missingSandboxGrants('D:(A;;0x80000000;;;AC)(A;;0x20000000;;;AC)(A;;0x10000000;;;S-1-15-2-2)')).toEqual([]);
    expect(missingSandboxGrants('D:(A;;0x1200a9;;;AC)')).toEqual([ALL_RESTRICTED_APPLICATION_PACKAGES]);
    expect(missingSandboxGrants('D:(A;;FR;;;AC)(A;;0x1200a9;;;S-1-15-2-2)')).toEqual([ALL_APPLICATION_PACKAGES]);
    expect(missingSandboxGrants('D:(A;OICIIO;GRGX;;;AC)(A;OICIIO;GRGX;;;S-1-15-2-2)')).toEqual(BOTH);
    expect(missingSandboxGrants('D:(D;;0x1200a9;;;AC)(D;;0x1200a9;;;S-1-15-2-2)')).toEqual(BOTH);
  });

  it('the launcher check names the folder, the principals and the fix', () => {
    const exe = 'C:\\tools\\cft\\1.2.3.4\\chrome-win64\\chrome.exe';
    const problem = sandboxAccessProblem(exe, { platform: 'win32', readAccessList: () => UNPACKED });
    expect(problem).toBeInstanceOf(ChromeSandboxAccessError);
    expect(problem?.code).toBe('CHROME_SANDBOX_ACCESS');
    expect(problem?.directory).toBe('C:\\tools\\cft\\1.2.3.4\\chrome-win64');
    expect(problem?.principals).toEqual(BOTH);
    expect(problem?.fix).toBe('icacls "C:\\tools\\cft\\1.2.3.4\\chrome-win64" /grant "*S-1-15-2-1:(OI)(CI)(RX)" "*S-1-15-2-2:(OI)(CI)(RX)" /T');
    expect(problem?.fix).toBe(sandboxGrantCommand('C:\\tools\\cft\\1.2.3.4\\chrome-win64'));
    expect(problem?.message).toContain('C:\\tools\\cft\\1.2.3.4\\chrome-win64');

    expect(sandboxAccessProblem(exe, { platform: 'win32', readAccessList: () => GRANTED })).toBeUndefined();
    // An unreadable access list does not stop a launch; other systems are never checked.
    expect(sandboxAccessProblem(exe, { platform: 'win32', readAccessList: () => undefined })).toBeUndefined();
    expect(sandboxAccessProblem('/x/chrome', { platform: 'darwin', readAccessList: () => { throw new Error('must not read'); } })).toBeUndefined();
  });

  it('the installer grant runs icacls, then checks; a failed or absent grant throws', () => {
    const calls: string[][] = [];
    const run = (_file: string, args: string[]) => { calls.push(args); return { status: 0 }; };
    expect(grantSandboxAccess('C:\\v', 'C:\\v\\chrome.exe', { platform: 'win32', run, readAccessList: () => GRANTED })).toBe(true);
    expect(calls[0]).toEqual(['C:\\v', '/grant', '*S-1-15-2-1:(OI)(CI)(RX)', '*S-1-15-2-2:(OI)(CI)(RX)', '/T', '/C', '/Q']);
    expect(() => grantSandboxAccess('C:\\v', 'C:\\v\\chrome.exe', { platform: 'win32', run: () => ({ status: 5 }), readAccessList: () => GRANTED })).toThrow(/icacls exit 5/);
    expect(() => grantSandboxAccess('C:\\v', 'C:\\v\\chrome.exe', { platform: 'win32', run, readAccessList: () => UNPACKED })).toThrow(ChromeSandboxAccessError);
    expect(() => grantSandboxAccess('C:\\v', 'C:\\v\\chrome.exe', { platform: 'win32', run, readAccessList: () => undefined })).toThrow(/could not be read/);
    expect(grantSandboxAccess('/v', '/v/chrome', { platform: 'darwin', run: () => { throw new Error('must not run'); } })).toBe(false);
  });

  // The real thing, on Windows: a fresh folder has no grant; after the installer's grant it has both.
  if (process.platform === 'win32') it('a fresh folder lacks the grant until the installer grants it', () => {
    const dir = mkdtempSync(path.join(tmpdir(), 'rk-sbx-'));
    try {
      const exe = path.join(dir, 'chrome.exe');
      writeFileSync(exe, 'not a browser');
      const before = sandboxAccessProblem(exe);
      expect(before?.directory).toBe(dir);
      expect(before?.principals).toEqual(BOTH);
      expect(grantSandboxAccess(dir, exe)).toBe(true);
      expect(sandboxAccessProblem(exe)).toBeUndefined();
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
