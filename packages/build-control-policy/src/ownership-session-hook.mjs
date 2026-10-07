import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const catalogUrl = new URL('./ownership-session-catalog.json', import.meta.url);
const versionsUrl = new URL('./ownership-session-versions.json', import.meta.url);
const catalog = JSON.parse(readFileSync(fileURLToPath(catalogUrl), 'utf8'));
const versions = JSON.parse(readFileSync(fileURLToPath(versionsUrl), 'utf8'));

export function renderSessionCatalog(manifest = catalog, versionManifest = versions) {
  const rows = (manifest.capabilities ?? []).map((item) => {
    const resolvedVersions = availabilityFrom(item, versionManifest);
    return `- ${item.capability} → ${item.package}: ${resolvedVersions || 'no published version recorded'}`;
  });
  return ['RightKit ownership index (availability from rightkit-versions.json; dependency declarations do not prove integration):', ...rows, 'Lookup details: `rightkit owns <capability>`'].join('\n');
}

function availabilityFrom(item, versionManifest) {
  const name = item.package;
  if (name?.startsWith('@rightkit/')) {
    const version = versionManifest.stagedNpm?.[name] ?? versionManifest.npm?.[name];
    return version ? `${name}@${version} [${versionManifest.stagedNpm?.[name] ? 'latest' : 'floor'}]` : null;
  }
  if (name?.startsWith('rightkit-')) {
    const version = versionManifest.stagedCargo?.[name] ?? versionManifest.cargo?.[name];
    return version ? `${name}@${version} [${versionManifest.stagedCargo?.[name] ? 'latest' : 'floor'}]` : null;
  }
  return null;
}

export function ownershipSessionDecision() {
  return { hookSpecificOutput: { hookEventName: 'SessionStart', additionalContext: renderSessionCatalog() } };
}
