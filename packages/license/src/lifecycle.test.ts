import { describe, expect, it } from 'vitest';
import {
  classifyLifecycle,
  isLicensed,
  lifecycleGrantsAccess,
  lifecycleNeedsOnlineRefresh,
  type Lifecycle,
  type StoredLicense,
} from './index.js';

const DAY = 86_400_000;
const NOW = Date.parse('2026-10-11T00:00:00.000Z');
const at = (offsetMs: number) => new Date(NOW + offsetMs).toISOString();

const base: StoredLicense = { status: 'active', deviceId: 'd', activationToken: 'tok' };
const trial = (validUntil: string | null): StoredLicense => ({
  ...base, validUntil, licenseKind: 'trial', entitlementStage: 'time_bound',
});
const term = (validUntil: string | null): StoredLicense => ({
  ...base, validUntil, licenseKind: 'term', entitlementStage: 'time_bound',
});
const provisional = (validUntil: string | null): StoredLicense => ({
  ...base, validUntil, licenseKind: 'lifetime', entitlementStage: 'provisional',
});
const final = (validUntil: string | null): StoredLicense => ({
  ...base, validUntil, licenseKind: 'lifetime', entitlementStage: 'final',
});
const legacy = (validUntil: string | null): StoredLicense => ({ ...base, validUntil });

describe('classifyLifecycle', () => {
  const table: Array<[string, StoredLicense | null, Lifecycle]> = [
    ['no record', null, 'unlicensed'],
    ['status none', { status: 'none', deviceId: 'd' }, 'unlicensed'],
    ['activation failed', { status: 'failed', deviceId: 'd', key: 'k' }, 'unlicensed'],
    ['key rejected at activation', { status: 'invalid', deviceId: 'd', key: 'k', message: 'nope' }, 'invalid'],
    ['revoked by refresh', { ...base, status: 'invalid', message: 'revoked' }, 'revoked'],
    ['legacy perpetual', legacy(null), 'active'],
    ['legacy live', legacy(at(DAY)), 'active'],
    ['legacy in grace', legacy(at(-2 * DAY)), 'grace'],
    ['legacy past grace', legacy(at(-8 * DAY)), 'expired'],
    ['legacy unparseable until', legacy('not-a-date'), 'active'],
    ['term live', term(at(DAY)), 'active'],
    ['term in grace', term(at(-DAY)), 'grace'],
    ['term expired', term(at(-30 * DAY)), 'expired'],
    ['trial live', trial(at(DAY)), 'trial'],
    ['trial at exact boundary', trial(at(0)), 'expired_trial'],
    ['trial one ms before', trial(at(1)), 'trial'],
    ['trial never graced', trial(at(-DAY)), 'expired_trial'],
    ['trial unparseable until', trial('not-a-date'), 'expired_trial'],
    ['provisional live', provisional(at(DAY)), 'lifetime_provisional'],
    ['provisional at due', provisional(at(0)), 'verification_required'],
    ['provisional never graced', provisional(at(-DAY)), 'verification_required'],
    ['final perpetual', final(null), 'lifetime'],
    ['final live', final(at(DAY)), 'lifetime'],
    ['final in grace', final(at(-DAY)), 'grace'],
    ['final expired', final(at(-30 * DAY)), 'expired'],
    ['kind without stage', { ...base, licenseKind: 'lifetime' }, 'invalid'],
    ['trial with lifetime stage', { ...base, licenseKind: 'trial', entitlementStage: 'final' }, 'invalid'],
  ];
  it.each(table)('%s', (_name, lic, expected) => {
    expect(classifyLifecycle(lic, { now: NOW })).toBe(expected);
  });

  it('honours graceDays', () => {
    expect(classifyLifecycle(legacy(at(-DAY)), { now: NOW, graceDays: 0 })).toBe('expired');
    expect(classifyLifecycle(legacy(at(-DAY)), { now: NOW, graceDays: 1 })).toBe('expired');
    expect(classifyLifecycle(legacy(at(-DAY + 1)), { now: NOW, graceDays: 1 })).toBe('grace');
  });

  it('flags a missing signed token only when asked to', () => {
    expect(classifyLifecycle(legacy(null), { now: NOW })).toBe('active');
    expect(classifyLifecycle(legacy(null), { now: NOW, requireSignedToken: true })).toBe('migration_required');
    expect(classifyLifecycle({ ...legacy(null), signedToken: 'a.b' }, { now: NOW, requireSignedToken: true })).toBe('active');
    expect(classifyLifecycle({ status: 'active', deviceId: 'd' }, { now: NOW, requireSignedToken: true })).toBe('invalid');
  });

  it('reports a native clock rollback for everything except a final lifetime license', () => {
    const rolled = { now: NOW, clockRolledBack: true };
    expect(classifyLifecycle(trial(at(DAY)), rolled)).toBe('clock_repair_required');
    expect(classifyLifecycle(term(at(DAY)), rolled)).toBe('clock_repair_required');
    expect(classifyLifecycle(provisional(at(DAY)), rolled)).toBe('clock_repair_required');
    expect(classifyLifecycle(final(null), rolled)).toBe('lifetime');
    // An inconsistent record stays invalid rather than asking for a clock repair.
    expect(classifyLifecycle({ ...base, licenseKind: 'trial' }, rolled)).toBe('invalid');
  });

  it('agrees with isLicensed for every consistent active record', () => {
    const records = [
      legacy(null), legacy(at(DAY)), legacy(at(-DAY)), legacy(at(-30 * DAY)), legacy('not-a-date'),
      term(at(DAY)), term(at(-DAY)), term(at(-30 * DAY)),
      trial(null), trial(at(DAY)), trial(at(0)), trial(at(-DAY)), trial('not-a-date'),
      provisional(at(DAY)), provisional(at(0)), provisional(at(-DAY)), provisional('not-a-date'),
      final(null), final(at(DAY)), final(at(-DAY)), final(at(-30 * DAY)),
    ];
    for (const lic of records) {
      for (const graceDays of [0, 7]) {
        const granted = lifecycleGrantsAccess(classifyLifecycle(lic, { now: NOW, graceDays }));
        expect(granted, JSON.stringify({ lic, graceDays })).toBe(isLicensed(lic, graceDays, NOW));
      }
    }
  });

  it('never grants a non-active record', () => {
    for (const status of ['none', 'invalid', 'failed'] as const) {
      expect(lifecycleGrantsAccess(classifyLifecycle({ ...base, status }, { now: NOW }))).toBe(false);
    }
  });
});

describe('lifecycle helpers', () => {
  it('grants access exactly for the entitled states', () => {
    const granted: Lifecycle[] = ['trial', 'active', 'grace', 'lifetime_provisional', 'lifetime'];
    const denied: Lifecycle[] = [
      'unlicensed', 'migration_required', 'expired_trial', 'expired',
      'verification_required', 'clock_repair_required', 'revoked', 'invalid',
    ];
    for (const l of granted) expect(lifecycleGrantsAccess(l), l).toBe(true);
    for (const l of denied) expect(lifecycleGrantsAccess(l), l).toBe(false);
  });

  it('names the states one online refresh can resolve', () => {
    for (const l of ['migration_required', 'verification_required', 'clock_repair_required'] as Lifecycle[]) {
      expect(lifecycleNeedsOnlineRefresh(l), l).toBe(true);
    }
    for (const l of ['trial', 'expired_trial', 'revoked', 'invalid', 'lifetime'] as Lifecycle[]) {
      expect(lifecycleNeedsOnlineRefresh(l), l).toBe(false);
    }
  });
});
