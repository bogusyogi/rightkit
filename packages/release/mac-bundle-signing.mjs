// Directory entries (.app bundles) in `sign.prePackageFiles`: sign nested code inside-out first
// (no entitlements unless declared per path), then the bundle itself, reusing the swift-mac-app inside-out discovery.
import { existsSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { discoverNestedCode } from "./swift-mac-app.mjs";

const fail = (message) => Object.assign(new Error(message), { code: "MAC_SIGN_ENTRY_INVALID" });

export function mainExecutableName(app) {
  try {
    const plist = readFileSync(path.join(app, "Contents", "Info.plist"), "utf8");
    const match = /<key>CFBundleExecutable<\/key>\s*<string>([^<]+)<\/string>/.exec(plist);
    if (match) return match[1];
  } catch { /* binary plist or unreadable: fall back to the bundle name */ }
  return path.basename(app, ".app");
}

/**
 * Split signing entries into nested code (deepest first, signed without entitlements) and the entries
 * themselves. A directory must be a `.app` bundle; anything else is rejected with a clear error.
 * Injectable: isDirectory, discover, exists.
 */
export function expandMacSigningEntries(files, { isDirectory = (f) => statSync(f).isDirectory(), exists = existsSync, discover = discoverNestedCode, mainName = mainExecutableName } = {}) {
  const nested = [];
  for (const file of files) {
    if (!isDirectory(file)) continue;
    if (!file.endsWith(".app") || !exists(path.join(file, "Contents", "Info.plist"))) {
      throw fail(`sign.prePackageFiles entry is a directory but not a .app bundle with Contents/Info.plist: ${file}`);
    }
    nested.push(...discover(file, mainName(file)));
  }
  return { nested: [...new Set(nested)], files };
}

// ---------------------------------------------------------------------------------------------------------------------
// Declarative per-binary signing settings, shared by the dev installer (development-signing.mjs) and release nested code.
//
//   identifiers:        { "<bundle-relative path or glob>": "<--identifier value>" }
//   identifierTemplate: "<bundleId>.runtime.<relpath>"  (default identifier for the remaining plain Mach-O files)
//   entitlements:       { "<bundle-relative path or glob>": "<entitlements plist path>" }
//
// Paths are relative to the .app bundle with `/` separators (`Contents/MacOS/engine`); "." names the bundle itself.
// An exact key beats a glob; among globs the first declared wins. `*` and `?` stay inside one path segment, `**` crosses
// segments. Nothing declared means nothing changes: no `--identifier`, no entitlements (release nested code stays unentitled).
// ---------------------------------------------------------------------------------------------------------------------

/** A codesign `--identifier` value. Must not start with `-` (argv safety). */
export const SIGNING_IDENTIFIER_PATTERN = /^[A-Za-z0-9][A-Za-z0-9.-]{0,254}$/; // same charset as release validation and the signing agent
const TEMPLATE_TOKENS = new Set(["bundleId", "relpath", "basename"]);
const BUNDLE_DIRECTORY = /\.(app|framework|xpc|appex|bundle)$/;
const sanitizePart = (value) => String(value).replace(/[^A-Za-z0-9.-]+/g, ".");

export const isGlobPattern = (pattern) => /[*?]/.test(pattern);

export function globToRegExp(glob) {
  let out = "";
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i];
    if (c === "*") {
      if (glob[i + 1] === "*") {
        if (glob[i + 2] === "/") { out += "(?:.*/)?"; i += 2; } else { out += ".*"; i += 1; }
      } else out += "[^/]*";
    } else if (c === "?") out += "[^/]";
    else out += c.replace(/[.+^${}()|[\]\\]/g, "\\$&");
  }
  return new RegExp(`^${out}$`);
}

/** Bundle-relative POSIX path of `file` inside `app`; "" for the bundle itself. */
export const bundleRelativePath = (app, file) => path.relative(app, file).split(path.sep).join("/");

export function validateBundleRelativeKey(key, label, reject) {
  if (typeof key !== "string" || !key.trim() || key !== key.trim()) reject(`${label}: keys must be non-empty bundle-relative paths`);
  if (key !== "." && (key.startsWith("/") || /^[A-Za-z]:/.test(key) || key.includes("\\") || key.split("/").includes(".."))) {
    reject(`${label}: ${JSON.stringify(key)} must be a path inside the .app bundle (relative, no "..", "/" separators)`);
  }
}

export function validateSigningIdentifier(value, label, reject) {
  if (typeof value !== "string" || !SIGNING_IDENTIFIER_PATTERN.test(value)) reject(`${label} must be a codesign identifier (letters, digits, "." "_" "-"; not starting with "-"), got ${JSON.stringify(value)}`);
  return value;
}

/** `<relpath>` and `<basename>` are the bundle-relative path / file name with runs outside [A-Za-z0-9.-] replaced by ".". */
export function expandIdentifierTemplate(template, { bundleId, relpath }) {
  return template.replace(/<(bundleId|relpath|basename)>/g, (_, token) => {
    if (token === "bundleId") {
      if (!bundleId) throw fail("identifier template uses <bundleId> but the bundle identifier is unknown");
      return bundleId;
    }
    return sanitizePart(token === "relpath" ? relpath : path.posix.basename(relpath));
  });
}

export function validateIdentifierTemplate(template, label, reject) {
  if (typeof template !== "string" || !template) reject(`${label} must be a non-empty string such as "<bundleId>.runtime.<relpath>"`);
  const tokens = [...template.matchAll(/<([^<>]*)>/g)].map((match) => match[1]);
  if (tokens.some((token) => !TEMPLATE_TOKENS.has(token))) reject(`${label} may only use <bundleId>, <relpath> and <basename>`);
  if (!tokens.includes("relpath") && !tokens.includes("basename")) reject(`${label} must contain <relpath> or <basename> so each binary gets its own identifier`);
  const rest = template.replace(/<[^<>]*>/g, "");
  if (rest.includes("<") || rest.includes(">")) reject(`${label} has an unbalanced "<" or ">"`);
  validateSigningIdentifier(expandIdentifierTemplate(template, { bundleId: "a.b", relpath: "Contents/x" }), `${label} (expanded)`, reject);
  return template;
}

/** Compile `{ pattern: value }` into lookup entries. `value(raw, label)` validates and transforms each value. */
export function compileBundlePathMap(map, label, { reject, value = (raw) => raw }) {
  if (map === undefined) return [];
  if (!map || typeof map !== "object" || Array.isArray(map)) reject(`${label} must be an object mapping bundle-relative paths or globs to values`);
  return Object.entries(map).map(([pattern, raw]) => {
    validateBundleRelativeKey(pattern, label, reject);
    return { pattern, regex: isGlobPattern(pattern) ? globToRegExp(pattern) : null, value: value(raw, `${label}[${JSON.stringify(pattern)}]`) };
  });
}

/** Exact key first, then the first matching glob. undefined when nothing matches. */
export function lookupBundlePath(entries, relative) {
  const exact = entries.find((entry) => !entry.regex && entry.pattern === relative);
  if (exact) return exact.value;
  return entries.find((entry) => entry.regex?.test(relative))?.value;
}

/**
 * The `--identifier` for one path, or undefined (codesign then derives it as before). The map applies to anything,
 * including "." for the bundle; the template applies only to plain Mach-O files (`plain`), never to bundles.
 */
export function resolveBundleIdentifier({ relative, identifiers = [], template, bundleId, plain }) {
  const explicit = lookupBundlePath(identifiers, relative || ".");
  if (explicit !== undefined) return explicit;
  if (!template || !plain || !relative) return undefined;
  const expanded = expandIdentifierTemplate(template, { bundleId, relpath: relative });
  if (!SIGNING_IDENTIFIER_PATTERN.test(expanded)) throw fail(`identifier template produced an invalid identifier for ${relative}: ${JSON.stringify(expanded)}`);
  return expanded;
}

/** CFBundleIdentifier of a bundle (XML plist only; undefined when unreadable). */
export function bundleIdentifierOf(app) {
  try {
    const plist = readFileSync(path.join(app, "Contents", "Info.plist"), "utf8");
    return /<key>CFBundleIdentifier<\/key>\s*<string>([^<]+)<\/string>/.exec(plist)?.[1];
  } catch { return undefined; }
}

/**
 * Release nested-code settings from `sign.identifiers`, `sign.identifierTemplate` and a per-path `sign.entitlements` object.
 * Returns Map<absolute nested file, { identifier?, entitlements?: absolute }> holding only files with something declared;
 * every other file keeps today's behaviour (no entitlements, no --identifier). A string `sign.entitlements` is the
 * app-level file and is ignored here. Paths in the map are resolved against `root`; a missing file is an error.
 * The caller (release signing) adds the flags with `nestedSigningFlags`.
 */
export function resolveNestedSigningSettings(nested, { app, sign = {}, bundleId = bundleIdentifierOf(app), root, exists = existsSync }) {
  const reject = (message) => { throw fail(message); };
  const identifiers = compileBundlePathMap(sign.identifiers, "sign.identifiers", { reject, value: (raw, label) => validateSigningIdentifier(raw, label, reject) });
  const template = sign.identifierTemplate === undefined ? undefined : validateIdentifierTemplate(sign.identifierTemplate, "sign.identifierTemplate", reject);
  const perPath = typeof sign.entitlements === "object" && sign.entitlements !== null && !Array.isArray(sign.entitlements) ? sign.entitlements : undefined;
  const entitlements = compileBundlePathMap(perPath, "sign.entitlements", { reject, value: (raw, label) => {
    if (typeof raw !== "string" || !raw.trim()) reject(`${label} must be an entitlements plist path`);
    const file = path.resolve(root, raw);
    if (!exists(file)) reject(`${label} entitlements file not found: ${file}`);
    return file;
  } });
  const settings = new Map();
  if (!identifiers.length && !template && !entitlements.length) return settings;
  for (const file of nested) {
    const relative = bundleRelativePath(app, file);
    const identifier = resolveBundleIdentifier({ relative, identifiers, template, bundleId, plain: !BUNDLE_DIRECTORY.test(file) });
    const entitlementsFile = lookupBundlePath(entitlements, relative);
    if (identifier !== undefined || entitlementsFile !== undefined) {
      settings.set(file, { ...(identifier !== undefined ? { identifier } : {}), ...(entitlementsFile !== undefined ? { entitlements: entitlementsFile } : {}) });
    }
  }
  return settings;
}

export const nestedSigningFlags = (settings) => [...(settings?.identifier ? ["--identifier", settings.identifier] : []), ...(settings?.entitlements ? ["--entitlements", settings.entitlements] : [])];
