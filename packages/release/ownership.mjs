// RightKit ownership scan. Public exports retain the original findings-array API.
import { spawnSync } from "./background-process.mjs";
import { canonicalRepositoryIdentity, loadTrustedExternal, occurrenceHash, sha256, verifyException } from "./ownership-approval.mjs";
import { parse as parseToml } from "smol-toml";
import { existsSync, lstatSync, readdirSync, readFileSync, realpathSync, statSync } from "node:fs";
import { createHash } from "node:crypto";
import os from "node:os";
import path from "node:path";

export const MANIFEST_FILE = "rightkit-ownership.json";
export const EXCEPTIONS_FILE = path.join(".rightkit", "ownership-exceptions.json");
const CODE_EXT = new Set([".rs", ".ts", ".tsx", ".js", ".mjs", ".cjs", ".jsx", ".swift", ".m", ".mm", ".c", ".cc", ".cpp", ".h", ".py", ".go", ".kt", ".java", ".cs"]);
// Large installed stores are reported as unscanned; source-bearing trees are walked regardless of name.
const OPAQUE_DIRS = new Set(["target", ".build", "DerivedData", "Pods"]);
export const SCANNER_VERSION = "2.0.0";
export const catalogDigest = (manifest) => sha256(manifest);
const byteDigest = (bytes) => createHash("sha256").update(bytes).digest("hex");

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
function parseCargo(parsed, file, out, workspace, root, coverage) {
  const pathSpec = (pathValue, sourceFile, display) => {
    if (!pathValue) return;
    const resolved = path.resolve(root, path.posix.dirname(sourceFile), pathValue);
    try {
      const rootReal = realpathSync(root), targetReal = realpathSync(resolved);
      if (targetReal !== rootReal && !targetReal.startsWith(rootReal + path.sep)) coverage.push({ path: display, status: "unscanned", reason: "local-dependency-outside-repository" });
    } catch { coverage.push({ path: display, status: "unscanned", reason: "local-dependency-path-unresolved" }); }
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

function scanFiles(root, visit, coverage) {
  const rootReal = realpathSync(root); const seen = new Set();
  const walk = (dir, rel) => {
    let entries;
    try { entries = readdirSync(dir, { withFileTypes: true }); }
    catch (e) { coverage.push({ path: rel || ".", status: "unscanned", reason: `directory-read:${e.code ?? "error"}` }); return; }
    for (const ent of entries) {
      const r = rel ? `${rel}/${ent.name}` : ent.name; const abs = path.join(dir, ent.name);
      let st;
      try { st = lstatSync(abs); } catch (e) { coverage.push({ path: r, status: "unscanned", reason: `stat:${e.code ?? "error"}` }); continue; }
      if (st.isSymbolicLink()) {
        let real; try { real = realpathSync(abs); } catch { coverage.push({ path: r, status: "unscanned", reason: "broken-symlink" }); continue; }
        if (real !== rootReal && !real.startsWith(rootReal + path.sep)) { coverage.push({ path: r, status: "unscanned", reason: "symlink-outside-repository" }); continue; }
        if (seen.has(real)) continue; seen.add(real);
        try { const target = statSync(real); if (target.isDirectory()) walk(real, r); else if (target.isFile()) visit(r, real, target.size); } catch { coverage.push({ path: r, status: "unscanned", reason: "symlink-target-unreadable" }); }
      } else if (st.isDirectory()) {
        if (ent.name === ".git") continue;
        if (ent.name === "node_modules") {
          // Directory names and package-manager layout are not provenance. Without an exact
          // protected exclusion, installed source and linked local wrappers remain unscanned.
          coverage.push({ path: r, status: "unscanned", reason: "node-modules-provenance-not-verified" });
          continue;
        }
        if (OPAQUE_DIRS.has(ent.name)) coverage.push({ path: r, status: "unscanned", reason: "installed-or-build-cache" });
        else walk(abs, r);
      } else if (st.isFile()) visit(r, abs, st.size);
    }
  };
  walk(rootReal, "");
}

export function directDependencies(appRoot) {
  const deps = [], coverage = [], contentDigests = [];
  // Workspace inheritance is loaded before member manifests.
  const files = [];
  scanFiles(appRoot, (rel, abs, size) => { if (["Cargo.toml", "package.json", "Package.swift"].includes(path.posix.basename(rel))) files.push({ rel, abs, size }); }, coverage);
  const cargoFiles = files.filter((x) => path.posix.basename(x.rel) === "Cargo.toml");
  const workspaces = [];
  for (const x of cargoFiles) {
    try { const parsed = parseToml(readFileSync(x.abs, "utf8")); const deps = cargoWorkspace(parsed); if (Object.keys(deps).length) workspaces.push({ dir: path.posix.dirname(x.rel), deps }); }
    catch { coverage.push({ path: x.rel, status: "unscanned", reason: "invalid-cargo-toml" }); }
  }
  for (const { rel, abs } of files) {
    const base = path.posix.basename(rel), bytes = readFileSync(abs), text = bytes.toString("utf8");
    contentDigests.push({ path: rel, digest: byteDigest(bytes) });
    if (base === "Cargo.toml") {
      const dir = path.posix.dirname(rel);
      const workspace = workspaces.filter((x) => x.dir === "." || dir === x.dir || dir.startsWith(`${x.dir}/`)).sort((a, b) => b.dir.length - a.dir.length)[0] ?? { dir: ".", deps: {} };
      try { parseCargo(parseToml(text), rel, deps, workspace, appRoot, coverage); }
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
          const target = path.resolve(appRoot, path.posix.dirname(rel), localPath);
          try { const base = realpathSync(appRoot), real = realpathSync(target); if (real !== base && !real.startsWith(base + path.sep)) coverage.push({ path: rel, status: "unscanned", reason: `local-npm-dependency-outside-repository:${name}` }); }
          catch { coverage.push({ path: rel, status: "unscanned", reason: `local-npm-dependency-path-unresolved:${name}` }); }
        } else if (isGit) coverage.push({ path: rel, status: "unscanned", reason: `git-npm-dependency-source:${name}` });
      }
    } else {
      for (const m of text.matchAll(/\.package\s*\(([^)]*)\)/g)) {
        const arg = m[1], url = arg.match(/url:\s*["']([^"']+)["']/)?.[1], pathDep = arg.match(/path:\s*["']([^"']+)["']/)?.[1];
        const id = arg.match(/name:\s*["']([^"']+)["']/)?.[1] ?? url?.replace(/\.git$/i, "").split("/").pop()?.toLowerCase() ?? pathDep?.split(/[\\/]/).pop();
        addDependency(deps, "swiftpm", id, rel, { local: !!pathDep, source: pathDep ? "path" : url });
        if (pathDep) {
          const target = path.resolve(appRoot, path.posix.dirname(rel), pathDep);
          try { const base = realpathSync(appRoot), real = realpathSync(target); if (real !== base && !real.startsWith(base + path.sep)) coverage.push({ path: rel, status: "unscanned", reason: `local-swift-dependency-outside-repository:${id}` }); }
          catch { coverage.push({ path: rel, status: "unscanned", reason: `local-swift-dependency-path-unresolved:${id}` }); }
        }
      }
    }
  }
  Object.defineProperty(deps, "coverage", { value: coverage, enumerable: false });
  Object.defineProperty(deps, "contentDigests", { value: contentDigests, enumerable: false });
  return deps;
}

/** Content inventory for cache keys; unsupported trees remain visible as incomplete coverage. */
export function contentInventory(appRoot) {
  const files = [], coverage = [];
  scanFiles(appRoot, (rel, abs, size) => {
    if (size > 64 * 1024 * 1024) { coverage.push({ path: rel, status: "unscanned", reason: "inventory-file-over-64MiB" }); return; }
    try { files.push({ path: rel, size, digest: byteDigest(readFileSync(abs)) }); }
    catch { coverage.push({ path: rel, status: "unscanned", reason: "file-read-failed" }); }
  }, coverage);
  files.sort((a, b) => a.path.localeCompare(b.path));
  return { digest: sha256(files), files, coverage, complete: coverage.length === 0 };
}

function commentOnly(line) { const t = line.trim(); return t.startsWith("//") || t.startsWith("#") || t.startsWith("*") || t.startsWith("/*") || t.startsWith("--"); }
function scanInternal(appRoot, manifest) {
  const findings = [], coverage = [], deps = directDependencies(appRoot), contentDigests = [...(deps.contentDigests ?? [])]; coverage.push(...(deps.coverage ?? []));
  for (const c of manifest.capabilities) for (const f of c.fingerprints.filter((x) => x.type === "dependency")) for (const d of deps) {
    if (d.ecosystem === f.ecosystem && (d.name === f.name || d.alias === f.name)) {
      findings.push({ fingerprint: f.id, capability: c.capability, package: c.package, file: d.file, line: 0, text: `${d.ecosystem} dependency ${d.alias ? `${d.alias} aliases ` : ""}${d.name}`, local: d.local, provenance: d.source });
    }
  }
  const sources = manifest.capabilities.flatMap((c) => c.fingerprints.filter((f) => f.type === "source").map((f) => ({ c, f, re: new RegExp(f.pattern, f.flags ?? "") })));
  scanFiles(appRoot, (rel, abs, size) => {
    if (!CODE_EXT.has(path.posix.extname(rel).toLowerCase())) return;
    if (size > 2 * 1024 * 1024) { coverage.push({ path: rel, status: "unscanned", reason: "source-file-over-2MiB" }); return; }
    let bytes, text; try { bytes = readFileSync(abs); text = bytes.toString("utf8"); } catch { coverage.push({ path: rel, status: "unscanned", reason: "file-read-failed" }); return; }
    contentDigests.push({ path: rel, digest: byteDigest(bytes) });
    for (const [i, line] of text.split(/\r?\n/).entries()) if (!commentOnly(line)) for (const { c, f, re } of sources) {
      re.lastIndex = 0; if (re.test(line)) findings.push({ fingerprint: f.id, capability: c.capability, package: c.package, file: rel, line: i + 1, text: line.trim().slice(0, 160) });
    }
  }, coverage);
  for (const f of findings) f.occurrenceHash = occurrenceHash(f);
  contentDigests.sort((a, b) => a.path.localeCompare(b.path));
  return { findings, coverage, deps, contentDigests };
}

export function scanApp(appRoot, manifest) {
  const result = scanInternal(appRoot, manifest);
  Object.defineProperty(result.findings, "coverage", { value: result.coverage, enumerable: false });
  Object.defineProperty(result.findings, "metadata", { value: scanMetadata(appRoot, manifest, result.coverage, result.contentDigests), enumerable: false });
  return result.findings;
}

function scanMetadata(root, manifest, coverage = [], contentDigests = []) {
  const git = (...args) => spawnSync("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true, maxBuffer: 4 * 1024 * 1024 });
  const commit = git("rev-parse", "HEAD"); const origin = git("config", "--get", "remote.origin.url");
  const fullCoverage = [...coverage];
  let repositoryIdentity = null;
  if (origin.status === 0) { try { repositoryIdentity = canonicalRepositoryIdentity(origin.stdout.trim()); } catch {} }
  return { repositoryIdentity, commit: commit.status === 0 ? commit.stdout.trim() : null, host: `${os.hostname()}/${process.platform}`, scannedAt: new Date().toISOString(), catalogDigest: catalogDigest(manifest), catalogVersion: manifest.version ?? String(manifest.schema ?? "1"), scannerVersion: SCANNER_VERSION, contentDigests, coverage: fullCoverage };
}

function globToRegExp(glob) {
  let out = ""; for (let i = 0; i < glob.length; i++) { const c = glob[i]; if (c === "*" && glob[i + 1] === "*") { out += ".*"; i++; if (glob[i + 1] === "/") i++; } else if (c === "*") out += "[^/]*"; else if (c === "?") out += "[^/]"; else out += c.replace(/[.+^${}()|[\]\\]/g, "\\$&"); }
  return new RegExp(`^${out}$`);
}
export function loadExceptions(root) { const file = path.join(root, EXCEPTIONS_FILE); if (!existsSync(file)) return []; return (JSON.parse(readFileSync(file, "utf8")).exceptions ?? []).map((e, index) => ({ ...e, index })); }

export function evaluate(findings, exceptions, options = {}) {
  const today = options.today ?? new Date().toISOString().slice(0, 10), problems = [], covered = [], violations = [];
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
  if (findings.length && (!options.trust || options.trust.enrolled === false || options.trustEnrollment?.pending)) problems.push("approval enrollment pending; detected findings remain open backlog requests");
  for (const f of findings) if (!covered.some((x) => x.occurrenceHash === occurrenceHash(f))) violations.push({ ...f, occurrenceHash: occurrenceHash(f) });
  return { violations, covered, problems };
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
  const scanned = scanInternal(appRoot, manifest), metadata = scanMetadata(appRoot, manifest, scanned.coverage, scanned.contentDigests);
  let trustState = options.trust;
  if (!trustState) {
    try { trustState = loadTrustedExternal(); }
    catch (error) { trustState = { enrolled: false, pending: true, trust: null, error: error.message }; }
  }
  const trust = trustState && Object.prototype.hasOwnProperty.call(trustState, "trust") ? trustState.trust : trustState;
  const result = evaluate(scanned.findings, loadExceptions(appRoot), { ...options, trust, trustEnrollment: trustState?.pending !== undefined ? trustState : undefined, metadata });
  const coverage = [...new Map(metadata.coverage.map((x) => [`${x.path}\0${x.reason}`, x])).values()];
  const rows = capabilityTable(manifest, scanned.deps, { ...result, coverage });
  return { ok: !result.violations.length && !result.problems.length && !coverage.length, rows, ...result, findings: scanned.findings, coverage, metadata };
}
