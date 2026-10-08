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
function parsePnpmYaml(text) {
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

function graph() {
  const nodes = new Map(), roots = [];
  return { nodes, roots, diagnostics: [], add(id, name, version, source, local = false) {
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
function cargoGraph(text, manifests) {
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
  for (const { rel, bytes } of manifests) {
    const pkg = parseToml(bytes.toString("utf8")).package;
    if (!pkg) continue; // Virtual workspace: member manifests supply roots.
    const candidates = [...g.nodes.values()].filter((n) => n.name === pkg.name && n.local && (typeof pkg.version !== "string" || n.version === pkg.version));
    if (candidates.length !== 1) { g.diagnostics.push({ reason: "lockfile-root-unresolved", detail: rel }); continue; }
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
function pnpmGraph(text, manifests, lockDir) {
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
  for (const { rel, bytes } of manifests) {
    const id = path.posix.relative(lockDir, path.posix.dirname(rel)) || ".", node = g.nodes.get(`importer:${id}`);
    if (!node) { g.diagnostics.push({ reason: "lockfile-root-unresolved", detail: rel }); continue; }
    node.name = JSON.parse(bytes.toString("utf8")).name ?? id;
    const pkg = JSON.parse(bytes.toString("utf8")), importer = lock.importers[id];
    for (const section of sections) for (const name of Object.keys(pkg[section] ?? {})) if (!Object.hasOwn(importer[section] ?? {}, name)) g.diagnostics.push({ reason: "lockfile-root-unresolved", detail: `${rel}: ${section}.${name}` });
    g.roots.push(node.id);
  }
  return g;
}

/** Lock bytes stay bound to the scan inventory or committed snapshot; no installed graph reads. */
export function transitiveDependencies(appRoot, inventory, manifests, catalogOwners, coverage, contentDigests, targets) {
  const result = [], cache = new Map(), groups = new Map(), files = new Map(inventory.files.map((file) => [file.rel, file])), targetNames = new Set(targets);
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
  for (const manifest of manifests) {
    const base = path.posix.basename(manifest.rel), ecosystem = base === "Cargo.toml" ? "cargo" : base === "package.json" ? "npm" : null;
    if (!ecosystem || !manifest.bytes) continue;
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
        if (dir === ".") break;
        dir = path.posix.dirname(dir);
      }
      if (!found) diagnostic(manifest.rel, "lockfile-missing", ecosystem);
    } catch (error) { diagnostic(manifest.rel, error.reason ?? "lockfile-unreadable", error.message); }
  }
  for (const [rel, group] of groups) {
    try {
      const text = read(rel).toString("utf8"), base = path.posix.basename(rel), dir = path.posix.dirname(rel);
      if (base === "yarn.lock") throw new LockError("lockfile-unsupported-format", "yarn transitive graph");
      const g = group.ecosystem === "cargo" ? cargoGraph(text, group.manifests) : base === "pnpm-lock.yaml" ? pnpmGraph(text, group.manifests, dir) : packageLockGraph(text, group.manifests, dir);
      for (const entry of g.diagnostics) diagnostic(rel, entry.reason, entry.detail);
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
  return result.sort((a, b) => a.dependencyPath.length - b.dependencyPath.length || JSON.stringify(a.dependencyPath).localeCompare(JSON.stringify(b.dependencyPath)));
}
