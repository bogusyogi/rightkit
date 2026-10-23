import { describe, expect, it } from 'vitest';
import * as suite from './suite-api.js';
import { RIGHT_SUITE_API_CONTRACT } from './suite-api.generated.js';
import contractJson from '../../../contracts/right-suite-api.v1.json' with { type: 'json' };

describe('suite-api contract projection', () => {
  it('matches the vendored contract exactly', () => {
    // The projection carries the contract minus its comment and the legacy hosts, which are served only by the gate.
    const { _comment, legacyHosts, ...projected } = contractJson;
    expect(_comment).toEqual(expect.any(String));
    expect(RIGHT_SUITE_API_CONTRACT).toEqual(projected);
    expect(suite.RIGHT_SUITE_API_HOST).toBe(contractJson.host);
    expect(suite.RIGHT_SUITE_API_ORIGIN).toBe(contractJson.origin);
    expect([...suite.RIGHT_SUITE_LEGACY_HOSTS]).toEqual(contractJson.legacyHosts);
  });
});

describe('suite-api helpers', () => {
  it('returns the exact release status URL', () => {
    expect(suite.releaseStatusUrl('viewright')).toBe('https://api.orthiclabs.com/v1/apps/viewright/releases/status.json');
  });

  it('returns the exact updater endpoint and keeps the Tauri target placeholder', () => {
    expect(suite.updaterEndpoint('mailright')).toBe('https://api.orthiclabs.com/v1/apps/mailright/releases/latest.json?platform={{target}}');
  });

  it('returns per-app artifact hosts as fresh arrays', () => {
    expect(suite.artifactHosts('viewright')).toEqual(['pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev']);
    expect(suite.artifactHosts('mailright')).toEqual([
      'rightapps-license-gate.adrdsouza.workers.dev',
      'pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev',
    ]);
    expect(suite.artifactHosts('scraperight')).toEqual([]);
    expect(suite.artifactHosts('constructor')).toEqual([]);
    const copy = suite.artifactHosts('viewright');
    copy.push('api.spoares.com');
    expect(suite.artifactHosts('viewright')).not.toContain('api.spoares.com');
  });

  it('returns the exact public installer URL and refuses traversal', () => {
    expect(suite.publicInstallerUrl('viewright', 'mac/current/ViewRight_universal.dmg')).toBe(
      'https://pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev/viewright/installers/mac/current/ViewRight_universal.dmg',
    );
    for (const bad of ['../secret', 'mac/../x', '/abs/x', 'mac//x', '', 'mac/current/a b.dmg', 'mac/current/']) {
      expect(() => suite.publicInstallerUrl('viewright', bad)).toThrow(TypeError);
    }
  });

  it('refuses app keys outside the contract key pattern', () => {
    for (const bad of ['', 'View Right', 'ViewRight', '1right', '../viewright', 'viewright/x', 'viewright?x=1', 'view_right', 'viewright.']) {
      expect(() => suite.releaseStatusUrl(bad)).toThrow(TypeError);
      expect(() => suite.updaterEndpoint(bad)).toThrow(TypeError);
      expect(() => suite.artifactHosts(bad)).toThrow(TypeError);
      expect(() => suite.publicInstallerUrl(bad, 'mac/current/x.dmg')).toThrow(TypeError);
    }
  });

  it('does not import Tauri or Node built-ins', async () => {
    const { readFileSync } = await import('node:fs');
    const source = readFileSync(new URL('./suite-api.ts', import.meta.url), 'utf8');
    expect(source).not.toMatch(/@tauri-apps|from ['"]node:|require\(/u);
  });
});
