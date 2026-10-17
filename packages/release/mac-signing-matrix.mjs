// Per-file macOS signing matrix for `sign.prePackageFiles[]` object entries and `sign.nestedCode`.
//
//   sign: {
//     prePackageFiles: ["dist/tool", { file: "dist/App.app", entitlements: "release/app.entitlements.plist" },
//                       { file: "dist/legion-hook", identifier: "com.example.legion-hook" }],
//     nestedCode: { "Contents/Helpers/Helper": { options: ["library"], identifier: "com.example.helper" },
//                   "Contents/PlugIns/Ext.appex": { entitlements: "release/ext.entitlements.plist" } },
//   }
//
// This module owns the config shape (validation, fail-early at `right-release doctor`) and the
// inside-out plan. It never runs codesign: sign-macos.mjs (via mac-sign-core.mjs) executes the plan.
import { existsSync, lstatSync, openSync, closeSync, readSync, realpathSync, statSync } from "node:fs";
import path from "node:path";
import { expandMacSigningEntries, mainExecutableName, resolveNestedSigningSettings } from "./mac-bundle-signing.mjs";

const invalid = (message) => Object.assign(new Error(message), { code: "MAC_SIGN_CONFIG_INVALID" });

/** codesign `--options` names accepted in `options[]` (man codesign, OPTION FLAGS). `runtime` is always on. */
export const CODESIGN_OPTION_FLAGS = Object.freeze(["expires", "hard", "host", "kill", "library", "runtime"]);
const ENTRY_KEYS = new Set(["file", "entitlements", "hardenedRuntime", "options", "identifier"]);
const SETTING_KEYS = new Set(["entitlements", "hardenedRuntime", "options", "identifier"]);
// A stable per-file code identity: codesign `--identifier`. A loose tool otherwise gets an identifier derived from its file
// name or cdhash, and macOS keys privacy grants (TCC) to the code identity, so it must not change between builds.
// Same pattern as mac-sign-core.mjs (not imported: that module imports this one).
const IDENTIFIER_PATTERN = /^[A-Za-z0-9][A-Za-z0-9.-]{0,254}$/;

const isPlainObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

/** Plist sniff: XML plists contain `<plist`, binary plists start with `bplist00`. Catches wrong or empty files. */
function looksLikePlist(file) {
  const fd = openSync(file, "r");
  try {
    const buffer = Buffer.alloc(4096);
    const length = readSync(fd, buffer, 0, buffer.length, 0);
    const head = buffer.subarray(0, length);
    return head.subarray(0, 8).toString("latin1") === "bplist00" || head.toString("utf8").includes("<plist");
  } finally { closeSync(fd); }
}

function validateSettings(settings, label, { root, errors, isFile }) {
  const out = { entitlements: undefined, hardenedRuntime: true, options: [], identifier: undefined };
  if (settings.identifier !== undefined) {
    if (typeof settings.identifier !== "string" || !IDENTIFIER_PATTERN.test(settings.identifier)) errors.push(`${label}.identifier must be 1-255 characters of letters, digits, '.' and '-', starting with a letter or digit`);
    else out.identifier = settings.identifier;
  }
  if (settings.entitlements !== undefined) {
    const value = settings.entitlements;
    if (typeof value !== "string" || !value.trim()) errors.push(`${label}.entitlements must be a non-empty path string`);
    else if (path.isAbsolute(value) || /^[A-Za-z]:/.test(value)) errors.push(`${label}.entitlements must be relative to the release config directory: ${value}`);
    else {
      const resolved = path.resolve(root, value);
      const relative = path.relative(root, resolved);
      if (relative.startsWith("..") || path.isAbsolute(relative)) errors.push(`${label}.entitlements must stay inside the release config directory: ${value}`);
      else if (!isFile(resolved)) errors.push(`${label}.entitlements file not found: ${value}`);
      else if (!looksLikePlist(resolved)) errors.push(`${label}.entitlements is not a property list (expected an XML or binary plist): ${value}`);
      else out.entitlements = resolved;
    }
  }
  if (settings.hardenedRuntime !== undefined) {
    if (typeof settings.hardenedRuntime !== "boolean") errors.push(`${label}.hardenedRuntime must be a boolean`);
    else if (settings.hardenedRuntime === false) errors.push(`${label}.hardenedRuntime: false is not allowed: macOS Developer ID signing is notarized and notarization rejects code without the hardened runtime (omit the field; it defaults to true)`);
  }
  if (settings.options !== undefined) {
    if (!Array.isArray(settings.options) || settings.options.some((item) => typeof item !== "string")) errors.push(`${label}.options must be an array of codesign --options names`);
    else {
      const seen = new Set();
      for (const item of settings.options) {
        if (!CODESIGN_OPTION_FLAGS.includes(item)) errors.push(`${label}.options: unsupported codesign option ${JSON.stringify(item)} (allowed: ${CODESIGN_OPTION_FLAGS.join(", ")})`);
        else if (seen.has(item)) errors.push(`${label}.options: duplicate option ${JSON.stringify(item)}`);
        seen.add(item);
      }
      out.options = settings.options.filter((item) => item !== "runtime");
    }
  }
  return out;
}

function validateNestedKey(key) {
  if (!key.trim() || key !== key.trim()) return "must be a non-empty path without surrounding whitespace";
  if (key.includes("\0") || key.includes("\\")) return "must use forward slashes and no NUL";
  if (path.posix.isAbsolute(key) || /^[A-Za-z]:/.test(key)) return "must be relative to the .app bundle";
  const segments = key.split("/");
  if (segments.some((segment) => segment === "" || segment === "." || segment === "..")) return "must not contain empty, '.' or '..' segments or a trailing slash";
  if (segments[0] !== "Contents" || segments.length < 2) return "must name code inside the bundle, starting with Contents/";
  return null;
}

// Per-path nested signing maps: `sign.identifiers`, `sign.identifierTemplate` and an object-valued `sign.entitlements`.
// Same strictness as the per-item settings above: reverse-DNS identifiers, contained relative plist paths that exist.
function validateNestedSigningMaps(sign, { root, errors, isFile }) {
  const declared = {};
  const check = (map, label, value) => {
    if (!isPlainObject(map)) { errors.push(`${label} must be an object mapping bundle-relative paths or globs to values`); return; }
    for (const [key, raw] of Object.entries(map)) {
      const entry = `${label}[${JSON.stringify(key)}]`;
      if (!key.trim() || key !== key.trim() || key.includes("\0") || key.includes("\\") || path.posix.isAbsolute(key) || /^[A-Za-z]:/.test(key) || key.split("/").includes("..")) {
        errors.push(`${entry}: key must be a bundle-relative path (no "..", no absolute path, "/" separators)`);
      }
      value(raw, entry);
    }
  };
  if (sign.identifiers !== undefined) {
    check(sign.identifiers, "sign.identifiers", (raw, entry) => {
      if (typeof raw !== "string" || !IDENTIFIER_PATTERN.test(raw)) errors.push(`${entry} must be 1-255 characters of letters, digits, '.' and '-', starting with a letter or digit`);
    });
    declared.identifiers = sign.identifiers;
  }
  if (sign.identifierTemplate !== undefined) {
    const template = sign.identifierTemplate;
    if (typeof template !== "string" || !template) errors.push("sign.identifierTemplate must be a non-empty string such as \"<bundleId>.runtime.<relpath>\"");
    else if (!/^[A-Za-z0-9.-]*$/.test(template.replace(/<[^<>]*>/g, ""))) errors.push("sign.identifierTemplate literal text may only contain letters, digits, '.' and '-' around <bundleId>, <relpath> and <basename>");
    else declared.identifierTemplate = template;
  }
  if (isPlainObject(sign.entitlements)) {
    check(sign.entitlements, "sign.entitlements", (raw, entry) => {
      if (typeof raw !== "string" || !raw.trim()) { errors.push(`${entry} must be a non-empty path string`); return; }
      if (path.isAbsolute(raw) || /^[A-Za-z]:/.test(raw) || raw.includes("\0")) { errors.push(`${entry} must be relative to the release config directory: ${raw}`); return; }
      const resolved = path.resolve(root, raw);
      const relative = path.relative(root, resolved);
      if (relative.startsWith("..") || path.isAbsolute(relative)) errors.push(`${entry} must stay inside the release config directory: ${raw}`);
      else if (!isFile(resolved)) errors.push(`${entry} entitlements file not found: ${raw}`);
      else if (!looksLikePlist(resolved)) errors.push(`${entry} is not a property list (expected an XML or binary plist): ${raw}`);
    });
    declared.entitlements = sign.entitlements;
  }
  if (errors.length || !Object.keys(declared).length) return undefined;
  // Delegate key globs and template tokens to the shared resolver so release and development read them identically.
  try { resolveNestedSigningSettings([], { app: "x.app", sign: declared, bundleId: "a.b", root, exists: () => true }); }
  catch (error) { errors.push(error.message); return undefined; }
  return declared;
}

/**
 * Validate and normalize `sign` for a macOS target. Collects every problem, then throws one
 * MAC_SIGN_CONFIG_INVALID error. Returns { entries, nestedCode, nestedSigning? } where
 *   entries:       [{ file, identifier?, entitlements?: absolute, options: [] }]   (file stays as declared)
 *   nestedCode:    [{ key, identifier?, entitlements?: absolute, options: [] }]
 *   nestedSigning: { identifiers?, identifierTemplate?, entitlements? } as declared; present only when one is declared
 * Injectable: isFile.
 */
export function validateMacSigningConfig(sign, { root, isFile = (file) => existsSync(file) && statSync(file).isFile() } = {}) {
  const errors = [];
  const entries = [];
  const nestedCode = [];
  if (!isPlainObject(sign)) throw invalid("sign must be an object");
  const context = { root: path.resolve(root), errors, isFile };
  if (!Array.isArray(sign.prePackageFiles) || !sign.prePackageFiles.length) errors.push("sign.prePackageFiles must be a non-empty array");
  else {
    const seen = new Set();
    sign.prePackageFiles.forEach((entry, index) => {
      const label = `sign.prePackageFiles[${index}]`;
      let file;
      let settings = {};
      if (typeof entry === "string") file = entry;
      else if (isPlainObject(entry)) {
        for (const key of Object.keys(entry)) if (!ENTRY_KEYS.has(key)) errors.push(`${label}: unknown key ${JSON.stringify(key)} (allowed: ${[...ENTRY_KEYS].join(", ")})`);
        file = entry.file;
        settings = entry;
      } else { errors.push(`${label} must be a path string or { file, entitlements?, hardenedRuntime?, options? }`); return; }
      if (typeof file !== "string" || !file.trim()) { errors.push(`${label}.file must be a non-empty path string`); return; }
      if (seen.has(file)) errors.push(`${label}: duplicate signing entry ${file}`);
      seen.add(file);
      const normalized = validateSettings(settings, label, context);
      entries.push({ file, ...(normalized.identifier ? { identifier: normalized.identifier } : {}), ...(normalized.entitlements ? { entitlements: normalized.entitlements } : {}), options: normalized.options });
    });
  }
  if (sign.nestedCode !== undefined) {
    if (!isPlainObject(sign.nestedCode)) errors.push("sign.nestedCode must be an object keyed by path inside the .app bundle");
    else {
      for (const [key, settings] of Object.entries(sign.nestedCode)) {
        const label = `sign.nestedCode[${JSON.stringify(key)}]`;
        const keyProblem = validateNestedKey(key);
        if (keyProblem) errors.push(`${label}: key ${keyProblem}`);
        if (!isPlainObject(settings)) { errors.push(`${label} must be an object { entitlements?, hardenedRuntime?, options? }`); continue; }
        for (const name of Object.keys(settings)) if (!SETTING_KEYS.has(name)) errors.push(`${label}: unknown key ${JSON.stringify(name)} (allowed: ${[...SETTING_KEYS].join(", ")})`);
        const normalized = validateSettings(settings, label, context);
        nestedCode.push({ key, ...(normalized.identifier ? { identifier: normalized.identifier } : {}), ...(normalized.entitlements ? { entitlements: normalized.entitlements } : {}), options: normalized.options });
      }
      if (nestedCode.length) {
        const apps = entries.filter((entry) => /\.app\/?$/.test(entry.file));
        if (apps.length !== 1) errors.push(`sign.nestedCode needs exactly one .app entry in sign.prePackageFiles to resolve its paths against (found ${apps.length})`);
      }
    }
  }
  const nestedSigning = validateNestedSigningMaps(sign, context);
  if (nestedSigning && !entries.some((entry) => /\.app\/?$/.test(entry.file))) errors.push("sign.identifiers, sign.identifierTemplate and per-path sign.entitlements need a .app entry in sign.prePackageFiles to resolve their paths against");
  if (errors.length) throw invalid(`invalid macOS signing configuration:\n${errors.map((line) => `- ${line}`).join("\n")}`);
  return { entries, nestedCode, ...(nestedSigning ? { nestedSigning } : {}) };
}

/** True when an entry or nested item carries anything beyond the default (hardened runtime, no entitlements). */
export const hasNonDefaultSettings = (item) => Boolean(item.entitlements) || Boolean(item.identifier) || (item.options?.length ?? 0) > 0;

const depth = (file) => file.split(path.sep).length;

/**
 * Inside-out signing plan for a macOS target.
 *   entries:    [{ file: absolute, entitlements?, options }]  top-level signing entries, in declared order
 *   nestedCode: validated `sign.nestedCode` items (paths relative to the single .app entry)
 * Returns { nested, top }: `nested` is every nested code item of each .app entry, deepest first,
 * defaults unless named by nestedCode; `top` is the entries themselves. Fails (before any signing) when
 * a nestedCode path is missing, is a symlink, escapes the bundle, or names the bundle's main executable.
 * Injectable: expand, lstat, realpath, mainName.
 */
export function planMacSigning({ entries, nestedCode = [], nestedSigning, root, resolveSettings = resolveNestedSigningSettings, expand = expandMacSigningEntries, lstat = lstatSync, realpath = realpathSync, mainName = mainExecutableName }) {
  const { nested: discovered } = expand(entries.map((entry) => entry.file));
  const byPath = new Map(discovered.map((file) => [file, { file, options: [] }]));
  if (nestedCode.length) {
    const app = entries.find((entry) => /\.app\/?$/.test(entry.file));
    if (!app) throw invalid("sign.nestedCode requires an .app entry in sign.prePackageFiles");
    const appReal = realpath(app.file);
    const mainReal = path.join(appReal, "Contents", "MacOS", mainName(app.file));
    for (const item of nestedCode) {
      const label = `sign.nestedCode[${JSON.stringify(item.key)}]`;
      const file = path.join(app.file, ...item.key.split("/"));
      let info;
      try { info = lstat(file); } catch { throw invalid(`${label}: not found inside ${app.file}`); }
      if (info.isSymbolicLink()) throw invalid(`${label}: symlinks cannot be signed; name the real file`);
      const real = realpath(file);
      if (real !== appReal && !real.startsWith(appReal + path.sep)) throw invalid(`${label}: resolves outside the bundle`);
      if (real === mainReal) throw invalid(`${label}: the main executable is signed with the bundle; set entitlements on its sign.prePackageFiles entry`);
      byPath.set(file, { file, ...(item.identifier ? { identifier: item.identifier } : {}), ...(item.entitlements ? { entitlements: item.entitlements } : {}), options: item.options });
    }
  }
  // Per-path maps (sign.identifiers / identifierTemplate / per-path sign.entitlements) fill in only what a nestedCode item left
  // unset. Nothing declared: this block is skipped and the items stay exactly as discovered.
  if (nestedSigning && Object.keys(nestedSigning).length) {
    const apps = entries.map((entry) => entry.file).filter((file) => /\.app\/?$/.test(file)).map((file) => file.replace(/\/+$/, ""));
    for (const app of apps) {
      const own = [...byPath.values()].filter((item) => item.file.startsWith(app + path.sep) || item.file.startsWith(`${app}/`));
      let settings;
      try { settings = resolveSettings(own.map((item) => item.file), { app, sign: nestedSigning, root: root ?? process.cwd() }); }
      catch (error) { throw invalid(error.message); }
      for (const item of own) {
        const found = settings.get(item.file);
        if (!found) continue;
        const identifier = item.identifier ?? found.identifier;
        if (identifier !== undefined && !IDENTIFIER_PATTERN.test(identifier)) throw invalid(`signing identifier for ${path.relative(app, item.file)} must be letters, digits, '.' and '-': ${JSON.stringify(identifier)}`);
        const entitlements = item.entitlements ?? found.entitlements;
        byPath.set(item.file, { ...item, ...(identifier !== undefined ? { identifier } : {}), ...(entitlements ? { entitlements } : {}) });
      }
    }
  }
  const nested = [...byPath.values()].sort((a, b) => depth(b.file) - depth(a.file) || a.file.localeCompare(b.file));
  return { nested, top: entries };
}

/** Receipt-side check: every entry's recorded entitlements digest and options match the plan. */
export function matrixReceiptProblems(evidenceFiles, entries, hashFile) {
  const problems = [];
  const byFile = new Map((evidenceFiles ?? []).map((item) => [path.resolve(item.file), item]));
  for (const entry of entries) {
    const item = byFile.get(path.resolve(entry.file));
    if (!item) { problems.push(`missing receipt entry for ${entry.file}`); continue; }
    const wantOptions = ["runtime", ...(entry.options ?? [])].join(",");
    if ((item.codesignOptions ?? "runtime") !== wantOptions) problems.push(`${entry.file}: signed with options ${item.codesignOptions ?? "runtime"}, expected ${wantOptions}`);
    if (entry.identifier) {
      if (item.identifier !== entry.identifier) problems.push(`${entry.file}: signed with identifier ${item.identifier ?? "(none recorded)"}, expected ${entry.identifier}`);
    } else if (item.identifier) problems.push(`${entry.file}: signed with identifier ${item.identifier} the plan did not declare`);
    if (entry.entitlements) {
      if (item.entitlements?.sha256 !== hashFile(entry.entitlements)) problems.push(`${entry.file}: receipt entitlements digest does not match ${entry.entitlements}`);
    } else if (item.entitlements) problems.push(`${entry.file}: signed with entitlements the plan did not declare`);
  }
  return problems;
}
