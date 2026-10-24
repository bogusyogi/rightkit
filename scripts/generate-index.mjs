#!/usr/bin/env node
// Generates RightKit's reference docs from source files. Do not hand-edit the outputs.
//   INDEX.md                crates and npm packages: source, published and staged versions, mismatch flags, owned capabilities
//   docs/ADOPTION.md        app repos that depend on each package, their pins, and stale or duplicate copies
//   docs/CAPABILITY-MAP.md  capability -> owner, tier, detection, published availability
// Inputs: crates/*/Cargo.toml, packages/*/package.json, packages/release/rightkit-versions.json,
// packages/release/rightkit-ownership.json, and the app repos under the workspace root.
// Run:   node rightkit/scripts/generate-index.mjs           (writes the three files)
//        node rightkit/scripts/generate-index.mjs --check   (exit 1 when committed files differ)
// --check-adoption   same comparison for docs/ADOPTION.md, which reads the app repos (not run by the test suite)
// Test-only options: --root <rightkit dir> --workspace <dir with app repos>
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { ownershipDetection } from "../packages/release/ownership-availability.mjs";

export const DEFAULT_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const DEFAULT_WORKSPACE = "/Volumes/D/claude";
export const APP_REPOS = ["cutright", "genright", "scraperight", "viewright", "heardright", "coderight", "mailright", "postright",
  "cockpit", "workright", "sellright", "rightsites", "voiceright", "legion"];
export const OUTPUTS = ["INDEX.md", "docs/ADOPTION.md", "docs/CAPABILITY-MAP.md"];
export const LOCAL_OUTPUTS = ["INDEX.md", "docs/CAPABILITY-MAP.md"];
export const ADOPTION_OUTPUTS = ["docs/ADOPTION.md"];
const SKIP_DIRS = new Set(["node_modules", "target", ".cache"]);
const NPM_SECTIONS = ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"];

const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
const cell = (text) => String(text ?? "").replace(/\|/g, "\\|").replace(/\s+/g, " ").trim();
const unquote = (text) => String(text).trim().replace(/^["']|["']$/g, "");
const listOrNone = (items) => (items.length ? items.join(", ") : "none");

// ---------- minimal TOML reader (sections, string/inline-table/array values, dotted keys) ----------

function splitTop(text) {
  const parts = [];
  let depth = 0, quote = null, current = "";
  for (const ch of text) {
    if (quote) { current += ch; if (ch === quote) quote = null; continue; }
    if (ch === '"' || ch === "'") { quote = ch; current += ch; continue; }
    if ("[{(".includes(ch)) depth++;
    if ("]})".includes(ch)) depth--;
    if (ch === "," && depth === 0) { parts.push(current); current = ""; continue; }
    current += ch;
  }
  if (current.trim()) parts.push(current);
  return parts.map((part) => part.trim()).filter(Boolean);
}

function parseValue(raw) {
  const value = raw.trim();
  if (value.startsWith('"') || value.startsWith("'")) return value.slice(1, value.indexOf(value[0], 1));
  if (value.startsWith("{")) {
    const table = {};
    for (const part of splitTop(value.slice(1, value.lastIndexOf("}")))) {
      const eq = part.indexOf("=");
      if (eq > 0) table[unquote(part.slice(0, eq))] = parseValue(part.slice(eq + 1));
    }
    return table;
  }
  if (value.startsWith("[")) return splitTop(value.slice(1, value.lastIndexOf("]"))).map(parseValue);
  return value.replace(/\s+#.*$/, "");
}

// Returns { [sectionName]: { [key]: value } }. Section names keep dots, e.g. "workspace.package".
export function parseToml(text) {
  const sections = { "": {} };
  let table = sections[""];
  let inMultiline = false;
  for (const raw of String(text).split(/\r?\n/)) {
    const line = raw.trim();
    if (line.includes('"""') || line.includes("'''")) { inMultiline = !inMultiline; continue; }
    if (inMultiline || !line || line.startsWith("#")) continue;
    if (line.startsWith("[")) {
      const name = line.replace(/^\[+/, "").replace(/\]+.*$/, "").trim();
      table = sections[name] ??= {};
      continue;
    }
    const match = /^("[^"]+"|'[^']+'|[A-Za-z0-9_.-]+)\s*=\s*(.*)$/.exec(line);
    if (match) table[unquote(match[1])] = parseValue(match[2]);
  }
  return sections;
}

const CARGO_DEP_SECTION = /^(?:target\..+\.)?(dependencies|dev-dependencies|build-dependencies|workspace\.dependencies)(?:\.(.+))?$/;

function cargoEntry(section, key, spec) {
  const body = spec && typeof spec === "object" ? spec : { version: spec };
  let source = "registry";
  if (body.path !== undefined) source = "local";
  else if (body.git !== undefined) source = "git";
  else if (body.registry !== undefined) source = "other-registry";
  return {
    section,
    ecosystem: "cargo",
    name: unquote(body.package ?? key),
    req: String(body.version ?? ""),
    source,
    inherits: String(body.workspace) === "true",
  };
}

export function cargoDependencies(toml) {
  const entries = [];
  for (const [name, table] of Object.entries(parseToml(toml))) {
    const match = CARGO_DEP_SECTION.exec(name);
    if (!match) continue;
    if (match[2]) { entries.push(cargoEntry(match[1], unquote(match[2]), table)); continue; }
    const grouped = {};
    for (const [key, value] of Object.entries(table)) {
      const dot = key.indexOf(".");
      if (dot > 0) (grouped[key.slice(0, dot)] ??= {})[key.slice(dot + 1)] = value;
      else entries.push(cargoEntry(match[1], unquote(key), value));
    }
    for (const [key, spec] of Object.entries(grouped)) entries.push(cargoEntry(match[1], key, spec));
  }
  return entries.filter((entry) => !entry.inherits);
}

const npmSource = (req) => (/^(workspace|file|link|portal):/.test(req) ? "local"
  : /^(git\+|git:|github:|https?:)|^[\w.-]+\/[\w.-]+$/.test(req) ? "git" : "registry");

export function npmDependencies(pkg) {
  const entries = [];
  for (const section of NPM_SECTIONS) {
    for (const [key, raw] of Object.entries(pkg[section] ?? {})) {
      let name = key, req = String(raw);
      if (req.startsWith("npm:")) {
        const body = req.slice(4), at = body.lastIndexOf("@");
        if (at > 0) { name = body.slice(0, at); req = body.slice(at + 1); } else { name = body; req = ""; }
      }
      entries.push({ section, ecosystem: "npm", name, req, source: npmSource(req) });
    }
  }
  return entries;
}

// ---------- semver subset: enough for cargo and npm requirements used by RightKit consumers ----------

const pad = (nums) => [0, 1, 2].map((i) => nums[i] ?? 0);
const cmpVer = (a, b) => { for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1; return 0; };
export function partsOf(text) {
  const match = /(\d+)(?:\.([0-9]+|[xX*]))?(?:\.([0-9]+|[xX*]))?/.exec(String(text));
  if (!match) return null;
  const nums = [];
  for (const part of match.slice(1)) { if (part === undefined || !/^\d+$/.test(part)) break; nums.push(Number(part)); }
  return nums;
}
const caretUpper = (nums) => {
  const first = nums.findIndex((n) => n !== 0);
  const k = first < 0 ? nums.length - 1 : first;
  const upper = [0, 0, 0];
  for (let j = 0; j < k; j++) upper[j] = nums[j];
  upper[k] = nums[k] + 1;
  return upper;
};
const tildeUpper = (nums) => (nums.length <= 1 ? [nums[0] + 1, 0, 0] : [nums[0], nums[1] + 1, 0]);
const OPERATOR = /^(\^|~|>=|<=|>|<|=)?/;

function clauseHolds(v, clause, flavor) {
  if (/^(\*|x|latest)$/i.test(clause)) return true;
  const op = OPERATOR.exec(clause)[1] ?? "";
  const nums = partsOf(clause);
  if (!nums) return false;
  const lower = pad(nums);
  const full = nums.length === 3;
  switch (op) {
    case ">=": return cmpVer(v, lower) >= 0;
    case ">": return full ? cmpVer(v, lower) > 0 : cmpVer(v, tildeUpper(nums)) >= 0;
    case "<=": return full ? cmpVer(v, lower) <= 0 : cmpVer(v, tildeUpper(nums)) < 0;
    case "<": return cmpVer(v, lower) < 0;
    case "^": return cmpVer(v, lower) >= 0 && cmpVer(v, caretUpper(nums)) < 0;
    case "~": return cmpVer(v, lower) >= 0 && cmpVer(v, tildeUpper(nums)) < 0;
    default:
      // "=1.2.3" and npm bare "1.2.3" are exact; npm partial "1.2" is a wildcard; cargo bare is caret.
      if (full && (op === "=" || flavor === "npm")) return cmpVer(v, lower) === 0;
      if (op === "=" || flavor === "npm") return cmpVer(v, lower) >= 0 && cmpVer(v, tildeUpper(nums)) < 0;
      return cmpVer(v, lower) >= 0 && cmpVer(v, caretUpper(nums)) < 0;
  }
}

export function satisfies(version, req, flavor = "npm") {
  const parts = partsOf(version);
  if (!parts) return false;
  const v = pad(parts);
  const clauses = String(req).replace(/([<>=~^]+)\s+/g, "$1").split(/[\s,]+/).filter(Boolean);
  return clauses.every((clause) => clauseHolds(v, clause, flavor));
}

export function isExactPin(req, flavor) {
  const nums = partsOf(req);
  const op = OPERATOR.exec(String(req).trim())[1] ?? "";
  return nums?.length === 3 && (op === "=" || (op === "" && flavor === "npm"));
}

export function pinStatus(rec, published) {
  if (rec.source === "local") return "local";
  if (rec.source === "git") return "git";
  if (rec.source !== "registry" || rec.source === "other-registry") return "unparsed";
  const req = rec.req.trim();
  if (req !== "" && req !== "*" && !partsOf(req)) return "unparsed";
  if (!published) return "unpublished";
  const pinned = pad(partsOf(req) ?? [0]);
  const latest = pad(partsOf(published));
  if (isExactPin(req, rec.ecosystem)) {
    const c = cmpVer(pinned, latest);
    return c === 0 ? "current" : c > 0 ? "ahead" : "stale";
  }
  if (satisfies(published, req, rec.ecosystem)) return "range ok";
  return cmpVer(pinned, latest) > 0 ? "ahead" : "stale";
}

// ---------- inventory ----------

const versionOf = (value) => (typeof value === "string" ? value : value?.version ?? "");
const STAGED_KEY = { npm: "stagedNpm", cargo: "stagedCargo", swift: "stagedSwift" };

function readJson(file) { return JSON.parse(readFileSync(file, "utf8")); }

function readmeLine(dir) {
  const file = path.join(dir, "README.md");
  if (!existsSync(file)) return "";
  return readFileSync(file, "utf8").split(/\r?\n/).map((l) => l.trim())
    .find((l) => l && !l.startsWith("#") && !l.startsWith("[!") && !l.startsWith("```")) ?? "";
}

function subdirs(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir, { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => e.name).sort(cmp);
}


function loadInventory(root) {
  const rootWorkspace = existsSync(path.join(root, "Cargo.toml"))
    ? parseToml(readFileSync(path.join(root, "Cargo.toml"), "utf8"))["workspace.package"]?.version ?? "" : "";
  const crates = [];
  for (const dirName of subdirs(path.join(root, "crates"))) {
    const dir = path.join(root, "crates", dirName);
    if (!existsSync(path.join(dir, "Cargo.toml"))) continue;
    const pkg = parseToml(readFileSync(path.join(dir, "Cargo.toml"), "utf8")).package ?? {};
    crates.push({
      ecosystem: "cargo",
      name: unquote(pkg.name ?? dirName),
      version: unquote(pkg.version ?? rootWorkspace),
      description: unquote(pkg.description ?? "") || readmeLine(dir),
      isPrivate: /^publish\s*=\s*false/m.test(readFileSync(path.join(dir, "Cargo.toml"), "utf8")),
      bins: "",
      dir: `crates/${dirName}`,
    });
  }
  const packages = [];
  for (const dirName of subdirs(path.join(root, "packages"))) {
    const dir = path.join(root, "packages", dirName);
    if (!existsSync(path.join(dir, "package.json"))) continue;
    const pkg = readJson(path.join(dir, "package.json"));
    const name = pkg.name ?? dirName;
    packages.push({
      ecosystem: "npm",
      name,
      version: pkg.version ?? "",
      description: pkg.description || readmeLine(dir) || "",
      isPrivate: pkg.private === true,
      bins: Object.keys(pkg.bin ?? {}).join(", "),
      dir: `packages/${dirName}`,
    });
  }
  const versions = readJson(path.join(root, "packages/release/rightkit-versions.json"));
  const catalog = readJson(path.join(root, "packages/release/rightkit-ownership.json")).capabilities ?? [];
  return { crates, packages, versions, catalog, root };
}

// Published = registry-recorded, else the release manifest pin. Staged is never published.
function publishedOf(versions, ecosystem, name) {
  return versionOf(versions.registry?.[ecosystem]?.[name] ?? versions[ecosystem]?.[name]) || "";
}
function stagedOf(versions, ecosystem, name) {
  return versionOf(versions[STAGED_KEY[ecosystem]]?.[name]) || "";
}

function versionFlag(pkg, published, staged) {
  if (!pkg.version) return "MISMATCH no source version";
  if (staged && staged !== pkg.version) return `MISMATCH staged ${staged} != source ${pkg.version}`;
  if (published && published !== pkg.version) {
    return staged === pkg.version ? "staged, not yet published" : `MISMATCH published ${published} != source ${pkg.version}`;
  }
  if (!published) {
    if (pkg.isPrivate) return staged ? `MISMATCH private package has a staged record ${staged}` : "private";
    return staged === pkg.version ? "staged, never published" : "MISMATCH public package has no release record";
  }
  return "ok";
}

// ---------- app repo scan ----------

function* manifestsUnder(dir) {
  let entries;
  try { entries = readdirSync(dir, { withFileTypes: true }); } catch { return; }
  entries.sort((a, b) => cmp(a.name, b.name));
  for (const entry of entries) {
    if (entry.isSymbolicLink()) continue;
    if (entry.isDirectory()) {
      if (SKIP_DIRS.has(entry.name) || entry.name.startsWith(".")) continue;
      yield* manifestsUnder(path.join(dir, entry.name));
    } else if (entry.isFile() && (entry.name === "Cargo.toml" || entry.name === "package.json")) {
      yield path.join(dir, entry.name);
    }
  }
}

function scanApps({ workspace, apps, inventory }) {
  const known = {
    cargo: new Map(inventory.crates.map((c) => [c.name, c])),
    npm: new Map(inventory.packages.map((p) => [p.name, p])),
  };
  const pins = [], vendored = [], scans = [];
  for (const app of apps) {
    const appDir = path.join(workspace, app);
    const scan = { app, exists: existsSync(appDir), manifests: 0, parseErrors: 0, pins: 0 };
    scans.push(scan);
    if (!scan.exists) continue;
    for (const file of manifestsUnder(appDir)) {
      scan.manifests++;
      const rel = path.relative(workspace, file).split(path.sep).join("/");
      const ecosystem = path.basename(file) === "Cargo.toml" ? "cargo" : "npm";
      let ownName = "", ownVersion = "", entries = [];
      try {
        if (ecosystem === "cargo") {
          const text = readFileSync(file, "utf8");
          ownName = unquote(parseToml(text).package?.name ?? "");
          ownVersion = unquote(parseToml(text).package?.version ?? "");
          entries = cargoDependencies(text);
        } else {
          const pkg = readJson(file);
          ownName = pkg.name ?? "";
          ownVersion = pkg.version ?? "";
          entries = npmDependencies(pkg);
        }
      } catch {
        scan.parseErrors++;
        continue;
      }
      if (known[ecosystem].has(ownName)) vendored.push({ app, manifest: rel, ecosystem, name: ownName, version: ownVersion });
      for (const entry of entries) {
        if (!known[entry.ecosystem].has(entry.name)) continue;
        pins.push({ app, manifest: rel, ...entry });
        scan.pins++;
      }
    }
  }
  return { pins, vendored, scans };
}

// ---------- renderers ----------

function indexRows(pkgs, inventory, owned) {
  return pkgs.map((pkg) => {
    const published = publishedOf(inventory.versions, pkg.ecosystem, pkg.name);
    const staged = stagedOf(inventory.versions, pkg.ecosystem, pkg.name);
    const registry = pkg.isPrivate ? "private" : pkg.ecosystem === "cargo" ? "crates.io" : "npm";
    return { pkg, published, staged, registry, flag: versionFlag(pkg, published, staged), owns: owned.get(pkg.name) ?? [] };
  });
}

function renderIndex(inventory) {
  const owned = ownedBy(inventory.catalog);
  const crateRows = indexRows(inventory.crates, inventory, owned);
  const packageRows = indexRows(inventory.packages, inventory, owned);
  const rows = [...crateRows, ...packageRows];
  const mismatches = rows.filter((r) => r.flag.startsWith("MISMATCH")).length;
  const names = new Set([...inventory.crates, ...inventory.packages].map((p) => p.name));
  const orphans = [];
  for (const ecosystem of ["cargo", "npm"]) {
    const keys = new Set([
      ...Object.keys(inventory.versions[ecosystem] ?? {}), ...Object.keys(inventory.versions[STAGED_KEY[ecosystem]] ?? {}),
      ...Object.keys(inventory.versions.registry?.[ecosystem] ?? {}),
    ]);
    for (const key of [...keys].sort(cmp)) if (!names.has(key)) orphans.push(`${key} (${ecosystem})`);
  }
  const versionCells = (r) => [cell(r.pkg.version), cell(r.published || "none recorded"), cell(r.staged),
    r.flag.startsWith("MISMATCH") ? `**${cell(r.flag)}**` : cell(r.flag)];
  return [
    "# RightKit index",
    "",
    "Generated by `node rightkit/scripts/generate-index.mjs`; do not hand-edit. Drift check: `node rightkit/scripts/generate-index.mjs --check`.",
    "",
    "Contracts and boundaries: [README.md](README.md), [docs/RIGHTKIT-SHARED-PACKAGES.md](../../docs/RIGHTKIT-SHARED-PACKAGES.md). Ownership enforcement: `right-release ownership`. Adoption by app repos: [docs/ADOPTION.md](docs/ADOPTION.md). Capability owners: [docs/CAPABILITY-MAP.md](docs/CAPABILITY-MAP.md).",
    "",
    "Version columns: **Source** is the manifest version. **Published** is the latest registry-recorded version, else the release manifest pin. **Staged** is the version prepared for release and not yet published.",
    "",
    "Flags: `ok` means source, published and staged agree. `staged, not yet published` is in-flight work. `private` has no registry record by design. `MISMATCH` needs a release decision.",
    "",
    `Flagged mismatches: ${mismatches}.`,
    "",
    "The build broker is the Rust RustRight product (`bogusyogi/rustright`, binary `rustright`; its old `rightkit` alias is retired); it is not part of this inventory. `packages/build-control-policy` keeps the agent permission hooks.",
    "",
    `## Rust crates (${crateRows.length})`,
    "",
    "| Crate | Registry | Source | Published | Staged | Flag | Owns | What it is |",
    "|---|---|---|---|---|---|---|---|",
    ...crateRows.map((r) => `| \`${r.pkg.name}\` | ${r.registry} | ${versionCells(r).join(" | ")} | ${cell(r.owns.join(", "))} | ${cell(r.pkg.description)} |`),
    "",
    `## npm packages (${packageRows.length})`,
    "",
    "| Package | Registry | Source | Published | Staged | Flag | CLI | Owns | What it is |",
    "|---|---|---|---|---|---|---|---|---|",
    ...packageRows.map((r) => `| \`${r.pkg.name}\` | ${r.registry} | ${versionCells(r).join(" | ")} | ${cell(r.pkg.bins)} | ${cell(r.owns.join(", "))} | ${cell(r.pkg.description)} |`),
    "",
    "## Version records without a manifest",
    "",
    listOrNone(orphans) + ".",
    "",
  ].join("\n");
}

function ownedBy(catalog) {
  const map = new Map();
  for (const entry of catalog) {
    if (!map.has(entry.package)) map.set(entry.package, []);
    map.get(entry.package).push(entry.capability);
  }
  for (const list of map.values()) list.sort(cmp);
  return map;
}

function renderAdoption(inventory, scan) {
  const all = [
    ...inventory.crates.map((p) => ({ ...p, registry: p.isPrivate ? "private" : "crates.io" })),
    ...inventory.packages.map((p) => ({ ...p, registry: p.isPrivate ? "private" : "npm" })),
  ].sort((a, b) => cmp(`${a.ecosystem}:${a.name}`, `${b.ecosystem}:${b.name}`));
  const latestOf = (pkg) => publishedOf(inventory.versions, pkg.ecosystem, pkg.name) || "none recorded";
  const keyOf = (ecosystem, name) => `${ecosystem}:${name}`;
  const pinsBy = new Map();
  for (const pin of scan.pins) {
    const key = keyOf(pin.ecosystem, pin.name);
    if (!pinsBy.has(key)) pinsBy.set(key, []);
    pinsBy.get(key).push({ ...pin, status: pinStatus(pin, publishedOf(inventory.versions, pin.ecosystem, pin.name)) });
  }
  const sortPins = (list) => list.sort((a, b) => cmp(`${a.app}|${a.manifest}|${a.section}`, `${b.app}|${b.manifest}|${b.section}`));
  const vendoredBy = new Map();
  for (const copy of scan.vendored) {
    const key = keyOf(copy.ecosystem, copy.name);
    if (!vendoredBy.has(key)) vendoredBy.set(key, []);
    vendoredBy.get(key).push(copy);
  }
  const count = (list, status) => list.filter((p) => p.status === status).length;
  let splitGroups = 0;
  const splitRows = [];
  for (const [key, list] of [...pinsBy].sort(([a], [b]) => cmp(a, b))) {
    const byApp = new Map();
    for (const pin of list) {
      if (!byApp.has(pin.app)) byApp.set(pin.app, new Set());
      byApp.get(pin.app).add(`${pin.source === "registry" ? "" : `${pin.source} `}${pin.req.trim() || "*"}`);
    }
    for (const [app, reqs] of [...byApp].sort(([a], [b]) => cmp(a, b))) {
      if (reqs.size > 1) {
        splitGroups++;
        const [ecosystem, name] = [key.slice(0, key.indexOf(":")), key.slice(key.indexOf(":") + 1)];
        splitRows.push(`| \`${name}\` | ${ecosystem} | ${cell(app)} | ${cell([...reqs].sort(cmp).join("; "))} |`);
      }
    }
  }
  const allPins = scan.pins.map((pin) => ({ ...pin, status: pinStatus(pin, publishedOf(inventory.versions, pin.ecosystem, pin.name)) }));
  const totals = {
    stale: count(allPins, "stale"), ahead: count(allPins, "ahead"), unpublished: count(allPins, "unpublished"),
    local: count(allPins, "local"), git: count(allPins, "git"), unparsed: count(allPins, "unparsed"),
  };
  const packageRows = all.map((pkg) => {
    const key = keyOf(pkg.ecosystem, pkg.name);
    const list = pinsBy.get(key) ?? [];
    const apps = [...new Set(list.map((p) => p.app))].sort(cmp);
    const copies = vendoredBy.get(key) ?? [];
    const notes = [];
    if (copies.length) notes.push(`${copies.length} vendored cop${copies.length === 1 ? "y" : "ies"}`);
    if (count(list, "stale")) notes.push(`${count(list, "stale")} stale`);
    if (count(list, "ahead")) notes.push(`${count(list, "ahead")} ahead of published`);
    return `| \`${pkg.name}\` | ${pkg.registry} | ${cell(pkg.version)} | ${cell(latestOf(pkg))} | ${apps.length} | ${cell(listOrNone(apps))} | ${cell(notes.join("; ") || "-")} |`;
  });
  const pinRows = [...pinsBy].sort(([a], [b]) => cmp(a, b)).flatMap(([, list]) => sortPins(list))
    .map((p) => `| \`${p.name}\` | ${cell(p.app)} | \`${cell(p.manifest)}\` | ${p.section} | \`${cell(p.req || "*")}\` | ${cell(publishedOf(inventory.versions, p.ecosystem, p.name) || "none recorded")} | ${p.status} |`);
  const copyRows = [...vendoredBy].sort(([a], [b]) => cmp(a, b)).flatMap(([, list]) => list)
    .map((c) => `| \`${c.name}\` | ${cell(c.app)} | \`${cell(c.manifest)}\` | ${cell(c.version)} | ${cell(publishedOf(inventory.versions, c.ecosystem, c.name) || "none recorded")} |`);
  const scanRows = scan.scans.map((s) => `| ${cell(s.app)} | ${s.exists ? "yes" : "**missing**"} | ${s.manifests} | ${s.parseErrors} | ${s.pins} |`);
  return [
    "# RightKit adoption by app repos",
    "",
    "Generated by `node rightkit/scripts/generate-index.mjs`; do not hand-edit. Drift check: `node rightkit/scripts/generate-index.mjs --check`.",
    "",
    "Scope: every `Cargo.toml` and `package.json` under each app repo in `/Volumes/D/claude`. Directories named `node_modules`, `target` and `.cache`, all dot-directories (`.git`, `.claude`, `.worktrees` and similar) and symlinks are skipped, so worktree copies do not count as consumers. Lock files are not read, so a resolved version can differ from the pin shown here. Workspace-inherited Cargo entries (`workspace = true`) are not counted; the pin in the workspace root is.",
    "",
    "Status values compare each pin with the latest published version (registry-recorded, else the release manifest pin):",
    "",
    "- `current`: exact pin equals the latest published version.",
    "- `range ok`: a range pin admits the latest published version.",
    "- `stale`: the pin admits no version at or after the latest published version.",
    "- `ahead`: the pin names a version newer than the latest published version (unreleased or staged).",
    "- `unpublished`: no published version is recorded for the package.",
    "- `local`: path, workspace or file dependency. `git`: git dependency. `unparsed`: requirement not understood.",
    "",
    "## Summary",
    "",
    `- Packages in RightKit: ${all.length} (${inventory.crates.length} crates, ${inventory.packages.length} npm packages).`,
    `- Packages with at least one app pin: ${[...pinsBy.keys()].length}.`,
    `- App pins: ${allPins.length}. Stale: ${totals.stale}. Ahead of published: ${totals.ahead}. Unpublished: ${totals.unpublished}. Local: ${totals.local}. Git: ${totals.git}. Unparsed: ${totals.unparsed}.`,
    `- Vendored copies of RightKit packages inside app repos: ${scan.vendored.length}.`,
    `- App and package pairs with more than one requirement: ${splitGroups}.`,
    "",
    "## Packages",
    "",
    "| Package | Registry | Source | Latest published | Apps | App names | Notes |",
    "|---|---|---|---|---|---|---|",
    ...packageRows,
    "",
    "## Pins",
    "",
    "| Package | App | Manifest | Section | Requirement | Latest published | Status |",
    "|---|---|---|---|---|---|---|",
    ...pinRows,
    "",
    "## Duplicate copies and split pins",
    "",
    "Vendored copies are app-owned source trees that carry a RightKit package name. They are not registry dependencies.",
    "",
    ...(copyRows.length ? ["| Package | App | Manifest | Version in tree | Latest published |", "|---|---|---|---|---|", ...copyRows] : ["None."]),
    "",
    "Split pins: one app names the same package with different requirements, possibly in different manifests.",
    "",
    ...(splitRows.length ? ["| Package | Ecosystem | App | Requirements |", "|---|---|---|---|", ...splitRows] : ["None."]),
    "",
    "## Scan scope",
    "",
    "| App | Present | Manifests read | Parse errors | RightKit pins matched |",
    "|---|---|---|---|---|",
    ...scanRows,
    "",
  ].join("\n");
}

// Ecosystem comes from the manifests, not from the name prefix: npm scopes other than @rightkit are npm too.
function availabilityOf(owner, inventory) {
  const ecosystem = inventory.crates.some((c) => c.name === owner) ? "cargo"
    : inventory.packages.some((p) => p.name === owner) ? "npm"
      : owner === "RightKitSwift" || owner === "rightkit-swift" ? "swift" : null;
  if (!ecosystem) return `${owner}: none recorded`;
  const key = ecosystem === "swift" ? "rightkit-swift" : owner;
  const published = versionOf(inventory.versions.registry?.[ecosystem]?.[key] ?? inventory.versions[ecosystem]?.[key]);
  const staged = versionOf(inventory.versions[STAGED_KEY[ecosystem]]?.[key]);
  const registry = { cargo: "crates.io", npm: "npm", swift: "SwiftPM" }[ecosystem];
  if (published) return `${owner}: published ${published} (${registry})`;
  if (staged) return `${owner}: staged ${staged}, not published`;
  return `${owner}: none recorded`;
}

function renderCapabilityMap(inventory) {
  const byCapability = new Map();
  for (const entry of inventory.catalog) {
    if (!byCapability.has(entry.capability)) byCapability.set(entry.capability, []);
    byCapability.get(entry.capability).push(entry);
  }
  const rows = [...byCapability].sort(([a], [b]) => cmp(a, b)).map(([capability, entries]) => {
    const owners = [...new Set(entries.map((e) => e.package))].sort(cmp);
    const fingerprints = entries.flatMap((e) => e.fingerprints ?? []);
    const blocks = fingerprints.filter((f) => f.tier === "block").length;
    const advises = fingerprints.filter((f) => f.tier === "advise").length;
    const tier = blocks ? "block" : advises ? "advise" : "none";
    const detection = [...new Set(entries.map((e) => ownershipDetection(e)))].sort(cmp).join(", ");
    const availability = owners.map((owner) => availabilityOf(owner, inventory));
    const fingerprintCell = fingerprints.length ? `${fingerprints.length} (${blocks} block, ${advises} advise)` : "0";
    return `| ${cell(capability)} | ${cell(owners.map((o) => `\`${o}\``).join(", "))} | ${tier} | ${detection} | ${fingerprintCell} | ${cell(availability.join("; "))} |`;
  });
  const fingerprinted = [...byCapability.values()].filter((list) => list.some((e) => ownershipDetection(e) === "fingerprinted")).length;
  return [
    "# RightKit capability map",
    "",
    "Generated by `node rightkit/scripts/generate-index.mjs`; do not hand-edit. Drift check: `node rightkit/scripts/generate-index.mjs --check`.",
    "",
    "Owners come from `packages/release/rightkit-ownership.json`, which the ownership generator builds from each package's declaration. Published availability comes from `packages/release/rightkit-versions.json`. Capability lookup is `rustright owns <capability>`.",
    "",
    "- Tier: `block` when any fingerprint blocks, `advise` when fingerprints only advise, `none` when there are no fingerprints.",
    "- Detection: `fingerprinted` when the owner declares dependency or source fingerprints; `declaration-only` when it declares ownership without a precise fingerprint. A declared dependency alone does not prove integration.",
    "- Published availability: `published` means a registry version is recorded. `staged` means a version is prepared and unverified on the registry.",
    "",
    `Capabilities: ${byCapability.size} (${fingerprinted} with fingerprints, ${byCapability.size - fingerprinted} declaration-only). Internal owners that are not application capabilities are excluded.`,
    "",
    "| Capability | Owner | Tier | Detection | Fingerprints | Published availability |",
    "|---|---|---|---|---|---|",
    ...rows,
    "",
  ].join("\n");
}

// ---------- entry points ----------

// Repo-local outputs depend only on this repository's manifests, versions and catalog.
export function generateLocal({ root = DEFAULT_ROOT } = {}) {
  const inventory = loadInventory(root);
  return {
    "INDEX.md": renderIndex(inventory),
    "docs/CAPABILITY-MAP.md": renderCapabilityMap(inventory),
  };
}

// Adoption output also reads the app repos under the workspace root, so it is checked separately.
export function generateAdoption({ root = DEFAULT_ROOT, workspace = DEFAULT_WORKSPACE, apps = APP_REPOS } = {}) {
  const inventory = loadInventory(root);
  const scan = scanApps({ workspace, apps, inventory });
  return { "docs/ADOPTION.md": renderAdoption(inventory, scan) };
}

export function generate(options = {}) {
  return { ...generateLocal(options), ...generateAdoption(options) };
}

export function driftOf(root, outputs) {
  return Object.keys(outputs).filter((rel) => {
    const file = path.join(root, rel);
    // A Windows checkout may hold the committed LF file with CRLF: that is not drift.
    return !existsSync(file) || readFileSync(file, "utf8").replace(/\r\n/g, "\n") !== outputs[rel];
  });
}

function parseArgs(argv) {
  const options = { mode: "write", root: DEFAULT_ROOT, workspace: DEFAULT_WORKSPACE };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--check") options.mode = "check";
    else if (argv[i] === "--check-adoption") options.mode = "check-adoption";
    else if (argv[i] === "--root") options.root = path.resolve(argv[++i]);
    else if (argv[i] === "--workspace") options.workspace = path.resolve(argv[++i]);
    else throw new Error(`unknown argument: ${argv[i]}`);
  }
  return options;
}

function reportDrift(root, outputs, hint) {
  const drift = driftOf(root, outputs);
  if (drift.length) {
    for (const rel of drift) console.error(`DRIFT ${rel}: committed file differs from the generated output`);
    console.error(hint);
    return 1;
  }
  console.log(`generated docs are current (${Object.keys(outputs).join(", ")})`);
  return 0;
}

export function main(argv) {
  const options = parseArgs(argv);
  // --check reads only this repository (no app repos), so it is deterministic.
  if (options.mode === "check") {
    return reportDrift(options.root, generateLocal(options), "Run: node rightkit/scripts/generate-index.mjs");
  }
  // --check-adoption reads the app repos; run it from the doctor or the inventory check, not from tests.
  if (options.mode === "check-adoption") {
    return reportDrift(options.root, generateAdoption(options), "Run: node rightkit/scripts/generate-index.mjs (regenerates all three files)");
  }
  for (const [rel, content] of Object.entries(generate(options))) {
    mkdirSync(path.dirname(path.join(options.root, rel)), { recursive: true });
    writeFileSync(path.join(options.root, rel), content);
    console.log(`${rel}: ${content.split("\n").length} lines`);
  }
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = main(process.argv.slice(2));
}
