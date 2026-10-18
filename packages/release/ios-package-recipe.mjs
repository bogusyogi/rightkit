// Declarative iOS packaging recipe (bogusyogi/rightkit#349). A `targets.ios` entry with `iosPackage` replaces the per-app
// archive / export scripts (HeardRight appstore-archive.sh, and the missing ones in ScrapeRight, CodeRight and Pulse):
//
//   1. preflight       host is macOS, GUI signing session, xcodebuild (and xcodegen when a project.yml is named) exist
//   2. generate        `xcodegen generate` when the config names a project.yml; REQUIRED, never a fallback to a committed xcodeproj
//   3. prepare         the app's own argv commands (no shell)
//   4. archive         xcodebuild archive, automatic signing, -allowProvisioningUpdates, date-based build number
//   5. verify-archive  Info.plist identity (app, appex, Watch: one version), bundle id, build number, QA scan, forbidden strings
//   6. export          ExportOptions.plist (app-store-connect, destination export) and xcodebuild -exportArchive
//   7. verify-ipa      extract the IPA: version identity again, entitlements (no get-task-allow, production APNs),
//                      embedded.mobileprovision (team, application identifier, no device list), code signature
//   8. receipt         .right-release/receipts/ios-archive.json with the sha256 of the FINAL .ipa bytes
//
// Upload is a separate command (ios-upload.mjs): `build` never uploads, `upload` never builds. Nothing here reads an App Store
// Connect credential: archive and export sign through the Xcode account of the GUI session. Fails closed: every problem
// throws IosPackageError, and once an archive or an IPA exists a failure deletes them (and any stale receipt) so a later
// seal cannot pick up a half-verified artifact. Every external effect is injectable through `deps`.
import { createHash } from "node:crypto";
import * as nodeFs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { assertGuiSigningSession, IosHostError, outputTail, resolveXcodeStorage } from "./ios-host.mjs";

export const IOS_SIGNING_CONTRACT = "ios-xcodebuild-archive-automatic-v1";
export const IOS_RECEIPT_NAME = "ios-archive.json";
export const IOS_PACKAGE_STEPS = Object.freeze(["preflight", "generate", "prepare", "archive", "verify-archive", "export", "verify-ipa", "receipt"]);
export const IOS_DEFAULT_TIMEOUT_MS = 60 * 60 * 1000;

/**
 * Codes: IOS_PACKAGE_CONFIG_INVALID, IOS_PACKAGE_HOST_UNSUPPORTED, IOS_PACKAGE_TOOL_MISSING, IOS_PACKAGE_GENERATE_FAILED,
 * IOS_PACKAGE_PREPARE_FAILED, IOS_PACKAGE_ARCHIVE_FAILED, IOS_PACKAGE_SOURCE_CHANGED, IOS_PACKAGE_VERIFY_FAILED,
 * IOS_PACKAGE_EXPORT_FAILED, IOS_PACKAGE_BUILD_NUMBER, IOS_PACKAGE_RECEIPT_FAILED.
 */
export class IosPackageError extends Error {
  constructor(code, message, options) { super(message, options); this.name = "IosPackageError"; this.code = code; }
}
const invalid = (message) => new IosPackageError("IOS_PACKAGE_CONFIG_INVALID", `iosPackage: ${message}`);
const isPlainObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

// ---- config ------------------------------------------------------------------------------------------------------
const IOS_KEYS = ["project", "projectSpec", "scheme", "configuration", "teamId", "bundleId", "exportMethod", "signing", "buildNumber", "extraBuildSettings", "packageResolved", "trustedSwiftMacros", "trustedSwiftPlugins", "prepare", "cleanup", "checks", "archive", "ipa", "timeoutMs"];
const CLEANUP_KEYS = ["derivedData"];
const CHECK_KEYS = ["versionIdentity", "forbidStrings", "plist"];
const COMMAND_KEYS = ["cmd", "args", "cwd"];
const TEST_SEGMENT = /^(?:dev|qa|test|debug)$/i;
const RESERVED_SETTING = /^(?:DEVELOPMENT_TEAM|CURRENT_PROJECT_VERSION|CODE_SIGN\w*|PROVISIONING\w*|OTHER_CODE_SIGN_FLAGS|ARCHS?|SDKROOT|SKIP_INSTALL|PRODUCT_BUNDLE_IDENTIFIER)$/;
const SECRET_SETTING = /SECRET|TOKEN|PASSWORD|PASSWD|BEARER|PRIVATE|CREDENTIAL|API_?KEY|AUTH|SIGNING_KEY/i;
export const SECRET_VALUE = /-----BEGIN [A-Z ]*PRIVATE KEY-----|Bearer\s+\S{8,}|[\w-]{16,}\.[\w-]{16,}\.[\w-]{8,}/;

function relativeInside(value, label, extension) {
  if (typeof value !== "string" || !value.trim()) throw invalid(`${label} must be a non-empty relative path`);
  const normalized = value.replaceAll("\\", "/");
  if (path.isAbsolute(value) || path.win32.isAbsolute(value) || /^[A-Za-z]:/.test(value) || normalized.split("/").includes("..")) throw invalid(`${label} must be relative and stay inside the app: ${value}`);
  if (extension && !extension.test(normalized)) throw invalid(`${label} must match ${extension}: ${value}`);
  return normalized.replace(/^\.\//, "");
}

function commandList(value, label) {
  if (!Array.isArray(value)) throw invalid(`${label} must be an array of { cmd, args?, cwd? }`);
  return value.map((item, index) => {
    const where = `${label}[${index}]`;
    if (!isPlainObject(item)) throw invalid(`${where} must be an object`);
    const unknown = Object.keys(item).filter((key) => !COMMAND_KEYS.includes(key));
    if (unknown.length) throw invalid(`${where} has unknown key(s): ${unknown.join(", ")} (allowed: ${COMMAND_KEYS.join(", ")})`);
    if (typeof item.cmd !== "string" || !item.cmd.trim() || /\s/.test(item.cmd.trim())) throw invalid(`${where}.cmd must be one executable; put its arguments in args`);
    if (item.args !== undefined && (!Array.isArray(item.args) || item.args.some((arg) => typeof arg !== "string"))) throw invalid(`${where}.args must be a list of strings`);
    return { cmd: item.cmd.trim(), args: [...(item.args ?? [])], ...(item.cwd === undefined ? {} : { cwd: relativeInside(item.cwd, `${where}.cwd`) }) };
  });
}

const TRUST_ENTRY_KEYS = ["package", "revision"];
const SWIFT_IDENTITY = /^[a-z0-9][a-z0-9._-]{0,100}$/;
const GIT_REVISION = /^[0-9a-f]{40}$/;
/** Where Xcode reads the SwiftPM pins of a project (`-project` builds). */
export const defaultPackageResolvedPath = (project) => path.posix.join(project, "project.xcworkspace", "xcshareddata", "swiftpm", "Package.resolved");

function trustList(value, label) {
  if (!Array.isArray(value)) throw invalid(`${label} must be an array of { package, revision }`);
  const seen = new Set();
  return value.map((item, index) => {
    const where = `${label}[${index}]`;
    if (!isPlainObject(item)) throw invalid(`${where} must be an object { package, revision }`);
    const unknown = Object.keys(item).filter((key) => !TRUST_ENTRY_KEYS.includes(key));
    if (unknown.length) throw invalid(`${where} has unknown key(s): ${unknown.join(", ")} (allowed: ${TRUST_ENTRY_KEYS.join(", ")})`);
    if (typeof item.package !== "string" || !SWIFT_IDENTITY.test(item.package)) throw invalid(`${where}.package must be a lowercase SwiftPM package identity such as swift-composable-architecture`);
    if (typeof item.revision !== "string" || !GIT_REVISION.test(item.revision)) throw invalid(`${where}.revision must be the exact 40 character lowercase hex git commit the package is pinned to (no tag, branch or abbreviation)`);
    if (seen.has(item.package)) throw invalid(`${label} lists ${item.package} twice`);
    seen.add(item.package);
    return { package: item.package, revision: item.revision };
  });
}

/** Strictly validate and normalize an `iosPackage` value. Unknown keys, wrong types and missing fields throw. */
export function validateIosPackageConfig(spec) {
  if (!isPlainObject(spec)) throw invalid("must be an object");
  const unknown = Object.keys(spec).filter((key) => !IOS_KEYS.includes(key));
  if (unknown.length) throw invalid(`unknown key(s): ${unknown.join(", ")} (allowed: ${IOS_KEYS.join(", ")})`);

  const project = relativeInside(spec.project, "project", /\.xcodeproj$/i);
  const projectSpec = spec.projectSpec === undefined ? null : relativeInside(spec.projectSpec, "projectSpec", /\.ya?ml$/i);
  if (projectSpec && path.posix.dirname(projectSpec) !== path.posix.dirname(project)) throw invalid("projectSpec and project must be in the same directory (xcodegen writes the project beside its project.yml)");

  if (typeof spec.scheme !== "string" || !/^[A-Za-z0-9][A-Za-z0-9 ._-]{0,100}$/.test(spec.scheme)) throw invalid("scheme is required and must be an Xcode scheme name");
  const configuration = spec.configuration ?? "Release";
  if (typeof configuration !== "string" || !/^[A-Za-z][A-Za-z0-9 _-]{0,40}$/.test(configuration)) throw invalid("configuration must be a build configuration name");
  if (/debug/i.test(configuration)) throw invalid(`configuration ${configuration} is a debug configuration; a release archives Release`);
  if (typeof spec.teamId !== "string" || !/^[A-Z0-9]{10}$/.test(spec.teamId)) throw invalid("teamId is required: the 10 character Apple team id (there is no built-in default)");
  if (typeof spec.bundleId !== "string" || !/^[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+$/.test(spec.bundleId)) throw invalid("bundleId is required and must be the reverse-DNS app bundle id");
  if (TEST_SEGMENT.test(spec.bundleId.split(".").at(-1))) throw invalid(`bundleId ${spec.bundleId} is a dev/qa/test/debug id; those never go through the release lane`);
  if (spec.exportMethod !== undefined && spec.exportMethod !== "app-store-connect") throw invalid("exportMethod must be app-store-connect (the only method v1 supports)");
  if (spec.signing !== undefined && spec.signing !== "automatic") throw invalid("signing must be automatic (v1 signs with Xcode automatic signing and -allowProvisioningUpdates)");
  const buildNumber = spec.buildNumber === undefined ? "date" : isPlainObject(spec.buildNumber) ? spec.buildNumber.kind : spec.buildNumber;
  if (buildNumber !== "date" || (isPlainObject(spec.buildNumber) && Object.keys(spec.buildNumber).some((key) => key !== "kind"))) throw invalid('buildNumber must be "date" (YYYYMMDDNN); the marketing version stays as the app sets it');

  const extraBuildSettings = {};
  if (spec.extraBuildSettings !== undefined) {
    if (!isPlainObject(spec.extraBuildSettings)) throw invalid("extraBuildSettings must be an object of NAME: value");
    for (const [name, value] of Object.entries(spec.extraBuildSettings)) {
      if (!/^[A-Z][A-Z0-9_]*$/.test(name)) throw invalid(`extraBuildSettings.${name} is not a build setting name`);
      if (RESERVED_SETTING.test(name)) throw invalid(`extraBuildSettings.${name} is set by the recipe or by signing; it cannot be overridden`);
      if (SECRET_SETTING.test(name)) throw invalid(`extraBuildSettings.${name} looks like a secret; build settings travel on the command line and must be non-secret`);
      if (typeof value !== "string" || /[\0\r\n]/.test(value) || SECRET_VALUE.test(value)) throw invalid(`extraBuildSettings.${name} must be a single-line non-secret string`);
      extraBuildSettings[name] = value;
    }
  }

  const trustedSwiftMacros = spec.trustedSwiftMacros === undefined ? [] : trustList(spec.trustedSwiftMacros, "trustedSwiftMacros");
  const trustedSwiftPlugins = spec.trustedSwiftPlugins === undefined ? [] : trustList(spec.trustedSwiftPlugins, "trustedSwiftPlugins");
  const trusting = trustedSwiftMacros.length > 0 || trustedSwiftPlugins.length > 0;
  const packageResolved = spec.packageResolved === undefined ? null : relativeInside(spec.packageResolved, "packageResolved", /(?:^|\/)Package\.resolved$/);
  if (packageResolved && !trusting) throw invalid("packageResolved is only used with trustedSwiftMacros or trustedSwiftPlugins; remove it or list the reviewed packages");
  if (trusting && projectSpec && !packageResolved) throw invalid("trustedSwiftMacros/trustedSwiftPlugins with a projectSpec need packageResolved: the xcodegen project is regenerated, so its own Package.resolved is untracked; name a committed Package.resolved outside the xcodeproj");

  const cleanup = { derivedData: false };
  if (spec.cleanup !== undefined) {
    if (!isPlainObject(spec.cleanup)) throw invalid("cleanup must be an object");
    const bad = Object.keys(spec.cleanup).filter((key) => !CLEANUP_KEYS.includes(key));
    if (bad.length) throw invalid(`cleanup has unknown key(s): ${bad.join(", ")} (allowed: ${CLEANUP_KEYS.join(", ")})`);
    if (spec.cleanup.derivedData !== undefined && typeof spec.cleanup.derivedData !== "boolean") throw invalid("cleanup.derivedData must be a boolean");
    cleanup.derivedData = spec.cleanup.derivedData === true;
  }

  const checks = { versionIdentity: true, forbidStrings: [], plist: {} };
  if (spec.checks !== undefined) {
    if (!isPlainObject(spec.checks)) throw invalid("checks must be an object");
    const bad = Object.keys(spec.checks).filter((key) => !CHECK_KEYS.includes(key));
    if (bad.length) throw invalid(`checks has unknown key(s): ${bad.join(", ")} (allowed: ${CHECK_KEYS.join(", ")})`);
    if (spec.checks.versionIdentity !== undefined && spec.checks.versionIdentity !== true) throw invalid("checks.versionIdentity cannot be turned off: the app, appex and Watch plists must carry one version");
    if (spec.checks.forbidStrings !== undefined) {
      if (!Array.isArray(spec.checks.forbidStrings) || spec.checks.forbidStrings.some((item) => typeof item !== "string" || !item || item.length > 200 || item.includes("\0"))) throw invalid("checks.forbidStrings must be a list of non-empty strings");
      checks.forbidStrings = [...spec.checks.forbidStrings];
    }
    if (spec.checks.plist !== undefined) {
      if (!isPlainObject(spec.checks.plist)) throw invalid("checks.plist must be an object of Info.plist key: expected value");
      for (const [key, value] of Object.entries(spec.checks.plist)) {
        if (!key || !["string", "boolean", "number"].includes(typeof value)) throw invalid(`checks.plist.${key} must be a string, boolean or number`);
      }
      checks.plist = { ...spec.checks.plist };
    }
  }

  let timeoutMs = IOS_DEFAULT_TIMEOUT_MS;
  if (spec.timeoutMs !== undefined) {
    if (!Number.isInteger(spec.timeoutMs) || spec.timeoutMs < 1) throw invalid("timeoutMs must be a positive integer (milliseconds)");
    timeoutMs = spec.timeoutMs;
  }

  return {
    project,
    projectSpec,
    scheme: spec.scheme,
    configuration,
    teamId: spec.teamId,
    bundleId: spec.bundleId,
    exportMethod: "app-store-connect",
    signing: "automatic",
    buildNumber: "date",
    extraBuildSettings,
    packageResolved,
    trustedSwiftMacros,
    trustedSwiftPlugins,
    prepare: spec.prepare === undefined ? [] : commandList(spec.prepare, "prepare"),
    cleanup,
    checks,
    archive: relativeInside(spec.archive, "archive", /\.xcarchive$/i),
    ipa: relativeInside(spec.ipa, "ipa", /\.ipa$/i),
    timeoutMs,
  };
}

// ---- pure builders -----------------------------------------------------------------------------------------------
const xmlEscape = (value) => String(value).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

/** ExportOptions.plist: app-store-connect, export (the upload is a separate command), automatic signing, our build number kept. */
export function exportOptionsPlist(spec) {
  return [
    '<?xml version="1.0" encoding="UTF-8"?>',
    '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">',
    '<plist version="1.0">',
    "<dict>",
    "  <key>destination</key><string>export</string>",
    "  <key>manageAppVersionAndBuildNumber</key><false/>",
    `  <key>method</key><string>${xmlEscape(spec.exportMethod)}</string>`,
    "  <key>signingStyle</key><string>automatic</string>",
    "  <key>stripSwiftSymbols</key><true/>",
    `  <key>teamID</key><string>${xmlEscape(spec.teamId)}</string>`,
    "  <key>uploadSymbols</key><true/>",
    "</dict>",
    "</plist>",
    "",
  ].join("\n");
}

/**
 * YYYYMMDDNN: the UTC date plus a two-digit counter that rises for every build on the same day. Fails closed: only a missing file
 * (ENOENT) means "first build"; an unreadable, corrupt or malformed counter, or one from a later day than the clock, is an error
 * (a silently restarted counter could repeat a build number Apple already holds). The write is atomic (temp file, then rename).
 */
export function nextDateBuildNumber({ now, stateFile, fs = nodeFs }) {
  const day = now.toISOString().slice(0, 10).replaceAll("-", "");
  const broken = (why) => new IosPackageError("IOS_PACKAGE_BUILD_NUMBER", `the build number counter ${stateFile} is ${why}; fix or remove it only after checking the highest build number already uploaded`);
  let seq = 1;
  let raw;
  try { raw = fs.readFileSync(stateFile, "utf8"); } catch (error) {
    if (error?.code !== "ENOENT") throw broken(`unreadable (${error?.code ?? "error"})`);
  }
  if (raw !== undefined) {
    let previous;
    try { previous = JSON.parse(raw); } catch { throw broken("not valid JSON"); }
    if (!previous || typeof previous !== "object" || !/^\d{8}$/.test(String(previous.day)) || !Number.isInteger(previous.seq) || previous.seq < 1 || previous.seq > 99) throw broken("malformed");
    if (previous.day > day) throw broken(`from ${previous.day}, later than today (${day}); the clock moved back`);
    if (previous.day === day) seq = previous.seq + 1;
  }
  if (seq > 99) throw new IosPackageError("IOS_PACKAGE_BUILD_NUMBER", `more than 99 iOS builds on ${day}; the YYYYMMDDNN build number is exhausted for today`);
  fs.mkdirSync(path.dirname(stateFile), { recursive: true });
  const temporary = `${stateFile}.tmp-${process.pid}`;
  fs.writeFileSync(temporary, `${JSON.stringify({ day, seq }, null, 2)}\n`);
  fs.renameSync(temporary, stateFile);
  return `${day}${String(seq).padStart(2, "0")}`;
}

export function xcodegenArgv(spec, root) {
  return ["generate", "--spec", path.resolve(root, spec.projectSpec), "--project", path.resolve(root, path.posix.dirname(spec.project))];
}

// ---- reviewed Swift macro / plugin trust ---------------------------------------------------------------------------
// Xcode refuses a macro or build-tool plugin the user has not enabled ("Macro ... must be enabled before it can be used").
// -skipMacroValidation / -skipPackagePluginValidation are BUILD-WIDE switches (they trust every macro/plugin of every
// package), so they are added only when the app lists the exact reviewed package revisions AND every one is pinned to that
// revision in a tracked, hashed Package.resolved. -disableAutomaticPackageResolution and -onlyUsePackageVersionsFromResolvedFile
// then stop Xcode from moving a pin after the check. Without a list nothing changes.
const VERIFIED_TRUST = new WeakSet();
const trustError = (code, message) => new IosPackageError(code, message);

/** Pure: parse a Package.resolved (format versions 2 and 3) into pins keyed by lowercase identity. Anything else throws. */
export function parsePackageResolved(text) {
  let json;
  try { json = JSON.parse(String(text)); } catch { throw trustError("IOS_PACKAGE_TRUST_FAILED", "Package.resolved is not valid JSON"); }
  if (!isPlainObject(json) || (json.version !== 2 && json.version !== 3)) throw trustError("IOS_PACKAGE_TRUST_FAILED", `Package.resolved version ${JSON.stringify(json?.version)} is not supported (only versions 2 and 3, which key pins by identity)`);
  if (!Array.isArray(json.pins)) throw trustError("IOS_PACKAGE_TRUST_FAILED", "Package.resolved has no pins array");
  const pins = new Map();
  for (const [index, pin] of json.pins.entries()) {
    if (!isPlainObject(pin) || typeof pin.identity !== "string" || !pin.identity) throw trustError("IOS_PACKAGE_TRUST_FAILED", `Package.resolved pins[${index}] has no identity`);
    const identity = pin.identity.toLowerCase();
    if (pins.has(identity)) throw trustError("IOS_PACKAGE_TRUST_FAILED", `Package.resolved pins ${identity} twice`);
    const state = isPlainObject(pin.state) ? pin.state : {};
    pins.set(identity, { identity, kind: String(pin.kind ?? ""), location: String(pin.location ?? ""), revision: typeof state.revision === "string" ? state.revision.toLowerCase() : null, version: typeof state.version === "string" ? state.version : null });
  }
  return pins;
}

/**
 * Throws IOS_PACKAGE_TRUST_MISMATCH naming every package that is not pinned, or is pinned to another revision than its
 * allowlist entry. Returns the evidence object that archiveArgv requires before it adds a skip flag.
 */
export function assertTrustedPins(pins, { trustedSwiftMacros = [], trustedSwiftPlugins = [] }) {
  const problems = [];
  const evidence = { macros: [], plugins: [] };
  for (const [listName, key, list] of [["trustedSwiftMacros", "macros", trustedSwiftMacros], ["trustedSwiftPlugins", "plugins", trustedSwiftPlugins]]) {
    for (const entry of list) {
      const pin = pins.get(entry.package);
      if (!pin) problems.push(`${listName}: ${entry.package} is not pinned in Package.resolved (reviewed revision ${entry.revision})`);
      else if (!pin.revision) problems.push(`${listName}: ${entry.package} is pinned without a git revision (${pin.kind || "unknown kind"}); reviewed revision ${entry.revision}`);
      else if (pin.revision !== entry.revision) problems.push(`${listName}: ${entry.package} is pinned to ${pin.revision} in Package.resolved but the reviewed revision is ${entry.revision}`);
      else evidence[key].push({ package: entry.package, revision: entry.revision, location: pin.location });
    }
  }
  if (problems.length) throw trustError("IOS_PACKAGE_TRUST_MISMATCH", `Swift macro/plugin trust refused; review the update, then change the allowlist in the same commit as Package.resolved:\n- ${problems.join("\n- ")}`);
  VERIFIED_TRUST.add(evidence);
  return evidence;
}

/** The xcodebuild flags for the trust lists. Fails closed: a non-empty list without verified evidence throws. */
export function swiftTrustFlags(spec, trusted) {
  const macros = spec.trustedSwiftMacros?.length > 0;
  const plugins = spec.trustedSwiftPlugins?.length > 0;
  if (!macros && !plugins) return [];
  if (!trusted || !VERIFIED_TRUST.has(trusted)) throw trustError("IOS_PACKAGE_TRUST_FAILED", "trustedSwiftMacros/trustedSwiftPlugins are set but the Package.resolved pins were not verified; refusing to skip macro or plugin validation");
  return [...(macros ? ["-skipMacroValidation"] : []), ...(plugins ? ["-skipPackagePluginValidation"] : []), "-disableAutomaticPackageResolution", "-onlyUsePackageVersionsFromResolvedFile"];
}

/**
 * Before any effect: the pinned file must exist, be tracked by git and equal HEAD, parse, and hold every listed package at
 * exactly the reviewed revision. Returns { sourceRel, effectiveRel, bytes, sha256, evidence }, or null when no list is configured.
 */
export function verifySwiftTrustSource({ spec, root, sh, fs = nodeFs }) {
  if (!spec.trustedSwiftMacros?.length && !spec.trustedSwiftPlugins?.length) return null;
  const effectiveRel = defaultPackageResolvedPath(spec.project);
  const sourceRel = spec.packageResolved ?? effectiveRel;
  const sourceAbs = path.resolve(root, sourceRel);
  if (!fs.existsSync(sourceAbs)) throw trustError("IOS_PACKAGE_TRUST_FAILED", `${sourceRel} does not exist; trustedSwiftMacros/trustedSwiftPlugins need a committed Package.resolved${spec.packageResolved ? "" : " inside the xcodeproj (or name one with iosPackage.packageResolved)"}`);
  const tracked = sh("git", ["ls-files", "--error-unmatch", "--", sourceRel], { timeout: 60_000 });
  if (tracked.error || tracked.status !== 0) throw trustError("IOS_PACKAGE_TRUST_FAILED", `${sourceRel} is not tracked by git; the pins that unlock macro trust must be committed`);
  const clean = sh("git", ["diff", "--quiet", "HEAD", "--", sourceRel], { timeout: 60_000 });
  if (clean.error || clean.status !== 0) throw trustError("IOS_PACKAGE_TRUST_FAILED", `${sourceRel} differs from HEAD; commit the reviewed pins first`);
  const bytes = Buffer.from(fs.readFileSync(sourceAbs));
  const evidence = assertTrustedPins(parsePackageResolved(bytes.toString("utf8")), spec);
  return { sourceRel, effectiveRel, bytes, sha256: sha256Bytes(bytes), evidence };
}

/** The file Xcode reads must be exactly the verified bytes (checked before and after the archive). */
export function assertSwiftTrustUnchanged({ trust, root, fs = nodeFs }) {
  let current;
  try { current = fs.readFileSync(path.resolve(root, trust.effectiveRel)); } catch { throw trustError("IOS_PACKAGE_TRUST_FAILED", `${trust.effectiveRel} is missing; Xcode would read other pins than the verified ones`); }
  if (sha256Bytes(current) !== trust.sha256) throw trustError("IOS_PACKAGE_TRUST_FAILED", `${trust.effectiveRel} differs from the verified ${trust.sourceRel} (sha256 changed)`);
}

/**
 * After generate and prepare: make the committed file the one Xcode reads. When it lives elsewhere it is copied into the
 * project's swiftpm location; a different file already there is refused, never overwritten.
 */
export function installSwiftTrustSource({ trust, root, fs = nodeFs }) {
  if (trust.sourceRel !== trust.effectiveRel) {
    const effectiveAbs = path.resolve(root, trust.effectiveRel);
    if (fs.existsSync(effectiveAbs)) {
      if (!Buffer.from(fs.readFileSync(effectiveAbs)).equals(trust.bytes)) throw trustError("IOS_PACKAGE_TRUST_FAILED", `${trust.effectiveRel} already exists and differs from ${trust.sourceRel}; refusing to overwrite it`);
    } else {
      fs.mkdirSync(path.dirname(effectiveAbs), { recursive: true });
      fs.writeFileSync(effectiveAbs, trust.bytes);
    }
  }
  assertSwiftTrustUnchanged({ trust, root, fs });
}

export function archiveArgv(spec, { root, buildNumber, storage, trusted }) {
  const trustFlags = swiftTrustFlags(spec, trusted);
  return [
    "-project", path.resolve(root, spec.project),
    "-scheme", spec.scheme,
    "-configuration", spec.configuration,
    "-destination", "generic/platform=iOS",
    "-archivePath", path.resolve(root, spec.archive),
    "-derivedDataPath", storage.derivedData,
    ...(storage.resultBundle ? ["-resultBundlePath", storage.resultBundle] : []),
    `DEVELOPMENT_TEAM=${spec.teamId}`,
    `CURRENT_PROJECT_VERSION=${buildNumber}`,
    ...Object.entries(spec.extraBuildSettings).map(([name, value]) => `${name}=${value}`),
    "-allowProvisioningUpdates",
    ...trustFlags,
    "archive",
  ];
}

/** A dedicated directory beside the IPA (never the IPA's own directory, which may be the app root): xcodebuild fills it with logs and plists. */
export const exportDirectory = (spec, root) => path.join(path.dirname(path.resolve(root, spec.ipa)), `${path.basename(spec.ipa, ".ipa")}.export`);

export function exportArgv(spec, { root, optionsFile }) {
  return ["-exportArchive", "-archivePath", path.resolve(root, spec.archive), "-exportPath", exportDirectory(spec, root), "-exportOptionsPlist", optionsFile, "-allowProvisioningUpdates"];
}

/** Dry-run display only: the flags the run would add once its own check passes. The real run never uses this. */
const plannedTrust = (spec) => {
  if (!spec.trustedSwiftMacros.length && !spec.trustedSwiftPlugins.length) return undefined;
  const planned = { macros: [], plugins: [] };
  VERIFIED_TRUST.add(planned);
  return planned;
};

/** The ordered plan (pure; no secret is part of any argument). Used by --dry-run and by the run itself. */
export function iosPackagePlan(spec, { root, buildNumber = "<YYYYMMDDNN>", storage = { derivedData: "<rightkit storage xcode-args -derivedDataPath>" }, receiptDir }) {
  const optionsFile = path.join(receiptDir, "ios-export-options.plist");
  return {
    steps: [
      { id: "preflight", description: "macOS host, GUI signing session, xcodebuild" + (spec.projectSpec ? ", xcodegen (required)" : "") },
      ...(spec.projectSpec ? [{ id: "generate", cmd: "xcodegen", args: xcodegenArgv(spec, root) }] : []),
      ...spec.prepare.map((step, index) => ({ id: "prepare", index, cmd: step.cmd, args: step.args, cwd: step.cwd ?? "." })),
      ...(spec.trustedSwiftMacros.length || spec.trustedSwiftPlugins.length
        ? [{ id: "trust-check", description: `Package.resolved (${spec.packageResolved ?? defaultPackageResolvedPath(spec.project)}) tracked and equal to HEAD; ${[...spec.trustedSwiftMacros, ...spec.trustedSwiftPlugins].map((entry) => entry.package).join(", ")} pinned to the reviewed revisions; the skip flags below are added only after this passes` }]
        : []),
      { id: "archive", cmd: "xcodebuild", args: archiveArgv(spec, { root, buildNumber, storage, trusted: plannedTrust(spec) }) },
      { id: "verify-archive", description: "version identity across app, appex and Watch plists; bundle id; build number; QA scan; forbidden strings" },
      { id: "export", cmd: "xcodebuild", args: exportArgv(spec, { root, optionsFile }), optionsFile },
      { id: "verify-ipa", description: "extracted IPA: version identity, entitlements, embedded.mobileprovision, code signature" },
      { id: "receipt", file: path.join(receiptDir, IOS_RECEIPT_NAME) },
    ],
    teamId: spec.teamId,
    bundleId: spec.bundleId,
    ipa: path.resolve(root, spec.ipa),
    archive: path.resolve(root, spec.archive),
  };
}

// ---- verification helpers ----------------------------------------------------------------------------------------
const sha256Bytes = (bytes) => createHash("sha256").update(bytes).digest("hex");

export function iosSealEntry(receipt) {
  return { role: "ipa", name: receipt.file.name, sha256: receipt.file.sha256, sizeBytes: receipt.file.sizeBytes };
}

/** Walk a bundle for the plists HeardRight checks: the app, PlugIns/*.appex, Watch/*.app and Watch/*.app/PlugIns/*.appex. */
export function bundlePlistPaths(appDir, fs = nodeFs) {
  const list = (dir, suffix) => {
    try { return fs.readdirSync(dir).filter((name) => name.endsWith(suffix)).sort().map((name) => path.join(dir, name)); } catch { return []; }
  };
  const bundles = [{ role: "app", dir: appDir }];
  for (const appex of list(path.join(appDir, "PlugIns"), ".appex")) bundles.push({ role: "appex", dir: appex });
  for (const watch of list(path.join(appDir, "Watch"), ".app")) {
    bundles.push({ role: "watch", dir: watch });
    for (const appex of list(path.join(watch, "PlugIns"), ".appex")) bundles.push({ role: "watch-appex", dir: appex });
  }
  return bundles.map((bundle) => ({ ...bundle, plist: path.join(bundle.dir, "Info.plist") }));
}

/** Pure: throws IosPackageError unless every bundle shares the main app's marketing version and build, with no `$(...)`. */
export function assertVersionIdentity(infos, { expectedBuild } = {}) {
  const [main] = infos;
  const marketing = String(main?.info?.CFBundleShortVersionString ?? "");
  const build = String(main?.info?.CFBundleVersion ?? "");
  const fail = (message) => new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", message);
  if (!marketing || !build || marketing.includes("$") || build.includes("$")) throw fail(`the archive has an unresolved version identity: ${marketing || "(none)"} (${build || "(none)"})`);
  for (const { role, label, info } of infos.slice(1)) {
    const m = String(info?.CFBundleShortVersionString ?? ""); const b = String(info?.CFBundleVersion ?? "");
    if (m !== marketing || b !== build) throw fail(`version drift: ${role} ${label} has ${m || "(none)"} (${b || "(none)"}), the app has ${marketing} (${build})`);
  }
  if (expectedBuild !== undefined && build !== expectedBuild) throw fail(`the app build number is ${build}, not the recipe's ${expectedBuild}: an Info.plist literal (not $(CURRENT_PROJECT_VERSION)) overrides it`);
  return { marketingVersion: marketing, buildNumber: build };
}

/** Pure: entitlements of one signed bundle in the distribution IPA. */
export function assertDistributionEntitlements(entitlements, label) {
  const fail = (message) => new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", message);
  if (!isPlainObject(entitlements)) throw fail(`${label} has no readable entitlements`);
  if (entitlements["get-task-allow"] === true) throw fail(`${label} carries get-task-allow: a debuggable build cannot be a release`);
  const aps = entitlements["aps-environment"];
  if (aps !== undefined && aps !== "production") throw fail(`${label} has aps-environment ${JSON.stringify(aps)}; a release needs production`);
}

/**
 * Pure: XML plist text that `plutil -convert json` accepts. JSON has no date or data type, so a provisioning profile (its
 * ExpirationDate/CreationDate and DeveloperCertificates) is refused as-is: dates become their ISO strings, data blobs empty
 * strings. Only used to read values the checks compare; never written back.
 */
export function jsonSafePlistText(text) {
  return String(text ?? "")
    .replace(/<date>([^<]*)<\/date>/g, "<string>$1</string>")
    .replace(/<data>[^<]*<\/data>/g, "<string></string>")
    .replace(/<data\/>/g, "<string></string>");
}

/** Pure: the distribution provisioning profile embedded in the main app. */
export function assertDistributionProfile(profile, { teamId, bundleId }) {
  const fail = (message) => new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", message);
  if (!isPlainObject(profile)) throw fail("embedded.mobileprovision is missing or unreadable");
  if (!Array.isArray(profile.TeamIdentifier) || !profile.TeamIdentifier.includes(teamId)) throw fail(`the embedded profile is not for team ${teamId}`);
  const appId = profile.Entitlements?.["application-identifier"];
  if (appId !== `${teamId}.${bundleId}`) throw fail(`the embedded profile authorizes ${String(appId)}, not ${teamId}.${bundleId}`);
  if (profile.ProvisionedDevices !== undefined || profile.ProvisionsAllDevices === true) throw fail("the embedded profile is a development, ad hoc or enterprise profile; an App Store build carries a distribution profile");
  if (profile.Entitlements?.["get-task-allow"] === true) throw fail("the embedded profile allows get-task-allow");
  return { teamIdentifier: teamId, applicationIdentifier: appId, name: String(profile.Name ?? ""), uuid: String(profile.UUID ?? ""), expiration: profile.ExpirationDate ? String(profile.ExpirationDate) : null };
}

/** For the sealing step: the receipt must describe exactly the IPA that is about to be sealed. */
export function verifyIosReceipt(receiptPath, ipaFile, { fs = nodeFs } = {}) {
  const receipt = JSON.parse(fs.readFileSync(receiptPath, "utf8"));
  if (receipt.schema !== 1 || receipt.contract !== IOS_SIGNING_CONTRACT) throw new Error("not an iOS IPA receipt");
  if (path.basename(ipaFile) !== receipt.file?.name) throw new Error("IPA receipt names a different file");
  const bytes = fs.readFileSync(ipaFile);
  if (bytes.length !== receipt.file.sizeBytes || sha256Bytes(bytes) !== receipt.file.sha256) throw new Error("IPA bytes differ from the build receipt");
  return receipt;
}

export function scanForbiddenStrings(dir, strings, fs = nodeFs) {
  const findings = [];
  const walk = (current) => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const file = path.join(current, entry.name);
      if (entry.isDirectory()) walk(file);
      else if (entry.isFile()) {
        const bytes = fs.readFileSync(file);
        for (const text of strings) if (bytes.includes(text)) findings.push({ file, string: text });
      }
    }
  };
  walk(dir);
  return findings;
}

// ---- the recipe --------------------------------------------------------------------------------------------------
/**
 * Runs the steps. `root` is the app root. `deps` (all optional, defaults are the real host):
 *   run(cmd, args, options)      spawnSync-shaped; fakes in tests
 *   fs                           node:fs-shaped
 *   scanFiles(label, files, o)   release.mjs's assertNoQaControlBinaries (QA rules plus the release-switch binary layer)
 *   detectSession / route        GUI signing session seams (ios-host.mjs)
 *   gitHead()                    the source commit; it must not change during the build
 *   storage()                    DerivedData and result bundle (rightkit storage xcode-args)
 *   scanForbidden(dir, strings)  forbidden-string scan
 *   counterDir                   where the per-app build number counter lives (the vault state root under right-release build)
 */
export async function runIosPackageRecipe({
  root, app = "app", spec, dryRun = false, env = process.env, platform = process.platform,
  receiptDir = path.join(root, ".right-release", "receipts"), stateDir = path.join(root, ".right-release"), counterDir = stateDir,
  run = spawnSync, fs = nodeFs, scanFiles, detectSession, route, gitHead, storage, scanForbidden = scanForbiddenStrings,
  now = () => new Date(), tmpRoot = os.tmpdir(), log = console.log,
}) {
  if (!path.isAbsolute(root)) throw invalid("root must be an absolute path");
  const ipa = path.resolve(root, spec.ipa);
  const archive = path.resolve(root, spec.archive);
  const receiptPath = path.join(receiptDir, IOS_RECEIPT_NAME);

  if (dryRun) {
    const plan = iosPackagePlan(spec, { root, receiptDir });
    log(`right-release: dry-run: ios ${spec.scheme} ${spec.configuration} -> ${ipa}; ${plan.steps.length} steps (nothing is run, no credential is read)`);
    for (const step of plan.steps) log(`  ${step.id}${step.cmd ? `: ${step.cmd} ${step.args.join(" ")}` : step.description ? `: ${step.description}` : ""}`);
    return { dryRun: true, plan, ipa };
  }
  if (platform !== "darwin") throw new IosPackageError("IOS_PACKAGE_HOST_UNSUPPORTED", `the iOS package recipe needs macOS with Xcode; this host is ${platform} (only config validation and --dry-run run elsewhere)`);

  // ---- preflight: before any effect ----
  let session;
  try { session = assertGuiSigningSession({ operation: "ios package", env, run, detect: detectSession, route }); }
  catch (error) { if (error instanceof IosHostError) throw new IosPackageError("IOS_PACKAGE_HOST_UNSUPPORTED", error.message, { cause: error }); throw error; }
  const xcRun = session.routed ? session.route : run;
  const sh = (cmd, args, options = {}) => xcRun(cmd, args, { cwd: root, env, encoding: "utf8", windowsHide: true, shell: false, maxBuffer: 512 * 1024 * 1024, ...options });
  const xcode = sh("xcodebuild", ["-version"], { timeout: 60_000 });
  if (xcode.error || xcode.status !== 0) throw new IosPackageError("IOS_PACKAGE_TOOL_MISSING", "xcodebuild is not available (install Xcode and select it with xcode-select)");
  const xcodebuildVersion = String(xcode.stdout ?? "").trim().split(/\r?\n/).join(" ");
  if (spec.projectSpec) {
    const gen = sh("xcodegen", ["--version"], { timeout: 60_000 });
    if (gen.error || gen.status !== 0) throw new IosPackageError("IOS_PACKAGE_TOOL_MISSING", `xcodegen is required because iosPackage.projectSpec names ${spec.projectSpec}; install it (brew install xcodegen). A committed xcodeproj is never used instead`);
  }
  if (!fs.existsSync(path.resolve(root, spec.projectSpec ?? spec.project))) throw new IosPackageError("IOS_PACKAGE_CONFIG_INVALID", `${spec.projectSpec ?? spec.project} does not exist`);
  const resolvedStorage = (storage ?? (() => resolveXcodeStorage({ env, run, cwd: root, fallbackRoot: path.join(stateDir, "ios") })))();
  const head = gitHead ?? (() => { const r = run("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8", windowsHide: true }); if (r.status !== 0) throw new IosPackageError("IOS_PACKAGE_SOURCE_CHANGED", "git rev-parse HEAD failed"); return String(r.stdout).trim(); });
  const sourceCommit = head();

  // ---- stale outputs go first, so an old IPA is never mistaken for this build's ----
  const exportDir = exportDirectory(spec, root);
  const stale = [archive, ipa, exportDir, receiptPath, path.join(receiptDir, "ios-export-options.plist"), ...(resolvedStorage.resultBundle ? [resolvedStorage.resultBundle] : []), ...(spec.cleanup.derivedData ? [resolvedStorage.derivedData] : [])];
  for (const file of stale) fs.rmSync(file, { recursive: true, force: true });
  fs.mkdirSync(receiptDir, { recursive: true });
  fs.mkdirSync(path.dirname(ipa), { recursive: true });

  const cleanupOutputs = () => { for (const file of [ipa, archive, exportDir, receiptPath]) { try { fs.rmSync(file, { recursive: true, force: true }); } catch { /* best effort */ } } };
  const step = (id, code, cmd, args, options = {}) => {
    log(`right-release: ios ${id}: ${cmd}`);
    const { logFile, ...runOptions } = options;
    const result = sh(cmd, args, { timeout: spec.timeoutMs, ...runOptions });
    if (logFile) { try { fs.writeFileSync(logFile, `${outputTail(result, 4000)}\n`); } catch { /* diagnostics only */ } }
    if (result.error || result.status !== 0) throw new IosPackageError(code, `${cmd} ${id} failed (${result.status ?? "unavailable"})${result.error ? `: ${result.error.message}` : ""}\n${outputTail(result)}`);
    return result;
  };
  const plist = (file) => {
    const result = sh("plutil", ["-convert", "json", "-o", "-", "--", file], { timeout: 60_000 });
    try { if (result.status === 0) return JSON.parse(String(result.stdout)); } catch { /* reported below */ }
    throw new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", `cannot read ${path.basename(path.dirname(file))}/${path.basename(file)} with plutil`);
  };
  const plistFromText = (id, command, args, { emptyIsNone = false } = {}) => {
    const first = sh(command, args, { timeout: 60_000 });
    // A signed bundle with no entitlements prints nothing: that is an empty set, not a failure.
    if (emptyIsNone && first.status === 0 && !String(first.stdout ?? "").trim()) return {};
    if (first.status !== 0 || !String(first.stdout ?? "").includes("<")) throw new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", `${id} could not be read`);
    const second = sh("plutil", ["-convert", "json", "-o", "-", "-"], { input: jsonSafePlistText(first.stdout), timeout: 60_000 });
    try { if (second.status === 0) return JSON.parse(String(second.stdout)); } catch { /* reported below */ }
    throw new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", `${id} is not a readable property list`);
  };
  const identityOf = (appDir) => bundlePlistPaths(appDir, fs).map((bundle) => {
    if (!fs.existsSync(bundle.plist)) throw new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", `${bundle.role} ${path.basename(bundle.dir)} has no Info.plist`);
    return { ...bundle, label: path.basename(bundle.dir), info: plist(bundle.plist) };
  });

  let tempDir;
  try {
    // ---- trust-check: reviewed Swift macro/plugin pins, before anything is generated or built ----
    const trust = verifySwiftTrustSource({ spec, root, sh, fs });
    // ---- generate ----
    if (spec.projectSpec) {
      step("generate", "IOS_PACKAGE_GENERATE_FAILED", "xcodegen", xcodegenArgv(spec, root));
      if (!fs.existsSync(path.resolve(root, spec.project))) throw new IosPackageError("IOS_PACKAGE_GENERATE_FAILED", `xcodegen did not produce ${spec.project}`);
    }
    // ---- prepare ----
    for (const [index, command] of spec.prepare.entries()) {
      const cwd = path.resolve(root, command.cwd ?? ".");
      step(`prepare ${index + 1}`, "IOS_PACKAGE_PREPARE_FAILED", command.cmd, command.args, { cwd });
    }
    if (trust) installSwiftTrustSource({ trust, root, fs });
    // ---- archive ----
    const buildNumber = nextDateBuildNumber({ now: now(), stateFile: path.join(counterDir, `ios-build-number.${spec.bundleId}.json`), fs });
    step("archive", "IOS_PACKAGE_ARCHIVE_FAILED", "xcodebuild", archiveArgv(spec, { root, buildNumber, storage: resolvedStorage, trusted: trust?.evidence }), { logFile: path.join(receiptDir, "ios-archive.log") });
    if (trust) assertSwiftTrustUnchanged({ trust, root, fs });
    const appsDir = path.join(archive, "Products", "Applications");
    const apps = fs.existsSync(appsDir) ? fs.readdirSync(appsDir).filter((name) => name.endsWith(".app")) : [];
    if (apps.length !== 1) throw new IosPackageError("IOS_PACKAGE_ARCHIVE_FAILED", `the archive must hold exactly one app under Products/Applications, found ${apps.length}`);
    const archivedApp = path.join(appsDir, apps[0]);

    // ---- verify the archive ----
    const archiveInfos = identityOf(archivedApp);
    const identity = assertVersionIdentity(archiveInfos, { expectedBuild: buildNumber });
    const mainInfo = archiveInfos[0].info;
    if (mainInfo.CFBundleIdentifier !== spec.bundleId) throw new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", `the archived app bundle id is ${mainInfo.CFBundleIdentifier}, not iosPackage.bundleId ${spec.bundleId}`);
    for (const [key, expected] of Object.entries(spec.checks.plist)) {
      if (mainInfo[key] !== expected) throw new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", `Info.plist ${key} is ${JSON.stringify(mainInfo[key])}, expected ${JSON.stringify(expected)}`);
    }
    scanFiles?.("ios archive app", [archivedApp], { requireBinary: true, prePack: true });
    if (spec.checks.forbidStrings.length) {
      const found = scanForbidden(archivedApp, spec.checks.forbidStrings);
      if (found.length) throw new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", `the archive contains forbidden string(s): ${[...new Set(found.map((item) => item.string))].join(", ")} (first file: ${path.relative(archive, found[0].file)})`);
    }
    if (head() !== sourceCommit) throw new IosPackageError("IOS_PACKAGE_SOURCE_CHANGED", "the repository HEAD changed during the archive; refusing to export a mixed-source build");

    // ---- export ----
    const optionsFile = path.join(receiptDir, "ios-export-options.plist");
    const optionsText = exportOptionsPlist(spec);
    fs.writeFileSync(optionsFile, optionsText);
    // Taken just before the export: xcodebuild registers a build upload for this version and build during it (see ios-build-upload.mjs).
    const exportStartedAt = now().toISOString();
    step("export", "IOS_PACKAGE_EXPORT_FAILED", "xcodebuild", exportArgv(spec, { root, optionsFile }), { logFile: path.join(receiptDir, "ios-export.log") });
    {
      const produced = fs.existsSync(exportDir) ? fs.readdirSync(exportDir).filter((name) => name.endsWith(".ipa")) : [];
      if (produced.length !== 1) throw new IosPackageError("IOS_PACKAGE_EXPORT_FAILED", `the export must produce exactly one .ipa in ${path.basename(exportDir)}, found ${produced.length}`);
      fs.mkdirSync(path.dirname(ipa), { recursive: true });
      fs.renameSync(path.join(exportDir, produced[0]), ipa);
    }
    if (!fs.statSync(ipa).size) throw new IosPackageError("IOS_PACKAGE_EXPORT_FAILED", "the exported IPA is empty");

    // ---- verify the IPA: the bytes that will ship ----
    tempDir = fs.mkdtempSync(path.join(tmpRoot, "rightkit-ios-ipa-"));
    step("unzip ipa", "IOS_PACKAGE_VERIFY_FAILED", "ditto", ["-x", "-k", ipa, tempDir], { timeout: 10 * 60_000 });
    const payload = path.join(tempDir, "Payload");
    const shipped = fs.existsSync(payload) ? fs.readdirSync(payload).filter((name) => name.endsWith(".app")) : [];
    if (shipped.length !== 1) throw new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", `the IPA must hold exactly one app under Payload, found ${shipped.length}`);
    const shippedApp = path.join(payload, shipped[0]);
    const ipaInfos = identityOf(shippedApp);
    const ipaIdentity = assertVersionIdentity(ipaInfos, { expectedBuild: buildNumber });
    if (ipaInfos[0].info.CFBundleIdentifier !== spec.bundleId) throw new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", `the IPA app bundle id is ${ipaInfos[0].info.CFBundleIdentifier}, not ${spec.bundleId}`);
    let mainEntitlements;
    for (const bundle of ipaInfos) {
      const verify = sh("codesign", ["--verify", "--strict", bundle.dir], { timeout: 5 * 60_000 });
      if (verify.error || verify.status !== 0) throw new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", `${bundle.label} fails codesign --verify --strict\n${outputTail(verify, 10)}`);
      const entitlements = plistFromText(`${bundle.label} entitlements`, "codesign", ["-d", "--entitlements", ":-", bundle.dir], { emptyIsNone: true });
      assertDistributionEntitlements(entitlements, `${bundle.role} ${bundle.label}`);
      if (bundle.role === "app") mainEntitlements = entitlements;
    }
    // Every bundle that carries its own profile (the app, each appex, the Watch app and its appex) must hold a distribution profile for
    // the configured team and its own bundle id; a missing profile fails closed.
    let profile;
    for (const bundle of ipaInfos) {
      const profileFile = path.join(bundle.dir, "embedded.mobileprovision");
      if (!fs.existsSync(profileFile)) throw new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", `${bundle.role} ${bundle.label} has no embedded.mobileprovision`);
      const summary = assertDistributionProfile(plistFromText(`${bundle.label} embedded.mobileprovision`, "security", ["cms", "-D", "-i", profileFile]), { teamId: spec.teamId, bundleId: String(bundle.info.CFBundleIdentifier ?? "") });
      if (bundle.role === "app") profile = summary;
    }
    scanFiles?.("ios IPA app", [shippedApp], { requireBinary: true, prePack: true });
    if (spec.checks.forbidStrings.length) {
      const found = scanForbidden(shippedApp, spec.checks.forbidStrings);
      if (found.length) throw new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", `the IPA contains forbidden string(s): ${[...new Set(found.map((item) => item.string))].join(", ")}`);
    }
    if (head() !== sourceCommit) throw new IosPackageError("IOS_PACKAGE_SOURCE_CHANGED", "the repository HEAD changed during the export");

    // ---- receipt over the FINAL bytes ----
    const bytes = fs.readFileSync(ipa);
    const receipt = {
      schema: 1,
      contract: IOS_SIGNING_CONTRACT,
      app,
      builtAt: now().toISOString(),
      exportStartedAt,
      sourceCommit,
      xcodebuild: xcodebuildVersion,
      scheme: spec.scheme,
      configuration: spec.configuration,
      teamId: spec.teamId,
      bundleId: spec.bundleId,
      marketingVersion: ipaIdentity.marketingVersion,
      buildNumber: ipaIdentity.buildNumber,
      file: { name: path.basename(ipa), sha256: sha256Bytes(bytes), sizeBytes: bytes.length },
      exportOptionsSha256: sha256Bytes(optionsText),
      entitlementsSha256: sha256Bytes(JSON.stringify(Object.fromEntries(Object.entries(mainEntitlements).sort(([a], [b]) => (a < b ? -1 : 1))))),
      profile,
      bundles: ipaInfos.map((bundle) => ({ role: bundle.role, name: bundle.label, bundleId: String(bundle.info.CFBundleIdentifier ?? "") })),
      archiveVersion: identity,
      // Present only when the app trusts reviewed Swift macros/plugins: what was trusted and the exact pins file it was checked against.
      ...(trust ? { swiftTrust: { packageResolved: { path: trust.sourceRel, sha256: trust.sha256 }, macros: trust.evidence.macros, plugins: trust.evidence.plugins } } : {}),
    };
    try { fs.writeFileSync(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`); }
    catch (error) { throw new IosPackageError("IOS_PACKAGE_RECEIPT_FAILED", `cannot write the iOS receipt: ${error.message}`); }
    log(`right-release: ios IPA ${receipt.file.name} ${receipt.marketingVersion} (${receipt.buildNumber}) sha256 ${receipt.file.sha256}; receipt ${receiptPath}`);
    return { dryRun: false, ipa, receipt, receiptPath, sealEntry: iosSealEntry(receipt) };
  } catch (error) {
    cleanupOutputs();
    throw error instanceof IosPackageError ? error : new IosPackageError("IOS_PACKAGE_VERIFY_FAILED", `iOS package failed: ${error.message}`, { cause: error });
  } finally {
    try { fs.rmSync(exportDir, { recursive: true, force: true }); } catch { /* best effort */ }
    if (tempDir) { try { fs.rmSync(tempDir, { recursive: true, force: true }); } catch { /* best effort */ } }
  }
}
