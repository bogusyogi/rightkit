// Declarative macOS packaging recipe (bogusyogi/rightkit#345). A mac target that sets `macPackage` and has no
// `prePackage` / `package` hook scripts is packaged by this module instead of a per-app mac-dmg.mjs:
//
//   1. universal check   every Mach-O (main executable + nested) must hold x86_64 and arm64 (macPackage.universal)
//   2. sign nested code  Developer ID, hardened runtime, no entitlements, deepest first
//   3. sign the app      Developer ID, hardened runtime, optional entitlements
//   4. build the DMG     branded, from the signed app (dmg-build)
//   5. sign the DMG      Developer ID with a secure timestamp (no hardened runtime, as for a disk image)
//   6. notarize + staple the DMG and the app (macos-notarization), when macPackage.notarize
//   7. updater tarball   tar.gz of the stapled app's Contents + the updater signature, when macPackage.updater
//
// Every external effect is injectable through `deps`; nothing here reads the host unless a default is used.
// Fails closed: every problem throws MacPackageRecipeError, and a failure after the DMG exists removes the
// half-processed DMG / updater files so a later seal cannot pick up an artifact that skipped a step.
import * as nodeFs from "node:fs";
import path from "node:path";
import { buildDmg } from "./dmg-build.mjs";
import { spawnSync } from "./background-process.mjs";
import { signMacosEntries } from "./mac-sign-core.mjs";
import { mainExecutableName } from "./mac-bundle-signing.mjs";
import { resolveArtifactPath, splitArtifactContext } from "./release-artifact-path.mjs";
import { notarizeMacosArtifact } from "./macos-notarization.mjs";
import { resolveMacosDeveloperIdIdentity } from "./macos-signing-identity.mjs";
import { runMacCodesignCommands, signMacUpdaterFiles } from "./macos-signing-session.mjs";
import { discoverNestedCode } from "./swift-mac-app.mjs";
import { assertUpdaterMatchesShippedKey } from "./updater-shipped-key.mjs";

/**
 * Typed error. Codes: MAC_PACKAGE_CONFIG_INVALID, MAC_PACKAGE_HOST_UNSUPPORTED, MAC_PACKAGE_INPUT_MISSING,
 * MAC_PACKAGE_UNIVERSAL_FAILED, MAC_PACKAGE_SIGN_FAILED, MAC_PACKAGE_DMG_FAILED, MAC_PACKAGE_NOTARIZE_FAILED,
 * MAC_PACKAGE_UPDATER_FAILED.
 */
export class MacPackageRecipeError extends Error {
  constructor(code, message, options) { super(message, options); this.name = "MacPackageRecipeError"; this.code = code; }
}

export const MAC_PACKAGE_STEPS = Object.freeze(["universal", "sign-nested", "sign-app", "dmg", "sign-dmg", "notarize", "updater"]);
const UNIVERSAL_ARCHS = ["x86_64", "arm64"];
const MACPACKAGE_KEYS = ["universal", "dmg", "notarize", "updater", "nested", "entitlements", "requiredEntitlements"];
const DMG_KEYS = ["name", "background", "windowSize", "iconPositions"];
const ICON_KEYS = ["app", "applications"];

const invalid = (message) => new MacPackageRecipeError("MAC_PACKAGE_CONFIG_INVALID", `macPackage: ${message}`);
const isPlainObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const rejectUnknown = (object, allowed, label) => {
  const unknown = Object.keys(object).filter((key) => !allowed.includes(key));
  if (unknown.length) throw invalid(`${label} has unknown key(s): ${unknown.join(", ")} (allowed: ${allowed.join(", ")})`);
};
const integerPair = (value, label, min) => {
  if (!Array.isArray(value) || value.length !== 2 || value.some((n) => !Number.isInteger(n) || n < min || n > 32767)) {
    throw invalid(`${label} must be [x, y] integers ${min}..32767`);
  }
  return [...value];
};
const requireBoolean = (value, label) => {
  if (typeof value !== "boolean") throw invalid(`${label} is required and must be true or false`);
  return value;
};

/** A bundle-relative glob: forward slashes only, no `..`, no absolute path, only `*`, `**` and `?` wildcards. */
function validateGlob(pattern, label) {
  if (typeof pattern !== "string" || !pattern.trim()) throw invalid(`${label} must be a non-empty string`);
  if (pattern.startsWith("/") || /^[A-Za-z]:/.test(pattern) || pattern.includes("\\")) throw invalid(`${label} must be a bundle-relative path with forward slashes: ${pattern}`);
  if (/[[\]{}()!]/.test(pattern)) throw invalid(`${label} supports only *, ** and ? wildcards: ${pattern}`);
  const segments = pattern.split("/");
  if (segments.some((segment) => segment === "" || segment === "." || segment === "..")) throw invalid(`${label} must not contain empty, '.' or '..' segments: ${pattern}`);
  if (segments.some((segment) => segment.includes("**") && segment !== "**")) throw invalid(`${label}: '**' must be a whole path segment: ${pattern}`);
  return pattern;
}

/** Strictly validate and normalize a `macPackage` value. Unknown keys, wrong types and missing fields throw. */
export function validateMacPackageConfig(macPackage) {
  if (!isPlainObject(macPackage)) throw invalid("must be an object");
  rejectUnknown(macPackage, MACPACKAGE_KEYS, "macPackage");
  const universal = macPackage.universal === undefined ? false : requireBoolean(macPackage.universal, "universal");
  const notarize = requireBoolean(macPackage.notarize, "notarize");
  const updater = requireBoolean(macPackage.updater, "updater");
  const dmg = macPackage.dmg;
  if (!isPlainObject(dmg)) throw invalid("dmg is required and must be an object");
  rejectUnknown(dmg, DMG_KEYS, "dmg");
  if (typeof dmg.name !== "string" || !dmg.name.trim() || /[\0\r\n/:]/.test(dmg.name) || Buffer.byteLength(dmg.name) > 255) {
    throw invalid("dmg.name must be a non-empty volume name without NUL, CR, LF, '/' or ':'");
  }
  const normalized = { name: dmg.name };
  if (dmg.background !== undefined) {
    if (typeof dmg.background !== "string" || !/\.(png|tiff?)$/i.test(dmg.background)) throw invalid("dmg.background must be a path to a PNG or TIFF file");
    normalized.background = dmg.background;
  }
  if (dmg.windowSize !== undefined) normalized.windowSize = integerPair(dmg.windowSize, "dmg.windowSize", 1);
  if (dmg.iconPositions !== undefined) {
    if (!isPlainObject(dmg.iconPositions)) throw invalid("dmg.iconPositions must be an object with app and/or applications");
    rejectUnknown(dmg.iconPositions, ICON_KEYS, "dmg.iconPositions");
    normalized.iconPositions = Object.fromEntries(Object.entries(dmg.iconPositions).map(([key, value]) => [key, integerPair(value, `dmg.iconPositions.${key}`, 0)]));
  }
  let nested = [];
  if (macPackage.nested !== undefined) {
    if (!Array.isArray(macPackage.nested) || !macPackage.nested.length) throw invalid("nested must be a non-empty array of bundle-relative globs");
    nested = macPackage.nested.map((pattern, index) => validateGlob(pattern, `nested[${index}]`));
    if (new Set(nested).size !== nested.length) throw invalid("nested globs must be unique");
  }
  if (macPackage.entitlements !== undefined && (typeof macPackage.entitlements !== "string" || !macPackage.entitlements.trim())) throw invalid("entitlements must be a path string");
  // rightkit#1443 (hrmac: R2 checked the production APNs entitlement in the signed app): `requiredEntitlements` maps a bundle-relative
  // executable to entitlement keys and the exact value each must carry after signing (a string, or true for a boolean entitlement).
  let requiredEntitlements = null;
  if (macPackage.requiredEntitlements !== undefined) {
    const re = macPackage.requiredEntitlements;
    if (!re || typeof re !== "object" || Array.isArray(re) || !Object.keys(re).length) throw invalid("requiredEntitlements must map bundle-relative executables to {entitlement: value}");
    for (const [file, keys] of Object.entries(re)) {
      validateGlob(file, `requiredEntitlements key ${file}`);
      if (!keys || typeof keys !== "object" || Array.isArray(keys) || !Object.keys(keys).length) throw invalid(`requiredEntitlements.${file} must be a non-empty {entitlement: value} object`);
      for (const [k, v] of Object.entries(keys)) if (!(typeof v === "string" || v === true)) throw invalid(`requiredEntitlements.${file}.${k} must be a string or true`);
    }
    requiredEntitlements = re;
  }
  return { universal, dmg: normalized, notarize, updater, nested, ...(macPackage.entitlements ? { entitlements: macPackage.entitlements } : {}), ...(requiredEntitlements ? { requiredEntitlements } : {}) };
}

/**
 * Validate the mac target around `macPackage`: the declarative recipe and the hook scripts are mutually exclusive, and the
 * app / DMG paths come from the existing `notarize.app` and `notarize.file` fields so there is one source of truth.
 */
export function validateMacPackageTarget(target) {
  if (!isPlainObject(target)) throw invalid("the mac target must be an object");
  if (target.prePackage !== undefined || target.package !== undefined) throw invalid("a target with macPackage must not also declare prePackage or package; delete the hook scripts");
  if (target.dmg !== undefined) throw invalid("a target with macPackage must not also declare targets.mac.dmg; use macPackage.dmg");
  const recipe = validateMacPackageConfig(target.macPackage);
  const app = target.notarize?.app;
  const file = target.notarize?.file;
  if (typeof app !== "string" || !/\.app\/?$/.test(app)) throw invalid("notarize.app must name the .app bundle the recipe signs and packs");
  if (typeof file !== "string" || !/\.dmg$/i.test(file)) throw invalid("notarize.file must name the .dmg the recipe produces");
  return { recipe, app, dmgFile: file };
}

/** Bundle-relative glob to RegExp; `**` spans path segments, `*` and `?` stay inside one. */
export function globToRegExp(pattern) {
  const segments = pattern.split("/");
  const body = segments.map((segment, index) => {
    const last = index === segments.length - 1;
    if (segment === "**") return last ? ".+" : "(?:[^/]+/)*";
    return `${segment.replace(/[.+^$|\\]/g, "\\$&").replace(/\*/g, "[^/]*").replace(/\?/g, "[^/]")}${last ? "" : "/"}`;
  }).join("");
  return new RegExp(`^${body}$`);
}

function listTree(root, fs) {
  const found = [];
  const walk = (dir, prefix) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
      found.push(relative);
      if (entry.isDirectory() && !entry.isSymbolicLink()) walk(path.join(dir, entry.name), relative);
    }
  };
  walk(root, "");
  return found;
}

const depth = (file) => file.split(/[\\/]/).length;
const tail = (text, lines = 20) => String(text ?? "").split(/\r?\n/).slice(-lines).join("\n");

const defaultInspect = (cmd, args) => {
  const result = spawnSync(cmd, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
  const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  if (result.error || result.status !== 0) throw new Error(`${cmd} ${args.join(" ")} failed: ${output.trim() || result.error?.message}`);
  return output;
};

/**
 * Run the declarative mac package recipe. Returns { steps, app, dmg, updater, receipt, signing } where `steps` lists the
 * steps that actually ran, in order. Throws MacPackageRecipeError on the first failure.
 *
 * deps (all optional): platform, env, fs, resolveArtifact(root, p), resolveIdentity({env}), signOne(codesignArgs),
 * inspect(cmd, args) -> output, evidence(file), hashFile(file), discover(app, mainName), mainName(app),
 * listFiles(dir) -> bundle-relative posix paths, buildDmg(options), notarize(options), run(cmd, args, opts),
 * signUpdater(files, options), receiptPath, log(event).
 */
/** Every required entitlement is present, with its exact value, in the signed executable (`codesign -d --entitlements :-`, an XML plist). */
export function macRequiredEntitlementsCheck({ app, required, run = spawnSync, env = process.env }) {
  const esc = (t) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  for (const [file, keys] of Object.entries(required)) {
    const target = path.join(app, ...file.split("/"));
    const r = run("codesign", ["-d", "--entitlements", ":-", target], { env, encoding: "utf8" });
    if (!r || r.error || r.status !== 0) throw new Error(`codesign -d --entitlements on ${file} failed (${r?.error?.message ?? `exit ${r?.status}`})`);
    const plist = String(r.stdout ?? "");
    for (const [k, v] of Object.entries(keys)) {
      const want = v === true ? "<true\\s*/>" : `<string>${esc(String(v))}</string>`;
      if (!new RegExp(`<key>${esc(k)}</key>\\s*${want}`).test(plist)) throw new Error(`${file} is signed without the required entitlement ${k} = ${JSON.stringify(v)}`);
    }
  }
}

/** Gatekeeper's verdict on the shipped app and DMG, as a user's Mac reaches it: staple validation and spctl assessment. Throws on any refusal. */
export function macGatekeeperCheck({ app, dmg, run = spawnSync, env = process.env }) {
  const checks = [
    ["xcrun", ["stapler", "validate", app]],
    ["xcrun", ["stapler", "validate", dmg]],
    ["spctl", ["--assess", "--type", "execute", "-v", app]],
    ["spctl", ["--assess", "--type", "open", "--context", "context:primary-signature", "-v", dmg]],
  ];
  for (const [cmd, args] of checks) {
    const r = run(cmd, args, { env, encoding: "utf8" });
    if (!r || r.error || r.status !== 0) throw new Error(`${cmd} ${args.join(" ")} refused (${r?.error?.message ?? `exit ${r?.status}`}): ${String(r?.stderr ?? r?.stdout ?? "").trim().split(/\r?\n/).slice(-2).join(" | ")}`);
  }
}

export function runMacPackageRecipe({ appRoot, config = {}, target, deps = {} }) {
  let artifactPaths = null;
  const { recipe, app: declaredApp, dmgFile } = validateMacPackageTarget(target);
  const {
    platform = process.platform, env = process.env, fs = nodeFs,
    // A door split (row 7.04c, R-T2) reads a declared target path in the bound Cargo target, never the snapshot's or the checkout's; outside a split this is path.resolve.
    resolveArtifact = (root, p) => { artifactPaths ??= splitArtifactContext({ cwd: root, env, target }); return artifactPaths.mode === "split" ? resolveArtifactPath(p, artifactPaths) : path.resolve(root, p); },
    resolveIdentity = resolveMacosDeveloperIdIdentity,
    signOne = (codesignArgs) => runMacCodesignCommands([codesignArgs], { env })[0],
    inspect = defaultInspect, evidence, hashFile,
    discover = discoverNestedCode, mainName = mainExecutableName,
    listFiles = (dir) => listTree(dir, fs),
    buildDmg: build = buildDmg, notarize = notarizeMacosArtifact,
    run = spawnSync, signUpdater = (files, options) => signMacUpdaterFiles(files, options),
    log = () => {},
  } = deps;
  const appName = config.app ?? path.basename(appRoot ?? ".");
  if (typeof appRoot !== "string" || !path.isAbsolute(appRoot)) throw invalid("appRoot must be an absolute path");
  if (platform !== "darwin") throw new MacPackageRecipeError("MAC_PACKAGE_HOST_UNSUPPORTED", `macPackage signs and notarizes on macOS only (host: ${platform})`);

  const app = path.resolve(resolveArtifact(appRoot, declaredApp));
  const dmg = path.resolve(resolveArtifact(appRoot, dmgFile));
  const updater = `${app}.tar.gz`;
  const receiptPath = deps.receiptPath ?? path.join(path.resolve(env.RIGHT_RELEASE_STATE_ROOT ?? path.join(appRoot, ".right-release"), "receipts"), `${appName}-mac-notarization.json`);
  const missing = (what, file) => new MacPackageRecipeError("MAC_PACKAGE_INPUT_MISSING", `${what} not found: ${file}`);
  const wrap = (code, step, error) => (error instanceof MacPackageRecipeError ? error
    : new MacPackageRecipeError(code, `macPackage ${step} failed: ${error?.message ?? error}`, { cause: error }));

  // Resolve every input before any effect so a typo cannot leave a half-signed app.
  if (!fs.existsSync(app)) throw missing("app bundle", app);
  const infoPlist = path.join(app, "Contents", "Info.plist");
  if (!fs.existsSync(infoPlist)) throw missing("Contents/Info.plist in the app bundle", infoPlist);
  const entitlements = recipe.entitlements ? path.resolve(appRoot, recipe.entitlements) : undefined;
  if (entitlements && !fs.existsSync(entitlements)) throw missing("entitlements file", entitlements);
  const background = recipe.dmg.background ? path.resolve(appRoot, recipe.dmg.background) : undefined;
  if (background && !fs.existsSync(background)) throw missing("dmg.background file", background);

  const executable = mainName(app);
  const mainBinary = path.join(app, "Contents", "MacOS", executable);
  const nestedSet = new Set(discover(app, executable).map((file) => path.resolve(file)));
  if (recipe.nested.length) {
    const tree = listFiles(app);
    for (const pattern of recipe.nested) {
      const matcher = globToRegExp(pattern);
      const hits = tree.filter((relative) => matcher.test(relative));
      if (!hits.length) throw new MacPackageRecipeError("MAC_PACKAGE_INPUT_MISSING", `macPackage.nested glob matched nothing inside ${path.basename(app)}: ${pattern}`);
      for (const hit of hits) nestedSet.add(path.resolve(app, hit));
    }
  }
  nestedSet.delete(mainBinary);
  nestedSet.delete(app);
  // Inside-out: a path nested deeper is always signed before the code that contains it.
  const nested = [...nestedSet].sort((a, b) => depth(b) - depth(a));

  const steps = [];
  const record = (step) => { steps.push(step); log({ event: "mac-package-step", step, app: appName }); };
  const signOptions = { signOne, inspect, ...(evidence ? { evidence } : {}), ...(hashFile ? { hashFile } : {}) };
  let signing = [];
  let receipt;
  let updaterFiles = [];
  try {
    // 1) universal: verify every Mach-O holds both architectures. A thin slice makes the whole app Intel- or
    //    Apple-Silicon-only, which compiles, signs and notarizes fine and then fails on half the users' Macs.
    if (recipe.universal) {
      const archsOf = (file) => String(inspect("lipo", ["-archs", file])).trim().split(/\s+/);
      const assertFat = (file) => {
        const archs = archsOf(file);
        const lacking = UNIVERSAL_ARCHS.filter((arch) => !archs.includes(arch));
        if (lacking.length) throw new Error(`${path.relative(app, file)} is not universal (has: ${archs.join(" ")}; missing: ${lacking.join(" ")})`);
      };
      try {
        assertFat(mainBinary); // an unreadable main executable is a failure, not a skip
        for (const file of nested) {
          let isFile = false;
          try { isFile = fs.statSync(file).isFile(); } catch { /* absent: signing reports it */ }
          if (!isFile) continue; // code bundles are covered by the Mach-O files inside them
          try { archsOf(file); } catch { continue; } // lipo rejects non-Mach-O files (scripts, resources matched by a glob)
          assertFat(file);
        }
      } catch (error) { throw wrap("MAC_PACKAGE_UNIVERSAL_FAILED", "universal check", error); }
      record("universal");
    }

    // 2-3) sign nested code, then the app. One identity for the whole run.
    let identity;
    try { identity = resolveIdentity({ env }); } catch (error) { throw wrap("MAC_PACKAGE_SIGN_FAILED", "identity lookup", error); }
    if (typeof identity !== "string" || !identity.trim()) throw new MacPackageRecipeError("MAC_PACKAGE_SIGN_FAILED", "macPackage signing identity is empty");
    if (nested.length) {
      try { signing.push(...signMacosEntries(nested.map((file) => ({ file, hardenedRuntime: true, options: [] })), { identity, ...signOptions })); }
      catch (error) { throw wrap("MAC_PACKAGE_SIGN_FAILED", "nested code signing", error); }
      record("sign-nested");
    }
    try { signing.push(...signMacosEntries([{ file: app, hardenedRuntime: true, options: [], ...(entitlements ? { entitlements } : {}) }], { identity, ...signOptions })); }
    catch (error) { throw wrap("MAC_PACKAGE_SIGN_FAILED", "app signing", error); }
    if (recipe.requiredEntitlements) {
      try { (deps.entitlementsCheck ?? macRequiredEntitlementsCheck)({ app, required: recipe.requiredEntitlements, run, env }); }
      catch (error) { throw wrap("MAC_PACKAGE_SIGN_FAILED", "required entitlements", error); }
    }
    record("sign-app");

    // 4) branded DMG from the signed app.
    fs.rmSync(dmg, { force: true });
    try {
      build({
        app, out: dmg, volname: recipe.dmg.name,
        ...(background ? { background } : {}),
        ...(recipe.dmg.windowSize ? { window: recipe.dmg.windowSize } : {}),
        ...(recipe.dmg.iconPositions?.app ? { appPosition: recipe.dmg.iconPositions.app } : {}),
        ...(recipe.dmg.iconPositions?.applications ? { applicationsPosition: recipe.dmg.iconPositions.applications } : {}),
      });
    } catch (error) { throw wrap("MAC_PACKAGE_DMG_FAILED", "DMG build", error); }
    if (!fs.existsSync(dmg)) throw new MacPackageRecipeError("MAC_PACKAGE_DMG_FAILED", `DMG build reported success but ${dmg} does not exist`);
    record("dmg");

    // 5) sign the DMG: secure timestamp, no hardened runtime (a disk image has none).
    try { signing.push(...signMacosEntries([{ file: dmg, hardenedRuntime: false, options: [] }], { identity, ...signOptions })); }
    catch (error) { throw wrap("MAC_PACKAGE_SIGN_FAILED", "DMG signing", error); }
    record("sign-dmg");

    // 6) notarize + staple. The DMG submission covers the app inside it; the app is stapled too so the updater tarball carries the ticket.
    if (recipe.notarize) {
      try { receipt = notarize({ archive: dmg, app, receiptPath, env, run }); }
      catch (error) { throw wrap("MAC_PACKAGE_NOTARIZE_FAILED", "notarization", error); }
      if (!receipt || receipt.notarization?.status !== "Accepted") throw new MacPackageRecipeError("MAC_PACKAGE_NOTARIZE_FAILED", `macPackage notarization did not report Accepted for ${dmg}`);
      // rightkit#1443 (hrmac: R2's verify-macos-release-artifacts.mjs did this; the gate had dropped it): what a user's Mac will check, checked
      // here: the staple of the app and the DMG validates, and Gatekeeper accepts the app (execute) and the DMG (open, primary signature).
      try { (deps.gatekeeper ?? macGatekeeperCheck)({ app, dmg, run, env }); }
      catch (error) { throw wrap("MAC_PACKAGE_NOTARIZE_FAILED", "Gatekeeper and staple validation", error); }
      record("notarize");
    }

    // 7) updater tarball from the final (stapled) app, then its signature.
    if (recipe.updater) {
      updaterFiles = [updater, `${updater}.sig`];
      for (const file of updaterFiles) fs.rmSync(file, { force: true });
      // bsdtar otherwise serializes extended attributes as AppleDouble `._*` entries, which Tauri's extractor writes
      // into the bundle and invalidates the seal. The explicit App.app/ directory entry is omitted for the same extractor.
      let packed;
      try { packed = run("tar", ["-czf", updater, "-C", path.dirname(app), path.join(path.basename(app), "Contents")], { cwd: appRoot, env: { ...env, COPYFILE_DISABLE: "1" }, encoding: "utf8", stdio: "pipe" }); }
      catch (error) { throw wrap("MAC_PACKAGE_UPDATER_FAILED", "updater tar", error); }
      if (!packed || packed.error || packed.status !== 0) throw new MacPackageRecipeError("MAC_PACKAGE_UPDATER_FAILED", `macPackage updater tar failed: ${packed?.error?.message ?? (tail(packed?.stderr) || packed?.status)}`);
      try { signUpdater([updater], { cwd: appRoot, env }); }
      catch (error) { throw wrap("MAC_PACKAGE_UPDATER_FAILED", "updater signing", error); }
      for (const file of updaterFiles) if (!fs.existsSync(file)) throw missing("updater artifact", file);
      try { (deps.verifyUpdater ?? assertUpdaterMatchesShippedKey)({ appRoot, file: updater }); }
      catch (error) { throw wrap("MAC_PACKAGE_UPDATER_FAILED", "updater signature against the shipped key", error); }
      record("updater");
    }
  } catch (error) {
    // Fail closed: never leave a DMG or updater that skipped a step where the seal can find it.
    for (const file of [dmg, ...updaterFiles]) { try { fs.rmSync(file, { force: true }); } catch { /* best effort */ } }
    throw error;
  }
  return { steps, app, dmg, updater: recipe.updater ? updater : null, receipt: receipt ?? null, receiptPath: receipt ? receiptPath : null, signing };
}
