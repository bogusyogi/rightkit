// Declared native Swift helpers embedded in a macOS .app (shared by the dev lane and the release lane).
//
//   nativeHelpers: [{
//     name: "menubar",                       // unique label (logs, receipts)
//     package: "macos-native/Pkg"            // app-relative SwiftPM package dir ...
//     // sharedPackage: "cloudkit",          // ... or a RightKit shared Swift package key (exactly one of the two)
//     product: "MenuBar",                    // SwiftPM executable product
//     kind: "app" | "executable",
//     embed: "Contents/Library/LoginItems/MenuBar.app",   // path inside the .app
//     universal: true,                       // arm64 + x86_64 (required, explicit)
//     entitlements: "src-tauri/entitlements-x.plist",      // optional, app-relative
//     // kind "app" only (wrap the SwiftPM executable as a helper .app):
//     executable: "MenuBar",                 // CFBundleExecutable / file in Contents/MacOS (default: product)
//     info: { bundleId, name?, displayName?, uiElement?, minimumSystemVersion? },   // Info.plist fields
//     resources: [{ from: "src-tauri/icons/tray", to: "tray" }],   // copied into Contents/Resources
//     appIntents: { moduleName, deploymentTarget? },               // appintentsmetadataprocessor step
//     provisioningProfile: "src-tauri/provisioning/X.provisionprofile",  // -> Contents/embedded.provisionprofile
//   }]
//
// Build: `swift build -c <release|debug> --package-path <pkg> --product <product> --arch <a> [--arch <b>]` (SwiftPM, the same
// tool swift-package / assemble-app use; one explicit --arch per slice so the output lands in SwiftPM's xcbuild layout for
// both thin and universal builds), then `--show-bin-path` to find the product. Embedded BEFORE the app is signed. Signing is
// inside-out: release.mjs signs the helpers through sign.nestedCode (macPackage expansion derives it), the dev signer signs
// nested bundles itself, and signNativeHelpers below does the same for any other caller. Every external effect is injectable.
import { createHash } from "node:crypto";
import { buildLaunch } from "./build-launch.mjs";
import * as nodeFs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync as nodeSpawnSync } from "./background-process.mjs";
import { signMacosEntries } from "./mac-sign-core.mjs";
import { discoverNestedCode } from "./swift-mac-app.mjs";

export class NativeHelpersError extends Error {
  constructor(code, message, details = {}) { super(message); this.name = "NativeHelpersError"; this.code = code; this.details = details; }
}
const fail = (code, message, details) => { throw new NativeHelpersError(code, message, details); };
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

export const NATIVE_HELPER_KEYS = Object.freeze(["name", "package", "sharedPackage", "product", "kind", "embed", "universal", "entitlements", "executable", "info", "resources", "appIntents", "provisioningProfile"]);
const INFO_KEYS = ["bundleId", "name", "displayName", "uiElement", "minimumSystemVersion"];
const APP_INTENTS_KEYS = ["moduleName", "deploymentTarget"];
const RESOURCE_KEYS = ["from", "to"];
/** RightKit shared Swift packages, keyed, relative to the RightKit checkout root (env RIGHTKIT_SWIFT_ROOT overrides the root). */
export const SHARED_SWIFT_PACKAGES = Object.freeze({ cloudkit: "swift/RightKitCloudKitHelper" });
/** Where a helper may be embedded: only roots the inside-out signing discovery walks. */
const EMBED_ROOTS = ["Contents/Library/", "Contents/Helpers/", "Contents/XPCServices/", "Contents/PlugIns/", "Contents/MacOS/"];
const NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const PRODUCT = /^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$/;
const FLAT = /^[A-Za-z0-9_][A-Za-z0-9_. -]{0,127}$/;
const VERSION = /^\d+(\.\d+){0,2}$/;
const BUNDLE_ID = /^[A-Za-z0-9.-]+\.[A-Za-z0-9.-]+$/;
const ARCHS = ["arm64", "x86_64"];

function relative(value, label, errors, { ext } = {}) {
  if (typeof value !== "string" || !value.trim()) { errors.push(`${label} must be a non-empty relative path`); return undefined; }
  if (value.includes("\0") || value.includes("\\") || path.isAbsolute(value) || /^[A-Za-z]:/.test(value)) { errors.push(`${label} must be a relative path with forward slashes: ${JSON.stringify(value)}`); return undefined; }
  const segments = value.split("/");
  if (segments.some((segment) => segment === "" || segment === "." || segment === "..")) { errors.push(`${label} must not contain empty, '.' or '..' segments or escape its root: ${JSON.stringify(value)}`); return undefined; }
  if (ext && !ext.test(value)) { errors.push(`${label} must match ${ext}: ${JSON.stringify(value)}`); return undefined; }
  return value;
}
const unknown = (object, allowed, label, errors) => { for (const key of Object.keys(object)) if (!allowed.includes(key)) errors.push(`${label}: unknown key ${JSON.stringify(key)} (allowed: ${allowed.join(", ")})`); };

/** Strictly validate and normalize a `nativeHelpers` list. Every problem is reported at once (NATIVE_HELPERS_INVALID). */
export function validateNativeHelpers(list, { label = "nativeHelpers" } = {}) {
  const errors = [];
  if (!Array.isArray(list) || !list.length) fail("NATIVE_HELPERS_INVALID", `${label} must be a non-empty array`);
  const out = [];
  list.forEach((raw, index) => {
    const at = `${label}[${index}]`;
    if (!isObject(raw)) { errors.push(`${at} must be an object`); return; }
    unknown(raw, NATIVE_HELPER_KEYS, at, errors);
    const helper = {};
    if (typeof raw.name !== "string" || !NAME.test(raw.name)) errors.push(`${at}.name must be 1-64 characters of letters, digits, '.', '_' and '-'`); else helper.name = raw.name;
    if ((raw.package === undefined) === (raw.sharedPackage === undefined)) errors.push(`${at} needs exactly one of package or sharedPackage`);
    else if (raw.package !== undefined) { const value = relative(raw.package, `${at}.package`, errors); if (value) helper.package = value; }
    else if (typeof raw.sharedPackage !== "string" || !Object.hasOwn(SHARED_SWIFT_PACKAGES, raw.sharedPackage)) errors.push(`${at}.sharedPackage must be one of ${Object.keys(SHARED_SWIFT_PACKAGES).join(", ")}`);
    else helper.sharedPackage = raw.sharedPackage;
    if (typeof raw.product !== "string" || !PRODUCT.test(raw.product)) errors.push(`${at}.product must be a SwiftPM product name`); else helper.product = raw.product;
    if (raw.kind !== "app" && raw.kind !== "executable") errors.push(`${at}.kind must be "app" or "executable"`); else helper.kind = raw.kind;
    if (typeof raw.universal !== "boolean") errors.push(`${at}.universal is required and must be true or false`); else helper.universal = raw.universal;
    const embed = relative(raw.embed, `${at}.embed`, errors);
    if (embed) {
      if (!EMBED_ROOTS.some((root) => embed.startsWith(root)) || embed.split("/").length < 3) errors.push(`${at}.embed must be inside the .app under ${EMBED_ROOTS.join(" | ")}: ${embed}`);
      else if (helper.kind === "app" && (!embed.endsWith(".app") || embed.startsWith("Contents/MacOS/"))) errors.push(`${at}.embed for kind "app" must be a <Name>.app below Contents/Library, Helpers, XPCServices or PlugIns: ${embed}`);
      else if (helper.kind === "executable" && (embed.endsWith(".app") || (embed.startsWith("Contents/MacOS/") && embed.split("/").length !== 3))) errors.push(`${at}.embed for kind "executable" must be a file (Contents/MacOS/<name> or a path below another allowed root): ${embed}`);
      else helper.embed = embed;
    }
    if (raw.entitlements !== undefined) { const value = relative(raw.entitlements, `${at}.entitlements`, errors, { ext: /\.plist$/i }); if (value) helper.entitlements = value; }
    if (raw.executable !== undefined) {
      if (raw.kind !== "app") errors.push(`${at}.executable applies only to kind "app"`);
      else if (typeof raw.executable !== "string" || !FLAT.test(raw.executable) || raw.executable.includes("/")) errors.push(`${at}.executable must be a flat file name`);
      else helper.executable = raw.executable;
    }
    if (raw.kind === "app") {
      if (!isObject(raw.info)) errors.push(`${at}.info is required for kind "app" ({ bundleId, name?, displayName?, uiElement?, minimumSystemVersion? })`);
      else {
        unknown(raw.info, INFO_KEYS, `${at}.info`, errors);
        const info = {};
        if (typeof raw.info.bundleId !== "string" || !BUNDLE_ID.test(raw.info.bundleId)) errors.push(`${at}.info.bundleId must be reverse-DNS`); else info.bundleId = raw.info.bundleId;
        for (const key of ["name", "displayName"]) if (raw.info[key] !== undefined) { if (typeof raw.info[key] !== "string" || !FLAT.test(raw.info[key])) errors.push(`${at}.info.${key} must be a simple name`); else info[key] = raw.info[key]; }
        if (raw.info.uiElement !== undefined) { if (typeof raw.info.uiElement !== "boolean") errors.push(`${at}.info.uiElement must be a boolean`); else info.uiElement = raw.info.uiElement; }
        if (raw.info.minimumSystemVersion !== undefined) { if (typeof raw.info.minimumSystemVersion !== "string" || !VERSION.test(raw.info.minimumSystemVersion)) errors.push(`${at}.info.minimumSystemVersion must be 1-3 numeric parts`); else info.minimumSystemVersion = raw.info.minimumSystemVersion; }
        helper.info = info;
      }
      if (raw.resources !== undefined) {
        if (!Array.isArray(raw.resources)) errors.push(`${at}.resources must be an array of { from, to }`);
        else helper.resources = raw.resources.map((item, i) => {
          const where = `${at}.resources[${i}]`;
          if (!isObject(item)) { errors.push(`${where} must be an object`); return undefined; }
          unknown(item, RESOURCE_KEYS, where, errors);
          const from = relative(item.from, `${where}.from`, errors);
          const to = relative(item.to, `${where}.to`, errors);
          return from && to ? { from, to } : undefined;
        }).filter(Boolean);
      }
      if (raw.appIntents !== undefined) {
        if (!isObject(raw.appIntents)) errors.push(`${at}.appIntents must be { moduleName, deploymentTarget? }`);
        else {
          unknown(raw.appIntents, APP_INTENTS_KEYS, `${at}.appIntents`, errors);
          if (typeof raw.appIntents.moduleName !== "string" || !PRODUCT.test(raw.appIntents.moduleName)) errors.push(`${at}.appIntents.moduleName must be a Swift module name`);
          if (raw.appIntents.deploymentTarget !== undefined && (typeof raw.appIntents.deploymentTarget !== "string" || !VERSION.test(raw.appIntents.deploymentTarget))) errors.push(`${at}.appIntents.deploymentTarget must be 1-3 numeric parts`);
          helper.appIntents = { moduleName: raw.appIntents.moduleName, deploymentTarget: raw.appIntents.deploymentTarget ?? "14.0" };
        }
      }
      if (raw.provisioningProfile !== undefined) { const value = relative(raw.provisioningProfile, `${at}.provisioningProfile`, errors, { ext: /\.(provisionprofile|mobileprovision)$/ }); if (value) helper.provisioningProfile = value; }
    } else if (raw.kind === "executable") {
      for (const key of ["info", "resources", "appIntents", "provisioningProfile"]) if (raw[key] !== undefined) errors.push(`${at}.${key} applies only to kind "app"`);
    }
    out.push(helper);
  });
  const names = new Set();
  const embeds = [];
  for (const helper of out) {
    if (helper.name) { if (names.has(helper.name)) errors.push(`${label}: duplicate name ${helper.name}`); names.add(helper.name); }
    if (helper.embed) {
      const clash = embeds.find((other) => other === helper.embed || other.startsWith(`${helper.embed}/`) || helper.embed.startsWith(`${other}/`));
      if (clash) errors.push(`${label}: embed paths must be unique and not nested in each other (${clash} vs ${helper.embed})`);
      embeds.push(helper.embed);
    }
  }
  if (errors.length) fail("NATIVE_HELPERS_INVALID", `invalid ${label}:\n${errors.map((line) => `- ${line}`).join("\n")}`, { errors });
  return out;
}

/** Release signing config derived from the list: `sign.nestedCode[embed]` per helper (hardened runtime is implicit, entitlements per helper). */
export function nativeHelperNestedCode(helpers) {
  return Object.fromEntries(helpers.map((helper) => [helper.embed, helper.entitlements ? { entitlements: helper.entitlements } : {}]));
}
/** Dev signer map `{ <bundle-relative embed>: <entitlements plist> }` for helpers that declare entitlements (merge into install.entitlements). */
export function nativeHelperEntitlementMap(helpers) {
  return Object.fromEntries(helpers.filter((helper) => helper.entitlements).map((helper) => [helper.embed, helper.entitlements]));
}
/** The shared list from a release config: `targets.mac.macPackage.nativeHelpers` (undefined when none is declared). */
export function nativeHelpersFromConfig(config) {
  const raw = config?.targets?.mac?.macPackage?.nativeHelpers;
  return raw === undefined ? undefined : validateNativeHelpers(raw, { label: "targets.mac.macPackage.nativeHelpers" });
}

const esc = (value) => String(value).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
/** Info.plist for a wrapped helper .app (same fields the helper bundles always carried). */
export function helperInfoPlist({ helper, executable, version }) {
  const info = helper.info;
  const rows = [
    ["CFBundleExecutable", `<string>${esc(executable)}</string>`],
    ["CFBundleIdentifier", `<string>${esc(info.bundleId)}</string>`],
    ["CFBundleName", `<string>${esc(info.name ?? helper.product)}</string>`],
    ...(info.displayName ? [["CFBundleDisplayName", `<string>${esc(info.displayName)}</string>`]] : []),
    ["CFBundlePackageType", "<string>APPL</string>"],
    ["CFBundleShortVersionString", `<string>${esc(version)}</string>`],
    ["CFBundleVersion", `<string>${esc(version)}</string>`],
    ...(info.uiElement ? [["LSUIElement", "<true/>"]] : []),
    ["LSMinimumSystemVersion", `<string>${esc(info.minimumSystemVersion ?? "14.0")}</string>`],
  ];
  return `<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict>\n${rows.map(([key, value]) => `  <key>${key}</key>${value}`).join("\n")}\n</dict></plist>\n`;
}

const sha256 = (fs, file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const tail = (text, lines = 15) => String(text ?? "").split(/\r?\n/).slice(-lines).join("\n");
const archSet = (text) => String(text ?? "").trim().split(/\s+/).filter(Boolean).sort();

function findFile(fs, root, predicate) {
  let entries;
  try { entries = fs.readdirSync(root, { withFileTypes: true }); } catch { return null; }
  for (const entry of entries) {
    const candidate = path.join(root, entry.name);
    if (entry.isDirectory()) { const found = findFile(fs, candidate, predicate); if (found) return found; }
    else if (predicate(candidate.split(path.sep).join("/"))) return candidate;
  }
  return null;
}

/**
 * Build every helper with SwiftPM and embed it into `app` (before signing). Strictly in declared order; the first failure throws.
 * options: appRoot (absolute), app (absolute .app), helpers (validated or raw), version (required with a kind "app" helper),
 * forceArch ("arm64"|"x86_64": build only that slice for every helper, e.g. a dev build or a thin release triple),
 * configuration ("release"|"debug"), env, spawn, fs, log, replaceExisting (default false: an existing embed path is an error),
 * sharedRoot, hostArch, tmpdir. Without forceArch a universal helper gets both slices and a thin one the host arch.
 * Returns { helpers: [{ name, embed, path, source, archs, sha256 }], events: [...] }.
 */
export function buildAndEmbedNativeHelpers({ appRoot, app, helpers: rawHelpers, version, forceArch, configuration = "release", env = process.env, spawn = nodeSpawnSync, fs = nodeFs, log = () => {}, replaceExisting = false, sharedRoot, hostArch = process.arch === "arm64" ? "arm64" : "x86_64", tmpdir = os.tmpdir(), packageOwner = (pkg) => { const result = nodeSpawnSync("git", ["-C", pkg, "rev-parse", "--show-toplevel"], { encoding: "utf8", windowsHide: true }); return result.status === 0 ? String(result.stdout).trim() : null; } } = {}) {
  if (typeof appRoot !== "string" || !path.isAbsolute(appRoot)) fail("NATIVE_HELPERS_INVALID", "appRoot must be an absolute path");
  if (typeof app !== "string" || !path.isAbsolute(app) || !app.replace(/\/+$/, "").endsWith(".app")) fail("NATIVE_HELPERS_INVALID", "app must be the absolute path of the .app bundle");
  if (!["release", "debug"].includes(configuration)) fail("NATIVE_HELPERS_INVALID", "configuration must be release or debug");
  if (forceArch !== undefined && !ARCHS.includes(forceArch)) fail("NATIVE_HELPERS_INVALID", `forceArch must be one of ${ARCHS.join(", ")}`);
  const helpers = validateNativeHelpers(rawHelpers);
  if (helpers.some((helper) => helper.kind === "app") && (typeof version !== "string" || !VERSION.test(version))) fail("NATIVE_HELPERS_INVALID", "a kind \"app\" helper needs the app version (1-3 numeric parts) for its Info.plist");
  if (!fs.existsSync(path.join(app, "Contents", "Info.plist"))) fail("NATIVE_HELPERS_INPUT_MISSING", `${app} is not a built .app (no Contents/Info.plist); helpers are embedded after the app build`);
  const root = sharedRoot ?? env.RIGHTKIT_SWIFT_ROOT ?? fileURLToPath(new URL("../../", import.meta.url));
  const events = [];
  const results = [];
  const run = (what, cmd, args, { capture = false, cwd = appRoot } = {}) => {
    log(`native-helpers: ${what}: ${cmd} ${args.join(" ")}`);
    const launch = cmd === "swift" ? buildLaunch(cmd, args, { cwd, env, exists: fs.existsSync }) : { command: cmd, args };
    const result = spawn(launch.command, launch.args, { cwd, env, ...(capture ? { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] } : { stdio: "inherit" }), windowsHide: true });
    if (!result || result.error || result.status !== 0) fail("NATIVE_HELPERS_STEP_FAILED", `native helper ${what} failed (${result?.error?.message ?? `exit ${result?.status ?? result?.signal ?? "unknown"}`})${capture ? `: ${tail(result?.stderr)}` : ""}`, { cmd, status: result?.status ?? null });
    return result;
  };
  for (const helper of helpers) {
    const pkgDir = helper.package ? path.resolve(appRoot, helper.package) : path.resolve(root, SHARED_SWIFT_PACKAGES[helper.sharedPackage]);
    if (helper.package && path.relative(appRoot, pkgDir).startsWith("..")) fail("NATIVE_HELPERS_INVALID", `${helper.name}: package escapes the app root`);
    if (!fs.existsSync(path.join(pkgDir, "Package.swift"))) fail("NATIVE_HELPERS_INPUT_MISSING", `${helper.name}: no Package.swift in ${pkgDir}${helper.sharedPackage ? " (set RIGHTKIT_SWIFT_ROOT to the RightKit checkout)" : ""}`);
    const entitlements = helper.entitlements ? path.resolve(appRoot, helper.entitlements) : undefined;
    if (entitlements && !fs.existsSync(entitlements)) fail("NATIVE_HELPERS_INPUT_MISSING", `${helper.name}: entitlements file not found: ${entitlements}`);
    const archs = forceArch ? [forceArch] : helper.universal ? [...ARCHS] : [hostArch];
    // Each Swift job is admitted against the checkout that owns its package, including shared RightKit helpers.
    const packageRoot = packageOwner(pkgDir) || appRoot;
    const relativePackage = path.relative(packageRoot, pkgDir);
    const packageArg = relativePackage === "" ? "." : relativePackage;
    const swiftArgs = ["build", "-c", configuration, "--package-path", packageArg, "--product", helper.product, ...archs.flatMap((arch) => ["--arch", arch])];
    run(`build ${helper.name}`, "swift", swiftArgs, { cwd: packageRoot });
    events.push(`build:${helper.name}`);
    const binDir = String(run(`bin path ${helper.name}`, "swift", [...swiftArgs, "--show-bin-path"], { capture: true, cwd: packageRoot }).stdout ?? "").trim().split(/\r?\n/).pop();
    if (!binDir || !path.isAbsolute(binDir)) fail("NATIVE_HELPERS_STEP_FAILED", `${helper.name}: swift build --show-bin-path returned no absolute path`);
    const source = path.join(binDir, helper.product);
    if (!fs.existsSync(source)) fail("NATIVE_HELPERS_INPUT_MISSING", `${helper.name}: the build did not produce ${source}`);
    const found = archSet(run(`lipo ${helper.name}`, "lipo", ["-archs", source], { capture: true }).stdout);
    if (found.join(" ") !== [...archs].sort().join(" ")) fail("NATIVE_HELPERS_ARCH_MISMATCH", `${helper.name}: ${source} holds [${found.join(" ")}], expected exactly [${[...archs].sort().join(" ")}]`, { found, expected: archs });
    const dest = path.join(app, ...helper.embed.split("/"));
    if (fs.existsSync(dest)) {
      if (!replaceExisting) fail("NATIVE_HELPERS_EMBED_EXISTS", `${helper.name}: ${helper.embed} already exists in ${app}; something else wrote it (dev rebuilds pass replaceExisting)`);
      fs.rmSync(dest, { recursive: true, force: true });
    }
    let binary;
    if (helper.kind === "executable") {
      fs.mkdirSync(path.dirname(dest), { recursive: true });
      fs.copyFileSync(source, dest);
      fs.chmodSync(dest, 0o755);
      binary = dest;
    } else {
      const executable = helper.executable ?? helper.product;
      binary = path.join(dest, "Contents", "MacOS", executable);
      fs.mkdirSync(path.dirname(binary), { recursive: true });
      fs.writeFileSync(path.join(dest, "Contents", "Info.plist"), helperInfoPlist({ helper, executable, version }));
      fs.copyFileSync(source, binary);
      fs.chmodSync(binary, 0o755);
      for (const resource of helper.resources ?? []) {
        const from = path.resolve(appRoot, resource.from);
        if (!fs.existsSync(from)) fail("NATIVE_HELPERS_INPUT_MISSING", `${helper.name}: resource not found: ${from}`);
        const to = path.join(dest, "Contents", "Resources", ...resource.to.split("/"));
        fs.mkdirSync(path.dirname(to), { recursive: true });
        fs.cpSync(from, to, { recursive: true, verbatimSymlinks: true });
      }
      if (helper.appIntents) embedAppIntents({ helper, run, fs, tmpdir, pkgDir, binDir, dest, archs, configuration });
      if (helper.provisioningProfile) {
        const profile = path.resolve(appRoot, helper.provisioningProfile);
        if (!fs.existsSync(profile)) fail("NATIVE_HELPERS_INPUT_MISSING", `${helper.name}: provisioning profile not found: ${profile}`);
        fs.copyFileSync(profile, path.join(dest, "Contents", "embedded.provisionprofile"));
      }
    }
    events.push(`embed:${helper.name}`);
    results.push({ name: helper.name, embed: helper.embed, path: dest, source, archs: found, sha256: sha256(fs, binary) });
  }
  return { helpers: results, events };
}

/** The appintentsmetadataprocessor step: SwiftPM leaves the const-value inputs under Intermediates.noindex (xcbuild layout). */
function embedAppIntents({ helper, run, fs, tmpdir, pkgDir, binDir, dest, archs, configuration }) {
  const { moduleName, deploymentTarget } = helper.appIntents;
  const cfg = configuration === "debug" ? "Debug" : "Release";
  const arch = archs.includes("arm64") ? "arm64" : archs[0];
  const intermediates = path.join(path.resolve(binDir, "../.."), "Intermediates.noindex");
  const sourceList = findFile(fs, intermediates, (file) => file.endsWith(`/${moduleName}.SwiftFileList`) && file.includes(`/${cfg}/`) && file.includes(`/${arch}/`));
  const constVals = sourceList
    ? fs.readdirSync(path.dirname(sourceList), { withFileTypes: true }).filter((entry) => entry.isFile() && entry.name.endsWith(".swiftconstvalues")).map((entry) => path.join(path.dirname(sourceList), entry.name)).sort()
    : [];
  if (!sourceList || !constVals.length) fail("NATIVE_HELPERS_APPINTENTS_FAILED", `${helper.name}: SwiftPM did not emit App Intents metadata inputs for ${moduleName} under ${intermediates}`);
  const out = (what, cmd, args) => String(run(what, cmd, args, { capture: true }).stdout ?? "").trim();
  const swiftc = out("xcrun swiftc", "xcrun", ["--find", "swiftc"]);
  const sdkRoot = out("sdk path", "xcrun", ["--sdk", "macosx", "--show-sdk-path"]);
  const xcodeVersion = /^Xcode (.+)$/m.exec(out("xcodebuild version", "xcodebuild", ["-version"]))?.[1];
  if (!swiftc || !sdkRoot || !xcodeVersion) fail("NATIVE_HELPERS_APPINTENTS_FAILED", `${helper.name}: cannot determine swiftc, the macOS SDK and the Xcode version for App Intents metadata`);
  const scratch = fs.mkdtempSync(path.join(tmpdir, "rightkit-appintents-"));
  const list = path.join(scratch, "swift-const-values.list");
  const output = path.join(dest, "Contents", "Resources");
  try {
    fs.writeFileSync(list, `${constVals.join("\n")}\n`);
    run(`appintents metadata ${helper.name}`, path.join(path.dirname(swiftc), "appintentsmetadataprocessor"), [
      "--output", output, "--toolchain-dir", path.dirname(path.dirname(path.dirname(swiftc))), "--module-name", moduleName, "--sdk-root", sdkRoot,
      "--xcode-version", xcodeVersion, "--platform-family", "macOS", "--deployment-target", deploymentTarget, "--target-triple", `${arch}-apple-macosx${deploymentTarget}`,
      "--source-file-list", sourceList, "--swift-const-vals-list", list, "--force"], { cwd: pkgDir });
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
  if (!fs.existsSync(path.join(output, "Metadata.appintents", "extract.actionsdata"))) fail("NATIVE_HELPERS_APPINTENTS_FAILED", `${helper.name}: App Intents metadata generation produced no Metadata.appintents/extract.actionsdata`);
}

const depth = (file) => file.split(/[\\/]/).length;

/**
 * Inside-out signing plan for embedded helpers: for every helper the code nested inside it first (a kind "app" helper's
 * own frameworks and plug-ins; its main executable is sealed with its bundle), then the helper itself with its own
 * entitlements and the hardened runtime; deepest path first overall. The app itself is NOT in the plan (it is signed last).
 */
export function planNativeHelperSigning({ app, appRoot, helpers: rawHelpers, discover = discoverNestedCode, readExecutable }) {
  const helpers = validateNativeHelpers(rawHelpers);
  const entries = [];
  for (const helper of helpers) {
    const file = path.join(app, ...helper.embed.split("/"));
    if (helper.kind === "app") {
      const executable = helper.executable ?? helper.product;
      for (const nested of discover(file, readExecutable ? readExecutable(file) : executable)) entries.push({ file: nested, hardenedRuntime: true, options: [] });
    }
    entries.push({ file, hardenedRuntime: true, options: [], ...(helper.entitlements ? { entitlements: path.resolve(appRoot, helper.entitlements) } : {}) });
  }
  return entries.sort((a, b) => depth(b.file) - depth(a.file) || a.file.localeCompare(b.file));
}

/** Sign the helpers inside-out (see planNativeHelperSigning). deps: identity, signOne(codesignArgs), inspect(cmd, args) -> text. */
export function signNativeHelpers({ app, appRoot, helpers, identity, signOne, inspect, discover, evidence, hashFile }) {
  if (typeof identity !== "string" || !identity.trim()) fail("NATIVE_HELPERS_SIGN_FAILED", "a signing identity is required");
  const entries = planNativeHelperSigning({ app, appRoot, helpers, ...(discover ? { discover } : {}) });
  return signMacosEntries(entries, { identity, signOne, inspect, ...(evidence ? { evidence } : {}), ...(hashFile ? { hashFile } : {}) });
}

/**
 * Whole sequence for a caller that owns signing itself: build helpers, embed, sign helpers inside-out, sign the app
 * (`signApp()` supplied by the caller), verify (`codesign --verify --strict --deep`). Returns { build, signing, events }.
 */
export function buildEmbedSignNativeHelpers({ signApp, verify = true, ...options }) {
  if (typeof signApp !== "function") fail("NATIVE_HELPERS_INVALID", "signApp is required: the app is signed after its helpers");
  const built = buildAndEmbedNativeHelpers(options);
  const events = [...built.events];
  const signing = signNativeHelpers({ app: options.app, appRoot: options.appRoot, helpers: options.helpers, ...options.signing });
  for (const record of signing) events.push(`sign:${path.relative(options.app, record.file).split(path.sep).join("/")}`);
  signApp();
  events.push("sign:app");
  if (verify) {
    const spawn = options.spawn ?? nodeSpawnSync;
    const checked = spawn("codesign", ["--verify", "--strict", "--deep", options.app], { cwd: options.appRoot, env: options.env ?? process.env, encoding: "utf8", stdio: "pipe", windowsHide: true });
    if (!checked || checked.error || checked.status !== 0) fail("NATIVE_HELPERS_VERIFY_FAILED", `codesign --verify --strict --deep failed for ${options.app}: ${tail(checked?.stderr)}`);
    events.push("verify");
  }
  return { build: built, signing, events };
}
