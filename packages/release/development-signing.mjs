import { createHash, X509Certificate } from "node:crypto";
import { nativeHelperEntitlementMap, nativeHelpersFromConfig } from "./native-helpers.mjs";
import { closeSync, copyFileSync, existsSync, mkdtempSync, openSync, readFileSync, readSync, readdirSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { adhocSafeEntitlements } from "./adhoc-entitlements.mjs";
import { runMacCodesignCommands } from "./macos-signing-session.mjs";
import { bundleRelativePath, compileBundlePathMap, globToRegExp, isGlobPattern, lookupBundlePath, resolveBundleIdentifier, validateBundleRelativeKey, validateIdentifierTemplate, validateSigningIdentifier } from "./mac-bundle-signing.mjs";

export class MacosDevelopmentSigningError extends Error {
  constructor(code, message, details = {}) {
    super(message, details.cause ? { cause: details.cause } : undefined);
    this.name = "MacosDevelopmentSigningError";
    this.code = code;
    Object.assign(this, details);
  }
}

const DEFAULT_TEAM = "6KLGD3LLKF";
const output = (result) => `${result.stdout ?? ""}\n${result.stderr ?? ""}`.trim();
const captureOptions = (root, env) => ({ cwd: root, env, encoding: "utf8", stdio: ["inherit", "pipe", "pipe"], windowsHide: true, shell: false });

function checked(command, args, context, code, identity) {
  const result = context.run(command, args, captureOptions(context.root, context.env));
  if (result.error || result.status !== 0) {
    const diagnostic = result.error?.message || output(result) || `exit ${result.status ?? "unavailable"}${result.signal ? ` (${result.signal})` : ""}`;
    const interaction = result.status === 36 || /errSecInteractionNotAllowed|User interaction is not allowed/i.test(diagnostic);
    throw new MacosDevelopmentSigningError(interaction ? "DEV_MAC_SIGNING_KEY_UNAVAILABLE" : code,
      `${identity ? `Signing identity ${JSON.stringify(identity)}: ` : ""}${command} failed (${result.status ?? "unavailable"}): ${diagnostic}. ${interaction
        ? "Run right-release dev from Terminal in a logged-in macOS GUI session where this identity's private key is accessible; SSH cannot authorize Keychain interaction."
        : "Check the configured identity, profile and entitlements; confirm this identity and its private key are available in Keychain Access in a logged-in GUI session."}`,
      { identity, status: result.status, diagnostic, cause: result.error });
  }
  return result;
}

function readPlist(file, context) {
  const result = checked("plutil", ["-convert", "json", "-o", "-", file], context, "DEV_MAC_SIGNING_CONFIG_INVALID");
  try { return JSON.parse(result.stdout); }
  catch { throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_CONFIG_INVALID", `Invalid plist JSON: ${file}`); }
}

/** CFBundleExecutable of a bundle, read through the same plutil path (and injectable runner) as every other plist here. */
export function readBundleExecutable(bundle, { root, env = process.env, run = spawnSync }) {
  const file = path.join(bundle, "Contents", "Info.plist");
  const name = readPlist(file, { root, env, run }).CFBundleExecutable;
  if (typeof name !== "string" || !name.trim() || /[\\/]/.test(name)) throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_CONFIG_INVALID", `Info.plist has no usable CFBundleExecutable: ${file}`);
  return name.trim();
}

function decodeProfile(file, context) {
  const cms = checked("security", ["cms", "-D", "-i", file], context, "DEV_MAC_SIGNING_PROFILE_INVALID");
  // plutil JSON conversion rejects the date/data values in provisioning profiles.
  const parser = "import base64,datetime,json,plistlib,sys\ndef encode(v):\n if isinstance(v,bytes): return base64.b64encode(v).decode('ascii')\n if isinstance(v,(datetime.date,datetime.datetime)): return v.isoformat()\n raise TypeError(type(v).__name__)\nprint(json.dumps(plistlib.loads(sys.stdin.buffer.read()),default=encode))";
  const parsed = context.run("python3", ["-c", parser], { ...captureOptions(context.root, context.env), stdio: "pipe", input: cms.stdout });
  try {
    if (parsed.error || parsed.status !== 0) throw new Error(parsed.error?.message || output(parsed));
    const profile = JSON.parse(parsed.stdout);
    if (!Array.isArray(profile.TeamIdentifier) || profile.TeamIdentifier.length !== 1 || !profile.TeamIdentifier[0]) throw new Error("profile must name one team");
    if (!profile.Platform?.includes("OSX")) throw new Error("profile must target OSX");
    if (!(Date.parse(profile.ExpirationDate) > Date.now())) throw new Error("profile is expired or has no expiration date");
    const certificates = (profile.DeveloperCertificates ?? []).map((data) => {
      const cert = new X509Certificate(Buffer.from(data, "base64"));
      return { name: /^CN=(.+)$/m.exec(cert.subject)?.[1], sha1: createHash("sha1").update(cert.raw).digest("hex").toUpperCase() };
    });
    if (!certificates.length) throw new Error("profile has no signing certificates");
    return { profile, certificates, team: profile.TeamIdentifier[0] };
  } catch (cause) {
    throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_PROFILE_INVALID", `Cannot use provisioning profile ${file}: ${cause.message}. Configure a valid macOS profile authorizing this app and signing certificate.`, { cause });
  }
}

/** The only supported value of `install.signing` / RIGHTKIT_DEV_SIGNING: ad hoc signing on an ephemeral GitHub-hosted runner. */
export const CI_ADHOC_SIGNING = "ci-adhoc";
/** Environment spelling of the same opt-in (the generated rust-hybrid candidate and dev-qualification jobs set it). */
export const CI_ADHOC_SIGNING_ENV = "RIGHTKIT_DEV_SIGNING";

/**
 * Explicit opt-in to CI-only ad hoc signing: `development.targets.mac.install.signing: "ci-adhoc"` or
 * RIGHTKIT_DEV_SIGNING=ci-adhoc. Never inferred from a missing identity. Any other value is refused so a typo cannot silently
 * fall back to the default; the opt-in is refused on every host that is not a GitHub-hosted Actions runner.
 */
export function requestedCiAdhocSigning({ descriptor = {}, env = process.env }) {
  const fromConfig = descriptor.signing;
  const fromEnv = typeof env[CI_ADHOC_SIGNING_ENV] === "string" ? env[CI_ADHOC_SIGNING_ENV].trim() : "";
  for (const [value, source] of [[fromConfig, "install.signing"], [fromEnv || undefined, CI_ADHOC_SIGNING_ENV]]) {
    if (value !== undefined && value !== CI_ADHOC_SIGNING) throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_MODE_INVALID", `${source} must be ${JSON.stringify(CI_ADHOC_SIGNING)} or unset, got ${JSON.stringify(value)}.`);
  }
  if (fromConfig === undefined && !fromEnv) return false;
  const onRunner = env.GITHUB_ACTIONS === "true" && env.CI === "true" && env.RUNNER_ENVIRONMENT === "github-hosted";
  if (!onRunner) throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_CI_ADHOC_REFUSED", `Ad hoc development signing (${CI_ADHOC_SIGNING}) is allowed only on a GitHub-hosted Actions runner (GITHUB_ACTIONS=true, CI=true, RUNNER_ENVIRONMENT=github-hosted). Ad hoc signatures change on every build, so on a developer machine they re-trigger macOS TCC prompts; use a Keychain Developer ID identity there.`);
  return true;
}

export function resolveMacosDevelopmentSigning({ root, bundle, descriptor = {}, config = {}, env = process.env, run = spawnSync }) {
  const context = { root, env, run };
  const ciAdhoc = requestedCiAdhocSigning({ descriptor, env });
  const tauriRoot = path.dirname(path.resolve(root, descriptor.manifestPath ?? "src-tauri/Cargo.toml"));
  let mac = {};
  for (const name of ["tauri.conf.json", "tauri.macos.conf.json"]) {
    const file = path.join(tauriRoot, name);
    if (existsSync(file)) mac = { ...mac, ...JSON.parse(readFileSync(file, "utf8")).bundle?.macOS };
  }
  const sign = config.targets?.mac?.sign ?? {};
  // `sign.entitlements` is the app-level file when a string; an object is the per-path map (readDevSigningSettings).
  const entitlements = typeof sign.entitlements === "string" && sign.entitlements ? path.resolve(root, sign.entitlements) : mac.entitlements ? path.resolve(tauriRoot, mac.entitlements) : null;
  if (entitlements && !existsSync(entitlements)) throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_CONFIG_INVALID", `Configured entitlements file is missing: ${entitlements}`);
  // CI-only ad hoc: no Keychain, no provisioning profile (a profile authorizes restricted entitlements that an ad hoc signature cannot carry).
  if (ciAdhoc) return { identity: "-", certificateSha1: "-", teamIdentifier: null, entitlements, provisioningProfile: null, mode: CI_ADHOC_SIGNING, adhoc: true };
  const tauriProfile = mac.provisioningProfile ?? mac.files?.["embedded.provisionprofile"] ?? mac.files?.["/embedded.provisionprofile"];
  const configuredProfile = sign.provisioningProfile ? path.resolve(root, sign.provisioningProfile) : tauriProfile ? path.resolve(tauriRoot, tauriProfile) : null;
  const embedded = path.join(bundle, "Contents", "embedded.provisionprofile");
  // App preparation may already embed an app-specific profile (e.g. HeardRight).
  let provisioningProfile = configuredProfile ?? (existsSync(embedded) ? embedded : null);
  if (provisioningProfile && !existsSync(provisioningProfile)) throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_PROFILE_INVALID", `Configured provisioning profile is missing: ${provisioningProfile}`);
  let profileInfo = provisioningProfile ? decodeProfile(provisioningProfile, context) : null;
  const override = env.APPLE_SIGNING_IDENTITY?.trim() || env.APPLE_DEVELOPER_ID?.trim();
  const releaseTeam = env.APPLE_TEAM_ID?.trim() || DEFAULT_TEAM;
  const configuredIdentity = sign.identity?.trim() || mac.signingIdentity?.trim();
  if (override === "-" || (!profileInfo && configuredIdentity === "-")) throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_IDENTITY_INVALID", 'Signing identity "-" is ad hoc; configure APPLE_SIGNING_IDENTITY or APPLE_DEVELOPER_ID with a real Team identity.', { identity: "-" });
  const requestedIdentity = override || (!profileInfo ? configuredIdentity : undefined);
  const expected = requestedIdentity || profileInfo?.certificates.map((cert) => cert.name).join(" or ") || `Developer ID Application (team ${releaseTeam})`;
  const found = checked("security", ["find-identity", "-v", "-p", "codesigning"], context, "DEV_MAC_SIGNING_IDENTITY_MISSING", expected);
  const identities = [...String(found.stdout ?? "").matchAll(/([A-Fa-f0-9]{40})\s+"([^"\n]+)"/g)].map((match) => ({ sha1: match[1].toUpperCase(), name: match[2] }));
  const matchesRequest = (item, requested) => item.name === requested || item.sha1 === requested?.toUpperCase();
  const profiled = (info) => identities.filter((item) => info.certificates.some((cert) => cert.sha1 === item.sha1) && /^Developer ID Application:/.test(item.name));
  // A configured profile can go stale when its certificate is renewed: prefer a sibling profile for the
  // same app identifier that authorizes a certificate whose private key is in this Keychain.
  if (profileInfo && !profiled(profileInfo).length && configuredProfile) {
    const appId = (info) => info.profile.Entitlements?.["com.apple.application-identifier"] ?? info.profile.Entitlements?.["application-identifier"];
    const siblings = readdirSync(path.dirname(configuredProfile)).filter((name) => name.endsWith(".provisionprofile")).map((name) => path.join(path.dirname(configuredProfile), name)).filter((file) => path.resolve(file) !== path.resolve(configuredProfile));
    const usable = siblings.flatMap((file) => { try { const info = decodeProfile(file, context); return appId(info) === appId(profileInfo) && profiled(info).length ? [{ file, info }] : []; } catch { return []; } })
      .sort((a, b) => Date.parse(b.info.profile.ExpirationDate) - Date.parse(a.info.profile.ExpirationDate));
    if (usable.length) { provisioningProfile = usable[0].file; profileInfo = usable[0].info; }
  }
  let candidates = profileInfo
    ? profiled(profileInfo)
    : identities.filter((item) => item.name.startsWith("Developer ID Application:") && item.name.endsWith(`(${releaseTeam})`));
  if (requestedIdentity) candidates = candidates.filter((item) => matchesRequest(item, requestedIdentity));
  else if (profileInfo && configuredIdentity && candidates.some((item) => matchesRequest(item, configuredIdentity))) candidates = candidates.filter((item) => matchesRequest(item, configuredIdentity));
  // One identity for dev and release (no new id): Developer ID Application only. An Apple Development or Mac Developer certificate gives a different
  // designated requirement, so macOS would drop the app's privacy grants when dev and release alternate. Never a fallback.
  if (!candidates.length) throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED", `Signing identity ${JSON.stringify(expected)}: no Developer ID Application identity with its private key is in Keychain${profileInfo ? ` (and authorized by ${provisioningProfile})` : ""}. Dev builds sign with the same Developer ID Application identity as release. Install that identity${profileInfo ? ", and use a Developer ID provisioning profile (not Apple Development) if the app's entitlements need one" : ""}; there is no fallback to Apple Development or ad hoc signing.`, { identity: expected });
  if (candidates.length !== 1) throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_IDENTITY_MISSING", `Signing identity ${JSON.stringify(expected)}: ${candidates.length ? "multiple matching identities" : "no valid matching identity/private key"} in Keychain${provisioningProfile ? ` for ${provisioningProfile}` : ""}. ${candidates.length ? "Set APPLE_SIGNING_IDENTITY or APPLE_DEVELOPER_ID to one matching identity." : "Confirm the profile-authorized certificate and its private key are available in Keychain Access in a logged-in GUI session."}`, { identity: expected });
  return { identity: candidates[0].name, certificateSha1: candidates[0].sha1, teamIdentifier: profileInfo?.team ?? releaseTeam, entitlements, provisioningProfile };
}

function bundleInfo(directory, context) {
  for (const location of ["Contents/Info.plist", "Resources/Info.plist", "Info.plist"]) {
    const file = path.join(directory, location);
    if (existsSync(file)) return readPlist(file, context);
  }
  return {};
}

function macho(file) {
  const fd = openSync(file, "r");
  try {
    const magic = Buffer.alloc(4);
    return readSync(fd, magic, 0, 4, 0) === 4 && new Set(["feedface", "cefaedfe", "feedfacf", "cffaedfe", "cafebabe", "bebafeca", "cafebabf", "bfbafeca"]).has(magic.toString("hex"));
  } finally { closeSync(fd); }
}

const invalidConfig = (message) => { throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_CONFIG_INVALID", message); };
const ASSERT_KEYS = new Set(["path", "identifier", "teamIdentifier", "authority"]);

/**
 * Declarative per-binary signing settings of a dev install. Everything is optional; with none declared the result is
 * empty and signing is exactly what it was before these keys existed.
 *   install.identifiers / sign.identifiers          { path-or-glob: "--identifier value" }   ("." is the bundle)
 *   install.identifierTemplate / sign.identifierTemplate   default identifier for remaining plain Mach-O files
 *   install.entitlements / sign.entitlements (object)      { path-or-glob: plist path } (a string sign.entitlements is the app file)
 *   install.timestamp                               true adds --timestamp (a secure timestamp contacts Apple's server)
 *   install.assert                                  [{ path-or-glob, identifier?, teamIdentifier?, authority? }]
 *   install.assertTeamSignedNested                  true: every Mach-O in the bundle carries the app's TeamIdentifier
 * install.* entries win over sign.* entries (so one declaration in `sign` keeps dev and release identifiers identical).
 * `root` resolves entitlement plist paths and checks they exist; without it only the shape is validated.
 */
export function readDevSigningSettings(descriptor = {}, config = {}, { root, exists = existsSync } = {}) {
  const sign = config?.targets?.mac?.sign ?? {};
  const identifier = (raw, label) => validateSigningIdentifier(raw, label, invalidConfig);
  const identifiers = [...compileBundlePathMap(descriptor.identifiers, "install.identifiers", { reject: invalidConfig, value: identifier }),
    ...compileBundlePathMap(sign.identifiers, "sign.identifiers", { reject: invalidConfig, value: identifier })];
  const templateSource = descriptor.identifierTemplate !== undefined ? ["install.identifierTemplate", descriptor.identifierTemplate] : sign.identifierTemplate !== undefined ? ["sign.identifierTemplate", sign.identifierTemplate] : null;
  const identifierTemplate = templateSource ? validateIdentifierTemplate(templateSource[1], templateSource[0], invalidConfig) : undefined;
  const plistPath = (raw, label) => {
    if (typeof raw !== "string" || !raw.trim()) invalidConfig(`${label} must be an entitlements plist path`);
    if (!root) return raw;
    const file = path.resolve(root, raw);
    if (!exists(file)) invalidConfig(`${label} entitlements file not found: ${file}`);
    return file;
  };
  const signMap = typeof sign.entitlements === "object" && sign.entitlements !== null && !Array.isArray(sign.entitlements) ? sign.entitlements : undefined;
  const entitlements = [...compileBundlePathMap(descriptor.entitlements, "install.entitlements", { reject: invalidConfig, value: plistPath }),
    ...compileBundlePathMap(signMap, "sign.entitlements", { reject: invalidConfig, value: plistPath }),
    // Native helpers embedded in the dev .app carry their own entitlements, exactly as the release lane signs them.
    ...compileBundlePathMap((() => { const h = nativeHelpersFromConfig(config); return h ? nativeHelperEntitlementMap(h) : undefined; })(), "macPackage.nativeHelpers", { reject: invalidConfig, value: plistPath })];
  if (descriptor.timestamp !== undefined && typeof descriptor.timestamp !== "boolean") invalidConfig("install.timestamp must be true or false");
  if (descriptor.assertTeamSignedNested !== undefined && typeof descriptor.assertTeamSignedNested !== "boolean") invalidConfig("install.assertTeamSignedNested must be true or false");
  const rawAssertions = descriptor.assert ?? [];
  if (!Array.isArray(rawAssertions)) invalidConfig("install.assert must be an array of { path, identifier?, teamIdentifier?, authority? }");
  const assertions = rawAssertions.map((item, index) => {
    const label = `install.assert[${index}]`;
    if (!item || typeof item !== "object" || Array.isArray(item)) invalidConfig(`${label} must be an object`);
    for (const key of Object.keys(item)) if (!ASSERT_KEYS.has(key)) invalidConfig(`${label}.${key} is not a known key (path, identifier, teamIdentifier, authority)`);
    validateBundleRelativeKey(item.path, `${label}.path`, invalidConfig);
    const checks = ["identifier", "teamIdentifier", "authority"].filter((key) => item[key] !== undefined);
    if (!checks.length) invalidConfig(`${label} needs at least one of identifier, teamIdentifier, authority`);
    for (const key of checks) if (typeof item[key] !== "string" || !item[key].trim()) invalidConfig(`${label}.${key} must be a non-empty string`);
    return { path: item.path, ...Object.fromEntries(checks.map((key) => [key, item[key]])) };
  });
  return { identifiers, identifierTemplate, entitlements, timestamp: descriptor.timestamp === true, assertions, assertTeamSignedNested: descriptor.assertTeamSignedNested === true };
}

function listBundleFiles(bundle) {
  const out = [];
  const walk = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const file = path.join(directory, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) walk(file);
      else if (entry.isFile()) out.push(bundleRelativePath(bundle, file));
    }
  };
  walk(bundle);
  return out;
}

/**
 * Post-sign identity assertions, read from `codesign -dvv` (stdout and stderr: codesign reports on stderr). Fails closed with
 * DEV_MAC_SIGNING_ASSERT_FAILED listing every mismatch. A glob that matches no file is a failure, so a typo cannot pass.
 * `assertTeamSignedNested` additionally requires every Mach-O in the bundle (dylibs included) to be signed by
 * `teamIdentifier`, not ad hoc. Used on the signed staging copy and again on the installed copy (a failure there rolls back).
 * Injectable: run, listFiles(bundle) -> bundle-relative file paths, isMachO(file), join.
 */
export function assertSignedIdentities({ root, bundle, settings, teamIdentifier, identity, env = process.env, run = spawnSync, listFiles = listBundleFiles, isMachO = macho, join = path.join, label = "signed bundle" }) {
  const { assertions = [], assertTeamSignedNested = false } = settings ?? {};
  if (!assertions.length && !assertTeamSignedNested) return { checked: 0 };
  const reports = new Map();
  const report = (file) => {
    if (!reports.has(file)) {
      const result = run("codesign", ["-dvv", file], captureOptions(root, env));
      if (result.error || result.status !== 0) {
        throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_ASSERT_FAILED", `${label}: cannot inspect the signing identity of ${file}: ${result.error?.message || output(result) || `exit ${result.status ?? "unavailable"}`}`, { identity });
      }
      const text = output(result);
      reports.set(file, { identifier: /^Identifier=(.+)$/m.exec(text)?.[1]?.trim(), team: /^TeamIdentifier=(.+)$/m.exec(text)?.[1]?.trim(), authorities: [...text.matchAll(/^Authority=(.+)$/gm)].map((match) => match[1].trim()), adhoc: /^Signature=adhoc$/m.test(text) });
    }
    return reports.get(file);
  };
  const absolute = (relative) => (relative === "." ? bundle : join(bundle, ...relative.split("/")));
  let files;
  const listed = () => (files ??= listFiles(bundle));
  const problems = [];
  let checked = 0;
  for (const item of assertions) {
    const targets = isGlobPattern(item.path) ? listed().filter((relative) => globToRegExp(item.path).test(relative)) : [item.path];
    if (!targets.length) { problems.push(`${item.path}: matches no file in the bundle`); continue; }
    for (const relative of targets) {
      const actual = report(absolute(relative));
      checked += 1;
      if (item.identifier !== undefined && actual.identifier !== item.identifier) problems.push(`${relative}: Identifier=${actual.identifier ?? "missing"}, expected ${item.identifier}`);
      if (item.teamIdentifier !== undefined && actual.team !== item.teamIdentifier) problems.push(`${relative}: TeamIdentifier=${actual.team ?? "missing"}, expected ${item.teamIdentifier}`);
      if (item.authority !== undefined && !actual.authorities.includes(item.authority)) problems.push(`${relative}: Authority does not include ${JSON.stringify(item.authority)} (found ${actual.authorities.map((name) => JSON.stringify(name)).join(", ") || "none"})`);
    }
  }
  if (assertTeamSignedNested) {
    if (!teamIdentifier) problems.push("assertTeamSignedNested: the expected TeamIdentifier is unknown (ad hoc signing has none)");
    else {
      for (const relative of [".", ...listed().filter((entry) => isMachO(absolute(entry)))]) {
        const actual = report(absolute(relative));
        checked += 1;
        if (actual.adhoc || actual.team !== teamIdentifier) problems.push(`${relative}: ${actual.adhoc ? "ad hoc signed" : `TeamIdentifier=${actual.team ?? "missing"}`}, expected TeamIdentifier=${teamIdentifier}`);
      }
    }
  }
  if (problems.length) {
    throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_ASSERT_FAILED", `${label}: signing identity assertions failed (${problems.length}): ${problems.slice(0, 20).join("; ")}${problems.length > 20 ? "; ..." : ""}. Refusing dev installation.`, { identity, problems });
  }
  return { checked };
}

export function signMacosDevelopmentBundle(options) {
  const { root, bundle, env = process.env, run = spawnSync } = options;
  const context = { root, env, run };
  // Declared per-binary settings are validated before any Keychain or codesign call.
  const settings = readDevSigningSettings(options.descriptor, options.config, { root });
  const signing = resolveMacosDevelopmentSigning({ ...options, env, run });
  const { identity, certificateSha1, teamIdentifier, entitlements, provisioningProfile } = signing;
  const embedded = path.join(bundle, "Contents", "embedded.provisionprofile");
  if (provisioningProfile && path.resolve(provisioningProfile) !== path.resolve(embedded)) copyFileSync(provisioningProfile, embedded);
  const scratch = mkdtempSync(path.join(os.tmpdir(), "rightkit-dev-signing-"));
  try {
    const appInfo = bundleInfo(bundle, context);
    const appEntitlements = entitlements ? readPlist(entitlements, context) : {};
    // Loose tools cannot claim the parent app's profile-restricted entitlements.
    // Retain their existing entitlements; give unsigned tools only the app's runtime relaxations.
    const runtimeEntitlements = Object.fromEntries(Object.entries(appEntitlements).filter(([key]) => key.startsWith("com.apple.security.cs.") || key === "com.apple.security.device.audio-input"));
    const runtimeFile = path.join(scratch, "runtime.plist");
    writeFileSync(runtimeFile, JSON.stringify(runtimeEntitlements));
    checked("plutil", ["-convert", "xml1", runtimeFile], context, "DEV_MAC_SIGNING_CONFIG_INVALID", identity);
    // Apps keep sidecar/helper variants beside their main entitlements file:
    // entitlements-engine.plist, entitlements-cloudsync.plist, entitlements-credentials.plist.
    const entitlementVariants = entitlements ? readdirSync(path.dirname(entitlements))
      .filter((name) => name.startsWith(`${path.basename(entitlements, path.extname(entitlements))}-`) && name.endsWith(".plist"))
      .map((name) => ({ file: path.join(path.dirname(entitlements), name), suffix: path.basename(name, ".plist").slice(path.basename(entitlements, path.extname(entitlements)).length + 1).toLowerCase() }))
      .sort((a, b) => b.suffix.length - a.suffix.length) : [];
    // Ad hoc signatures cannot carry profile-restricted entitlements (iCloud, keychain-access-groups, application-identifier):
    // AMFI would kill the app at launch. Ad hoc mode therefore applies only the hardened-runtime relaxations and audio input.
    const safeFiles = new Map();
    const entitlementFile = (file) => {
      if (!signing.adhoc) return file;
      if (!safeFiles.has(file)) {
        const safe = adhocSafeEntitlements(readPlist(file, context));
        const out = path.join(scratch, `adhoc-${safeFiles.size}.plist`);
        writeFileSync(out, JSON.stringify(safe));
        checked("plutil", ["-convert", "xml1", out], context, "DEV_MAC_SIGNING_CONFIG_INVALID", identity);
        safeFiles.set(file, out);
      }
      return safeFiles.get(file);
    };
    const targets = [];
    const bundleExecutables = new Set();
    const visit = (directory, isBundle = false) => {
      const info = isBundle ? bundleInfo(directory, context) : {};
      if (info.CFBundleExecutable) {
        for (const file of [path.join(directory, "Contents/MacOS", info.CFBundleExecutable), path.join(directory, info.CFBundleExecutable)]) {
          if (existsSync(file)) bundleExecutables.add(realpathSync(file));
        }
      }
      for (const entry of readdirSync(directory, { withFileTypes: true })) {
        const file = path.join(directory, entry.name);
        if (entry.isSymbolicLink()) continue;
        if (entry.isDirectory()) visit(file, /\.(app|framework|xpc|appex|bundle)$/.test(entry.name));
        else if (entry.isFile() && macho(file)) targets.push({ file, info: null });
      }
      if (isBundle) targets.push({ file: directory, info });
    };
    visit(bundle, true);
    const ordered = targets.filter((target) => target.info || !bundleExecutables.has(realpathSync(target.file)))
      .sort((a, b) => b.file.split(path.sep).length - a.file.split(path.sep).length || a.file.localeCompare(b.file));
    const commands = [];
    for (const target of ordered) {
      // Bind the exact profile-authorized certificate even when names collide in Keychain.
      const relative = bundleRelativePath(bundle, target.file);
      let identifier;
      try {
        identifier = resolveBundleIdentifier({ relative, identifiers: settings.identifiers, template: settings.identifierTemplate, bundleId: options.descriptor?.bundleId ?? appInfo.CFBundleIdentifier, plain: !target.info });
      } catch (cause) { throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_CONFIG_INVALID", cause.message, { cause }); }
      // --timestamp only when declared (install.timestamp): it needs Apple's timestamp server, and dev builds otherwise sign offline.
      const args = ["--force", "--options", "runtime", ...(settings.timestamp ? ["--timestamp"] : []), ...(identifier ? ["--identifier", identifier] : []), "--sign", certificateSha1];
      const parent = target.file === bundle;
      const mappedEntitlements = lookupBundlePath(settings.entitlements, relative || ".");
      const profiledHelper = target.info && existsSync(path.join(target.file, "Contents/embedded.provisionprofile")) && target.info.CFBundleIdentifier === appInfo.CFBundleIdentifier;
      if (profiledHelper && !parent && provisioningProfile) copyFileSync(embedded, path.join(target.file, "Contents/embedded.provisionprofile"));
      const variant = !parent && entitlementVariants.find((item) => path.basename(target.file).replace(/\.(app|xpc|appex|bundle)$/, "").toLowerCase().endsWith(item.suffix));
      if (mappedEntitlements) args.push("--entitlements", entitlementFile(mappedEntitlements));
      else if (variant) args.push("--entitlements", entitlementFile(variant.file));
      else if (entitlements && (parent || profiledHelper)) args.push("--entitlements", entitlementFile(entitlements));
      else {
        const existing = run("codesign", ["--display", "--entitlements", ":-", target.file], captureOptions(root, env));
        if (existing.status === 0 && /<plist|<dict/.test(String(existing.stdout ?? ""))) args.push("--preserve-metadata=entitlements");
        else if (!target.info && (statSync(target.file).mode & 0o111) && !/\.(dylib|so)$/.test(target.file) && Object.keys(runtimeEntitlements).length) args.push("--entitlements", runtimeFile);
      }
      commands.push([...args, target.file]);
    }
    const signed = runMacCodesignCommands(commands, { ...options.sessionOptions, cwd: root, env, run });
    for (const [index, args] of commands.entries()) checked("codesign", args, { ...context, run: () => signed[index] }, "DEV_MAC_SIGNING_FAILED", identity);
    checked("codesign", ["--verify", "--strict", "--deep", bundle], context, "DEV_MAC_SIGNATURE_INVALID", identity);
    const details = output(checked("codesign", ["--display", "--verbose=4", bundle], context, "DEV_MAC_SIGNATURE_INVALID", identity));
    if (signing.adhoc) {
      // No TeamIdentifier or identity-based designated requirement to check: an ephemeral runner has no TCC state to keep stable.
      if (!/^Signature=adhoc$/m.test(details)) throw new MacosDevelopmentSigningError("DEV_MAC_SIGNATURE_INVALID", `CI ad hoc signing: expected Signature=adhoc on ${bundle}. Refusing dev installation.`, { identity });
      return { ...signing, provisioningProfile: null, verified: "strict-deep", designatedRequirement: null, signedTargets: ordered.length, releaseArtifact: false,
        receipt: { signing: CI_ADHOC_SIGNING, signature: "adhoc", releaseArtifact: false, note: "Ad hoc signed on a GitHub-hosted CI runner for candidate qualification only; never a release artifact. Release signing happens in the protected sign job." } };
    }
    const actualTeam = /^TeamIdentifier=(.+)$/m.exec(details)?.[1]?.trim();
    if (actualTeam !== teamIdentifier || /^Signature=adhoc$/m.test(details)) throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_TEAM_MISMATCH", `Signing identity ${JSON.stringify(identity)}: expected TeamIdentifier=${teamIdentifier}, found ${actualTeam ?? "missing"}. Refusing dev installation.`, { identity, expectedTeam: teamIdentifier, actualTeam });
    // Fail closed unless the designated requirement is identity-based: an ad hoc or cdhash requirement changes every build, so macOS TCC re-prompts.
    const requirement = output(checked("codesign", ["--display", "-r-", bundle], context, "DEV_MAC_SIGNATURE_INVALID", identity));
    const designated = /^designated => (.+)$/m.exec(requirement)?.[1]?.trim();
    if (!designated || /\bcdhash\b/.test(designated)) throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_UNSTABLE_REQUIREMENT", `Signing identity ${JSON.stringify(identity)}: designated requirement ${designated ? JSON.stringify(designated) : "is missing"} is not stable across rebuilds (cdhash-bound). Refusing dev installation.`, { identity, designatedRequirement: designated });
    // Declared identity assertions (install.assert, install.assertTeamSignedNested) on the signed staging copy, before any install.
    const asserted = assertSignedIdentities({ root, bundle, settings, teamIdentifier, identity, env, run, label: "signed staging copy", ...(options.listFiles ? { listFiles: options.listFiles } : {}), ...(options.isMachO ? { isMachO: options.isMachO } : {}) });
    return { ...signing, provisioningProfile: provisioningProfile ? embedded : null, verified: "strict-deep", designatedRequirement: designated, signedTargets: ordered.length, ...(asserted.checked ? { assertedIdentities: asserted.checked } : {}) };
  } finally { rmSync(scratch, { recursive: true, force: true }); }
}

/**
 * One stable designated requirement across dev installs. macOS keys privacy grants (TCC) to the code identity, so a dev
 * build whose designated requirement differs from the installed copy's would silently drop every grant. Fails with
 * DEV_MAC_SIGNING_REQUIREMENT_CHANGED. An installed copy that is unsigned, ad hoc or cdhash-bound has no stable
 * grants to keep, so it does not block the move to the stable identity.
 */
export function assertStableDesignatedRequirement({ root, installed, expected, env = process.env, run = spawnSync }) {
  const result = run("codesign", ["--display", "-r-", installed], captureOptions(root, env));
  if (result.error || result.status !== 0) return null;
  const current = /^designated => (.+)$/m.exec(output(result))?.[1]?.trim();
  if (!current || /\bcdhash\b/.test(current)) return null;
  if (current !== expected) throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_REQUIREMENT_CHANGED", `Installed ${installed} has designated requirement ${JSON.stringify(current)} but this dev build is signed with ${JSON.stringify(expected)}. Installing it would reset the app's macOS privacy grants. Sign with the same identity as the installed copy (the one stable Developer ID) or remove the installed app deliberately.`, { designatedRequirement: expected, installedRequirement: current });
  return current;
}
