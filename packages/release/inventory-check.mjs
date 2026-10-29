#!/usr/bin/env node
// RightKit inventory check: registry parity, account parity, outdated third-party
// requirements, app adoption, and new-package hygiene. Public endpoints only; no
// credentials or tokens are read. Invoked as `right-release inventory check`.
import { createHash } from "node:crypto";
import { realpathSync } from "node:fs";
import { mkdir, readFile, readdir, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parse as parseToml } from "smol-toml";
import {
  APP_REPOS,
  DEFAULT_WORKSPACE,
  driftOf,
  generateAdoption,
  generateLocal,
} from "../../scripts/generate-index.mjs";
import { PORT_MATRIX_OUTPUTS, generatePortMatrix } from "../../scripts/generate-port-matrix.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
export const DEFAULT_REPO_ROOT = resolve(HERE, "..", "..");
export const DEFAULT_CACHE_DIR = join(tmpdir(), "rightkit-inventory-cache");
const CACHE_TTL_MS = 60 * 60 * 1000;
const CONCURRENCY = 8;
const REQUEST_TIMEOUT_MS = 15_000;
const CRATES_API_INTERVAL_MS = 1000;
const USER_AGENT = "rightkit-inventory-check/1 (+https://github.com/bogusyogi/rightkit)";
const EXCLUDED_PREFIX = "build-control-";
const OWNER_PROBE_CRATE = "rightkit-fs";
const EXTRA_ACCOUNT_CRATES = Object.freeze([]);
const SWIFT_REPO = "Orthic-Labs/rightkit"; // RightKitSwift is tagged on the public repository itself
const SWIFT_KEY = "rightkit-swift";
const SWIFT_NAME = "RightKitSwift";
const SKIP_DIRS = new Set(["node_modules", ".git", "target", "dist", "build", ".cache"]);
const NON_REGISTRY = /^(workspace:|file:|link:|portal:|git\+|git:|github:|https?:|npm:)|\//;

export const ACTION = "action";
export const INFO = "info";
export const SEVERITY = Object.freeze({
  "never-published": ACTION,
  "source-unpublished": ACTION,
  "staged-not-published": ACTION,
  yanked: ACTION,
  deprecated: ACTION,
  "registry-newer-than-source": ACTION,
  "feed-out-of-date": ACTION,
  "published-but-private": ACTION,
  "orphan-registry-package": ACTION,
  "missing-from-registry": ACTION,
  "outdated-requirement": ACTION,
  "outdated-pin": ACTION,
  "unpublished-pin": ACTION,
  "split-app-pins": ACTION,
  "duplicate-app-copies": ACTION,
  "generated-doc-drift": ACTION,
  "missing-ownership-file": ACTION,
  "missing-readme": ACTION,
  "missing-from-catalog": ACTION,
  unverified: ACTION,
  "private-crate": INFO,
  "private-package": INFO,
  "duplicate-lock-versions": INFO,
  "unsupported-requirement": INFO,
  "app-missing": INFO,
});
export const SECTIONS = Object.freeze({
  registry: "1. Registry parity",
  account: "2. Account parity",
  thirdParty: "3. Third-party requirements",
  apps: "4. App adoption",
  generated: "5. Generated docs",
  hygiene: "6. New-package hygiene",
});

export function issue(section, subject, kind, detail, severity = SEVERITY[kind] ?? ACTION) {
  return { section, subject, kind, severity, detail };
}

// ---------- versions and requirements ----------

export function parseVersion(text) {
  const match = /^v?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/.exec(String(text ?? "").trim());
  return match ? { major: Number(match[1]), minor: Number(match[2]), patch: Number(match[3]), pre: match[4] ?? null } : null;
}

function comparePair(a, b) {
  for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
  return 0;
}
const partsOfVersion = (v) => [v.major, v.minor, v.patch];

// Compares two version strings or parsed versions; throws when either is not semver.
export function compareVersions(a, b) {
  const x = typeof a === "string" ? parseVersion(a) : a;
  const y = typeof b === "string" ? parseVersion(b) : b;
  if (!x || !y) throw new TypeError(`not a semver version: ${!x ? a : b}`);
  const c = comparePair(partsOfVersion(x), partsOfVersion(y));
  if (c) return c;
  if (x.pre === y.pre) return 0;
  if (x.pre === null) return 1;
  if (y.pre === null) return -1;
  return x.pre < y.pre ? -1 : x.pre > y.pre ? 1 : 0;
}

// Highest version that is not yanked, deprecated, or a prerelease.
export function latestStable(versions) {
  let best = null;
  for (const entry of versions) {
    if (entry.yanked || entry.deprecated) continue;
    const parsed = parseVersion(entry.version);
    if (!parsed || parsed.pre) continue;
    if (!best || compareVersions(parsed, best.parsed) > 0) best = { version: entry.version, parsed };
  }
  return best?.version ?? null;
}

function normalizeRange(text) {
  return String(text ?? "").trim().replace(/(\^|~|>=|<=|>|<|=)\s+/g, "$1");
}

// One comparator: op, numeric parts (n = count of leading numeric parts before a wildcard).
function parseComparator(token, ecosystem) {
  const match = /^(\^|~|>=|<=|>|<|=)?v?([0-9xX*]+(?:\.[0-9xX*]+){0,2})(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]*)?$/.exec(token);
  if (!match) return null;
  const numeric = [];
  for (const part of match[2].split(".")) {
    if (!/^\d+$/.test(part)) break;
    numeric.push(Number(part));
  }
  const [M = 0, m = 0, p = 0] = numeric;
  const op = match[1] ?? (ecosystem === "cargo" ? "^" : "");
  return { op, n: numeric.length, M, m, p };
}

function matchComparator(c, v) {
  const lo = [c.M, c.m, c.p];
  const vv = partsOfVersion(v);
  switch (c.op) {
    case "":
    case "=":
      if (c.n === 0) return true;
      if (c.n === 3) return comparePair(vv, lo) === 0;
      return vv[0] === c.M && (c.n < 2 || vv[1] === c.m);
    case "^": {
      if (c.n === 0) return true;
      const upper = c.n === 1 || c.M > 0 ? [c.M + 1, 0, 0]
        : c.n === 2 || c.m > 0 ? [0, c.m + 1, 0]
          : [0, 0, c.p + 1];
      return comparePair(vv, lo) >= 0 && comparePair(vv, upper) < 0;
    }
    case "~": {
      if (c.n === 0) return true;
      const upper = c.n === 1 ? [c.M + 1, 0, 0] : [c.M, c.m + 1, 0];
      return comparePair(vv, lo) >= 0 && comparePair(vv, upper) < 0;
    }
    case ">=": return comparePair(vv, lo) >= 0;
    case ">": return comparePair(vv, lo) > 0;
    case "<=": return comparePair(vv, lo) <= 0;
    case "<": return comparePair(vv, lo) < 0;
    default: return false;
  }
}

// True when `version` lies inside the requirement; false when it is outside; null when the
// requirement uses syntax this check does not model (tags, git, local specifiers).
export function satisfies(range, version, ecosystem = "cargo") {
  const v = parseVersion(version);
  if (!v) return null;
  const text = normalizeRange(range);
  if (text === "") return true;
  let matched = false;
  for (const group of text.split("||")) {
    const tokens = group.trim().split(/\s*,\s*|\s+/).filter(Boolean);
    let comparators;
    if (tokens.length === 3 && tokens[1] === "-") {
      const lo = parseComparator(tokens[0], ecosystem);
      const hi = parseComparator(tokens[2], ecosystem);
      if (!lo || !hi) return null;
      comparators = [{ op: ">=", n: 3, M: lo.M, m: lo.m, p: lo.p }];
      if (hi.n === 3) comparators.push({ op: "<=", n: 3, M: hi.M, m: hi.m, p: hi.p });
      else if (hi.n === 2) comparators.push({ op: "<", n: 3, M: hi.M, m: hi.m + 1, p: 0 });
      else if (hi.n === 1) comparators.push({ op: "<", n: 3, M: hi.M + 1, m: 0, p: 0 });
    } else {
      comparators = tokens.map((token) => parseComparator(token, ecosystem));
    }
    if (comparators.some((c) => !c)) return null;
    if (comparators.every((c) => matchComparator(c, v))) matched = true;
  }
  return matched;
}

// The lower bound a requirement pins to, as "M.m.p"; null for upper-bound-only or unsupported syntax.
export function baseVersion(range, ecosystem = "cargo") {
  const first = normalizeRange(range).split("||")[0].trim().split(/\s*,\s*|\s+/).filter(Boolean)[0];
  const c = first ? parseComparator(first, ecosystem) : null;
  if (!c || c.op === "<" || c.op === "<=") return null;
  return `${c.M}.${c.m}.${c.p}`;
}

export function behindKind(range, latest, ecosystem = "cargo") {
  const base = baseVersion(range, ecosystem);
  const b = base ? parseVersion(base) : null;
  const l = parseVersion(latest);
  if (!b || !l) return "unknown";
  if (l.major > b.major) return "major";
  if (l.minor > b.minor) return "minor";
  return "patch";
}

// ---------- bounded concurrency ----------

export async function mapLimit(items, limit, fn) {
  const results = new Array(items.length);
  let next = 0;
  const workers = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) {
      const index = next++;
      results[index] = await fn(items[index], index);
    }
  });
  await Promise.all(workers);
  return results;
}

// ---------- HTTP with a one-hour on-disk cache ----------

export function crateIndexPath(name) {
  const n = name.toLowerCase();
  if (n.length === 1) return `1/${n}`;
  if (n.length === 2) return `2/${n}`;
  if (n.length === 3) return `3/${n[0]}/${n}`;
  return `${n.slice(0, 2)}/${n.slice(2, 4)}/${n}`;
}

const sleep = (ms) => new Promise((resolveSleep) => setTimeout(resolveSleep, ms));

async function readCache(file, nowMs, ttlMs) {
  try {
    const entry = JSON.parse(await readFile(file, "utf8"));
    return nowMs - entry.at < ttlMs && typeof entry.text === "string" ? { status: entry.status, text: entry.text } : null;
  } catch {
    return null;
  }
}

async function writeCache(file, entry) {
  try {
    await mkdir(dirname(file), { recursive: true });
    await writeFile(file, JSON.stringify(entry));
  } catch {
    // The cache is best-effort; a failed write only costs a refetch.
  }
}

// Returns get(url) -> { status, text }. Resolves 200 and 404; throws on anything else.
// Requests to crates.io are spaced to one per second, per the crates.io crawler policy.
export function createFetcher(options = {}) {
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  const cacheDir = "cacheDir" in options ? options.cacheDir : DEFAULT_CACHE_DIR;
  const now = options.now ?? Date.now;
  const ttlMs = options.ttlMs ?? CACHE_TTL_MS;
  const intervalMs = options.crateApiIntervalMs ?? CRATES_API_INTERVAL_MS;
  let crateSlot = Promise.resolve();
  const get = async (url, { headers = {} } = {}) => {
    const file = cacheDir ? join(cacheDir, `${createHash("sha256").update(url).digest("hex")}.json`) : null;
    if (file) {
      const hit = await readCache(file, now(), ttlMs);
      if (hit) return hit;
    }
    if (new URL(url).hostname === "crates.io") {
      const slot = crateSlot.then(() => sleep(intervalMs));
      crateSlot = slot;
      await slot;
    }
    const response = await fetchImpl(url, {
      headers: { "user-agent": USER_AGENT, accept: "application/json", ...headers },
      signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    });
    if (response.status !== 200 && response.status !== 404) throw new Error(`HTTP ${response.status} from ${new URL(url).host}`);
    const result = { status: response.status, text: await response.text() };
    if (file) await writeCache(file, { at: now(), ...result });
    return result;
  };
  return get;
}

async function jsonOrNull(get, url, options) {
  const response = await get(url, options);
  return response.status === 404 ? null : JSON.parse(response.text);
}

export async function fetchCrateVersions(get, name) {
  const response = await get(`https://index.crates.io/${crateIndexPath(name)}`);
  if (response.status === 404) return { versions: [] };
  const versions = response.text.split("\n").filter(Boolean).map((line) => {
    const entry = JSON.parse(line);
    return { version: entry.vers, yanked: entry.yanked === true, deprecated: null };
  });
  return { versions };
}

export async function fetchNpmPackage(get, name) {
  const doc = await jsonOrNull(get, `https://registry.npmjs.org/${encodeURIComponent(name)}`, {
    headers: { accept: "application/vnd.npm.install-v1+json, application/json;q=0.5" },
  });
  if (!doc) return { versions: [], latest: null };
  const versions = Object.entries(doc.versions ?? {}).map(([version, meta]) => ({
    version,
    yanked: false,
    deprecated: typeof meta?.deprecated === "string" && meta.deprecated ? meta.deprecated : null,
  }));
  return { versions, latest: latestStable(versions) };
}

export async function fetchSwiftTags(get) {
  const response = await get(`https://api.github.com/repos/${SWIFT_REPO}/tags?per_page=100`, {
    headers: { accept: "application/vnd.github+json" },
  });
  if (response.status !== 200) throw new Error(`GitHub tags unavailable without credentials (HTTP ${response.status})`);
  const versions = JSON.parse(response.text).map((tag) => ({ version: String(tag.name).replace(/^v/, ""), yanked: false, deprecated: null }))
    .filter((entry) => parseVersion(entry.version));
  return { versions, latest: latestStable(versions) };
}

// One memoized lookup per (ecosystem, name) so sections share registry reads.
export function createLiveIndex(get) {
  const memo = new Map();
  return (ecosystem, name) => {
    const key = `${ecosystem}:${name}`;
    if (!memo.has(key)) {
      const lookup = ecosystem === "cargo"
        ? fetchCrateVersions(get, name).then(({ versions }) => ({ versions, latest: latestStable(versions) }))
        : ecosystem === "npm" ? fetchNpmPackage(get, name)
          : Promise.reject(new Error(`no registry for ${ecosystem}`));
      memo.set(key, lookup);
    }
    return memo.get(key);
  };
}

// ---------- source inventory ----------

async function childDirs(dir) {
  try {
    return (await readdir(dir, { withFileTypes: true })).filter((e) => e.isDirectory()).map((e) => e.name).sort();
  } catch {
    return [];
  }
}

async function exists(path) {
  try {
    await stat(path);
    return true;
  } catch {
    return false;
  }
}

async function readText(path) {
  try {
    return await readFile(path, "utf8");
  } catch (error) {
    if (error.code === "ENOENT") return null;
    throw error;
  }
}

async function readToml(path) {
  const text = await readText(path);
  return text === null ? null : parseToml(text);
}

async function readJson(path) {
  const text = await readText(path);
  return text === null ? null : JSON.parse(text);
}

// Depth-limited file walk for the named files; skips dependency and build output.
async function walkNamed(root, names, maxDepth, depth = 0, out = []) {
  for (const entry of await readdir(root, { withFileTypes: true }).catch(() => [])) {
    if (entry.isDirectory()) {
      if (!SKIP_DIRS.has(entry.name) && !entry.name.startsWith(".") && depth < maxDepth) {
        await walkNamed(join(root, entry.name), names, maxDepth, depth + 1, out);
      }
    } else if (entry.isFile() && names.includes(entry.name)) {
      out.push(join(root, entry.name));
    }
  }
  return out;
}

export function cargoDeclarations(manifest) {
  const tables = [];
  if (manifest.workspace?.dependencies) tables.push(manifest.workspace.dependencies);
  const kinds = ["dependencies", "dev-dependencies", "build-dependencies"];
  for (const kind of kinds) if (manifest[kind]) tables.push(manifest[kind]);
  for (const target of Object.values(manifest.target ?? {})) {
    for (const kind of kinds) if (target[kind]) tables.push(target[kind]);
  }
  const out = [];
  for (const table of tables) {
    for (const [alias, spec] of Object.entries(table)) {
      if (typeof spec === "string") out.push({ name: alias, req: spec });
      else if (spec && typeof spec === "object" && typeof spec.version === "string") out.push({ name: spec.package ?? alias, req: spec.version });
      // path, git, and workspace-inherited entries carry no registry requirement here.
    }
  }
  return out;
}

export function npmDeclarations(pkg) {
  const out = [];
  for (const kind of ["dependencies", "devDependencies", "optionalDependencies"]) {
    for (const [name, req] of Object.entries(pkg[kind] ?? {})) {
      if (typeof req === "string" && !NON_REGISTRY.test(req.trim())) out.push({ name, req });
    }
  }
  return out;
}

export async function loadSource(repoRoot) {
  const releaseDir = join(repoRoot, "packages", "release");
  const versions = await readJson(join(releaseDir, "rightkit-versions.json"));
  const catalog = await readJson(join(releaseDir, "rightkit-ownership.json"));
  const catalogNames = new Set((catalog?.capabilities ?? []).map((c) => c.package));
  const records = [];

  for (const dir of await childDirs(join(repoRoot, "crates"))) {
    if (dir.startsWith(EXCLUDED_PREFIX)) continue;
    const base = join(repoRoot, "crates", dir);
    const manifest = await readToml(join(base, "Cargo.toml"));
    if (!manifest?.package) continue;
    const pkg = manifest.package;
    const publish = pkg.publish;
    records.push({
      ecosystem: "cargo",
      name: pkg.name,
      dir: base,
      version: typeof pkg.version === "string" ? pkg.version : null,
      isPublic: !(publish === false || (Array.isArray(publish) && publish.length === 0)),
      manifest,
    });
  }

  for (const dir of await childDirs(join(repoRoot, "packages"))) {
    if (dir.startsWith(EXCLUDED_PREFIX)) continue;
    const base = join(repoRoot, "packages", dir);
    const pkg = await readJson(join(base, "package.json"));
    if (!pkg?.name) continue;
    records.push({ ecosystem: "npm", name: pkg.name, dir: base, version: pkg.version ?? null, isPublic: pkg.private !== true, manifest: pkg });
  }

  const swiftDir = join(repoRoot, "swift", SWIFT_NAME);
  const swiftVersion = (await readText(join(swiftDir, "VERSION")))?.trim() ?? null;
  if (swiftVersion !== null) {
    records.push({ ecosystem: "swift", name: SWIFT_NAME, dir: swiftDir, version: swiftVersion, isPublic: true, manifest: null });
  }

  for (const record of records) {
    record.hygiene = {
      hasOwnership: await exists(join(record.dir, "rightkit-ownership.json")),
      hasReadme: await exists(join(record.dir, "README.md")),
      inCatalog: catalogNames.has(record.name),
    };
    Object.assign(record, feedFor(record, versions ?? {}));
  }
  return { repoRoot, versions: versions ?? {}, records };
}

// rightkit-versions.json: recorded (last published) and staged versions per package.
export function feedFor(record, versions) {
  if (record.ecosystem === "swift") {
    return { recorded: versions.swift?.[SWIFT_KEY]?.version ?? null, staged: versions.stagedSwift?.[SWIFT_KEY]?.version ?? null };
  }
  const bucket = record.ecosystem === "cargo" ? "cargo" : "npm";
  const staged = record.ecosystem === "cargo" ? "stagedCargo" : "stagedNpm";
  return {
    recorded: versions.registry?.[bucket]?.[record.name] ?? versions[bucket]?.[record.name] ?? null,
    staged: versions[staged]?.[record.name] ?? null,
  };
}

const subjectOf = (record) => `${record.ecosystem === "cargo" ? "crate" : record.ecosystem} ${record.name}`;

// ---------- section 1: registry parity (pure) ----------

export function assessParity(record) {
  const subject = subjectOf(record);
  const row = {
    ecosystem: record.ecosystem, name: record.name, source: record.version ?? null, public: record.isPublic,
    latest: null, recorded: record.recorded ?? null, staged: record.staged ?? null, status: "ok",
  };
  const findings = [];
  const add = (kind, detail, severity) => findings.push(issue("registry", subject, kind, detail, severity));

  if (record.liveError) {
    add("unverified", `registry lookup failed: ${record.liveError}`, record.ecosystem === "swift" ? INFO : ACTION);
    row.status = "unverified";
    return { row, findings };
  }
  const live = record.live ?? { versions: [], latest: null };
  const versions = live.versions;
  row.latest = live.latest ?? null;

  if (!record.isPublic) {
    if (versions.length > 0) add("published-but-private", `${versions.length} version(s) on the registry; source is private`);
    else add(record.ecosystem === "cargo" ? "private-crate" : "private-package", "private source, not published (informational)");
  } else if (versions.length === 0) {
    add("never-published", `no versions on the registry; source ${record.version ?? "unknown"}`);
  } else {
    const src = record.version;
    if (!src || !parseVersion(src)) add("unverified", "source version could not be read");
    const entry = src ? versions.find((v) => v.version === src) : undefined;
    if (src && !entry) add("source-unpublished", `source ${src} is not on the registry${record.staged ? ` (feed stages ${record.staged})` : ""}`);
    if (entry?.yanked) add("yanked", `source ${src} is yanked on the registry`);
    if (entry?.deprecated) add("deprecated", `source ${src} is deprecated: ${entry.deprecated}`);
    if (record.staged && record.staged !== src && !versions.some((v) => v.version === record.staged)) {
      add("staged-not-published", `feed stages ${record.staged}; it is not on the registry`);
    }
    if (!live.latest) add("yanked", "every published version is yanked or deprecated");
    else if (src && parseVersion(src) && compareVersions(live.latest, src) > 0) {
      add("registry-newer-than-source", `registry latest ${live.latest} is newer than source ${src}`);
    }
    if (!record.recorded) {
      if (live.latest) add("feed-out-of-date", `rightkit-versions.json has no entry; registry latest ${live.latest}`);
    } else if (!versions.some((v) => v.version === record.recorded)) {
      add("feed-out-of-date", `feed records ${record.recorded}, which is not on the registry`);
    } else if (live.latest && compareVersions(live.latest, record.recorded) > 0) {
      add("feed-out-of-date", `feed records ${record.recorded}; registry latest is ${live.latest}`);
    }
  }
  row.status = findings.length ? findings.map((f) => f.kind).join(", ") : "ok";
  return { row, findings };
}

// ---------- section 2: account parity ----------

async function searchCrateNames(get) {
  const names = [];
  for (let page = 1; page <= 10; page++) {
    const body = await jsonOrNull(get, `https://crates.io/api/v1/crates?q=rightkit&per_page=100&page=${page}`);
    const crates = body?.crates ?? [];
    names.push(...crates.map((c) => c.name));
    if (crates.length < 100 || names.length >= (body?.meta?.total ?? 0)) break;
  }
  return [...new Set(names)];
}

// Lists @rightkit/* via the scope query. That query has returned no results on some days, so an
// empty answer falls back to a plain "@rightkit" text search and the report says so.
async function searchNpmNames(get) {
  for (const text of ["scope:rightkit", "%40rightkit"]) {
    const names = [];
    for (let from = 0; from < 2500; from += 250) {
      const body = await jsonOrNull(get, `https://registry.npmjs.org/-/v1/search?text=${text}&size=250&from=${from}`);
      const objects = body?.objects ?? [];
      names.push(...objects.map((o) => o.package?.name).filter((n) => typeof n === "string" && n.startsWith("@rightkit/")));
      if (objects.length < 250 || from + 250 >= (body?.total ?? 0)) break;
    }
    if (names.length) return { names: [...new Set(names)], query: text };
  }
  return { names: [], query: null };
}

async function checkAccount({ records, get, findings }) {
  const section = { crates: { verified: false, owned: [] }, npm: { verified: false, owned: [] } };
  try {
    const probe = await jsonOrNull(get, `https://crates.io/api/v1/crates/${OWNER_PROBE_CRATE}/owner_user`);
    const ownerIds = new Set((probe?.users ?? []).map((u) => u.id));
    if (ownerIds.size === 0) throw new Error(`no owner found for ${OWNER_PROBE_CRATE}`);
    const names = await searchCrateNames(get);
    const candidates = [...new Set([...names.filter((n) => n.startsWith("rightkit-")), ...EXTRA_ACCOUNT_CRATES])];
    const owned = await mapLimit(candidates, CONCURRENCY, async (name) => {
      const body = await jsonOrNull(get, `https://crates.io/api/v1/crates/${name}/owner_user`);
      return (body?.users ?? []).some((u) => ownerIds.has(u.id)) ? name : null;
    });
    section.crates = { verified: true, owned: owned.filter(Boolean).sort(), candidates: candidates.length };
  } catch (error) {
    findings.push(issue("account", "crates.io", "unverified", `crate account listing failed: ${error.message}`));
  }
  try {
    const listing = await searchNpmNames(get);
    section.npm = { verified: true, owned: listing.names.sort(), query: listing.query };
    if (listing.query !== "scope:rightkit") {
      findings.push(issue("account", "npm", "unverified", `scope:rightkit search returned nothing; listed via ${listing.query ?? "no query"} instead`, INFO));
    }
  } catch (error) {
    findings.push(issue("account", "npm", "unverified", `npm scope listing failed: ${error.message}`));
  }

  for (const [ecosystem, key] of [["cargo", "crates"], ["npm", "npm"]]) {
    const listing = section[key];
    if (!listing.verified) continue;
    const known = new Set(records.filter((r) => r.ecosystem === ecosystem).map((r) => r.name));
    const owned = new Set(listing.owned);
    for (const name of listing.owned) {
      if (!known.has(name)) findings.push(issue("account", `${ecosystem === "cargo" ? "crate" : "npm"} ${name}`, "orphan-registry-package", "published under our account with no source in the repo"));
    }
    for (const record of records.filter((r) => r.ecosystem === ecosystem && r.isPublic)) {
      // npm's listing cannot prove ownership; a published @rightkit name is ours by scope, so
      // only never-published npm packages are reported here (section 1 covers them).
      if (owned.has(record.name) || (ecosystem === "npm" && record.live?.versions?.length)) continue;
      const hasVersions = (record.live?.versions?.length ?? 0) > 0;
      findings.push(issue("account", subjectOf(record), "missing-from-registry",
        `not listed under our account${hasVersions ? "; registry has versions under another owner or the listing lags" : "; never published"}`));
    }
  }
  return section;
}

// ---------- section 3: third-party requirements ----------

async function checkThirdParty({ repoRoot, records, live, findings }) {
  const internalCargo = new Set(records.filter((r) => r.ecosystem === "cargo").map((r) => r.name));
  const declarations = new Map();
  const add = (ecosystem, name, req, where) => {
    const key = `${ecosystem}:${name}:${req}`;
    if (!declarations.has(key)) declarations.set(key, { ecosystem, name, req, wheres: new Set() });
    declarations.get(key).wheres.add(where);
  };

  // Every crate is its own Cargo workspace (rightkit#1285): there is no root manifest, each crate declares its own requirements.
  const cargoSources = [];
  for (const record of records.filter((r) => r.ecosystem === "cargo")) {
    cargoSources.push({ where: relative(repoRoot, join(record.dir, "Cargo.toml")), manifest: record.manifest });
  }
  for (const { where, manifest } of cargoSources) {
    if (!manifest) continue;
    for (const dep of cargoDeclarations(manifest)) {
      if (internalCargo.has(dep.name) || dep.name.startsWith("rightkit-")) continue;
      add("cargo", dep.name, dep.req, where);
    }
  }
  for (const record of records.filter((r) => r.ecosystem === "npm" && r.isPublic)) {
    for (const dep of npmDeclarations(record.manifest)) {
      if (dep.name.startsWith("@rightkit/")) continue;
      add("npm", dep.name, dep.req, relative(repoRoot, join(record.dir, "package.json")));
    }
  }

  const grouped = new Map();
  for (const decl of declarations.values()) {
    const key = `${decl.ecosystem}:${decl.name}`;
    if (!grouped.has(key)) grouped.set(key, []);
    grouped.get(key).push(decl);
  }
  let unique = 0;
  await mapLimit([...grouped.values()], CONCURRENCY, async (decls) => {
    unique++;
    const { ecosystem, name } = decls[0];
    let info;
    try {
      info = await live(ecosystem, name);
    } catch (error) {
      findings.push(issue("thirdParty", `${ecosystem} ${name}`, "unverified", `registry lookup failed: ${error.message}`));
      return;
    }
    if (!info.latest) {
      findings.push(issue("thirdParty", `${ecosystem} ${name}`, "unverified", "no stable version found on the registry (prerelease or empty)", INFO));
      return;
    }
    for (const decl of decls) {
      const ok = satisfies(decl.req, info.latest, ecosystem);
      const wheres = [...decl.wheres].sort().join(", ");
      if (ok === null) findings.push(issue("thirdParty", `${ecosystem} ${name}`, "unsupported-requirement", `requirement "${decl.req}" in ${wheres} is not modelled`));
      else if (ok === false) {
        findings.push(issue("thirdParty", `${ecosystem} ${name}`, "outdated-requirement",
          `"${decl.req}" in ${wheres} excludes latest ${info.latest} (${behindKind(decl.req, info.latest, ecosystem)} behind)`));
      }
    }
  });

  const lockDuplicates = [];
  // One lock per crate workspace: look for duplicate resolutions in each crate's own crates/<x>/Cargo.lock.
  for (const record of records.filter((r) => r.ecosystem === "cargo").sort((a, b) => a.dir.localeCompare(b.dir))) {
    const lockFile = relative(repoRoot, join(record.dir, "Cargo.lock"));
    const lockText = await readText(join(repoRoot, lockFile));
    if (lockText === null) continue;
    for (const dup of duplicateCargoVersions(lockText)) {
      lockDuplicates.push({ ...dup, lock: lockFile });
      findings.push(issue("thirdParty", `crate ${dup.name} (${lockFile})`, "duplicate-lock-versions", `${lockFile} resolves ${dup.versions.join(", ")}`));
    }
  }
  return { declarations: declarations.size, uniquePackages: unique, lockDuplicates };
}

// ---------- lock parsing ----------

export function duplicateCargoVersions(lockText, filter = () => true) {
  const byName = new Map();
  for (const pkg of parseToml(lockText).package ?? []) {
    if (!filter(pkg.name)) continue;
    if (!byName.has(pkg.name)) byName.set(pkg.name, new Set());
    byName.get(pkg.name).add(pkg.version);
  }
  return [...byName].filter(([, versions]) => versions.size > 1)
    .map(([name, versions]) => ({ name, versions: [...versions].sort() }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

// Versions per package in a pnpm lock's `packages:` block (v6 and v9 key forms).
export function pnpmPackageVersions(lockText, filter = () => true) {
  const byName = new Map();
  let inPackages = false;
  for (const line of lockText.split("\n")) {
    if (/^\S/.test(line)) {
      inPackages = line.trimEnd() === "packages:";
      continue;
    }
    if (!inPackages) continue;
    const match = /^ {2}'?\/?(@?[^@'\s]+(?:\/[^@'\s]+)?)@([^'\s:(]+)(?:\([^']*\))?'?:\s*$/.exec(line);
    if (!match || !filter(match[1])) continue;
    if (!byName.has(match[1])) byName.set(match[1], new Set());
    byName.get(match[1]).add(match[2]);
  }
  return [...byName].filter(([, versions]) => versions.size > 1)
    .map(([name, versions]) => ({ name, versions: [...versions].sort() }));
}

// ---------- section 4: app adoption ----------

// Reuses the adoption generator's own output (pin statuses and split pins) instead of a second
// manifest scanner. Lock files are the one extra read; the generator does not read them.
async function checkApps({ appsRoot, apps, generatedAdoption, findings }) {
  const rows = [];
  const lines = generatedAdoption.split("\n");
  const section = (title) => {
    const start = lines.indexOf(title);
    if (start < 0) return [];
    const body = [];
    for (let i = start + 1; i < lines.length && !lines[i].startsWith("## "); i++) body.push(lines[i]);
    return body.filter((line) => line.startsWith("| "));
  };
  const cells = (line) => line.slice(1, line.lastIndexOf("|")).split("|").map((c) => c.trim().replace(/^`|`$/g, ""));
  const pinRows = section("## Pins").slice(2).map(cells);
  for (const [name, app, manifest, , requirement, latest, status] of pinRows) {
    if (status === "stale") {
      findings.push(issue("apps", `${name} (${app})`, "outdated-pin", `${manifest} pins "${requirement}"; latest published ${latest}`));
    } else if (status === "unpublished") {
      findings.push(issue("apps", `${name} (${app})`, "unpublished-pin", `${manifest} pins "${requirement}" but no published version is recorded`));
    } else if (status === "unparsed") {
      findings.push(issue("apps", `${name} (${app})`, "unsupported-requirement", `${manifest} requirement "${requirement}" is not understood`));
    }
  }
  for (const [name, ecosystem, app, requirements] of section("## Duplicate copies and split pins").slice(2).map(cells)) {
    findings.push(issue("apps", `${name} (${app})`, "split-app-pins", `${ecosystem} pins differ within one app: ${requirements}`));
  }

  for (const app of apps) {
    const appDir = join(appsRoot, app);
    if (!(await exists(appDir))) {
      findings.push(issue("apps", app, "app-missing", "app repository not found under the workspace root"));
      rows.push({ app, present: false, duplicates: 0 });
      continue;
    }
    let duplicates = 0;
    for (const lockPath of await walkNamed(appDir, ["Cargo.lock"], 5)) {
      for (const dup of duplicateCargoVersions(await readFile(lockPath, "utf8"), (n) => n.startsWith("rightkit-"))) {
        duplicates++;
        findings.push(issue("apps", `${dup.name} (${app})`, "duplicate-app-copies", `${relative(appDir, lockPath)} resolves ${dup.versions.join(", ")}`));
      }
    }
    for (const lockPath of await walkNamed(appDir, ["pnpm-lock.yaml"], 4)) {
      const text = await readFile(lockPath, "utf8");
      for (const dup of pnpmPackageVersions(text, (n) => n.startsWith("@rightkit/"))) {
        duplicates++;
        findings.push(issue("apps", `${dup.name} (${app})`, "duplicate-app-copies", `${relative(appDir, lockPath)} resolves ${dup.versions.join(", ")}`));
      }
    }
    rows.push({ app, present: true, duplicates });
  }
  return rows;
}

// ---------- section 5: generated docs ----------

function checkGenerated({ repoRoot, appsRoot, apps, findings }) {
  const local = generateLocal({ root: repoRoot });
  for (const rel of driftOf(repoRoot, local)) {
    findings.push(issue("generated", rel, "generated-doc-drift", "committed file differs from the generator output; run generate-index.mjs"));
  }
  const adoption = generateAdoption({ root: repoRoot, workspace: appsRoot, apps });
  for (const rel of driftOf(repoRoot, adoption)) {
    findings.push(issue("generated", rel, "generated-doc-drift", "committed adoption report differs from the app repos; run generate-index.mjs"));
  }
  // The port matrix reads the app repos like the adoption report. Its stamp, app directories and HEADs, and per-app check counts
  // are carried over from the committed docs/port-matrix.json, so only a change in the data itself reads as drift.
  try {
    const matrix = generatePortMatrix({ root: repoRoot, workspace: appsRoot, carry: true });
    for (const rel of driftOf(repoRoot, matrix.outputs)) {
      findings.push(issue("generated", rel, "generated-doc-drift", "committed port matrix differs from the app repos; run generate-index.mjs"));
    }
  } catch (error) {
    findings.push(issue("generated", PORT_MATRIX_OUTPUTS[2], "generated-doc-drift", `the port matrix could not be regenerated: ${error.message}`));
  }
  return { local, adoption: Object.values(adoption)[0] };
}

// ---------- section 6: hygiene ----------

function checkHygiene(records, findings) {
  for (const record of records) {
    if (!record.hygiene.hasOwnership) findings.push(issue("hygiene", subjectOf(record), "missing-ownership-file", "no rightkit-ownership.json"));
    if (!record.hygiene.hasReadme) findings.push(issue("hygiene", subjectOf(record), "missing-readme", "no README.md"));
    if (!record.hygiene.inCatalog) findings.push(issue("hygiene", subjectOf(record), "missing-from-catalog", "not listed in packages/release/rightkit-ownership.json"));
  }
}

// ---------- orchestration ----------

export async function runInventoryCheck(options = {}) {
  const repoRoot = resolve(options.repoRoot ?? DEFAULT_REPO_ROOT);
  const appsRoot = resolve(options.appsRoot ?? DEFAULT_WORKSPACE);
  const apps = options.apps ?? APP_REPOS;
  const get = options.get ?? createFetcher(options.http ?? {});
  const liveIndex = createLiveIndex(get);
  const findings = [];
  const source = await loadSource(repoRoot);
  const { records } = source;

  // Registry lookups for every in-scope package; Swift uses GitHub tags.
  await mapLimit(records, CONCURRENCY, async (record) => {
    try {
      record.live = record.ecosystem === "swift" ? await fetchSwiftTags(get) : await liveIndex(record.ecosystem, record.name);
    } catch (error) {
      record.liveError = error.message;
    }
  });

  const registryRows = [];
  for (const record of records) {
    const { row, findings: found } = assessParity(record);
    registryRows.push(row);
    findings.push(...found);
  }

  const account = await checkAccount({ records, get, findings });
  const thirdParty = await checkThirdParty({ repoRoot, records, live: liveIndex, findings });
  const generated = checkGenerated({ repoRoot, appsRoot, apps, findings });
  const appRows = await checkApps({ appsRoot, apps, generatedAdoption: generated.adoption, findings });
  checkHygiene(records, findings);

  const counts = { action: 0, info: 0 };
  for (const f of findings) counts[f.severity] += 1;
  return {
    schema: 1,
    generatedAt: new Date().toISOString(),
    repoRoot,
    appsRoot,
    counts,
    findings,
    sections: {
      registry: { rows: registryRows },
      account: { crates: account.crates, npm: account.npm },
      thirdParty: { declarations: thirdParty.declarations, uniquePackages: thirdParty.uniquePackages, lockDuplicates: thirdParty.lockDuplicates },
      apps: { rows: appRows },
      generated: { checked: ["INDEX.md", "docs/CAPABILITY-MAP.md", "docs/ADOPTION.md", ...PORT_MATRIX_OUTPUTS] },
      hygiene: { checked: records.length },
    },
  };
}

export function decideExit(report, warnOnly = false) {
  return warnOnly ? 0 : report.counts.action > 0 ? 1 : 0;
}

// ---------- rendering ----------

const esc = (value) => String(value ?? "").replace(/\|/g, "\\|").replace(/\n/g, " ");

export function renderSummary(report, markdownPath) {
  const out = [];
  out.push(`RightKit inventory check (${report.generatedAt})`);
  for (const [key, title] of Object.entries(SECTIONS)) {
    const list = report.findings.filter((f) => f.section === key);
    const action = list.filter((f) => f.severity === ACTION).length;
    out.push(`  ${title.padEnd(30)} ${action} action, ${list.length - action} info`);
  }
  const actions = report.findings.filter((f) => f.severity === ACTION);
  if (actions.length) {
    out.push("", `Action items (${actions.length}):`);
    for (const f of actions) out.push(`  [${f.section}] ${f.subject}: ${f.kind}, ${f.detail}`);
  }
  out.push("", `Result: ${report.counts.action} action, ${report.counts.info} info. Report: ${markdownPath}`);
  return `${out.join("\n")}\n`;
}

export function renderMarkdown(report) {
  const lines = [];
  lines.push("# RightKit inventory status", "");
  lines.push(`Generated: ${report.generatedAt}. Command: \`right-release inventory check\`.`, "");
  lines.push(`Result: ${report.counts.action} action item(s), ${report.counts.info} info item(s).`, "");
  lines.push("Sources: crates.io sparse index and search API, npm registry, GitHub tags for RightKitSwift, and the local manifests. No credentials are read. Registry responses are cached for one hour under the OS temp directory.", "");
  lines.push("## Summary", "", "| Section | Action | Info |", "| --- | ---: | ---: |");
  for (const [key, title] of Object.entries(SECTIONS)) {
    const list = report.findings.filter((f) => f.section === key);
    const action = list.filter((f) => f.severity === ACTION).length;
    lines.push(`| ${title} | ${action} | ${list.length - action} |`);
  }
  lines.push("", `## ${SECTIONS.registry}`, "", "| Ecosystem | Package | Public | Source | Registry latest | Feed recorded | Staged | Status |", "| --- | --- | --- | --- | --- | --- | --- | --- |");
  for (const r of report.sections.registry.rows) {
    lines.push(`| ${r.ecosystem} | \`${esc(r.name)}\` | ${r.public ? "yes" : "no"} | ${esc(r.source)} | ${esc(r.latest)} | ${esc(r.recorded)} | ${esc(r.staged)} | ${esc(r.status)} |`);
  }
  lines.push("", `## ${SECTIONS.account}`, "", `Crates owned by the \`${OWNER_PROBE_CRATE}\` owner: ${report.sections.account.crates.owned.length}. npm @rightkit packages listed: ${report.sections.account.npm.owned.length}.`, "");
  const section = (key) => {
    const list = report.findings.filter((f) => f.section === key);
    lines.push(`## ${SECTIONS[key]}`, "");
    if (!list.length) lines.push("No findings.", "");
    else {
      lines.push("| Severity | Subject | Kind | Detail |", "| --- | --- | --- | --- |");
      for (const f of list) lines.push(`| ${f.severity} | ${esc(f.subject)} | ${f.kind} | ${esc(f.detail)} |`);
      lines.push("");
    }
  };
  section("account");
  lines.push(`## ${SECTIONS.thirdParty}`, "", `Declarations checked: ${report.sections.thirdParty.declarations}. Unique external packages: ${report.sections.thirdParty.uniquePackages}.`, "");
  section("thirdParty");
  lines.push("Multiple versions in a crate's own Cargo.lock (one per crate workspace):", "");
  for (const dup of report.sections.thirdParty.lockDuplicates) lines.push(`- \`${dup.name}\` in \`${dup.lock}\`: ${dup.versions.join(", ")}`);
  if (!report.sections.thirdParty.lockDuplicates.length) lines.push("- none");
  lines.push("");
  lines.push(`## ${SECTIONS.apps}`, "", "| App | Present | Duplicate copies in lock files |", "| --- | --- | ---: |");
  for (const row of report.sections.apps.rows) lines.push(`| ${row.app} | ${row.present ? "yes" : "**missing**"} | ${row.duplicates} |`);
  lines.push("");
  section("apps");
  section("generated");
  section("hygiene");
  return `${lines.join("\n")}\n`;
}

// ---------- CLI ----------

const USAGE = `right-release inventory check [--json] [--warn-only] [--md <file>] [--no-cache] [--apps-root <dir>]

Checks RightKit registry parity, account parity, third-party requirements,
app adoption, generated docs, and package hygiene. Exit 1 when any action item
exists (0 with --warn-only). Writes docs/INVENTORY-STATUS.md unless --md is given.
`;

export function parseArgs(argv) {
  const options = { json: false, warnOnly: false, noCache: false, md: null, appsRoot: null, help: false };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--json") options.json = true;
    else if (arg === "--warn-only") options.warnOnly = true;
    else if (arg === "--no-cache") options.noCache = true;
    else if (arg === "--md") options.md = resolve(argv[++i] ?? "");
    else if (arg === "--apps-root") options.appsRoot = resolve(argv[++i] ?? "");
    else if (arg === "-h" || arg === "--help") options.help = true;
    else throw new Error(`unknown argument: ${arg}`);
  }
  return options;
}

export async function main(argv = process.argv.slice(2), io = { stdout: process.stdout, stderr: process.stderr }) {
  let options;
  try {
    options = parseArgs(argv);
  } catch (error) {
    io.stderr.write(`inventory check: ${error.message}\n${USAGE}`);
    return 2;
  }
  if (options.help) {
    io.stdout.write(USAGE);
    return 0;
  }
  const report = await runInventoryCheck({
    appsRoot: options.appsRoot ?? undefined,
    http: options.noCache ? { cacheDir: null } : {},
  });
  const mdPath = options.md ?? join(report.repoRoot, "docs", "INVENTORY-STATUS.md");
  await mkdir(dirname(mdPath), { recursive: true });
  await writeFile(mdPath, renderMarkdown(report));
  io.stdout.write(options.json ? `${JSON.stringify(report, null, 2)}\n` : renderSummary(report, mdPath));
  return decideExit(report, options.warnOnly);
}

const realOrSelf = (file) => {
  try {
    return realpathSync(file);
  } catch {
    return resolve(file);
  }
};
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  main().then((code) => {
    process.exitCode = code;
  }, (error) => {
    process.stderr.write(`inventory check failed: ${error.stack ?? error.message}\n`);
    process.exitCode = 2;
  });
}
