import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { formatDrift, rightkitDrift } from './rightkit-drift.mjs';
import { fileURLToPath } from 'node:url';
import { ownershipAvailability, ownershipDetection } from './ownership-availability.mjs';

const catalogUrl = new URL('./ownership-session-catalog.json', import.meta.url);
const versionsUrl = new URL('./ownership-session-versions.json', import.meta.url);
const catalog = JSON.parse(readFileSync(fileURLToPath(catalogUrl), 'utf8'));
const versions = JSON.parse(readFileSync(fileURLToPath(versionsUrl), 'utf8'));

export function renderSessionCatalog(manifest = catalog, versionManifest = versions) {
  const rows = (manifest.capabilities ?? []).map((item) => {
    const resolvedVersions = availabilityFrom(item, versionManifest);
    return `- ${item.capability} → ${item.package}: ${resolvedVersions || 'no published version recorded'}; ${ownershipDetection(item)}`;
  });
  if (manifest.internalOwners?.length) rows.push('Internal owners (not app capabilities):', ...manifest.internalOwners.map((item) => `- ${item.package}: ${item.reason}`));
  return ['RightKit ownership index (availability from rightkit-versions.json; dependency declarations do not prove integration):', ...rows, 'Lookup details: `rustright owns <capability>`'].join('\n');
}

function availabilityFrom(item, versionManifest) {
  return ownershipAvailability(item, versionManifest).map((row) => `${row.package}@${row.version} [${row.state}]`).join(', ');
}

/** The session's repository pins that lag the latest published RightKit versions, with the adopt command. */
export function sessionDrift(cwd) {
  if (!cwd) return null;
  try {
    const top = spawnSync('git', ['-C', cwd, 'rev-parse', '--show-toplevel'], { encoding: 'utf8', windowsHide: true, timeout: 5_000 });
    if (top.status !== 0 || !top.stdout.trim()) return null;
    const drift = rightkitDrift(top.stdout.trim(), versions);
    if (!drift.length) return null;
    const adopt = `node "${fileURLToPath(new URL('./rightkit-drift.mjs', import.meta.url))}" --apply "${top.stdout.trim()}"`;
    return `${formatDrift(drift)}
Adopt compatible updates (refreshes lockfiles; add --all for breaking ones, which need code changes): ${adopt}`;
  } catch { return null; }
}

export function ownershipSessionDecision(input = {}) {
  const drift = sessionDrift(input?.cwd);
  return { hookSpecificOutput: { hookEventName: 'SessionStart', additionalContext: drift ? `${renderSessionCatalog()}
${drift}` : renderSessionCatalog() } };
}
