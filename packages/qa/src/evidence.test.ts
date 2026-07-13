import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  addEvidenceCheck,
  createEvidenceBundle,
  finishEvidenceBundle,
  writeEvidenceBundle,
} from './evidence.js';

describe('QA evidence bundle', () => {
  it('writes a stable, machine-readable bundle atomically', () => {
    const bundle = createEvidenceBundle({
      app: 'coderight',
      platform: 'win32',
      runId: 'run-1',
      startedAt: '2026-07-13T10:00:00.000Z',
    });
    addEvidenceCheck(bundle, {
      name: 'native.health',
      status: 'passed',
      details: { endpoint: '/health' },
    });
    finishEvidenceBundle(bundle, {
      status: 'passed',
      finishedAt: '2026-07-13T10:00:01.000Z',
    });

    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'rightkit-evidence-'));
    const output = path.join(root, 'nested', 'evidence.json');
    writeEvidenceBundle(output, bundle);

    expect(JSON.parse(fs.readFileSync(output, 'utf8'))).toEqual({
      schemaVersion: 1,
      app: 'coderight',
      platform: 'win32',
      runId: 'run-1',
      startedAt: '2026-07-13T10:00:00.000Z',
      finishedAt: '2026-07-13T10:00:01.000Z',
      status: 'passed',
      checks: [
        {
          name: 'native.health',
          status: 'passed',
          details: { endpoint: '/health' },
        },
      ],
    });
    expect(fs.readdirSync(path.dirname(output))).toEqual(['evidence.json']);
  });
});
