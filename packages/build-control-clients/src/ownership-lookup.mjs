import { readFileSync, realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { ownershipAvailability, ownershipDetection } from "./ownership-availability.mjs";

export const OWNERSHIP_CATALOG_PATH = fileURLToPath(new URL("./ownership-catalog.json", import.meta.url));
export const RIGHTKIT_VERSIONS_PATH = fileURLToPath(new URL("./rightkit-versions.json", import.meta.url));

export function loadOwnershipCatalog({ catalogPath = OWNERSHIP_CATALOG_PATH, versionsPath = RIGHTKIT_VERSIONS_PATH } = {}) {
  return { manifest: JSON.parse(readFileSync(catalogPath, "utf8")), versions: JSON.parse(readFileSync(versionsPath, "utf8")) };
}

const published = ownershipAvailability;

export function findOwnedCapability(query, { manifest, versions }) {
  const normalized = String(query ?? "").trim().toLowerCase();
  if (!normalized) throw new TypeError("usage: rightkit owns <capability>");
  const entries = [...(manifest.capabilities ?? []), ...(manifest.internalOwners ?? []).map((item) => ({ ...item, capability: "internal-infrastructure", summary: item.reason }))];
  const matches = entries.filter((item) => [item.capability, item.package, ...(item.aliases ?? [])].some((value) => String(value).toLowerCase() === normalized));
  return matches.map((item) => ({
    capability: item.capability,
    owner: item.package,
    summary: item.summary ?? null,
    detection: ownershipDetection(item),
    availability: published(item, versions),
    ...(item.platforms ? { platforms: item.platforms } : {}),
    ...(item.api ? { api: item.api } : {}),
    ...(item.minimumVersion ? { minimumVersion: item.minimumVersion } : {}),
    ...(item.migration ? { migration: item.migration } : {}),
  }));
}

export function formatOwnedCapability(items) {
  if (!items.length) return "No matching RightKit capability. Try a capability name from the session ownership index.";
  return items.map((item) => {
    const available = item.availability.length ? item.availability.map((x) => `${x.package}@${x.version} (${x.state}, ${x.registry})`).join(", ") : "no published version recorded";
    const lines = [`${item.capability} — ${item.owner}`, item.summary, `Availability: ${available}`, `Detection: ${item.detection}`];
    if (item.platforms) lines.push(`Platforms: ${Array.isArray(item.platforms) ? item.platforms.join(", ") : item.platforms}`);
    if (item.minimumVersion) lines.push(`Minimum version: ${item.minimumVersion}`);
    if (item.api) lines.push(`API: ${typeof item.api === "string" ? item.api : JSON.stringify(item.api)}`);
    if (item.migration) lines.push(`Migration: ${typeof item.migration === "string" ? item.migration : JSON.stringify(item.migration)}`);
    return lines.filter(Boolean).join("\n");
  }).join("\n\n");
}

export function ownershipLookup(query, catalogs = loadOwnershipCatalog()) {
  if (query === "--compact") return renderOwnershipSessionCatalog(catalogs);
  return formatOwnedCapability(findOwnedCapability(query, catalogs));
}

export function renderOwnershipSessionCatalog({ manifest, versions }) {
  const rows = (manifest.capabilities ?? []).map((item) => {
    const availability = published(item, versions).map((x) => `${x.package}@${x.version} [${x.state}]`).join(", ") || "no published version recorded";
    return `- ${item.capability} → ${item.package}: ${availability}; ${ownershipDetection(item)}`;
  });
  if (manifest.internalOwners?.length) rows.push("Internal owners (not app capabilities):", ...manifest.internalOwners.map((item) => `- ${item.package}: ${item.reason}`));
  return ["RightKit ownership index (availability from rightkit-versions.json; dependency declarations do not prove integration):", ...rows, "Lookup details: `rightkit owns <capability>`"].join("\n");
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try {
    const query = process.argv[2];
    if (!query) throw new TypeError("usage: rightkit owns <capability>");
    const output = ownershipLookup(query, loadOwnershipCatalog());
    if (output.startsWith("No matching RightKit capability.")) process.exitCode = 1;
    process.stdout.write(`${output}\n`);
  } catch (error) { process.stderr.write(`rightkit owns: ${error.message}\n`); process.exitCode = 2; }
}
