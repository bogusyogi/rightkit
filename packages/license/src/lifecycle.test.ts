import { describe, expect, it } from 'vitest';
import { hasFeature, isLicensed, isPro, satisfiesRequirement, tierAtLeast } from './index.js';
import { createLicenseMock } from './testing.js';
describe('native display helpers', () => {
  it('consumes native lifecycle without extending expiry locally', async () => {
    const view = await createLicenseMock().client.activate('key');
    view.validUntil = 0;
    expect(isLicensed(view)).toBe(true);
    view.lifecycle = 'expired'; view.status = 'expired';
    expect(isLicensed(view)).toBe(false);
  });
  it('fails closed for busy, unresolved, tombstone & disabled views', async () => {
    const view = await createLicenseMock().client.activate('key');
    for (const denied of [{ ...view, busy: true }, { ...view, unresolved: true }, { ...view, revisionState: 'tombstone' as const }, { ...view, mode: 'disabled' as const }]) {
      expect(isLicensed(denied)).toBe(false); expect(isPro(denied)).toBe(false);
      expect(satisfiesRequirement(denied, {})).toBe(false); expect(hasFeature(denied, 'export')).toBe(false);
    }
  });
  it('preserves tier & feature display requirements', async () => {
    const view = await createLicenseMock().client.activate('key'); view.features = ['export'];
    expect(tierAtLeast(view, 'plus')).toBe(true);
    expect(tierAtLeast(view, 'unknown')).toBe(false);
    expect(satisfiesRequirement(view, { tier: 'pro', feature: 'export' })).toBe(true);
    expect(satisfiesRequirement(view, { anyFeature: ['missing'] })).toBe(false);
  });
});
