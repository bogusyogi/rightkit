#!/usr/bin/env node
// Release feed: set rightkit-versions.json staged maps from what crates.io and npm actually serve.
// Every publishable crate under crates/ and package under packages/ is looked up; a newer
// published version becomes the staged (latest) version and the one it replaces stays allowed
// in legacyCargo / legacyNpm, so apps pinned to it keep validating until they adopt.
import { existsSync, readFileSync, readdirSync, writeFileSync, realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = path.dirname(fileURLToPath(import.meta.url));
const rightkitRoot = path.resolve(packageRoot, "..", "..");
const VERSIONS_FILE = path.join(packageRoot, "rightkit-versions.json");
const USER_AGENT = "rightkit-sync/1 (https://github.com/bogusyogi/rightkit)";

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

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

export function compareVersions(left, right) {
  const a = String(left).match(SEMVER); const b = String(right).match(SEMVER);
  if (!a || !b) throw new Error(`invalid semver: ${!a ? left : right}`);
  const compare = (x, y) => x < y ? -1 : x > y ? 1 : 0;
  for (let i = 1; i <= 3; i += 1) {
    const order = compare(BigInt(a[i]), BigInt(b[i]));
    if (order) return order;
  }
  if (!a[4] || !b[4]) return compare(Boolean(b[4]), Boolean(a[4]));
  const ap = a[4].split("."); const bp = b[4].split(".");
  for (let i = 0; i < Math.max(ap.length, bp.length); i += 1) {
    if (ap[i] === undefined || bp[i] === undefined) return compare(ap.length, bp.length);
    const an = /^\d+$/.test(ap[i]); const bn = /^\d+$/.test(bp[i]);
    const order = an && bn ? compare(BigInt(ap[i]), BigInt(bp[i]))
      : an !== bn ? (an ? -1 : 1) : compare(ap[i], bp[i]);
    if (order) return order;
  }
  return 0;
}

const versionKeys = (registry) => {
  if (!["npm", "cargo"].includes(registry)) throw new Error(`unknown registry: ${registry}; expected npm or cargo`);
  return registry === "npm" ? ["stagedNpm", "legacyNpm"] : ["stagedCargo", "legacyCargo"];
};

// Use publication evidence captured before replacing registry[name], never staging alone.
function retainPublished(versions, { registry, name, version }, candidates, latest) {
  const [, legacyKey] = versionKeys(registry);
  const recorded = versions.registry?.[registry]?.[name];
  const eligible = candidates.filter((previous) => previous && compareVersions(previous, version) < 0
    && (previous === recorded || previous === latest));
  if (!eligible.length) return;
  versions[legacyKey] ??= {};
  versions[legacyKey][name] = [...new Set([...(versions[legacyKey][name] ?? []), ...eligible])].sort(compareVersions);
}

/** Stage a rollout while preserving published pins, then refresh consumer copies. */
export async function stageVersion({ registry, name, version }, { file = VERSIONS_FILE, root = rightkitRoot, log = (line) => process.stdout.write(`${line}\n`), fetchImpl = fetch } = {}) {
  const [stagedKey] = versionKeys(registry);
  if (typeof name !== "string" || !name.trim()) throw new Error("stage requires a package or crate name");
  if (typeof version !== "string" || !SEMVER.test(version)) throw new Error(`invalid semver: ${version}`);
  const versions = JSON.parse(readFileSync(file, "utf8"));
  const current = versions[stagedKey]?.[name] ?? versions[registry]?.[name] ?? null;
  if (current && compareVersions(version, current) < 0) throw new Error(`refusing to stage ${name} ${version} below ${current}`);
  const next = structuredClone(versions);
  const recorded = next.registry?.[registry]?.[name];
  let latest;
  if (current && compareVersions(current, version) < 0 && current !== recorded) {
    const [result] = await registryLatest({ crates: registry === "cargo" ? [name] : [], packages: registry === "npm" ? [name] : [] }, { fetchImpl });
    latest = result.version;
  }
  retainPublished(next, { registry, name, version }, [current, recorded], latest);
  next[stagedKey] ??= {};
  next[stagedKey][name] = version;
  next[stagedKey] = Object.fromEntries(Object.entries(next[stagedKey]).sort(([a], [b]) => a.localeCompare(b)));
  const written = JSON.stringify(next) !== JSON.stringify(versions);
  if (written) writeFileSync(file, `${JSON.stringify(next, null, 2)}\n`);
  const copies = syncConsumerCopies(root);
  log(`${registry} ${name}: ${current ?? "unlisted"} -> ${version}`);
  for (const copy of copies) log(`refreshed ${copy}`);
  return { changes: current !== version ? [{ registry, name, from: current, to: version }] : [], copies, written };
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
  const next = structuredClone(versions); const changes = []; let verified = 0;
  for (const { registry, name, version } of latest) {
    if (!version) continue;
    const [stagedKey] = versionKeys(registry);
    const current = next[stagedKey]?.[name] ?? next[registry]?.[name] ?? null;
    const recorded = next.registry?.[registry]?.[name];
    retainPublished(next, { registry, name, version }, [current, recorded], version);
    // A registry can lag an unpublished staged rollout: its published pin still stays allowed.
    if (current && compareVersions(version, current) < 0) {
      retainPublished(next, { registry, name, version: current }, [recorded, version], version);
    }
    // What the registry serves: the only versions adoption recommends (staged may still be unpublished).
    next.registry ??= { cargo: {}, npm: {} }; next.registry[registry] ??= {};
    if (next.registry[registry][name] !== version) { next.registry[registry][name] = version; verified += 1; }
    if (current && compareVersions(version, current) <= 0) continue;
    next[stagedKey] ??= {};
    next[stagedKey][name] = version;
    next[stagedKey] = Object.fromEntries(Object.entries(next[stagedKey]).sort(([a], [b]) => a.localeCompare(b)));
    changes.push({ registry, name, from: current, to: version });
  }
  for (const key of ["cargo", "npm"]) if (next.registry?.[key]) next.registry[key] = Object.fromEntries(Object.entries(next.registry[key]).sort(([a], [b]) => a.localeCompare(b)));
  return { versions: next, changes, verified };
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
  const written = JSON.stringify(next) !== JSON.stringify(versions);
  if (written) writeFileSync(file, `${JSON.stringify(next, null, 2)}\n`);
  const copies = syncConsumerCopies(root);
  for (const copy of copies) log(`refreshed ${copy}`);
  return { changes, unpublished, copies, written };
}

/** Stage source versions into the feed BEFORE packing, so the rightkit-versions.json a tarball ships lists
 * the very version it ships with (the feed is otherwise written after publish and lags by one release).
 * Synchronous and offline: a unit below its staged version is skipped (never lowered); published pins stay
 * allowed through the same retainPublished rule as stageVersion. Refreshes the consumer copies. */
export function stageSourceVersions(entries, { file = VERSIONS_FILE, root = rightkitRoot, log = (line) => process.stdout.write(`${line}\n`) } = {}) {
  const versions = JSON.parse(readFileSync(file, "utf8"));
  const next = structuredClone(versions); const changes = [];
  for (const { registry, name, version } of entries) {
    const [stagedKey] = versionKeys(registry);
    if (typeof name !== "string" || !name.trim()) throw new Error("stageSourceVersions requires a package or crate name");
    if (typeof version !== "string" || !SEMVER.test(version)) throw new Error(`invalid semver: ${version}`);
    const current = next[stagedKey]?.[name] ?? next[registry]?.[name] ?? null;
    if (current && compareVersions(version, current) <= 0) continue;
    retainPublished(next, { registry, name, version }, [current, next.registry?.[registry]?.[name]], undefined);
    next[stagedKey] ??= {};
    next[stagedKey][name] = version;
    next[stagedKey] = Object.fromEntries(Object.entries(next[stagedKey]).sort(([a], [b]) => a.localeCompare(b)));
    changes.push({ registry, name, from: current, to: version });
  }
  const written = JSON.stringify(next) !== JSON.stringify(versions);
  if (written) writeFileSync(file, `${JSON.stringify(next, null, 2)}\n`);
  const copies = syncConsumerCopies(root);
  for (const change of changes) log(`${change.registry} ${change.name}: ${change.from ?? "unlisted"} -> ${change.to} (staged before pack)`);
  return { changes, copies, written };
}

/** Feed file and consumer-copy root of one requested workspace. recordPublished/stageSourceVersions default to the
 * location of THIS module, which is inside the package store when right-release runs from an installed
 * @rightkit/release; every caller that knows its workspace passes these explicitly. */
export function workspaceFeedOptions(workspaceRoot) {
  const root = path.resolve(workspaceRoot);
  return { root, file: path.join(root, "packages", "release", "rightkit-versions.json") };
}

/** Record one just-published version (the registry index can lag the upload) and refresh the copies.
 * Called by right-release after a verified cargo or npm publish; never fails the publish itself unless `strict` is set. */
export function recordPublished({ registry, name, version }, { file = VERSIONS_FILE, root = rightkitRoot, log = (line) => process.stdout.write(`${line}\n`), strict = false } = {}) {
  try {
    const versions = JSON.parse(readFileSync(file, "utf8"));
    const { versions: next, changes } = applyRegistryLatest(versions, [{ registry, name, version }]);
    if (JSON.stringify(next) !== JSON.stringify(versions)) writeFileSync(file, `${JSON.stringify(next, null, 2)}\n`);
    const copies = syncConsumerCopies(root);
    log(`[right-release] release feed: ${name} ${version} ${changes.length ? "staged" : "already current"}${copies.length ? `; refreshed ${copies.length} copies` : ""}; commit rightkit-versions.json and its copies`);
  } catch (error) {
    // `strict` (publish.mjs ledger) propagates: a recording that did not complete must fail its caller. Every other caller keeps the log-and-continue behaviour.
    if (strict) throw new Error(`release feed not updated for ${name} ${version}: ${error.message}`);
    log(`[right-release] release feed not updated (${error.message}); run node rightkit-sync.mjs`);
  }
}

/** The session hook carries byte copies of the catalog and versions; keep them exact. */
export function syncConsumerCopies(root = rightkitRoot) {
  const release = path.join(root, "packages", "release");
  const targets = [
    ["rightkit-ownership.json", path.join(root, "packages", "build-control-policy", "src", "ownership-session-catalog.json")],
    ["rightkit-ownership.json", path.join(root, "packages", "build-control-policy", "src", "ownership-fingerprints.json")],
    ["rightkit-versions.json", path.join(root, "packages", "build-control-policy", "src", "ownership-session-versions.json")],
    ["ownership-repositories.json", path.join(root, "packages", "build-control-policy", "src", "ownership-repositories.json")],
    ["rightkit-drift.mjs", path.join(root, "packages", "build-control-policy", "src", "rightkit-drift.mjs")],
    ["background-process.mjs", path.join(root, "packages", "build-control-policy", "src", "background-process.mjs")],
    ["ownership-dependency-allowlist.json", path.join(root, "packages", "build-control-policy", "src", "ownership-dependency-allowlist.json")],
    ["ownership-dependency-allowlist.mjs", path.join(root, "packages", "build-control-policy", "src", "ownership-dependency-allowlist.mjs")],
    ["ownership-source.mjs", path.join(root, "packages", "build-control-policy", "src", "ownership-source.mjs")],
    ["ownership-availability.mjs", path.join(root, "packages", "build-control-policy", "src", "ownership-availability.mjs")],
  ];
  const updated = [];
  for (const [source, target] of targets) {
    const bytes = readFileSync(path.join(release, source));
    if (source.endsWith(".json")) JSON.parse(bytes.toString("utf8"));
    if (existsSync(target) && readFileSync(target).equals(bytes)) continue;
    writeFileSync(target, bytes);
    updated.push(path.relative(root, target).split(path.sep).join("/"));
  }
  return updated;
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  const main = async () => {
    const args = process.argv.slice(2);
    if (args[0] === "stage") {
      if (args.length !== 4) throw new Error("usage: node rightkit-sync.mjs stage <npm|cargo> <name> <version>");
      await stageVersion({ registry: args[1], name: args[2], version: args[3] });
      return;
    }
    const check = args.includes("--check");
    const { changes } = await syncRightkitVersions({ check });
    if (check && changes.length) process.exitCode = 1;
  };
  main().catch((error) => { process.stderr.write(`rightkit-sync: ${error.message}\n`); process.exitCode = 1; });
}
