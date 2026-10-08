import assert from 'node:assert/strict';
import test from 'node:test';
import { discoverManagedVolume } from './volume-discovery.mjs';

test('Windows volume discovery resolves containing volume with Get-Volume FilePath', async () => {
  let command;
  const volume = await discoverManagedVolume('D:\\managed\\broker', {
    platform: 'win32',
    realpath: (value) => value,
    statfs: () => ({ blocks: 1_000, bsize: 1024 }),
    run: async (_file, args) => { command = args.at(-1); return { stdout: '\\\\?\\Volume{ABC-123}\\\r\n' }; }
  });
  assert.match(command, /Get-Volume -FilePath 'D:\\managed\\broker'/);
  assert.equal(volume.volumeId, '\\\\?\\Volume{ABC-123}\\');
  assert.equal(volume.capacityBytes, 1_024_000);
});
