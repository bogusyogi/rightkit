// RightKit ownership gate: no Right Suite app keeps its own copy of a capability RightKit owns.
//
// Each RightKit package declares what it owns in `rightkit-ownership.json` beside its manifest
// (capability + fingerprints of a reimplementation). `aggregateOwnership` folds those into the
// shipped `rightkit-ownership.json`, so an app is always checked against the data of the
// RightKit version it pins. `scanApp` reports fingerprint matches in an app repository; each
// must be covered by a live, specific entry in the app's `.rightkit/ownership-exceptions.json`
// (the migration backlog) or the gate fails.
//
// Fingerprint types:
//   dependency  direct dependency by name (cargo | npm | swiftpm). Transitive use through a
//               RightKit package is never flagged.
//   source      regex over code files (comments-only lines, docs, tests fixtures, vendored and
//               generated trees are skipped).
import { spawnSync } from "./background-process.mjs";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";

export const MANIFEST_FILE = "rightkit-ownership.json";
export const EXCEPTIONS_FILE = path.join(".rightkit", "ownership-exceptions.json");

const SKIP_DIRS = new Set([
  ".git", "node_modules", "target", "dist", "build", "out", ".next", ".turbo", ".cache",
  "vendor", "third_party", "Pods", "DerivedData", ".build", "generated", "fixtures",
  "test-fixtures", "__fixtures__", ".rightkit", ".audit",
]);
const CODE_EXT = new Set([".rs", ".ts", ".tsx", ".js", ".mjs", ".cjs", ".jsx", ".swift", ".m", ".mm", ".c", ".cc", ".cpp", ".h", ".py", ".go", ".kt", ".java", ".cs"]);
const MAX_FILE_BYTES = 2 * 1024 * 1024;

/** Collect every package's ownership declaration under a RightKit checkout. */
export function aggregateOwnership(rightkitRoot) {
  const entries = [];
  for (const group of ["crates", "packages"]) {
    const dir = path.join(rightkitRoot, group);
    if (!existsSync(dir)) continue;
    for (const name of readdirSync(dir).sort()) {
      const file = path.join(dir, name, MANIFEST_FILE);
      if (!existsSync(file)) continue;
      const declared = JSON.parse(readFileSync(file, "utf8"));
      if (declared.schema !== undefined) continue; // the shipped aggregate itself
      for (const capability of declared.capabilities ?? []) {
        entries.push({ package: declared.package, ...capability });
      }
    }
  }
  const manifest = { schema: 1, capabilities: entries };
  validateManifest(manifest);
  return manifest;
}

export function validateManifest(manifest) {
  const ids = new Set();
  for (const c of manifest.capabilities) {
    if (!c.package || !c.capability || !Array.isArray(c.fingerprints) || c.fingerprints.length === 0) {
      throw new Error(`ownership: incomplete capability ${JSON.stringify(c).slice(0, 120)}`);
    }
    for (const f of c.fingerprints) {
      if (!f.id || ids.has(f.id)) throw new Error(`ownership: missing or duplicate fingerprint id ${f.id}`);
      ids.add(f.id);
      if (f.type === "dependency") {
        if (!["cargo", "npm", "swiftpm"].includes(f.ecosystem) || !f.name) throw new Error(`ownership: bad dependency fingerprint ${f.id}`);
      } else if (f.type === "source") {
        new RegExp(f.pattern); // throws on a bad pattern
      } else {
        throw new Error(`ownership: unknown fingerprint type ${f.type} (${f.id})`);
      }
    }
  }
}

function skippedDir(name) {
  return SKIP_DIRS.has(name) || name.startsWith(".") || /^(node_modules|target)/.test(name);
}

const RIGHTKIT_MARKER = `packages/release/${MANIFEST_FILE}`;

// A nested repository is its own app with its own exceptions file, and RightKit's own sources
// are the owner, never a copy.
function walkTree(root, visit, rel = "") {
  let entries;
  try { entries = readdirSync(path.join(root, rel), { withFileTypes: true }); } catch { return; }
  for (const e of entries) {
    const r = rel ? `${rel}/${e.name}` : e.name;
    if (e.isDirectory()) {
      if (!skippedDir(e.name) && !existsSync(path.join(root, r, ".git"))
        && !existsSync(path.join(root, r, RIGHTKIT_MARKER))) walkTree(root, visit, r);
    } else if (e.isFile()) {
      visit(r);
    }
  }
}

// Tracked plus untracked-but-not-ignored files. Git never descends into nested repositories,
// and honoring .gitignore keeps model, data and build trees out of the scan.
function gitFiles(root) {
  if (!existsSync(path.join(root, ".git"))) return null;
  const r = spawnSync("git", ["-C", root, "ls-files", "-z", "--cached", "--others", "--exclude-standard"], {
    encoding: "utf8", maxBuffer: 256 * 1024 * 1024, windowsHide: true,
  });
  if (r.status !== 0) return null;
  const files = r.stdout.split("\0").filter((f) => f && !f.endsWith("/"));
  const rightkitRoots = files.filter((f) => f === RIGHTKIT_MARKER || f.endsWith(`/${RIGHTKIT_MARKER}`))
    .map((f) => f.slice(0, f.length - RIGHTKIT_MARKER.length));
  return files.filter((f) => !rightkitRoots.some((p) => f.startsWith(p))
    && !f.split("/").slice(0, -1).some(skippedDir));
}

function walk(root, visit) {
  const files = gitFiles(root);
  if (files) files.forEach((f) => visit(f));
  else walkTree(root, visit);
}

/** Direct dependencies declared by an app repository: [{ecosystem, name, file}]. */
export function directDependencies(appRoot) {
  const deps = [];
  walk(appRoot, (rel) => {
    const base = path.posix.basename(rel);
    const abs = path.join(appRoot, rel);
    if (base === "Cargo.toml") {
      let section = "";
      for (const line of readFileSync(abs, "utf8").split(/\r?\n/)) {
        const header = line.match(/^\s*\[([^\]]+)\]/);
        if (header) { section = header[1]; continue; }
        const dotted = section.match(/(^|\.)(dev-|build-)?dependencies\.([A-Za-z0-9_-]+)$/);
        if (dotted) { deps.push({ ecosystem: "cargo", name: dotted[3], file: rel }); section = "~"; continue; }
        if (!/(^|\.)((dev|build)-)?dependencies$/.test(section)) continue;
        const m = line.match(/^\s*([A-Za-z0-9_-]+)\s*=/);
        if (m) deps.push({ ecosystem: "cargo", name: m[1], file: rel });
      }
    } else if (base === "package.json") {
      let pkg;
      try { pkg = JSON.parse(readFileSync(abs, "utf8")); } catch { return; }
      for (const field of ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"]) {
        for (const name of Object.keys(pkg[field] ?? {})) deps.push({ ecosystem: "npm", name, file: rel });
      }
    } else if (base === "Package.swift") {
      for (const m of readFileSync(abs, "utf8").matchAll(/\.package\([^)]*url:\s*"([^"]+)"/g)) {
        const name = m[1].replace(/\.git$/, "").split("/").pop();
        deps.push({ ecosystem: "swiftpm", name, file: rel });
      }
    }
  });
  return deps;
}

function isCommentOnly(line) {
  const t = line.trim();
  return t.startsWith("//") || t.startsWith("#") || t.startsWith("*") || t.startsWith("/*") || t.startsWith("--");
}

/** All fingerprint matches in an app: [{fingerprint, capability, package, file, line, text}]. */
export function scanApp(appRoot, manifest) {
  const findings = [];
  const deps = directDependencies(appRoot);
  const sources = [];
  for (const c of manifest.capabilities) {
    for (const f of c.fingerprints) {
      if (f.type === "dependency") {
        for (const d of deps) {
          if (d.ecosystem === f.ecosystem && d.name === f.name) {
            findings.push({ fingerprint: f.id, capability: c.capability, package: c.package, file: d.file, line: 0, text: `${d.ecosystem} dependency ${d.name}` });
          }
        }
      } else {
        sources.push({ c, f, re: new RegExp(f.pattern, f.flags ?? "") });
      }
    }
  }
  if (sources.length) {
    // One combined pass rejects almost every file and line before the per-fingerprint tests.
    const any = new RegExp(sources.map(({ f }) => `(?:${f.pattern})`).join("|"));
    walk(appRoot, (rel) => {
      if (!CODE_EXT.has(path.posix.extname(rel))) return;
      const abs = path.join(appRoot, rel);
      try { if (statSync(abs).size > MAX_FILE_BYTES) return; } catch { return; }
      const text = readFileSync(abs, "utf8");
      if (!any.test(text)) return;
      const lines = text.split(/\r?\n/);
      lines.forEach((line, i) => {
        if (!any.test(line) || isCommentOnly(line)) return;
        for (const { c, f, re } of sources) {
          if (re.test(line)) findings.push({ fingerprint: f.id, capability: c.capability, package: c.package, file: rel, line: i + 1, text: line.trim().slice(0, 160) });
        }
      });
    });
  }
  return findings;
}

function globToRegExp(glob) {
  let re = "";
  for (let i = 0; i < glob.length; i++) {
    const ch = glob[i];
    if (ch === "*" && glob[i + 1] === "*") { re += ".*"; i++; if (glob[i + 1] === "/") i++; }
    else if (ch === "*") re += "[^/]*";
    else if (ch === "?") re += "[^/]";
    else re += ch.replace(/[.+^${}()|[\]\\]/g, "\\$&");
  }
  return new RegExp(`^${re}$`);
}

/** Load and validate an app's exceptions (the migration backlog). */
export function loadExceptions(appRoot) {
  const file = path.join(appRoot, EXCEPTIONS_FILE);
  if (!existsSync(file)) return [];
  const parsed = JSON.parse(readFileSync(file, "utf8"));
  return (parsed.exceptions ?? []).map((e, i) => ({ ...e, index: i }));
}

/**
 * Apply exceptions. Returns {violations, covered, problems}: uncovered findings, findings an
 * exception covers, and exception defects (expired, stale, wildcard-only, incomplete).
 */
export function evaluate(findings, exceptions, { today = new Date().toISOString().slice(0, 10) } = {}) {
  const problems = [];
  const live = [];
  for (const e of exceptions) {
    const where = `exception #${e.index} (${e.fingerprint ?? "?"})`;
    const missing = ["fingerprint", "paths", "reason", "owner", "approvedBy"].filter((k) => !e[k] || (Array.isArray(e[k]) && e[k].length === 0));
    if (missing.length) { problems.push(`${where}: missing ${missing.join(", ")}`); continue; }
    if (!e.expires && !e.tracking) { problems.push(`${where}: needs an expires date or a tracking item`); continue; }
    if (e.paths.some((p) => /^\*+(\/\*+)*$/.test(p))) { problems.push(`${where}: wildcard-only paths are not allowed`); continue; }
    if (e.expires && e.expires < today) { problems.push(`${where}: expired ${e.expires}`); continue; }
    live.push({ ...e, matchers: e.paths.map(globToRegExp), used: false });
  }
  const violations = [];
  const covered = [];
  for (const f of findings) {
    const e = live.find((x) => x.fingerprint === f.fingerprint && x.matchers.some((m) => m.test(f.file)));
    if (e) { e.used = true; covered.push({ ...f, exception: e }); } else violations.push(f);
  }
  for (const e of live) if (!e.used) problems.push(`exception #${e.index} (${e.fingerprint}): stale, matches nothing; delete it`);
  return { violations, covered, problems };
}

/** One row per capability: uses RightKit | own copy (exception, expiry) | not needed. */
export function capabilityTable(manifest, deps, result) {
  const rows = [];
  const byCapability = new Map();
  for (const c of manifest.capabilities) {
    if (!byCapability.has(c.capability)) byCapability.set(c.capability, c.package);
  }
  for (const [capability, owner] of byCapability) {
    const uses = deps.some((d) => d.name === owner || d.name === owner.replace(/^rightkit-/, "@rightkit/"));
    const own = result.violations.filter((v) => v.capability === capability);
    const excepted = result.covered.filter((v) => v.capability === capability);
    let status;
    if (own.length) status = `OWN COPY, NOT EXCEPTED (${own.length})`;
    else if (excepted.length) {
      const e = excepted[0].exception;
      status = `own copy, excepted until ${e.expires ?? e.tracking} (${excepted.length})`;
    } else status = uses ? "uses RightKit" : "not needed";
    rows.push({ capability, owner, uses, status });
  }
  return rows;
}

export function formatTable(rows) {
  const w1 = Math.max(10, ...rows.map((r) => r.capability.length));
  const w2 = Math.max(7, ...rows.map((r) => r.owner.length));
  const lines = [`${"capability".padEnd(w1)}  ${"owner".padEnd(w2)}  status`];
  for (const r of rows) lines.push(`${r.capability.padEnd(w1)}  ${r.owner.padEnd(w2)}  ${r.status}`);
  return lines.join("\n");
}

/** Full check for one app. `ok` is false on any uncovered copy or exception defect. */
export function checkApp(appRoot, manifest, options = {}) {
  const findings = scanApp(appRoot, manifest);
  const result = evaluate(findings, loadExceptions(appRoot), options);
  const rows = capabilityTable(manifest, directDependencies(appRoot), result);
  return { ok: result.violations.length === 0 && result.problems.length === 0, rows, ...result };
}
