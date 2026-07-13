import fs from 'node:fs';
import path from 'node:path';

export type EvidenceCheckStatus = 'passed' | 'failed' | 'skipped';
export type EvidenceRunStatus = 'running' | 'passed' | 'failed';

export interface EvidenceCheck {
  name: string;
  status: EvidenceCheckStatus;
  details?: Record<string, unknown>;
}

export interface EvidenceBundle {
  schemaVersion: 1;
  app: string;
  platform: string;
  runId: string;
  startedAt: string;
  finishedAt?: string;
  status: EvidenceRunStatus;
  checks: EvidenceCheck[];
}

export interface EvidenceBundleInput {
  app: string;
  platform: string;
  runId: string;
  startedAt?: string;
}

export function createEvidenceBundle(input: EvidenceBundleInput): EvidenceBundle {
  return {
    schemaVersion: 1,
    app: requireText(input.app, 'app'),
    platform: requireText(input.platform, 'platform'),
    runId: requireText(input.runId, 'runId'),
    startedAt: input.startedAt ?? new Date().toISOString(),
    status: 'running',
    checks: [],
  };
}

export function addEvidenceCheck(bundle: EvidenceBundle, check: EvidenceCheck): void {
  if (bundle.status !== 'running') {
    throw new Error('cannot add a QA evidence check after the run has finished');
  }
  bundle.checks.push({ ...check, name: requireText(check.name, 'check name') });
}

export function finishEvidenceBundle(
  bundle: EvidenceBundle,
  result: { status: Exclude<EvidenceRunStatus, 'running'>; finishedAt?: string },
): void {
  bundle.status = result.status;
  bundle.finishedAt = result.finishedAt ?? new Date().toISOString();
}

export function writeEvidenceBundle(filePath: string, bundle: EvidenceBundle): void {
  if (bundle.status === 'running' || !bundle.finishedAt) {
    throw new Error('refusing to write unfinished QA evidence');
  }
  const resolved = path.resolve(filePath);
  fs.mkdirSync(path.dirname(resolved), { recursive: true });
  const temporary = `${resolved}.tmp-${process.pid}`;
  try {
    fs.writeFileSync(temporary, `${JSON.stringify(bundle, null, 2)}\n`, { encoding: 'utf8', flag: 'wx' });
    fs.renameSync(temporary, resolved);
  } finally {
    fs.rmSync(temporary, { force: true });
  }
}

function requireText(value: string, name: string): string {
  const normalized = value.trim();
  if (!normalized) throw new Error(`${name} is required`);
  return normalized;
}
