// Registry-bound RightKit pins: at build admission the controller resolves the app's EXACT locked RightKit versions
// (Cargo.lock entries, pnpm-lock.yaml resolutions) against the canonical registries, so a new version of a known
// RightKit package needs no new release of this tool.
//
// What admission checks, per exact locked name+version of a RightKit-OWNED package (known names only; there is no
// maintainer-membership proof, so a name first published after this tool is refused until the ownership data lists it):
//   - ownership: named in the bundled ownership/inventory data (a rightkit-* or @rightkit/* prefix alone is not enough)
//   - source: canonical crates.io / npmjs.org registry resolution only (no path, git, tarball or directory overrides)
//   - policy: not revoked, not below the recorded minimum (rightkit-versions.json keeps those policies)
//   - registry: the exact version exists, is not yanked (crates.io) / deprecated (npm), and the registry digest
//     (Cargo index `cksum`, npm `dist.integrity`) equals the lock's digest. That is METADATA agreement between the
//     registry and the lock; it is not proof of the bytes a build consumes (nothing here materializes or hashes packages)
//   - freshness (build admission only): a pin behind the registry's latest (crates.io max_stable_version, npm
//     dist-tags.latest) is refused. A .rightkit/update.json hold is a migration lane task and grants no exception
//   - coverage: every RightKit dependency a manifest declares needs a lock entry for that package name somewhere in the
//     canonical lock inventory (a name-level check, not importer-specific proof)
// Registry existence proves publication only; every other qualification requirement stays separate.
//
// TRUST SCOPE: the admitted snapshot, its admission record and the release-inputs payload are plain JSON files in the
// release state root. Reuse (worker, resume, seal) checks that they agree with each other, with this release's identity
// and with the current locks. That is a TRUSTED-LOCAL consistency check, not an authenticated provenance boundary:
// anyone who can write the state root can forge a consistent set. A resume therefore proves "the recorded admission
// is internally consistent and still matches the locks", and does NOT prove the registry still says so or that the
// controller wrote it. Nothing here rewrites a pin.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { parse as parseToml } from "smol-toml";
import { isExcludedScanDirectory } from "./cargo-contract.mjs";
import { resolveSharedCacheRoot } from "./cache-policy.mjs";
import { parsePnpmYaml } from "./ownership-lockfiles.mjs";
import { compareVersions } from "./rightkit-sync.mjs";

export const SNAPSHOT_SCHEMA = 1;
export const SNAPSHOT_ENV = "RIGHT_RELEASE_RIGHTKIT_SNAPSHOT";
export const EVIDENCE_VERSION = 1;
export const REGISTRIES = Object.freeze({
  cargo: "https://index.crates.io/",
  cargoApi: "https://crates.io/api/v1/crates/",
  npm: "https://registry.npmjs.org/",
});
const CRATES_IO_SOURCES = new Set(["registry+https://github.com/rust-lang/crates.io-index", "registry+https://index.crates.io/"]);
const EXACT_SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
const USER_AGENT = "right-release-resolver/1 (https://github.com/bogusyogi/rightkit)";
const FIX = "run `right-update run` (@rightkit/outdated 0.2.0) to move the pin";
const DEFAULT_ATTEMPTS = 3;
const DEFAULT_TIMEOUT_MS = 15000;
const CONCURRENCY = 6;

export class RightKitRegistryError extends Error {
  constructor(code, message, { recoverable = false, problems = [] } = {}) {
    super(message);
    this.name = "RightKitRegistryError";
    this.code = code;
    this.recoverable = recoverable;
    this.problems = problems;
  }
}

const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const stable = (value) => {
  if (Array.isArray(value)) return `[${value.map(stable).join(",")}]`;
  if (value && typeof value === "object") return `{${Object.keys(value).sort().filter((k) => value[k] !== undefined).map((k) => `${JSON.stringify(k)}:${stable(value[k])}`).join(",")}}`;
  return JSON.stringify(value);
};

// ---------------------------------------------------------------------------------------------- ownership + policy

/** Owned names per registry, from the bundled versions file and ownership inventory. */
export function ownedRightKitNames(versions = {}, ownership = {}) {
  const owned = { cargo: new Set(), npm: new Set() };
  for (const key of ["npm", "stagedNpm", "legacyNpm"]) for (const name of Object.keys(versions[key] ?? {})) owned.npm.add(name);
  for (const name of Object.keys(versions.registry?.npm ?? {})) owned.npm.add(name);
  for (const key of ["cargo", "stagedCargo", "legacyCargo"]) for (const name of Object.keys(versions[key] ?? {})) owned.cargo.add(name);
  for (const name of Object.keys(versions.registry?.cargo ?? {})) owned.cargo.add(name);
  for (const entry of ownership.capabilities ?? []) {
    if (typeof entry?.package !== "string") continue;
    (entry.package.startsWith("@") ? owned.npm : owned.cargo).add(entry.package);
  }
  return owned;
}

const prefixed = (ecosystem, name) => (ecosystem === "cargo" ? name.startsWith("rightkit-") : name.startsWith("@rightkit/"));

function recordedVersions(versions, ecosystem, name) {
  const keys = ecosystem === "npm" ? ["npm", "stagedNpm", "legacyNpm"] : ["cargo", "stagedCargo", "legacyCargo"];
  const found = new Set();
  for (const value of [...keys.map((key) => versions[key]?.[name]), versions.registry?.[ecosystem]?.[name]]) {
    for (const version of Array.isArray(value) ? value : value === undefined ? [] : [value]) if (typeof version === "string" && EXACT_SEMVER.test(version)) found.add(version);
  }
  return [...found].sort(compareVersions);
}

/** Policy that survives the move to registry resolution: recorded minimum and explicit revocations. */
function policyProblem(versions, ecosystem, name, version) {
  const revoked = versions.revoked?.[ecosystem]?.[name];
  if ((Array.isArray(revoked) ? revoked : revoked === undefined ? [] : [revoked]).includes(version)) return `${name} ${version} is revoked by release policy`;
  const known = recordedVersions(versions, ecosystem, name);
  if (known.length && EXACT_SEMVER.test(version) && compareVersions(version, known[0]) < 0) return `${name} ${version} is below the recorded minimum ${known[0]}`;
  return null;
}

export function policyRevision(versions, releaseVersion = "") {
  return sha256(stable({ versions, releaseVersion })).slice(0, 16);
}

// ---------------------------------------------------------------------------------------------- lockfiles

function scanFiles(root, wanted) {
  const found = [];
  const visit = (dir) => {
    let entries;
    try { entries = readdirSync(dir, { withFileTypes: true }); } catch { return; }
    for (const entry of entries) {
      if (entry.isDirectory()) {
        if (!isExcludedScanDirectory(entry.name, path.basename(dir))) visit(path.join(dir, entry.name));
      } else if (entry.isFile() && wanted.has(entry.name)) found.push(path.join(dir, entry.name));
    }
  };
  visit(root);
  return found.sort();
}

function cargoLockEntries(file) {
  let doc;
  try { doc = parseToml(readFileSync(file, "utf8")); } catch (error) { throw new RightKitRegistryError("bad-lock", `${file} is not a readable Cargo.lock: ${error.message}`); }
  return (doc.package ?? []).map((pkg) => ({ ecosystem: "cargo", name: pkg.name, version: pkg.version, source: pkg.source ?? null, digest: pkg.checksum ? { algo: "sha256", value: String(pkg.checksum).toLowerCase() } : null, lockFile: file }));
}

function pnpmLockEntries(file) {
  let lock;
  try { lock = parsePnpmYaml(readFileSync(file, "utf8")); } catch (error) { throw new RightKitRegistryError("bad-lock", `${file} is not a readable pnpm-lock.yaml: ${error.message}`); }
  const entries = [];
  for (const [key, meta] of Object.entries(lock.packages ?? {})) {
    const match = /^(@[^/]+\/[^@]+|[^@]+)@(.+)$/.exec(String(key).replace(/^\//, ""));
    if (!match) continue;
    const resolution = meta && typeof meta === "object" ? meta.resolution : undefined;
    const registryOnly = resolution && typeof resolution === "object" && typeof resolution.integrity === "string"
      && !resolution.tarball && !resolution.directory && !resolution.type && !resolution.repo;
    entries.push({
      ecosystem: "npm", name: match[1], version: match[2].split(/[(_]/)[0],
      source: registryOnly ? "registry" : null,
      digest: registryOnly ? { algo: "integrity", value: resolution.integrity } : null,
      lockFile: file,
    });
  }
  return entries;
}

/** Every RightKit-candidate lock entry (owned, or carrying a RightKit prefix) under root. */
export function collectLockedRightKit(root, owned, { extraLocks = [] } = {}) {
  const lockFiles = [...new Set([...scanFiles(root, new Set(["Cargo.lock", "pnpm-lock.yaml"])), ...extraLocks.map((f) => path.resolve(f)).filter((f) => existsSync(f) && /^(?:Cargo\.lock|pnpm-lock\.yaml)$/.test(path.basename(f)))])];
  const entries = [];
  for (const file of lockFiles) {
    const all = path.basename(file) === "Cargo.lock" ? cargoLockEntries(file) : pnpmLockEntries(file);
    for (const entry of all) {
      if (owned[entry.ecosystem].has(entry.name)) { entries.push(entry); continue; }
      // An app's own path/workspace crate or package that happens to carry the prefix (no registry source, e.g. Pulse's
      // rightkit-mesh-ffi) is app-local code, not a RightKit pin. A prefixed name that DOES resolve from a registry and is not
      // owned stays a refusal (it could be a squatted or foreign package).
      if (prefixed(entry.ecosystem, entry.name) && entry.source !== null) entries.push(entry);
    }
  }
  return { lockFiles, entries };
}

const MANIFEST_DEP_TABLES = ["dependencies", "dev-dependencies", "build-dependencies"];
const NPM_DEP_SECTIONS = ["dependencies", "devDependencies", "optionalDependencies"];

/** Directories whose manifests need lock proof: the app root plus the directory of every canonical lock outside it. */
function manifestRoots(root, extraLocks) {
  const roots = [path.resolve(root)];
  for (const lock of extraLocks) {
    const dir = path.dirname(path.resolve(lock));
    if (!roots.some((r) => dir === r || dir.startsWith(r + path.sep))) roots.push(dir);
  }
  return roots;
}

/** RightKit dependencies the manifests under root declare (Cargo.toml, package.json): the inputs that need lock proof. */
export function declaredRightKitDependencies(root, owned, extraLocks = []) {
  const found = [];
  const take = (ecosystem, name, file, local = false) => {
    if (owned[ecosystem].has(name)) found.push({ ecosystem, name, file });
    else if (prefixed(ecosystem, name) && !local) found.push({ ecosystem, name, file });
  };
  for (const file of manifestRoots(root, extraLocks).flatMap((dir) => scanFiles(dir, new Set(["Cargo.toml", "package.json"])))) {
    try {
      if (path.basename(file) === "package.json") {
        const pkg = JSON.parse(readFileSync(file, "utf8"));
        // A git/file/link/workspace specifier never resolves from the registry; the package-contract validator refuses it by name.
        for (const section of NPM_DEP_SECTIONS) for (const [name, spec] of Object.entries(pkg[section] ?? {})) if (!/[:/]/.test(String(spec))) take("npm", name, file);
      } else {
        const doc = parseToml(readFileSync(file, "utf8"));
        const tables = [...MANIFEST_DEP_TABLES.map((t) => doc[t]), doc.workspace?.dependencies, ...Object.values(doc.target ?? {}).flatMap((target) => MANIFEST_DEP_TABLES.map((t) => target?.[t]))];
        // A path or workspace dependency on an unowned prefixed name is app-local (see collectLockedRightKit).
        for (const table of tables) for (const [key, spec] of Object.entries(table ?? {})) take("cargo", typeof spec === "object" && spec && typeof spec.package === "string" ? spec.package : key, file, typeof spec === "object" && spec !== null && (typeof spec.path === "string" || spec.workspace === true));
      }
    } catch { /* an unreadable manifest is the manifest validators' finding */ }
  }
  return found;
}

/** Declared RightKit dependencies that no lock entry proves: no bundled-only fallback. */
export function coverageProblems(declared, lockedEntries) {
  const have = new Set(lockedEntries.map((e) => `${e.ecosystem}\0${e.name}`));
  return [...new Set(declared.filter((d) => !have.has(`${d.ecosystem}\0${d.name}`)).map((d) => `${path.basename(d.file)} declares ${d.name} but no ${d.ecosystem === "cargo" ? "Cargo.lock" : "pnpm-lock.yaml"} entry proves it (${d.file}); a missing or incomplete lockfile cannot be admitted`))];
}

/** Pre-network refusals for one lock entry. */
function lockProblems(entry, owned, versions) {
  const label = `${entry.name} ${entry.version}`;
  const where = `(${path.basename(entry.lockFile)})`;
  if (!owned[entry.ecosystem].has(entry.name)) return [`${label} ${where} is not a RightKit-owned package in the ownership data (a ${entry.ecosystem === "cargo" ? "rightkit-" : "@rightkit/"} prefix alone does not establish ownership)`];
  const problems = [];
  if (entry.ecosystem === "cargo" ? !CRATES_IO_SOURCES.has(entry.source) : entry.source !== "registry") problems.push(`${label} ${where} does not resolve from the canonical ${entry.ecosystem === "cargo" ? "crates.io" : "npm"} registry (path, git, tarball and directory sources are refused)`);
  if (!EXACT_SEMVER.test(String(entry.version))) problems.push(`${label} ${where} is not an exact version`);
  if (!entry.digest) problems.push(`${label} ${where} has no ${entry.ecosystem === "cargo" ? "checksum" : "integrity"} in the lock`);
  const policy = policyProblem(versions, entry.ecosystem, entry.name, entry.version);
  if (policy) problems.push(policy);
  return problems;
}

// ---------------------------------------------------------------------------------------------- registry client

const defaultSleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function makeClient({ fetchImpl, sleep, attempts, timeoutMs }) {
  async function getJson(url, { accept = "application/json", text = false } = {}) {
    let last;
    for (let attempt = 1; attempt <= attempts; attempt += 1) {
      try {
        const response = await fetchImpl(url, { headers: { accept, "user-agent": USER_AGENT }, signal: AbortSignal.timeout(timeoutMs) });
        if (response.status === 404) return { status: 404 };
        if (response.status === 429 || response.status >= 500) { last = new Error(`HTTP ${response.status}`); }
        else if (!response.ok) throw Object.assign(new Error(`HTTP ${response.status}`), { final: true });
        else return { status: 200, body: text ? await response.text() : await response.json() };
      } catch (error) {
        if (error?.final) throw new RightKitRegistryError("registry-outage", `${url} answered ${error.message}`, { recoverable: true });
        last = error;
      }
      if (attempt < attempts) await sleep(250 * 4 ** (attempt - 1));
    }
    throw new RightKitRegistryError("registry-outage", `${url} unreachable after ${attempts} attempts: ${last?.message ?? last}`, { recoverable: true });
  }
  return { getJson };
}

function cargoIndexUrl(name) {
  const n = name.toLowerCase();
  const dir = n.length === 1 ? "1" : n.length === 2 ? "2" : n.length === 3 ? `3/${n[0]}` : `${n.slice(0, 2)}/${n.slice(2, 4)}`;
  return `${REGISTRIES.cargo}${dir}/${n}`;
}
const npmUrl = (name) => `${REGISTRIES.npm}${name.replace("/", "%2f")}`;

// ---------------------------------------------------------------------------------------------- cache (external storage only)

/** Cache root under the release tool's configured shared cache root; null when none is configured (never internal disk). */
export function defaultRegistryCacheDir(env = process.env, platform = process.platform) {
  try { return path.join(resolveSharedCacheRoot({ env, platform }), "rightkit-registry"); } catch { return null; }
}

function makeCache(dir, warn) {
  const file = (...parts) => path.join(dir, ...parts.map((p) => p.replaceAll("/", "%2f").replaceAll("..", "_")));
  return {
    dir,
    read(...parts) {
      if (!dir) return null;
      try { return JSON.parse(readFileSync(`${file(...parts)}.json`, "utf8")); } catch { return null; }
    },
    write(record, ...parts) {
      if (!dir) return;
      try {
        const target = `${file(...parts)}.json`;
        mkdirSync(path.dirname(target), { recursive: true });
        const temp = `${target}.tmp-${process.pid}`;
        writeFileSync(temp, `${JSON.stringify(record)}\n`);
        renameSync(temp, target);
      } catch (error) { warn(`registry cache not written (${error.message})`); }
    },
  };
}

// ---------------------------------------------------------------------------------------------- resolution

/**
 * Resolve every distinct exact RightKit lock entry once. Returns the snapshot; throws RightKitRegistryError.
 * Options are injectable for tests: fetchImpl, sleep, now, cacheDir.
 */
export async function resolveRightKitPins({
  root, extraLocks = [], versions, ownership, releaseVersion = "",
  fetchImpl = globalThis.fetch, sleep = defaultSleep, now = () => Date.now(),
  cacheDir = defaultRegistryCacheDir(), requireLiveLatest = false, attempts = DEFAULT_ATTEMPTS, timeoutMs = DEFAULT_TIMEOUT_MS,
  warn = (message) => console.error(`right-release: ${message}`),
}) {
  const owned = ownedRightKitNames(versions, ownership);
  const { lockFiles, entries: locked } = collectLockedRightKit(root, owned, { extraLocks });
  const problems = [...locked.flatMap((entry) => lockProblems(entry, owned, versions)), ...coverageProblems(declaredRightKitDependencies(root, owned, extraLocks), locked)];
  if (problems.length) throw new RightKitRegistryError("refused", `RightKit pins refused:\n${[...new Set(problems)].map((p) => `- ${p}`).join("\n")}`, { problems });

  const distinct = new Map();
  for (const entry of locked) {
    const key = `${entry.ecosystem}\0${entry.name}\0${entry.version}`;
    const known = distinct.get(key);
    if (!known) distinct.set(key, { ...entry, lockFiles: [entry.lockFile] });
    else if (known.digest.value !== entry.digest.value) problems.push(`${entry.name} ${entry.version} is locked with two different digests (${known.lockFiles[0]} and ${entry.lockFile})`);
    else known.lockFiles.push(entry.lockFile);
  }
  if (problems.length) throw new RightKitRegistryError("digest-mismatch", `RightKit pins refused:\n${problems.map((p) => `- ${p}`).join("\n")}`, { problems });

  const client = makeClient({ fetchImpl, sleep, attempts, timeoutMs });
  const cache = makeCache(cacheDir, warn);
  const observedAt = new Date(now()).toISOString();
  const packuments = new Map();
  const packument = (name) => { if (!packuments.has(name)) packuments.set(name, client.getJson(npmUrl(name), { accept: "application/vnd.npm.install-v1+json" })); return packuments.get(name); };
  const indexes = new Map();
  const index = (name) => { if (!indexes.has(name)) indexes.set(name, client.getJson(cargoIndexUrl(name), { text: true })); return indexes.get(name); };

  const refused = [];
  const outages = [];
  const verified = [];
  async function verify(entry) {
    const label = `${entry.name} ${entry.version}`;
    let registryDigest;
    try {
      if (entry.ecosystem === "cargo") {
        const response = await index(entry.name);
        if (response.status === 404) { refused.push(`${label} is not published on crates.io (no index entry for ${entry.name})`); return; }
        const rows = String(response.body).split("\n").filter(Boolean).map((line) => { try { return JSON.parse(line); } catch { return null; } });
        const row = rows.find((candidate) => candidate?.vers === entry.version);
        if (!row) { refused.push(`${label} is not published on crates.io (staged or unpublished version)`); return; }
        if (row.yanked) { refused.push(`${label} is yanked on crates.io`); return; }
        registryDigest = String(row.cksum ?? "").toLowerCase();
      } else {
        const response = await packument(entry.name);
        if (response.status === 404) { refused.push(`${label} is not published on npm (no package ${entry.name})`); return; }
        const doc = response.body?.versions?.[entry.version];
        if (!doc) { refused.push(`${label} is not published on npm (staged or unpublished version)`); return; }
        if (doc.deprecated) { refused.push(`${label} is deprecated on npm: ${String(doc.deprecated).slice(0, 200)}`); return; }
        registryDigest = doc.dist?.integrity ?? "";
      }
    } catch (error) {
      if (!(error instanceof RightKitRegistryError) || !error.recoverable) throw error;
      outages.push({ label, message: error.message });
      return;
    }
    if (!registryDigest || registryDigest !== entry.digest.value) {
      refused.push(`${label} ${entry.ecosystem === "cargo" ? "checksum" : "integrity"} mismatch: lock has ${entry.digest.value}, registry has ${registryDigest || "none"}`);
      return;
    }
    verified.push({ ...entryRecord(entry), registryDigest, verifiedAt: observedAt, via: "live" });
  }
  await pool([...distinct.values()], verify);
  if (refused.length) throw new RightKitRegistryError("refused", `RightKit pins refused:\n${[...new Set(refused)].map((p) => `- ${p}`).join("\n")}`, { problems: refused });
  if (outages.length) {
    throw new RightKitRegistryError("registry-outage", `registry unreachable; cannot prove exact RightKit pins (recoverable: retry; there is no offline override for a fresh admission):\n${outages.map((o) => `- ${o.label}: ${o.message}`).join("\n")}`, { recoverable: true, problems: outages.map((o) => o.label) });
  }

  // Latest per distinct package. `latest` is ALWAYS the value the registry reports now. The cache keeps a separate
  // high-water mark that only guards against stale or out-of-order answers; it is never reported as the live state.
  const latest = [];
  const freshOutages = [];
  const conflicts = [];
  const names = [...new Map([...distinct.values()].map((e) => [`${e.ecosystem}\0${e.name}`, e])).values()];
  const stillPublished = async (entry, version) => {
    if (entry.ecosystem === "npm") {
      const doc = (await packument(entry.name)).body?.versions?.[version];
      return Boolean(doc) && !doc.deprecated;
    }
    const response = await index(entry.name);
    return String(response.body ?? "").split("\n").filter(Boolean).some((line) => { try { const row = JSON.parse(line); return row.vers === version && !row.yanked; } catch { return false; } });
  };
  await pool(names, async (entry) => {
    try {
      let value;
      if (entry.ecosystem === "npm") {
        value = (await packument(entry.name)).body?.["dist-tags"]?.latest;
      } else {
        const response = await client.getJson(`${REGISTRIES.cargoApi}${entry.name}`);
        value = response.body?.crate?.max_stable_version;
      }
      if (typeof value !== "string" || !EXACT_SEMVER.test(value)) throw new RightKitRegistryError("registry-outage", `${entry.name} registry answer carries no usable latest version`, { recoverable: true });
      const previous = cache.read("latest", entry.ecosystem, entry.name);
      const mark = previous?.highWater?.version ?? previous?.latest;
      const row = { ecosystem: entry.ecosystem, name: entry.name, latest: value, observedAt, via: "live" };
      let highWater = { version: value, observedAt };
      if (typeof mark === "string" && EXACT_SEMVER.test(mark) && compareVersions(mark, value) > 0) {
        if (entry.ecosystem === "npm" && await stillPublished(entry, mark)) {
          // An npm `latest` dist-tag is set by the maintainer and may be moved lower on purpose while the higher version stays
          // published: follow the registry. The high-water mark only protects against event-order downgrades of derived answers.
          row.moved = { from: mark, observedAt: previous?.highWater?.observedAt ?? previous?.observedAt };
        } else if (await stillPublished(entry, mark)) {
          // The higher version is still published, so the lower answer is stale or out of order: keep the mark, never downgrade.
          conflicts.push(`${entry.name}: registry reports latest ${value} but ${mark} (observed ${previous?.highWater?.observedAt ?? previous?.observedAt}) is still published; the answer is stale or out of order`);
          highWater = { version: mark, observedAt: previous?.highWater?.observedAt ?? previous?.observedAt };
          row.conflict = { highWater: mark };
        } else {
          // The previously observed version was withdrawn (yanked, deprecated or unpublished): the live state wins, no cache deletion needed.
          row.withdrawn = { version: mark, observedAt: previous?.highWater?.observedAt ?? previous?.observedAt };
        }
      }
      cache.write({ schema: 2, registry: entry.ecosystem, name: entry.name, latest: value, observedAt, highWater }, "latest", entry.ecosystem, entry.name);
      latest.push(row);
    } catch (error) {
      if (!(error instanceof RightKitRegistryError) || !error.recoverable) throw error;
      const record = cache.read("latest", entry.ecosystem, entry.name);
      if (!requireLiveLatest && record && typeof record.latest === "string") latest.push({ ecosystem: entry.ecosystem, name: entry.name, latest: record.latest, observedAt: record.observedAt, via: "cache", ageMs: Math.max(0, now() - Date.parse(record.observedAt)) });
      else freshOutages.push(`${entry.name}: ${error.message}${record?.latest ? ` (cached latest ${record.latest} observed ${record.observedAt} is last-observed, not current)` : ""}`);
    }
  });
  if (freshOutages.length) throw new RightKitRegistryError("registry-outage", `${requireLiveLatest ? "freshness cannot be established at admission" : "registry unreachable and no cached latest observation"} (recoverable: retry${requireLiveLatest ? "; a cached latest is only last-observed, never current" : ""}):\n${freshOutages.map((p) => `- ${p}`).join("\n")}`, { recoverable: true, problems: freshOutages });
  if (conflicts.length && requireLiveLatest) throw new RightKitRegistryError("eligibility-conflict", `eligibility conflict (recoverable: retry; no cache deletion is needed, a withdrawn version resolves itself):\n${conflicts.map((p) => `- ${p}`).join("\n")}`, { recoverable: true, problems: conflicts });

  const sort = (rows) => rows.sort((a, b) => `${a.ecosystem}${a.name}${a.version ?? ""}`.localeCompare(`${b.ecosystem}${b.name}${b.version ?? ""}`));
  const snapshot = {
    schema: SNAPSHOT_SCHEMA,
    // admission: latest was read live at build admission; inspection: doctor/informational (never bindable to a build).
    mode: requireLiveLatest ? "admission" : "inspection",
    observedAt,
    registries: { cargo: REGISTRIES.cargo, npm: REGISTRIES.npm },
    policyRevision: policyRevision(versions, releaseVersion),
    lockFiles: lockFiles.map((file) => path.relative(root, file).replaceAll("\\", "/")),
    entries: sort(verified.map((e) => ({ ...e, lockFiles: e.lockFiles.map((file) => path.relative(root, file).replaceAll("\\", "/")) }))),
    latest: sort(latest),
  };
  snapshot.digest = snapshotDigest(snapshot);
  return snapshot;
}

function entryRecord(entry) {
  return { ecosystem: entry.ecosystem, name: entry.name, version: entry.version, lockDigest: entry.digest.value, lockFiles: entry.lockFiles };
}

async function pool(items, task) {
  const queue = [...items];
  await Promise.all(Array.from({ length: Math.min(CONCURRENCY, queue.length) }, async () => {
    while (queue.length) await task(queue.shift());
  }));
}

// ---------------------------------------------------------------------------------------------- snapshot

export function snapshotDigest(snapshot) {
  const { digest, ...rest } = snapshot;
  return sha256(stable(rest));
}

export function writeSnapshot(file, snapshot) {
  mkdirSync(path.dirname(file), { recursive: true });
  const temp = `${file}.tmp-${process.pid}`;
  writeFileSync(temp, `${JSON.stringify(snapshot, null, 2)}\n`);
  renameSync(temp, file);
}

export function loadSnapshot(file) {
  let snapshot;
  try { snapshot = JSON.parse(readFileSync(file, "utf8")); } catch (error) { throw new RightKitRegistryError("bad-snapshot", `RightKit registry snapshot unreadable (${file}): ${error.message}`); }
  assertSnapshotIntegrity(snapshot);
  return snapshot;
}

const key3 = (e) => `${e.ecosystem}\0${e.name}\0${e.version}`;

/** Structure, digest and internal consistency: entries agree with their lock digest, latest covers exactly the entries. */
export function assertSnapshotIntegrity(snapshot) {
  if (!snapshot || snapshot.schema !== SNAPSHOT_SCHEMA || !Array.isArray(snapshot.entries) || !Array.isArray(snapshot.latest)) throw new RightKitRegistryError("bad-snapshot", "RightKit registry snapshot has an unsupported shape");
  if (snapshot.digest !== snapshotDigest(snapshot)) throw new RightKitRegistryError("bad-snapshot", "RightKit registry snapshot digest does not match its content");
  const problems = [];
  const seen = new Set();
  for (const entry of snapshot.entries) {
    const label = `${entry?.name} ${entry?.version}`;
    if (!["cargo", "npm"].includes(entry?.ecosystem) || typeof entry.name !== "string" || !EXACT_SEMVER.test(String(entry.version))) { problems.push(`malformed entry ${label}`); continue; }
    if (seen.has(key3(entry))) problems.push(`duplicate entry ${label}`);
    seen.add(key3(entry));
    if (typeof entry.lockDigest !== "string" || !entry.lockDigest || entry.registryDigest !== entry.lockDigest) problems.push(`${label}: registry digest does not equal the lock digest`);
    if (entry.via !== "live") problems.push(`${label}: not verified live`);
  }
  const wanted = new Set(snapshot.entries.map((e) => `${e.ecosystem}\0${e.name}`));
  const rows = new Set();
  for (const row of snapshot.latest) {
    const k = `${row?.ecosystem}\0${row?.name}`;
    if (!wanted.has(k)) problems.push(`latest row for ${row?.name} has no verified entry`);
    else if (rows.has(k)) problems.push(`duplicate latest row for ${row.name}`);
    else if (typeof row.latest !== "string" || !EXACT_SEMVER.test(row.latest)) problems.push(`latest row for ${row.name} has no usable version`);
    rows.add(k);
  }
  for (const k of wanted) if (!rows.has(k)) problems.push(`no latest observation for ${k.split("\0")[1]}`);
  if (problems.length) throw new RightKitRegistryError("bad-snapshot", `RightKit registry snapshot is inconsistent:\n${problems.map((p) => `- ${p}`).join("\n")}`, { problems });
}

/** A snapshot that may be bound to a build: it must be an admission, with every latest read live. */
export function assertAdmissionSnapshot(snapshot) {
  assertSnapshotIntegrity(snapshot);
  if (snapshot.mode !== "admission") throw new RightKitRegistryError("bad-snapshot", `RightKit registry snapshot was not taken at build admission (mode ${snapshot.mode ?? "<missing>"})`);
  const stale = snapshot.latest.filter((row) => row.via !== "live").map((row) => row.name);
  if (stale.length) throw new RightKitRegistryError("bad-snapshot", `RightKit registry snapshot carries non-live latest observations: ${stale.join(", ")}`);
}

/** The bound snapshot must describe exactly the current locked RightKit inputs: nothing missing, nothing extra, same digests. */
export function assertSnapshotCoversLocks(snapshot, { root, versions, ownership, extraLocks = [] }) {
  const owned = ownedRightKitNames(versions, ownership);
  const { entries } = collectLockedRightKit(root, owned, { extraLocks });
  const bound = new Map(snapshot.entries.map((e) => [key3(e), e]));
  const problems = [...coverageProblems(declaredRightKitDependencies(root, owned, extraLocks), entries)];
  const current = new Set();
  for (const entry of entries) {
    problems.push(...lockProblems(entry, owned, versions));
    current.add(key3(entry));
    const match = bound.get(key3(entry));
    if (!match) problems.push(`${entry.name} ${entry.version} is locked but not in the bound registry snapshot`);
    else if (entry.digest && match.lockDigest !== entry.digest.value) problems.push(`${entry.name} ${entry.version} lock digest changed since the bound registry snapshot`);
  }
  for (const e of snapshot.entries) if (!current.has(key3(e))) problems.push(`${e.name} ${e.version} is in the bound registry snapshot but not in the locked inputs`);
  if (problems.length) throw new RightKitRegistryError("snapshot-mismatch", `bound RightKit registry snapshot does not match the locked inputs:\n${[...new Set(problems)].map((p) => `- ${p}`).join("\n")}`, { problems });
}

/** Canonical lock inventory: the build's own inputs (requiredInputs, native Cargo locks, declared extra locks), not a rebuilt partial list. */
export function rightKitLockInventory({ requiredInputs = [], cargoLockPaths = [], nativeLayout = null, config = {}, root }) {
  const isLock = (file) => /^(?:Cargo\.lock|pnpm-lock\.yaml)$/.test(path.basename(file));
  return [...new Set([
    ...requiredInputs.filter(isLock), ...cargoLockPaths, ...(nativeLayout?.lockCandidates ?? []), ...(nativeLayout?.lockPath ? [nativeLayout.lockPath] : []),
    ...(config.cargoLocks ?? []).map((entry) => path.resolve(root, entry)),
  ].map((file) => path.resolve(file)))];
}

// ---------------------------------------------------------------------------------------------- admitted state (controller-owned)

export const SNAPSHOT_FILE = "rightkit-registry-snapshot.json";
export const ADMISSION_FILE = "rightkit-admission.json";
const LOCK_PAYLOAD_FILE = "release-inputs.lock.json";
const fileSha = (file) => sha256(readFileSync(file));

/**
 * The controller (build-release) retains, in the release state root it already owns, which snapshot was admitted for
 * which release: release id, commit, platform, the snapshot digest and the hash of the release-inputs lock payload.
 * Workers and resumes accept a snapshot only when this record, the snapshot and the lock payload agree. These are
 * trusted-local files (the state root the controller already uses for release-inputs.lock.json): the check catches
 * mix-ups, stale or foreign state and edits that leave the files inconsistent; it does NOT authenticate them, and a
 * writer of the state root can forge a consistent set. There is no signing layer.
 */
export function writeAdmissionRecord(stateRoot, { releaseId, commit, platform, snapshot }) {
  const record = { schema: 1, releaseId, commit, platform, snapshotDigest: snapshot.digest, lockPayloadSha256: fileSha(path.join(stateRoot, LOCK_PAYLOAD_FILE)) };
  const file = path.join(stateRoot, ADMISSION_FILE);
  const temp = `${file}.tmp-${process.pid}`;
  writeFileSync(temp, `${JSON.stringify(record, null, 2)}\n`);
  renameSync(temp, file);
  return record;
}

/** Load the admitted snapshot of THIS release from its state root, or refuse. `inPreflight` skips the lock payload hash (the controller rewrites it next). */
export function loadAdmittedSnapshot(stateRoot, { releaseId, commit, platform, snapshotFile = path.join(stateRoot, SNAPSHOT_FILE), inPreflight = false }) {
  const refuse = (message) => { throw new RightKitRegistryError("no-admission", `${message}; the RightKit registry snapshot is only accepted from this release's admitted state (${stateRoot}). Start a fresh build to re-admit.`); };
  if (path.resolve(snapshotFile) !== path.resolve(stateRoot, SNAPSHOT_FILE)) refuse(`snapshot ${snapshotFile} is not the admitted snapshot of this release`);
  const recordFile = path.join(stateRoot, ADMISSION_FILE);
  if (!existsSync(recordFile)) refuse("no admission record exists for this release");
  let record;
  try { record = JSON.parse(readFileSync(recordFile, "utf8")); } catch { refuse("the admission record is unreadable"); }
  if (record?.schema !== 1 || record.releaseId !== releaseId || record.commit !== commit || record.platform !== platform) refuse(`the admission record belongs to another release (${record?.releaseId ?? "?"} ${String(record?.commit ?? "").slice(0, 8)} ${record?.platform ?? "?"})`);
  if (!existsSync(snapshotFile)) refuse("the admitted snapshot file is missing");
  const snapshot = loadSnapshot(snapshotFile);
  assertAdmissionSnapshot(snapshot);
  if (snapshot.digest !== record.snapshotDigest) refuse("the snapshot is not the one admitted for this release (digest differs from the admission record)");
  if (!inPreflight) {
    const payloadFile = path.join(stateRoot, LOCK_PAYLOAD_FILE);
    if (!existsSync(payloadFile) || fileSha(payloadFile) !== record.lockPayloadSha256) refuse("the release input identity changed since admission (release-inputs.lock.json)");
    const payload = JSON.parse(readFileSync(payloadFile, "utf8"));
    if (payload.releaseId !== releaseId || payload.commit !== commit || payload.platform !== platform) refuse("the release input identity names another release");
  }
  return snapshot;
}

export function verifiedVersionMaps(snapshot) {
  const maps = { cargo: new Map(), npm: new Map() };
  for (const entry of snapshot.entries) {
    const set = maps[entry.ecosystem].get(entry.name) ?? new Set();
    set.add(entry.version);
    maps[entry.ecosystem].set(entry.name, set);
  }
  return maps;
}

// ---------------------------------------------------------------------------------------------- freshness + holds

function matchesPattern(name, pattern) {
  if (pattern.startsWith("/")) {
    try { return new RegExp(pattern.replace(/^\/|\/[a-z]*$/g, "")).test(name); } catch { return false; }
  }
  return name === pattern;
}
/**
 * Holds named in the nearest .rightkit/update.json (app root up to the repository boundary). A hold is a migration
 * lane task for right-update; it is reported next to a stale pin and never grants permission to seal it.
 * Only holds the file itself declares are read (policy.held, policy.tier.held); no built-in pattern is applied here.
 */
export function readUpdateHolds(root) {
  let dir = path.resolve(root);
  for (;;) {
    const file = path.join(dir, ".rightkit", "update.json");
    if (existsSync(file)) {
      try {
        const policy = JSON.parse(readFileSync(file, "utf8")).policy ?? {};
        return { file, names: Array.isArray(policy.held) ? policy.held.filter((v) => typeof v === "string") : [], patterns: Array.isArray(policy.tier?.held) ? policy.tier.held.filter((v) => typeof v === "string") : [] };
      } catch (error) {
        return { file, names: [], patterns: [], malformed: error.message };
      }
    }
    if (existsSync(path.join(dir, ".git")) || path.dirname(dir) === dir) return null;
    dir = path.dirname(dir);
  }
}

function holdFor(holds, name) {
  if (!holds) return null;
  if (holds.names.includes(name)) return "policy.held";
  const pattern = holds.patterns.find((p) => matchesPattern(name, p));
  return pattern ? `tier.held ${pattern}` : null;
}

/**
 * Pure verdict over a snapshot's observed latest values.
 *  - enforce (admission): EVERY pin behind latest is refused, held or not. A hold is a migration lane task
 *    (`tasks`), never permission to seal a stale pin; no exception is derived from right-update's hold list.
 *  - otherwise (doctor): the same rows are informational `newer` lines.
 */
export function evaluateRightKitFreshness(snapshot, holds, { enforce = false } = {}) {
  const refusals = [];
  const newer = [];
  const tasks = [];
  const warnings = [];
  const notes = [];
  if (holds?.malformed) warnings.push(`${holds.file} is unreadable (${holds.malformed}); no holds read`);
  const newest = new Map(snapshot.latest.map((l) => [`${l.ecosystem}\0${l.name}`, l]));
  for (const entry of snapshot.entries) {
    const seen = newest.get(`${entry.ecosystem}\0${entry.name}`);
    if (!seen || compareVersions(entry.version, seen.latest) >= 0) continue;
    const age = seen.via === "cache" ? ` (latest from cached observation ${seen.observedAt}, ${formatAge(seen.ageMs)} old; last-observed, not current)` : "";
    const hold = holdFor(holds, entry.name);
    const line = `${entry.name} pinned ${entry.version} -> latest ${seen.latest}${age}: ${FIX}`;
    if (hold) tasks.push(`${entry.name} pinned ${entry.version}, latest ${seen.latest}: held by .rightkit/update.json (${hold}); migration lane task, not current and not permission to seal`);
    (enforce ? refusals : newer).push(hold ? `${line} [held by .rightkit/update.json (${hold}): a hold is a lane task, not permission to seal stale]` : line);
  }
  for (const l of snapshot.latest) {
    if (l.via === "cache") notes.push(`${l.name} latest ${l.latest} taken from cache, ${formatAge(l.ageMs)} old`);
    if (l.conflict) notes.push(`${l.name}: registry reports ${l.latest} but ${l.conflict.highWater} is still published (stale or out-of-order answer)`);
    if (l.moved) notes.push(`${l.name}: the npm latest tag moved from ${l.moved.from} to ${l.latest}; the registry is followed`);
    if (l.withdrawn) notes.push(`${l.name}: previously observed latest ${l.withdrawn.version} was withdrawn; the live registry state (${l.latest}) is used`);
  }
  return { refusals, newer, tasks, warnings, notes };
}

export function formatAge(ms) {
  const minutes = Math.round((ms ?? 0) / 60000);
  if (minutes < 120) return `${minutes} minutes`;
  const hours = Math.round(minutes / 60);
  return hours < 72 ? `${hours} hours` : `${Math.round(hours / 24)} days`;
}

// ---------------------------------------------------------------------------------------------- orchestration

/**
 * One entry point for the doctor, the build and the release worker.
 *  - snapshotFile + reuse: consume that bound snapshot (no network) after checking it covers the current locks.
 *  - otherwise resolve live; with snapshotFile + write, bind the result there for later phases.
 * The freshness verdict is recomputed from the snapshot every time, so a changed hold takes effect on resume.
 */
export async function prepareRightKitResolution({ root, snapshotFile = null, boundSnapshot = null, reuse = false, write = false, admission = false, env = process.env, ...options }) {
  const { versions, ownership } = options;
  let snapshot;
  let source;
  if (boundSnapshot) {
    assertAdmissionSnapshot(boundSnapshot);
    snapshot = boundSnapshot;
    assertSnapshotCoversLocks(snapshot, { root, versions, ownership, extraLocks: options.extraLocks });
    source = "bound";
  } else if (reuse) {
    // A resume or a worker never replaces frozen evidence: a missing admitted snapshot is refused, not re-resolved.
    throw new RightKitRegistryError("snapshot-missing", "the admitted RightKit registry snapshot is missing; refusing to resolve again over frozen evidence. Start a fresh build to re-admit.");
  } else {
    snapshot = await resolveRightKitPins({ root, cacheDir: options.cacheDir ?? defaultRegistryCacheDir(env), requireLiveLatest: admission, ...options });
    source = "resolved";
  }
  // Freshness is an admission gate. A bound snapshot is frozen evidence: seal, resume and upload never re-check it,
  // so a publication after admission neither mutates nor restarts an admitted build.
  const enforce = admission && source === "resolved";
  const freshness = evaluateRightKitFreshness(snapshot, readUpdateHolds(root), { enforce });
  if (freshness.refusals.length) {
    throw new RightKitRegistryError("stale-pin", `RightKit pins are behind the latest published version (build admission refused):\n${freshness.refusals.map((p) => `- ${p}`).join("\n")}`, { problems: freshness.refusals });
  }
  if (source === "resolved" && snapshotFile && write) writeSnapshot(snapshotFile, snapshot);
  return { snapshot, source, freshness, verified: verifiedVersionMaps(snapshot), summary: summarizeSnapshot(snapshot, source) };
}

export function summarizeSnapshot(snapshot, source) {
  const count = (eco) => snapshot.entries.filter((e) => e.ecosystem === eco).length;
  const how = source === "bound" ? "bound snapshot (trusted-local record, not re-checked against the registry, not authenticated)" : "resolved snapshot";
  return `${snapshot.entries.length} exact pins verified against the registries (cargo ${count("cargo")}, npm ${count("npm")}); ${how} ${snapshot.digest.slice(0, 12)} observed ${snapshot.observedAt}; policy ${snapshot.policyRevision}`;
}

/** Evidence block for the sealed manifest. Metadata agreement (lock digest = registry digest) only, not byte proof. */
export function registryEvidence(snapshot) {
  return { schema: EVIDENCE_VERSION, digest: snapshot.digest, snapshot };
}

/** Manifest fields a new seal carries: the evidence and an explicit marker that makes its absence detectable. */
export function registryManifestFields(snapshot, toolVersion) {
  return { rightkitRegistryEvidence: EVIDENCE_VERSION, ...(toolVersion ? { rightReleaseVersion: toolVersion } : {}), rightkitRegistry: registryEvidence(snapshot) };
}

export const LEGACY_RELEASES_ENV = "RIGHT_RELEASE_LEGACY_REGISTRY_RELEASES";
const NEW_SEAL_FROM = "0.6.0";
const legacyReleaseIdsFromEnv = (env = process.env) => String(env[LEGACY_RELEASES_ENV] ?? "").split(",").map((v) => v.trim()).filter(Boolean);

/**
 * A manifest must carry versioned registry evidence. New seals record `rightReleaseVersion` (>= 0.6.0) with the
 * evidence; stripping the evidence from such a seal is refused whatever else is set. Only an explicitly identified
 * pre-0.6.0 artifact is exempt: its manifest records no tool version (or one below 0.6.0) AND its release id is
 * named in `legacyReleaseIds` / RIGHT_RELEASE_LEGACY_REGISTRY_RELEASES. There is no global switch.
 */
export function assertManifestRegistryEvidence(manifest, { legacyReleaseIds = legacyReleaseIdsFromEnv() } = {}) {
  const marker = manifest?.rightkitRegistryEvidence;
  const evidence = manifest?.rightkitRegistry;
  const recorded = manifest?.rightReleaseVersion;
  if (recorded !== undefined && !(typeof recorded === "string" && /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(recorded))) {
    throw new Error(`sealed manifest rightReleaseVersion is malformed (${JSON.stringify(recorded)}); refusing (only an absent or valid pre-${NEW_SEAL_FROM} version is legacy)`);
  }
  if (marker === undefined && evidence === undefined) {
    let newSeal = false;
    try { newSeal = typeof recorded === "string" && compareVersions(recorded, NEW_SEAL_FROM) >= 0; } catch { newSeal = false; }
    if (newSeal) throw new Error(`sealed manifest was produced by @rightkit/release ${recorded} but carries no RightKit registry evidence; refusing (a stripped new seal is never a legacy seal)`);
    if (typeof manifest?.releaseId === "string" && legacyReleaseIds.includes(manifest.releaseId)) return { legacy: true };
    throw new Error(`sealed manifest carries no RightKit registry evidence; refusing (only a pre-${NEW_SEAL_FROM} release named in ${LEGACY_RELEASES_ENV} is exempt)`);
  }
  if (marker !== EVIDENCE_VERSION) throw new Error(`sealed manifest registry evidence marker must be ${EVIDENCE_VERSION}, got ${marker === undefined ? "<missing>" : JSON.stringify(marker)}`);
  assertRegistryEvidence(evidence);
  return { legacy: false };
}

export function assertRegistryEvidence(evidence) {
  if (!evidence || evidence.schema !== EVIDENCE_VERSION) throw new Error("sealed manifest rightkitRegistry evidence is missing or has an unsupported schema");
  if (evidence.digest !== evidence.snapshot?.digest) throw new Error("sealed manifest rightkitRegistry digest does not match its snapshot");
  try { assertAdmissionSnapshot(evidence.snapshot); } catch (error) { throw new Error(`sealed manifest rightkitRegistry rejected: ${error.message}`); }
}
