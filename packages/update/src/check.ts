// `right-update check`: compare every pinned dependency with its upstream latest.
// No agent in the loop. Each source is isolated: a failing source or lookup
// becomes an entry with `error` set, and never aborts the other sources.

import fs from "node:fs";
import path from "node:path";
import { execFile as execFileCb } from "node:child_process";
import { parse as parseToml } from "smol-toml";
import { driftCommand } from "@rightkit/git/drift.mjs";
import { RIGHTKIT_VERSIONS_DEFAULT } from "./manifest.js";
import { compareVersions, isRegistryVersion, parseLatest, resolvePolicy } from "./compare.js";
import { fetchJson, fetchText, mapLimit, type FetchLike } from "./http.js";
import { sourceManifests } from "./glob.js";
import {
  SCHEMA_VERSION,
  type CheckEntry,
  type CheckReport,
  type Policy,
  type PlatformReport,
  type Source,
  type UpdateManifest,
} from "./types.js";

export interface ExecResult {
  stdout: string;
  error?: string;
}
export type ExecFn = (file: string, args: string[], options: { cwd: string }) => Promise<ExecResult>;

export interface DriftResult {
  ok: boolean;
  driftDetected: boolean | null;
  visibility: { allowed: boolean };
  errors: string[];
}
export type DriftFn = (repoPath: string) => Promise<DriftResult>;

/** Registry answers shared across every manifest in one check run. */
export interface LookupCache {
  crates: Map<string, Promise<string | null>>;
  npm: Map<string, Promise<string | null>>;
}

export interface CheckDeps {
  fetch?: FetchLike;
  exec?: ExecFn;
  drift?: DriftFn;
}

export const defaultExec: ExecFn = (file, args, options) =>
  new Promise((resolve) => {
    execFileCb(file, args, { cwd: options.cwd, timeout: 120_000, maxBuffer: 8 * 1024 * 1024 }, (error, stdout) => {
      // `pnpm outdated` exits 1 when anything is outdated; stdout is still the answer.
      resolve({ stdout: String(stdout ?? ""), error: error ? error.message : undefined });
    });
  });

const defaultDrift: DriftFn = async (repoPath) => {
  const result = await driftCommand(repoPath);
  return {
    ok: result.ok,
    driftDetected: result.driftDetected,
    visibility: { allowed: result.visibility.allowed },
    errors: result.errors,
  };
};

const CONCURRENCY = 4;
const MANIFEST_CONCURRENCY = 4;
/** crates.io returns every version of a large crate (windows exceeds 1 MiB), so crate lookups get a larger cap. */
const CRATES_MAX_BYTES = 16 * 1024 * 1024;

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function readJsonFile(file: string): unknown {
  return JSON.parse(fs.readFileSync(file, "utf8")) as unknown;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** Entry builder shared by the kinds. */
function entry(
  base: { name: string; kind: Source["kind"]; source: string },
  config: UpdateManifest["policy"],
  explicit: Policy | undefined,
  fields: Partial<Pick<CheckEntry, "pinned" | "latest" | "status" | "reason" | "error" | "platforms">>,
): CheckEntry {
  return {
    name: base.name,
    kind: base.kind,
    source: base.source,
    pinned: fields.pinned ?? null,
    latest: fields.latest ?? null,
    status: fields.status ?? "unknown",
    policy: resolvePolicy(base.name, explicit, config),
    ...(fields.reason ? { reason: fields.reason } : {}),
    ...(fields.error ? { error: fields.error } : {}),
    ...(fields.platforms ? { platforms: fields.platforms } : {}),
  };
}

/** Compare a pin with a latest value, turning comparison failures into entry fields. */
function judge(pinned: string, latest: string | null): Pick<CheckEntry, "status" | "reason" | "latest"> {
  if (latest === null) return { status: "unknown", reason: "no upstream version found", latest: null };
  const result = compareVersions(pinned, latest);
  return { status: result.status, latest, ...(result.reason ? { reason: result.reason } : {}) };
}

// ---------------------------------------------------------------- cargo

interface CargoDep {
  name: string;
  spec: string | null;
  reason?: string;
}

const CARGO_SECTIONS = ["dependencies", "dev-dependencies", "build-dependencies"];

export function cargoDependencies(doc: Record<string, unknown>): CargoDep[] {
  const tables: Record<string, unknown>[] = [];
  for (const section of CARGO_SECTIONS) {
    if (isObject(doc[section])) tables.push(doc[section]);
  }
  if (isObject(doc.workspace) && isObject(doc.workspace.dependencies)) tables.push(doc.workspace.dependencies);
  if (isObject(doc.target)) {
    for (const target of Object.values(doc.target)) {
      if (!isObject(target)) continue;
      for (const section of CARGO_SECTIONS) {
        if (isObject(target[section])) tables.push(target[section]);
      }
    }
  }

  const out: CargoDep[] = [];
  for (const table of tables) {
    for (const [key, value] of Object.entries(table)) {
      if (typeof value === "string") {
        out.push({ name: key, spec: value });
      } else if (isObject(value)) {
        const name = typeof value.package === "string" ? value.package : key;
        if (value.path !== undefined || value.git !== undefined) {
          // A version next to a path is the publish-time requirement, owned by the release tooling.
          out.push({ name, spec: null, reason: "path or git dependency" });
        } else if (typeof value.version === "string") {
          out.push({ name, spec: value.version });
        } else {
          out.push({ name, spec: null, reason: "no version requirement" });
        }
      }
    }
  }
  return out;
}

async function checkCargo(repo: string, source: Extract<Source, { kind: "cargo" }> & { manifest: string }, config: UpdateManifest["policy"], deps: { fetch?: FetchLike; cache: LookupCache }): Promise<CheckEntry[]> {
  const file = path.join(repo, source.manifest);
  const doc = parseToml(fs.readFileSync(file, "utf8")) as Record<string, unknown>;
  const unique = new Map<string, CargoDep>();
  for (const dep of cargoDependencies(doc)) {
    const key = `${dep.name}\u0000${dep.spec ?? ""}`;
    if (!unique.has(key)) unique.set(key, dep);
  }
  const lookup = (name: string) => {
    let pending = deps.cache.crates.get(name);
    if (!pending) {
      pending = (async () => {
        const body = (await fetchJson(`https://crates.io/api/v1/crates/${encodeURIComponent(name)}`, { fetch: deps.fetch, maxBytes: CRATES_MAX_BYTES })) as {
          crate?: { max_stable_version?: string; max_version?: string };
        };
        return body.crate?.max_stable_version ?? body.crate?.max_version ?? null;
      })();
      deps.cache.crates.set(name, pending);
    }
    return pending;
  };

  const rows = [...unique.values()];
  return mapLimit(rows, CONCURRENCY, async (dep) => {
    const base = { name: dep.name, kind: "cargo" as const, source: source.manifest };
    if (dep.spec === null || !isRegistryVersion(dep.spec)) {
      return entry(base, config, source.policy, {
        pinned: dep.spec,
        reason: dep.reason ?? "non-registry version requirement",
      });
    }
    try {
      const latest = await lookup(dep.name);
      return entry(base, config, source.policy, { pinned: dep.spec, ...judge(dep.spec, latest) });
    } catch (error) {
      return entry(base, config, source.policy, { pinned: dep.spec, error: `crates.io lookup failed: ${errorText(error)}` });
    }
  });
}

// ---------------------------------------------------------------- pnpm

function npmLatest(name: string, cache: LookupCache, fetchFn?: FetchLike): Promise<string | null> {
  let pending = cache.npm.get(name);
  if (!pending) {
    pending = (async () => {
      const body = (await fetchJson(`https://registry.npmjs.org/${encodeURIComponent(name)}/latest`, { fetch: fetchFn })) as { version?: string };
      return body.version ?? null;
    })();
    cache.npm.set(name, pending);
  }
  return pending;
}

export function pnpmDependencies(pkg: Record<string, unknown>): Array<{ name: string; spec: string }> {
  const out = new Map<string, string>();
  for (const section of ["dependencies", "devDependencies", "optionalDependencies"]) {
    const table = pkg[section];
    if (!isObject(table)) continue;
    for (const [name, spec] of Object.entries(table)) {
      if (typeof spec === "string" && !out.has(name)) out.set(name, spec);
    }
  }
  return [...out].map(([name, spec]) => ({ name, spec }));
}

async function checkPnpm(repo: string, source: Extract<Source, { kind: "pnpm" }> & { manifest: string }, config: UpdateManifest["policy"], deps: { fetch?: FetchLike; exec: ExecFn; cache: LookupCache }): Promise<CheckEntry[]> {
  const file = path.join(repo, source.manifest);
  const dir = path.dirname(file);
  const pkg = readJsonFile(file);
  if (!isObject(pkg)) throw new Error(`${source.manifest} is not a JSON object`);
  const rows = pnpmDependencies(pkg);

  // `pnpm outdated --json` reports latest for packages it can see; registry is the fallback.
  const outdatedLatest = new Map<string, string>();
  const outdated = await deps.exec("pnpm", ["outdated", "--json"], { cwd: dir });
  try {
    const parsed = JSON.parse(outdated.stdout || "{}") as unknown;
    if (isObject(parsed)) {
      for (const [name, info] of Object.entries(parsed)) {
        if (isObject(info) && typeof info.latest === "string") outdatedLatest.set(name, info.latest);
      }
    }
  } catch {
    // Not JSON (no lockfile, pnpm missing): fall back to the registry for every dependency.
  }

  return mapLimit(rows, CONCURRENCY, async ({ name, spec }) => {
    const base = { name, kind: "pnpm" as const, source: source.manifest };
    if (!isRegistryVersion(spec)) {
      return entry(base, config, source.policy, { pinned: spec, reason: "non-registry version requirement" });
    }
    try {
      const latest = outdatedLatest.get(name) ?? (await npmLatest(name, deps.cache, deps.fetch));
      return entry(base, config, source.policy, { pinned: spec, ...judge(spec, latest) });
    } catch (error) {
      return entry(base, config, source.policy, { pinned: spec, error: `npm registry lookup failed: ${errorText(error)}` });
    }
  });
}

// ---------------------------------------------------------------- swiftpm

export interface SwiftDep {
  name: string;
  url: string;
  requirement: string | null;
  reason?: string;
}

/** Collect the argument text of each `.package(` call in Package.swift. */
function swiftPackageCalls(text: string): string[] {
  const calls: string[] = [];
  let index = text.indexOf(".package(");
  while (index !== -1) {
    let depth = 0;
    let inString = false;
    let start = index + ".package".length;
    let end = start;
    for (let i = start; i < text.length; i++) {
      const ch = text[i];
      if (ch === '"') inString = !inString;
      if (inString) continue;
      if (ch === "(") depth++;
      if (ch === ")") {
        depth--;
        if (depth === 0) {
          end = i;
          break;
        }
      }
    }
    calls.push(text.slice(start + 1, end));
    index = text.indexOf(".package(", end);
  }
  return calls;
}

function repoNameFromUrl(url: string): string {
  const last = url.replace(/\/+$/, "").split("/").pop() ?? url;
  return last.replace(/\.git$/, "");
}

export function parseSwiftPackage(text: string): SwiftDep[] {
  const out: SwiftDep[] = [];
  for (const call of swiftPackageCalls(text)) {
    const url = /url:\s*"([^"]+)"/.exec(call)?.[1];
    if (!url) continue;
    const name = repoNameFromUrl(url);
    const requirement = /\b(?:exact|from)\s*:\s*"([^"]+)"/.exec(call)?.[1] ?? null;
    if (requirement === null) {
      const reason = /\b(?:branch|revision)\s*:/.test(call) ? "branch or revision pin" : "unsupported version requirement";
      out.push({ name, url, requirement: null, reason });
    } else {
      out.push({ name, url, requirement });
    }
  }
  return out;
}

export function parseSwiftProjectYml(text: string): SwiftDep[] {
  const out: SwiftDep[] = [];
  const lines = text.split(/\r?\n/);
  lines.forEach((line, i) => {
    const match = /^(\s*)url:\s*"?([^"\s]+)"?\s*$/.exec(line);
    if (!match) return;
    const indent = match[1].length;
    const url = match[2];
    const name = repoNameFromUrl(url);
    let requirement: string | null = null;
    for (let j = i + 1; j < lines.length; j++) {
      const next = lines[j];
      if (next.trim() === "") continue;
      const nextIndent = /^\s*/.exec(next)?.[0].length ?? 0;
      if (nextIndent < indent) break;
      if (nextIndent !== indent) continue;
      const version = /^\s*(?:from|exactVersion|exact|version|minorVersion|majorVersion)\s*:\s*"?([^"\s]+)"?/.exec(next);
      if (version) {
        requirement = version[1];
        break;
      }
    }
    if (requirement === null) {
      out.push({ name, url, requirement: null, reason: "unsupported version requirement" });
    } else {
      out.push({ name, url, requirement });
    }
  });
  return out;
}

function githubRepoFromUrl(url: string): { owner: string; repo: string } | null {
  try {
    const parsed = new URL(url);
    if (parsed.hostname.toLowerCase() !== "github.com") return null;
    const parts = parsed.pathname.replace(/^\/+|\/+$/g, "").replace(/\.git$/, "").split("/");
    if (parts.length !== 2) return null;
    return { owner: parts[0], repo: parts[1] };
  } catch {
    return null;
  }
}

async function latestGithubTag(owner: string, repo: string, fetchFn?: FetchLike): Promise<string | null> {
  const tags = (await fetchJson(`https://api.github.com/repos/${owner}/${repo}/tags?per_page=100`, { fetch: fetchFn })) as unknown;
  if (!Array.isArray(tags)) throw new Error("GitHub tags response is not an array");
  let best: { name: string; parts: [number, number, number] } | null = null;
  for (const tag of tags) {
    const name = isObject(tag) && typeof tag.name === "string" ? tag.name : null;
    if (!name || !/^v?\d+(?:\.\d+){0,2}$/.test(name)) continue;
    const parts = parseLatest(name);
    if (!parts) continue;
    if (!best || compareParts(parts, best.parts) > 0) best = { name: name.replace(/^v/, ""), parts };
  }
  return best?.name ?? null;
}

function compareParts(a: [number, number, number], b: [number, number, number]): number {
  for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] - b[i];
  return 0;
}

async function checkSwiftpm(repo: string, source: Extract<Source, { kind: "swiftpm" }>, config: UpdateManifest["policy"], deps: { fetch?: FetchLike }): Promise<CheckEntry[]> {
  const file = path.join(repo, source.manifest);
  const text = fs.readFileSync(file, "utf8");
  const rows = path.basename(source.manifest) === "Package.swift" ? parseSwiftPackage(text) : parseSwiftProjectYml(text);
  const tagCache = new Map<string, Promise<string | null>>();
  return mapLimit(rows, CONCURRENCY, async (dep) => {
    const base = { name: dep.name, kind: "swiftpm" as const, source: source.manifest };
    if (dep.requirement === null) {
      return entry(base, config, source.policy, { reason: dep.reason ?? "unsupported version requirement" });
    }
    const gh = githubRepoFromUrl(dep.url);
    if (!gh) {
      return entry(base, config, source.policy, { pinned: dep.requirement, reason: "non-GitHub package URL" });
    }
    const key = `${gh.owner}/${gh.repo}`;
    try {
      let pending = tagCache.get(key);
      if (!pending) {
        pending = latestGithubTag(gh.owner, gh.repo, deps.fetch);
        tagCache.set(key, pending);
      }
      const latest = await pending;
      if (latest === null) {
        return entry(base, config, source.policy, { pinned: dep.requirement, reason: "no numeric release tags" });
      }
      return entry(base, config, source.policy, { pinned: dep.requirement, ...judge(dep.requirement, latest) });
    } catch (error) {
      return entry(base, config, source.policy, { pinned: dep.requirement, error: `GitHub tags lookup failed: ${errorText(error)}` });
    }
  });
}

// ---------------------------------------------------------------- rightkit

async function checkRightkit(repo: string, source: Extract<Source, { kind: "rightkit" }>, config: UpdateManifest["policy"], deps: CheckDeps): Promise<CheckEntry[]> {
  const versionsRel = source.versions ?? RIGHTKIT_VERSIONS_DEFAULT;
  const versions = readJsonFile(path.join(repo, versionsRel));
  const npm = isObject(versions) ? versions.npm : undefined;
  if (!isObject(npm)) throw new Error(`${versionsRel} has no npm registry section`);

  const pins = Object.entries(npm).filter((pair): pair is [string, string] => typeof pair[1] === "string");
  const results = await mapLimit(pins, CONCURRENCY, async ([name, pinned]) => {
    const base = { name, kind: "rightkit" as const, source: versionsRel };
    try {
      const body = (await fetchJson(`https://registry.npmjs.org/${encodeURIComponent(name)}/latest`, { fetch: deps.fetch })) as { version?: string };
      return entry(base, config, source.policy, { pinned, ...judge(pinned, body.version ?? null) });
    } catch (error) {
      return entry(base, config, source.policy, { pinned, error: `npm registry lookup failed: ${errorText(error)}` });
    }
  });

  // Workflow drift for repos that carry a right-git manifest, via the git package's drift check.
  if (fs.existsSync(path.join(repo, ".rightgit.json"))) {
    const drift = await (deps.drift ?? defaultDrift)(repo);
    const base = { name: "rightgit-workflows", kind: "rightkit" as const, source: ".rightgit.json" };
    if (!drift.ok) {
      results.push(entry(base, config, source.policy, { error: `drift check failed: ${drift.errors.join("; ") || "unknown error"}` }));
    } else {
      results.push(
        entry(base, config, source.policy, {
          pinned: "installed",
          latest: "rendered",
          status: drift.driftDetected ? "outdated" : "current",
          ...(drift.visibility.allowed ? {} : { reason: "visibility gate did not confirm a public repository" }),
        }),
      );
    }
  }
  return results;
}

// ---------------------------------------------------------------- tool

const TOOL_VERSION_RE = /^v?\d[0-9A-Za-z._+-]{0,63}$/;

async function checkTool(source: Extract<Source, { kind: "tool" }>, config: UpdateManifest["policy"], deps: CheckDeps): Promise<CheckEntry[]> {
  const base = { name: source.name, kind: "tool" as const, source: `tool:${source.name}` };
  const platforms: PlatformReport[] = Object.entries(source.assets).map(([platform, asset]) => ({
    platform: platform as PlatformReport["platform"],
    asset: asset!.asset,
    checksum: asset!.checksum,
    ...(asset!.extract ? { extract: asset!.extract } : {}),
  }));

  try {
    let latest: string;
    let missing: string[] = [];
    if (source.upstream.type === "github-release") {
      const release = (await fetchJson(`https://api.github.com/repos/${source.upstream.repo}/releases/latest`, { fetch: deps.fetch })) as {
        tag_name?: unknown;
        assets?: unknown;
      };
      if (typeof release.tag_name !== "string") throw new Error("release has no tag_name");
      latest = release.tag_name;
      const names = new Set(
        Array.isArray(release.assets) ? release.assets.map((a) => (isObject(a) && typeof a.name === "string" ? a.name : "")) : [],
      );
      missing = platforms.filter((p) => !names.has(p.asset)).map((p) => `${p.platform}:${p.asset}`);
    } else {
      const text = (await fetchText(source.upstream.latest, { fetch: deps.fetch, accept: "text/plain" })).trim();
      if (!TOOL_VERSION_RE.test(text)) throw new Error("latest URL did not return a version string");
      latest = text;
    }
    if (missing.length > 0) {
      return [entry(base, config, source.policy, { pinned: source.pin, latest, platforms, error: `assets missing from upstream release: ${missing.join(", ")}` })];
    }
    return [entry(base, config, source.policy, { pinned: source.pin, ...judge(source.pin, latest), platforms })];
  } catch (error) {
    return [entry(base, config, source.policy, { pinned: source.pin, platforms, error: `upstream lookup failed: ${errorText(error)}` })];
  }
}

// ---------------------------------------------------------------- driver

/** Check every source in the manifest. Never throws. */
export async function checkSources(repo: string, manifest: UpdateManifest, deps: CheckDeps = {}): Promise<CheckEntry[]> {
  const exec = deps.exec ?? defaultExec;
  const fetchFn = deps.fetch;
  const cache: LookupCache = { crates: new Map(), npm: new Map() };
  const out: CheckEntry[] = [];
  for (const source of manifest.sources) {
    const label =
      source.kind === "tool"
        ? `tool:${source.name}`
        : ("manifest" in source && source.manifest) || ("glob" in source && source.glob) || source.kind;
    try {
      switch (source.kind) {
        case "cargo":
        case "pnpm": {
          const manifests = sourceManifests(repo, source);
          if (manifests.length === 0) throw new Error(`glob ${source.glob} matched no files`);
          // `pnpm outdated` per package dominates the run time, so manifests are checked a few at a time.
          const perManifest = await mapLimit(manifests, MANIFEST_CONCURRENCY, async (rel) => {
            try {
              if (source.kind === "cargo") {
                return await checkCargo(repo, { ...source, manifest: rel }, manifest.policy, { fetch: fetchFn, cache });
              }
              return await checkPnpm(repo, { ...source, manifest: rel }, manifest.policy, { fetch: fetchFn, exec, cache });
            } catch (error) {
              return [entry({ name: rel, kind: source.kind, source: rel }, manifest.policy, source.policy, { error: errorText(error) })];
            }
          });
          for (const rows of perManifest) out.push(...rows);
          break;
        }
        case "swiftpm":
          out.push(...(await checkSwiftpm(repo, source, manifest.policy, { fetch: fetchFn })));
          break;
        case "rightkit":
          out.push(...(await checkRightkit(repo, source, manifest.policy, { ...deps, fetch: fetchFn })));
          break;
        case "tool":
          out.push(...(await checkTool(source, manifest.policy, { ...deps, fetch: fetchFn })));
          break;
      }
    } catch (error) {
      out.push(
        entry({ name: label, kind: source.kind, source: label }, manifest.policy, source.policy, {
          error: errorText(error),
        }),
      );
    }
  }
  return out;
}

/** Map entries to the exit code: 2 on any error, else 1 on any outdated, else 0. */
export function exitCodeFor(entries: CheckEntry[], errors: string[] = []): 0 | 1 | 2 {
  if (errors.length > 0 || entries.some((e) => e.error !== undefined)) return 2;
  if (entries.some((e) => e.status === "outdated")) return 1;
  return 0;
}

export function buildReport(repo: string, entries: CheckEntry[], errors: string[] = []): CheckReport {
  return { schemaVersion: SCHEMA_VERSION, repo, exitCode: exitCodeFor(entries, errors), errors, entries };
}
