import { describe, expect, it } from 'vitest';
import { createLicenseMock, RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP } from './testing.js';
describe('testing-only native command mock', () => {
  it('refreshes & deactivates in memory', async () => {
    const { client } = createLicenseMock();
    expect((await client.refresh()).outcome).toBe('noCredential');
    await client.activate('key'); expect((await client.refresh()).outcome).toBe('refreshed');
    expect((await client.deactivate()).revisionState).toBe('tombstone');
    expect((await client.refresh()).outcome).toBe('noCredential');
  });
  it('exports mandatory sentinel & rejects alternate commands', async () => {
    const mock = createLicenseMock(); expect(mock.sentinel).toBe(RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP);
    await expect(mock.bridge.invoke('fetch', { url: 'https://example.test' })).rejects.toMatchObject({ code: 'unsupported' });
  });
});
