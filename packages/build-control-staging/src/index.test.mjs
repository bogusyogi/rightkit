import test from 'node:test';
import assert from 'node:assert/strict';
import { createExternalStage, prepareSyncBack } from './index.mjs';

test('staging interface only describes external transactional roots', () => {
  const stage = createExternalStage({ externalRoot: '/Volumes/builds', requestId: 'req-12345678' });
  assert.equal(stage.stageRoot, '/Volumes/builds/stages/req-12345678');
  assert.equal(prepareSyncBack({ stage, destination: '/workspace', sourceDigest: 'b'.repeat(64) }).transaction, 'PENDING_SYNC_BACK');
  assert.throws(() => createExternalStage({ externalRoot: 'relative', requestId: 'req-12345678' }));
});
