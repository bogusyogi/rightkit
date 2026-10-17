import { createHash, X509Certificate } from "node:crypto";
import { closeSync, copyFileSync, existsSync, mkdtempSync, openSync, readFileSync, readSync, readdirSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { runMacCodesignCommands } from "./macos-signing-session.mjs";

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
  const entitlements = sign.entitlements ? path.resolve(root, sign.entitlements) : mac.entitlements ? path.resolve(tauriRoot, mac.entitlements) : null;
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
  const profiled = (info) => identities.filter((item) => info.certificates.some((cert) => cert.sha1 === item.sha1) && /^(Apple Development|Mac Developer|Developer ID Application):/.test(item.name));
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

export function signMacosDevelopmentBundle(options) {
  const { root, bundle, env = process.env, run = spawnSync } = options;
  const context = { root, env, run };
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
        const safe = Object.fromEntries(Object.entries(readPlist(file, context)).filter(([key]) => key.startsWith("com.apple.security.cs.") || key === "com.apple.security.device.audio-input"));
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
      const args = ["--force", "--options", "runtime", "--sign", certificateSha1];
      const parent = target.file === bundle;
      const profiledHelper = target.info && existsSync(path.join(target.file, "Contents/embedded.provisionprofile")) && target.info.CFBundleIdentifier === appInfo.CFBundleIdentifier;
      if (profiledHelper && !parent && provisioningProfile) copyFileSync(embedded, path.join(target.file, "Contents/embedded.provisionprofile"));
      const variant = !parent && entitlementVariants.find((item) => path.basename(target.file).replace(/\.(app|xpc|appex|bundle)$/, "").toLowerCase().endsWith(item.suffix));
      if (variant) args.push("--entitlements", entitlementFile(variant.file));
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
    return { ...signing, provisioningProfile: provisioningProfile ? embedded : null, verified: "strict-deep", designatedRequirement: designated, signedTargets: ordered.length };
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
  if (!current || /cdhash/.test(current)) return null;
  if (current !== expected) throw new MacosDevelopmentSigningError("DEV_MAC_SIGNING_REQUIREMENT_CHANGED", `Installed ${installed} has designated requirement ${JSON.stringify(current)} but this dev build is signed with ${JSON.stringify(expected)}. Installing it would reset the app's macOS privacy grants. Sign with the same identity as the installed copy (the one stable Developer ID) or remove the installed app deliberately.`, { designatedRequirement: expected, installedRequirement: current });
  return current;
}
