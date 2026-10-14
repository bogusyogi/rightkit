import { describe, expect, it } from 'vitest';
import { compareVersions, isNewerVersion, parseVersion } from './version.js';

describe('version compare', () => {
  it('orders core versions numerically', () => {
    expect(compareVersions('1.10.0', '1.9.9')).toBe(1);
    expect(compareVersions('1.2.3', '1.2.3')).toBe(0);
    expect(compareVersions('0.9.0', '1.0.0')).toBe(-1);
  });
  it('treats prerelease as lower than release and orders identifiers', () => {
    expect(compareVersions('1.0.0-beta.2', '1.0.0')).toBe(-1);
    expect(compareVersions('1.0.0-beta.2', '1.0.0-beta.10')).toBe(-1);
    expect(compareVersions('1.0.0-alpha', '1.0.0-1')).toBe(1);
    expect(compareVersions('1.0.0-alpha', '1.0.0-alpha.1')).toBe(-1);
  });
  it('accepts v prefix and ignores build metadata', () => {
    expect(compareVersions('v1.2.3+abc', '1.2.3')).toBe(0);
  });
  it('rejects invalid input', () => {
    expect(parseVersion('1.2')).toBeNull();
    expect(() => compareVersions('x', '1.0.0')).toThrow(/invalid version/);
  });
  it('isNewerVersion is strict', () => {
    expect(isNewerVersion('1.0.1', '1.0.0')).toBe(true);
    expect(isNewerVersion('1.0.0', '1.0.0')).toBe(false);
  });
});
