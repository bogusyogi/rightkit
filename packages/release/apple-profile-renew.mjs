import { createHash, createPrivateKey, randomUUID, sign, X509Certificate } from "node:crypto";
import { constants, copyFileSync, existsSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { notarytoolAuthArgs } from "./notary-auth.mjs";

const ASC = "https://api.appstoreconnect.apple.com";
export class AppleProfileRenewError extends Error {
  constructor(code, message) { super(message); this.name = "AppleProfileRenewError"; this.code = code; }
}
const fail = (code, message) => { throw new AppleProfileRenewError(code, message); };
const sha1 = (bytes) => createHash("sha1").update(bytes).digest("hex").toUpperCase();
const serial = (value) => String(value ?? "").replace(/[^a-f0-9]/gi, "").replace(/^0+/, "").toUpperCase();
const query = (endpoint, params) => `${endpoint}?${new URLSearchParams(params)}`;

export function appleProfileAuth(env = process.env) {
  if (!env.APPLE_API_ISSUER?.trim()) fail("APPLE_PROFILE_ISSUER_MISSING", "Set APPLE_API_ISSUER using existing notarization environment.");
  if (!env.APPLE_API_KEY?.trim() || !env.APPLE_API_KEY_PATH?.trim()) fail("APPLE_PROFILE_KEY_MISSING", "Set APPLE_API_KEY & APPLE_API_KEY_PATH using existing notarization environment.");
  const args = notarytoolAuthArgs({ env });
  const auth = { keyPath: args[1], keyId: args[3], issuer: args[5] };
  if (!existsSync(auth.keyPath) || !statSync(auth.keyPath).isFile()) fail("APPLE_PROFILE_KEY_MISSING", "Configured App Store Connect API key file is missing.");
  return auth;
}

// Only this in-process signer reads key material. Never send it to a subprocess,
// include it in diagnostics, or expose it in returned renewal plans.
export function signAppleApiJwt({ keyPath, keyId, issuer }, now = new Date()) {
  try {
    const key = createPrivateKey(readFileSync(keyPath));
    if (key.asymmetricKeyType !== "ec" || key.asymmetricKeyDetails?.namedCurve !== "prime256v1") throw new Error();
    const iat = Math.floor(now.getTime() / 1000);
    const encoded = (value) => Buffer.from(JSON.stringify(value)).toString("base64url");
    const payload = `${encoded({ alg: "ES256", kid: keyId, typ: "JWT" })}.${encoded({ iss: issuer, iat, exp: iat + 600, aud: "appstoreconnect-v1" })}`;
    return `${payload}.${sign("sha256", Buffer.from(payload), { key, dsaEncoding: "ieee-p1363" }).toString("base64url")}`;
  } catch { fail("APPLE_PROFILE_KEY_INVALID", "Unable to sign API JWT with configured .p8 key."); }
}

function checked(run, command, args, options = {}) {
  const result = run(command, args, { encoding: "utf8", stdio: "pipe", shell: false, windowsHide: true, ...options });
  if (result.error || result.status !== 0) fail("APPLE_PROFILE_LOCAL_INSPECTION_FAILED", `${command} failed while inspecting profile or public signing certificates.`);
  return result.stdout;
}

export function decodeAppleProfile(file, { run = spawnSync } = {}) {
  const cms = checked(run, "security", ["cms", "-D", "-i", file]);
  // plutil JSON does not support provisioning profiles' native date/data values.
  const parser = "import base64,datetime,json,plistlib,sys\ndef encode(v):\n if isinstance(v,bytes): return base64.b64encode(v).decode('ascii')\n if isinstance(v,(datetime.date,datetime.datetime)): return v.isoformat()\n raise TypeError(type(v).__name__)\nprint(json.dumps(plistlib.loads(sys.stdin.buffer.read()),default=encode))";
  try { return JSON.parse(checked(run, "python3", ["-c", parser], { input: cms })); }
  catch (error) {
    if (error instanceof AppleProfileRenewError) throw error;
    fail("APPLE_PROFILE_INVALID", "Cannot decode macOS provisioning profile.");
  }
}

export function describeAppleProfile(profile) {
  if (!profile.Platform?.includes("OSX") || profile.TeamIdentifier?.length !== 1 || !profile.Name || !profile.Entitlements || !profile.DeveloperCertificates?.length) fail("APPLE_PROFILE_INVALID", "Expected named macOS profile with one team, entitlements & signing certificates.");
  const team = profile.TeamIdentifier[0];
  const appId = profile.Entitlements["com.apple.application-identifier"] ?? profile.Entitlements["application-identifier"];
  const prefixes = profile.ApplicationIdentifierPrefix ?? [team];
  const prefix = prefixes.find((value) => typeof appId === "string" && appId.startsWith(`${value}.`));
  const bundleId = prefix && appId.slice(prefix.length + 1);
  if (!bundleId || bundleId.includes("*")) fail("APPLE_PROFILE_INVALID", "Profile must authorize an explicit bundle identifier.");
  const development = profile.Entitlements["get-task-allow"] ?? profile.Entitlements["com.apple.security.get-task-allow"];
  const type = profile.ProvisionsAllDevices === true && development !== true ? "MAC_APP_DIRECT"
    : development === true && Array.isArray(profile.ProvisionedDevices) && profile.ProvisionedDevices.length ? "MAC_APP_DEVELOPMENT" : null;
  if (!type) fail("APPLE_PROFILE_TYPE_UNSUPPORTED", "Only MAC_APP_DIRECT & MAC_APP_DEVELOPMENT profiles can be renewed.");
  return { team, bundleId, type, name: profile.Name, entitlements: profile.Entitlements };
}

export function findAppleSigningCertificate(info, { env = process.env, run = spawnSync, now = new Date() } = {}) {
  const identities = checked(run, "security", ["find-identity", "-v", "-p", "codesigning"]);
  const valid = new Set([...identities.matchAll(/([a-f0-9]{40})\s+"[^"\n]+"/gi)].map((match) => match[1].toUpperCase()));
  const pem = checked(run, "security", ["find-certificate", "-a", "-p"]);
  const prefix = info.type === "MAC_APP_DIRECT" ? "Developer ID Application:" : "Apple Development:";
  const requested = env.APPLE_SIGNING_IDENTITY?.trim() || env.APPLE_DEVELOPER_ID?.trim();
  const candidates = [...pem.matchAll(/-----BEGIN CERTIFICATE-----[\s\S]*?-----END CERTIFICATE-----/g)].map((match) => {
    const cert = new X509Certificate(match[0]);
    return { sha1: sha1(cert.raw), serialNumber: cert.serialNumber, name: /^CN=(.+)$/m.exec(cert.subject)?.[1], team: /^OU=(.+)$/m.exec(cert.subject)?.[1], issued: Date.parse(cert.validFrom), expires: Date.parse(cert.validTo) };
  }).filter((cert) => valid.has(cert.sha1) && cert.name?.startsWith(prefix) && cert.team === info.team && cert.issued <= now.getTime() && cert.expires > now.getTime()
    && (!requested || cert.sha1 === requested.toUpperCase() || cert.name === requested))
    .sort((a, b) => b.issued - a.issued || a.sha1.localeCompare(b.sha1));
  if (!candidates.length) fail("APPLE_PROFILE_SIGNING_CERT_MISSING", `No valid ${prefix} certificate/private-key identity for team ${info.team} in Keychain.`);
  return candidates[0];
}

export function entitlementsSuperset(newValue, oldValue) {
  if (Array.isArray(oldValue)) return Array.isArray(newValue) && oldValue.every((oldItem) => newValue.some((newItem) => entitlementsSuperset(newItem, oldItem)));
  if (oldValue && typeof oldValue === "object") return !!newValue && typeof newValue === "object" && !Array.isArray(newValue)
    && Object.entries(oldValue).every(([key, value]) => Object.hasOwn(newValue, key) && entitlementsSuperset(newValue[key], value));
  return newValue === oldValue;
}

export function verifyAppleProfile(profile, previous, certificate, now = new Date()) {
  const before = describeAppleProfile(previous), after = describeAppleProfile(profile);
  if (before.team !== after.team) fail("APPLE_PROFILE_TEAM_MISMATCH", "Renewed profile TeamIdentifier differs from current profile.");
  if (before.bundleId !== after.bundleId || before.type !== after.type) fail("APPLE_PROFILE_IDENTITY_MISMATCH", "Renewed profile bundle identifier or type differs from current profile.");
  if (!profile.DeveloperCertificates.some((data) => sha1(Buffer.from(data, "base64")) === certificate.sha1)) fail("APPLE_PROFILE_CERT_MISMATCH", "Renewed profile does not authorize selected Keychain certificate.");
  if (!(Date.parse(profile.ExpirationDate) > now.getTime())) fail("APPLE_PROFILE_INVALID", "Renewed profile is expired or has no expiration date.");
  if (!entitlementsSuperset(after.entitlements, before.entitlements)) fail("APPLE_PROFILE_ENTITLEMENTS_MISMATCH", "Renewed profile entitlements are not a superset of current profile.");
  if (before.type === "MAC_APP_DEVELOPMENT" && !previous.ProvisionedDevices.every((udid) => profile.ProvisionedDevices?.includes(udid))) fail("APPLE_PROFILE_DEVICES_MISMATCH", "Renewed profile omits previously provisioned devices.");
}

export function configuredAppleProfile({ root, config = {}, profilePath }) {
  if (profilePath) return path.resolve(root, profilePath);
  const configured = config.targets?.mac?.sign?.provisioningProfile;
  if (configured) return path.resolve(root, configured);
  const manifest = config.development?.targets?.mac?.install?.manifestPath ?? "src-tauri/Cargo.toml";
  const tauriRoot = path.dirname(path.resolve(root, manifest));
  let mac = {};
  for (const name of ["tauri.conf.json", "tauri.macos.conf.json"]) {
    const file = path.join(tauriRoot, name);
    if (existsSync(file)) mac = { ...mac, ...JSON.parse(readFileSync(file, "utf8")).bundle?.macOS };
  }
  const value = mac.provisioningProfile ?? mac.files?.["embedded.provisionprofile"] ?? mac.files?.["/embedded.provisionprofile"];
  if (!value) fail("APPLE_PROFILE_PATH_MISSING", "Configure targets.mac.sign.provisioningProfile or Tauri macOS profile; use --profile for script-managed embedding.");
  return path.resolve(tauriRoot, value);
}

export async function renewAppleProfile({ root, config = {}, profilePath, dryRun = false, env = process.env, run = spawnSync, fetch: fetchApi = globalThis.fetch, jwtSigner = signAppleApiJwt, now = new Date(), log = console.log }) {
  const file = configuredAppleProfile({ root, config, profilePath });
  if (!existsSync(file)) fail("APPLE_PROFILE_PATH_MISSING", "Configured provisioning profile file is missing.");
  const original = readFileSync(file);
  const previous = decodeAppleProfile(file, { run });
  const info = describeAppleProfile(previous);
  const certificate = findAppleSigningCertificate(info, { env, run, now });
  const auth = appleProfileAuth(env);
  const token = await jwtSigner(auth, now);
  const calls = [];
  const request = async (method, endpoint, body) => {
    const url = new URL(endpoint, ASC);
    if (url.origin !== ASC || !url.pathname.startsWith("/v1/")) fail("APPLE_PROFILE_API_INVALID", "ASC pagination URL is outside API origin.");
    calls.push({ method, path: `${url.pathname}${url.search}` });
    log(`${method} ${decodeURIComponent(url.pathname + url.search)}`);
    if (dryRun && method !== "GET") fail("APPLE_PROFILE_DRY_RUN_MUTATION", "Dry-run cannot mutate ASC.");
    let response;
    try { response = await fetchApi(url.href, { method, headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" }, ...(body ? { body: JSON.stringify(body) } : {}), redirect: "error", signal: AbortSignal.timeout(30_000) }); }
    catch { fail("APPLE_PROFILE_API_FAILED", `ASC ${method} ${url.pathname} request failed.`); }
    if (response.status === 403) fail("APPLE_PROFILE_INSUFFICIENT_ROLE", "ASC key lacks permission for Certificates, Identifiers & Profiles (403).");
    if (!response.ok) fail("APPLE_PROFILE_API_FAILED", `ASC ${method} ${url.pathname} returned HTTP ${response.status}.`);
    try { return await response.json(); }
    catch { fail("APPLE_PROFILE_API_INVALID", "ASC returned invalid JSON."); }
  };
  const list = async (endpoint) => {
    const items = [], visited = new Set();
    while (endpoint) {
      if (visited.has(endpoint) || visited.size >= 100) fail("APPLE_PROFILE_API_INVALID", "Invalid or excessive ASC pagination.");
      visited.add(endpoint);
      const result = await request("GET", endpoint);
      if (!Array.isArray(result.data)) fail("APPLE_PROFILE_API_INVALID", "ASC list response has no resource array.");
      items.push(...result.data); endpoint = result.links?.next;
    }
    return items;
  };
  const certificates = await list(query("/v1/certificates", { "filter[serialNumber]": certificate.serialNumber, limit: "200" }));
  const ascCert = certificates.find((item) => serial(item.attributes?.serialNumber) === serial(certificate.serialNumber));
  if (!ascCert?.id) fail("APPLE_PROFILE_CERT_NOT_IN_ASC", `Keychain certificate ${certificate.sha1} (serial ${certificate.serialNumber}) is absent from ASC.`);
  const bundles = await list(query("/v1/bundleIds", { "filter[identifier]": info.bundleId, limit: "200" }));
  const bundle = bundles.find((item) => item.attributes?.identifier === info.bundleId && ["MAC_OS", "UNIVERSAL"].includes(item.attributes?.platform));
  if (!bundle?.id) fail("APPLE_PROFILE_BUNDLE_ID_MISSING", `No macOS bundleId resource for ${info.bundleId} in ASC.`);
  const name = `${info.name.replace(/-\d{8}$/, "")}-${now.toISOString().slice(0, 10).replaceAll("-", "")}`;
  const relationships = { bundleId: { data: { type: "bundleIds", id: bundle.id } }, certificates: { data: [{ type: "certificates", id: ascCert.id }] } };
  if (info.type === "MAC_APP_DEVELOPMENT") {
    const devices = await list(query("/v1/devices", { "filter[platform]": "MAC_OS", limit: "200" }));
    relationships.devices = { data: previous.ProvisionedDevices.map((udid) => {
      const device = devices.find((item) => item.attributes?.udid === udid && item.attributes?.status === "ENABLED");
      if (!device?.id) fail("APPLE_PROFILE_DEVICE_MISSING", `Previously provisioned device ${udid} is absent or disabled in ASC.`);
      return { type: "devices", id: device.id };
    }) };
  }
  const body = { data: { type: "profiles", attributes: { name, profileType: info.type }, relationships } };
  const plan = { dryRun, profilePath: file, profileType: info.type, teamIdentifier: info.team, name, certificate: { ...certificate, id: ascCert.id }, bundleId: { identifier: info.bundleId, id: bundle.id }, entitlements: info.entitlements, calls, body };
  log(JSON.stringify({ profilePath: file, profileType: info.type, teamIdentifier: info.team, name, certificate: plan.certificate, bundleId: plan.bundleId, entitlements: plan.entitlements }, null, 2));
  if (dryRun) {
    calls.push({ method: "POST", path: "/v1/profiles", planned: true });
    log(`PLANNED POST /v1/profiles ${JSON.stringify(body)}`);
    log(`PLANNED download profileContent, verify & replace ${file} with adjacent backup`);
    return plan;
  }
  let created = (await request("POST", "/v1/profiles", body)).data;
  if (!created?.id) fail("APPLE_PROFILE_API_INVALID", "ASC did not return a created profile id.");
  if (!created.attributes?.profileContent) created = (await request("GET", `/v1/profiles/${encodeURIComponent(created.id)}`)).data;
  const content = created?.attributes?.profileContent;
  if (typeof content !== "string" || !content || !/^[A-Za-z0-9+/]+={0,2}$/.test(content)) fail("APPLE_PROFILE_API_INVALID", "ASC profileContent is missing or invalid.");
  const temporary = `${file}.renew-${randomUUID()}.tmp`;
  const backup = `${file}.backup-${now.toISOString().replace(/[:.]/g, "-")}-${randomUUID()}`;
  try {
    writeFileSync(temporary, Buffer.from(content, "base64"), { flag: "wx", mode: statSync(file).mode & 0o777 });
    verifyAppleProfile(decodeAppleProfile(temporary, { run }), previous, certificate, now);
    if (!readFileSync(file).equals(original)) fail("APPLE_PROFILE_CHANGED", "Current profile changed during renewal; refusing replacement.");
    copyFileSync(file, backup, constants.COPYFILE_EXCL);
    renameSync(temporary, file);
  } finally { rmSync(temporary, { force: true }); }
  log(`Renewed ${file}; preserved previous profile at ${backup}`);
  return { ...plan, backupPath: backup, profileId: created.id };
}

export async function main(args = process.argv.slice(2), options = {}) {
  if (args.includes("--help") || args.includes("-h")) {
    (options.log ?? console.log)("right-release apple profile renew [--config <file>] [--profile <file>] [--dry-run]\nUses APPLE_API_ISSUER, APPLE_API_KEY & APPLE_API_KEY_PATH. Dry-run performs GET discovery only; never POSTs or writes files.");
    return;
  }
  if (args[0] !== "profile" || args[1] !== "renew") fail("APPLE_PROFILE_USAGE", "Expected: right-release apple profile renew");
  let configPath = "right-release.config.mjs", profilePath, dryRun = false;
  for (let i = 2; i < args.length; i++) {
    if (args[i] === "--dry-run") dryRun = true;
    else if (["--config", "--profile"].includes(args[i])) {
      const flag = args[i], value = args[++i];
      if (!value || value.startsWith("--")) fail("APPLE_PROFILE_USAGE", `Missing value for ${flag}`);
      if (flag === "--config") configPath = value; else profilePath = value;
    } else fail("APPLE_PROFILE_USAGE", `Unknown option: ${args[i]}`);
  }
  const absolute = path.resolve(configPath);
  const loaded = await import(pathToFileURL(absolute));
  return renewAppleProfile({ ...options, root: path.dirname(absolute), config: loaded.default ?? loaded.config ?? loaded, profilePath, dryRun });
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => { console.error(`right-release apple profile renew: [${error.code ?? "APPLE_PROFILE_FAILED"}] ${error instanceof AppleProfileRenewError ? error.message : "Renewal failed; check app configuration & local file access."}`); process.exitCode = 1; });
}
