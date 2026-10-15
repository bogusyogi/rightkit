import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { breakingBump, loadRightkitVersions } from "./rightkit-drift.mjs";

export function validateQaBackdoorContract(root, packageScripts = {}) {
  if (!existsSync(path.join(root, "right-qa.config.mjs"))) return [];

  const issues = [];
  const cargoPath = path.join(root, "src-tauri", "Cargo.toml");
  if (!existsSync(cargoPath)) return ["right-qa.config.mjs requires src-tauri/Cargo.toml"];

  const cargoToml = readFileSync(cargoPath, "utf8");
  const dependency = cargoToml.match(/tauri-plugin-wdio-webdriver\s*=\s*\{([^}]*)\}/s)?.[1] ?? "";
  if (!dependency) issues.push("Cargo.toml must declare tauri-plugin-wdio-webdriver");
  else if (!/optional\s*=\s*true/.test(dependency)) issues.push("tauri-plugin-wdio-webdriver must be optional");

  const qaFeature = cargoToml.match(/qa-native\s*=\s*\[([^\]]*)\]/s)?.[1] ?? "";
  if (!/dep:tauri-plugin-wdio-webdriver/.test(qaFeature)) {
    issues.push("qa-native must enable dep:tauri-plugin-wdio-webdriver");
  }

  const rustSource = readRustSources(path.join(root, "src-tauri", "src")).replace(/\s+/g, " ");
  if (!/cfg\s*\(\s*all\s*\(\s*not\s*\(\s*debug_assertions\s*\)\s*,\s*feature\s*=\s*"qa-native"\s*\)\s*\)/.test(rustSource)
      || !/compile_error!/.test(rustSource)) {
    issues.push("Rust must compile_error when qa-native is enabled outside debug builds");
  }
  if (!/cfg\s*\(\s*all\s*\(\s*debug_assertions\s*,\s*feature\s*=\s*"qa-native"\s*\)\s*\)/.test(rustSource)
      || !/tauri_plugin_wdio_webdriver::init/.test(rustSource)) {
    issues.push("WebDriver registration must require debug_assertions and qa-native");
  }
  if (!/RIGHTKIT_QA_NATIVE/.test(rustSource)) {
    issues.push("WebDriver registration must also require RIGHTKIT_QA_NATIVE");
  }

  for (const [name, command] of Object.entries(packageScripts)) {
    if (!/qa-native|RIGHTKIT_QA_NATIVE/.test(command)) continue;
    if (/^(qa|test|dev)(:|$)/.test(name)) continue;
    if (/(release|publish|build|dmg|installer|package)/i.test(name) || /\btauri\s+build\b/.test(command)) {
      issues.push(`package script ${name} must not enable native QA`);
    }
  }
  return issues;
}

export function assertQaBackdoorContract(root, packageScripts = {}) {
  const issues = validateQaBackdoorContract(root, packageScripts);
  if (issues.length) throw new Error(`production QA backdoor contract failed:\n- ${issues.join("\n- ")}`);
}

function readRustSources(root) {
  if (!existsSync(root)) return "";
  const files = [];
  const visit = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const absolute = path.join(dir, entry.name);
      if (entry.isDirectory()) visit(absolute);
      else if (entry.isFile() && entry.name.endsWith(".rs")) files.push(absolute);
    }
  };
  visit(root);
  return files.sort().map((file) => readFileSync(file, "utf8")).join("\n");
}

// ---------------------------------------------------------------- QA adoption (mandatory for every app)
//
// RightKit QA is a mandatory component: every Orthic Labs app must carry a recognised QA lane. Recognised lanes:
//  - the native wdio lane (`right-qa.config.mjs`, validated by validateQaBackdoorContract above),
//  - `@rightkit/qa` in any non-node_modules package.json (dependencies or devDependencies), and/or
//  - `rightkit-qa` in any Cargo.toml (dependencies, dev-dependencies or build-dependencies).
// `@rightkit/qa-chrome` is the web-UI companion; its presence is not enforced (a UI cannot be detected reliably)
// but its pin is lag-checked like the others.
// A pin behind the latest published version (rightkit-versions.json) is a warning; a breaking-bump lag (the
// `breakingBump` rule rightkit-drift uses) refuses. The only exemption is for repositories that are not apps
// (qa-adoption-exemptions.json, RightKit-owned: changing it takes a RightKit release). No per-app opt-out.

export const QA_EXEMPTIONS_FILE = "qa-adoption-exemptions.json";
const NPM_QA = "@rightkit/qa";
const NPM_QA_CHROME = "@rightkit/qa-chrome";
const CARGO_QA = "rightkit-qa";
const NPM_SECTIONS = ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"];
const SKIP_DIRS = /^(node_modules(\..*)?|target|vendor|dist|build|\.git|\.cache|\.right-release)$/;
const MAX_DEPTH = 6;
const EXEMPTION_KINDS = ["library", "tooling"];

const numeric = (value) => String(value).split("-")[0].split(".").map((part) => Number.parseInt(part, 10) || 0);
const newer = (candidate, current) => { const a = numeric(candidate); const b = numeric(current); for (let i = 0; i < 3; i += 1) if (a[i] !== b[i]) return a[i] > b[i]; return false; };
const latestFor = (versions, ecosystem, name) => versions.registry
  ? versions.registry[ecosystem]?.[name]
  : ecosystem === "npm" ? versions.stagedNpm?.[name] ?? versions.npm?.[name] : versions.stagedCargo?.[name] ?? versions.cargo?.[name];

/** package.json and Cargo.toml files in the app tree, skipping node_modules (any suffix), target, vendor, dist and the RightKit tree. */
export function qaManifests(appRoot, { readdir = readdirSync } = {}) {
  const found = [];
  const visit = (dir, depth) => {
    let entries;
    try { entries = readdir(dir, { withFileTypes: true }); } catch { return; }
    for (const entry of entries) {
      const relative = path.relative(appRoot, path.join(dir, entry.name)).split(path.sep).join("/");
      if (entry.isDirectory()) {
        if (depth < MAX_DEPTH && !SKIP_DIRS.test(entry.name) && relative !== "rightkit" && relative !== "tools/rightkit") visit(path.join(dir, entry.name), depth + 1);
      } else if (entry.name === "package.json" || entry.name === "Cargo.toml") found.push(relative);
    }
  };
  visit(appRoot, 0);
  return found.sort();
}

/** `[dependencies]`-style section header that holds dependencies (including target-specific ones). */
const DEPENDENCY_HEADER = /^\[\s*(?:target\.[^\]]+\.)?(?:dev-|build-)?dependencies\s*\]/;
const DEPENDENCY_TABLE_HEADER = /^\[\s*(?:target\.[^\]]+\.)?(?:dev-|build-)?dependencies\.([A-Za-z0-9_-]+)\s*\]/;
const WORKSPACE_DEPENDENCY = /^\[\s*workspace\.dependencies\s*\]/;

/** `rightkit-qa` declarations in a Cargo.toml: [{ version | null }]. Path, git and workspace deps carry no version. */
export function cargoQaPins(text, crate = CARGO_QA) {
  const pins = [];
  let section = null;
  let table = null;
  const push = (body) => {
    const version = body.match(/\bversion\s*=\s*"[=^~<>\s]*(\d+\.\d+\.\d+)/)?.[1] ?? null;
    pins.push({ version: /\bpath\s*=|\bgit\s*=/.test(body) ? null : version });
  };
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.replace(/\s+#.*$/, "").trim();
    if (!line || line.startsWith("#")) continue;
    if (line.startsWith("[")) {
      table = line.match(DEPENDENCY_TABLE_HEADER);
      if (table && table[1] === crate) { section = "table"; pins.push({ version: null, open: true }); continue; }
      section = DEPENDENCY_HEADER.test(line) || WORKSPACE_DEPENDENCY.test(line) ? "deps" : null;
      continue;
    }
    if (section === "table") {
      const open = pins.at(-1);
      const version = line.match(/^version\s*=\s*"[=^~<>\s]*(\d+\.\d+\.\d+)/)?.[1];
      if (version) open.version = version;
      if (/^(path|git)\s*=/.test(line)) open.local = true;
      continue;
    }
    if (section === "deps") {
      const match = line.match(new RegExp(`^${crate}(?:\\.[A-Za-z-]+)?\\s*=\\s*(.*)$`));
      if (match) push(match[1]);
    }
  }
  return pins.map(({ version, local }) => ({ version: local ? null : version }));
}

/** `@rightkit/qa` style declarations in one parsed package.json. */
export function npmQaPins(manifest, name) {
  const pins = [];
  for (const section of NPM_SECTIONS) {
    const spec = manifest?.[section]?.[name];
    if (typeof spec !== "string") continue;
    pins.push({ version: spec.match(/^[\^~=]?(\d+\.\d+\.\d+)$/)?.[1] ?? null });
  }
  return pins;
}

/** Normalise a git remote to `host/owner/repo` lowercase, so ssh and https spellings of one repository agree. */
export function normalizeRepositoryIdentity(url) {
  const text = String(url ?? "").trim().replace(/\.git$/i, "").replace(/\/+$/, "");
  const ssh = text.match(/^(?:ssh:\/\/)?git@([^:/]+)[:/](.+)$/);
  if (ssh) return `${ssh[1]}/${ssh[2]}`.toLowerCase();
  return text.replace(/^[a-z+]+:\/\//i, "").replace(/^[^@/]+@/, "").toLowerCase();
}

function originRemote(root) {
  const result = spawnSync("git", ["-C", root, "remote", "get-url", "origin"], { encoding: "utf8", windowsHide: true });
  return result.status === 0 ? String(result.stdout).trim() : null;
}

function loadExemptions() {
  try { return JSON.parse(readFileSync(new URL(`./${QA_EXEMPTIONS_FILE}`, import.meta.url), "utf8")); } catch { return { nonAppRepositories: [] }; }
}

/**
 * The exemption for a repository that is not an app, or null. Matched by git `origin` identity against the
 * RightKit-owned list; the RightKit source tree itself is recognised by its release package, with no list entry.
 */
export function qaExemption({ appRoot, repoRoot }, { exemptions = loadExemptions(), remote = originRemote, exists = existsSync, readFile = readFileSync } = {}) {
  for (const root of [appRoot, repoRoot].filter(Boolean)) {
    try {
      const manifest = path.join(root, "packages", "release", "package.json");
      if (exists(manifest) && JSON.parse(readFile(manifest, "utf8")).name === "@rightkit/release") return { kind: "tooling", reason: "the RightKit source tree is not an app" };
    } catch { /* not the RightKit tree */ }
  }
  for (const entry of exemptions.nonAppRepositories ?? []) {
    if (!EXEMPTION_KINDS.includes(entry?.kind) || !entry.reason || !entry.repositoryIdentity) continue;
    for (const root of [appRoot, repoRoot].filter(Boolean)) {
      const origin = remote(root);
      if (origin && normalizeRepositoryIdentity(origin) === normalizeRepositoryIdentity(entry.repositoryIdentity)) return { kind: entry.kind, reason: entry.reason };
    }
  }
  return null;
}


/** Pure evaluation: { exempt, lane, refusals[], warnings[] }. */
export function evaluateQaAdoption({ appRoot, repoRoot, app }, deps = {}) {
  const { readFile = readFileSync, exists = existsSync, versions = loadRightkitVersions() } = deps;
  const exempt = qaExemption({ appRoot, repoRoot }, deps);
  if (exempt) return { exempt, lane: null, refusals: [], warnings: [] };
  const files = deps.manifests ?? qaManifests(appRoot, deps);
  const pins = [];
  const unreadable = [];
  for (const file of files) {
    let text;
    try { text = readFile(path.join(appRoot, file), "utf8"); } catch (error) { unreadable.push(`${file}: ${error.message}`); continue; }
    if (file.endsWith("package.json")) {
      let manifest;
      try { manifest = JSON.parse(text); } catch { unreadable.push(`${file}: not valid JSON`); continue; }
      for (const name of [NPM_QA, NPM_QA_CHROME]) for (const pin of npmQaPins(manifest, name)) pins.push({ ecosystem: "npm", name, file, ...pin });
    } else {
      for (const pin of cargoQaPins(text)) pins.push({ ecosystem: "cargo", name: CARGO_QA, file, ...pin });
    }
  }
  const label = app ?? "this app";
  const adopted = [...new Set(pins.filter((pin) => pin.name === NPM_QA || pin.name === CARGO_QA).map((pin) => pin.name))];
  const lane = exists(path.join(appRoot, "right-qa.config.mjs")) ? ["native wdio lane (right-qa.config.mjs)", ...adopted] : adopted;
  const refusals = [];
  const warnings = [];
  if (lane.length === 0) {
    refusals.push(`RightKit QA is mandatory for every Orthic Labs app, and ${label} has no QA lane: no right-qa.config.mjs, no ${NPM_QA} in a package.json, no ${CARGO_QA} in a Cargo.toml${unreadable.length ? ` (unreadable: ${unreadable.join("; ")})` : ""}. Adopt it: add ${NPM_QA} to devDependencies (plus ${NPM_QA_CHROME} for a web UI) and/or ${CARGO_QA} to Cargo.toml [dev-dependencies], add the rightkit-qa floor run (smoke.launch, journey), see docs/ADOPTION.md, then run: node rightkit-drift.mjs --apply. Only a repository that is not an app is exempt (${QA_EXEMPTIONS_FILE}); an app cannot opt out`);
  }
  const lagging = pins
    .map((pin) => ({ ...pin, latest: pin.version && latestFor(versions, pin.ecosystem, pin.name) }))
    .filter((pin) => pin.latest && newer(pin.latest, pin.version))
    .map((pin) => ({ ...pin, breaking: breakingBump(pin.version, pin.latest) }));
  const describe = (list) => list.map((pin) => `${pin.name} ${pin.version} -> ${pin.latest}${pin.breaking ? " (breaking)" : ""} (${pin.file})`).join("; ");
  const breaking = lagging.filter((pin) => pin.breaking);
  const soft = lagging.filter((pin) => !pin.breaking);
  if (breaking.length) refusals.push(`${label} runs RightKit QA behind a breaking version: ${describe(breaking)}. Move the QA pins forward and adapt the QA setup: node rightkit-drift.mjs --apply --all`);
  if (soft.length) warnings.push(`${label} runs RightKit QA behind the latest published version: ${describe(soft)}. Update with: node rightkit-drift.mjs --apply`);
  return { exempt: null, lane, refusals, warnings };
}

/** Release gate: throws when the app has no QA lane or lags a breaking version; warnings go to `warn`. */
export function assertQaAdoption(opts, deps = {}) {
  const { warn = (line) => console.error(line) } = deps;
  const result = evaluateQaAdoption(opts, deps);
  for (const line of result.warnings) warn(`[right-release] QA warning: ${line}`);
  if (result.refusals.length) throw new Error(`QA adoption contract failed:
- ${result.refusals.join("
- ")}`);
  return result;
}
