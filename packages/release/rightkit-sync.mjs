#!/usr/bin/env node
// Release feed: set rightkit-versions.json staged maps from what crates.io and npm actually serve.
// Every publishable crate under crates/ and package under packages/ is looked up; a newer
// published version becomes the staged (latest) version and the one it replaces stays allowed
// in legacyCargo / legacyNpm, so apps pinned to it keep validating until they adopt.
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = path.dirname(fileURLToPath(import.meta.url));
const rightkitRoot = path.resolve(packageRoot, "..", "..");
const VERSIONS_FILE = path.join(packageRoot, "rightkit-versions.json");
const USER_AGENT = "rightkit-sync/1 (https://github.com/bogusyogi/claude)";

const field = (toml, key) => toml.match(new RegExp(`^${key}\\s*=\\s*"([^"]*)"`, "m"))?.[1] ?? null;

/** Publishable RightKit crates and npm packages, from the source tree. */
export function rightkitPublishables(root = rightkitRoot) {
  const crates = []; const packages = [];
  for (const dir of readdirSync(path.join(root, "crates")).sort()) {
    const manifest = path.join(root, "crates", dir, "Cargo.toml");
    if (!existsSync(manifest)) continue;
    const toml = readFileSync(manifest, "utf8");
    const [packageSection] = toml.split(/^\[(?!package\])/m);
    if (/^publish\s*=\s*false/m.test(packageSection)) continue;
    const name = field(packageSection, "name");
    if (name?.startsWith("rightkit-")) crates.push(name);
  }
  for (const dir of readdirSync(path.join(root, "packages")).sort()) {
    const manifest = path.join(root, "packages", dir, "package.json");
    if (!existsSync(manifest)) continue;
    const pkg = JSON.parse(readFileSync(manifest, "utf8"));
    if (!pkg.private && pkg.name?.startsWith("@rightkit/")) packages.push(pkg.name);
  }
  return { crates, packages };
}

export function compareVersions(left, right) {
  const parse = (value) => String(value).split("-")[0].split(".").map((part) => Number.parseInt(part, 10) || 0);
  const a = parse(left); const b = parse(right);
  for (let i = 0; i < 3; i += 1) if (a[i] !== b[i]) return a[i] - b[i];
  return 0;
}

async function fetchJson(fetchImpl, url) {
  const response = await fetchImpl(url, { headers: { "user-agent": USER_AGENT, accept: "application/json" }, signal: AbortSignal.timeout(15_000) });
  if (response.status === 404) return null;
  if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
  return response.json();
}

/** Latest stable published version per registry, or null when never published. */
export async function registryLatest({ crates, packages }, { fetchImpl = fetch, concurrency = 6 } = {}) {
  const jobs = [
    ...crates.map((name) => ({ registry: "cargo", name, url: `https://crates.io/api/v1/crates/${name}`, pick: (body) => body?.crate?.max_stable_version ?? null })),
    ...packages.map((name) => ({ registry: "npm", name, url: `https://registry.npmjs.org/${name.replace("/", "%2f")}`, pick: (body) => body?.["dist-tags"]?.latest ?? null })),
  ];
  const results = []; let cursor = 0;
  const worker = async () => {
    while (cursor < jobs.length) {
      const job = jobs[cursor]; cursor += 1;
      results.push({ registry: job.registry, name: job.name, version: job.pick(await fetchJson(fetchImpl, job.url)) });
    }
  };
  await Promise.all(Array.from({ length: Math.min(concurrency, jobs.length) }, worker));
  return results.sort((a, b) => `${a.registry}:${a.name}`.localeCompare(`${b.registry}:${b.name}`));
}

/** Pure update: returns the new versions document and the list of changes. */
export function applyRegistryLatest(versions, latest) {
  const next = structuredClone(versions); const changes = [];
  for (const { registry, name, version } of latest) {
    if (!version) continue;
    const stagedKey = registry === "cargo" ? "stagedCargo" : "stagedNpm";
    const legacyKey = registry === "cargo" ? "legacyCargo" : "legacyNpm";
    const current = next[stagedKey]?.[name] ?? next[registry]?.[name] ?? null;
    if (current && compareVersions(version, current) <= 0) continue;
    next[stagedKey] ??= {};
    const previous = next[stagedKey][name];
    if (previous && previous !== next[registry]?.[name]) {
      next[legacyKey] ??= {};
      const legacy = new Set(next[legacyKey][name] ?? []); legacy.add(previous);
      next[legacyKey][name] = [...legacy].sort(compareVersions);
    }
    next[stagedKey][name] = version;
    next[stagedKey] = Object.fromEntries(Object.entries(next[stagedKey]).sort(([a], [b]) => a.localeCompare(b)));
    changes.push({ registry, name, from: current, to: version });
  }
  return { versions: next, changes };
}

export async function syncRightkitVersions({ check = false, fetchImpl = fetch, file = VERSIONS_FILE, root = rightkitRoot, log = (line) => process.stdout.write(`${line}\n`) } = {}) {
  const versions = JSON.parse(readFileSync(file, "utf8"));
  const latest = await registryLatest(rightkitPublishables(root), { fetchImpl });
  const { versions: next, changes } = applyRegistryLatest(versions, latest);
  for (const change of changes) log(`${change.registry} ${change.name}: ${change.from ?? "unlisted"} -> ${change.to}`);
  const unpublished = latest.filter((item) => !item.version).map((item) => `${item.registry}:${item.name}`);
  if (unpublished.length) log(`never published: ${unpublished.join(", ")}`);
  if (!changes.length) log("rightkit-versions.json matches the registries");
  if (check) return { changes, unpublished, copies: [], written: false };
  if (changes.length) writeFileSync(file, `${JSON.stringify(next, null, 2)}\n`);
  const copies = syncConsumerCopies(root);
  for (const copy of copies) log(`refreshed ${copy}`);
  return { changes, unpublished, copies, written: changes.length > 0 };
}

/** The broker client and the session hook carry byte copies of the catalog and versions; keep them exact. */
export function syncConsumerCopies(root = rightkitRoot) {
  const release = path.join(root, "packages", "release");
  const targets = [
    ["rightkit-ownership.json", path.join(root, "packages", "build-control-clients", "src", "ownership-catalog.json")],
    ["rightkit-versions.json", path.join(root, "packages", "build-control-clients", "src", "rightkit-versions.json")],
    ["rightkit-ownership.json", path.join(root, "packages", "build-control-policy", "src", "ownership-session-catalog.json")],
    ["rightkit-ownership.json", path.join(root, "packages", "build-control-policy", "src", "ownership-fingerprints.json")],
    ["rightkit-versions.json", path.join(root, "packages", "build-control-policy", "src", "ownership-session-versions.json")],
  ];
  const updated = [];
  for (const [source, target] of targets) {
    const bytes = readFileSync(path.join(release, source));
    JSON.parse(bytes.toString("utf8"));
    if (existsSync(target) && readFileSync(target).equals(bytes)) continue;
    writeFileSync(target, bytes);
    updated.push(path.relative(root, target).split(path.sep).join("/"));
  }
  return updated;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const check = process.argv.includes("--check");
  syncRightkitVersions({ check })
    .then(({ changes }) => { if (check && changes.length) process.exitCode = 1; })
    .catch((error) => { process.stderr.write(`rightkit-sync: ${error.message}\n`); process.exitCode = 1; });
}
