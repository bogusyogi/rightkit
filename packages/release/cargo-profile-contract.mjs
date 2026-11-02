// RightKit standard Cargo profile contract (Adrian, 2026-10-16; Astra-reviewed option C).
//
// One build output and one warm cache per app per host (GATE-CONTRACT GC-A01, GC-A07). Every gate compile (check, clippy,
// rustdoc, the test compile and both installers) builds with [profile.release] for the host's own target, whatever the caller
// passed (cross-targets.mjs oneOutput); the dev installer is the release profile plus the `qa-native` feature. So no gate build
// has debug_assertions, and the release tool's scans (not debug assertions) keep the QA control server out of a release.
// Every app workspace-root Cargo.toml therefore carries:
//   - [profile.release] with panic = "unwind" (templates/cargo-profiles.toml),
//   - no other profile table: no [profile.dev], [profile.test] or custom profile (each would be a second output directory),
//   - no [profile.release-iterate] (retired; nor anything that inherits it).
// The same scan also flags a source pattern that behaves differently between the profiles:
//   - an environment-variable override gated by cfg(debug_assertions): no gate build compiles it (tests and installers
//     are all release), so only a developer's own cargo run reaches it. Gate it on an explicit feature instead. Heuristic: a
//     cfg(debug_assertions) attribute or cfg! within 12 lines before an env read.
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

export function loadCargoProfileTemplate(file = CARGO_PROFILE_TEMPLATE) {
  const profile = parse(readFileSync(file, "utf8")).profile ?? {};
  return { releasePanic: profile.release?.panic };
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
  // One build output (GC-A01, GC-A07): no gate build uses any profile but release, so an app carries no other profile table.
  for (const name of Object.keys(profiles)) {
    if (name !== "release" && name !== RETIRED_PROFILE) finding("profile-extra", `[profile.${name}] is not allowed: every gate build uses [profile.release] for the host target (one output directory, GATE-CONTRACT GC-A07); delete the table`);
  }
  if (profiles.release?.panic !== template.releasePanic) finding("panic", `[profile.release] panic must be ${show(template.releasePanic)} (found ${show(profiles.release?.panic)})`);
  if (RETIRED_PROFILE in profiles) finding("retired-profile", `[profile.${RETIRED_PROFILE}] is retired; delete it and its package overrides`);
  for (const [name, table] of Object.entries(profiles)) {
    if (isTable(table) && table.inherits === RETIRED_PROFILE) finding("retired-profile", `[profile.${name}] inherits the retired ${RETIRED_PROFILE} profile`);
  }
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

// Source patterns that behave differently between the profiles (see the header). Returns findings with a line number.
export function scanDebugAssertionSources(root) {
  const findings = [];
  for (const file of rustFiles(root)) {
    const text = readFileSync(file, "utf8");
    if (!text.includes("debug_assertions")) continue;
    const label = path.relative(root, file).split(path.sep).join("/");
    const lineAt = (index) => text.slice(0, index).split("\n").length;
    const lines = text.split("\n");
    lines.forEach((line, index) => {
      const trimmed = line.trim();
      if (trimmed.startsWith("//") || !line.includes("debug_assertions") || line.includes("windows_subsystem")) return;
      if (!/#!?\[cfg|cfg!\s*\(/.test(line)) return;
      if (line.includes(ENV_ALLOW_MARKER) || (lines[index - 1] ?? "").includes(ENV_ALLOW_MARKER)) return;
      if (ENV_READ.test(lines.slice(index, index + ENV_WINDOW_LINES + 1).join("\n"))) {
        findings.push({ rule: "debug-assertions-env", file: label, line: index + 1, message: "environment-variable override gated by cfg(debug_assertions): present in cargo test, absent from both installers (dev and release use [profile.release]); inject it (a config value or a test root the caller passes), never read it from the environment" });
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
