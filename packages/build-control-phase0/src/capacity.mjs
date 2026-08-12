import { statfsSync } from 'node:fs';
import { tmpdir } from 'node:os';

export function capacity() {
  const fs = statfsSync(tmpdir()); const block = Number(fs.bsize);
  return { schemaVersion: '1.0.0', generatedAt: new Date().toISOString(), mountProbe: tmpdir(), protectedNonCacheClasses: ['broker-state', 'receipts', 'policy', 'toolchains', 'staging', 'request-targets', 'sync-back'], filesystem: { blockSize: block, totalBytes: Number(fs.blocks) * block, freeBytes: Number(fs.bfree) * block, availableBytes: Number(fs.bavail) * block }, assumptions: { requestReservations: 0, cacheBytes: 0, reserveFloorBytes: 0 }, status: 'MEASURED_BASELINE_REQUIRES_PRODUCT_RESERVATIONS' };
}
