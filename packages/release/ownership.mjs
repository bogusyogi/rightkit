// RightKit ownership scan. Public exports retain the original findings-array API.
import { isBlockingFinding } from "./ownership-verdict.mjs";
import { spawnSync } from "./background-process.mjs";
import { canonicalRepositoryIdentity, loadTrustedExternal, occurrenceHash, policyEntryDigest, resolveKeyLogTrust, sha256, verifyException, verifyProtectedProvenance } from "./ownership-approval.mjs";
import installedManifest from "./rightkit-ownership.json" with { type: "json" };
import { parse as parseToml } from "smol-toml";
import { sourceNonCode } from "./ownership-source.mjs";
import { OWNERSHIP_LOCKFILES, transitiveDependencies } from "./ownership-lockfiles.mjs";
import { ALLOWLIST_FILE, ALLOWLIST_FINDING_ID, evaluateDependency, sourceKind, validateAllowlist } from "./ownership-dependency-allowlist.mjs";
import { fileURLToPath } from "node:url";
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
export const SCANNER_VERSION = "2.6.0";
export const catalogDigest = (manifest) => sha256(manifest);
const byteDigest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const inputDigest = (file) => file.digest ?? byteDigest(file.bytes);
const sourceCache = new Map();
const catalogCache = new Map();

function sourceMatchers(manifest) {
  const digest = catalogDigest(manifest);
  if (catalogCache.has(digest)) return catalogCache.get(digest);
  const sources = manifest.capabilities.flatMap((c) => c.fingerprints.filter((f) => f.type === "source").map((f) => ({ c, f, re: new RegExp(f.pattern, f.codeOnly ? `${(f.flags ?? "").replace(/[gy]/g, "")}g` : f.flags ?? "") })));
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
  const ext = path.posix.extname(file.rel).toLowerCase();
  const key = `${matchers.digest}:${ext}:${file.cacheKey ?? inputDigest(file)}`;
  if (sourceCache.has(key)) return sourceCache.get(key);
  const text = file.bytes.toString("utf8"), findings = [];
  const ordinals = new Map();
  const push = (c, f, line, lineText) => {
    const identity = `${f.id}\0${lineText}`, ordinal = ordinals.get(identity) ?? 0;
    ordinals.set(identity, ordinal + 1);
    findings.push({ fingerprint: f.id, capability: c.capability, package: c.package, ...findingPolicy(c, f), kind: "source", line, lineText, ordinal, text: lineText.trim().slice(0, 160) });
  };
  if (matchers.prefilter.test(text)) {
    const lines = text.split(/\r?\n/);
    const applicable = matchers.sources.filter(({ f }) => !f.extensions || f.extensions.includes(ext));
    const lineMatchers = applicable.filter(({ f }) => !f.codeOnly);
    for (const [i, line] of lines.entries()) if (!commentOnly(line)) for (const { c, f, re } of lineMatchers) {
      re.lastIndex = 0;
      if (re.test(line)) push(c, f, i + 1, line);
    }
    const codeMatchers = applicable.filter(({ f }) => f.codeOnly);
    if (codeMatchers.length) {
      const nonCode = sourceNonCode(text, ext === ".rs");
      for (const { c, f, re } of codeMatchers) {
        re.lastIndex = 0;
        const reported = new Set();
        let line = 1, offset = 0;
        for (const match of text.matchAll(re)) {
          if (!match[0] || nonCode[match.index]) continue;
          for (; offset < match.index; offset++) if (text[offset] === "\n") line++;
          if (reported.has(line)) continue;
          reported.add(line);
          push(c, f, line, lines[line - 1]);
        }
      }
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
      let file = path.join(child, "ownership-declaration.json");
      if (!existsSync(file)) file = path.join(child, MANIFEST_FILE);
      if (!existsSync(file) && group === "swift") file = path.join(child, "rightkit-ownership.json");
      if (!existsSync(file)) continue;
      const declared = JSON.parse(readFileSync(file, "utf8"));
      if (declared.schema !== undefined) continue;
      for (const capability of declared.capabilities ?? []) entries.push({ package: declared.package, ...capability });
    }
  }
  const internalFile = path.join(rightkitRoot, "packages/release/ownership-internal-owners.json");
  const internalOwners = existsSync(internalFile) ? JSON.parse(readFileSync(internalFile, "utf8")).owners : [];
  const manifest = { schema: 1, version: "2", capabilities: entries, internalOwners };
  validateManifest(manifest);
  return manifest;
}

export function validateManifest(manifest) {
  const ids = new Set();
  for (const c of manifest.capabilities ?? []) {
    if (!c.package || !c.capability || !Array.isArray(c.fingerprints)) throw new Error(`ownership: incomplete capability ${JSON.stringify(c).slice(0, 120)}`);
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
  const addTable = (table, prefix = "") => {
    if (!table || typeof table !== "object") return;
    for (const section of ["dependencies", "dev-dependencies", "build-dependencies"]) for (const [alias, raw] of Object.entries(table[section] ?? {})) {
      const spec = raw && typeof raw === "object" ? raw : {};
      const inheritedRaw = spec.workspace === true ? workspace.deps?.[alias] : undefined;
      if (spec.workspace === true && inheritedRaw === undefined) throw new Error(`missing workspace dependency: ${alias}`);
      const inherited = typeof inheritedRaw === "string" ? { version: inheritedRaw } : inheritedRaw ?? {};
      const name = spec.package ?? inherited.package ?? alias;
      const pathValue = spec.path ?? inherited.path, git = spec.git ?? inherited.git, selector = spec.rev ?? inherited.rev ?? spec.branch ?? inherited.branch ?? spec.tag ?? inherited.tag;
      const source = pathValue ? `path:${pathValue}` : git ? `git+${git}${selector ? `#${selector}` : ""}` : "registry";
      const requirement = typeof raw === "string" ? raw : spec.version ?? inherited.version ?? null;
      if (spec.features !== undefined && !Array.isArray(spec.features) || inherited.features !== undefined && !Array.isArray(inherited.features)) throw new Error(`malformed dependency features: ${alias}`);
      const features = [...new Set([...(inherited.features ?? []), ...(spec.features ?? [])])].sort();
      if (!raw || typeof raw !== "string" && (typeof raw !== "object" || Array.isArray(raw)) || features.some((x) => typeof x !== "string") || requirement !== null && typeof requirement !== "string") throw new Error(`malformed dependency declaration: ${alias}`);
      addDependency(out, "cargo", name, file, { alias: name === alias ? null : alias, section: `${prefix}${section}`, requirement, features, defaultFeatures: spec["default-features"] ?? inherited["default-features"] ?? null, source, resolvedVersion: null, local: !!pathValue || !!git });
      if (pathValue) pathSpec(pathValue, spec.path ? file : path.posix.join(workspace.dir ?? ".", "Cargo.toml"), file);
      if (git) coverage.push({ path: file, status: "unscanned", reason: `git-dependency-source:${name}` });
    }
    for (const [target, value] of Object.entries(table.target ?? {})) addTable(value, `${prefix}target.'${target}'.`);
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
    relevant: (rel) => ["Cargo.toml", "package.json", "Package.swift", "Package.resolved", ...OWNERSHIP_LOCKFILES].includes(path.posix.basename(rel)) || CODE_EXT.has(path.posix.extname(rel).toLowerCase()),
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
    const relevant = protectedPaths.has(entry.rel) || ["Cargo.toml", "package.json", "Package.swift", "Package.resolved", "ownership-exceptions.json", ...OWNERSHIP_LOCKFILES].includes(path.posix.basename(rel)) || CODE_EXT.has(path.posix.extname(rel).toLowerCase());
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

// Blank out Swift `//` and nested `/* */` comments outside string literals, keeping offsets.
export function stripSwiftComments(text) {
  const out = text.split("");
  let quote = false, depth = 0;
  for (let i = 0; i < text.length; i++) {
    if (depth) {
      if (text.startsWith("/*", i)) { depth++; out[i] = out[i + 1] = " "; i++; continue; }
      if (text.startsWith("*/", i)) { depth--; out[i] = out[i + 1] = " "; i++; continue; }
      if (text[i] !== "\n") out[i] = " ";
      continue;
    }
    if (quote) {
      if (text[i] === "\\") { i++; continue; }
      if (text[i] === '"' || text[i] === "\n") quote = false;
      continue;
    }
    if (text[i] === '"') { quote = true; continue; }
    if (text.startsWith("//", i)) { for (; i < text.length && text[i] !== "\n"; i++) out[i] = " "; continue; }
    if (text.startsWith("/*", i)) { depth = 1; out[i] = out[i + 1] = " "; i++; }
  }
  return out.join("");
}

function swiftPackageArguments(text) {
  text = stripSwiftComments(text);
  const args = [];
  for (const match of text.matchAll(/\.package\s*\(/g)) {
    let at = match.index + match[0].length, start = at, depth = 1, quote = false;
    for (; at < text.length && depth; at++) {
      if (text[at] === "\\" && quote) { at++; continue; }
      if (text[at] === '"') quote = !quote;
      if (!quote && text[at] === "(") depth++;
      if (!quote && text[at] === ")") depth--;
    }
    if (depth) throw new Error("unterminated Swift package declaration");
    args.push(text.slice(start, at - 1));
  }
  return args;
}
function resolveDependencyVersions(appRoot, inventory, deps, coverage, contentDigests) {
  const cache = new Map();
  const adjacent = (file, name) => {
    const rel = path.posix.join(path.posix.dirname(file), name);
    if (cache.has(rel)) return cache.get(rel);
    let bytes;
    if (inventory.committed) {
      const item = inventory.snapshotFiles.get(`${inventory.pathPrefix ?? ""}${rel}`);
      if (item && !item.bytes) throw new Error(`lockfile bytes unavailable: ${rel}`);
      bytes = item?.bytes;
    } else {
      const inventoried = OWNERSHIP_LOCKFILES.includes(name) ? inventory.files.find((item) => item.rel === rel) : undefined;
      if (inventoried && !inventoried.bytes) throw new Error(`lockfile bytes unavailable: ${rel}`);
      const absolute = path.resolve(appRoot, ...rel.split("/"));
      if (inventoried) bytes = inventoried.bytes;
      else if (existsSync(absolute)) {
        const root = realpathSync(appRoot), real = realpathSync(absolute);
        if (!real.startsWith(`${root}${path.sep}`)) throw new Error(`lockfile outside scan root: ${rel}`);
        if (!lstatSync(absolute).isFile() || lstatSync(absolute).size > 64 * 1024 * 1024) throw new Error(`lockfile unsupported or too large: ${rel}`);
        bytes = readFileSync(absolute);
      }
    }
    const text = bytes?.toString("utf8");
    cache.set(rel, text);
    if (bytes) contentDigests.push({ path: rel, digest: byteDigest(bytes) });
    return text;
  };
  for (const d of deps) {
    try {
      if (d.ecosystem === "cargo") {
        const text = adjacent(d.file, "Cargo.lock");
        if (text !== undefined) {
          const lock = parseToml(text);
          if (!Array.isArray(lock.package)) throw new Error("Cargo.lock package list missing");
          let candidates = lock.package.filter((p) => p.name === d.name && (d.source === "registry" ? String(p.source ?? "").startsWith("registry+") : d.source.startsWith("git+") ? String(p.source ?? "").split("#")[0].split("?")[0] === d.source.split("#")[0] : !p.source));
          if (candidates.length > 1) {
            const manifestText = inventory.files.find((f) => f.rel === d.file)?.bytes?.toString("utf8");
            const owner = manifestText && parseToml(manifestText).package?.name;
            const refs = lock.package.filter((p) => p.name === owner).flatMap((p) => p.dependencies ?? []);
            candidates = candidates.filter((p) => refs.some((ref) => ref === `${d.name} ${p.version}` || ref.startsWith(`${d.name} ${p.version} (`)));
          }
          if (candidates.length !== 1 || typeof candidates[0].version !== "string") throw new Error(`Cargo.lock resolution missing or ambiguous: ${d.name}`);
          d.resolvedVersion = candidates[0].version;
        }
      } else if (d.ecosystem === "npm") {
        const text = adjacent(d.file, "npm-shrinkwrap.json") ?? adjacent(d.file, "package-lock.json");
        if (text !== undefined) {
          const lock = JSON.parse(text), name = d.alias ?? d.name;
          const item = lock.packages?.[`node_modules/${name}`] ?? lock.dependencies?.[name];
          if (typeof item?.version !== "string") throw new Error(`npm lock resolution missing: ${name}`);
          d.resolvedVersion = item.version;
        } else {
          const pnpm = adjacent(d.file, "pnpm-lock.yaml"), yarn = pnpm === undefined ? adjacent(d.file, "yarn.lock") : undefined;
          if (pnpm !== undefined) {
            let section, name, dependencyIndent, inRoot = !/^importers:/m.test(pnpm), rootIndent;
            for (const line of pnpm.split(/\r?\n/)) {
              const entry = /^(\s*)(?:'([^']+)'|"([^"]+)"|([^:]+)):\s*(.*?)\s*$/.exec(line);
              if (!entry) continue;
              const indent = entry[1].length, key = entry[2] ?? entry[3] ?? entry[4], value = entry[5];
              if (key === "." && indent === 2) { inRoot = true; rootIndent = 2; continue; }
              if (rootIndent !== undefined && indent <= rootIndent) inRoot = false;
              if (!inRoot) continue;
              if (["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"].includes(key)) { section = key; dependencyIndent = indent + 2; name = undefined; continue; }
              if (indent === dependencyIndent) { name = key; if (name === (d.alias ?? d.name) && section === d.section && value) d.resolvedVersion = value.replace(/^['"]|['"]$/g, "").split("(")[0]; }
              if (key === "version" && name === (d.alias ?? d.name) && section === d.section && indent === dependencyIndent + 2) d.resolvedVersion = value.replace(/^['"]|['"]$/g, "").split("(")[0].replace(/^.*@(?=\d)/, "");
            }
            if (!d.resolvedVersion) throw new Error(`pnpm lock resolution missing: ${d.alias ?? d.name}`);
          } else if (yarn !== undefined) {
            let selected = false;
            for (const line of yarn.split(/\r?\n/)) {
              if (/^\S/.test(line) && /:$/.test(line)) selected = line.includes(`${d.alias ?? d.name}@${d.requirement}`) || line.includes(`${d.alias ?? d.name}@npm:${d.requirement}`);
              const version = /^\s+version(?::|\s)\s*["']?([^\s"']+)/.exec(line);
              if (selected && version) d.resolvedVersion = version[1];
            }
            if (!d.resolvedVersion) throw new Error(`yarn lock resolution missing: ${d.alias ?? d.name}`);
          }
        }
      } else if (d.ecosystem === "swiftpm") {
        const text = adjacent(d.file, "Package.resolved");
        if (text !== undefined && !d.local) {
          const lock = JSON.parse(text), pins = lock.pins ?? lock.object?.pins;
          if (!Array.isArray(pins)) throw new Error("Swift lock pins missing");
          const pin = pins.filter((p) => (p.identity ?? p.package)?.toLowerCase() === d.name.toLowerCase() || p.location === d.source.slice(4).split("#")[0] || p.repositoryURL === d.source.slice(4).split("#")[0]);
          if (pin.length !== 1) throw new Error(`Swift lock resolution missing or ambiguous: ${d.name}`);
          d.resolvedVersion = pin[0].state?.version ?? null;
          if (d.resolvedVersion !== null && typeof d.resolvedVersion !== "string") throw new Error(`invalid Swift resolved version: ${d.name}`);
        }
      }
    } catch (error) { coverage.push({ path: d.file, status: "unscanned", reason: `dependency-lock-resolution:${error.message}` }); }
  }
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
        if (typeof spec !== "string") { coverage.push({ path: rel, status: "unscanned", reason: `invalid-npm-dependency:${name}` }); continue; }
        const alias = spec.match(/^(?:npm:)(@[^/]+\/[^@]+|[^@]+)(?:@|$)/)?.[1];
        const localPath = spec.match(/^(?:file:|link:)(.+)$/)?.[1];
        const isGit = /^(git\+|https?:.*\.git)/i.test(spec);
        const devOnly = field === "devDependencies" && !["dependencies", "optionalDependencies", "peerDependencies"].some((section) => Object.hasOwn(pkg[section] ?? {}, name));
        addDependency(deps, "npm", alias ?? name, rel, { alias: alias ? name : null, section: field, requirement: spec, features: [], defaultFeatures: null, resolvedVersion: null, local: !!localPath, source: localPath ? `path:${localPath}` : isGit ? spec.startsWith("git+") ? spec : `git+${spec}` : spec.startsWith("workspace:") ? `path:${spec}` : "registry", ...(devOnly ? { devOnly: true } : {}) });
        if (localPath) {
          resolveLocalPath(appRoot, rel, localPath, coverage, rel, inventory, "local-npm-dependency", name);
        } else if (isGit) coverage.push({ path: rel, status: "unscanned", reason: `git-npm-dependency-source:${name}` });
      }
    } else {
      for (const arg of swiftPackageArguments(text)) {
        const url = arg.match(/url:\s*["']([^"']+)["']/)?.[1], pathDep = arg.match(/path:\s*["']([^"']+)["']/)?.[1], registry = arg.match(/id:\s*["']([^"']+)["']/)?.[1];
        const explicitName = arg.match(/name:\s*["']([^"']+)["']/)?.[1];
        const id = explicitName ?? registry ?? url?.replace(/\.git$/i, "").split("/").pop()?.toLowerCase() ?? pathDep?.split(/[\\/]/).pop();
        if (!id || !url && !pathDep && !registry) throw new Error("unsupported Swift package identity or source");
        const selector = arg.match(/(?:branch|revision):\s*["']([^"']+)["']/)?.[1];
        const requirement = arg.replace(/(?:name|url|path|id):\s*"(?:\\.|[^"\\])*"\s*,?\s*/g, "").trim() || null;
        addDependency(deps, "swiftpm", id, rel, { alias: null, section: "dependencies", requirement, features: [], defaultFeatures: null, resolvedVersion: null, local: !!pathDep, source: pathDep ? `path:${pathDep}` : registry ? "registry" : `git+${url}${selector ? `#${selector}` : ""}` });
        if (pathDep) {
          resolveLocalPath(appRoot, rel, pathDep, coverage, rel, inventory, "local-swift-dependency", id);
        }
      }
    }
  }
  resolveDependencyVersions(appRoot, inventory, deps, coverage, contentDigests);
  Object.defineProperty(deps, "coverage", { value: coverage, enumerable: false });
  Object.defineProperty(deps, "contentDigests", { value: contentDigests, enumerable: false });
  Object.defineProperty(deps, "manifests", { value: files, enumerable: false });
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
  // Git prints the resolved root (macOS /private/var/...); resolve the app root the same way so a
  // symlinked spelling (/var, /tmp) is not mistaken for a path outside the repository.
  const real = (p) => { try { return realpathSync(p); } catch { return path.resolve(p); } };
  const root = real(rootPath), app = real(appRoot);
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
      const rootReal = realpathSync(repositoryRoot);
      const filePath = path.resolve(rootReal, ...rel.split("/"));
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

function scanInternal(appRoot, manifest, { inventory = fileInventory(appRoot), trust, repositoryIdentity, repositoryRoot = appRoot, pathPrefix = "", allowlistPolicy = null } = {}) {
  const findings = [], coverage = [...inventory.coverage], deps = dependenciesFromInventory(appRoot, inventory), contentDigests = [...(deps.contentDigests ?? [])];
  const catalogOwners = new Set(manifest.capabilities.map((entry) => entry.package));
  coverage.push(...(deps.coverage ?? []));
  const provenance = trustedProvenance(appRoot, inventory, trust, repositoryIdentity, coverage, repositoryRoot, pathPrefix);
  const pushDependency = (c, f, d, kind = "dependency") => findings.push({ fingerprint: f.id, capability: c.capability, package: c.package, ...findingPolicy(c, f), file: d.file, line: 0, kind, ecosystem: d.ecosystem, name: d.name,
    ...(kind === "dependency" ? { alias: d.alias, section: d.section, requirement: d.requirement, features: d.features, defaultFeatures: d.defaultFeatures, local: d.local } : { lockfile: d.lockfile, dependencyPath: d.dependencyPath }),
    source: d.source, resolvedVersion: d.resolvedVersion, text: `${d.ecosystem} dependency ${d.alias ? `${d.alias} aliases ` : ""}${d.name}`, provenance: d.source });
  for (const c of manifest.capabilities) for (const f of c.fingerprints.filter((x) => x.type === "dependency")) for (const d of deps) {
    if (d.ecosystem === f.ecosystem && (d.name === f.name || d.alias === f.name) && !catalogOwners.has(d.name)) {
      pushDependency(c, f, d);
    }
  }
  if (allowlistPolicy && repositoryIdentity) {
    // Default-deny: a direct dependency that is not a RightKit package, not a first-party path dependency, and
    // not in the repository baseline or global list is a finding. Block-tier catalog dependencies are reported by
    // their own (always blocking) fingerprints above, so allowlisting can never relax them.
    const owners = [...catalogOwners, ...(manifest.internalOwners ?? []).map((owner) => typeof owner === "string" ? owner : owner.package)];
    const blocked = new Set(manifest.capabilities.flatMap((c) => c.fingerprints.filter((f) => f.type === "dependency" && findingPolicy(c, f).tier === "block").map((f) => `${f.ecosystem}:${f.name}`)));
    for (const d of deps) {
      if (blocked.has(`${d.ecosystem}:${d.name}`) || evaluateDependency(allowlistPolicy.allowlist, repositoryIdentity, { ecosystem: d.ecosystem, name: d.name, kind: sourceKind(d.source) }, { owners }).allowed) continue;
      findings.push({ fingerprint: ALLOWLIST_FINDING_ID, capability: "dependency-allowlist", package: "RightKit dependency allowlist", tier: allowlistPolicy.tier, file: d.file, line: 0, kind: "dependency", ecosystem: d.ecosystem, name: d.name,
        alias: d.alias, section: d.section, requirement: d.requirement, features: d.features, defaultFeatures: d.defaultFeatures, local: d.local, source: d.source, resolvedVersion: d.resolvedVersion,
        text: `${d.ecosystem} dependency ${d.name} is not in ${ALLOWLIST_FILE}; propose it there and Adrian activates it via trust-sync`, provenance: d.source });
    }
  }
  const blockRules = manifest.capabilities.flatMap((c) => c.fingerprints.filter((f) => f.type === "dependency" && findingPolicy(c, f).tier === "block").map((f) => ({ c, f })));
  const transitiveOwners = new Set([...catalogOwners, ...(manifest.internalOwners ?? []).map((owner) => typeof owner === "string" ? owner : owner.package)]);
  const transitive = transitiveDependencies(appRoot, inventory, deps.manifests, transitiveOwners, coverage, contentDigests, blockRules.map(({ f }) => `${f.ecosystem}:${f.name}`));
  for (const { c, f } of blockRules) for (const d of transitive) if (d.ecosystem === f.ecosystem && d.name === f.name) pushDependency(c, f, d, "transitive-dependency");
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
    for (const f of findings) { f.file = prefix(f.file); if (f.lockfile) f.lockfile = prefix(f.lockfile); }
    for (const c of coverage) c.path = prefix(c.path);
    for (const d of deps) d.file = prefix(d.file);
    for (const d of contentDigests) d.path = prefix(d.path);
  }
  for (const f of findings) f.occurrenceHash = occurrenceHash(f);
  contentDigests.sort((a, b) => a.path.localeCompare(b.path));
  return { findings, coverage, deps, contentDigests, provenance: provenance.classifications };
}

const shippedAllowlistFile = path.join(path.dirname(fileURLToPath(import.meta.url)), ALLOWLIST_FILE);
function shippedAllowlist() {
  try { const parsed = JSON.parse(readFileSync(shippedAllowlistFile, "utf8")); return validateAllowlist(parsed).ok ? parsed : null; } catch { return null; }
}
/**
 * Which allowlist applies and how hard. The protected trust copy (activated by trust-sync) governs every repository
 * registered in trust and blocks. Before trust-sync, or while approval enrollment is pending, the shipped proposal
 * is advisory and only for repositories it has a baseline for.
 */
export function dependencyAllowlistPolicy(trust, repositoryIdentity, { pending = false, shipped } = {}) {
  if (!repositoryIdentity) return null;
  const trusted = trust?.dependencyAllowlist && validateAllowlist(trust.dependencyAllowlist).ok ? trust.dependencyAllowlist : null;
  if (trusted && trust.repositories?.some((entry) => entry.repositoryIdentity === repositoryIdentity)) return { allowlist: trusted, tier: pending ? "advise" : "block", source: "protected-trust" };
  const proposal = shipped === undefined ? shippedAllowlist() : shipped;
  if (proposal?.repositories?.some((entry) => entry.repositoryIdentity === repositoryIdentity)) return { allowlist: proposal, tier: "advise", source: "shipped-proposal" };
  return null;
}

function trustForScan(options = {}) {
  if (options.trust !== undefined) return options.trust?.trust ?? options.trust;
  try { return loadTrustedExternal().trust; } catch { return null; }
}

function resolveSharedAuthority(trust, readAuthority = readCommittedKeyAuthority) {
  if (!trust?.keyAuthority) return { trust };
  const authority = readAuthority(trust);
  if (!authority.ok) throw new Error(`protected ownership key authority failed: ${authority.problems.join("; ")}`);
  if (!authority.resolvedTrust.ownershipKeys.length) throw new Error("protected ownership key authority has no active approval keys");
  return { trust: authority.resolvedTrust, keyLogDigest: authority.digest, keyCheckpoint: authority.keyCheckpoint, rootFingerprint: authority.rootFingerprint, keyAuthority: authority.authority };
}

function resolveRepositoryKeyLog(trust, repositoryRoot, repositoryIdentity, readAuthority = readCommittedKeyAuthority) {
  if (!trust?.rootFingerprint || trust.keyAuthority) return { trust };
  const authority = readAuthority({ ...trust, keyAuthority: { repositoryIdentity, checkoutPath: repositoryRoot } });
  if (!authority.ok) throw new Error(`protected ownership key log failed: ${authority.problems.join("; ")}`);
  if (!authority.resolvedTrust.ownershipKeys.length) throw new Error("protected ownership key log has no active approval keys");
  return { trust: authority.resolvedTrust, keyLogDigest: authority.digest, keyCheckpoint: authority.keyCheckpoint, rootFingerprint: authority.rootFingerprint };
}

export function scanApp(appRoot, manifest, options = {}) {
  return scanAppInternal(appRoot, manifest, options);
}
export function scanAppForTest(appRoot, manifest, options = {}) {
  return scanAppInternal(appRoot, manifest, options, options.readAuthorityForTest);
}
function scanAppInternal(appRoot, manifest, options, readAuthority) {
  const initialTrust = trustForScan(options), location = scanRepositoryRoot(appRoot);
  const repositoryIdentity = repositoryIdentityFor(appRoot);
  const authority = initialTrust?.keyAuthority ? resolveSharedAuthority(initialTrust, readAuthority) : resolveRepositoryKeyLog(initialTrust, location.root, repositoryIdentity, readAuthority);
  const trust = authority.trust;
  const result = scanInternal(appRoot, manifest, { inventory: fileInventory(appRoot, trust, location.prefix, location.root), trust, repositoryIdentity, repositoryRoot: location.root, pathPrefix: location.prefix, allowlistPolicy: dependencyAllowlistPolicy(trust, repositoryIdentity, { pending: enrollmentPending(trust, options.trust), shipped: options.shippedAllowlist }) });
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

/** Effective enforcement policy; a fingerprint without a tier is treated as `block` (fail closed). */
function findingPolicy(c, f) {
  return { tier: f.tier ?? c.tier ?? "block", ...(f.policy ? { policy: f.policy } : {}) };
}
export function evaluate(findings, exceptions, options = {}) {
  const today = options.today ?? new Date().toISOString().slice(0, 10), problems = [], covered = [], violations = [], warnings = [];
  const metadata = options.metadata ?? findings.metadata ?? {};
  const catalogDigest = metadata.catalogDigest ?? options.catalogDigest ?? "";
  const identity = options.repositoryIdentity ?? metadata.repositoryIdentity ?? "";
  for (const e of exceptions) {
    const p = e?.payload ?? e ?? {};
    const where = `exception #${e?.index} (${p.fingerprint ?? "?"})`;
    const missing = ["fingerprint", "occurrences", "policyDigest", "minimumScannerVersion", "minimumCatalogVersion", "issued", "reason", "owner", "expires", "requestId"].filter((k) => !p[k] || (Array.isArray(p[k]) && !p[k].length));
    if (missing.length) { problems.push(`${where}: missing ${missing.join(", ")}`); continue; }
    const approvedBy = e.approvedBy ?? p.approvedBy;
    if (approvedBy !== undefined && approvedBy !== "Adrian") { problems.push(`${where}: approvedBy must be Adrian`); continue; }
    if (!/^\d{4}-\d{2}-\d{2}$/.test(p.expires) || Number.isNaN(Date.parse(`${p.expires}T00:00:00Z`)) || p.expires < today) { problems.push(`${where}: invalid or expired expires date`); continue; }
    if (!p.trackingRef || typeof p.trackingRef !== "object" || !p.trackingRef.verified || !/^https:\/\//.test(p.trackingRef.url ?? "") || !p.trackingRef.id) { problems.push(`${where}: tracking must be a verified RightKit request reference`); continue; }
    let policyDigest;
    try { policyDigest = policyEntryDigest(options.manifest ?? installedManifest, p.fingerprint); } catch (error) { problems.push(`${where}: ${error.message}`); continue; }
    const { index, ...record } = e;
    const verification = verifyException(record, { repositoryIdentity: identity, catalogDigest, policyDigest, findings, trust: options.trust, today, scannerVersion: options.scannerVersion ?? metadata.scannerVersion ?? SCANNER_VERSION, catalogVersion: options.catalogVersion ?? metadata.catalogVersion });
    if (!verification?.ok) { problems.push(`${where}: signature verification failed${verification?.problems?.length ? ` (${verification.problems.join("; ")})` : ""}`); continue; }
    const matching = findings.filter((f) => f.fingerprint === p.fingerprint && p.occurrences.some((item) => item.path === f.file && item.hash === occurrenceHash(f)));
    if (!matching.length) { problems.push(`${where}: stale, matches no signed occurrence`); continue; }
    for (const f of matching) covered.push({ ...f, occurrenceHash: occurrenceHash(f), exception: e });
  }
  const pending = enrollmentPending(options.trust, options.trustEnrollment);
  if (pending) warnings.push("approval enrollment pending; detected findings remain open backlog requests until distinct Windows & macOS keys are enrolled");
  for (const f of findings) if (!covered.some((x) => x.occurrenceHash === occurrenceHash(f))) violations.push({ ...f, occurrenceHash: occurrenceHash(f) });
  return { violations, covered, problems, warnings, enrollmentPending: pending };
}

export function capabilityTable(manifest, deps, result) {
  const groups = new Map();
  for (const c of manifest.capabilities) {
    if (!groups.has(c.capability)) groups.set(c.capability, []);
    groups.get(c.capability).push(c.package);
  }
  return [...groups].map(([capability, owners]) => {
    const owner = owners[0];
    const declared = deps.some((d) => owners.includes(d.name) || owners.some((name) => d.name === name.replace(/^rightkit-/, "@rightkit/")));
    const own = result.violations.filter((x) => x.capability === capability), covered = result.covered.filter((x) => x.capability === capability);
    const status = own.length ? "blocked" : covered.length ? "excepted" : declared ? "declared-dependency" : result.coverage?.length ? "unscanned" : "not-detected";
    return { capability, owner, ...(owners.length > 1 ? { owners } : {}), uses: declared, status, count: own.length + covered.length };
  });
}
export function formatTable(rows) { return ["capability  owner  status", ...rows.map((x) => `${x.capability}  ${x.owner}  ${x.status}`)].join("\n"); }

export function checkApp(appRoot, manifest, options = {}) {
  return checkAppInternal(appRoot, manifest, options);
}
export function checkAppForTest(appRoot, manifest, options = {}) {
  return checkAppInternal(appRoot, manifest, options, options.readAuthorityForTest);
}
function checkAppInternal(appRoot, manifest, options, readAuthority) {
  let trustState = options.trust;
  if (!trustState) trustState = loadTrustedExternal();
  const initialTrust = trustState && Object.prototype.hasOwnProperty.call(trustState, "trust") ? trustState.trust : trustState;
  const repositoryLocation = scanRepositoryRoot(appRoot);
  const repositoryIdentity = repositoryIdentityFor(appRoot);
  const resolved = initialTrust?.keyAuthority ? resolveSharedAuthority(initialTrust, readAuthority) : resolveRepositoryKeyLog(initialTrust, repositoryLocation.root, repositoryIdentity, readAuthority);
  const trust = resolved.trust;
  const location = repositoryLocation;
  const enrollment = trust?.rootFingerprint
    ? { enrolled: trust.enrollmentComplete === true, pending: trust.enrollmentComplete !== true }
    : trustState?.pending !== undefined || trustState?.enrolled !== undefined ? trustState : undefined;
  const scanned = scanInternal(appRoot, manifest, { inventory: fileInventory(appRoot, trust, location.prefix, location.root), trust, repositoryIdentity, repositoryRoot: location.root, pathPrefix: location.prefix, allowlistPolicy: dependencyAllowlistPolicy(trust, repositoryIdentity, { pending: enrollmentPending(trust, enrollment), shipped: options.shippedAllowlist }) });
  const metadata = { ...scanMetadata(appRoot, manifest, scanned.coverage, scanned.contentDigests, repositoryIdentity, location.commit), pathBase: location.pathBase, pathPrefix: location.prefix, provenance: scanned.provenance, ...Object.fromEntries(Object.entries(resolved).filter(([key]) => key !== "trust")) };
  if (trust?.rootFingerprint && Array.isArray(trust.ownershipKeys) && !trust.ownershipKeys.length) throw new Error("protected ownership key log has no active approval keys");
  const result = evaluate(scanned.findings, loadExceptions(appRoot), { ...options, manifest, trust, trustEnrollment: enrollment, metadata });
  const coverage = [...new Map(metadata.coverage.map((x) => [`${x.path}\0${x.reason}`, x])).values()];
  const rows = capabilityTable(manifest, scanned.deps, { ...result, coverage });
  return { ok: !result.enrollmentPending && !result.violations.some(isBlockingFinding) && !result.problems.length && !coverage.length, rows, ...result, findings: scanned.findings, coverage, metadata };
}

function checkCommitted(appRoot, manifest, options, acquire, readAuthority = readCommittedKeyAuthority) {
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
    const authority = readAuthority(trust);
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
  const committedEnrollment = trust?.rootFingerprint
    ? { enrolled: trust.enrollmentComplete === true, pending: trust.enrollmentComplete !== true }
    : trustState?.pending !== undefined || trustState?.enrolled !== undefined ? trustState : undefined;
  const scanned = scanInternal(appRoot, manifest, { inventory, trust, repositoryIdentity, repositoryRoot: location.root, pathPrefix: location.prefix, allowlistPolicy: dependencyAllowlistPolicy(trust, repositoryIdentity, { pending: enrollmentPending(trust, committedEnrollment), shipped: options.shippedAllowlist }) });
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
  const result = evaluate(scanned.findings, exceptions, { ...options, manifest, trust, trustEnrollment: trust?.rootFingerprint
    ? { enrolled: trust.enrollmentComplete === true, pending: trust.enrollmentComplete !== true }
    : trustState?.pending !== undefined || trustState?.enrolled !== undefined ? trustState : undefined, metadata });
  const coverage = [...new Map(metadata.coverage.map((x) => [`${x.path}\0${x.reason}`, x])).values()];
  const rows = capabilityTable(manifest, scanned.deps, { ...result, coverage });
  return { ok: !result.enrollmentPending && !result.violations.some(isBlockingFinding) && !result.problems.length && !coverage.length, rows, ...result, findings: scanned.findings, coverage, metadata };
}

/** Protected production scan: acquire and inspect exact HEAD blobs without reading checkout contents. */
export function checkCommittedApp(appRoot, manifest, options = {}) {
  return checkCommitted(appRoot, manifest, options, acquireCommittedSource);
}

/** Explicit test-only seam for bounded Git process failures and fixtures. */
export function checkCommittedAppForTest(appRoot, manifest, options = {}) {
  return checkCommitted(appRoot, manifest, options, options.acquireCommittedSourceForTest ?? acquireCommittedSource, options.readAuthorityForTest);
}
