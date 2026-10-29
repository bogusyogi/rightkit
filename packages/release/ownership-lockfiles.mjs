// Stage 1: resolved lock graphs, without feature or production-only pruning.
import { parse as parseToml } from "smol-toml";
import { existsSync, lstatSync, readFileSync, realpathSync } from "node:fs";
import path from "node:path";
import { sha256 } from "./ownership-approval.mjs";

export const OWNERSHIP_LOCKFILES = ["Cargo.lock", "pnpm-lock.yaml", "package-lock.json", "npm-shrinkwrap.json", "yarn.lock"];
const sections = ["dependencies", "devDependencies", "optionalDependencies"];
class LockError extends Error {
  constructor(reason, message) { super(message); this.reason = reason; }
}
const fail = (message) => { throw new LockError("lockfile-parse-failed", message); };
const object = (value) => value && typeof value === "object" && !Array.isArray(value);

// pnpm emits a small YAML subset. Reject unsupported syntax rather than guess.
// Split only outside quotes/flow collections (URLs, scoped names & peer suffixes are scalars).
function splitFlow(text, delimiter) {
  const parts = []; let quote = null, depth = 0, start = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quote) {
      if (c === "\\" && quote === '"') { i++; continue; }
      if (c === quote) { if (quote === "'" && text[i + 1] === "'") i++; else quote = null; }
    } else if (c === '"' || c === "'") quote = c;
    else if (c === "{" || c === "[") depth++;
    else if (c === "}" || c === "]") depth--;
    else if (!depth && c === delimiter && (delimiter !== ":" || !text[i + 1] || /\s/.test(text[i + 1]))) {
      parts.push(text.slice(start, i)); start = i + 1;
      if (delimiter === ":") { parts.push(text.slice(start)); return parts; }
    }
    if (depth < 0) fail("unbalanced YAML flow value");
  }
  if (quote || depth) fail("unterminated YAML value");
  parts.push(text.slice(start)); return parts;
}
function scalar(raw) {
  const value = raw.trim();
  if (/^[&*!|>]/.test(value)) throw new LockError("lockfile-unsupported-format", "YAML anchors, tags or block scalars");
  if (value.startsWith('"')) { try { return JSON.parse(value); } catch { fail("invalid YAML quoted scalar"); } }
  if (value.startsWith("'")) {
    if (!value.endsWith("'") || value.length < 2) fail("invalid YAML quoted scalar");
    return value.slice(1, -1).replaceAll("''", "'");
  }
  if (value.startsWith("{")) {
    if (!value.endsWith("}")) fail("invalid YAML flow map");
    const out = Object.create(null);
    if (value.slice(1, -1).trim()) for (const item of splitFlow(value.slice(1, -1), ",")) {
      const pair = splitFlow(item.trim(), ":");
      if (pair.length !== 2) fail("invalid YAML flow entry");
      const key = scalar(pair[0]); if (Object.hasOwn(out, key)) fail("duplicate YAML key");
      out[key] = scalar(pair[1]);
    }
    return out;
  }
  if (value.startsWith("[")) {
    if (!value.endsWith("]")) fail("invalid YAML flow sequence");
    return value.slice(1, -1).trim() ? splitFlow(value.slice(1, -1), ",").map(scalar) : [];
  }
  if (/[\[\]{}]/.test(value)) fail("invalid YAML scalar");
  return value.replace(/\s+#.*$/, "");
}
export function parsePnpmYaml(text) {
  const root = Object.create(null), stack = [{ indent: -1, value: root }];
  for (const line of text.split(/\r?\n/)) {
    if (!line.trim() || /^\s*#/.test(line)) continue;
    if (/^\s*\t/.test(line)) fail("YAML tab indentation");
    const indent = line.match(/^ */)[0].length;
    while (stack.length > 1 && indent <= stack.at(-1).indent) stack.pop();
    const parent = stack.at(-1);
    if (indent !== parent.indent + 2 && parent.indent !== -1 || parent.indent === -1 && indent !== 0) fail("invalid YAML indentation");
    // Block sequences occur in package metadata (os/cpu/bundledDependencies), never graph maps.
    const content = line.slice(indent);
    if (content.startsWith("- ")) {
      if (!parent.sequence) {
        if (Object.keys(parent.value).length) fail("mixed YAML collection");
        parent.sequence = []; parent.owner[parent.key] = parent.sequence;
      }
      parent.sequence.push(scalar(content.slice(2))); continue;
    }
    if (parent.sequence) fail("mixed YAML collection");
    const pair = splitFlow(content, ":");
    if (pair.length !== 2) fail("invalid YAML mapping");
    const key = scalar(pair[0]);
    if (typeof key !== "string" || !key || Object.hasOwn(parent.value, key)) fail("invalid or duplicate YAML key");
    const value = pair[1].trim();
    parent.value[key] = value ? scalar(value) : Object.create(null);
    if (!value) stack.push({ indent, value: parent.value[key], owner: parent.value, key });
  }
  return root;
}

// Workspace membership: a manifest is a root of a lock only when the lock's workspace owns it.
// Membership is decided only from a definition we fully understand; anything else (no
// definition, unsupported glob syntax, unparsable file) returns null = "every manifest is a
// member", which keeps the unresolved-root gap rather than hiding it.
const UNSUPPORTED_GLOB = /[\[\]{}\\]/;
const globPath = (value) => path.posix.normalize(String(value).trim().replace(/^(?:\.\/)+/, "").replace(/\/+$/, "") || ".").replace(/^\.$/, "");
function matchSegments(pattern, parts) {
  if (!pattern.length) return !parts.length;
  if (pattern[0] === "**") { for (let i = 0; i <= parts.length; i++) if (matchSegments(pattern.slice(1), parts.slice(i))) return true; return false; }
  if (!parts.length) return false;
  const source = [...pattern[0]].map((c) => c === "*" ? "[^/]*" : c === "?" ? "[^/]" : c.replace(/[.+^$|()]/g, "\\$&")).join("");
  return new RegExp(`^${source}$`).test(parts[0]) && matchSegments(pattern.slice(1), parts.slice(1));
}
function globMatches(glob, rel) {
  if (UNSUPPORTED_GLOB.test(glob)) throw new Error("unsupported glob syntax");
  const pattern = globPath(glob), segments = pattern ? pattern.split("/") : [];
  return matchSegments(segments, rel ? rel.split("/") : []);
}
/** A directory or any of its ancestors matches the glob. */
const underGlob = (glob, rel) => { const parts = rel ? rel.split("/") : []; for (let n = parts.length; n >= 1; n--) if (globMatches(glob, parts.slice(0, n).join("/"))) return true; return false; };
const stringList = (value) => { if (value === undefined) return []; if (!Array.isArray(value) || value.some((item) => typeof item !== "string")) throw new Error("not a string list"); return value; };
const dependencyTables = ["dependencies", "dev-dependencies", "build-dependencies"];
function cargoPathDependencies(doc) {
  const out = [], take = (table) => { if (object(table)) for (const dep of Object.values(table)) if (object(dep) && typeof dep.path === "string") out.push(dep.path); };
  for (const name of dependencyTables) take(doc[name]);
  take(doc.workspace?.dependencies);
  if (object(doc.target)) for (const target of Object.values(doc.target)) if (object(target)) for (const name of dependencyTables) take(target[name]);
  return out;
}
/** Predicate over a manifest directory relative to the lock directory ("" is the root), or null when undecidable. */
function cargoMembership(manifests, lockDir) {
  try {
    const rootRel = path.posix.join(lockDir, "Cargo.toml"), docs = new Map();
    for (const { rel, bytes } of manifests) { try { docs.set(path.posix.relative(lockDir, path.posix.dirname(rel)), parseToml(bytes.toString("utf8"))); } catch { /* a bad manifest fails its own parse later */ } }
    const root = docs.get("");
    if (!manifests.some((m) => m.rel === rootRel) || !root || !object(root.workspace)) return null;
    const patterns = [...stringList(root.workspace.members), ...stringList(root.workspace["default-members"])], excludes = stringList(root.workspace.exclude);
    const excluded = (rel) => excludes.some((glob) => underGlob(glob, rel));
    const members = new Set([""]), queue = [""];
    for (const rel of docs.keys()) if (rel && !excluded(rel) && patterns.some((glob) => globMatches(glob, rel))) { members.add(rel); queue.push(rel); }
    // Cargo adds path dependencies that live inside the workspace directory as members too.
    for (let at = 0; at < queue.length; at++) for (const dep of cargoPathDependencies(docs.get(queue[at]) ?? {})) {
      const target = path.posix.normalize(path.posix.join(queue[at] || ".", dep)).replace(/^\.$/, "");
      if (target.startsWith("..") || path.posix.isAbsolute(target) || members.has(target) || excluded(target)) continue;
      members.add(target); queue.push(target);
    }
    return (rel) => members.has(rel);
  } catch { return null; }
}
/** pnpm-workspace.yaml `packages:` globs; the root importer is always a member. */
function pnpmMembership(bytes) {
  if (!bytes) return null;
  try {
    const lines = bytes.toString("utf8").split(/\r?\n/), globs = [];
    const start = lines.findIndex((line) => /^packages\s*:/.test(line));
    if (start < 0) return null;
    const inline = lines[start].replace(/^packages\s*:/, "").replace(/\s+#.*$/, "").trim();
    if (inline) { const list = scalar(inline); if (!Array.isArray(list)) return null; globs.push(...list); }
    else for (const line of lines.slice(start + 1)) {
      if (!line.trim() || /^\s*#/.test(line)) continue;
      if (!/^\s+-\s/.test(line)) break;
      globs.push(scalar(line.replace(/^\s+-\s+/, "").replace(/\s+#.*$/, "")));
    }
    if (!globs.length || globs.some((glob) => typeof glob !== "string" || !glob.replace(/^!/, ""))) return null;
    const positive = globs.filter((glob) => !glob.startsWith("!")), negative = globs.filter((glob) => glob.startsWith("!")).map((glob) => glob.slice(1));
    for (const glob of globs) globMatches(glob.replace(/^!/, ""), ""); // throws on unsupported syntax
    return (rel) => !rel || positive.some((glob) => globMatches(glob, rel)) && !negative.some((glob) => underGlob(glob, rel));
  } catch { return null; }
}

function graph() {
  const nodes = new Map(), roots = [];
  return { nodes, roots, diagnostics: [], info: [], add(id, name, version, source, local = false) {
    if (typeof name !== "string" || !name || typeof version !== "string" || !version) fail("package name/version missing");
    const node = { id, name, version, source, local, edges: [] }; nodes.set(id, node); return node;
  } };
}
function connect(node, target, reference, optional = false) {
  if (!target) {
    if (optional) return; // Optional platform packages can be absent from a resolved install.
    (node.unresolved ??= []).push(`${node.name}: ${reference}`); return;
  }
  node.edges.push(target.id);
}
function sourceFor(value, fallback = "registry") {
  if (typeof value === "string" && /^(git\+|git:|https?:.*\.git)/.test(value)) return value.startsWith("git+") ? value : `git+${value}`;
  return value || fallback;
}
function cargoGraph(text, manifests, lockDir) {
  const lock = parseToml(text), g = graph();
  if (!Array.isArray(lock.package) || ![1, 2, 3, 4].includes(lock.version ?? 1)) throw new LockError("lockfile-unsupported-format", "Cargo lock version/package list");
  for (const [i, pkg] of lock.package.entries()) g.add(String(i), pkg.name, pkg.version, pkg.source ?? `path:${pkg.name}`, !pkg.source);
  for (const [i, pkg] of lock.package.entries()) {
    if (pkg.dependencies !== undefined && !Array.isArray(pkg.dependencies)) fail("Cargo dependencies must be a list");
    for (const ref of pkg.dependencies ?? []) {
      if (typeof ref !== "string") fail("invalid Cargo dependency reference");
      const match = /^(\S+)(?: (\S+))?(?: \((.+)\))?$/.exec(ref);
      if (!match) fail("invalid Cargo dependency reference");
      const candidates = [...g.nodes.values()].filter((n) => n.name === match[1] && (!match[2] || n.version === match[2]) && (!match[3] || n.source === match[3]));
      connect(g.nodes.get(String(i)), candidates.length === 1 ? candidates[0] : null, ref);
    }
  }
  const membership = cargoMembership(manifests, lockDir);
  for (const { rel, bytes } of manifests) {
    const pkg = parseToml(bytes.toString("utf8")).package;
    if (!pkg) continue; // Virtual workspace: member manifests supply roots.
    const candidates = [...g.nodes.values()].filter((n) => n.name === pkg.name && n.local && (typeof pkg.version !== "string" || n.version === pkg.version));
    if (candidates.length !== 1) {
      // Only a manifest the workspace does not own is informational; a member that fails to resolve is a gap.
      const outside = membership && !membership(path.posix.relative(lockDir, path.posix.dirname(rel)));
      (outside ? g.info : g.diagnostics).push({ reason: outside ? "manifest-outside-workspace" : "lockfile-root-unresolved", detail: rel }); continue;
    }
    g.roots.push(candidates[0].id);
  }
  return g;
}
function packageLockGraph(text, manifests, lockDir) {
  const lock = JSON.parse(text), g = graph();
  if (![2, 3].includes(lock.lockfileVersion) || !object(lock.packages)) throw new LockError("lockfile-unsupported-format", "npm requires v2/v3 packages");
  for (const [id, pkg] of Object.entries(lock.packages)) {
    if (!object(pkg)) fail("invalid npm package");
    if (pkg.link) continue;
    const name = pkg.name ?? (id.includes("node_modules/") ? id.split("node_modules/").at(-1) : id || lock.name || "app");
    g.add(id, name, pkg.version ?? (!id.includes("node_modules/") ? "0.0.0" : undefined), sourceFor(pkg.resolved), !id.includes("node_modules/") || /^(?:file:|link:)/.test(pkg.resolved ?? ""));
  }
  const resolve = (from, name) => {
    let dir = from;
    while (true) {
      const id = path.posix.join(dir, "node_modules", name);
      const linked = lock.packages[id];
      const target = linked?.link ? g.nodes.get(path.posix.normalize(linked.resolved)) : g.nodes.get(id);
      if (target) return target;
      if (!dir || dir === ".") return null;
      dir = path.posix.dirname(dir); if (dir === ".") dir = "";
    }
  };
  for (const [id, node] of g.nodes) {
    const pkg = lock.packages[id];
    for (const section of [...(node.local ? sections : ["dependencies", "optionalDependencies"]), "peerDependencies"]) {
      if (pkg[section] !== undefined && !object(pkg[section])) fail("invalid npm dependencies");
      for (const name of Object.keys(pkg[section] ?? {})) connect(node, resolve(id, name), name, section === "optionalDependencies" || section === "peerDependencies" && pkg.peerDependenciesMeta?.[name]?.optional === true);
    }
  }
  for (const { rel, bytes } of manifests) {
    const id = path.posix.relative(lockDir, path.posix.dirname(rel));
    const node = g.nodes.get(id);
    if (!node) { g.diagnostics.push({ reason: "lockfile-root-unresolved", detail: rel }); continue; }
    // Root display names come from app manifests, not declarations in ownership files.
    const pkg = JSON.parse(bytes.toString("utf8"));
    node.name = pkg.name ?? node.name;
    for (const section of [...sections, "peerDependencies"]) for (const name of Object.keys(pkg[section] ?? {})) connect(node, resolve(id, name), name, section === "optionalDependencies" || section === "peerDependencies" && pkg.peerDependenciesMeta?.[name]?.optional === true);
    g.roots.push(id);
  }
  return g;
}
function pnpmGraph(text, manifests, lockDir, workspaceBytes) {
  const lock = parsePnpmYaml(text), version = String(lock.lockfileVersion), g = graph();
  if (!/^(?:9|6)(?:\.\d+)?$/.test(version) || !object(lock.importers) || !object(lock.packages ?? {})) throw new LockError("lockfile-unsupported-format", "pnpm requires v6/v9 importers");
  const v9 = version.startsWith("9"), packages = lock.packages ?? {}, snapshots = v9 ? lock.snapshots ?? {} : packages;
  if (!object(snapshots)) fail("invalid pnpm snapshots");
  const packageIdentity = (id) => {
    const bare = id.replace(/^\//, ""), match = /^(@[^/]+\/[^@]+|[^@]+)@(.+)$/.exec(bare);
    if (!match) throw new LockError("lockfile-unsupported-format", `pnpm package key: ${id}`);
    return { name: match[1], version: match[2].split(/[(_]/)[0] };
  };
  for (const [id, pkg] of Object.entries(snapshots)) {
    if (!object(pkg)) fail("invalid pnpm snapshot");
    const identity = packageIdentity(id), metadata = packages[id] ?? packages[id.split("(")[0]] ?? {};
    const resolved = metadata.resolution?.tarball ?? (metadata.resolution?.directory ? `path:${metadata.resolution.directory}` : undefined);
    g.add(id, identity.name, metadata.version ?? identity.version, sourceFor(resolved), metadata.resolution?.type === "directory" || identity.version.startsWith("file:"));
  }
  for (const [id, importer] of Object.entries(lock.importers)) {
    if (!object(importer)) fail("invalid pnpm importer");
    g.add(`importer:${id}`, id, "0.0.0", `path:${id}`, true);
  }
  const resolve = (node, name, raw) => {
    const ref = object(raw) ? raw.version : raw;
    if (typeof ref !== "string") fail("invalid pnpm dependency version");
    if (/^(?:link:|workspace:)/.test(ref)) {
      if (!ref.startsWith("link:")) throw new LockError("lockfile-unresolved-dependency", ref);
      const importer = node.id.replace(/^importer:/, "");
      return g.nodes.get(`importer:${path.posix.normalize(path.posix.join(importer, ref.slice(5)))}`);
    }
    const candidates = [ref, `${name}@${ref}`, `/${name}@${ref}`];
    const exact = candidates.map((id) => g.nodes.get(id)).find(Boolean);
    if (exact) return exact;
    // Aliases encode the actual package name in the version reference.
    const normalized = ref.replace(/^npm:/, "");
    return g.nodes.get(normalized) ?? g.nodes.get(`/${normalized}`);
  };
  for (const node of g.nodes.values()) {
    const isImporter = node.id.startsWith("importer:"), pkg = isImporter ? lock.importers[node.id.slice(9)] : snapshots[node.id];
    for (const section of isImporter ? sections : ["dependencies", "optionalDependencies"]) {
      if (pkg[section] !== undefined && !object(pkg[section])) fail("invalid pnpm dependency map");
      for (const [name, raw] of Object.entries(pkg[section] ?? {})) connect(node, resolve(node, name, raw), name, section === "optionalDependencies");
    }
  }
  const membership = pnpmMembership(workspaceBytes);
  for (const { rel, bytes } of manifests) {
    const id = path.posix.relative(lockDir, path.posix.dirname(rel)) || ".", node = g.nodes.get(`importer:${id}`);
    if (!node) {
      const outside = membership && !membership(id === "." ? "" : id);
      (outside ? g.info : g.diagnostics).push({ reason: outside ? "manifest-outside-workspace" : "lockfile-root-unresolved", detail: rel }); continue;
    }
    node.name = JSON.parse(bytes.toString("utf8")).name ?? id;
    const pkg = JSON.parse(bytes.toString("utf8")), importer = lock.importers[id];
    for (const section of sections) for (const name of Object.keys(pkg[section] ?? {})) if (!Object.hasOwn(importer[section] ?? {}, name)) g.diagnostics.push({ reason: "lockfile-root-unresolved", detail: `${rel}: ${section}.${name}` });
    g.roots.push(node.id);
  }
  return g;
}

/** Lock bytes stay bound to the scan inventory or committed snapshot; no installed graph reads. */
export function transitiveDependencies(appRoot, inventory, manifests, catalogOwners, coverage, contentDigests, targets) {
  const result = [], info = [], cache = new Map(), groups = new Map(), files = new Map(inventory.files.map((file) => [file.rel, file])), targetNames = new Set(targets);
  const diagnostic = (rel, reason, detail) => coverage.push({ path: rel, status: "unscanned", reason, ...(detail ? { detail } : {}) });
  const read = (rel) => {
    if (cache.has(rel)) return cache.get(rel);
    let file = files.get(rel), bytes = file?.bytes;
    if (!file && !inventory.committed) {
      const absolute = path.resolve(appRoot, rel);
      if (existsSync(absolute)) {
        const st = lstatSync(absolute), root = realpathSync(appRoot), real = realpathSync(absolute);
        if (!st.isFile() || st.isSymbolicLink() || !real.startsWith(`${root}${path.sep}`) || st.size > 64 * 1024 * 1024) throw new LockError("lockfile-unreadable", rel);
        bytes = readFileSync(absolute); file = { rel, bytes };
        diagnostic(rel, "lockfile-not-in-inventory", "lock bytes are absent from Git-listed scan inputs");
      }
    }
    if (file && !bytes) throw new LockError("lockfile-unreadable", rel);
    cache.set(rel, bytes);
    if (bytes && !contentDigests.some((item) => item.path === rel)) contentDigests.push({ path: rel, digest: sha256(bytes) });
    return bytes;
  };
  // Directories the repository declares as gate fixtures (ci/gate.config.json `fixtures`: consumer recipes, vendored crates,
  // spikes) are samples and harnesses, never the app's code: the gate builds none of them as units, and neither are they scanned here.
  let fixtures = [];
  try {
    const rel = "ci/gate.config.json";
    const config = inventory.snapshotFiles?.get(`${inventory.pathPrefix ?? ""}${rel}`)?.bytes ?? files.get(rel)?.bytes
      ?? (!inventory.committed && existsSync(path.resolve(appRoot, rel)) ? readFileSync(path.resolve(appRoot, rel)) : undefined);
    if (config) fixtures = (JSON.parse(config.toString("utf8")).fixtures ?? []).map((d) => String(d).replace(/\/+$/, ""));
  } catch { fixtures = []; }
  const underFixture = (rel) => fixtures.some((d) => rel === d || rel.startsWith(`${d}/`));
  for (const manifest of manifests) {
    const base = path.posix.basename(manifest.rel), ecosystem = base === "Cargo.toml" ? "cargo" : base === "package.json" ? "npm" : null;
    if (!ecosystem || !manifest.bytes) continue;
    if (underFixture(manifest.rel)) continue;
    // A Cargo manifest with its own [workspace] table is a workspace root: its lock sits beside it and Cargo never looks higher.
    const workspaceRoot = ecosystem === "cargo" && /^\s*\[workspace\]\s*$/m.test(manifest.bytes.toString("utf8"));
    const names = ecosystem === "cargo" ? ["Cargo.lock"] : OWNERSHIP_LOCKFILES.slice(1);
    let dir = path.posix.dirname(manifest.rel), found = false;
    try {
      while (true) {
        const locks = names.map((name) => path.posix.join(dir, name)).filter((rel) => read(rel) !== undefined);
        if (locks.length) {
          for (const rel of locks) {
            if (!groups.has(rel)) groups.set(rel, { ecosystem, manifests: [] });
            groups.get(rel).manifests.push(manifest);
          }
          found = true; break;
        }
        if (dir === "." || workspaceRoot) break;
        dir = path.posix.dirname(dir);
      }
      if (!found) diagnostic(manifest.rel, "lockfile-missing", ecosystem);
    } catch (error) { diagnostic(manifest.rel, error.reason ?? "lockfile-unreadable", error.message); }
  }
  for (const [rel, group] of groups) {
    try {
      const text = read(rel).toString("utf8"), base = path.posix.basename(rel), dir = path.posix.dirname(rel);
      if (base === "yarn.lock") throw new LockError("lockfile-unsupported-format", "yarn transitive graph");
      const g = group.ecosystem === "cargo" ? cargoGraph(text, group.manifests, dir) : base === "pnpm-lock.yaml" ? pnpmGraph(text, group.manifests, dir, read(path.posix.join(dir, "pnpm-workspace.yaml"))) : packageLockGraph(text, group.manifests, dir);
      for (const entry of g.diagnostics) diagnostic(rel, entry.reason, entry.detail);
      // Non-blocking: manifests outside the lock's workspace are reported, never counted as coverage gaps.
      for (const entry of g.info) info.push({ path: rel, status: "info", reason: entry.reason, detail: entry.detail });
      for (const node of g.nodes.values()) for (const detail of node.unresolved ?? []) diagnostic(rel, "lockfile-unresolved-dependency", detail);
      // Retain only nodes leading to block fingerprints. Unrelated lock inventory cannot
      // multiply traversal work; owner boundaries cannot authorize app/workspace nodes.
      const reverse = new Map(), viable = new Set();
      for (const node of g.nodes.values()) {
        if (!node.local && catalogOwners.has(node.name)) continue;
        if (targetNames.has(`${group.ecosystem}:${node.name}`)) viable.add(node.id);
        for (const id of node.edges) {
          if (!reverse.has(id)) reverse.set(id, []);
          reverse.get(id).push(node.id);
        }
      }
      const ancestors = [...viable];
      for (let at = 0; at < ancestors.length; at++) for (const id of reverse.get(ancestors[at]) ?? []) if (!viable.has(id)) { viable.add(id); ancestors.push(id); }
      const seen = new Set(); let steps = 0;
      for (const root of new Set(g.roots)) {
        const queue = [[root]];
        for (let at = 0; at < queue.length; at++) {
          if (++steps > 20000) throw new LockError("lockfile-traversal-limit", "more than 20000 dependency paths");
          const ids = queue[at], node = g.nodes.get(ids.at(-1));
          // App/workspace names & app ownership declarations never grant an owner boundary.
          if (ids.length > 1 && !node.local && catalogOwners.has(node.name)) continue;
          const dependencyPath = ids.map((id) => g.nodes.get(id).name);
          if (ids.length > 2 && targetNames.has(`${group.ecosystem}:${node.name}`)) {
            const key = JSON.stringify([node.name, node.version, node.source, dependencyPath]);
            if (!seen.has(key)) {
              seen.add(key); result.push({ ecosystem: group.ecosystem, name: node.name, resolvedVersion: node.version, source: node.source, file: rel, lockfile: rel, dependencyPath });
            }
          }
          for (const id of node.edges) if (viable.has(id) && !ids.includes(id)) {
            if (queue.length >= 20000) throw new LockError("lockfile-traversal-limit", "more than 20000 dependency paths");
            queue.push([...ids, id]);
          }
        }
      }
    } catch (error) { diagnostic(rel, error.reason ?? "lockfile-parse-failed", error.message); }
  }
  result.sort((a, b) => a.dependencyPath.length - b.dependencyPath.length || JSON.stringify(a.dependencyPath).localeCompare(JSON.stringify(b.dependencyPath)));
  Object.defineProperty(result, "info", { value: info, enumerable: false });
  return result;
}
