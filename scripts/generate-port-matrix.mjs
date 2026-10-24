// RightKit port matrix: which RightKit package is where, per app, and how far each app has ported.
// Written by scripts/generate-index.mjs next to ADOPTION.md; never hand-edit the outputs.
//   docs/PORT-MATRIX.md     readable tables: stamp, per-app summary, one matrix per domain, Apple targets, RK-ready and to-move
//   docs/port-matrix.html   the same data as one self-contained page (inline CSS and JS, no network), with filters and progress bars
//   docs/port-matrix.json   the data both are rendered from
// Inputs (all read-only): the RightKit inventory (crates, npm packages, RightKitSwift), packages/release/rightkit-versions.json,
// packages/release/ownership-mandatory-set.json (the foundation rows), packages/release/rightkit-ownership.json and
// ownership-catalog-config.json (capability names), each app repo (manifests, rightkit-mandatory.json,
// .rightkit/ownership-exceptions.json, Apple project files, release flags), and an optional triage file (--triage).
// Parsers and the pin-status computation come from generate-index.mjs; this file adds no second manifest scanner.
//
// Volatile fields (the generation stamp, each app's directory and HEAD, and the per-app check counts) do not decide drift:
// --check-adoption regenerates with those fields carried over from the committed docs/port-matrix.json, so only a change in
// the data itself (pins, exceptions, rulings, versions, triage) reads as drift. --check needs no app repos: it verifies that
// the committed MD and HTML are exactly the rendering of the committed JSON.
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync, realpathSync, statSync } from "node:fs";
import path from "node:path";
import {
  DEFAULT_WORKSPACE, cmp, loadInventory, manifestsUnder, parseToml, partsOf, pinStatus, publishedOf, readJson,
  readManifestEntries, satisfies, stagedOf, unquote,
} from "./generate-index.mjs";

export const PORT_MATRIX_MD = "docs/PORT-MATRIX.md";
export const PORT_MATRIX_HTML = "docs/port-matrix.html";
export const PORT_MATRIX_JSON = "docs/port-matrix.json";
export const PORT_MATRIX_OUTPUTS = [PORT_MATRIX_MD, PORT_MATRIX_HTML, PORT_MATRIX_JSON];
export const MANDATORY_SET_FILE = "packages/release/ownership-mandatory-set.json";
export const CATALOG_CONFIG_FILE = "packages/release/ownership-catalog-config.json";
export const SCHEMA = 1;

// Apps that are not in the mandatory set's defaultApps but are part of the matrix.
export const EXTRA_APPS = [
  { name: "sellright", group: "release consumer" },
  { name: "legion", group: "tool consumer" },
];
const GROUP_MANDATORY = "mandatory-set app";
const GROUP_TOOL = "tool consumer";

// Cell states. These tokens and no others appear in a cell.
export const STATES = [
  { state: "adopted", meaning: "Exact pin equal to the latest published version." },
  { state: "range", meaning: "Range pin that admits the latest published version." },
  { state: "stale", meaning: "Pin behind the latest published version, shown as pinned→latest." },
  { state: "ahead", meaning: "Pin newer than the latest published version (staged or unreleased), or no published version is recorded." },
  { state: "local", meaning: "Path, workspace or git dependency, or a requirement that cannot be resolved to a registry version." },
  { state: "missing", meaning: "Foundation owner required for the app's profile, with no pin and no N/A ruling." },
  { state: "n/a", meaning: "Ruled not applicable for this app; the mandatory-set row id is shown." },
  { state: "exception", meaning: "No pin, and an ownership exception names this package or its capability; issue and expiry are shown and EXPIRED or a missing issue or expiry is flagged." },
  { state: "unused", meaning: "Add-on with no pin." },
  { state: "not present", meaning: "The app directory does not exist on this machine." },
];
export const CLASSES = [
  { class: "foundation", meaning: "An owner in a mandatory-set row for the app's profile. Every app of that profile owes it unless a row is ruled not applicable." },
  { class: "add-on", meaning: "Any other published RightKit package." },
  { class: "tool", meaning: "Private or unpublished, or consumed only by non-app repos (legion, build tooling)." },
];

// Domain grouping. rightkit-ownership.json and ownership-catalog-config.json name capabilities but no domains, so the
// grouping lives here. A package that is not listed falls into "Other"; the generator prints that list.
export const DOMAINS = [
  { id: "shell-ui", name: "Shell, UI and app frame", packages: [
    "rightkit-tauri-shell", "@rightkit/tauri", "@rightkit/tauri-shell", "@rightkit/react-shell", "@rightkit/platform-ui", "@rightkit/theme",
    "@rightkit/settings-ui", "@rightkit/onboarding", "@rightkit/legal-ui", "@rightkit/media-ui", "@rightkit/pdf-ui", "@rightkit/doc-render",
    "@rightkit/i18n", "rightkit-overlay", "rightkit-overlay-win", "rightkit-hotkeys"] },
  { id: "processes", name: "Processes, jobs and local services", packages: [
    "rightkit-job-queue", "rightkit-child-process", "rightkit-sidecar", "rightkit-daemon", "rightkit-sandbox"] },
  { id: "storage", name: "Storage, files and core utilities", packages: [
    "rightkit-fs", "rightkit-db", "rightkit-app-settings", "rightkit-cache", "rightkit-file-watch", "rightkit-fsindex", "rightkit-search",
    "rightkit-channel-memory", "rightkit-time"] },
  { id: "identity", name: "Licensing, identity and security", packages: [
    "rightkit-license", "@rightkit/license", "@rightkit/legal", "rightkit-device", "rightkit-identity", "rightkit-google-identity",
    "rightkit-oauth", "rightkit-credentials", "rightkit-crypto", "rightkit-vault", "rightkit-sync-keys"] },
  { id: "diagnostics", name: "Diagnostics and updates", packages: [
    "rightkit-logs", "@rightkit/logs", "rightkit-crash", "@rightkit/app-update"] },
  { id: "network", name: "Network and sync", packages: [
    "rightkit-http", "rightkit-sync", "@rightkit/sync-worker", "rightkit-send"] },
  { id: "media", name: "Media and speech", packages: [
    "rightkit-audio", "rightkit-asr", "rightkit-tts", "rightkit-tts-qwen", "rightkit-tts-qwen-sys", "rightkit-vad", "rightkit-ffmpeg",
    "rightkit-playback", "rightkit-record", "rightkit-ocr", "rightkit-speech-client", "rightkit-align"] },
  { id: "ai", name: "Models and AI", packages: [
    "rightkit-onnx", "rightkit-models", "rightkit-llm", "rightkit-embed", "rightkit-spend"] },
  { id: "content", name: "Documents and content", packages: [
    "rightkit-docs", "rightkit-pdf", "rightkit-typeset", "rightkit-review", "rightkit-contracts", "rightkit-compose", "rightkit-transcript",
    "rightkit-motion"] },
  { id: "agents", name: "Agents and automation", packages: [
    "rightkit-agent-tools", "rightkit-agents", "rightkit-automation", "rightkit-browser", "rightkit-qa-driver", "rightkit-mcp",
    "rightkit-workflow", "rightkit-hooks", "@rightkit/hooks"] },
  { id: "qa", name: "QA", packages: ["rightkit-qa", "@rightkit/qa", "@rightkit/qa-chrome", "@rightkit/ax"] },
  { id: "release", name: "Release and build tooling", packages: [
    "@rightkit/release", "@rightkit/ci", "@rightkit/issues-tracker", "@rightkit/outdated", "rightkit-ownership", "rightkit-platform-rules",
    "@rightkit/build-control-policy"] },
  { id: "apple", name: "Apple and mobile", packages: ["RightKitSwift", "rightkit-android"] },
];
const OTHER_DOMAIN = { id: "other", name: "Other (no domain assigned)" };

// ---------- small helpers ----------

const posix = (from, to) => path.relative(from, to).split(path.sep).join("/");
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const str = (value) => (typeof value === "string" ? value.trim() : "");
const uniqSorted = (list) => [...new Set(list)].sort(cmp);
const keyOf = (ecosystem, name) => `${ecosystem}:${name}`;
const plural = (n, one, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
const readText = (file) => { try { return readFileSync(file, "utf8"); } catch { return null; } };
const readJsonSafe = (file) => { try { return JSON.parse(readFileSync(file, "utf8")); } catch { return null; } };
const isDirectory = (dir) => { try { return statSync(dir).isDirectory(); } catch { return false; } };

// Pin statuses from generate-index.mjs onto the cell tokens. "unpublished" (no published version recorded) reads as ahead;
// "git" and "unparsed" read as local.
const TOKEN_OF_STATUS = {
  current: "adopted", "range ok": "range", stale: "stale", ahead: "ahead", local: "local", git: "local", unparsed: "local", unpublished: "ahead",
};
// Worst first: the cell takes the worst token among an app's pins of one package.
const SEVERITY = ["stale", "local", "ahead", "range", "adopted"];
const CURRENT_TOKENS = new Set(["adopted", "range", "ahead"]);
const DEV_SECTION = /dev/i;

// ---------- workspace and app directories ----------

// An explicit workspace is used as given. Otherwise DEFAULT_WORKSPACE when it exists, else the directory that holds the
// RightKit checkout (the apps are its siblings).
export function resolveWorkspace(requested, root, defaultWorkspace = DEFAULT_WORKSPACE) {
  if (requested) return path.resolve(requested);
  if (isDirectory(defaultWorkspace)) return defaultWorkspace;
  return path.dirname(path.resolve(root));
}

// "--app-dir ember=/path/to/ember" -> { ember: "/path/to/ember" }
export function parseAppDirArg(text) {
  const at = String(text ?? "").indexOf("=");
  if (at < 1 || at === text.length - 1) throw new Error(`--app-dir expects <app>=<directory>, got: ${text}`);
  return { [text.slice(0, at)]: path.resolve(text.slice(at + 1)) };
}

// Directory of each app: an explicit --app-dir, else <workspace>/<app>; when that does not exist and the committed port-matrix.json
// (carried) names a directory that does, that one.
function resolveApps({ data, workspace, appDirs = {}, extraApps = EXTRA_APPS, carried = null }) {
  const apps = [];
  const seen = new Set();
  const add = (name, group) => {
    if (seen.has(name)) return;
    seen.add(name);
    let dir = appDirs[name] ? path.resolve(appDirs[name]) : path.join(workspace, name);
    const recorded = carried?.apps?.[name]?.dir;
    if (!appDirs[name] && !isDirectory(dir) && typeof recorded === "string" && isDirectory(recorded)) dir = recorded;
    apps.push({ name, group, dir });
  };
  for (const name of [...(data.defaultApps ?? [])].sort(cmp)) add(name, GROUP_MANDATORY);
  for (const extra of extraApps) add(extra.name, extra.group);
  for (const name of Object.keys(appDirs).sort(cmp)) add(name, "other app");
  return apps;
}

// ---------- git ----------

function git(dir, args) {
  const result = spawnSync("git", ["-C", dir, ...args], { encoding: "utf8", env: { ...process.env, GIT_OPTIONAL_LOCKS: "0" } });
  return result.status === 0 ? result.stdout.trim() : null;
}
const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };

// Short HEAD of the repository rooted at `dir`; null when `dir` is not itself a git repository root with a commit.
export function gitHead(dir) {
  const top = git(dir, ["rev-parse", "--show-toplevel"]);
  if (!top || realOrSelf(top) !== realOrSelf(dir)) return null;
  return git(dir, ["rev-parse", "--short=7", "HEAD"]) || null;
}

function hookState(dir) {
  const rel = ".githooks/pre-push";
  if (!existsSync(path.join(dir, rel))) return "absent";
  if (!gitHead(dir)) return "present";
  return git(dir, ["ls-files", "--error-unmatch", "--", rel]) === null ? "uncommitted" : "committed";
}

// ---------- directory walks (RK-ready packages and Apple targets) ----------

const WALK_SKIP = new Set(["node_modules", "target", "vendor", "dist", "build", "pods", "deriveddata"]);
const TEST_SEGMENT = /^(tests?|__tests__|__fixtures__|fixtures?|e2e)$/i;
const isTestOnly = (rel) => rel.split("/").slice(0, -1).some((segment) => TEST_SEGMENT.test(segment));

function walkFiles(root, wanted) {
  const found = [];
  const visit = (dir, rel, depth) => {
    if (depth > 12) return;
    let entries;
    try { entries = readdirSync(dir, { withFileTypes: true }); } catch { return; }
    entries.sort((a, b) => cmp(a.name, b.name));
    for (const entry of entries) {
      if (entry.isSymbolicLink()) continue;
      const next = rel ? `${rel}/${entry.name}` : entry.name;
      if (entry.isDirectory()) {
        if (entry.name.startsWith(".") || WALK_SKIP.has(entry.name.toLowerCase())) continue;
        visit(path.join(dir, entry.name), next, depth + 1);
      } else if (entry.isFile() && wanted.has(entry.name)) {
        found.push({ abs: path.join(dir, entry.name), rel: next });
      }
    }
  };
  visit(root, "", 0);
  return found;
}

// ---------- the mandatory set ----------

function loadMandatorySet(root) {
  const data = readJsonSafe(path.join(root, MANDATORY_SET_FILE));
  return {
    defaultProfile: "desktop", defaultApps: [], appDeclarationFile: "rightkit-mandatory.json", profiles: {}, rows: [], pendingRows: [],
    appAmendments: {}, ...(isObject(data) ? data : {}),
  };
}

function profileRows(data, profile) {
  if (!profile || data.profiles?.[profile]?.ratified === false) return [];
  return (data.rows ?? []).filter((row) => row.profile === profile).map((row) => ({
    id: row.id, name: row.name ?? "", owners: (row.owners ?? []).map((o) => ({ name: o.name, ecosystem: o.ecosystem, allowDev: o.allowDev === true })),
  }));
}

// <app>/rightkit-mandatory.json, read the way ownership-mandatory-set.mjs reads it: a profile, and notApplicable rulings that carry
// a reason of at least 10 characters (shorter ones are ignored).
function loadDeclaration(appDir, data) {
  const file = path.join(appDir, data.appDeclarationFile);
  const base = { present: false, profile: data.defaultProfile, notApplicable: {}, ignored: [], error: "" };
  if (!existsSync(file)) return base;
  const doc = readJsonSafe(file);
  if (!isObject(doc)) return { ...base, present: true, error: `${data.appDeclarationFile} is not valid JSON` };
  const notApplicable = {};
  const ignored = [];
  for (const [id, reason] of Object.entries(isObject(doc.notApplicable) ? doc.notApplicable : {})) {
    if (typeof reason === "string" && reason.trim().length >= 10) notApplicable[id] = reason.trim();
    else ignored.push(id);
  }
  return { present: true, profile: str(doc.profile) || data.defaultProfile, notApplicable, ignored: ignored.sort(cmp), error: "" };
}

// ---------- ownership exceptions ----------

const ISO_DAY = /^\d{4}-\d{2}-\d{2}/;
const ISSUE_LIKE = /#\d+|https?:\/\//;

function normaliseException(raw) {
  const issue = str(raw.issue) || str(raw.trackingRef?.id) || str(raw.trackingRef?.url) || (ISSUE_LIKE.test(str(raw.tracking)) ? str(raw.tracking) : "");
  const expiresText = str(raw.expires) || str(raw.expiry) || str(raw.expiresAt);
  const expires = ISO_DAY.test(expiresText) ? expiresText.slice(0, 10) : "";
  return {
    package: str(raw.package), capability: str(raw.capability), issue, expires, invalidExpiry: Boolean(expiresText) && !expires,
  };
}

// Entries are read tolerantly: the file may be a bare array, a legacy {schema, generated, exceptions} object, or {exceptions};
// any field may be missing.
function loadExceptions(appDir) {
  const file = path.join(appDir, ".rightkit", "ownership-exceptions.json");
  if (!existsSync(file)) return { present: false, error: "", entries: [] };
  const doc = readJsonSafe(file);
  const list = Array.isArray(doc) ? doc : Array.isArray(doc?.exceptions) ? doc.exceptions : null;
  if (!list) return { present: true, error: "ownership-exceptions.json is not valid JSON or has no exceptions list", entries: [] };
  return { present: true, error: "", entries: list.filter(isObject).map(normaliseException) };
}

function exceptionSummary(entries, today, matched) {
  const summary = { total: entries.length, open: 0, expired: 0, withoutIssueOrExpiry: 0, unmatched: 0 };
  for (const entry of entries) {
    const expired = entry.expires !== "" && entry.expires < today;
    if (expired) summary.expired++; else summary.open++;
    if (!entry.issue || !entry.expires) summary.withoutIssueOrExpiry++;
    if (!matched.has(entry)) summary.unmatched++;
  }
  return summary;
}

// One package's exceptions in one app: counts, flags and the text a cell shows.
function exceptionOverlay(entries, today) {
  const issues = uniqSorted(entries.map((e) => e.issue).filter(Boolean));
  const expiries = entries.map((e) => e.expires).filter(Boolean).sort(cmp);
  const expired = entries.filter((e) => e.expires && e.expires < today).length;
  const withoutIssue = entries.filter((e) => !e.issue).length;
  const withoutExpiry = entries.filter((e) => !e.expires).length;
  const issueText = !issues.length ? "no issue" : `${issues.slice(0, 2).join("/")}${issues.length > 2 ? "+" : ""}${withoutIssue ? ` (${withoutIssue} without)` : ""}`;
  const expiryText = expired ? `EXPIRED ${expiries[0]}${expired < entries.length ? ` (${expired} of ${entries.length})` : ""}`
    : !expiries.length ? "no expiry" : `expires ${expiries[0]}${withoutExpiry ? ` (${withoutExpiry} without)` : ""}`;
  return {
    count: entries.length, expired, withoutIssue, withoutExpiry, issues, earliestExpiry: expiries[0] ?? "",
    text: `${plural(entries.length, "exception")}: ${issueText}, ${expiryText}`,
  };
}

// ---------- Apple targets ----------

const RIGHTKIT_URL = /github\.com[/:][^/\s]+\/rightkit(?:-swift)?(?:\.git)?\/?$/i;
const RIGHTKIT_PATH = /^(rightkit|rightkitswift|rightkit-swift)$/i;
const RIGHTKIT_KEY = /^(rightkit|rightkitswift)$/i;
const referencesRightKit = ({ key = "", url = "", pathValue = "" }) => RIGHTKIT_URL.test(url)
  || (pathValue !== "" && RIGHTKIT_PATH.test(path.basename(pathValue.replace(/\/+$/, ""))))
  || (url === "" && pathValue === "" && RIGHTKIT_KEY.test(key));

// attrs use the names of either XcodeGen's project.yml or Package.swift; kind is tag, branch, path, revision or unknown.
function applePin({ url = "", path: pathValue = "", exact = "", from = "", branch = "", revision = "" }) {
  if (pathValue) return { kind: "path", version: "", ref: pathValue, url, requirement: `path ${pathValue}` };
  if (branch) return { kind: "branch", version: "", ref: branch, url, requirement: `branch ${branch}` };
  if (revision) return { kind: "revision", version: "", ref: revision, url, requirement: `revision ${revision.slice(0, 12)}` };
  if (exact) return { kind: "tag", version: exact, ref: exact, url, requirement: `exact ${exact}`, exactPin: true };
  if (from) return { kind: "tag", version: from, ref: from, url, requirement: `from ${from}`, exactPin: false };
  return { kind: "unknown", version: "", ref: "", url, requirement: "no recognised requirement" };
}

const unquoteYaml = (value) => String(value ?? "").replace(/\s+#.*$/, "").trim().replace(/^["']|["']$/g, "");

// XcodeGen project.yml: the top-level `packages:` map, and the `package:`/`product:` pairs of the targets' dependencies.
export function parseXcodegen(text) {
  const lines = String(text).split(/\r?\n/);
  const start = lines.findIndex((line) => /^packages:\s*(?:#.*)?$/.test(line));
  const packages = [];
  if (start >= 0) {
    let baseIndent = -1, current = null;
    for (let i = start + 1; i < lines.length; i++) {
      const line = lines[i];
      if (!line.trim() || /^\s*#/.test(line)) continue;
      const indent = line.length - line.trimStart().length;
      if (indent === 0) break;
      if (baseIndent < 0) baseIndent = indent;
      if (indent === baseIndent) {
        const match = /^\s*([A-Za-z0-9_.-]+):\s*(.*)$/.exec(line);
        if (!match) continue;
        current = { key: match[1], attrs: {} };
        packages.push(current);
        const inline = /^\{(.*)\}\s*(?:#.*)?$/.exec(match[2].trim());
        if (inline) for (const part of inline[1].split(",")) {
          const kv = /^\s*([A-Za-z]+):\s*(.*)$/.exec(part);
          if (kv) current.attrs[kv[1]] = unquoteYaml(kv[2]);
        }
      } else if (current) {
        const kv = /^\s*([A-Za-z]+):\s*(.*)$/.exec(line);
        if (kv) current.attrs[kv[1]] = unquoteYaml(kv[2]);
      }
    }
  }
  const products = new Map();
  const pairs = /package:\s*["']?([A-Za-z0-9_.-]+)["']?\s*,?\s*(?:\r?\n\s*)?product:\s*["']?([A-Za-z0-9_.-]+)/g;
  for (const match of String(text).matchAll(pairs)) {
    if (!products.has(match[1])) products.set(match[1], new Set());
    products.get(match[1]).add(match[2]);
  }
  const name = /^name:\s*(.+)$/m.exec(text);
  return { name: name ? unquoteYaml(name[1]) : "", packages, products };
}

function stripSwiftComments(text) {
  let out = "", inString = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (inString) {
      out += ch;
      if (ch === "\\") { out += text[i + 1] ?? ""; i++; } else if (ch === '"') inString = false;
      continue;
    }
    if (ch === '"') { inString = true; out += ch; continue; }
    if (ch === "/" && text[i + 1] === "/") { while (i < text.length && text[i] !== "\n") i++; out += "\n"; continue; }
    if (ch === "/" && text[i + 1] === "*") { i += 2; while (i < text.length && !(text[i] === "*" && text[i + 1] === "/")) i++; i++; continue; }
    out += ch;
  }
  return out;
}

// Package.swift: every .package(...) call, and the .product(name:, package:) uses.
export function parseSwiftPackage(text) {
  const source = stripSwiftComments(String(text));
  const calls = [];
  const opener = /\.package\s*\(/g;
  let match;
  while ((match = opener.exec(source))) {
    let depth = 1, inString = false, i = opener.lastIndex;
    for (; i < source.length && depth > 0; i++) {
      const ch = source[i];
      if (inString) { if (ch === "\\") i++; else if (ch === '"') inString = false; continue; }
      if (ch === '"') inString = true; else if (ch === "(") depth++; else if (ch === ")") depth--;
    }
    calls.push(source.slice(opener.lastIndex, i - 1));
    opener.lastIndex = i;
  }
  const argument = (inner, name) => new RegExp(`${name}:\\s*"([^"]*)"`).exec(inner)?.[1] ?? "";
  const packages = calls.map((inner) => {
    const url = argument(inner, "url");
    const pathValue = argument(inner, "path");
    const range = /"([^"]+)"\s*\.\.[<.]\s*"[^"]+"/.exec(inner);
    const exact = argument(inner, "exact") || /\.exact\(\s*"([^"]+)"/.exec(inner)?.[1] || "";
    const identity = (url || pathValue).replace(/\/+$/, "").replace(/\.git$/, "").split("/").pop().toLowerCase();
    return { url, pathValue, identity, attrs: { url, path: pathValue, exact, from: argument(inner, "from") || range?.[1] || "", branch: argument(inner, "branch"), revision: argument(inner, "revision") } };
  });
  const uses = [...source.matchAll(/\.product\s*\(\s*name:\s*"([^"]+)"\s*,\s*package:\s*"([^"]+)"/g)].map((m) => ({ product: m[1], package: m[2] }));
  const name = /\bPackage\s*\(\s*name:\s*"([^"]+)"/.exec(source)?.[1] ?? "";
  return { name, packages, uses };
}

function compareVersionText(a, b) {
  const x = partsOf(a) ?? [0], y = partsOf(b) ?? [0];
  for (let i = 0; i < 3; i++) { const d = (x[i] ?? 0) - (y[i] ?? 0); if (d) return d < 0 ? -1 : 1; }
  return 0;
}

// The cell state of one Apple pin against the latest recorded RightKitSwift version.
export function applePinState(pin, latest) {
  if (pin.kind === "path") return { state: "local", detail: `path ${pin.ref}` };
  if (pin.kind === "branch") return { state: "local", detail: `branch ${pin.ref}` };
  if (pin.kind === "revision") return { state: "local", detail: `revision ${pin.ref.slice(0, 12)}` };
  if (pin.kind !== "tag") return { state: "local", detail: pin.requirement };
  if (!latest) return { state: "ahead", detail: `${pin.version} (no version recorded)` };
  const order = compareVersionText(pin.version, latest);
  if (pin.exactPin) {
    if (order === 0) return { state: "adopted", detail: `=${pin.version}` };
    return order < 0 ? { state: "stale", detail: `${pin.version}→${latest}` } : { state: "ahead", detail: `${pin.version} > ${latest}` };
  }
  const upper = `${(partsOf(pin.version) ?? [0])[0] + 1}.0.0`;
  if (order > 0) return { state: "ahead", detail: `from ${pin.version} > ${latest}` };
  if (compareVersionText(latest, upper) < 0) return { state: "range", detail: `from ${pin.version}` };
  return { state: "stale", detail: `from ${pin.version}→${latest}` };
}

// Every project.yml and Package.swift under the app (test-only paths excluded) that references RightKitSwift.
function scanAppleTargets(appDir, swiftLatest) {
  const targets = [];
  for (const file of walkFiles(appDir, new Set(["project.yml", "Package.swift"]))) {
    if (isTestOnly(file.rel)) continue;
    const text = readText(file.abs);
    if (text === null) continue;
    let target = null;
    if (file.rel.endsWith("project.yml")) {
      const parsed = parseXcodegen(text);
      const reference = parsed.packages.find((p) => referencesRightKit({ key: p.key, url: p.attrs.url ?? p.attrs.github ?? "", pathValue: p.attrs.path ?? "" }));
      if (reference) {
        const a = reference.attrs;
        target = {
          system: "xcodegen", name: parsed.name || path.posix.dirname(file.rel),
          pin: applePin({ url: a.url ?? a.github ?? "", path: a.path, exact: a.exactVersion ?? a.version ?? "", from: a.from ?? a.minVersion ?? a.majorVersion ?? a.minorVersion ?? "", branch: a.branch, revision: a.revision }),
          products: [...(parsed.products.get(reference.key) ?? [])],
        };
      }
    } else {
      const parsed = parseSwiftPackage(text);
      const reference = parsed.packages.find((p) => referencesRightKit({ url: p.url, pathValue: p.pathValue }));
      if (reference) {
        target = {
          system: "swiftpm", name: parsed.name || path.posix.dirname(file.rel),
          pin: applePin(reference.attrs),
          products: parsed.uses.filter((u) => u.package.toLowerCase() === reference.identity || RIGHTKIT_KEY.test(u.package)).map((u) => u.product),
        };
      }
    }
    if (!target) continue;
    const { state, detail } = applePinState(target.pin, swiftLatest);
    targets.push({
      file: file.rel, system: target.system, name: target.name, pinKind: target.pin.kind, pinVersion: target.pin.version,
      pinRef: target.pin.kind === "tag" ? "" : target.pin.ref, requirement: target.pin.requirement,
      products: uniqSorted(target.products), state, detail,
    });
  }
  return targets;
}

// ---------- RK-ready packages living inside an app ----------

const RK_PREFIX = /^(rightkit-|@rightkit\/|RightKit)/;

function describeManifest(file, dirName) {
  const base = path.basename(file.abs);
  const text = readText(file.abs);
  if (text === null) return null;
  if (base === "Cargo.toml") {
    const toml = parseToml(text);
    const pkg = toml.package;
    if (!pkg) return null;
    const unpublished = pkg.publish === "false" || (Array.isArray(pkg.publish) && pkg.publish.length === 0);
    return {
      kind: "cargo", name: unquote(pkg.name ?? ""), version: unquote(pkg.version ?? (pkg["version.workspace"] ? "workspace" : "")),
      publish: unpublished ? "publish = false" : "publishable", libName: toml.lib?.name ? unquote(toml.lib.name).replace(/_/g, "-") : "",
    };
  }
  if (base === "package.json") {
    const doc = JSON.parse(text);
    return { kind: "npm", name: str(doc.name), version: str(doc.version), publish: doc.private === true ? "private" : "publishable", libName: "" };
  }
  const parsed = parseSwiftPackage(text);
  const version = str(readText(path.join(path.dirname(file.abs), "VERSION")) ?? "");
  return { kind: "swift", name: parsed.name || dirName, version, publish: "n/a (SwiftPM, tag-based)", libName: "" };
}

// packages: the RightKit-owned names (packages and Swift products) an app copy can duplicate.
function rkReadyPackages(appDir, owners) {
  const list = [];
  for (const file of walkFiles(appDir, new Set(["Cargo.toml", "package.json", "Package.swift"]))) {
    if (isTestOnly(file.rel)) continue;
    const dirRel = path.posix.dirname(file.rel);
    const dirName = dirRel === "." ? "" : path.posix.basename(dirRel);
    let info;
    try { info = describeManifest(file, dirName); } catch { continue; }
    if (!info || ![info.name, dirName].some((name) => RK_PREFIX.test(name))) continue;
    const candidates = uniqSorted([dirName, info.name, info.libName].filter(Boolean));
    const owner = candidates.map((name) => owners.get(name)).find(Boolean) ?? null;
    const entry = {
      path: dirRel === "." ? "." : dirRel, name: info.name, directory: dirName, kind: info.kind, version: info.version || "-", publish: info.publish,
      status: owner ? "duplicate" : "awaiting promotion", owner: owner?.owner ?? "", ownerVersion: owner?.published || owner?.source || "",
      ownerPublished: Boolean(owner?.published),
    };
    entry.statusText = !owner ? "awaiting promotion"
      : owner.published ? `duplicate of published owner ${owner.label} ${owner.published}`
        : `duplicate of owner ${owner.label} (source ${owner.source || "unknown"}, not published)`;
    list.push(entry);
  }
  return list.sort((a, b) => cmp(a.path, b.path));
}

// ---------- triage ----------

export function loadTriage(file) {
  const doc = readJson(file);
  const rows = Array.isArray(doc) ? doc : Array.isArray(doc?.rows) ? doc.rows : null;
  if (!rows) throw new Error(`triage file ${file} is not a list of rows`);
  const moves = {};
  let moveRows = 0, skipped = 0;
  for (const row of rows) {
    if (!isObject(row)) { skipped++; continue; }
    if (str(row.verdict).toUpperCase() !== "MOVE") continue;
    const app = str(row.app), target = str(row.package);
    if (!app || !target) { skipped++; continue; }
    moves[app] ??= {};
    moves[app][target] = (moves[app][target] ?? 0) + 1;
    moveRows++;
  }
  return { moves, moveRows, skipped, totalRows: rows.length };
}

// ---------- per-app check (scripts/preflight.mjs) ----------

export function parsePreflightOutput(text) {
  const result = { fail: 0, known: 0, stale: 0, byRule: {} };
  for (const line of String(text).split(/\r?\n/)) {
    const match = /^(FAIL|KNOWN|STALE)\b\s*([A-Za-z0-9_-]*)/.exec(line);
    if (!match) continue;
    if (match[1] === "FAIL") result.fail++;
    else if (match[1] === "KNOWN") result.known++;
    else { result.stale++; continue; }
    const rule = match[2] || "unknown";
    result.byRule[rule] ??= { fail: 0, known: 0 };
    result.byRule[rule][match[1] === "FAIL" ? "fail" : "known"]++;
  }
  result.byRule = Object.fromEntries(Object.entries(result.byRule).sort(([a], [b]) => cmp(a, b)));
  return result;
}

const notRun = (reason) => ({ status: "not run", reason, fail: null, known: null, stale: null, byRule: {} });

// node scripts/preflight.mjs --repo <appDir> --commit HEAD: offline, reads committed files only. Never throws.
export function runPreflightCheck(root, appDir) {
  const script = path.join(root, "scripts", "preflight.mjs");
  if (!existsSync(script)) return notRun("scripts/preflight.mjs not found");
  if (!gitHead(appDir)) return notRun("not a git repo");
  const run = spawnSync(process.execPath, [script, "--repo", appDir, "--commit", "HEAD"], { encoding: "utf8", timeout: 300000, maxBuffer: 256 * 1024 * 1024 });
  if (run.error) return notRun(`preflight did not run: ${String(run.error.message).split("\n")[0]}`);
  if (run.status === 2 || run.status === null) return notRun(`preflight could not run (exit ${run.status ?? "signal"}): ${String(run.stderr).trim().split("\n")[0] || "no output"}`);
  return { status: "ran", reason: "", ...parsePreflightOutput(`${run.stdout}\n${run.stderr}`) };
}

// ---------- RightKit inventory, classes and domains ----------

function loadRightKitSwift(root, versions) {
  const dir = path.join(root, "swift", "RightKitSwift");
  const text = readText(path.join(dir, "Package.swift"));
  if (text === null) return null;
  const recorded = versions.swift?.["rightkit-swift"];
  return {
    ecosystem: "swift", kind: "swift", name: "RightKitSwift", dir: "swift/RightKitSwift", isPrivate: false,
    source: str(readText(path.join(dir, "VERSION")) ?? ""),
    // The `swift` record is a retired mirror tag; only a record that is not retired counts as published.
    published: recorded && !recorded.retired ? str(recorded.version) : "",
    staged: str(versions.stagedSwift?.["rightkit-swift"]?.version),
    products: uniqSorted([...stripSwiftComments(text).matchAll(/\.library\s*\(\s*name:\s*"([^"]+)"/g)].map((m) => m[1])),
  };
}

function buildPackages(inventory, swift, domains, catalog) {
  const versions = inventory.versions;
  const capabilitiesOf = new Map();
  for (const entry of catalog) {
    if (!capabilitiesOf.has(entry.package)) capabilitiesOf.set(entry.package, new Set());
    capabilitiesOf.get(entry.package).add(entry.capability);
  }
  const domainOf = new Map();
  for (const domain of domains) for (const name of domain.packages) domainOf.set(name, domain.id);
  const make = (ecosystem, item) => ({
    key: keyOf(ecosystem, item.name), ecosystem, kind: ecosystem, name: item.name, dir: item.dir, isPrivate: item.isPrivate,
    source: item.version, published: publishedOf(versions, ecosystem, item.name), staged: stagedOf(versions, ecosystem, item.name),
    capabilities: uniqSorted([...(capabilitiesOf.get(item.name) ?? [])]), domain: domainOf.get(item.name) ?? OTHER_DOMAIN.id,
  });
  const list = [...inventory.crates.map((c) => make("cargo", c)), ...inventory.packages.map((p) => make("npm", p))];
  if (swift) {
    list.push({
      key: keyOf("swift", swift.name), ecosystem: "swift", kind: "swift", name: swift.name, dir: swift.dir, isPrivate: false, source: swift.source,
      published: swift.published, staged: swift.staged, products: swift.products,
      capabilities: uniqSorted([...(capabilitiesOf.get(swift.name) ?? [])]), domain: domainOf.get(swift.name) ?? OTHER_DOMAIN.id,
    });
  }
  return list.sort((a, b) => cmp(a.name, b.name));
}

// capability -> the RightKit packages that own it (the catalog entries plus the owner families of the catalog config).
function capabilityOwners(catalog, config) {
  const owners = new Map();
  const add = (capability, name) => {
    if (!capability || !name) return;
    if (!owners.has(capability)) owners.set(capability, new Set());
    owners.get(capability).add(name);
  };
  for (const entry of catalog) add(entry.capability, entry.package);
  for (const family of Array.isArray(config?.ownerFamilies) ? config.ownerFamilies : []) for (const name of family.packages ?? []) add(family.capability, name);
  return owners;
}

// ---------- scanning one app ----------

const pinPiece = (token, pin, latest) => {
  const req = pin.req.trim() || "*";
  if (token === "adopted" || token === "range") return req;
  if (token === "stale") return `${req.replace(/^=/, "")}→${latest}`;
  if (token === "ahead") return latest ? `${req.replace(/^=/, "")} > ${latest}` : `${req.replace(/^=/, "")} (no published version)`;
  if (pin.source === "git") return "git";
  if (pin.source === "local") return pin.ecosystem === "cargo" ? "path" : (/^(workspace|file|link|portal):/.exec(req)?.[1] ?? "path");
  return pin.source === "other-registry" ? "other registry" : `unresolved ${req}`;
};

function scanPins(appDir, packageIndex) {
  const pins = new Map();
  let manifests = 0, parseErrors = 0, matched = 0, vendored = 0;
  for (const file of manifestsUnder(appDir)) {
    manifests++;
    let manifest;
    try { manifest = readManifestEntries(file); } catch { parseErrors++; continue; }
    const rel = posix(appDir, file);
    if (packageIndex.has(keyOf(manifest.ecosystem, manifest.ownName))) vendored++;
    for (const entry of manifest.entries) {
      const pkg = packageIndex.get(keyOf(entry.ecosystem, entry.name));
      if (!pkg) continue;
      const token = TOKEN_OF_STATUS[pinStatus(entry, pkg.published)];
      const record = { manifest: rel, section: entry.section, req: entry.req, dev: DEV_SECTION.test(entry.section), token, piece: pinPiece(token, entry, pkg.published) };
      if (!pins.has(pkg.key)) pins.set(pkg.key, []);
      pins.get(pkg.key).push(record);
      matched++;
    }
  }
  return { pins, manifests, parseErrors, matched, vendored };
}

function releaseConfigKind(appDir) {
  const code = ["right-release.config.mjs", "right-release.config.js", "right-release.config.cjs"].some((f) => existsSync(path.join(appDir, f)));
  const data = existsSync(path.join(appDir, "right-release.config.json"));
  return code && data ? "code+data" : code ? "code" : data ? "data" : "none";
}

function scanApp(app, ctx) {
  const { data, packageIndex, owners, swift, today, carried, options } = ctx;
  const carriedApp = carried?.apps?.[app.name];
  const present = isDirectory(app.dir);
  const base = { name: app.name, group: app.group, dir: carriedApp?.dir ?? app.dir, present };
  if (!present) {
    return { ...base, head: null, inSet: false, profile: null, declaration: null, rows: [], owners: new Map(), pins: new Map(), scan: null, exceptions: null,
      overlays: new Map(), flags: null, apple: [], rkReady: [], check: notRun("app directory not present") };
  }
  const declaration = loadDeclaration(app.dir, data);
  const inSet = (data.defaultApps ?? []).includes(app.name) || declaration.present;
  const profile = inSet ? declaration.profile : null;
  const rows = inSet ? profileRows(data, profile) : [];
  const scan = scanPins(app.dir, packageIndex);
  const apple = scanAppleTargets(app.dir, ctx.swiftLatest);
  if (swift && apple.length) {
    scan.pins.set(keyOf("swift", swift.name), apple.map((t) => ({
      manifest: t.file, section: t.system, req: t.requirement, dev: false, token: t.state, piece: t.detail,
    })));
  }
  // Foundation owners of this app: per package key, the rows it belongs to, which of them are ruled N/A, and whether dev pins count.
  const ruled = declaration.notApplicable;
  const ownerMap = new Map();
  for (const row of rows) for (const o of row.owners) {
    const key = keyOf(o.ecosystem, o.name);
    if (!ownerMap.has(key)) ownerMap.set(key, { name: o.name, rows: [], activeRows: [], ruledRows: [], allowDev: false });
    const entry = ownerMap.get(key);
    entry.rows.push(row.id);
    if (row.id in ruled) entry.ruledRows.push(row.id);
    else { entry.activeRows.push(row.id); entry.allowDev ||= o.allowDev; }
  }
  const exceptionFile = loadExceptions(app.dir);
  const overlays = new Map();
  const matched = new Set();
  const byPackage = new Map();
  for (const entry of exceptionFile.entries) {
    const names = new Set();
    if (entry.package) names.add(entry.package);
    for (const name of ctx.capabilityOwners.get(entry.capability) ?? []) names.add(name);
    for (const name of names) {
      const key = ctx.keyByName.get(name);
      if (!key) continue;
      matched.add(entry);
      if (!byPackage.has(key)) byPackage.set(key, []);
      byPackage.get(key).push(entry);
    }
  }
  for (const [key, list] of byPackage) overlays.set(key, exceptionOverlay(list, today));
  const hasCarriedCheck = carriedApp?.progress?.check;
  const check = options.withCheck ? options.runCheck(ctx.root, app.dir, { app: app.name })
    : hasCarriedCheck ? hasCarriedCheck : notRun("--with-check not passed");
  return {
    ...base, head: carriedApp ? carriedApp.head : gitHead(app.dir), inSet, profile, declaration, rows, owners: ownerMap, pins: scan.pins,
    ratified: data.appAmendments?.[app.name]?.notApplicable ?? {},
    scan: { manifests: scan.manifests, parseErrors: scan.parseErrors, pins: scan.matched, vendored: scan.vendored },
    exceptions: { file: exceptionFile, summary: exceptionSummary(exceptionFile.entries, today, matched) },
    overlays,
    flags: {
      prePushHook: hookState(app.dir), switches: existsSync(path.join(app.dir, "switches.json")),
      journeys: existsSync(path.join(app.dir, "journeys.toml")), releaseConfig: releaseConfigKind(app.dir),
    },
    apple, rkReady: rkReadyPackages(app.dir, owners), check,
  };
}

// ---------- cells ----------

function worstToken(pins) {
  for (const token of SEVERITY) if (pins.some((p) => p.token === token)) return token;
  return null;
}

function pinsCell(pins) {
  const byToken = new Map();
  for (const pin of pins) {
    if (!byToken.has(pin.token)) byToken.set(pin.token, []);
    byToken.get(pin.token).push(pin);
  }
  const tokens = SEVERITY.filter((t) => byToken.has(t));
  const primary = byToken.get(tokens[0]);
  const pieces = uniqSorted(primary.map((p) => p.piece));
  let detail = pieces.slice(0, 3).join(", ") + (pieces.length > 3 ? `, +${pieces.length - 3}` : "");
  if (primary.length > 1) detail += `, ${primary.length} pins`;
  const others = tokens.slice(1).map((t) => `${byToken.get(t).length} ${t}`);
  if (others.length) detail += `; also ${others.join(", ")}`;
  if (pins.every((p) => p.dev)) detail += " dev";
  return { state: tokens[0], detail };
}

const overlayJson = (o) => ({ count: o.count, expired: o.expired, withoutIssue: o.withoutIssue, withoutExpiry: o.withoutExpiry, issues: o.issues, earliestExpiry: o.earliestExpiry });

function evaluateRows(app, data) {
  const ruled = app.declaration?.notApplicable ?? {};
  const ratified = data.appAmendments?.[app.name]?.notApplicable ?? {};
  return app.rows.map((row) => {
    if (row.id in ruled) return { id: row.id, name: row.name, status: "ruled", ratified: row.id in ratified, owners: [], missing: [] };
    const owners = row.owners.map((o) => {
      const key = keyOf(o.ecosystem, o.name);
      const counted = (app.pins.get(key) ?? []).filter((p) => !p.dev || o.allowDev);
      return { name: o.name, state: counted.length ? worstToken(counted) : "missing", declared: counted.some((p) => p.token !== "local") };
    });
    const missing = owners.filter((o) => !o.declared).map((o) => o.name);
    return { id: row.id, name: row.name, status: missing.length ? "open" : "declared", ratified: false, owners: owners.map(({ name, state }) => ({ name, state })), missing };
  });
}

function cellOf(app, pkg, rowEval) {
  if (!app.present) return { state: "not present", detail: "" };
  const own = app.owners.get(pkg.key);
  const pins = app.pins.get(pkg.key) ?? [];
  const counted = own?.activeRows.length ? pins.filter((p) => !p.dev || own.allowDev) : pins;
  const overlay = app.overlays.get(pkg.key);
  const exceptions = overlay ? overlayJson(overlay) : undefined;
  const withRows = (cell) => (own ? { ...cell, rows: own.rows } : cell);
  if (counted.length) {
    const { state, detail } = pinsCell(counted);
    const notes = [];
    for (const id of own?.activeRows ?? []) {
      const row = rowEval.find((r) => r.id === id);
      if (row?.status === "open" && !row.missing.includes(pkg.name)) notes.push(`${id} incomplete: missing ${row.missing.join(", ")}`);
    }
    if (overlay) notes.push(overlay.text);
    return { ...withRows({ state, detail: [detail, ...notes].join("; ") }), ...(exceptions ? { exceptions } : {}) };
  }
  if (own && own.ruledRows.length && !own.activeRows.length) {
    const ratified = app.ratified ?? {};
    const detail = own.ruledRows.map((id) => `${id} ${id in ratified ? "ratified" : "declared"}`).join(", ");
    return withRows({ state: "n/a", detail });
  }
  if (overlay) return { ...withRows({ state: "exception", detail: `${overlay.text}${own?.activeRows.length ? `; required by ${own.activeRows.join(", ")}` : ""}` }), exceptions };
  if (own?.activeRows.length) {
    const devOnly = pins.length > 0 ? "; declared only as a dev dependency" : "";
    return withRows({ state: "missing", detail: `${own.activeRows.join(", ")}${devOnly}` });
  }
  return { state: "unused", detail: "" };
}

// ---------- progress ----------

const percentOf = (done, total) => (total > 0 ? Math.round((100 * done) / total) : null);

function progressOf(app, packages, rowEval, latestOf) {
  const requiredKeys = [...app.owners].filter(([, o]) => o.activeRows.length).map(([key]) => key).sort(cmp);
  let current = 0, pinned = 0;
  for (const key of requiredKeys) {
    const own = app.owners.get(key);
    const counted = (app.pins.get(key) ?? []).filter((p) => !p.dev || own.allowDev);
    if (counted.some((p) => p.token !== "local")) pinned++;
    if (counted.length && CURRENT_TOKENS.has(worstToken(counted))) current++;
  }
  const openRows = rowEval.filter((r) => r.status === "open").map((r) => ({ id: r.id, name: r.name, missing: r.missing }));
  const foundation = {
    profile: app.profile, rowsTotal: rowEval.length, rowsDeclared: rowEval.filter((r) => r.status === "declared").length,
    rowsRuled: rowEval.filter((r) => r.status === "ruled").length, rowsOpen: openRows.length, openRows,
    ownersRequired: requiredKeys.length, ownersPinned: pinned, ownersCurrent: current, percent: percentOf(current, requiredKeys.length),
  };
  const addons = { total: 0, adopted: 0, current: 0, stale: 0, local: 0 };
  for (const pkg of packages) {
    if (app.owners.has(pkg.key) || pkg.class === "tool") continue;
    addons.total++;
    const pins = app.pins.get(pkg.key) ?? [];
    if (!pins.length) continue;
    addons.adopted++;
    const token = worstToken(pins);
    if (CURRENT_TOKENS.has(token)) addons.current++;
    if (token === "stale") addons.stale++;
    if (token === "local") addons.local++;
  }
  const releasePins = (app.pins.get(keyOf("npm", "@rightkit/release")) ?? []);
  const releaseToken = releasePins.length ? worstToken(releasePins) : null;
  const releasePin = releaseToken ? {
    state: releaseToken, req: uniqSorted(releasePins.map((p) => p.req.trim() || "*")).join(", "), latest: latestOf(keyOf("npm", "@rightkit/release")),
    current: CURRENT_TOKENS.has(releaseToken), dev: releasePins.every((p) => p.dev),
  } : null;
  return { foundation, addons, exceptions: { ...app.exceptions.summary }, releasePin, check: app.check };
}

// ---------- model ----------

// The volatile fields of the committed docs/port-matrix.json, and the triage counts it was built from (the triage file lives outside
// this repository, so a drift check without --triage reuses them).
export function loadCarried(root) {
  const doc = readJsonSafe(path.join(root, PORT_MATRIX_JSON));
  if (!isObject(doc) || !Array.isArray(doc.apps) || !Array.isArray(doc.packages)) return null;
  let triage = null;
  if (isObject(doc.triage)) {
    const moves = {};
    const add = (app, name, count) => { moves[app] ??= {}; moves[app][name] = count; };
    for (const pkg of doc.packages) for (const [app, count] of Object.entries(pkg.toMoveFrom ?? {})) add(app, pkg.name, count);
    for (const need of Array.isArray(doc.newPackagesNeeded) ? doc.newPackagesNeeded : []) for (const [app, count] of Object.entries(need.byApp ?? {})) add(app, need.package, count);
    triage = { moves, moveRows: doc.triage.moveRows ?? 0, totalRows: doc.triage.totalRows ?? 0, skipped: doc.triage.skipped ?? 0 };
  }
  return { generatedAt: doc.generatedAt, rightkitHead: doc.rightkitHead, apps: Object.fromEntries(doc.apps.map((a) => [a.name, a])), triage };
}

// options: root, workspace, workspaceExplicit, appDirs {app: dir}, extraApps, domains, triage (file path), withCheck, runCheck, now,
// carry (take the volatile fields from the committed docs/port-matrix.json).
export function buildPortMatrixModel(options = {}) {
  const root = path.resolve(options.root);
  const workspace = resolveWorkspace(options.workspace, root);
  const now = options.now ? new Date(options.now) : new Date();
  if (Number.isNaN(now.getTime())) throw new Error(`--now is not a valid time: ${options.now}`);
  const today = now.toISOString().slice(0, 10);
  const carried = options.carry ? loadCarried(root) : null;
  const inventory = loadInventory(root);
  const versions = inventory.versions;
  const data = loadMandatorySet(root);
  const swift = loadRightKitSwift(root, versions);
  const domains = options.domains ?? DOMAINS;
  const packages = buildPackages(inventory, swift, domains, inventory.catalog);
  const packageIndex = new Map(packages.map((p) => [p.key, p]));
  const keyByName = new Map(packages.map((p) => [p.name, p.key]));
  const swiftLatest = swift ? [swift.published, swift.staged].filter(Boolean).sort(compareVersionText).pop() ?? "" : "";
  const latestOf = (key) => { const p = packageIndex.get(key); return p ? (p.kind === "swift" ? swiftLatest : p.published) : ""; };
  const owners = new Map();
  for (const pkg of packages) {
    owners.set(pkg.name, { owner: pkg.name, label: pkg.name, source: pkg.source, published: pkg.kind === "swift" ? swift.published : pkg.published });
  }
  for (const product of swift?.products ?? []) {
    if (!owners.has(product)) owners.set(product, { owner: swift.name, label: `${swift.name} product ${product}`, source: swift.source, published: swift.published });
  }
  const config = readJsonSafe(path.join(root, CATALOG_CONFIG_FILE));
  const ctx = {
    root, data, packageIndex, keyByName, owners, swift, swiftLatest, today, carried, capabilityOwners: capabilityOwners(inventory.catalog, config),
    options: { withCheck: Boolean(options.withCheck), runCheck: options.runCheck ?? runPreflightCheck },
  };

  const appList = resolveApps({ data, workspace, appDirs: options.appDirs, extraApps: options.extraApps, carried });
  const scanned = appList.map((app) => scanApp(app, ctx));

  // Foundation (default profile), pending rows and class.
  const defaultRows = new Map();
  for (const row of profileRows(data, data.defaultProfile)) for (const o of row.owners) {
    const key = keyOf(o.ecosystem, o.name);
    if (!defaultRows.has(key)) defaultRows.set(key, []);
    defaultRows.get(key).push(row.id);
  }
  const pendingRows = new Map();
  for (const row of data.pendingRows ?? []) for (const o of row.pendingOwners ?? []) {
    const key = keyOf(o.ecosystem, o.name);
    if (!pendingRows.has(key)) pendingRows.set(key, []);
    if (!pendingRows.get(key).includes(row.id)) pendingRows.get(key).push(row.id);
  }
  for (const pkg of packages) {
    const consumers = scanned.filter((a) => a.pins.has(pkg.key));
    const appConsumers = consumers.filter((a) => a.group !== GROUP_TOOL);
    const toolOnly = consumers.length > 0 && appConsumers.length === 0;
    const unpublished = pkg.kind === "swift" ? !(pkg.published || pkg.staged) : !pkg.published;
    pkg.mandatoryRows = uniqSorted(defaultRows.get(pkg.key) ?? []).sort(rowOrder);
    pkg.pendingRows = uniqSorted(pendingRows.get(pkg.key) ?? []).sort(rowOrder);
    pkg.class = pkg.mandatoryRows.length ? "foundation" : pkg.isPrivate || unpublished || toolOnly ? "tool" : "add-on";
  }

  // Triage.
  const triage = options.triage ? loadTriage(options.triage) : carried?.triage ?? null;
  const knownNames = new Set(packages.map((p) => p.name));
  const toMoveOf = (appName) => Object.entries(triage?.moves?.[appName] ?? {}).map(([name, count]) => {
    const pkg = packages.find((p) => p.name === name);
    return { package: name, count, exists: knownNames.has(name), kind: pkg?.kind ?? "new", status: knownNames.has(name) ? "move into existing package" : "new package needed" };
  }).sort((a, b) => b.count - a.count || cmp(a.package, b.package));

  // Apps.
  const apps = scanned.map((app) => {
    const rowEval = app.present ? evaluateRows(app, ctx.data) : [];
    app.rowEval = rowEval;
    const entry = {
      name: app.name, group: app.group, dir: app.dir, present: app.present, head: app.present ? app.head : null,
      profile: app.profile, inMandatorySet: app.inSet,
      declaration: app.declaration ? {
        present: app.declaration.present, error: app.declaration.error,
        notApplicable: Object.entries(app.declaration.notApplicable).sort(([a], [b]) => rowOrder(a, b)).map(([row]) => ({ row, ratified: row in app.ratified })),
        ignored: app.declaration.ignored,
      } : null,
      scan: app.scan, progress: app.present ? progressOf(app, packages, rowEval, latestOf) : null, flags: app.flags,
      foundationRows: rowEval, appleTargets: app.apple, rkReady: app.rkReady, toMove: toMoveOf(app.name),
    };
    return entry;
  });

  // Cells.
  const packageRecords = packages.map((pkg) => {
    const cells = {};
    for (const app of scanned) cells[app.name] = cellOf(app, pkg, app.rowEval);
    const toMoveFrom = {};
    for (const appName of Object.keys(triage?.moves ?? {}).sort(cmp)) if (triage.moves[appName][pkg.name]) toMoveFrom[appName] = triage.moves[appName][pkg.name];
    const record = {
      name: pkg.name, kind: pkg.kind, class: pkg.class, domain: pkg.domain, mandatoryRows: pkg.mandatoryRows, pendingRows: pkg.pendingRows,
      capabilities: pkg.capabilities, private: pkg.isPrivate, source: pkg.source, registry: pkg.published, staged: pkg.staged, dir: pkg.dir,
    };
    if (pkg.products) record.products = pkg.products;
    return { ...record, toMoveFrom, cells };
  });

  const newNeeded = new Map();
  for (const [appName, moves] of Object.entries(triage?.moves ?? {})) for (const [name, count] of Object.entries(moves)) {
    if (knownNames.has(name)) continue;
    if (!newNeeded.has(name)) newNeeded.set(name, { package: name, total: 0, byApp: {} });
    newNeeded.get(name).total += count;
    newNeeded.get(name).byApp[appName] = count;
  }
  const usedDomains = domains.map((d) => ({ id: d.id, name: d.name })).filter((d) => packageRecords.some((p) => p.domain === d.id));
  if (packageRecords.some((p) => p.domain === OTHER_DOMAIN.id)) usedDomains.push({ ...OTHER_DOMAIN });
  for (const domain of usedDomains) domain.packages = packageRecords.filter((p) => p.domain === domain.id).length;

  const dirOf = (name) => carried?.apps?.[name]?.dir;
  return {
    schema: SCHEMA,
    generatedAt: carried?.generatedAt ?? now.toISOString().replace(/\.\d{3}Z$/, "Z"),
    rightkitHead: carried ? carried.rightkitHead ?? null : gitHead(root),
    legend: STATES.map((s) => ({ ...s })),
    classLegend: CLASSES.map((c) => ({ ...c })),
    domains: usedDomains,
    triage: triage ? { moveRows: triage.moveRows, totalRows: triage.totalRows, skipped: triage.skipped, apps: Object.keys(triage.moves).sort(cmp) } : null,
    apps: apps.map((a) => (dirOf(a.name) ? { ...a, dir: dirOf(a.name) } : a)),
    packages: packageRecords,
    newPackagesNeeded: [...newNeeded.values()].sort((a, b) => b.total - a.total || cmp(a.package, b.package)),
  };
}

function rowOrder(a, b) {
  const n = (id) => Number(/\d+/.exec(id)?.[0] ?? Number.MAX_SAFE_INTEGER);
  return n(a) - n(b) || cmp(a, b);
}

// ---------- shared view helpers ----------

export const REGENERATE_COMMAND = "node scripts/generate-index.mjs --workspace <dir with the app repos> --app-dir <app>=<dir> --with-check --triage <rows.json>";
export const DRIFT_COMMAND = "node scripts/generate-index.mjs --check-adoption --workspace <dir with the app repos> --app-dir <app>=<dir>";
export const LOCAL_CHECK_COMMAND = "node scripts/generate-index.mjs --check";

export function stampLine(model) {
  const heads = model.apps.map((a) => `${a.name} ${!a.present ? "not present" : a.head ?? "not a git repo"}`).join(", ");
  return `Stamp: generated ${model.generatedAt} from RightKit ${model.rightkitHead ?? "not a git repo"}; apps: ${heads}.`;
}

const percentText = (p) => (p === null || p === undefined ? "n/a" : `${p}%`);
const checkText = (check) => (check?.status === "ran" ? `FAIL ${check.fail}, KNOWN ${check.known}${check.stale ? `, STALE ${check.stale}` : ""}` : `not run (${check?.reason || "no reason recorded"})`);
const latestText = (pkg) => {
  const parts = [pkg.registry || (pkg.kind === "swift" ? "no tag recorded" : pkg.private ? "private" : "none recorded")];
  if (pkg.staged && pkg.staged !== pkg.registry) parts.push(`staged ${pkg.staged}`);
  if (pkg.kind === "swift" && pkg.source && pkg.source !== pkg.staged) parts.push(`source ${pkg.source}`);
  return `${parts[0]}${parts.length > 1 ? ` (${parts.slice(1).join(", ")})` : ""}`;
};
const releasePinText = (pin) => (!pin ? "none" : pin.current
  ? `${pin.req}: ${pin.state === "adopted" ? "latest" : pin.state === "range" ? "admits latest" : "ahead of"} ${pin.latest}`
  : `${pin.req}: behind ${pin.latest}`);
const hookText = (state) => ({ committed: "committed", uncommitted: "present, not committed", present: "present (not a git repo)", absent: "absent" })[state] ?? state;
const releaseConfigText = (kind) => ({ code: "code (.mjs)", data: "data (.json)", "code+data": "code and data", none: "none" })[kind] ?? kind;
const yesNo = (value) => (value ? "yes" : "no");
const rowCoverage = (f) => `${f.rowsDeclared + f.rowsRuled}/${f.rowsTotal}${f.rowsRuled ? ` (${f.rowsRuled} ruled)` : ""}`;
const appsByDomain = (model) => model.domains.map((domain) => ({ domain, packages: model.packages.filter((p) => p.domain === domain.id) }));
const moveFromText = (pkg) => Object.entries(pkg.toMoveFrom).map(([app, n]) => `${app} ${n}`).join(", ");

// ---------- JSON ----------

export function renderJson(model) {
  return `${JSON.stringify(model, null, 2)}\n`;
}

// ---------- Markdown ----------

const mdCell = (text) => String(text ?? "").replace(/\|/g, "\\|").replace(/\s+/g, " ").trim();
const mdState = (cell) => mdCell(cell.detail ? `${cell.state} (${cell.detail})` : cell.state);
const mdTable = (head, rows) => [`| ${head.join(" | ")} |`, `|${head.map(() => "---").join("|")}|`, ...rows.map((r) => `| ${r.join(" | ")} |`)];

export function renderMarkdown(model) {
  const apps = model.apps;
  const out = [];
  out.push("# RightKit port matrix", "");
  out.push(stampLine(model), "");
  out.push(`Generated by \`node rightkit/scripts/generate-index.mjs\`; do not hand-edit. Source of the data: [\`port-matrix.json\`](port-matrix.json); the same data as a filterable page: [\`port-matrix.html\`](port-matrix.html) (open it from disk, it needs no network).`, "");
  out.push("One table that answers what is where: every RightKit package against every app, with the state of each pairing and, per app, how far the port has gone. The stamp line above is the only part that changes between two runs over the same data.", "");

  out.push("## Regenerate and check", "");
  out.push("```", `# regenerate (also rewrites INDEX.md, docs/ADOPTION.md and docs/CAPABILITY-MAP.md)`, REGENERATE_COMMAND, "",
    "# drift check against the app repos (what the inventory check runs; the stamp, app directories, app HEADs and check counts are carried over from port-matrix.json)",
    DRIFT_COMMAND, "", "# local check, no app repos: the MD and HTML must be the rendering of port-matrix.json", LOCAL_CHECK_COMMAND, "```", "");

  out.push("## How to read", "");
  out.push("Rows are RightKit packages, grouped by domain; columns are apps. A cell is a state token with the short detail in parentheses. `Rows` lists the mandatory-set rows (M1, M2, ...) a package owns for the default profile; `Latest` is the latest published version, with the staged version in parentheses when it differs; `To move from` counts the triage MOVE rows that name the package, per app.", "");
  out.push("**Class** of a package (Adrian, 2026-10-24):", "");
  for (const c of model.classLegend) out.push(`- \`${c.class}\`: ${c.meaning}`);
  out.push("", "**Cell states** (these tokens and no others):", "", ...mdTable(["State", "Meaning"], model.legend.map((l) => [`\`${l.state}\``, mdCell(l.meaning)])), "");
  out.push("**Progress figure.** One figure per app: foundation owners pinned current divided by foundation owners required for the app's profile. An owner is required unless every mandatory-set row it belongs to is ruled not applicable for the app; it is pinned current when the worst of its pins in that app is `adopted`, `range` or `ahead` (one `stale` or `local` pin makes it not current). A foundation row is declared when every one of its owners has a registry pin, ruled when the app rules it not applicable, and open otherwise; an open row lists the owners it still misses. Add-on adoption is reported next to the figure and never folded into it.", "");
  out.push("**Exceptions** come from `.rightkit/ownership-exceptions.json`. One that names a package or its capability shows on that package's cell for that app: as the state `exception` when the app has no pin, as a note when it does. `EXPIRED`, `no issue` and `no expiry` are flags. Exceptions that name neither a RightKit package nor a catalog capability (the dependency allowlist, for example) count in the per-app totals and appear on no cell.", "");
  out.push("**Domains** are a grouping kept in `scripts/generate-port-matrix.mjs`; the ownership catalog names capabilities but no domain. `RightKitSwift` is one row whose cell is the worst pin among the app's Apple targets (every `project.yml` and `Package.swift` that references it, test-only paths excluded); its latest version is the highest of the recorded published and staged versions in `rightkit-versions.json`. An Apple target's pin kind is `tag` (an exact or `from` version), `branch`, `path` or `revision` (url plus revision); only a tag can be `adopted`, `range`, `stale` or `ahead`, the others are `local`.", "");
  out.push("**RK-ready** packages are packages living inside an app repo whose package or directory name starts with `rightkit-`, `@rightkit/` or `RightKit`: built so they can be published. **To move** rows are the MOVE verdicts of the triage file, counted per target package.", "");

  out.push("## Per-app summary", "");
  const summaryRows = apps.map((a) => {
    if (!a.present) return [mdCell(a.name), mdCell(a.group), "-", "not present", "-", "-", "-", "-", "-", "-", "-", "-", "-"];
    const p = a.progress, f = p.foundation, x = p.exceptions;
    const figure = f.ownersRequired ? `${percentText(f.percent)} (${f.ownersCurrent}/${f.ownersRequired} owners current)` : "n/a (no foundation rows for this app)";
    return [
      mdCell(a.name), mdCell(a.group), mdCell(a.profile ?? "-"), mdCell(figure), f.rowsTotal ? rowCoverage(f) : "-",
      `${p.addons.adopted}/${p.addons.total} (${p.addons.current} current, ${p.addons.stale} stale)`,
      a.progress.exceptions.total || a.declaration ? `${x.open} / ${x.expired} / ${x.withoutIssueOrExpiry}` : "-",
      mdCell(releasePinText(p.releasePin)), mdCell(hookText(a.flags.prePushHook)), yesNo(a.flags.switches), yesNo(a.flags.journeys),
      mdCell(releaseConfigText(a.flags.releaseConfig)), mdCell(checkText(p.check)),
    ];
  });
  out.push(...mdTable(["App", "Group", "Profile", "Progress", "Rows declared or ruled", "Add-ons pinned (current, stale)", "Exceptions open / expired / no issue or expiry",
    "@rightkit/release", "pre-push hook", "switches.json", "journeys.toml", "right-release.config", "Check (preflight)"], summaryRows), "");
  const open = apps.filter((a) => a.present && a.progress.foundation.openRows.length);
  if (open.length) {
    out.push("Open foundation rows (rows an app has neither declared nor ruled):", "");
    for (const a of open) out.push(`- **${a.name}**: ${a.progress.foundation.openRows.map((r) => `${r.id} ${r.name} (missing ${r.missing.join(", ")})`).join("; ")}`);
    out.push("");
  }
  const ruledApps = apps.filter((a) => a.declaration?.notApplicable.length);
  if (ruledApps.length) {
    out.push("Not-applicable rulings (ratified = listed in `appAmendments` of the mandatory set; declared = only in the app's `rightkit-mandatory.json`):", "");
    for (const a of ruledApps) out.push(`- **${a.name}**: ${a.declaration.notApplicable.map((n) => `${n.row} ${n.ratified ? "ratified" : "declared"}`).join(", ")}`);
    out.push("");
  }

  out.push(`## Matrix (${model.packages.length} packages)`, "");
  const appHead = apps.map((a) => mdCell(a.group === GROUP_MANDATORY ? a.name : `${a.name} (${a.group})`));
  for (const { domain, packages } of appsByDomain(model)) {
    out.push(`### ${domain.name} (${packages.length})`, "");
    out.push(...mdTable(["Package", "Class", "Rows", "Latest", ...appHead, "To move from"], packages.map((pkg) => [
      `\`${mdCell(pkg.name)}\``, pkg.class, mdCell(pkg.mandatoryRows.join(", ") || "-"), mdCell(latestText(pkg)),
      ...apps.map((a) => mdState(pkg.cells[a.name])), mdCell(moveFromText(pkg) || "-"),
    ])), "");
  }

  out.push("## Apple targets", "");
  const targets = apps.flatMap((a) => a.appleTargets.map((t) => [mdCell(a.name), `\`${mdCell(t.file)}\``, mdCell(t.name), t.system, t.pinKind,
    mdCell(t.pinVersion || t.pinRef || "-"), mdCell(t.requirement), mdCell(t.products.join(", ") || "-"), mdCell(`${t.state} (${t.detail})`)]));
  if (targets.length) out.push(...mdTable(["App", "Project file", "Target", "System", "Pin kind", "Version or ref", "Requirement", "RightKitSwift products linked", "State"], targets), "");
  else out.push("No app has an Apple target that references RightKitSwift.", "");

  out.push("## RK-ready packages and to-move rows", "");
  out.push(model.triage
    ? `Triage: ${model.triage.moveRows} MOVE rows of ${model.triage.totalRows} (${model.triage.apps.join(", ")}). KEEP, ADOPT and DELETE rows are not shown.`
    : "No triage file was given (--triage), so no to-move rows are shown.", "");
  let any = false;
  for (const a of apps) {
    if (!a.rkReady.length && !a.toMove.length) continue;
    any = true;
    out.push(`### ${a.name}`, "");
    const rows = [
      ...a.rkReady.map((r) => ["RK-ready", `\`${mdCell(r.name)}\``, `\`${mdCell(r.path)}\``, r.kind, mdCell(r.version), mdCell(r.publish), mdCell(r.statusText)]),
      ...a.toMove.map((m) => ["To move", `\`${mdCell(m.package)}\``, "-", m.kind, "-", "-", `${plural(m.count, "MOVE row")}${m.exists ? "" : "; new package needed"}`]),
    ];
    out.push(...mdTable(["Section", "Name", "Path", "Kind", "Version", "Publish", "Status"], rows), "");
  }
  if (!any) out.push("No app has an RK-ready package or a MOVE row.", "");
  out.push("### New package needed", "");
  if (model.newPackagesNeeded.length) {
    out.push(...mdTable(["Package", "MOVE rows", "From"], model.newPackagesNeeded.map((n) => [`\`${mdCell(n.package)}\``, String(n.total),
      mdCell(Object.entries(n.byApp).map(([app, count]) => `${app} ${count}`).join(", "))])), "");
  } else out.push("None: every MOVE row names a package RightKit already has.", "");
  return out.join("\n");
}

// ---------- HTML ----------

const esc = (value) => String(value ?? "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const stateClass = (state) => `s-${state.replace(/[^a-z]+/g, "-")}`;
const slug = (text) => String(text).toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");

const HTML_CSS = `
:root{color-scheme:light dark;--bg:#fff;--fg:#1b1f24;--muted:#57606a;--line:#d0d7de;--panel:#f6f8fa;--accent:#0a58ca;--bar:#1a7f37;--barbg:#e6eaee;--flag:#b42318;
--adopted-bg:#d4f3dc;--adopted-fg:#0d3a1a;--range-bg:#cdeee8;--range-fg:#08362f;--stale-bg:#ffe7b0;--stale-fg:#533500;--ahead-bg:#d3e4ff;--ahead-fg:#0a2757;
--local-bg:#e5d9ff;--local-fg:#331573;--missing-bg:#ffd2cf;--missing-fg:#650a04;--na-bg:#e4e7eb;--na-fg:#33383e;--exception-bg:#ffd6ea;--exception-fg:#650838;
--unused-bg:transparent;--unused-fg:#59626b;--not-present-bg:#eceff2;--not-present-fg:#4b535b}
@media (prefers-color-scheme:dark){:root{--bg:#0f1318;--fg:#e6e9ed;--muted:#a3adb8;--line:#2f3943;--panel:#171d24;--accent:#79b0ff;--bar:#3fb950;--barbg:#26303a;--flag:#ff8b82;
--adopted-bg:#123a1e;--adopted-fg:#bdf0c9;--range-bg:#0f3a34;--range-fg:#b6ece3;--stale-bg:#4a3500;--stale-fg:#ffe3a0;--ahead-bg:#112c57;--ahead-fg:#c4dcff;
--local-bg:#2b1c58;--local-fg:#dccfff;--missing-bg:#5a1410;--missing-fg:#ffc9c4;--na-bg:#2a3038;--na-fg:#cfd5db;--exception-bg:#561034;--exception-fg:#ffcce4;
--unused-bg:transparent;--unused-fg:#98a3ae;--not-present-bg:#1d242c;--not-present-fg:#9aa5b0}}
*{box-sizing:border-box}
[hidden]{display:none!important}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.45 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
header,main{max-width:100%;padding:0 16px}
header{padding-top:20px}
h1{margin:0 0 4px;font-size:1.5rem}
h2{margin:28px 0 8px;font-size:1.15rem}
h3{margin:0;font-size:1rem}
a{color:var(--accent)}
code,.mono{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:.85em}
small,.muted{color:var(--muted)}
#stamp{margin:4px 0;color:var(--muted);font-size:.85rem;word-break:break-word}
.lede{max-width:70ch;margin:6px 0}
.cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(19rem,1fr));gap:12px}
.card{border:1px solid var(--line);border-radius:8px;background:var(--panel);padding:12px}
.card h3 small{display:block;font-weight:400;font-size:.8rem}
.bar{height:10px;border-radius:5px;background:var(--barbg);overflow:hidden;margin:8px 0 4px}
.bar>span{display:block;height:100%;background:var(--bar)}
.pct{margin:0 0 6px;font-size:.9rem}
.card dl{display:grid;grid-template-columns:auto 1fr;gap:2px 10px;margin:6px 0;font-size:.82rem}
.card dt{color:var(--muted)}
.card dd{margin:0;word-break:break-word}
.card details{margin-top:8px;font-size:.82rem}
.card summary{cursor:pointer;font-weight:600}
.card table{margin-top:6px;border-collapse:collapse;width:100%}
.card th,.card td{border-top:1px solid var(--line);padding:3px 4px;text-align:left;vertical-align:top;word-break:break-word}
.legend{display:grid;grid-template-columns:repeat(auto-fill,minmax(20rem,1fr));gap:6px 16px;padding:0;list-style:none;margin:6px 0}
.legend li{display:flex;gap:8px;align-items:baseline;font-size:.85rem}
.chip{display:inline-block;min-width:5.2rem;padding:1px 8px;border:1px solid var(--line);border-radius:10px;font-size:.78rem;font-weight:600;text-align:center;white-space:nowrap}
form#filters{display:flex;flex-wrap:wrap;gap:10px 16px;align-items:end;margin:8px 0;padding:10px;border:1px solid var(--line);border-radius:8px;background:var(--panel)}
form#filters label{display:flex;flex-direction:column;gap:2px;font-size:.8rem;color:var(--muted)}
form#filters select,form#filters input,form#filters button{font:inherit;color:var(--fg);background:var(--bg);border:1px solid var(--line);border-radius:6px;padding:4px 8px;min-height:32px}
#count{font-size:.85rem;color:var(--muted)}
.scroll{overflow:auto;max-height:80vh;border:1px solid var(--line);border-radius:8px}
.scroll:focus{outline:2px solid var(--accent)}
table#matrix{border-collapse:separate;border-spacing:0;font-size:.78rem;min-width:100%}
#matrix th,#matrix td{border-bottom:1px solid var(--line);border-right:1px solid var(--line);padding:4px 6px;vertical-align:top;background:var(--bg)}
#matrix thead th{position:sticky;background:var(--panel);z-index:2;text-align:left;font-weight:600}
#matrix thead tr:first-child th{top:0}
#matrix thead tr:nth-child(2) th{top:var(--h1,2.1rem)}
#matrix th.pkg{position:sticky;left:0;z-index:1;min-width:13rem;max-width:17rem;background:var(--panel);word-break:break-all}
#matrix thead th.pkg{z-index:3}
#matrix th.pkg small{display:block;font-weight:400}
#matrix td{min-width:7.5rem;max-width:15rem}
#matrix td .st{display:block;font-weight:700}
#matrix td .dt{display:block;color:inherit;opacity:.88;font-size:.72rem;word-break:break-word}
#matrix td.dim{opacity:.28}
#matrix td.flag{box-shadow:inset 0 0 0 2px var(--flag)}
#matrix tr.dom th{position:sticky;left:0;background:var(--panel);font-size:.82rem;z-index:1;text-align:left}
#matrix tr.dom th .lbl{position:sticky;left:8px;display:inline-block}
#matrix td.blank{background:var(--bg)}
#matrix .other-group{border-left:3px double var(--muted)}
#matrix th.apple,#matrix td.apple{border-left:2px dotted var(--line)}
${["adopted", "range", "stale", "ahead", "local", "missing", "exception", "unused", "not-present"].map((s) => `.s-${s}{background:var(--${s}-bg)!important;color:var(--${s}-fg)}`).join("\n")}
.s-n-a{background:var(--na-bg)!important;color:var(--na-fg)}
.c-foundation{background:var(--adopted-bg);color:var(--adopted-fg)}
.c-add-on{background:var(--ahead-bg);color:var(--ahead-fg)}
.c-tool{background:var(--na-bg);color:var(--na-fg)}
.table-plain{border-collapse:collapse;font-size:.82rem;margin:6px 0}
.table-plain th,.table-plain td{border:1px solid var(--line);padding:4px 8px;text-align:left;vertical-align:top}
.vh{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0);white-space:nowrap}
@media (max-width:640px){header,main{padding:0 12px}.cards{grid-template-columns:1fr}}
`;

const HTML_JS = `
(function () {
  var q = function (s, r) { return (r || document).querySelector(s); };
  var qa = function (s, r) { return Array.prototype.slice.call((r || document).querySelectorAll(s)); };
  var form = q('#filters');
  if (!form) return;
  var fApp = q('#f-app'), fClass = q('#f-class'), fDomain = q('#f-domain'), fState = q('#f-state'), fText = q('#f-q');
  var rows = qa('#matrix tbody tr[data-name]');
  var groups = qa('#matrix tbody[data-domain]');
  var h1 = q('#matrix thead tr:first-child');
  if (h1) q('#matrix').style.setProperty('--h1', h1.getBoundingClientRect().height + 'px');
  function matches(td, state) {
    return !state || td.getAttribute('data-state') === state || (state === 'exception' && td.getAttribute('data-exc') === '1');
  }
  function apply() {
    var app = fApp.value, cls = fClass.value, dom = fDomain.value, state = fState.value, text = fText.value.trim().toLowerCase();
    qa('[data-app]').forEach(function (el) { el.hidden = app !== '' && el.getAttribute('data-app') !== app; });
    qa('[data-card]').forEach(function (el) { el.hidden = app !== '' && el.getAttribute('data-card') !== app; });
    var shown = 0;
    rows.forEach(function (tr) {
      var ok = (!cls || tr.getAttribute('data-class') === cls) && (!dom || tr.getAttribute('data-domain') === dom) &&
        (!text || tr.getAttribute('data-name').indexOf(text) !== -1);
      var cells = qa('td[data-state]', tr).filter(function (td) { return !td.hidden; });
      if (ok && state) ok = cells.some(function (td) { return matches(td, state); });
      cells.forEach(function (td) { td.classList.toggle('dim', !!state && !matches(td, state)); });
      tr.hidden = !ok;
      if (ok) shown++;
    });
    groups.forEach(function (tb) { tb.hidden = !qa('tr[data-name]', tb).some(function (tr) { return !tr.hidden; }); });
    q('#count').textContent = 'Showing ' + shown + ' of ' + rows.length + ' packages';
  }
  ['change', 'input'].forEach(function (name) { form.addEventListener(name, apply); });
  form.addEventListener('reset', function () { setTimeout(apply, 0); });
  form.addEventListener('submit', function (e) { e.preventDefault(); });
  apply();
})();
`;

function htmlCard(a) {
  const id = `card-${slug(a.name)}`;
  if (!a.present) {
    return `<article class="card" aria-labelledby="${id}" data-card="${esc(a.name)}"><h3 id="${id}">${esc(a.name)} <small>${esc(a.group)}</small></h3><p class="muted">not present: the app directory does not exist on this machine.</p></article>`;
  }
  const p = a.progress, f = p.foundation, x = p.exceptions;
  const figure = f.ownersRequired
    ? `<div class="bar" role="img" aria-label="${esc(`${a.name}: ${f.ownersCurrent} of ${f.ownersRequired} foundation owners pinned current, ${f.percent} percent`)}"><span style="width:${f.percent}%"></span></div>
<p class="pct"><strong>${f.percent}%</strong> foundation owners pinned current (${f.ownersCurrent}/${f.ownersRequired})</p>`
    : `<p class="pct"><strong>n/a</strong> no foundation rows apply to this app</p>`;
  const rk = a.rkReady.length + a.toMove.length;
  const dl = [
    ["Profile", a.profile ?? "none"],
    ["Rows declared or ruled", f.rowsTotal ? `${rowCoverage(f)}${f.rowsOpen ? `; open: ${f.openRows.map((r) => r.id).join(", ")}` : ""}` : "-"],
    ["Add-ons pinned", `${p.addons.adopted}/${p.addons.total} (${p.addons.current} current, ${p.addons.stale} stale)`],
    ["Exceptions", `${x.open} open, ${x.expired} expired, ${x.withoutIssueOrExpiry} without issue or expiry`],
    ["@rightkit/release", releasePinText(p.releasePin)],
    ["pre-push hook", hookText(a.flags.prePushHook)],
    ["switches.json", yesNo(a.flags.switches)],
    ["journeys.toml", yesNo(a.flags.journeys)],
    ["right-release.config", releaseConfigText(a.flags.releaseConfig)],
    ["Check (preflight)", checkText(p.check)],
  ].map(([k, v]) => `<dt>${esc(k)}</dt><dd>${esc(v)}</dd>`).join("");
  const rkRows = [
    ...a.rkReady.map((r) => `<tr><td>RK-ready</td><td><code>${esc(r.name)}</code><br><small>${esc(r.path)}</small></td><td>${esc(r.kind)} ${esc(r.version)}<br><small>${esc(r.publish)}</small></td><td>${esc(r.statusText)}</td></tr>`),
    ...a.toMove.map((m) => `<tr><td>To move</td><td><code>${esc(m.package)}</code></td><td>${esc(m.kind)}</td><td>${esc(plural(m.count, "MOVE row"))}${m.exists ? "" : "; new package needed"}</td></tr>`),
  ].join("");
  const panel = rk ? `<details class="rk"><summary>RK-ready &amp; to-move (${rk})</summary><table><thead><tr><th scope="col">Section</th><th scope="col">Name</th><th scope="col">Kind and version</th><th scope="col">Status</th></tr></thead><tbody>${rkRows}</tbody></table></details>`
    : `<details class="rk"><summary>RK-ready &amp; to-move (0)</summary><p class="muted">No RK-ready package inside this app and no MOVE row for it.</p></details>`;
  return `<article class="card" aria-labelledby="${id}" data-card="${esc(a.name)}"><h3 id="${id}">${esc(a.name)} <small>${esc(a.group)}</small></h3>${figure}<dl>${dl}</dl>${panel}</article>`;
}

const groupAttr = (a) => (a.group === GROUP_MANDATORY ? "" : ` class="other-group" title="${esc(a.group)}"`);

function htmlMatrix(model) {
  const apps = model.apps;
  const colsOf = (a) => 1 + a.appleTargets.length;
  const totalCols = 4 + apps.reduce((n, a) => n + colsOf(a), 0) + 1;
  const head1 = ["<th scope=\"col\" rowspan=\"2\" class=\"pkg\">Package</th>", "<th scope=\"col\" rowspan=\"2\">Class</th>", "<th scope=\"col\" rowspan=\"2\">Rows</th>", "<th scope=\"col\" rowspan=\"2\">Latest</th>",
    ...apps.map((a) => `<th scope="colgroup" colspan="${colsOf(a)}" data-app="${esc(a.name)}"${groupAttr(a)}>${esc(a.name)}<small class="muted"> ${esc(a.group)}</small></th>`),
    "<th scope=\"col\" rowspan=\"2\">To move from</th>"];
  const head2 = apps.flatMap((a) => [
    `<th scope="col" data-app="${esc(a.name)}"${groupAttr(a)}>${a.appleTargets.length ? "app" : esc(a.present ? "state" : "not present")}</th>`,
    ...a.appleTargets.map((t) => `<th scope="col" class="apple" data-app="${esc(a.name)}" title="${esc(t.file)}">Apple: ${esc(path.posix.basename(path.posix.dirname(t.file)) === "." ? t.file : path.posix.basename(path.posix.dirname(t.file)))} <small class="muted">${esc(path.posix.basename(t.file))}</small></th>`),
  ]);
  const body = appsByDomain(model).map(({ domain, packages }) => {
    const rows = packages.map((pkg) => {
      const cells = apps.flatMap((a) => {
        const c = pkg.cells[a.name];
        const flagged = c.exceptions && (c.exceptions.expired || c.exceptions.withoutIssue || c.exceptions.withoutExpiry);
        const main = `<td class="${stateClass(c.state)}${flagged ? " flag" : ""}${a.group === GROUP_MANDATORY ? "" : " other-group"}" data-app="${esc(a.name)}" data-state="${esc(c.state)}"${c.exceptions ? " data-exc=\"1\"" : ""}><span class="st">${esc(c.state)}</span>${c.detail ? `<span class="dt">${esc(c.detail)}</span>` : ""}</td>`;
        const apple = a.appleTargets.map((t) => (pkg.kind === "swift"
          ? `<td class="apple ${stateClass(t.state)}" data-app="${esc(a.name)}" data-state="${esc(t.state)}"><span class="st">${esc(t.state)}</span><span class="dt">${esc(t.detail)}${t.products.length ? `; ${t.products.length} product${t.products.length === 1 ? "" : "s"}` : ""}</span></td>`
          : `<td class="apple blank" data-app="${esc(a.name)}"></td>`));
        return [main, ...apple];
      });
      return `<tr data-name="${esc(pkg.name.toLowerCase())}" data-class="${esc(pkg.class)}" data-domain="${esc(pkg.domain)}"><th scope="row" class="pkg"><code>${esc(pkg.name)}</code><small>${esc(pkg.kind)}${pkg.private ? ", private" : ""}</small></th>`
        + `<td><span class="chip c-${esc(pkg.class)}">${esc(pkg.class)}</span></td><td>${esc(pkg.mandatoryRows.join(", ") || "-")}</td><td>${esc(latestText(pkg))}</td>${cells.join("")}<td>${esc(moveFromText(pkg) || "-")}</td></tr>`;
    }).join("\n");
    return `<tbody data-domain="${esc(domain.id)}"><tr class="dom"><th scope="colgroup" colspan="${totalCols}"><span class="lbl">${esc(domain.name)} <small class="muted">(${packages.length})</small></span></th></tr>\n${rows}</tbody>`;
  }).join("\n");
  return `<table id="matrix"><caption class="vh">RightKit packages by app: state of each pairing</caption><thead><tr>${head1.join("")}</tr><tr>${head2.join("")}</tr></thead>\n${body}\n</table>`;
}

export function renderHtml(model) {
  const states = model.legend.map((l) => l.state);
  const option = (value, label) => `<option value="${esc(value)}">${esc(label)}</option>`;
  const appleRows = model.apps.flatMap((a) => a.appleTargets.map((t) => `<tr><td>${esc(a.name)}</td><td><code>${esc(t.file)}</code></td><td>${esc(t.name)}</td><td>${esc(t.pinKind)}</td><td>${esc(t.pinVersion || t.pinRef || "-")}</td><td>${esc(t.products.join(", ") || "-")}</td><td class="${stateClass(t.state)}">${esc(t.state)} (${esc(t.detail)})</td></tr>`));
  const needed = model.newPackagesNeeded.map((n) => `<tr><td><code>${esc(n.package)}</code></td><td>${n.total}</td><td>${esc(Object.entries(n.byApp).map(([app, count]) => `${app} ${count}`).join(", "))}</td></tr>`);
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<title>RightKit port matrix</title>
<style>${HTML_CSS}</style>
</head>
<body>
<header>
<h1>RightKit port matrix</h1>
<p id="stamp">${esc(stampLine(model))}</p>
<p class="lede">Every RightKit package against every app: what is where, and how far each app has ported. Generated by <code>node scripts/generate-index.mjs</code>; do not hand-edit. The data is in <a href="port-matrix.json">port-matrix.json</a>, the readable form in <a href="PORT-MATRIX.md">PORT-MATRIX.md</a>. This page is self-contained and makes no network request.</p>
</header>
<main>
<section aria-labelledby="h-progress">
<h2 id="h-progress">Progress by app</h2>
<p class="lede">The bar is foundation owners pinned current divided by foundation owners required for the app's profile. Add-on adoption is reported beside it and never folded into it.</p>
<div class="cards">
${model.apps.map(htmlCard).join("\n")}
</div>
</section>
<section aria-labelledby="h-legend">
<h2 id="h-legend">Legend</h2>
<ul class="legend">
${model.legend.map((l) => `<li><span class="chip ${stateClass(l.state)}">${esc(l.state)}</span><span>${esc(l.meaning)}</span></li>`).join("\n")}
${model.classLegend.map((c) => `<li><span class="chip c-${esc(c.class)}">${esc(c.class)}</span><span>${esc(c.meaning)}</span></li>`).join("\n")}
</ul>
</section>
<section aria-labelledby="h-matrix">
<h2 id="h-matrix">Matrix</h2>
<form id="filters" aria-label="Matrix filters">
<label>App<select id="f-app">${option("", "All apps")}${model.apps.map((a) => option(a.name, a.name)).join("")}</select></label>
<label>Class<select id="f-class">${option("", "All classes")}${model.classLegend.map((c) => option(c.class, c.class)).join("")}</select></label>
<label>Domain<select id="f-domain">${option("", "All domains")}${model.domains.map((d) => option(d.id, d.name)).join("")}</select></label>
<label>State<select id="f-state">${option("", "All states")}${states.map((s) => option(s, s)).join("")}</select></label>
<label>Search package<input id="f-q" type="search" autocomplete="off" placeholder="package name"></label>
<button type="reset">Reset filters</button>
<output id="count" aria-live="polite" for="f-app f-class f-domain f-state f-q"></output>
</form>
<div class="scroll" role="region" aria-label="Port matrix table" tabindex="0">
${htmlMatrix(model)}
</div>
</section>
<section aria-labelledby="h-apple">
<h2 id="h-apple">Apple targets</h2>
<div style="overflow-x:auto">${appleRows.length ? `<table class="table-plain"><thead><tr><th scope="col">App</th><th scope="col">Project file</th><th scope="col">Target</th><th scope="col">Pin kind</th><th scope="col">Version or ref</th><th scope="col">RightKitSwift products linked</th><th scope="col">State</th></tr></thead><tbody>${appleRows.join("")}</tbody></table>` : "<p class=\"muted\">No app has an Apple target that references RightKitSwift.</p>"}</div>
</section>
<section aria-labelledby="h-new">
<h2 id="h-new">New package needed</h2>
<div style="overflow-x:auto">${needed.length ? `<table class="table-plain"><thead><tr><th scope="col">Package</th><th scope="col">MOVE rows</th><th scope="col">From</th></tr></thead><tbody>${needed.join("")}</tbody></table>` : "<p class=\"muted\">None: every MOVE row names a package RightKit already has.</p>"}</div>
</section>
</main>
<script>${HTML_JS}</script>
</body>
</html>
`;
}

// ---------- entry points ----------

// The three outputs for a model, keyed by repo-relative path.
export function renderPortMatrix(model) {
  return { [PORT_MATRIX_MD]: `${renderMarkdown(model)}\n`, [PORT_MATRIX_HTML]: renderHtml(model), [PORT_MATRIX_JSON]: renderJson(model) };
}

export function generatePortMatrix(options = {}) {
  const model = buildPortMatrixModel(options);
  return { model, outputs: renderPortMatrix(model) };
}

const normalise = (text) => String(text).replace(/\r\n/g, "\n");

// Files whose committed text differs from `outputs` (a missing file counts as drift).
export function driftOfPortMatrix(root, outputs) {
  return Object.keys(outputs).filter((rel) => {
    const file = path.join(root, rel);
    return !existsSync(file) || normalise(readFileSync(file, "utf8")) !== outputs[rel];
  });
}

// Deterministic, repo-local check: the committed JSON is well formed and the committed MD and HTML are exactly its rendering.
// Returns [{ file, reason }]; empty when consistent.
export function checkPortMatrixConsistency(root) {
  const problems = [];
  const jsonFile = path.join(root, PORT_MATRIX_JSON);
  if (!existsSync(jsonFile)) return PORT_MATRIX_OUTPUTS.map((file) => ({ file, reason: "missing; run node scripts/generate-index.mjs" }));
  let model;
  try { model = JSON.parse(readFileSync(jsonFile, "utf8")); } catch (error) { return [{ file: PORT_MATRIX_JSON, reason: `not valid JSON: ${error.message}` }]; }
  if (!isObject(model) || model.schema !== SCHEMA || !Array.isArray(model.apps) || !Array.isArray(model.packages)) {
    return [{ file: PORT_MATRIX_JSON, reason: `not a schema ${SCHEMA} port matrix` }];
  }
  let rendered;
  try { rendered = renderPortMatrix(model); } catch (error) { return [{ file: PORT_MATRIX_JSON, reason: `cannot be rendered: ${error.message}` }]; }
  for (const rel of driftOfPortMatrix(root, rendered)) problems.push({ file: rel, reason: "committed file is not the rendering of docs/port-matrix.json" });
  return problems;
}

// ---------- console summary ----------

export function summaryLines(model) {
  const lines = [`port matrix: ${model.packages.length} packages x ${model.apps.length} apps (${model.packages.filter((p) => p.class === "foundation").length} foundation, ${model.packages.filter((p) => p.class === "add-on").length} add-on, ${model.packages.filter((p) => p.class === "tool").length} tool)`];
  const width = Math.max(...model.apps.map((a) => a.name.length));
  for (const a of model.apps) {
    if (!a.present) { lines.push(`  ${a.name.padEnd(width)}  not present`); continue; }
    const f = a.progress.foundation, x = a.progress.exceptions, d = a.progress.addons;
    const figure = f.ownersRequired ? `${percentText(f.percent).padStart(4)} (${f.ownersCurrent}/${f.ownersRequired} owners current)` : "n/a (no foundation rows)";
    lines.push(`  ${a.name.padEnd(width)}  ${figure}; rows ${f.rowsDeclared + f.rowsRuled}/${f.rowsTotal}; add-ons ${d.adopted}/${d.total} pinned (${d.current} current, ${d.stale} stale); exceptions ${x.total} (${x.expired} expired, ${x.withoutIssueOrExpiry} without issue or expiry); check ${checkText(a.progress.check)}`);
  }
  const other = model.packages.filter((p) => p.domain === OTHER_DOMAIN.id).map((p) => p.name);
  if (other.length) lines.push(`  no domain assigned: ${other.join(", ")}`);
  if (model.newPackagesNeeded.length) lines.push(`  new package needed: ${model.newPackagesNeeded.map((n) => `${n.package} (${n.total})`).join(", ")}`);
  return lines;
}

export function printPortMatrixSummary(model) {
  for (const line of summaryLines(model)) console.log(line);
}
