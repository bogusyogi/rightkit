// A receipt that names a file without hashing it is a claim, not evidence.
//
// The previous bundle recorded check names, statuses, and free-form details, then
// declared the run passed. Nothing tied those claims to the artifacts they were
// about: a screenshot could be regenerated, truncated, or never written at all
// and the receipt read exactly the same. "Signature verified" and "verified the
// signature of THESE bytes" are different statements, and only the second one
// survives someone asking later.
//
// So every artifact a check refers to is hashed at the moment it is recorded, a
// check may only reference artifacts that were hashed, and the bundle can be
// re-verified against the filesystem afterwards.

import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';

export type EvidenceCheckStatus = 'passed' | 'failed' | 'skipped';
export type EvidenceRunStatus = 'running' | 'passed' | 'failed';

export interface EvidenceArtifact {
  name: string;
  path: string;
  sha256: string;
  sizeBytes: number;
  recordedAt: string;
}

export interface EvidenceCheck {
  name: string;
  status: EvidenceCheckStatus;
  details?: Record<string, unknown>;
  /** Names of artifacts recorded via addEvidenceArtifact that back this check. */
  artifacts?: string[];
}

export interface EvidenceBundle {
  schemaVersion: 2;
  app: string;
  platform: string;
  runId: string;
  startedAt: string;
  finishedAt?: string;
  status: EvidenceRunStatus;
  checks: EvidenceCheck[];
  artifacts: EvidenceArtifact[];
}

export interface EvidenceBundleInput {
  app: string;
  platform: string;
  runId: string;
  startedAt?: string;
}

export function createEvidenceBundle(input: EvidenceBundleInput): EvidenceBundle {
  return {
    schemaVersion: 2,
    app: requireText(input.app, 'app'),
    platform: requireText(input.platform, 'platform'),
    runId: requireText(input.runId, 'runId'),
    startedAt: input.startedAt ?? new Date().toISOString(),
    status: 'running',
    checks: [],
    artifacts: [],
  };
}

/**
 * Hash a file into the bundle. Reading happens now, not at write time, so a file
 * that is later replaced or deleted cannot masquerade as the one that was seen.
 */
export function addEvidenceArtifact(
  bundle: EvidenceBundle,
  artifact: { name: string; path: string },
  now = new Date(),
): EvidenceArtifact {
  requireRunning(bundle);
  const name = requireText(artifact.name, 'artifact name');
  if (bundle.artifacts.some((existing) => existing.name === name)) {
    throw new Error(`QA evidence already records an artifact named ${name}`);
  }
  const resolved = path.resolve(requireText(artifact.path, 'artifact path'));
  let bytes: Buffer;
  try {
    bytes = fs.readFileSync(resolved);
  } catch (error) {
    throw new Error(`cannot record QA evidence artifact ${name}: ${resolved} is unreadable (${(error as Error).message})`);
  }
  const recorded: EvidenceArtifact = {
    name,
    path: resolved,
    sha256: crypto.createHash('sha256').update(bytes).digest('hex'),
    sizeBytes: bytes.length,
    recordedAt: now.toISOString(),
  };
  bundle.artifacts.push(recorded);
  return recorded;
}

export function addEvidenceCheck(bundle: EvidenceBundle, check: EvidenceCheck): void {
  requireRunning(bundle);
  const name = requireText(check.name, 'check name');
  const artifacts = check.artifacts ?? [];
  const unknown = artifacts.filter((reference) => !bundle.artifacts.some((item) => item.name === reference));
  if (unknown.length > 0) {
    throw new Error(
      `QA check ${name} references artifacts that were never hashed: ${unknown.join(', ')}. `
      + 'Record them with addEvidenceArtifact so the receipt is bound to real bytes.',
    );
  }
  bundle.checks.push({ ...check, name, ...(artifacts.length > 0 ? { artifacts: [...artifacts] } : {}) });
}

export function finishEvidenceBundle(
  bundle: EvidenceBundle,
  result: { status: Exclude<EvidenceRunStatus, 'running'>; finishedAt?: string },
): void {
  bundle.status = result.status;
  bundle.finishedAt = result.finishedAt ?? new Date().toISOString();
}

/**
 * Re-hash every recorded artifact. Returns the artifacts whose bytes no longer
 * match — empty means the receipt still describes what is on disk.
 */
export function verifyEvidenceArtifacts(bundle: EvidenceBundle): Array<{ name: string; reason: string }> {
  const drift: Array<{ name: string; reason: string }> = [];
  for (const artifact of bundle.artifacts) {
    let bytes: Buffer;
    try {
      bytes = fs.readFileSync(artifact.path);
    } catch {
      drift.push({ name: artifact.name, reason: `missing at ${artifact.path}` });
      continue;
    }
    const sha256 = crypto.createHash('sha256').update(bytes).digest('hex');
    if (sha256 !== artifact.sha256) {
      drift.push({ name: artifact.name, reason: `sha256 ${sha256} does not match recorded ${artifact.sha256}` });
    }
  }
  return drift;
}

export function writeEvidenceBundle(filePath: string, bundle: EvidenceBundle): void {
  if (bundle.status === 'running' || !bundle.finishedAt) {
    throw new Error('refusing to write unfinished QA evidence');
  }
  // A passing receipt is the one people trust without reading, so it is the one
  // whose artifacts must still be intact at write time.
  if (bundle.status === 'passed') {
    const drift = verifyEvidenceArtifacts(bundle);
    if (drift.length > 0) {
      throw new Error(
        `refusing to write a passing QA receipt whose evidence changed: ${drift.map((item) => `${item.name} (${item.reason})`).join('; ')}`,
      );
    }
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

function requireRunning(bundle: EvidenceBundle): void {
  if (bundle.status !== 'running') throw new Error('cannot modify a QA evidence bundle after the run has finished');
}

function requireText(value: string, name: string): string {
  const normalized = String(value ?? '').trim();
  if (!normalized) throw new Error(`${name} is required`);
  return normalized;
}
