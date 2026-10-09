// Default-deny direct-dependency allowlist: pure, dependency-free logic shared by the release scanner
// (ownership.mjs), the trust updater and the agent write hook. This file is byte-copied into
// packages/build-control-policy/src by rightkit-sync, so it must import nothing.
//
// Data file: ownership-dependency-allowlist.json
//   { schema: 1, global: ["npm:typescript", ...], repositories: [{ repositoryIdentity, dependencies: ["cargo:serde", "cargo:x#git"] }] }
// Entry grammar: "<ecosystem>:<name>[#<kind>]"; kind is registry (default), git or path.
// Local path/workspace dependencies are first-party code inside the guarded repository and are never
// gated here (source scanning and local-path coverage already cover them).

export const ALLOWLIST_FILE = "ownership-dependency-allowlist.json";
export const ALLOWLIST_FINDING_ID = "dependency-not-allowlisted";
const ECOSYSTEMS = ["cargo", "npm", "swiftpm"];
const KINDS = ["registry", "git", "path"];
const MAX_ENTRIES = 20000;

export const entryKey = (ecosystem, name, kind = "registry") => `${ecosystem}:${name}${kind === "registry" ? "" : `#${kind}`}`;

/** Parse "<ecosystem>:<name>[#<kind>]"; returns null when malformed. */
export function parseEntry(value) {
  if (typeof value !== "string") return null;
  const colon = value.indexOf(":");
  if (colon < 1) return null;
  const ecosystem = value.slice(0, colon);
  let name = value.slice(colon + 1), kind = "registry";
  const hash = name.lastIndexOf("#");
  if (hash >= 0) { kind = name.slice(hash + 1); name = name.slice(0, hash); }
  if (!ECOSYSTEMS.includes(ecosystem) || !KINDS.includes(kind) || !name || /\s/.test(name)) return null;
  return { ecosystem, name, kind };
}

/** Source string from the scanner ("registry", "path:..", "git+..") to its kind. */
export function sourceKind(source) {
  if (typeof source !== "string" || source === "registry" || source === "") return "registry";
  if (source.startsWith("path:")) return "path";
  if (source.startsWith("git+")) return "git";
  return "registry";
}

const listProblems = (list, label, problems) => {
  if (!Array.isArray(list)) { problems.push(`${label} must be an array`); return; }
  if (list.length > MAX_ENTRIES) problems.push(`${label} exceeds ${MAX_ENTRIES} entries`);
  const seen = new Set();
  for (const item of list) {
    if (!parseEntry(item)) { problems.push(`${label} has a malformed entry: ${JSON.stringify(item)}`); continue; }
    if (seen.has(item)) problems.push(`${label} has a duplicate entry: ${item}`);
    seen.add(item);
  }
};

export function validateAllowlist(value) {
  const problems = [];
  if (!value || typeof value !== "object" || Array.isArray(value)) return { ok: false, problems: ["dependency allowlist must be an object"] };
  for (const key of Object.keys(value)) if (!["schema", "global", "repositories"].includes(key)) problems.push(`dependency allowlist has an unknown field: ${key}`);
  if (value.schema !== 1) problems.push("dependency allowlist schema must be 1");
  listProblems(value.global, "global", problems);
  if (!Array.isArray(value.repositories)) problems.push("repositories must be an array");
  else {
    const seen = new Set();
    for (const entry of value.repositories) {
      const identity = entry?.repositoryIdentity;
      if (!entry || typeof entry !== "object" || Array.isArray(entry) || typeof identity !== "string" || !/^https:\/\/[^/\s]+\/[^\s]+$/.test(identity) || /\/$|\.git$/i.test(identity)) { problems.push(`repository baseline has an invalid identity: ${JSON.stringify(identity)}`); continue; }
      for (const key of Object.keys(entry)) if (!["repositoryIdentity", "dependencies", "note"].includes(key)) problems.push(`repository baseline has an unknown field: ${key}`);
      if (seen.has(identity)) problems.push(`repository baseline is duplicated: ${identity}`);
      seen.add(identity);
      listProblems(entry.dependencies, `repository ${identity}`, problems);
    }
  }
  return { ok: problems.length === 0, problems };
}

const indexCache = new WeakMap();
function index(allowlist) {
  let built = indexCache.get(allowlist);
  if (built) return built;
  const global = new Set(allowlist.global ?? []);
  const repositories = new Map((allowlist.repositories ?? []).map((entry) => [entry.repositoryIdentity, new Set(entry.dependencies ?? [])]));
  // Name-only view for inherited (workspace = true) cargo dependencies whose source kind the hook cannot see.
  const names = (set) => new Set([...set].map((item) => { const p = parseEntry(item); return p ? `${p.ecosystem}:${p.name}` : item; }));
  built = { global, repositories, globalNames: names(global), repositoryNames: new Map([...repositories].map(([id, set]) => [id, names(set)])) };
  indexCache.set(allowlist, built);
  return built;
}

/** RightKit-owned packages are always allowed: catalog owners, internal owners and the rightkit naming scheme. */
export function isRightKitPackage(name, owners = []) {
  return owners.includes(name) || /^(?:@rightkit\/|rightkit[-_])/i.test(name) || name === "RightKitSwift" || /^rightkit(?:swift)?$/i.test(name);
}

/**
 * @param {object} allowlist validated allowlist
 * @param {string|null} repositoryIdentity canonical identity of the guarded repository
 * @param {{ecosystem:string,name:string,kind?:string}} dependency kind "any" matches by name regardless of source kind
 * @returns {{allowed:boolean, reason:string}}
 */
export function evaluateDependency(allowlist, repositoryIdentity, { ecosystem, name, kind = "registry" }, { owners = [] } = {}) {
  if (kind === "path") return { allowed: true, reason: "first-party local path dependency" };
  if (isRightKitPackage(name, owners)) return { allowed: true, reason: "RightKit package" };
  const { global, repositories, globalNames, repositoryNames } = index(allowlist);
  if (kind === "any") {
    const key = `${ecosystem}:${name}`;
    if (globalNames.has(key)) return { allowed: true, reason: "global allowlist" };
    if (repositoryNames.get(repositoryIdentity)?.has(key)) return { allowed: true, reason: "repository baseline" };
    return { allowed: false, reason: "not in repository baseline or global list" };
  }
  const key = entryKey(ecosystem, name, kind);
  if (global.has(key)) return { allowed: true, reason: "global allowlist" };
  if (repositories.get(repositoryIdentity)?.has(key)) return { allowed: true, reason: "repository baseline" };
  return { allowed: false, reason: "not in repository baseline or global list" };
}

/** Semantic diff between two allowlists (either may be null). */
export function diffAllowlists(before, after) {
  const setDiff = (a, b) => ({ added: [...b].filter((x) => !a.has(x)).sort(), removed: [...a].filter((x) => !b.has(x)).sort() });
  const global = setDiff(new Set(before?.global ?? []), new Set(after?.global ?? []));
  const was = new Map((before?.repositories ?? []).map((entry) => [entry.repositoryIdentity, new Set(entry.dependencies ?? [])]));
  const now = new Map((after?.repositories ?? []).map((entry) => [entry.repositoryIdentity, new Set(entry.dependencies ?? [])]));
  const repositories = [];
  for (const identity of [...new Set([...was.keys(), ...now.keys()])].sort()) {
    const change = setDiff(was.get(identity) ?? new Set(), now.get(identity) ?? new Set());
    if (change.added.length || change.removed.length || !was.has(identity) || !now.has(identity)) repositories.push({ repositoryIdentity: identity, ...change, baseline: was.has(identity) ? (now.has(identity) ? "changed" : "removed") : "added" });
  }
  return { global, repositories, changed: global.added.length > 0 || global.removed.length > 0 || repositories.length > 0 };
}
