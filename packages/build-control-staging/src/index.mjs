import { isAbsolute, join, normalize } from 'node:path';

const requestId = /^req-[a-z0-9-]{8,64}$/;
export function createExternalStage({ externalRoot, requestId: id }) {
  if (!isAbsolute(externalRoot) || !requestId.test(id)) throw new TypeError('external absolute root & request ID are required');
  return Object.freeze({ requestId: id, stageRoot: join(normalize(externalRoot), 'stages', id), targetRoot: join(normalize(externalRoot), 'targets', id), transaction: 'PREPARED' });
}
export function prepareSyncBack({ stage, destination, sourceDigest }) {
  if (!stage?.stageRoot || !isAbsolute(destination) || !/^[a-f0-9]{64}$/.test(sourceDigest ?? '')) throw new TypeError('validated stage, destination & source digest are required');
  return Object.freeze({ ...stage, destination: normalize(destination), sourceDigest, transaction: 'PENDING_SYNC_BACK' });
}
