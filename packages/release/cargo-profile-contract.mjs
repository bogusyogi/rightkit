// RightKit standard Cargo profile contract (Adrian, 2026-10-16; Astra-reviewed option C).
//
// Every app workspace-root Cargo.toml carries the tables of templates/cargo-profiles.toml verbatim:
//   - [profile.dev] and [profile.dev.package."*"] equal the template (no missing, changed or extra keys),
//   - panic = "unwind" in dev and release,
//   - no [profile.release-iterate] (and nothing that inherits it),
//   - no [profile.test] overrides (test inherits dev).
// The same scan also flags the two source patterns the single dev profile makes wrong, because a dev
// build now has debug_assertions on:
//   - `cfg_attr(not(debug_assertions), windows_subsystem = ...)`: an installed dev app would open a
//     console window on Windows. Set windows_subsystem = "windows" unconditionally.
//   - an environment-variable override gated by cfg(debug_assertions): move it to the app's test-mode
//     feature. Heuristic: a cfg(debug_assertions) attribute or cfg! within 12 lines before an env read.
//     Suppress a reviewed site with a `rightkit-allow: debug-assertions-env` comment on the cfg line or
//     the line above it.
// The template file is the single source: this module reads it, it never copies the values.
import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parse } from "smol-toml";
import { findCargoManifests, isExcludedScanDirectory } from "./cargo-contract.mjs";
import { defaultReleaseConfigFile, isJsonReleaseConfig } from "./release-config.mjs";

export const CARGO_PROFILE_TEMPLATE = fileURLToPath(new URL("./templates/cargo-profiles.toml", import.meta.url));
export const RETIRED_PROFILE = "release-iterate";
const ENV_ALLOW_MARKER = "rightkit-allow: debug-assertions-env";
const ENV_WINDOW_LINES = 12;
// Never part of an app's shipped source: tests, examples, benches and vendored reference copies.
const SOURCE_SKIP = new Set(["tests", "examples", "benches", "reference"]);
const ENV_READ = /env::var|var_os|env::set_var|env::remove_var|\benv!\s*\(|option_env!|_env\(|env_var|env_nonempty/;
const WINDOWS_SUBSYSTEM_GATED = /cfg_attr\s*\((?:(?!\]).)*?not\s*\(\s*debug_assertions\s*\)(?:(?!\]).)*?windows_subsystem/gs;

export function loadCargoProfileTemplate(file = CARGO_PROFILE_TEMPLATE) {
  const profile = parse(readFileSync(file, "utf8")).profile ?? {};
  const { package: packages = {}, ...dev } = profile.dev ?? {};
  return { dev, devPackageStar: packages["*"] ?? {}, releasePanic: profile.release?.panic };
}

const isTable = (value) => value && typeof value === "object" && !Array.isArray(value);
const show = (value) => JSON.stringify(value);

// Compare one table with the template: every template key must match, and no other key may exist.
function compareTable(actual, expected, tableName, finding) {
  if (!isTable(actual)) { finding("profile-missing", `${tableName} is missing`); return; }
  for (const [key, value] of Object.entries(expected)) {
    if (!(key in actual)) finding("profile-key", `${tableName} is missing ${key} = ${show(value)}`);
    else if (actual[key] !== value) finding("profile-key", `${tableName} has ${key} = ${show(actual[key])}; the template requires ${show(value)}`);
  }
  for (const key of Object.keys(actual)) if (!(key in expected)) finding("profile-extra", `${tableName} has ${key}, which is not in the template`);
}

// Findings for one workspace-root manifest (already parsed).
export function checkManifestProfiles(manifest, template, file) {
  const findings = [];
  const finding = (rule, message) => findings.push({ rule, file, message });
  const profiles = isTable(manifest.profile) ? manifest.profile : {};
  const dev = isTable(profiles.dev) ? profiles.dev : undefined;
  const { package: devPackages, ...devRest } = dev ?? {};
  // Subtables of [profile.dev] are checked separately below.
  const devKeys = Object.fromEntries(Object.entries(devRest).filter(([, value]) => !isTable(value)));
  compareTable(dev ? devKeys : undefined, template.dev, "[profile.dev]", finding);
  compareTable(isTable(devPackages) ? devPackages["*"] : undefined, template.devPackageStar, '[profile.dev.package."*"]', finding);
  if (isTable(devPackages)) for (const name of Object.keys(devPackages)) if (name !== "*") finding("profile-extra", `[profile.dev.package.${show(name)}] is not in the template`);
  if (dev) for (const name of Object.keys(dev)) if (isTable(dev[name]) && name !== "package") finding("profile-extra", `[profile.dev.${name}] is not in the template`);
  // [profile.dev] panic is checked with the rest of the dev table above.
  if (profiles.release?.panic !== template.releasePanic) finding("panic", `[profile.release] panic must be ${show(template.releasePanic)} (found ${show(profiles.release?.panic)})`);
  if (RETIRED_PROFILE in profiles) finding("retired-profile", `[profile.${RETIRED_PROFILE}] is retired; delete it and its package overrides`);
  for (const [name, table] of Object.entries(profiles)) {
    if (isTable(table) && table.inherits === RETIRED_PROFILE) finding("retired-profile", `[profile.${name}] inherits the retired ${RETIRED_PROFILE} profile`);
  }
  if ("test" in profiles) finding("profile-extra", "[profile.test] overrides are not allowed; test inherits dev");
  return findings;
}

// Non-shipping workspaces: fixed prefixes plus the app's declared `cargoProfileExempt` directories.
const NON_SHIPPING_PREFIXES = ["qa", "benchmarks"];
const CARGO_PROFILE_EXEMPT_KEY = "cargoProfileExempt";
const STRING_LITERAL = /"([^"\\\n]*)"|'([^'\\\n]*)'/g;

// This scanner is synchronous and has only the app root, so it reads `cargoProfileExempt` from the release config file itself and
// never imports it. A JSON config (the one `defaultReleaseConfigFile` picks when it exists) is parsed as data: the array must hold
// plain strings. A legacy `.mjs` config, which release.mjs still imports as an ES module, is read as text: the literal array must
// hold plain string literals. Either way every entry names a directory inside the app, and anything else is a finding, never
// silently ignored. M24 command admission is not run here: this is a pure file reader.
export function readCargoProfileExempt(root) {
  const configFile = defaultReleaseConfigFile(root);
  const file = path.join(root, configFile);
  const findings = [];
  if (!existsSync(file)) return { paths: [], findings };
  let entries;
  if (isJsonReleaseConfig(configFile)) {
    let declared;
    try { declared = JSON.parse(readFileSync(file, "utf8").replace(/^\uFEFF/, ""))?.[CARGO_PROFILE_EXEMPT_KEY]; }
    catch (error) { return { paths: [], findings: [{ rule: "invalid-exempt", file: configFile, message: `is not valid JSON, so ${CARGO_PROFILE_EXEMPT_KEY} cannot be read: ${error.message}` }] }; }
    if (declared === undefined) return { paths: [], findings };
    if (!Array.isArray(declared) || declared.some((entry) => typeof entry !== "string")) findings.push({ rule: "invalid-exempt", file: configFile, message: `${CARGO_PROFILE_EXEMPT_KEY} must be an array of strings` });
    entries = Array.isArray(declared) ? declared.filter((entry) => typeof entry === "string") : [];
  } else {
    const match = new RegExp(`\\b${CARGO_PROFILE_EXEMPT_KEY}\\s*:\\s*\\[([^\\]]*)\\]`).exec(readFileSync(file, "utf8"));
    if (!match) return { paths: [], findings };
    const body = match[1];
    if (body.replace(STRING_LITERAL, "").replace(/[,\s]/g, "") !== "") {
      findings.push({ rule: "invalid-exempt", file: configFile, message: `${CARGO_PROFILE_EXEMPT_KEY} must be an array of string literals` });
    }
    entries = [...body.matchAll(STRING_LITERAL)].map(([, double, single]) => double ?? single);
  }
  const paths = [];
  for (const entry of entries) {
    const normalized = path.posix.normalize(entry.replace(/\\/g, "/")).replace(/\/+$/, "");
    if (normalized === "." || normalized === "" || path.posix.isAbsolute(normalized) || normalized.split("/").includes("..")) {
      findings.push({ rule: "invalid-exempt", file: configFile, message: `${CARGO_PROFILE_EXEMPT_KEY} entry ${JSON.stringify(entry)} must be a relative directory inside the app` });
    } else paths.push(normalized);
  }
  return { paths, findings };
}

const dirOf = (file) => path.posix.dirname(file);
const isWithinDir = (dir, ancestor) => ancestor === "." || dir === ancestor || dir.startsWith(`${ancestor}/`);

// The app's profile roots. Cargo reads profiles only from a workspace root, and a shipping app can ship binaries
// from several independent workspaces, so every manifest with a top-level [workspace] table is a root, and so is
// every standalone package manifest that no workspace contains. Only declared non-shipping directories are exempt:
// the fixed qa/ and benchmarks/ prefixes plus `cargoProfileExempt`. Exempt workspaces still count as containers.
export function findProfileRoots(root, manifests = findCargoManifests(root), declared = readCargoProfileExempt(root)) {
  const parsed = [];
  const findings = [...declared.findings];
  for (const manifest of manifests) {
    const file = path.relative(root, manifest).split(path.sep).join("/");
    try { parsed.push({ manifest, file, toml: parse(readFileSync(manifest, "utf8")) }); }
    catch (error) { findings.push({ rule: "invalid-toml", file, message: `is not valid TOML: ${error.message}` }); }
  }
  const exemptPrefixes = [...NON_SHIPPING_PREFIXES, ...declared.paths];
  const isWorkspace = ({ toml }) => isTable(toml.workspace);
  const roots = parsed.filter((entry) => {
    if (exemptPrefixes.some((prefix) => entry.file.startsWith(`${prefix}/`))) return false;
    if (isWorkspace(entry)) return true;
    return !parsed.some((other) => other !== entry && isWorkspace(other) && isWithinDir(dirOf(entry.file), dirOf(other.file)));
  });
  return { roots, findings };
}

function* rustFiles(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (isExcludedScanDirectory(entry.name, path.basename(dir)) || SOURCE_SKIP.has(entry.name)) continue;
      yield* rustFiles(full);
    } else if (entry.isFile() && entry.name.endsWith(".rs")) yield full;
  }
}

// Source patterns the single dev profile makes wrong (see the header). Returns findings with a line number.
export function scanDebugAssertionSources(root) {
  const findings = [];
  for (const file of rustFiles(root)) {
    const text = readFileSync(file, "utf8");
    if (!text.includes("debug_assertions")) continue;
    const label = path.relative(root, file).split(path.sep).join("/");
    const lineAt = (index) => text.slice(0, index).split("\n").length;
    for (const match of text.matchAll(WINDOWS_SUBSYSTEM_GATED)) {
      findings.push({ rule: "windows-subsystem", file: label, line: lineAt(match.index), message: 'cfg_attr(not(debug_assertions), windows_subsystem = ...) opens a console for installed dev apps; set windows_subsystem = "windows" unconditionally' });
    }
    const lines = text.split("\n");
    lines.forEach((line, index) => {
      const trimmed = line.trim();
      if (trimmed.startsWith("//") || !line.includes("debug_assertions") || line.includes("windows_subsystem")) return;
      if (!/#!?\[cfg|cfg!\s*\(/.test(line)) return;
      if (line.includes(ENV_ALLOW_MARKER) || (lines[index - 1] ?? "").includes(ENV_ALLOW_MARKER)) return;
      if (ENV_READ.test(lines.slice(index, index + ENV_WINDOW_LINES + 1).join("\n"))) {
        findings.push({ rule: "debug-assertions-env", file: label, line: index + 1, message: "environment-variable override gated by cfg(debug_assertions); move it to the app's test-mode feature" });
      }
    });
  }
  return findings;
}

export function scanCargoProfileContract(root, { template = loadCargoProfileTemplate(), sources = true } = {}) {
  const scanRoot = path.resolve(root);
  const { roots, findings } = findProfileRoots(scanRoot);
  for (const { toml, file } of roots) findings.push(...checkManifestProfiles(toml, template, file));
  if (sources && existsSync(scanRoot)) findings.push(...scanDebugAssertionSources(scanRoot));
  return findings;
}

export function formatCargoProfileFindings(findings, label) {
  return findings.map(({ rule, file, line, message }) => `${label} ${file}${line ? `:${line}` : ""} [${rule}] ${message}`);
}

// Throws one error listing every finding for the app; returns 0 when the app conforms.
export function assertCargoProfileContract(root, label = path.basename(path.resolve(root)), options) {
  const findings = scanCargoProfileContract(root, options);
  if (findings.length) {
    throw new Error(`${label} breaks the RightKit Cargo profile contract (templates/cargo-profiles.toml):\n${formatCargoProfileFindings(findings, label).join("\n")}`);
  }
  return 0;
}
