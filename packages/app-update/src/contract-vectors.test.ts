import { describe, expect, it } from 'vitest';
import { UPDATE_HOST_CONTRACT_VECTORS as fromEntry } from './index.js';
import { UPDATE_HOST_CONTRACT_VECTORS as fromSubpath } from './contract-vectors.js';

describe('contract vectors subpath', () => {
  it('is the same data as the main-entry re-export', () => {
    expect(fromSubpath).toBe(fromEntry);
    expect(fromSubpath).toHaveLength(4);
  });
});
