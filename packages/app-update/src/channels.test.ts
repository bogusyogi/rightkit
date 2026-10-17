import { describe, expect, it } from 'vitest';
import { resolveChannel, validateChannels } from './channels.js';

const channels = {
  stable: { endpoints: ['https://updates.example.com/stable.json'] },
  nightly: { endpoints: ['https://updates.example.com/nightly.json'] },
};

describe('update channels', () => {
  it('resolves default and named channels', () => {
    expect(resolveChannel(channels, null, { defaultChannel: 'stable' }).channel).toBe('stable');
    expect(resolveChannel(channels, 'nightly', { defaultChannel: 'stable' }).endpoints[0]).toContain('nightly');
  });
  it('throws on unknown channel unless fallback is allowed', () => {
    expect(() => resolveChannel(channels, 'beta', { defaultChannel: 'stable' })).toThrow(/unknown channel/);
    expect(resolveChannel(channels, 'beta', { defaultChannel: 'stable', allowFallback: true }).channel).toBe('stable');
  });
  it('rejects bad names, empty endpoints and insecure endpoints', () => {
    expect(() => validateChannels({})).toThrow(/at least one channel/);
    expect(() => validateChannels({ 'Bad Name': channels.stable })).toThrow(/invalid channel name/);
    expect(() => validateChannels({ a: { endpoints: [] } })).toThrow(/at least one endpoint/);
    expect(() => validateChannels({ a: { endpoints: ['http://example.com/x'] } })).toThrow(/https/);
    expect(() => validateChannels({ a: { endpoints: ['not a url'] } })).toThrow(/invalid endpoint/);
  });
  it('accepts a GitHub Releases latest.json endpoint', () => {
    const github = { stable: { endpoints: ['https://github.com/Orthic-Labs/ember/releases/latest/download/latest.json'] } };
    expect(() => validateChannels(github)).not.toThrow();
    expect(resolveChannel(github, null, { defaultChannel: 'stable' }).endpoints[0]).toMatch(/releases\/latest\/download\/latest\.json$/);
  });
  it('allows loopback http for local testing', () => {
    expect(() => validateChannels({ a: { endpoints: ['http://localhost:8080/x'] } })).not.toThrow();
  });
  it('rejects an undefined default channel', () => {
    expect(() => resolveChannel(channels, null, { defaultChannel: 'x' })).toThrow(/default channel/);
  });
});
