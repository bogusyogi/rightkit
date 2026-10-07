import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import {
  addEvidenceArtifact,
  addEvidenceCheck,
  createEvidenceBundle,
  finishEvidenceBundle,
  verifyEvidenceArtifacts,
  writeEvidenceBundle,
} from './evidence.js';

const roots: string[] = [];

function tempRoot(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'rightkit-evidence-'));
  roots.push(root);
  return root;
}

afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

describe('QA evidence bundle', () => {
  it('writes a stable, machine-readable bundle atomically', () => {
    const bundle = createEvidenceBundle({
      app: 'coderight',
      platform: 'win32',
      runId: 'run-1',
      startedAt: '2026-07-13T10:00:00.000Z',
    });
    addEvidenceCheck(bundle, { name: 'app.health', status: 'passed', details: { endpoint: '/health' } });
    finishEvidenceBundle(bundle, { status: 'passed', finishedAt: '2026-07-13T10:00:01.000Z' });

    const output = path.join(tempRoot(), 'nested', 'evidence.json');
    writeEvidenceBundle(output, bundle);

    expect(JSON.parse(fs.readFileSync(output, 'utf8'))).toEqual({
      schemaVersion: 2,
      app: 'coderight',
      platform: 'win32',
      runId: 'run-1',
      startedAt: '2026-07-13T10:00:00.000Z',
      finishedAt: '2026-07-13T10:00:01.000Z',
      status: 'passed',
      checks: [{ name: 'app.health', status: 'passed', details: { endpoint: '/health' } }],
      artifacts: [],
    });
    expect(fs.readdirSync(path.dirname(output))).toEqual(['evidence.json']);
  });

  it('hashes an artifact when it is recorded, not when the bundle is written', () => {
    const root = tempRoot();
    const screenshot = path.join(root, 'screenshot.png');
    fs.writeFileSync(screenshot, 'original-pixels');

    const bundle = createEvidenceBundle({ app: 'viewright', platform: 'win32', runId: 'run-2' });
    const artifact = addEvidenceArtifact(bundle, { name: 'screenshot', path: screenshot });

    expect(artifact.sha256).toMatch(/^[0-9a-f]{64}$/);
    expect(artifact.sizeBytes).toBe('original-pixels'.length);

    fs.writeFileSync(screenshot, 'replaced-after-the-fact');
    expect(verifyEvidenceArtifacts(bundle)).toEqual([
      { name: 'screenshot', reason: expect.stringMatching(/does not match recorded/) },
    ]);
  });

  it('refuses a check that references evidence nobody hashed', () => {
    const bundle = createEvidenceBundle({ app: 'viewright', platform: 'win32', runId: 'run-3' });
    expect(() => addEvidenceCheck(bundle, { name: 'signature.verified', status: 'passed', artifacts: ['installer.exe'] }))
      .toThrow(/references artifacts that were never hashed: installer\.exe.*addEvidenceArtifact/s);
  });

  it('refuses to write a passing receipt whose evidence changed underneath it', () => {
    const root = tempRoot();
    const log = path.join(root, 'driver.log');
    fs.writeFileSync(log, 'session ok');

    const bundle = createEvidenceBundle({ app: 'viewright', platform: 'win32', runId: 'run-4' });
    addEvidenceArtifact(bundle, { name: 'driver.log', path: log });
    addEvidenceCheck(bundle, { name: 'app.run', status: 'passed', artifacts: ['driver.log'] });
    finishEvidenceBundle(bundle, { status: 'passed' });

    fs.writeFileSync(log, 'session ok (edited)');
    expect(() => writeEvidenceBundle(path.join(root, 'evidence.json'), bundle))
      .toThrow(/refusing to write a passing QA receipt whose evidence changed.*driver\.log/s);
  });

  it('still writes a failing receipt so the failure is not lost with its evidence', () => {
    const root = tempRoot();
    const log = path.join(root, 'driver.log');
    fs.writeFileSync(log, 'crashed');

    const bundle = createEvidenceBundle({ app: 'viewright', platform: 'win32', runId: 'run-5' });
    addEvidenceArtifact(bundle, { name: 'driver.log', path: log });
    addEvidenceCheck(bundle, { name: 'app.run', status: 'failed', artifacts: ['driver.log'] });
    finishEvidenceBundle(bundle, { status: 'failed' });
    fs.rmSync(log);

    const output = path.join(root, 'evidence.json');
    writeEvidenceBundle(output, bundle);
    expect(JSON.parse(fs.readFileSync(output, 'utf8')).status).toBe('failed');
  });

  it('refuses an unreadable artifact rather than recording an empty hash', () => {
    const bundle = createEvidenceBundle({ app: 'viewright', platform: 'win32', runId: 'run-6' });
    expect(() => addEvidenceArtifact(bundle, { name: 'missing', path: path.join(tempRoot(), 'nope.png') }))
      .toThrow(/cannot record QA evidence artifact missing/);
  });

  it('rejects duplicate artifact names that would make a reference ambiguous', () => {
    const root = tempRoot();
    const first = path.join(root, 'a.png');
    const second = path.join(root, 'b.png');
    fs.writeFileSync(first, 'a');
    fs.writeFileSync(second, 'b');

    const bundle = createEvidenceBundle({ app: 'viewright', platform: 'win32', runId: 'run-7' });
    addEvidenceArtifact(bundle, { name: 'shot', path: first });
    expect(() => addEvidenceArtifact(bundle, { name: 'shot', path: second })).toThrow(/already records an artifact named shot/);
  });

  it('will not accept new evidence after the run is finished', () => {
    const bundle = createEvidenceBundle({ app: 'viewright', platform: 'win32', runId: 'run-8' });
    finishEvidenceBundle(bundle, { status: 'passed' });
    expect(() => addEvidenceCheck(bundle, { name: 'late', status: 'passed' })).toThrow(/after the run has finished/);
  });
});
