import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parse as parseToml } from "smol-toml";
import { ownershipAvailability } from "./ownership-availability.mjs";

const releaseRoot = path.dirname(fileURLToPath(import.meta.url));
export const CATALOG_CONFIG = JSON.parse(readFileSync(new URL("./ownership-catalog-config.json", import.meta.url)));
const json = (file) => JSON.parse(readFileSync(file, "utf8"));

export function validateCatalogChanges(previous, next, { approveNewCapabilities = false } = {}) {
  const owners = new Map();
  for (const entry of previous.capabilities ?? []) {
    if (!owners.has(entry.capability)) owners.set(entry.capability, new Set());
    owners.get(entry.capability).add(entry.package);
  }
  const newOwners = new Map();
  const declarations = new Set();
  for (const entry of next.capabilities) {
    const declaration = `${entry.capability}:${entry.package}`;
    if (declarations.has(declaration)) throw new Error(`ownership: duplicate owner declaration ${declaration}`);
    declarations.add(declaration);
    const known = owners.get(entry.capability);
    if (known && !known.has(entry.package)) throw new Error(`ownership: ${entry.capability} already belongs to ${[...known].join(", ")}; refusing ${entry.package}`);
    if (!known && !approveNewCapabilities) throw new Error(`ownership: new capability ${entry.capability} requires ${CATALOG_CONFIG.newCapabilityApprovalFlag}`);
    if (!known && newOwners.has(entry.capability) && newOwners.get(entry.capability) !== entry.package) throw new Error(`ownership: conflicting new owners for ${entry.capability}`);
    newOwners.set(entry.capability, entry.package);
  }
}

// Package declarations are canonical. Availability always comes from versions,
// never from package versions, descriptions, or generated consumer snapshots.
export function generateOwnershipCatalog(root) {
  const capabilities = [], ids = new Set();
  const versions = json(path.join(root, "packages/release/rightkit-versions.json"));
  for (const group of ["crates", "packages", "swift"]) {
    const parent = path.join(root, group);
    if (!existsSync(parent)) continue;
    for (const name of readdirSync(parent).sort()) {
      const dir = path.join(parent, name);
      const file = path.join(dir, name === "release" && group === "packages" ? "ownership-declaration.json" : "rightkit-ownership.json");
      if (!existsSync(file)) continue;
      const declaration = json(file);
      if (declaration.schema !== undefined) throw new Error(`ownership: generated catalog cannot be an owner declaration: ${file}`);
      const actual = group === "crates" ? parseToml(readFileSync(path.join(dir, "Cargo.toml"), "utf8")).package?.name
        : group === "packages" ? json(path.join(dir, "package.json")).name : existsSync(path.join(dir, "Package.swift")) ? /\bname\s*:\s*"([^"]+)"/.exec(readFileSync(path.join(dir, "Package.swift"), "utf8"))?.[1] : null;
      if (!actual || declaration.package !== actual) throw new Error(`ownership: declaration owner differs from package metadata: ${file}`);
      for (const entry of declaration.capabilities ?? []) {
        if (entry.package !== undefined || !/^[a-z0-9][a-z0-9._-]{1,127}$/.test(entry.capability) || !Array.isArray(entry.fingerprints)) throw new Error(`ownership: malformed declaration: ${file}`);
        const owner = { package: actual, ...entry };
        if (owner.detection !== (owner.fingerprints.length ? "fingerprinted" : "declaration-only")) throw new Error(`ownership: explicit detection required: ${file}`);
        for (const f of owner.fingerprints) {
          if (!/^[a-z0-9][a-z0-9._-]{1,127}$/.test(f.id) || ids.has(f.id)) throw new Error(`ownership: invalid or duplicate fingerprint ${f.id}`);
          ids.add(f.id);
          if (!["block", "advise"].includes(f.tier) || !f.usageContext || !f.tierReason) throw new Error(`ownership: per-fingerprint tier & context required: ${f.id}`);
          if (f.type === "dependency") {
            if (!["cargo", "npm", "swiftpm"].includes(f.ecosystem) || !f.name || /^(@rightkit\/|rightkit-)/.test(f.name)) throw new Error(`ownership: imprecise dependency: ${f.id}`);
          } else if (f.type === "source") {
            // codeOnly: false is explicit opt-in for rules that must see string literals (auth URLs, #279).
            if (typeof f.codeOnly !== "boolean" || !f.extensions?.length) throw new Error(`ownership: source fingerprint must be code & extension scoped: ${f.id}`);
            new RegExp(f.pattern);
          } else throw new Error(`ownership: unknown fingerprint type: ${f.id}`);
          // A banned dependency blocks regardless of owner qualification: the ban stands even
          // when no replacement is used. It names the authority that imposed it.
          if (f.tier === "block" && f.policy === "banned") {
            // A banned source rule (e.g. in-app webview sign-in, #279) is allowed: the verdict blocks on policy "banned" for any type.
            if (!["dependency", "source"].includes(f.type) || !f.policyAuthority) throw new Error(`ownership: banned fingerprint needs a dependency or source rule & policyAuthority: ${f.id}`);
          } else if (f.tier === "block") {
            const published = ownershipAvailability(owner, versions).find((row) => row.state !== "staged-unverified");
            const q = f.qualification;
            if (!published || q?.version !== published.version || !["macos", "windows"].every((host) => q.hosts?.includes(host)) || !q.evidence?.length) throw new Error(`ownership: block fingerprint lacks published two-host qualification: ${f.id}`);
          }
        }
        capabilities.push(owner);
      }
    }
  }
  const families = new Map();
  for (const entry of capabilities) {
    if (!families.has(entry.capability)) families.set(entry.capability, new Set());
    families.get(entry.capability).add(entry.package);
  }
  for (const [capability, owners] of families) if (owners.size > 1) {
    const family = CATALOG_CONFIG.ownerFamilies.find((entry) => entry.capability === capability);
    if (!family?.reason || owners.size !== family.packages.length || [...owners].some((owner) => !family.packages.includes(owner))) throw new Error(`ownership: conflicting owner family ${capability}`);
  }
  const internalOwners = json(path.join(root, "packages/release/ownership-internal-owners.json")).owners;
  return { schema: 1, version: CATALOG_CONFIG.version, capabilities, internalOwners };
}

export function writeOwnershipCatalog(root, { approveNewCapabilities = false, previous = json(path.join(releaseRoot, "rightkit-ownership.json")), output = path.join(root, "packages/release/rightkit-ownership.json") } = {}) {
  const catalog = generateOwnershipCatalog(root);
  validateCatalogChanges(previous, catalog, { approveNewCapabilities });
  writeFileSync(output, `${JSON.stringify(catalog, null, 2)}\n`);
  return catalog;
}
