import test from 'node:test';
import assert from 'node:assert/strict';
import { join, resolve } from 'node:path';
import { createExternalStage, prepareSyncBack } from './index.mjs';

test('staging interface only describes external transactional roots', () => {
  const root = resolve('/Volumes/builds'); const destination = resolve('/workspace');
  const stage = createExternalStage({ externalRoot: root, requestId: 'req-12345678' });
  assert.equal(stage.stageRoot, join(root, 'stages', 'req-12345678'));
  assert.equal(prepareSyncBack({ stage, destination, sourceDigest: 'b'.repeat(64) }).transaction, 'PENDING_SYNC_BACK');
  assert.throws(() => createExternalStage({ externalRoot: 'relative', requestId: 'req-12345678' }));
});
