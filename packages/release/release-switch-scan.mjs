// Release-time enforcement of M23 ("release code cannot use env/settings to select a test, data-root,
// executable, library or identity") and of the release half of M21 ("exclude the test switch from release
// builds"). Provisional owner decision D4: `rightkit_service::mode` is the runtime decision (a release build
// reads no override selector at all); this module is the proof, at release time, outside the app.
//
// Three layers, one vocabulary (selector classes S1-S6, see `README.md` "Release switch scan"):
//
//   1. Feature closure (source layer). `cargo tree -e features --target <triple>` output must not enable a
//      forbidden feature anywhere in the resolved graph (`featureClosureFindings`). Declared feature names alone
//      are not enough: a transitive crate can switch `test-mode` on. Finding id: `forbidden-feature <name>`.
//   2. Inventory completeness (source layer). Every app commits `switches.json` (schema below). The source walker
//      (`sourceSwitchFindings`) finds every environment read in app source and rejects any not in the inventory,
//      any S1-S4 inventory entry that is not compiled out of release builds, and any S5/S6 entry that breaks its
//      class rule. Finding ids: `unregistered-read`, `dynamic-read`, `not-compiled-out`, `inventory-invalid`.
//   3. Binary layer. Native binaries must not carry the test-mode marker (`test-mode-capability`) nor any
//      registry or app-declared deny name from classes S1-S4 (`release-switch-names`), matched as Latin-1/UTF-8
//      and as UTF-16LE (Windows wide strings). The scan returns a receipt: sha256 of every scanned file.
//
// Not claimed: packed installers (NSIS, DMG, compressed MSIX members) are covered only by the pre-pack scan of
// the staged payload; symlinks are skipped; browser JS is checked at the source layer only; argv flags are
// enforced only for deny-listed flags (a generic walker cannot tell a CLI definition from a hidden test flag).
//
// `switches.json` schema (one per app, committed at the repository root or `src-tauri/`):
//   {
//     "schema": 1,
//     "app": "viewright",
//     "switches": [
//       { "name": "HEARDRIGHT_ENGINE_TEST_MODE",         // env var, argv flag, settings key or VITE_* name
//         "class": "S1",                                  // S1 test activation | S2 authority/identity |
//                                                         // S3 executable/library/model | S4 state/data root |
//                                                         // S5 user-content location | S6 diagnostics/tuning
//         "channel": "env",                               // env | argv | settings | preference | frontend
//         "anchor": "src/test_mode.rs:97",                // where it is read (informational)
//         "compiledOut": "feature:test-mode",             // required for S1-S4: feature:<name> | cfg:test |
//                                                         // cfg:debug_assertions (read exists only there)
//         "valueType": "bool",                            // required for S6: bool | int | enum
//         "userGesture": true,                            // required for S5: chosen by a user gesture
//         "note": "free text" } ],
//     "dynamicReads": [ { "file": "src/x.rs", "reason": "..." } ]   // optional: files allowed a non-literal read
//   }
import { createHash } from "node:crypto";
import { closeSync, lstatSync, openSync, readFileSync, readSync, readdirSync } from "node:fs";
import path from "node:path";
import { isNativeBinaryHeader } from "./qa-control-scan.mjs";

export const TEST_MODE_MARKER = "RIGHTKIT-TEST-MODE-COMPILED"; // identical to `TEST_MODE_MARKER_TEXT` in rightkit-service/src/mode.rs
const CHUNK = 4 * 1024 * 1024;
const FORBIDDEN_CLASSES = new Set(["S1", "S2", "S3", "S4"]);
const CLASSES = new Set(["S1", "S2", "S3", "S4", "S5", "S6"]);
const CHANNELS = new Set(["env", "argv", "settings", "preference", "frontend"]);
const COMPILED_OUT = /^(feature:[A-Za-z0-9_-]+|cfg:test|cfg:debug_assertions)$/;

export const REGISTRY = JSON.parse(readFileSync(new URL("./release-switch-registry.json", import.meta.url), "utf8"));

// ---------------------------------------------------------------------------------------------------------------------
// Layer 1: feature closure.

// `cargo tree -e features` lines look like: `├── rightkit-service feature "test-mode"` (possibly `(*)` suffixed).
const FEATURE_LINE = /^[\s│├└─|`+\\-]*([A-Za-z0-9_][A-Za-z0-9_-]*)(?: v[^\s]+)? feature "([^"]+)"/;

export function featureClosureFindings(treeText, { forbidden = REGISTRY.forbiddenFeatures } = {}) {
  const deny = new Set(forbidden);
  const findings = []; const seen = new Set();
  for (const line of String(treeText).split(/\r?\n/)) {
    const m = FEATURE_LINE.exec(line);
    if (!m || !deny.has(m[2])) continue;
    const id = `forbidden-feature ${m[2]}`;
    const key = `${m[1]}/${m[2]}`;
    if (seen.has(key)) continue;
    seen.add(key);
    findings.push({ layer: "feature-closure", rule: id, file: `<cargo tree>`, reason: `${id}: crate \`${m[1]}\` has feature \`${m[2]}\` enabled in the resolved graph` });
  }
  return findings;
}

// Findings for a build command (`--features test-mode`, `-F`, `--all-features`, ...). Uses the existing tokenizer.
export function commandSwitchFindings(argv, { forbidden = REGISTRY.forbiddenFeatures } = {}) {
  const deny = new Set(forbidden);
  const tokens = argv;
  const enabled = []; let all = false;
  for (let i = 0; i < tokens.length; i += 1) {
    const t = tokens[i];
    const take = (v) => { for (const p of String(v).split(/[,\s]+/)) if (p) enabled.push(p); };
    if (t === "--all-features") all = true;
    else if (["--features", "-F", "-f", "--feature"].includes(t)) { if (i + 1 < tokens.length) take(tokens[i + 1]); }
    else if (t.startsWith("--features=")) take(t.slice("--features=".length));
    else if (/^-F.+/.test(t)) take(t.slice(2));
  }
  const findings = [];
  for (const item of enabled) {
    const name = item.includes("/") ? item.slice(item.lastIndexOf("/") + 1) : item;
    if (deny.has(name)) findings.push({ layer: "feature-closure", rule: `forbidden-feature ${name}`, file: "<command>", reason: `forbidden-feature ${name}: build command enables \`${item}\`` });
  }
  if (all) findings.push({ layer: "feature-closure", rule: "forbidden-feature all-features", file: "<command>", reason: "forbidden-feature all-features: build command enables every feature, including test-mode" });
  return findings;
}

// ---------------------------------------------------------------------------------------------------------------------
// Layer 2: inventory completeness.

export function parseInventory(text, label = "switches.json") {
  const issues = [];
  let doc;
  try { doc = JSON.parse(text); } catch (error) { return { doc: null, issues: [{ rule: "inventory-invalid", file: label, reason: `inventory-invalid: ${label} is not valid JSON (${error.message})` }] }; }
  const bad = (reason) => issues.push({ layer: "inventory", rule: "inventory-invalid", file: label, reason: `inventory-invalid: ${reason}` });
  if (doc?.schema !== 1) bad("`schema` must be 1");
  if (typeof doc?.app !== "string" || !doc.app) bad("`app` must be a non-empty string");
  if (!Array.isArray(doc?.switches)) { bad("`switches` must be an array"); return { doc, issues }; }
  const names = new Set();
  for (const entry of doc.switches) {
    const where = `switch \`${entry?.name}\``;
    if (typeof entry?.name !== "string" || !entry.name) { bad("every switch needs a `name`"); continue; }
    if (names.has(entry.name)) bad(`${where} is listed twice`);
    names.add(entry.name);
    if (!CLASSES.has(entry.class)) bad(`${where}: \`class\` must be one of S1..S6`);
    if (!CHANNELS.has(entry.channel)) bad(`${where}: \`channel\` must be one of ${[...CHANNELS].join(", ")}`);
    if (FORBIDDEN_CLASSES.has(entry.class)) {
      if (typeof entry.compiledOut !== "string" || !COMPILED_OUT.test(entry.compiledOut)) {
        issues.push({ layer: "inventory", rule: "not-compiled-out", file: label, reason: `not-compiled-out: ${where} is class ${entry.class} and must declare \`compiledOut\` as feature:<name>, cfg:test or cfg:debug_assertions (release builds may not contain it)` });
      }
    } else if (entry.class === "S6") {
      if (!["bool", "int", "enum"].includes(entry.valueType)) bad(`${where}: class S6 needs a bounded \`valueType\` (bool, int or enum)`);
    } else if (entry.class === "S5") {
      if (entry.userGesture !== true) bad(`${where}: class S5 needs \`userGesture: true\` (chosen by a picker, saved preference or the user's own argument)`);
    }
  }
  return { doc, issues };
}

const SOURCE_EXT = new Set([".rs", ".ts", ".tsx", ".js", ".mjs", ".cjs", ".jsx", ".vue", ".svelte", ".swift", ".c", ".cc", ".cpp", ".h", ".m", ".mm"]);
const SKIP_DIRS = new Set(["node_modules", "target", "dist", "build", ".git", "tests", "__tests__", "benches", "examples", "fixtures", "test", "coverage", ".vite"]);
const TEST_FILE = /(\.test\.|\.spec\.|_tests?\.rs$|^tests?\.rs$)/;

// Reads of a literal name; group 1 is the name.
const LITERAL_READS = [
  /\b(?:std::)?env::var(?:_os)?\(\s*"([^"]+)"\s*\)/g,
  /\b(?:option_)?env!\(\s*"([^"]+)"/g,
  /\bgetenv\(\s*"([^"]+)"\s*\)/g,
  /process\.env\.([A-Za-z_][A-Za-z0-9_]*)/g,
  /process\.env\[\s*["']([^"']+)["']\s*\]/g,
  /import\.meta\.env\.([A-Za-z_][A-Za-z0-9_]*)/g,
  /import\.meta\.env\[\s*["']([^"']+)["']\s*\]/g,
  /environment\[\s*"([^"]+)"\s*\]/g,
];
// Reads whose name is not a literal.
const DYNAMIC_READS = [
  /\b(?:std::)?env::var(?:_os)?\(\s*[^")\s]/g,
  /\b(?:std::)?env::vars(?:_os)?\(\s*\)/g,
  /\bgetenv\(\s*[^")\s]/g,
  /process\.env\[\s*[^"'\]\s]/g,
  /import\.meta\.env\[\s*[^"'\]\s]/g,
];
// Builtins every JS runtime sets; they select nothing and are not part of the inventory.
const JS_BUILTINS = new Set(["MODE", "DEV", "PROD", "BASE_URL", "SSR", "NODE_ENV"]);

function* walkSource(root) {
  const stack = [root];
  while (stack.length) {
    const dir = stack.pop();
    let entries;
    try { entries = readdirSync(dir, { withFileTypes: true }); } catch { continue; }
    for (const entry of entries) {
      const full = path.join(dir, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) { if (!SKIP_DIRS.has(entry.name)) stack.push(full); continue; }
      if (entry.isFile() && SOURCE_EXT.has(path.extname(entry.name)) && !TEST_FILE.test(entry.name)) yield full;
    }
  }
}

function lineOf(text, index) { let n = 1; for (let i = 0; i < index; i += 1) if (text.charCodeAt(i) === 10) n += 1; return n; }

// inventoryText: contents of switches.json. Returns findings only.
export function sourceSwitchFindings({ sourceRoot, inventoryText, inventoryLabel = "switches.json", extraDeny = [] }) {
  const { doc, issues } = parseInventory(inventoryText, inventoryLabel);
  const findings = [...issues];
  if (!doc || !Array.isArray(doc.switches)) return findings;
  const registered = new Set(doc.switches.map((s) => s.name));
  const dynamicAllowed = new Set((doc.dynamicReads ?? []).map((d) => path.normalize(d.file)));
  const reads = new Set();
  const denyFlags = [...REGISTRY.denyNames, ...extraDeny.map((n) => ({ name: typeof n === "string" ? n : n.name }))].map((d) => d.name).filter((n) => n.startsWith("-"));
  for (const file of walkSource(sourceRoot)) {
    let text;
    try { text = readFileSync(file, "utf8"); } catch { continue; }
    const rel = path.relative(sourceRoot, file);
    for (const pattern of LITERAL_READS) {
      pattern.lastIndex = 0;
      for (let m = pattern.exec(text); m; m = pattern.exec(text)) {
        const name = m[1];
        if (JS_BUILTINS.has(name) && /\.(ts|tsx|js|mjs|cjs|jsx|vue|svelte)$/.test(file)) continue;
        reads.add(name);
        if (!registered.has(name)) findings.push({ layer: "inventory", rule: "unregistered-read", file: `${rel}:${lineOf(text, m.index)}`, reason: `unregistered-read: \`${name}\` is read here but is not listed in ${inventoryLabel}` });
      }
    }
    if (!dynamicAllowed.has(path.normalize(rel))) {
      for (const pattern of DYNAMIC_READS) {
        pattern.lastIndex = 0;
        for (let m = pattern.exec(text); m; m = pattern.exec(text)) {
          findings.push({ layer: "inventory", rule: "dynamic-read", file: `${rel}:${lineOf(text, m.index)}`, reason: `dynamic-read: an environment read with a non-literal name cannot be inventoried; list the file under \`dynamicReads\` in ${inventoryLabel} with a reason, or use a literal name` });
        }
      }
    }
    for (const flag of denyFlags) {
      const literal = `"${flag}"`;
      for (let at = text.indexOf(literal); at >= 0; at = text.indexOf(literal, at + 1)) {
        reads.add(flag);
        if (!registered.has(flag)) findings.push({ layer: "inventory", rule: "unregistered-read", file: `${rel}:${lineOf(text, at)}`, reason: `unregistered-read: deny-listed flag \`${flag}\` appears here but is not listed in ${inventoryLabel}` });
      }
    }
  }
  return findings;
}

// ---------------------------------------------------------------------------------------------------------------------
// Layer 3: binary layer.

export function denyNamesFor({ denyNames = REGISTRY.denyNames, appForbid = [] } = {}) {
  const out = new Map();
  for (const entry of denyNames) out.set(entry.name, entry.class ?? "S1");
  for (const item of appForbid) {
    const name = typeof item === "string" ? item : item.name;
    if (name && !out.has(name)) out.set(name, typeof item === "string" ? "S1" : item.class ?? "S1");
  }
  return out;
}

function needlesFor(denyMap) {
  const needles = [{ rule: "test-mode-capability", name: TEST_MODE_MARKER, buffers: [Buffer.from(TEST_MODE_MARKER, "latin1"), Buffer.from(TEST_MODE_MARKER, "utf16le")] }];
  for (const [name, cls] of denyMap) needles.push({ rule: "release-switch-names", name, cls, buffers: [Buffer.from(name, "latin1"), Buffer.from(name, "utf16le")] });
  return needles;
}

class SwitchScanner {
  constructor(denyMap) {
    this.needles = needlesFor(denyMap);
    this.max = Math.max(...this.needles.flatMap((n) => n.buffers.map((b) => b.length)));
    this.tail = Buffer.alloc(0); this.hits = new Map();
  }
  push(chunk) {
    const window = this.tail.length ? Buffer.concat([this.tail, chunk]) : chunk;
    for (const needle of this.needles) {
      if (this.hits.has(needle.name)) continue;
      if (needle.buffers.some((b) => window.includes(b))) this.hits.set(needle.name, needle);
    }
    this.tail = Buffer.from(window.subarray(Math.max(0, window.length - (this.max - 1))));
  }
  findings(file) {
    return [...this.hits.values()].map((n) => ({ layer: "binary", rule: n.rule, file, reason: n.rule === "test-mode-capability"
      ? `test-mode-capability: contains ${TEST_MODE_MARKER} (the \`test-mode\` capability is compiled in)`
      : `release-switch-names: contains the ${n.cls} selector name \`${n.name}\` (release code must not read it)` }));
  }
}

// One in-memory buffer (tests, sealed files). Non-native buffers yield no findings (use scanSwitchPaths for files).
export function bufferSwitchFindings(buffer, { denyMap = denyNamesFor(), file = "<buffer>" } = {}) {
  const bytes = Buffer.from(buffer);
  if (!isNativeBinaryHeader(bytes.subarray(0, 4))) return [];
  const scanner = new SwitchScanner(denyMap);
  for (let at = 0; at < bytes.length; at += CHUNK) scanner.push(bytes.subarray(at, Math.min(bytes.length, at + CHUNK)));
  return scanner.findings(file);
}

function scanFile(file, denyMap) {
  const fd = openSync(file, "r");
  try {
    const head = Buffer.alloc(4);
    const got = readSync(fd, head, 0, 4, 0);
    if (!isNativeBinaryHeader(head.subarray(0, got))) return null;
    const scanner = new SwitchScanner(denyMap);
    const hash = createHash("sha256");
    const buffer = Buffer.alloc(CHUNK);
    let bytes = 0;
    for (;;) {
      const read = readSync(fd, buffer, 0, CHUNK, null);
      if (read === 0) break;
      const chunk = buffer.subarray(0, read);
      hash.update(chunk); bytes += read; scanner.push(chunk);
    }
    return { sha256: hash.digest("hex"), bytes, findings: scanner.findings(file) };
  } finally { closeSync(fd); }
}

// paths: files or directories. Returns { findings, receipt: { files: [{ file, sha256, bytes }], binaries } }.
export function scanSwitchPaths(paths, { denyMap = denyNamesFor() } = {}) {
  const findings = []; const files = [];
  const visit = (target) => {
    let st;
    try { st = lstatSync(target); } catch { return; }
    if (st.isSymbolicLink()) return;
    if (st.isDirectory()) { for (const name of readdirSync(target).sort()) visit(path.join(target, name)); return; }
    if (!st.isFile()) return;
    const result = scanFile(target, denyMap);
    if (!result) return;
    files.push({ file: target, sha256: result.sha256, bytes: result.bytes });
    findings.push(...result.findings);
  };
  for (const target of paths) visit(target);
  return { findings, receipt: { files, binaries: files.length } };
}

// ---------------------------------------------------------------------------------------------------------------------
// Combined gate.

// Every layer is optional input; a layer given no input is skipped, except that `requireBinary` fails a scan that
// found no native binary. `bypass: ["feature-closure"]` exists only for the negative fixtures that prove the binary
// layer independently; release.mjs never passes it.
export function releaseSwitchFindings({ treeText, commands = [], sourceRoot, inventoryText, binaryPaths = [], appForbid = [], requireBinary = true, bypass = [] } = {}) {
  const findings = [];
  const bypassed = new Set(bypass);
  const denyMap = denyNamesFor({ appForbid });
  if (!bypassed.has("feature-closure")) {
    if (treeText != null) findings.push(...featureClosureFindings(treeText));
    for (const argv of commands) findings.push(...commandSwitchFindings(argv));
  }
  if (sourceRoot && inventoryText != null) findings.push(...sourceSwitchFindings({ sourceRoot, inventoryText, extraDeny: appForbid }));
  let receipt = { files: [], binaries: 0 };
  if (binaryPaths.length) {
    const scanned = scanSwitchPaths(binaryPaths, { denyMap });
    findings.push(...scanned.findings); receipt = scanned.receipt;
    if (requireBinary && receipt.binaries === 0 && findings.length === 0) findings.push({ layer: "binary", rule: "unscannable", file: binaryPaths.join(", "), reason: "unscannable: no Mach-O, PE or ELF file found to scan; cannot verify" });
  }
  return { findings, receipt };
}

export function assertNoReleaseSwitches(input, label = "release") {
  const { findings, receipt } = releaseSwitchFindings(input);
  if (findings.length === 0) return receipt;
  throw new Error(
    `${label}: release code must not contain or read a test, data-root, executable, library or identity selector; `
    + `${findings.length} finding(s):\n${findings.map((f) => `- ${f.file}: ${f.reason}`).join("\n")}\n`
    + "Build release and update tiers without the `test-mode` feature, and register every environment read in the app's switches.json.",
  );
}
