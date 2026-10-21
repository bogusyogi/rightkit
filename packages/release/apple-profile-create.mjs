import { createHash, generateKeyPairSync, randomUUID, sign, X509Certificate } from "node:crypto";
import { chmodSync, copyFileSync, constants, existsSync, mkdirSync, mkdtempSync, realpathSync, renameSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { appleProfileAuth, decodeAppleProfile, findAppleSigningCertificate, signAppleApiJwt } from "./apple-profile-renew.mjs";

// right-release apple-profile-create: issue a Mac Development provisioning profile
// (MAC_APP_DEVELOPMENT) for one explicit bundle id with the ICLOUD/CloudKit capability,
// an Apple Development signing identity and every enabled Mac device of the team.
// Credentials follow apple-profile-renew: key id/issuer from the release config or
// APPLE_API_KEY/APPLE_API_ISSUER, the .p8 from a file path, never inline PEM.

const ASC = "https://api.appstoreconnect.apple.com";
const PROFILE_TYPE = "MAC_APP_DEVELOPMENT";
const DIRECT_TYPE = "MAC_APP_DIRECT";
export class AppleProfileCreateError extends Error {
  constructor(code, message) { super(message); this.name = "AppleProfileCreateError"; this.code = code; }
}
const fail = (code, message) => { throw new AppleProfileCreateError(code, message); };
const sha1 = (bytes) => createHash("sha1").update(bytes).digest("hex").toUpperCase();
const serial = (value) => String(value ?? "").replace(/[^a-f0-9]/gi, "").replace(/^0+/, "").toUpperCase();
const query = (endpoint, params) => `${endpoint}?${new URLSearchParams(params)}`;
const enc = encodeURIComponent;
const PEM_MARKER = "-----BEGIN";

export function containerManualSteps(bundleId, containers) {
  return [
    "The App Store Connect API has no endpoint to create or assign iCloud containers, so do this once at https://developer.apple.com/account/resources:",
    ...containers.map((container) => `  1. Identifiers > filter "iCloud Containers" > register ${container} if it is not listed.`),
    `  2. Identifiers > filter "App IDs" > ${bundleId} > Capabilities > iCloud > tick "Include CloudKit support" > Edit/Configure > select ${containers.join(", ")} > Save.`,
    "  3. Re-run the same right-release apple-profile-create command with --apply.",
  ].join("\n");
}

// ---- PKCS#10 CSR built with node:crypto only (no openssl binary) -------------------

const derLength = (n) => n < 0x80 ? Buffer.from([n]) : n < 0x100 ? Buffer.from([0x81, n]) : Buffer.from([0x82, n >> 8, n & 0xff]);
const tlv = (tag, ...parts) => { const body = Buffer.concat(parts); return Buffer.concat([Buffer.from([tag]), derLength(body.length), body]); };

export function buildCsr(commonName, { privateKey, publicKey }) {
  const subject = tlv(0x30, tlv(0x31, tlv(0x30, tlv(0x06, Buffer.from("550403", "hex")), tlv(0x0c, Buffer.from(commonName, "utf8")))));
  const info = tlv(0x30, tlv(0x02, Buffer.from([0])), subject, publicKey.export({ type: "spki", format: "der" }), Buffer.from([0xa0, 0x00]));
  const algorithm = tlv(0x30, tlv(0x06, Buffer.from("2a864886f70d01010b", "hex")), Buffer.from([0x05, 0x00]));
  const signature = sign("sha256", info, privateKey);
  const der = tlv(0x30, info, algorithm, tlv(0x03, Buffer.from([0]), signature));
  const pem = `-----BEGIN CERTIFICATE REQUEST-----\n${der.toString("base64").replace(/(.{64})/g, "$1\n").replace(/\n$/, "")}\n-----END CERTIFICATE REQUEST-----\n`;
  return { der, info, signature, pem };
}

// ---- arguments ---------------------------------------------------------------------

const CONTAINER_PATTERN = /^iCloud\.[A-Za-z0-9][A-Za-z0-9.-]*$/;
const BUNDLE_PATTERN = /^[A-Za-z0-9][A-Za-z0-9.-]*$/;
const UDID_PATTERN = /^[A-Fa-f0-9-]{8,64}$/;

export function parseCreateArgs(args) {
  const out = { configPath: undefined, bundleId: "", name: "", containers: [], type: PROFILE_TYPE, certificate: "create", out: "", dryRun: true, apply: false, registerDevices: [], keyOut: "" };
  const values = { "--config": "configPath", "--bundle-id": "bundleId", "--name": "name", "--type": "type", "--certificate": "certificate", "--out": "out", "--key-out": "keyOut", "--expected-team": "expectedTeam", "--expected-keychain-group": "expectedKeychainGroup" };
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === "--dry-run") out.dryRun = true;
    else if (arg === "--apply") out.apply = true;
    else if (Object.hasOwn(values, arg) || arg ==="--icloud-container" || arg === "--register-device") {
      const value = args[++i];
      if (!value || value.startsWith("--")) fail("APPLE_PROFILE_USAGE", `Missing value for ${arg}`);
      if (arg === "--icloud-container") out.containers.push(value);
      else if (arg === "--register-device") out.registerDevices.push(value);
      else out[values[arg]] = value;
    } else fail("APPLE_PROFILE_USAGE", `Unknown option: ${arg}`);
  }
  if (out.apply && args.includes("--dry-run")) fail("APPLE_PROFILE_USAGE", "--dry-run and --apply are mutually exclusive.");
  out.dryRun = !out.apply;
  validateProfileMode(out);
  if (!BUNDLE_PATTERN.test(out.bundleId)) fail("APPLE_PROFILE_USAGE", "--bundle-id must be an explicit bundle identifier (no wildcard).");
  if (!out.name.trim()) fail("APPLE_PROFILE_USAGE", "--name is required.");
  if (out.type === PROFILE_TYPE && !out.containers.length) fail("APPLE_PROFILE_USAGE", "At least one --icloud-container is required.");
  for (const container of out.containers) if (!CONTAINER_PATTERN.test(container)) fail("APPLE_PROFILE_USAGE", `Invalid iCloud container identifier: ${container}`);
  for (const udid of out.registerDevices) if (!UDID_PATTERN.test(udid)) fail("APPLE_PROFILE_USAGE", `Invalid device UDID: ${udid}`);
  if (!out.out) fail("APPLE_PROFILE_USAGE", "--out <profile path> is required.");
  if (out.certificate !== "create" && !/^[A-Za-z0-9]{4,40}$/.test(out.certificate)) fail("APPLE_PROFILE_USAGE", '--certificate must be an App Store Connect certificate id or "create".');
  return out;
}

function validateProfileMode({ type, certificate, containers = [], registerDevices = [], keyOut, expectedTeam, expectedKeychainGroup }) {
  if (![PROFILE_TYPE, DIRECT_TYPE].includes(type)) fail("APPLE_PROFILE_USAGE", `--type must be ${PROFILE_TYPE} or ${DIRECT_TYPE}.`);
  if (type !== DIRECT_TYPE) return;
  if (!/^[A-Z0-9]{10}$/.test(expectedTeam ?? "")) fail("APPLE_PROFILE_USAGE", "Direct profiles require --expected-team <10-character team id>.");
  if (typeof expectedKeychainGroup !== "string" || !expectedKeychainGroup.startsWith(`${expectedTeam}.`) || !BUNDLE_PATTERN.test(expectedKeychainGroup.slice(expectedTeam.length + 1))) fail("APPLE_PROFILE_USAGE", "Direct profiles require one exact --expected-keychain-group prefixed by expected team (no wildcard).");
  if (!/^[A-Za-z0-9]{4,40}$/.test(certificate ?? "") || certificate === "create") fail("APPLE_PROFILE_USAGE", "Direct profiles require --certificate <existing ASC certificate id>.");
  if (containers.length || registerDevices.length || keyOut) fail("APPLE_PROFILE_USAGE", "Direct profiles refuse iCloud containers, device registration & key output.");
}

// Key material must never ride in config, the environment or arguments. Unlike the
// renew command this refusal applies on every host.
export function assertNoInlinePem(env, config, extra = []) {
  const asc = config.appStoreConnect ?? {};
  const named = [["APPLE_API_KEY_PATH", env.APPLE_API_KEY_PATH], ["APPLE_API_KEY", env.APPLE_API_KEY], ["APPLE_API_ISSUER", env.APPLE_API_ISSUER],
    ["appStoreConnect.keyPath", asc.keyPath], ["appStoreConnect.keyId", asc.keyId], ["appStoreConnect.issuer", asc.issuer], ...extra];
  for (const [label, value] of named) {
    if (String(value ?? "").includes(PEM_MARKER)) fail("APPLE_PROFILE_INLINE_PEM_REFUSED", `${label} carries inline key material; supply the .p8 as a file path (appStoreConnect.keyPath or APPLE_API_KEY_PATH).`);
  }
}

// ---- local identities --------------------------------------------------------------

function localDevelopmentIdentities(run, now, { tolerant }) {
  const call = (command, args) => {
    const result = run(command, args, { encoding: "utf8", stdio: "pipe", shell: false, windowsHide: true });
    if (result.error || result.status !== 0) { if (tolerant) return null; fail("APPLE_PROFILE_LOCAL_INSPECTION_FAILED", `${command} failed while inspecting local signing identities.`); }
    return result.stdout;
  };
  const identities = call("security", ["find-identity", "-v", "-p", "codesigning"]);
  const pem = identities === null ? null : call("security", ["find-certificate", "-a", "-p"]);
  if (identities === null || pem === null) return [];
  const valid = new Set([...identities.matchAll(/([a-f0-9]{40})\s+"[^"\n]+"/gi)].map((match) => match[1].toUpperCase()));
  return [...pem.matchAll(/-----BEGIN CERTIFICATE-----[\s\S]*?-----END CERTIFICATE-----/g)].map((match) => {
    const cert = new X509Certificate(match[0]);
    return { sha1: sha1(cert.raw), serialNumber: cert.serialNumber, name: /^CN=(.+)$/m.exec(cert.subject)?.[1], expires: Date.parse(cert.validTo), issued: Date.parse(cert.validFrom) };
  }).filter((cert) => valid.has(cert.sha1) && cert.name?.startsWith("Apple Development:") && cert.issued <= now.getTime() && cert.expires > now.getTime());
}

// ---- main flow ---------------------------------------------------------------------

export async function createAppleProfile({ root, config = {}, bundleId, name, containers = [], type = PROFILE_TYPE, certificate = "create", out, dryRun = true, registerDevices = [], keyOut = "", expectedTeam, expectedKeychainGroup, env = process.env, run = spawnSync, fetch: fetchApi = globalThis.fetch, jwtSigner = signAppleApiJwt, now = new Date(), log = console.log, platform = process.platform, home = os.homedir() }) {
  validateProfileMode({ type, certificate, containers, registerDevices, keyOut, expectedTeam, expectedKeychainGroup });
  if (!BUNDLE_PATTERN.test(bundleId ?? "")) fail("APPLE_PROFILE_USAGE", "bundleId must be an explicit bundle identifier.");
  if (!name?.trim() || !out) fail("APPLE_PROFILE_USAGE", "Profile name & output path are required.");
  if (type === PROFILE_TYPE && !containers?.length) fail("APPLE_PROFILE_USAGE", "At least one iCloud container is required.");
  assertNoInlinePem(env, config, [["--certificate", certificate], ["--name", name]]);
  if (!dryRun && platform !== "darwin") fail("APPLE_PROFILE_PLATFORM", "A real run decodes the issued profile and uses the Keychain; run it on the macOS host.");
  const outFile = path.resolve(root, out);
  const auth = appleProfileAuth(env, { config, root });
  const token = await jwtSigner(auth, now);
  const calls = [], planned = [];

  const request = async (method, endpoint, body) => {
    const url = new URL(endpoint, ASC);
    if (url.origin !== ASC || !url.pathname.startsWith("/v1/")) fail("APPLE_PROFILE_API_INVALID", "ASC URL is outside the API origin.");
    calls.push({ method, path: `${url.pathname}${url.search}` });
    log(`${method} ${decodeURIComponent(url.pathname + url.search)}`);
    if (dryRun && method !== "GET") fail("APPLE_PROFILE_DRY_RUN_MUTATION", "Dry-run cannot mutate ASC.");
    let response;
    try { response = await fetchApi(url.href, { method, headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" }, ...(body ? { body: JSON.stringify(body) } : {}), redirect: "error", signal: AbortSignal.timeout(30_000) }); }
    catch { fail("APPLE_PROFILE_API_FAILED", `ASC ${method} ${url.pathname} request failed.`); }
    if (response.status === 403) { const error = new AppleProfileCreateError("APPLE_PROFILE_INSUFFICIENT_ROLE", "ASC key lacks permission for Certificates, Identifiers & Profiles (403); it needs the Admin or App Manager role."); error.status = 403; throw error; }
    if (!response.ok) {
      let detail = "";
      try { const parsed = await response.json(); detail = String(parsed?.errors?.[0]?.detail ?? parsed?.errors?.[0]?.title ?? "").slice(0, 240); } catch { /* no body */ }
      const error = new AppleProfileCreateError("APPLE_PROFILE_API_FAILED", `ASC ${method} ${url.pathname} returned HTTP ${response.status}${detail ? `: ${detail}` : ""}.`);
      error.status = response.status; throw error;
    }
    if (response.status === 204) return {};
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
  const plan = { dryRun, command: "apple-profile-create", profileType: type, out: outFile, name, calls, planned };
  const mutate = async (method, endpoint, body) => {
    if (dryRun) { planned.push({ method, path: endpoint, ...(body ? { body } : {}) }); log(`PLANNED ${method} ${endpoint}${body ? ` ${JSON.stringify(body)}` : ""}`); return null; }
    return request(method, endpoint, body);
  };

  if (type === DIRECT_TYPE) return createDirectProfile({ bundleId, name, certificate, expectedTeam, expectedKeychainGroup, outFile, dryRun, env, run, now, log, request, list, mutate, plan });

  // (1) bundle id
  const bundles = await list(query("/v1/bundleIds", { "filter[identifier]": bundleId, limit: "200" }));
  let bundle = bundles.find((item) => item.attributes?.identifier === bundleId && ["MAC_OS", "UNIVERSAL"].includes(item.attributes?.platform));
  plan.bundleId = { identifier: bundleId, id: bundle?.id ?? null, action: bundle ? "reuse" : "create" };
  if (!bundle) {
    const result = await mutate("POST", "/v1/bundleIds", { data: { type: "bundleIds", attributes: { identifier: bundleId, name: name.replace(/[^A-Za-z0-9 ]/g, " ").trim() || bundleId, platform: "MAC_OS" } } });
    if (result) { bundle = result.data; plan.bundleId.id = bundle?.id ?? null; if (!bundle?.id) fail("APPLE_PROFILE_API_INVALID", "ASC did not return a created bundleId."); }
  }

  // (2) ICLOUD capability with CloudKit, and the container
  const icloudSettings = [{ key: "ICLOUD_VERSION", options: [{ key: "XCODE_6" }] }];
  const capabilities = bundle ? await list(`/v1/bundleIds/${enc(bundle.id)}/bundleIdCapabilities`) : [];
  const icloud = capabilities.find((item) => item.attributes?.capabilityType === "ICLOUD");
  const cloudKit = icloud?.attributes?.settings?.some((setting) => setting.key === "ICLOUD_VERSION" && setting.options?.some((option) => option.key === "XCODE_6" && option.enabled !== false));
  plan.capability = { type: "ICLOUD", cloudKit: true, id: icloud?.id ?? null, action: !icloud ? "create" : cloudKit ? "reuse" : "update" };
  if (plan.capability.action !== "reuse") {
    try {
      const result = icloud
        ? await mutate("PATCH", `/v1/bundleIdCapabilities/${enc(icloud.id)}`, { data: { type: "bundleIdCapabilities", id: icloud.id, attributes: { capabilityType: "ICLOUD", settings: icloudSettings } } })
        : await mutate("POST", "/v1/bundleIdCapabilities", { data: { type: "bundleIdCapabilities", attributes: { capabilityType: "ICLOUD", settings: icloudSettings }, relationships: { bundleId: { data: { type: "bundleIds", id: bundle?.id ?? "<new bundleId>" } } } } });
      if (result?.data?.id) plan.capability.id = result.data.id;
    } catch (error) {
      if (error.code === "APPLE_PROFILE_API_FAILED") fail("APPLE_PROFILE_CAPABILITY_MISSING", `Could not enable the iCloud/CloudKit capability on ${bundleId} (${error.message})\n${containerManualSteps(bundleId, containers)}`);
      throw error;
    }
  }
  plan.containers = containers.map((identifier) => ({ identifier, assignment: "not-verifiable-via-api", verification: "checked on the issued profile entitlements before it is written" }));

  // (4 first, so a missing device list is found before any certificate is created)
  const devices = await list(query("/v1/devices", { "filter[platform]": "MAC_OS", limit: "200" }));
  const registered = [];
  for (const raw of registerDevices) {
    const udid = raw.toUpperCase();
    const existing = devices.find((item) => String(item.attributes?.udid ?? "").toUpperCase() === udid);
    if (existing) { if (existing.attributes?.status !== "ENABLED") fail("APPLE_PROFILE_DEVICE_DISABLED", `Device ${udid} is registered but disabled in ASC; enable it at developer.apple.com.`); continue; }
    const result = await mutate("POST", "/v1/devices", { data: { type: "devices", attributes: { name: `${name} Mac ${udid.slice(0, 8)}`, platform: "MAC_OS", udid } } });
    registered.push(udid);
    if (result?.data?.id) devices.push(result.data);
  }
  const enabledDevices = devices.filter((item) => item.attributes?.status === "ENABLED" && item.id);
  plan.devices = { enabled: enabledDevices.map((item) => ({ id: item.id, udid: item.attributes.udid })), registeredNow: registered };
  if (!enabledDevices.length && !dryRun) fail("APPLE_PROFILE_NO_DEVICES", "No enabled Mac devices in ASC; pass --register-device <UDID> for this Mac.");
  const deviceIds = [...enabledDevices.map((item) => item.id), ...(dryRun ? registered.map(() => "<new device id>") : [])];

  // (3) Apple Development certificate
  const local = localDevelopmentIdentities(run, now, { tolerant: dryRun });
  let ascCert, mode, generate = false;
  if (certificate !== "create") {
    mode = "explicit";
    ascCert = (await request("GET", `/v1/certificates/${enc(certificate)}`)).data;
    if (!ascCert?.id) fail("APPLE_PROFILE_CERT_NOT_IN_ASC", `Certificate ${certificate} not found in ASC.`);
    if (!["DEVELOPMENT", "MAC_APP_DEVELOPMENT"].includes(ascCert.attributes?.certificateType)) fail("APPLE_PROFILE_CERT_TYPE", `Certificate ${certificate} is ${ascCert.attributes?.certificateType}, not an Apple Development certificate.`);
    if (!(Date.parse(ascCert.attributes?.expirationDate) > now.getTime())) fail("APPLE_PROFILE_CERT_EXPIRED", `Certificate ${certificate} is expired.`);
  } else {
    const ascCerts = (await list(query("/v1/certificates", { "filter[certificateType]": "DEVELOPMENT", limit: "200" })))
      .filter((item) => item.id && Date.parse(item.attributes?.expirationDate) > now.getTime());
    const usable = ascCerts.map((item) => ({ item, identity: local.find((cert) => serial(cert.serialNumber) === serial(item.attributes?.serialNumber)) })).filter((entry) => entry.identity)
      .sort((a, b) => b.identity.expires - a.identity.expires || a.item.id.localeCompare(b.item.id));
    if (usable.length) { mode = "reuse"; ascCert = usable[0].item; }
    else { mode = "create"; generate = true; }
  }
  const certBytes = ascCert?.attributes?.certificateContent ? Buffer.from(ascCert.attributes.certificateContent, "base64") : null;
  const certSha1 = certBytes ? sha1(certBytes) : null;
  const localIdentity = certSha1 ? local.find((cert) => cert.sha1 === certSha1) : undefined;
  plan.certificate = { mode, id: ascCert?.id ?? null, serialNumber: ascCert?.attributes?.serialNumber ?? null, sha1: certSha1, name: localIdentity?.name ?? ascCert?.attributes?.name ?? null, privateKeyInKeychain: !!localIdentity, keyStore: generate ? (keyOut ? "file" : "login-keychain") : null };
  if (mode === "explicit" && !localIdentity) log(`WARNING: certificate ${certificate} has no valid private-key identity in the local Keychain; the profile will authorize it but this Mac cannot sign with it.`);
  if (generate) {
    const commonName = `${name} Apple Development`;
    if (dryRun) {
      planned.push({ method: "LOCAL", path: `generate RSA-2048 key + CSR (CN=${commonName}); store key in ${keyOut ? "file --key-out" : "login Keychain"}` });
      planned.push({ method: "POST", path: "/v1/certificates", body: { data: { type: "certificates", attributes: { certificateType: "DEVELOPMENT", csrContent: "<PEM CSR>" } } } });
      log("PLANNED local RSA-2048 key + CSR, then POST /v1/certificates certificateType=DEVELOPMENT");
    } else {
      const created = await createCertificate({ commonName, request, run, keyOut: keyOut ? path.resolve(root, keyOut) : "", home, log });
      ascCert = created.cert; plan.certificate = { ...plan.certificate, id: ascCert.id, serialNumber: ascCert.attributes?.serialNumber, sha1: created.sha1, name: created.name, privateKeyInKeychain: !keyOut };
    }
  }
  const certificateSha1 = plan.certificate.sha1;

  // (5) profile
  const profiles = bundle && ascCert ? await list(query("/v1/profiles", { "filter[name]": name, "filter[profileType]": PROFILE_TYPE, limit: "200" })) : [];
  let profile = profiles.find((item) => item.attributes?.name === name);
  let content = null, createdNow = false;
  if (profile) {
    const [bundleRel, certRel, deviceRel] = await Promise.all([
      request("GET", `/v1/profiles/${enc(profile.id)}/bundleId`), list(`/v1/profiles/${enc(profile.id)}/certificates`), list(`/v1/profiles/${enc(profile.id)}/devices`)]).then(([a, b, c]) => [a.data, b, c]);
    const satisfied = profile.attributes?.profileState === "ACTIVE" && Date.parse(profile.attributes?.expirationDate) > now.getTime() && bundleRel?.id === bundle.id
      && certRel.some((item) => item.id === ascCert.id) && deviceRel.length >= enabledDevices.length && enabledDevices.every((device) => deviceRel.some((item) => item.id === device.id));
    if (!satisfied) fail("APPLE_PROFILE_NAME_CONFLICT", `A profile named ${name} already exists but is not an active ${PROFILE_TYPE} profile for ${bundleId} covering this certificate and every Mac device. Delete it at developer.apple.com or pass a new --name.`);
    plan.profile = { id: profile.id, action: "reuse" };
    content = profile.attributes?.profileContent ?? (await request("GET", `/v1/profiles/${enc(profile.id)}`)).data?.attributes?.profileContent;
  } else {
    plan.profile = { id: null, action: "create" };
    const body = { data: { type: "profiles", attributes: { name, profileType: PROFILE_TYPE }, relationships: {
      bundleId: { data: { type: "bundleIds", id: bundle?.id ?? "<new bundleId>" } },
      certificates: { data: [{ type: "certificates", id: ascCert?.id ?? "<new certificate id>" }] },
      devices: { data: deviceIds.map((id) => ({ type: "devices", id })) } } } };
    const result = await mutate("POST", "/v1/profiles", body);
    if (result) {
      profile = result.data; createdNow = true;
      if (!profile?.id) fail("APPLE_PROFILE_API_INVALID", "ASC did not return a created profile id.");
      plan.profile.id = profile.id;
      content = profile.attributes?.profileContent ?? (await request("GET", `/v1/profiles/${enc(profile.id)}`)).data?.attributes?.profileContent;
    }
  }
  plan.verification = "On apply the issued profile is decoded and must carry the bundle id, get-task-allow, CloudKit, every requested container, the certificate and every enabled Mac device.";
  log(JSON.stringify({ ...plan, calls: undefined }, null, 2));
  if (dryRun) { plan.writes = `would write ${outFile} (0600)`; log(`PLANNED write ${outFile} with mode 0600`); return plan; }

  // verify, then write 0600
  if (typeof content !== "string" || !/^[A-Za-z0-9+/]+={0,2}$/.test(content)) fail("APPLE_PROFILE_API_INVALID", "ASC profileContent is missing or invalid.");
  mkdirSync(path.dirname(outFile), { recursive: true });
  const temporary = `${outFile}.create-${randomUUID()}.tmp`;
  try {
    writeFileSync(temporary, Buffer.from(content, "base64"), { flag: "wx", mode: 0o600 });
    const problems = verifyIssuedProfile(decodeAppleProfile(temporary, { run }), { bundleId, containers, certificateSha1, deviceUdids: enabledDevices.map((item) => item.attributes.udid), now });
    if (problems.containers.length) {
      if (createdNow) { try { await request("DELETE", `/v1/profiles/${enc(profile.id)}`); } catch { /* best effort; reported below */ } }
      fail("APPLE_PROFILE_ICLOUD_CONTAINER_UNASSIGNED", `The issued profile does not carry iCloud container(s) ${problems.containers.join(", ")}${createdNow ? " (the profile just created was deleted again)" : ""}.\n${containerManualSteps(bundleId, containers)}`);
    }
    if (problems.other.length) fail("APPLE_PROFILE_VERIFY_FAILED", `Issued profile ${profile.id} failed verification: ${problems.other.join("; ")}. Nothing was written to ${outFile}.`);
    if (existsSync(outFile)) copyFileSync(outFile, `${outFile}.backup-${now.toISOString().replace(/[:.]/g, "-")}-${randomUUID()}`, constants.COPYFILE_EXCL);
    renameSync(temporary, outFile);
    chmodSync(outFile, 0o600);
  } finally { rmSync(temporary, { force: true }); }
  log(`Wrote ${outFile}; use HEARDRIGHT_MAC_DEV_SIGN_IDENTITY=${certificateSha1}`);
  return { ...plan, profileId: profile.id, identitySha1: certificateSha1 };
}

// Direct profiles authorize one selected existing Developer ID identity. No device,
// capability, certificate or key creation is reachable from this branch.
async function createDirectProfile({ bundleId, name, certificate, expectedTeam, expectedKeychainGroup, outFile, dryRun, env, run, now, log, request, list, mutate, plan }) {
  const ascCert = (await request("GET", `/v1/certificates/${enc(certificate)}`)).data;
  if (!ascCert?.id || ascCert.id !== certificate) fail("APPLE_PROFILE_CERT_NOT_IN_ASC", "Selected certificate is absent from ASC.");
  if (!["DEVELOPER_ID_APPLICATION", "DEVELOPER_ID_APPLICATION_G2"].includes(ascCert.attributes?.certificateType)) fail("APPLE_PROFILE_CERT_TYPE", "Direct profiles require an existing DEVELOPER_ID_APPLICATION or DEVELOPER_ID_APPLICATION_G2 certificate.");
  if (!(Date.parse(ascCert.attributes?.expirationDate) > now.getTime())) fail("APPLE_PROFILE_CERT_EXPIRED", "Selected ASC certificate is expired.");
  let leaf;
  try { leaf = new X509Certificate(Buffer.from(ascCert.attributes?.certificateContent ?? "", "base64")); }
  catch { fail("APPLE_PROFILE_API_INVALID", "Selected ASC certificate has no valid public DER certificate."); }
  const certificateSha1 = sha1(leaf.raw);
  const identity = findAppleSigningCertificate({ type: DIRECT_TYPE, team: expectedTeam }, { env: { ...env, APPLE_SIGNING_IDENTITY: certificateSha1 }, run, now });
  if (identity.sha1 !== certificateSha1 || serial(identity.serialNumber) !== serial(ascCert.attributes?.serialNumber)) fail("APPLE_PROFILE_CERT_MISMATCH", "Selected ASC certificate does not match local Developer ID identity.");
  plan.teamIdentifier = expectedTeam;
  plan.expectedKeychainGroup = expectedKeychainGroup;
  plan.certificate = { ...identity, id: ascCert.id, mode: "explicit", privateKeyInKeychain: true };

  const bundles = await list(query("/v1/bundleIds", { "filter[identifier]": bundleId, limit: "200" }));
  const matching = bundles.filter((item) => item.attributes?.identifier === bundleId);
  if (matching.length > 1 || (matching.length && !["MAC_OS", "UNIVERSAL"].includes(matching[0].attributes?.platform))) fail("APPLE_PROFILE_BUNDLE_CONFLICT", "Existing bundle identifier has conflicting platform or duplicate resources.");
  let bundle = matching[0];
  // Look across types so a dedicated name is never silently duplicated.
  const profiles = (await list(query("/v1/profiles", { "filter[name]": name, limit: "200" }))).filter((item) => item.attributes?.name === name);
  if (profiles.length > 1) fail("APPLE_PROFILE_NAME_CONFLICT", "More than one profile already uses this dedicated name.");
  let profile = profiles[0];
  if (profile) {
    const [bundleRel, certRel] = await Promise.all([request("GET", `/v1/profiles/${enc(profile.id)}/bundleId`), list(`/v1/profiles/${enc(profile.id)}/certificates`)]);
    if (!bundle?.id || profile.attributes?.profileType !== DIRECT_TYPE || profile.attributes?.profileState !== "ACTIVE" || !(Date.parse(profile.attributes?.expirationDate) > now.getTime())
      || bundleRel.data?.id !== bundle.id || certRel.length !== 1 || certRel[0].id !== ascCert.id) fail("APPLE_PROFILE_NAME_CONFLICT", "Existing named profile does not match direct type, bundle & selected certificate.");
  }
  plan.bundleId = { identifier: bundleId, id: bundle?.id ?? null, action: bundle ? "reuse" : "create" };
  if (!bundle) {
    const result = await mutate("POST", "/v1/bundleIds", { data: { type: "bundleIds", attributes: { identifier: bundleId, name: name.replace(/[^A-Za-z0-9 ]/g, " ").trim() || bundleId, platform: "MAC_OS" } } });
    if (result) {
      bundle = result.data;
      if (!bundle?.id || bundle.attributes?.identifier !== bundleId || !["MAC_OS", "UNIVERSAL"].includes(bundle.attributes?.platform)) fail("APPLE_PROFILE_API_INVALID", "ASC returned an unexpected created bundle identifier.");
      plan.bundleId.id = bundle.id;
    }
  }
  plan.profile = { id: profile?.id ?? null, action: profile ? "reuse" : "create" };
  if (!profile) {
    const body = { data: { type: "profiles", attributes: { name, profileType: DIRECT_TYPE }, relationships: {
      bundleId: { data: { type: "bundleIds", id: bundle?.id ?? "<new bundleId>" } }, certificates: { data: [{ type: "certificates", id: ascCert.id }] } } } };
    const result = await mutate("POST", "/v1/profiles", body);
    if (result) {
      profile = result.data;
      if (!profile?.id) fail("APPLE_PROFILE_API_INVALID", "ASC did not return a created profile id.");
      plan.profile.id = profile.id;
    }
  }
  plan.verification = "Exact team, application identifier, OSX, expiry, all devices, debugging disabled, selected DER fingerprint & exact keychain group authorization; app must request only its exact group.";
  log(JSON.stringify({ ...plan, calls: undefined }, null, 2));
  if (dryRun) { plan.writes = `would write ${outFile} (0600)`; return plan; }
  const content = profile.attributes?.profileContent ?? (await request("GET", `/v1/profiles/${enc(profile.id)}`)).data?.attributes?.profileContent;
  if (typeof content !== "string" || !content || !/^[A-Za-z0-9+/]+={0,2}$/.test(content)) fail("APPLE_PROFILE_API_INVALID", "ASC profileContent is missing or invalid.");
  mkdirSync(path.dirname(outFile), { recursive: true });
  const temporary = `${outFile}.create-${randomUUID()}.tmp`;
  try {
    writeFileSync(temporary, Buffer.from(content, "base64"), { flag: "wx", mode: 0o600 });
    verifyDirectProfile(decodeAppleProfile(temporary, { run }), { bundleId, name, expectedTeam, expectedKeychainGroup, certificateSha1, now });
    if (existsSync(outFile)) copyFileSync(outFile, `${outFile}.backup-${now.toISOString().replace(/[:.]/g, "-")}-${randomUUID()}`, constants.COPYFILE_EXCL);
    renameSync(temporary, outFile);
    chmodSync(outFile, 0o600);
  } finally { rmSync(temporary, { force: true }); }
  log(`Wrote ${outFile}; Developer ID identity SHA-1 ${certificateSha1}`);
  return { ...plan, profileId: profile.id, identitySha1: certificateSha1 };
}

export function verifyDirectProfile(decoded, { bundleId, name, expectedTeam, expectedKeychainGroup, certificateSha1, now }) {
  const problems = [], entitlements = decoded.Entitlements ?? {};
  if (decoded.Name !== name) problems.push("profile name mismatch");
  if (!Array.isArray(decoded.TeamIdentifier) || decoded.TeamIdentifier.length !== 1 || decoded.TeamIdentifier[0] !== expectedTeam) problems.push("team mismatch");
  if (entitlements["com.apple.developer.team-identifier"] !== expectedTeam) problems.push("entitlement team mismatch");
  const appKeys = ["com.apple.application-identifier", "application-identifier"].filter((key) => Object.hasOwn(entitlements, key));
  if (!appKeys.length || appKeys.some((key) => entitlements[key] !== `${expectedTeam}.${bundleId}`)) problems.push("application identifier mismatch");
  if (!Array.isArray(decoded.Platform) || !decoded.Platform.includes("OSX")) problems.push("not a macOS profile");
  if (!(Date.parse(decoded.ExpirationDate) > now.getTime())) problems.push("expired or missing expiration");
  if (decoded.ProvisionsAllDevices !== true) problems.push("not an all-devices direct profile");
  const debugKeys = ["get-task-allow", "com.apple.security.get-task-allow"].filter((key) => Object.hasOwn(entitlements, key));
  // Profiles carry an entitlement allowlist (Apple TN3125). Direct profiles may
  // omit debugging entirely; every present alias must still be boolean false.
  if (debugKeys.some((key) => entitlements[key] !== false)) problems.push("debugging is not disabled");
  if (!certificateSha1 || !(decoded.DeveloperCertificates ?? []).some((data) => sha1(Buffer.from(data, "base64")) === certificateSha1)) problems.push("selected certificate not authorized");
  const groups = entitlements["keychain-access-groups"];
  // Apple-issued terminal wildcards may authorize an exact app group. This never
  // changes the app's requested entitlements or permits wildcard CLI input.
  if (!Array.isArray(groups) || !groups.some((group) => group === expectedKeychainGroup || (typeof group === "string" && group.startsWith(`${expectedTeam}.`) && /^[^*]+\*$/u.test(group) && expectedKeychainGroup.startsWith(group.slice(0, -1))))) problems.push("keychain group not authorized");
  if (problems.length) fail("APPLE_PROFILE_VERIFY_FAILED", `Direct profile failed verification: ${problems.join("; ")}.`);
}

export function verifyIssuedProfile(decoded, { bundleId, containers, certificateSha1, deviceUdids, now }) {
  const other = [], missing = [];
  const entitlements = decoded.Entitlements ?? {};
  const appId = entitlements["com.apple.application-identifier"] ?? entitlements["application-identifier"];
  if (!Array.isArray(decoded.Platform) || !decoded.Platform.includes("OSX")) other.push("not a macOS profile");
  if (typeof appId !== "string" || !appId.endsWith(`.${bundleId}`)) other.push("application identifier does not match the bundle id");
  if ((entitlements["com.apple.security.get-task-allow"] ?? entitlements["get-task-allow"]) !== true) other.push("not a development profile (get-task-allow)");
  if (!(Date.parse(decoded.ExpirationDate) > now.getTime())) other.push("expired or no expiration date");
  if (certificateSha1 && !(decoded.DeveloperCertificates ?? []).some((data) => sha1(Buffer.from(data, "base64")) === certificateSha1)) other.push("certificate not authorized");
  const provisioned = new Set((decoded.ProvisionedDevices ?? []).map((udid) => String(udid).toUpperCase()));
  for (const udid of deviceUdids) if (!provisioned.has(String(udid).toUpperCase())) other.push(`device ${udid} not provisioned`);
  if (!(entitlements["com.apple.developer.icloud-services"] ?? []).includes("CloudKit")) missing.push("CloudKit service");
  const carried = new Set([...(entitlements["com.apple.developer.icloud-container-identifiers"] ?? []), ...(entitlements["com.apple.developer.icloud-container-development-container-identifiers"] ?? [])]);
  for (const container of containers) if (!carried.has(container)) missing.push(container);
  return { other, containers: missing };
}

async function createCertificate({ commonName, request, run, keyOut, home, log }) {
  const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
  const csr = buildCsr(commonName, { privateKey, publicKey });
  const keychain = path.join(home, "Library", "Keychains", "login.keychain-db");
  const scratch = mkdtempSync(path.join(os.tmpdir(), "rk-apple-dev-"));
  const local = (args) => {
    const result = run("security", args, { encoding: "utf8", stdio: "pipe", shell: false, windowsHide: true });
    if (result.error || result.status !== 0) fail("APPLE_PROFILE_KEYCHAIN_FAILED", `security ${args[0]} failed; stopped before any further change.`);
    return result.stdout;
  };
  try {
    // The key is stored BEFORE the certificate is requested, so a failure can never
    // leave a certificate in ASC whose private key is lost. Key bytes are never logged.
    const keyFile = keyOut || path.join(scratch, "key.pem");
    writeFileSync(keyFile, privateKey.export({ type: "pkcs1", format: "pem" }), { mode: 0o600, flag: keyOut ? "wx" : "w" });
    if (!keyOut) {
      if (!existsSync(keychain)) fail("APPLE_PROFILE_KEYCHAIN_FAILED", `Login Keychain not found at ${keychain}.`);
      local(["import", keyFile, "-k", keychain, "-t", "priv", "-f", "openssl", "-T", "/usr/bin/codesign"]);
      rmSync(keyFile, { force: true });
    } else log(`Private key written to ${keyFile} (0600); import it into a Keychain yourself.`);
    const created = (await request("POST", "/v1/certificates", { data: { type: "certificates", attributes: { certificateType: "DEVELOPMENT", csrContent: csr.pem } } })).data;
    const content = created?.attributes?.certificateContent;
    if (!created?.id || typeof content !== "string" || !/^[A-Za-z0-9+/]+={0,2}$/.test(content)) fail("APPLE_PROFILE_API_INVALID", "ASC did not return a usable certificate.");
    const der = Buffer.from(content, "base64"), cert = new X509Certificate(der);
    const certFile = keyOut ? `${keyOut}.cer` : path.join(scratch, "cert.cer");
    writeFileSync(certFile, der, { mode: 0o600, flag: keyOut ? "wx" : "w" });
    const sha = sha1(der), commonSubject = /^CN=(.+)$/m.exec(cert.subject)?.[1];
    if (!keyOut) {
      local(["import", certFile, "-k", keychain, "-t", "cert", "-f", "x509"]);
      if (!new RegExp(sha, "i").test(local(["find-identity", "-v", "-p", "codesigning"]))) fail("APPLE_PROFILE_KEYCHAIN_FAILED", `Certificate ${sha} was imported but is not a valid code-signing identity (is the Apple WWDR intermediate installed?). Certificate id ${created.id} exists in ASC.`);
    }
    log(`Created Apple Development certificate ${created.id} (${commonSubject ?? "unnamed"}) sha1 ${sha}`);
    return { cert: created, sha1: sha, name: commonSubject };
  } finally { rmSync(scratch, { recursive: true, force: true }); }
}

export async function main(args = process.argv.slice(2), options = {}) {
  if (args.includes("--help") || args.includes("-h")) {
    (options.log ?? console.log)("right-release apple-profile-create --bundle-id <id> --name <profile name> --out <profile path>\n  [--config <file>] [--type MAC_APP_DEVELOPMENT|MAC_APP_DIRECT] [--certificate <asc cert id>|create] [--dry-run | --apply]\nDevelopment: --icloud-container <iCloud.x>... [--register-device <UDID>]... [--key-out <file>]\nDirect: --certificate <existing ASC Developer ID cert id> --expected-team <team id> --expected-keychain-group <exact group>; refuses iCloud, devices & key output.\nDefault is --dry-run (GET discovery only). Uses APPLE_API_ISSUER, APPLE_API_KEY & APPLE_API_KEY_PATH or appStoreConnect.* in the config; inline PEM is refused.\nThe ASC API cannot create or assign iCloud containers; the issued profile is verified and the command prints manual steps if the container is missing.");
    return;
  }
  const parsed = parseCreateArgs(args);
  const configPath = parsed.configPath ?? "right-release.config.mjs";
  const absolute = path.resolve(configPath);
  let config = {};
  if (existsSync(absolute)) { const loaded = await import(pathToFileURL(absolute)); config = loaded.default ?? loaded.config ?? loaded; }
  else if (parsed.configPath) fail("APPLE_PROFILE_USAGE", `Config file not found: ${parsed.configPath}`);
  return createAppleProfile({ ...options, root: path.dirname(absolute), config, ...parsed });
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  main().catch((error) => { console.error(`right-release apple-profile-create: [${error.code ?? "APPLE_PROFILE_FAILED"}] ${error instanceof AppleProfileCreateError ? error.message : "Profile creation failed; check app configuration & local file access."}`); process.exitCode = 1; });
}
