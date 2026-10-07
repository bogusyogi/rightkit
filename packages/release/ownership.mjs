// RightKit ownership scan. Public exports retain the original findings-array API.
import { spawnSync } from "./background-process.mjs";
import { canonicalRepositoryIdentity, loadTrustedExternal, occurrenceHash, resolveKeyLogTrust, sha256, verifyException, verifyProtectedProvenance } from "./ownership-approval.mjs";
import { parse as parseToml } from "smol-toml";
import { existsSync, lstatSync, readdirSync, readFileSync, realpathSync } from "node:fs";
import { createHash } from "node:crypto";
import { acquireCommittedSource, committedSourceDataForInternalUse } from "./committed-source.mjs";
import { readCommittedKeyAuthority } from "./ownership-key-authority.mjs";
import { gitListedInventory } from "./ownership-git-inventory.mjs";
import os from "node:os";
import path from "node:path";

export const MANIFEST_FILE = "rightkit-ownership.json";
export const EXCEPTIONS_FILE = path.join(".rightkit", "ownership-exceptions.json");
const CODE_EXT = new Set([".rs", ".ts", ".tsx", ".js", ".mjs", ".cjs", ".jsx", ".swift", ".m", ".mm", ".c", ".cc", ".cpp", ".h", ".py", ".go", ".kt", ".java", ".cs"]);
// Listed installed/build inputs retain explicit coverage diagnostics.
const OPAQUE_DIRS = new Set(["target", ".build", "DerivedData", "Pods"]);
export const SCANNER_VERSION = "2.2.0";
export const catalogDigest = (manifest) => sha256(manifest);
const byteDigest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const inputDigest = (file) => file.digest ?? byteDigest(file.bytes);
const sourceCache = new Map();
const catalogCache = new Map();

function sourceMatchers(manifest) {
  const digest = catalogDigest(manifest);
  if (catalogCache.has(digest)) return catalogCache.get(digest);
  const sources = manifest.capabilities.flatMap((c) => c.fingerprints.filter((f) => f.type === "source").map((f) => ({ c, f, re: new RegExp(f.pattern, f.flags ?? "") })));
  let prefilter;
  try {
    // Backreferences cannot be relocated safely inside one alternation. In that
    // case use a permissive prefilter; individual expressions stay authoritative.
    if (sources.some(({ f }) => /\\[1-9]|\\k</.test(f.pattern))) throw new Error("backreference");
    const flags = [...new Set(`m${sources.map(({ re }) => re.flags.replace(/[gy]/g, "")).join("")}`)].join("");
    prefilter = new RegExp(sources.map(({ f }) => `(?:${f.pattern})`).join("|") || "(?!)", flags);
  } catch { prefilter = /[\s\S]/; }
  const matchers = { digest, sources, prefilter };
  catalogCache.set(digest, matchers);
  if (catalogCache.size > 8) catalogCache.delete(catalogCache.keys().next().value);
  return matchers;
}

function sourceFindings(file, matchers) {
  const key = `${matchers.digest}:${file.cacheKey ?? inputDigest(file)}`;
  if (sourceCache.has(key)) return sourceCache.get(key);
  const text = file.bytes.toString("utf8"), findings = [];
  if (matchers.prefilter.test(text)) {
    for (const [i, line] of text.split(/\r?\n/).entries()) if (!commentOnly(line)) for (const { c, f, re } of matchers.sources) {
      re.lastIndex = 0;
      if (re.test(line)) findings.push({ fingerprint: f.id, capability: c.capability, package: c.package, line: i + 1, text: line.trim().slice(0, 160) });
    }
  }
  sourceCache.set(key, findings);
  if (sourceCache.size > 16384) sourceCache.delete(sourceCache.keys().next().value);
  return findings;
}

export function aggregateOwnership(rightkitRoot) {
  const entries = [];
  for (const group of ["crates", "packages", "swift"]) {
    const dir = path.join(rightkitRoot, group);
    if (!existsSync(dir)) continue;
    for (const name of readdirSync(dir).sort()) {
      const child = path.join(dir, name);
      let file = path.join(child, MANIFEST_FILE);
      if (!existsSync(file) && group === "swift") file = path.join(child, "rightkit-ownership.json");
      if (!existsSync(file)) continue;
      const declared = JSON.parse(readFileSync(file, "utf8"));
      if (declared.schema !== undefined) continue;
      for (const capability of declared.capabilities ?? []) entries.push({ package: declared.package, ...capability });
    }
  }
  const manifest = { schema: 1, version: "1", capabilities: entries };
  validateManifest(manifest);
  return manifest;
}

export function validateManifest(manifest) {
  const ids = new Set();
  for (const c of manifest.capabilities ?? []) {
    if (!c.package || !c.capability || !Array.isArray(c.fingerprints) || !c.fingerprints.length) throw new Error(`ownership: incomplete capability ${JSON.stringify(c).slice(0, 120)}`);
    for (const f of c.fingerprints) {
      if (!f.id || ids.has(f.id)) throw new Error(`ownership: missing or duplicate fingerprint id ${f.id}`);
      ids.add(f.id);
      if (f.type === "dependency") {
        if (!["cargo", "npm", "swiftpm"].includes(f.ecosystem) || !f.name) throw new Error(`ownership: bad dependency fingerprint ${f.id}`);
      } else if (f.type === "source") new RegExp(f.pattern);
      else throw new Error(`ownership: unknown fingerprint type ${f.type} (${f.id})`);
    }
  }
}

function addDependency(out, ecosystem, name, file, details = {}) {
  if (!name) return;
  out.push({ ecosystem, name, file, ...details });
}

function cargoWorkspace(parsed) { return parsed.workspace?.dependencies ?? {}; }
function resolveLocalPath(root, sourceFile, pathValue, coverage, display, inventory, reasonPrefix = "local-dependency", detail = "") {
  if (!pathValue) return;
  if (inventory?.committed) {
    if (path.posix.isAbsolute(pathValue) || path.win32.isAbsolute(pathValue) || pathValue.includes("\\")) { coverage.push({ path: display, status: "unscanned", reason: `${reasonPrefix}-outside-repository${detail ? `:${detail}` : ""}` }); return; }
    const relative = path.posix.normalize(path.posix.join(path.posix.dirname(sourceFile), pathValue));
    if (relative === ".." || relative.startsWith("../") || path.posix.isAbsolute(relative)) { coverage.push({ path: display, status: "unscanned", reason: `${reasonPrefix}-outside-repository${detail ? `:${detail}` : ""}` }); return; }
    const rel = `${inventory.pathPrefix ?? ""}${relative === "." ? "" : `${relative}/`}`.replace(/\/$/, "");
    const exists = relative === "." || inventory.repositoryFiles.some((item) => item.rel === rel || item.rel.startsWith(`${rel}/`));
    if (!exists) coverage.push({ path: display, status: "unscanned", reason: `${reasonPrefix}-path-unresolved${detail ? `:${detail}` : ""}` });
    return;
  }
  const resolved = path.resolve(root, path.posix.dirname(sourceFile), pathValue);
  try {
    const rootReal = realpathSync(root), targetReal = realpathSync(resolved);
    if (targetReal !== rootReal && !targetReal.startsWith(rootReal + path.sep)) coverage.push({ path: display, status: "unscanned", reason: `${reasonPrefix}-outside-repository${detail ? `:${detail}` : ""}` });
  } catch { coverage.push({ path: display, status: "unscanned", reason: `${reasonPrefix}-path-unresolved${detail ? `:${detail}` : ""}` }); }
}

function parseCargo(parsed, file, out, workspace, root, coverage, inventory) {
  const pathSpec = (pathValue, sourceFile, display) => {
    resolveLocalPath(root, sourceFile, pathValue, coverage, display, inventory);
  };
  const addTable = (table) => {
    if (!table || typeof table !== "object") return;
    for (const section of ["dependencies", "dev-dependencies", "build-dependencies"]) for (const [alias, raw] of Object.entries(table[section] ?? {})) {
      const spec = raw && typeof raw === "object" ? raw : {};
      const inherited = spec.workspace === true ? workspace.deps?.[alias] ?? {} : {};
      const name = spec.package ?? inherited.package ?? alias;
      const source = spec.path ?? inherited.path ? "path" : spec.git ?? inherited.git ? "git" : undefined;
      addDependency(out, "cargo", name, file, { local: source === "path" || source === "git", source });
      if (source === "path") pathSpec(spec.path ?? inherited.path, spec.path ? file : path.posix.join(workspace.dir ?? ".", "Cargo.toml"), file);
      if (source === "git") coverage.push({ path: file, status: "unscanned", reason: `git-dependency-source:${name}` });
    }
    for (const target of Object.values(table.target ?? {})) addTable(target);
  };
  addTable(parsed);
}

function listedCoverage(inventory) {
  const reported = new Set();
  for (const { rel } of inventory.files) {
    const parts = rel.split("/");
    for (const [at, reason] of [[parts.indexOf("node_modules"), "node-modules-provenance-not-verified"], [parts.findIndex((part) => OPAQUE_DIRS.has(part)), "installed-or-build-cache"]]) {
      if (at < 0) continue;
      const dir = parts.slice(0, at + 1).join("/"), key = `${dir}:${reason}`;
      if (!reported.has(key)) { inventory.coverage.push({ path: dir, status: "unscanned", reason }); reported.add(key); }
    }
  }
  return inventory;
}

function fileInventory(appRoot, trust, pathPrefix = "", repositoryRoot = appRoot) {
  const protectedPaths = new Set((trust?.provenance ?? []).flatMap((record) => (record.paths ?? []).map((p) => pathPrefix && p.startsWith(pathPrefix) ? p.slice(pathPrefix.length) : p)));
  const inventory = gitListedInventory(appRoot, {
    repositoryRoot,
    protectedPaths: [...protectedPaths],
    relevant: (rel) => ["Cargo.toml", "package.json", "Package.swift"].includes(path.posix.basename(rel)) || CODE_EXT.has(path.posix.extname(rel).toLowerCase()),
    maxFileBytes: (rel) => CODE_EXT.has(path.posix.extname(rel).toLowerCase()) || protectedPaths.has(rel) ? 2 * 1024 * 1024 : 64 * 1024 * 1024,
  });
  for (const item of inventory.coverage) if (item.reason === "file-over-size-limit") item.reason = protectedPaths.has(item.path) ? "protected-provenance-file-over-2MiB" : CODE_EXT.has(path.posix.extname(item.path).toLowerCase()) ? "source-file-over-2MiB" : "manifest-file-over-64MiB";
  return listedCoverage(inventory);
}

function committedFileInventory(appRoot, trust, pathPrefix, committed) {
  const prefix = pathPrefix ? `${pathPrefix.replace(/\/$/, "")}/` : "";
  const repositoryFiles = committed.entries.filter((entry) => entry.type === "blob").map((entry) => ({ rel: entry.rel, size: entry.size, bytes: entry.bytes, mode: entry.mode }));
  const files = [], coverage = committed.coverage.filter((entry) => !prefix || entry.path.startsWith(prefix)).map((entry) => ({ ...entry, path: prefix ? entry.path.slice(prefix.length) : entry.path }));
  const protectedPaths = new Set((trust?.provenance ?? []).flatMap((record) => record.paths ?? []));
  const reportedOpaque = new Set(), reportedNodeModules = new Set();
  for (const entry of committed.entries) {
    if (prefix && !entry.rel.startsWith(prefix)) continue;
    const rel = prefix ? entry.rel.slice(prefix.length) : entry.rel;
    if (!rel) continue;
    if (entry.mode === "120000") { coverage.push({ path: rel, status: "unscanned", reason: "committed-symlink" }); continue; }
    if (entry.mode === "160000" || entry.type === "commit") { coverage.push({ path: rel, status: "unscanned", reason: "committed-gitlink" }); continue; }
    if (entry.type !== "blob") { coverage.push({ path: rel, status: "unscanned", reason: "committed-unsupported-entry" }); continue; }
    const relevant = protectedPaths.has(entry.rel) || ["Cargo.toml", "package.json", "Package.swift", "ownership-exceptions.json"].includes(path.posix.basename(rel)) || CODE_EXT.has(path.posix.extname(rel).toLowerCase());
    const bytes = relevant ? entry.bytes : undefined;
    if (relevant && !bytes) coverage.push({ path: rel, status: "unscanned", reason: "committed-blob-unavailable" });
    const isExceptionFile = path.posix.basename(rel) === "ownership-exceptions.json";
    const limit = CODE_EXT.has(path.posix.extname(rel).toLowerCase()) || protectedPaths.has(entry.rel) || isExceptionFile ? 2 * 1024 * 1024 : 64 * 1024 * 1024;
    if (relevant && entry.size > limit) coverage.push({ path: rel, status: "unscanned", reason: isExceptionFile ? "committed-exceptions-over-2MiB" : protectedPaths.has(entry.rel) ? "protected-provenance-file-over-2MiB" : "source-file-over-2MiB" });
    const parts = rel.split("/");
    const nodeModulesAt = parts.indexOf("node_modules");
    if (nodeModulesAt >= 0) {
      const dir = parts.slice(0, nodeModulesAt + 1).join("/");
      if (!reportedNodeModules.has(dir)) { coverage.push({ path: dir, status: "unscanned", reason: "node-modules-provenance-not-verified" }); reportedNodeModules.add(dir); }
    }
    const opaqueAt = parts.findIndex((part) => OPAQUE_DIRS.has(part));
    if (opaqueAt >= 0) {
      const dir = parts.slice(0, opaqueAt + 1).join("/");
      if (!reportedOpaque.has(dir)) { coverage.push({ path: dir, status: "unscanned", reason: "installed-or-build-cache" }); reportedOpaque.add(dir); }
    }
    files.push({ rel, abs: path.join(appRoot, ...rel.split("/")), size: entry.size, bytes: entry.size <= limit ? bytes : undefined, inNodeModules: rel.split("/").includes("node_modules"), oid: entry.oid, digest: `git:${entry.oid}`, cacheKey: `gitblob:${entry.oid}` });
  }
  return { files, coverage, committed: true, pathPrefix, repositoryFiles, snapshotFiles: new Map(repositoryFiles.map((file) => [file.rel, file])) };
}

function dependenciesFromInventory(appRoot, inventory) {
  const deps = [], coverage = [], contentDigests = [];
  // Workspace inheritance is loaded before member manifests.
  const manifestNames = new Set(["Cargo.toml", "package.json", "Package.swift"]);
  const manifests = inventory.files.filter((x) => manifestNames.has(path.posix.basename(x.rel)));
  const primaryFiles = manifests.filter((x) => !x.inNodeModules);
  // Follow only explicitly declared local package roots into installed layouts. Their own
  // manifests are first-party inputs; unrelated installed transitive manifests stay opaque.
  const selected = new Set(primaryFiles.map((x) => x.rel));
  const visitedRoots = new Set();
  const queue = primaryFiles.filter((x) => path.posix.basename(x.rel) === "package.json");
  while (queue.length) {
    const manifest = queue.shift();
    try {
      const pkg = JSON.parse(manifest.bytes.toString("utf8"));
      for (const field of ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"]) for (const spec of Object.values(pkg[field] ?? {})) {
        if (typeof spec !== "string") continue;
        const local = spec.match(/^(?:file:|link:)(.+)$/)?.[1];
        if (!local) continue;
        const target = path.resolve(appRoot, path.posix.dirname(manifest.rel), local);
        const relative = path.relative(appRoot, target).split(path.sep).join("/").replace(/\/$/, "");
        if (visitedRoots.has(relative)) continue;
        visitedRoots.add(relative);
        for (const x of manifests) if (x.rel === `${relative}/package.json` || x.rel.startsWith(`${relative}/`) && manifestNames.has(path.posix.basename(x.rel))) {
          if (!selected.has(x.rel)) selected.add(x.rel);
          if (path.posix.basename(x.rel) === "package.json") queue.push(x);
        }
      }
    } catch { /* malformed manifests are reported in the normal parsing pass */ }
  }
  const files = manifests.filter((x) => selected.has(x.rel));
  const cargoFiles = files.filter((x) => path.posix.basename(x.rel) === "Cargo.toml");
  const workspaces = [];
  for (const x of cargoFiles) {
      try { const parsed = parseToml(x.bytes.toString("utf8")); const deps = cargoWorkspace(parsed); if (Object.keys(deps).length) workspaces.push({ dir: path.posix.dirname(x.rel), deps }); }
    catch { coverage.push({ path: x.rel, status: "unscanned", reason: "invalid-cargo-toml" }); }
  }
  for (const x of files) {
    const { rel, bytes } = x;
    if (!bytes) continue;
    const base = path.posix.basename(rel), text = bytes.toString("utf8");
    contentDigests.push({ path: rel, digest: inputDigest(x) });
    if (base === "Cargo.toml") {
      const dir = path.posix.dirname(rel);
      const workspace = workspaces.filter((x) => x.dir === "." || dir === x.dir || dir.startsWith(`${x.dir}/`)).sort((a, b) => b.dir.length - a.dir.length)[0] ?? { dir: ".", deps: {} };
      try { parseCargo(parseToml(text), rel, deps, workspace, appRoot, coverage, inventory); }
      catch { coverage.push({ path: rel, status: "unscanned", reason: "invalid-cargo-toml" }); }
    }
    else if (base === "package.json") {
      let pkg; try { pkg = JSON.parse(text); } catch { coverage.push({ path: rel, status: "unscanned", reason: "invalid-json" }); continue; }
      for (const field of ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"]) for (const [name, spec] of Object.entries(pkg[field] ?? {})) {
        if (typeof spec !== "string") continue;
        const alias = spec.match(/^(?:npm:)(@[^/]+\/[^@]+|[^@]+)(?:@|$)/)?.[1];
        const localPath = spec.match(/^(?:file:|link:)(.+)$/)?.[1];
        const isGit = /^(git\+|https?:.*\.git)/i.test(spec);
        addDependency(deps, "npm", alias ?? name, rel, { alias: alias ? name : undefined, local: !!localPath, source: localPath ? spec.split(":")[0] : isGit ? "git" : spec.startsWith("workspace:") ? "workspace" : undefined });
        if (localPath) {
          resolveLocalPath(appRoot, rel, localPath, coverage, rel, inventory, "local-npm-dependency", name);
        } else if (isGit) coverage.push({ path: rel, status: "unscanned", reason: `git-npm-dependency-source:${name}` });
      }
    } else {
      for (const m of text.matchAll(/\.package\s*\(([^)]*)\)/g)) {
        const arg = m[1], url = arg.match(/url:\s*["']([^"']+)["']/)?.[1], pathDep = arg.match(/path:\s*["']([^"']+)["']/)?.[1];
        const id = arg.match(/name:\s*["']([^"']+)["']/)?.[1] ?? url?.replace(/\.git$/i, "").split("/").pop()?.toLowerCase() ?? pathDep?.split(/[\\/]/).pop();
        addDependency(deps, "swiftpm", id, rel, { local: !!pathDep, source: pathDep ? "path" : url });
        if (pathDep) {
          resolveLocalPath(appRoot, rel, pathDep, coverage, rel, inventory, "local-swift-dependency", id);
        }
      }
    }
  }
  Object.defineProperty(deps, "coverage", { value: coverage, enumerable: false });
  Object.defineProperty(deps, "contentDigests", { value: contentDigests, enumerable: false });
  return deps;
}

export function directDependencies(appRoot) {
  const inventory = fileInventory(appRoot);
  const deps = dependenciesFromInventory(appRoot, inventory);
  deps.coverage.push(...inventory.coverage);
  return deps;
}

/** Git-listed content inventory; clean inputs use their blob IDs. */
export function contentInventory(appRoot) {
  const inventory = listedCoverage(gitListedInventory(appRoot, { maxFileBytes: () => 64 * 1024 * 1024 }));
  const files = inventory.files.filter((file) => file.digest).map((file) => ({ path: file.rel, size: file.size, digest: file.digest }));
  files.sort((a, b) => a.path.localeCompare(b.path));
  return { digest: sha256(files), files, coverage: inventory.coverage, complete: inventory.coverage.length === 0 };
}

function commentOnly(line) { const t = line.trim(); return t.startsWith("//") || t.startsWith("#") || t.startsWith("*") || t.startsWith("/*") || t.startsWith("--"); }
function repositoryIdentityFor(root) {
  const origin = spawnSync("git", ["-C", root, "config", "--get", "remote.origin.url"], { encoding: "utf8", windowsHide: true, maxBuffer: 4 * 1024 * 1024 });
  if (origin.status !== 0) return null;
  try { return canonicalRepositoryIdentity(origin.stdout.trim()); } catch { return null; }
}

function scanRepositoryRoot(appRoot) {
  const result = spawnSync("git", ["-C", appRoot, "rev-parse", "--show-toplevel", "HEAD"], { encoding: "utf8", windowsHide: true, maxBuffer: 4 * 1024 * 1024 });
  const [rootPath, head] = String(result.stdout ?? "").trimEnd().split(/\r?\n/);
  const commit = /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/.test(head ?? "") ? head : null;
  // Unborn repositories still print their root before HEAD resolution fails.
  if (!rootPath || !path.isAbsolute(rootPath)) return { root: path.resolve(appRoot), prefix: "", pathBase: "scan-root", commit };
  const root = path.resolve(rootPath), app = path.resolve(appRoot);
  const relative = path.relative(root, app);
  if (relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) return { root: app, prefix: "", pathBase: "scan-root", commit };
  return { root, prefix: relative ? `${relative.split(path.sep).join("/")}/` : "", pathBase: "repository", commit };
}

function trustedProvenance(appRoot, inventory, trust, repositoryIdentity, coverage, repositoryRoot, pathPrefix) {
  const excluded = new Map(), classifications = [];
  if (!trust || !repositoryIdentity) return { excluded, classifications };
  const scannedFiles = new Map(inventory.files.map((file) => [file.rel, file]));
  for (const record of trust.provenance ?? []) {
    if (record.repositoryIdentity !== repositoryIdentity) continue;
    if (!(record.paths ?? []).some((rel) => !pathPrefix || rel.startsWith(pathPrefix))) continue;
    const files = [];
    let safe = true;
    for (const rel of record.paths ?? []) {
      if (inventory.committed) {
        const snapshotFile = inventory.snapshotFiles.get(rel);
        if (!snapshotFile?.bytes || snapshotFile.size > 2 * 1024 * 1024) { safe = false; break; }
        files.push({ path: rel, content: snapshotFile.bytes });
        continue;
      }
      const filePath = path.resolve(repositoryRoot, ...rel.split("/"));
      const rootReal = realpathSync(repositoryRoot);
      if (!filePath.startsWith(rootReal + path.sep)) { safe = false; break; }
      try {
        let current = repositoryRoot, st;
        for (const part of rel.split("/")) {
          current = path.join(current, part);
          st = lstatSync(current);
          if (st.isSymbolicLink()) { safe = false; break; }
        }
        if (!safe || !st?.isFile()) { safe = false; break; }
        let content;
        if (!pathPrefix || rel.startsWith(pathPrefix)) {
          const inventoried = scannedFiles.get(pathPrefix ? rel.slice(pathPrefix.length) : rel);
          if (!inventoried?.bytes || inventoried.size > 2 * 1024 * 1024) { safe = false; break; }
          content = inventoried.bytes;
        } else {
          // Multi-root records may include repository siblings; read them only to verify the full signed record.
          if (st.size > 2 * 1024 * 1024) { safe = false; break; }
          content = readFileSync(current);
        }
        files.push({ path: rel, content });
      } catch { safe = false; break; }
    }
    const verified = safe && verifyProtectedProvenance(trust, { repositoryIdentity, paths: record.paths, files, kind: record.kind, requestId: record.requestId });
    if (!verified?.ok) {
      for (const rel of record.paths ?? []) if (!pathPrefix || rel.startsWith(pathPrefix)) coverage.push({ path: pathPrefix ? rel.slice(pathPrefix.length) : rel, status: "unscanned", reason: "protected-provenance-stale-or-unreadable" });
      continue;
    }
    const classification = { kind: record.kind, requestId: record.requestId, contentDigest: record.contentDigest };
    classifications.push({ ...classification, paths: [...record.paths].sort() });
    for (const rel of record.paths) if (!pathPrefix || rel.startsWith(pathPrefix)) excluded.set(pathPrefix ? rel.slice(pathPrefix.length) : rel, classification);
  }
  return { excluded, classifications };
}

function scanInternal(appRoot, manifest, { inventory = fileInventory(appRoot), trust, repositoryIdentity, repositoryRoot = appRoot, pathPrefix = "" } = {}) {
  const findings = [], coverage = [...inventory.coverage], deps = dependenciesFromInventory(appRoot, inventory), contentDigests = [...(deps.contentDigests ?? [])];
  coverage.push(...(deps.coverage ?? []));
  const provenance = trustedProvenance(appRoot, inventory, trust, repositoryIdentity, coverage, repositoryRoot, pathPrefix);
  for (const c of manifest.capabilities) for (const f of c.fingerprints.filter((x) => x.type === "dependency")) for (const d of deps) {
    if (d.ecosystem === f.ecosystem && (d.name === f.name || d.alias === f.name)) {
      findings.push({ fingerprint: f.id, capability: c.capability, package: c.package, file: d.file, line: 0, text: `${d.ecosystem} dependency ${d.alias ? `${d.alias} aliases ` : ""}${d.name}`, local: d.local, provenance: d.source });
    }
  }
  const matchers = sourceMatchers(manifest);
  for (const file of inventory.files) {
    const { rel, size, bytes } = file;
    if (!CODE_EXT.has(path.posix.extname(rel).toLowerCase()) || !bytes) continue;
    if (provenance.excluded.has(rel)) {
      contentDigests.push({ path: rel, digest: inputDigest(file), provenance: provenance.excluded.get(rel) });
      continue;
    }
    if (size > 2 * 1024 * 1024) { coverage.push({ path: rel, status: "unscanned", reason: "source-file-over-2MiB" }); continue; }
    contentDigests.push({ path: rel, digest: inputDigest(file) });
    for (const finding of sourceFindings(file, matchers)) findings.push({ ...finding, file: rel });
  }
  if (pathPrefix) {
    const prefix = (p) => p === "." ? pathPrefix.slice(0, -1) : `${pathPrefix}${p}`;
    for (const f of findings) f.file = prefix(f.file);
    for (const c of coverage) c.path = prefix(c.path);
    for (const d of deps) d.file = prefix(d.file);
    for (const d of contentDigests) d.path = prefix(d.path);
  }
  for (const f of findings) f.occurrenceHash = occurrenceHash(f);
  contentDigests.sort((a, b) => a.path.localeCompare(b.path));
  return { findings, coverage, deps, contentDigests, provenance: provenance.classifications };
}

function trustForScan(options = {}) {
  if (options.trust !== undefined) return options.trust?.trust ?? options.trust;
  try { return loadTrustedExternal().trust; } catch { return null; }
}

function resolveSharedAuthority(trust) {
  if (!trust?.keyAuthority) return { trust };
  const authority = readCommittedKeyAuthority(trust);
  if (!authority.ok) throw new Error(`protected ownership key authority failed: ${authority.problems.join("; ")}`);
  if (!authority.resolvedTrust.ownershipKeys.length) throw new Error("protected ownership key authority has no active approval keys");
  return { trust: authority.resolvedTrust, keyLogDigest: authority.digest, keyCheckpoint: authority.keyCheckpoint, rootFingerprint: authority.rootFingerprint, keyAuthority: authority.authority };
}

function resolveRepositoryKeyLog(trust, repositoryRoot, repositoryIdentity) {
  if (!trust?.rootFingerprint || trust.keyAuthority) return { trust };
  const authority = readCommittedKeyAuthority({ ...trust, keyAuthority: { repositoryIdentity, checkoutPath: repositoryRoot } });
  if (!authority.ok) throw new Error(`protected ownership key log failed: ${authority.problems.join("; ")}`);
  if (!authority.resolvedTrust.ownershipKeys.length) throw new Error("protected ownership key log has no active approval keys");
  return { trust: authority.resolvedTrust, keyLogDigest: authority.digest, keyCheckpoint: authority.keyCheckpoint, rootFingerprint: authority.rootFingerprint };
}

export function scanApp(appRoot, manifest, options = {}) {
  const initialTrust = trustForScan(options), location = scanRepositoryRoot(appRoot);
  const repositoryIdentity = repositoryIdentityFor(appRoot);
  const authority = initialTrust?.keyAuthority ? resolveSharedAuthority(initialTrust) : resolveRepositoryKeyLog(initialTrust, location.root, repositoryIdentity);
  const trust = authority.trust;
  const result = scanInternal(appRoot, manifest, { inventory: fileInventory(appRoot, trust, location.prefix, location.root), trust, repositoryIdentity, repositoryRoot: location.root, pathPrefix: location.prefix });
  Object.defineProperty(result.findings, "coverage", { value: result.coverage, enumerable: false });
  Object.defineProperty(result.findings, "metadata", { value: { ...scanMetadata(appRoot, manifest, result.coverage, result.contentDigests, repositoryIdentity, location.commit), pathBase: location.pathBase, pathPrefix: location.prefix, provenance: result.provenance, ...Object.fromEntries(Object.entries(authority).filter(([key]) => key !== "trust")) }, enumerable: false });
  return result.findings;
}

function scanMetadata(root, manifest, coverage = [], contentDigests = [], knownIdentity, commit = null) {
  const fullCoverage = [...coverage];
  let repositoryIdentity = null;
  if (knownIdentity !== undefined) repositoryIdentity = knownIdentity;
  else repositoryIdentity = repositoryIdentityFor(root);
  return { repositoryIdentity, commit, host: `${os.hostname()}/${process.platform}`, scannedAt: new Date().toISOString(), catalogDigest: catalogDigest(manifest), catalogVersion: manifest.version ?? String(manifest.schema ?? "1"), scannerVersion: SCANNER_VERSION, contentDigests, coverage: fullCoverage };
}

function globToRegExp(glob) {
  let out = ""; for (let i = 0; i < glob.length; i++) { const c = glob[i]; if (c === "*" && glob[i + 1] === "*") { out += ".*"; i++; if (glob[i + 1] === "/") i++; } else if (c === "*") out += "[^/]*"; else if (c === "?") out += "[^/]"; else out += c.replace(/[.+^${}()|[\]\\]/g, "\\$&"); }
  return new RegExp(`^${out}$`);
}
export function loadExceptions(root) { const file = path.join(root, EXCEPTIONS_FILE); if (!existsSync(file)) return []; return (JSON.parse(readFileSync(file, "utf8")).exceptions ?? []).map((e, index) => ({ ...e, index })); }

function enrollmentPending(trust, state) {
  if (state?.pending !== undefined) return state.pending;
  if (state?.enrolled !== undefined) return !state.enrolled;
  if (trust?.rootFingerprint) return !Array.isArray(trust.ownershipKeys) || trust.ownershipKeys.length === 0;
  const keys = [trust?.hosts?.windows, trust?.hosts?.macos];
  return !keys.every((key) => key?.keyId && key.publicKeyPem && !trust.revokedKeyIds?.includes(key.keyId)) || keys[0]?.keyId === keys[1]?.keyId || keys[0]?.publicKeyPem === keys[1]?.publicKeyPem;
}

export function evaluate(findings, exceptions, options = {}) {
  const today = options.today ?? new Date().toISOString().slice(0, 10), problems = [], covered = [], violations = [], warnings = [];
  const metadata = options.metadata ?? findings.metadata ?? {};
  const catalogDigest = metadata.catalogDigest ?? options.catalogDigest ?? "";
  const identity = options.repositoryIdentity ?? metadata.repositoryIdentity ?? "";
  for (const e of exceptions) {
    const p = e.payload ?? e;
    const where = `exception #${e.index} (${p.fingerprint ?? "?"})`;
    const missing = ["fingerprint", "paths", "reason", "owner", "expires", "requestId", "occurrenceHashes"].filter((k) => !p[k] || (Array.isArray(p[k]) && !p[k].length));
    if (missing.length) { problems.push(`${where}: missing ${missing.join(", ")}`); continue; }
    const approvedBy = e.approvedBy ?? p.approvedBy;
    if (approvedBy !== undefined && approvedBy !== "Adrian") { problems.push(`${where}: approvedBy must be Adrian`); continue; }
    if (!/^\d{4}-\d{2}-\d{2}$/.test(p.expires) || Number.isNaN(Date.parse(`${p.expires}T00:00:00Z`)) || p.expires < today) { problems.push(`${where}: invalid or expired expires date`); continue; }
    if (!p.trackingRef || typeof p.trackingRef !== "object" || !p.trackingRef.verified || !/^https:\/\//.test(p.trackingRef.url ?? "") || !p.trackingRef.id) { problems.push(`${where}: tracking must be a verified RightKit request reference`); continue; }
    if (p.paths.some((item) => !item || item.includes("\\") || item.startsWith("/") || item.split("/").includes("..") || /[*?\[\]{}]/.test(item))) { problems.push(`${where}: exception paths must be exact repository-relative paths`); continue; }
    const verification = verifyException(e, { repositoryIdentity: identity, catalogDigest, findings, trust: options.trust, today, scannerVersion: options.scannerVersion ?? metadata.scannerVersion ?? SCANNER_VERSION, catalogVersion: options.catalogVersion ?? metadata.catalogVersion });
    if (!verification?.ok) { problems.push(`${where}: signature verification failed${verification?.problems?.length ? ` (${verification.problems.join("; ")})` : ""}`); continue; }
    const matching = findings.filter((f) => f.fingerprint === p.fingerprint && p.paths.includes(f.file) && p.occurrenceHashes.includes(occurrenceHash(f)));
    if (!matching.length) { problems.push(`${where}: stale, matches no signed occurrence`); continue; }
    for (const f of matching) covered.push({ ...f, occurrenceHash: occurrenceHash(f), exception: e });
  }
  const pending = enrollmentPending(options.trust, options.trustEnrollment);
  if (pending) warnings.push("approval enrollment pending; detected findings remain open backlog requests until distinct Windows & macOS keys are enrolled");
  for (const f of findings) if (!covered.some((x) => x.occurrenceHash === occurrenceHash(f))) violations.push({ ...f, occurrenceHash: occurrenceHash(f) });
  return { violations, covered, problems, warnings, enrollmentPending: pending };
}

export function capabilityTable(manifest, deps, result) {
  return [...new Map(manifest.capabilities.map((c) => [c.capability, c.package])).entries()].map(([capability, owner]) => {
    const declared = deps.some((d) => d.name === owner || d.name === owner.replace(/^rightkit-/, "@rightkit/"));
    const own = result.violations.filter((x) => x.capability === capability), covered = result.covered.filter((x) => x.capability === capability);
    const status = own.length ? "blocked" : covered.length ? "excepted" : declared ? "declared-dependency" : result.coverage?.length ? "unscanned" : "not-detected";
    return { capability, owner, uses: declared, status, count: own.length + covered.length };
  });
}
export function formatTable(rows) { return ["capability  owner  status", ...rows.map((x) => `${x.capability}  ${x.owner}  ${x.status}`)].join("\n"); }

export function checkApp(appRoot, manifest, options = {}) {
  let trustState = options.trust;
  if (!trustState) trustState = loadTrustedExternal();
  const initialTrust = trustState && Object.prototype.hasOwnProperty.call(trustState, "trust") ? trustState.trust : trustState;
  const repositoryLocation = scanRepositoryRoot(appRoot);
  const repositoryIdentity = repositoryIdentityFor(appRoot);
  const resolved = initialTrust?.keyAuthority ? resolveSharedAuthority(initialTrust) : resolveRepositoryKeyLog(initialTrust, repositoryLocation.root, repositoryIdentity);
  const trust = resolved.trust;
  const location = repositoryLocation;
  const scanned = scanInternal(appRoot, manifest, { inventory: fileInventory(appRoot, trust, location.prefix, location.root), trust, repositoryIdentity, repositoryRoot: location.root, pathPrefix: location.prefix });
  const metadata = { ...scanMetadata(appRoot, manifest, scanned.coverage, scanned.contentDigests, repositoryIdentity, location.commit), pathBase: location.pathBase, pathPrefix: location.prefix, provenance: scanned.provenance, ...Object.fromEntries(Object.entries(resolved).filter(([key]) => key !== "trust")) };
  if (trust?.rootFingerprint && Array.isArray(trust.ownershipKeys) && !trust.ownershipKeys.length) throw new Error("protected ownership key log has no active approval keys");
  const enrollment = trust?.rootFingerprint
    ? { enrolled: trust.enrollmentComplete === true, pending: trust.enrollmentComplete !== true }
    : trustState?.pending !== undefined || trustState?.enrolled !== undefined ? trustState : undefined;
  const result = evaluate(scanned.findings, loadExceptions(appRoot), { ...options, trust, trustEnrollment: enrollment, metadata });
  const coverage = [...new Map(metadata.coverage.map((x) => [`${x.path}\0${x.reason}`, x])).values()];
  const rows = capabilityTable(manifest, scanned.deps, { ...result, coverage });
  return { ok: result.enrollmentPending || (!result.violations.length && !result.problems.length && !coverage.length), rows, ...result, findings: scanned.findings, coverage, metadata };
}

function checkCommitted(appRoot, manifest, options, acquire) {
  let trustState = options.trust;
  if (!trustState) trustState = loadTrustedExternal();
  let trust = trustState && Object.prototype.hasOwnProperty.call(trustState, "trust") ? trustState.trust : trustState;
  const requiredPaths = [...(trust?.provenance ?? []).flatMap((record) => record.paths ?? []), ...(trust?.rootFingerprint && !trust?.keyAuthority ? [".rightkit/ownership-keys.json"] : [])];
  const sourceHandle = options.source ?? acquire(appRoot, { expectedCommit: options.expectedCommit, expectedTree: options.expectedTree, requiredPaths });
  const committed = committedSourceDataForInternalUse(sourceHandle);
  if (!committed) throw new Error("committed ownership: supplied source handle was not acquired by trusted Git reader");
  if ((options.expectedCommit && committed.commit !== options.expectedCommit) || (options.expectedTree && committed.tree !== options.expectedTree)) throw new Error("committed ownership: source handle does not match expected commit/tree");
  // Compare real paths: Git reports the resolved root (macOS /private/var/...), while callers
  // may pass a symlinked spelling of the same directory (/var/..., /tmp/...).
  const real = (p) => { try { return realpathSync(p); } catch { return path.resolve(p); } };
  const relativeRoot = path.relative(real(committed.repositoryRoot), real(appRoot));
  if (relativeRoot === ".." || relativeRoot.startsWith(`..${path.sep}`) || path.isAbsolute(relativeRoot)) throw new Error("committed ownership: scan root is outside committed repository");
  const pathPrefix = relativeRoot ? `${relativeRoot.split(path.sep).join("/")}/` : "";
  const repositoryIdentity = committed.origin ? canonicalRepositoryIdentity(committed.origin) : null;
  if (!repositoryIdentity) throw new Error("committed ownership: repository origin is missing or invalid");
  let keyLogDigest, keyCheckpoint, keyAuthorityBinding;
  if (trust?.keyAuthority) {
    const authority = readCommittedKeyAuthority(trust);
    if (!authority.ok) throw new Error(`committed ownership key authority failed: ${authority.problems.join("; ")}`);
    trust = authority.resolvedTrust;
    if (!trust.ownershipKeys.length) throw new Error("protected ownership key authority has no active approval keys");
    keyLogDigest = authority.digest;
    keyCheckpoint = authority.keyCheckpoint;
    keyAuthorityBinding = authority.authority;
  } else if (trust?.rootFingerprint) {
    const keyLog = committed.entries.find((entry) => entry.rel === ".rightkit/ownership-keys.json");
    const resolved = resolveKeyLogTrust(trust, keyLog?.bytes);
    if (!resolved.ok) throw new Error(`committed ownership key log failed: ${resolved.problems.join("; ")}`);
    trust = resolved.trust;
    if (!trust.ownershipKeys.length) throw new Error("protected ownership key log has no active approval keys");
    keyLogDigest = resolved.keyLogDigest;
    keyCheckpoint = resolved.checkpoint;
  }
  const location = { root: committed.repositoryRoot, prefix: pathPrefix, pathBase: "repository" };
  const inventory = committedFileInventory(appRoot, trust, location.prefix, committed);
  if (location.prefix && !committed.entries.some((entry) => entry.rel.startsWith(location.prefix))) {
    inventory.coverage.push({ path: ".", status: "unscanned", reason: "registered-root-missing-from-committed-tree" });
  }
  const scanned = scanInternal(appRoot, manifest, { inventory, trust, repositoryIdentity, repositoryRoot: location.root, pathPrefix: location.prefix });
  const exceptionPath = [location.prefix.replace(/\/$/, ""), EXCEPTIONS_FILE.split(path.sep).join("/")].filter(Boolean).join("/");
  const exceptionObject = committed.entries.find((entry) => entry.rel === exceptionPath);
  const exceptionEntry = exceptionObject?.bytes;
  let exceptions = [];
  if (exceptionEntry && exceptionObject.size <= 2 * 1024 * 1024) {
    try { exceptions = (JSON.parse(exceptionEntry.toString("utf8")).exceptions ?? []).map((entry, index) => ({ ...entry, index })); }
    catch { scanned.coverage.push({ path: EXCEPTIONS_FILE.split(path.sep).join("/"), status: "unscanned", reason: "invalid-committed-exceptions-json" }); }
  }
  const exceptionDigest = exceptionEntry ? byteDigest(exceptionEntry) : exceptionObject ? sha256({ unavailableBlob: exceptionObject.oid, size: exceptionObject.size }) : sha256([]);
  const metadata = {
    repositoryIdentity, commit: committed.commit, tree: committed.tree, host: `${os.hostname()}/${process.platform}`,
    scannedAt: new Date().toISOString(), catalogDigest: catalogDigest(manifest), catalogVersion: manifest.version ?? String(manifest.schema ?? "1"),
    scannerVersion: SCANNER_VERSION, contentDigests: scanned.contentDigests, coverage: scanned.coverage,
    pathBase: location.pathBase, pathPrefix: location.prefix, provenance: scanned.provenance, exceptionDigest,
    source: "committed-git-tree", ...(keyLogDigest ? { keyLogDigest, keyCheckpoint, rootFingerprint: trust.rootFingerprint, ...(keyAuthorityBinding ? { keyAuthority: keyAuthorityBinding } : {}) } : {}),
  };
  const result = evaluate(scanned.findings, exceptions, { ...options, trust, trustEnrollment: trust?.rootFingerprint
    ? { enrolled: trust.enrollmentComplete === true, pending: trust.enrollmentComplete !== true }
    : trustState?.pending !== undefined || trustState?.enrolled !== undefined ? trustState : undefined, metadata });
  const coverage = [...new Map(metadata.coverage.map((x) => [`${x.path}\0${x.reason}`, x])).values()];
  const rows = capabilityTable(manifest, scanned.deps, { ...result, coverage });
  return { ok: !result.enrollmentPending && !result.violations.length && !result.problems.length && !coverage.length, rows, ...result, findings: scanned.findings, coverage, metadata };
}

/** Protected production scan: acquire and inspect exact HEAD blobs without reading checkout contents. */
export function checkCommittedApp(appRoot, manifest, options = {}) {
  return checkCommitted(appRoot, manifest, options, acquireCommittedSource);
}

/** Explicit test-only seam for bounded Git process failures and fixtures. */
export function checkCommittedAppForTest(appRoot, manifest, options = {}) {
  return checkCommitted(appRoot, manifest, options, options.acquireCommittedSourceForTest ?? acquireCommittedSource);
}
