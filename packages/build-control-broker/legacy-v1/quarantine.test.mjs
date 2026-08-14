import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import test from 'node:test';
import { readdir, readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const directory = dirname(fileURLToPath(import.meta.url));
const archive = Object.freeze({
  'v1-broker-source.mjs.txt': '6a7c4c97bdba35b285e8fac97277c22aa49a90fe23d2316396e0ea85265fc0d2',
  'v1-broker-tests.mjs.txt': '73d9de327d15ea85fb7b20e28686b2e3a7701613a4fd86b09ea2293a9b9018b1'
});

test('legacy v1 remains static, integrity-pinned forensic evidence', async () => {
  const files = await readdir(directory);
  assert.deepEqual(files.filter((file) => file.endsWith('.mjs') && file !== 'quarantine.test.mjs'), []);
  for (const [name, expectedDigest] of Object.entries(archive)) {
    const contents = await readFile(join(directory, name));
    assert.equal(createHash('sha256').update(contents).digest('hex'), expectedDigest, `${name} must remain intact`);
  }
});
