import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";

const SAFE_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const ROLES = new Set(["command", "service", "icon", "license", "eula", "privacy", "third-party-notices"]);
const LEGAL_ROLES = new Set(["license", "eula", "privacy", "third-party-notices"]);
const PLATFORM = new Set(["mac", "win"]);

export function validateAddonConfig(config, { root, platform, allowMissingExecutables = false }) {
  if (!PLATFORM.has(platform)) throw new Error("add-on platform must be mac or win");
  if (!config || config.schema !== 1 || config.kind !== "headless-addon") throw new Error("add-on config must use schema 1 headless-addon");
  for (const key of ["addon", "version", "packageManager"]) if (typeof config[key] !== "string" || !SAFE_NAME.test(config[key])) throw new Error(`unsafe add-on ${key}`);
  if (!Array.isArray(config.checks)) throw new Error("add-on checks must be an array");
  validateBuildInputs(config.buildInputs);
  const target = config.targets?.[platform];
  if (!target?.targetTriple || !target?.build?.cmd || !Array.isArray(target.build.args)) throw new Error(`add-on ${platform} target requires targetTriple and build command`);
  if (!target.signing?.contract) throw new Error(`add-on ${platform} target requires signing contract`);
  if (platform === "mac" && target.signing.contract === "apple-developer-id-executable-v1" && !/^[A-Z0-9]{10}$/.test(target.signing.teamId ?? "")) throw new Error("mac add-on requires Developer ID teamId");
  const files = resolveAddonFiles(config, root, platform);
  const seenRoles = new Set();
  const seenNames = new Set();
  for (const file of files) {
    if (!ROLES.has(file.role) || seenRoles.has(file.role)) throw new Error(`duplicate or unsafe add-on role: ${file.role}`);
    if (!SAFE_NAME.test(file.name) || file.name.includes("..")) throw new Error(`unsafe add-on file name: ${file.name}`);
    if (path.isAbsolute(file.source) || file.source.split(/[\\/]/).includes("..")) throw new Error(`unsafe add-on source: ${file.source}`);
    if (seenNames.has(file.name)) throw new Error(`duplicate add-on file name: ${file.name}`);
    if (!existsSync(path.resolve(root, file.source)) && !(allowMissingExecutables && file.executable === true)) throw new Error(`missing add-on file: ${file.source}`);
    seenRoles.add(file.role); seenNames.add(file.name);
  }
  for (const role of ["command", "service", "icon", ...LEGAL_ROLES]) if (!seenRoles.has(role)) throw new Error(`missing required add-on role: ${role}`);
  return { target, files };
}

export function resolveAddonFiles(config, root, platform) {
  const configured = config.targets?.[platform]?.files ?? config.files;
  if (!Array.isArray(configured)) throw new Error("add-on config requires files with role, name, and source");
  return configured.map((file) => ({ ...file, executable: file.executable === true, source: String(file.source ?? "") }));
}

export function createAddonManifest({ config, root, platform, commit, signedAt = new Date().toISOString(), signing = {} }) {
  const { target, files } = validateAddonConfig(config, { root, platform });
  if (!/^[0-9a-f]{40}$/i.test(commit)) throw new Error("add-on commit must be a full Git SHA");
  const manifestFiles = files.map((file) => {
    const source = path.resolve(root, file.source);
    const executable = file.executable === true;
    if ((file.role === "command" || file.role === "service") && !executable) throw new Error(`executable role must declare executable: ${file.role}`);
    const signature = signing[file.role] ?? (executable ? { contract: target.signing.contract, status: "verified" } : null);
    if (executable && (!signature || signature.status !== "verified")) throw new Error(`unsigned executable role: ${file.role}`);
    return { role: file.role, name: file.name, sha256: sha256(source), sizeBytes: statSync(source).size, executable, signing: signature };
  });
  const manifest = { schema: 1, kind: "headless-addon", addon: config.addon, version: config.version, commit, platform, targetTriple: target.targetTriple, consumer: config.consumer, files: manifestFiles, signedAt };
  validateAddonManifest(manifest);
  return manifest;
}

export function validateAddonManifest(manifest) {
  const expected = ["schema", "kind", "addon", "version", "commit", "platform", "targetTriple", "consumer", "files", "signedAt"];
  assertExactKeys(manifest, expected, "add-on manifest");
  if (manifest.schema !== 1 || manifest.kind !== "headless-addon" || !SAFE_NAME.test(manifest.addon) || !SAFE_NAME.test(manifest.version)) throw new Error("invalid add-on manifest identity");
  if (!PLATFORM.has(manifest.platform) || !/^[0-9a-f]{40}$/i.test(manifest.commit) || !Array.isArray(manifest.files)) throw new Error("invalid add-on manifest platform or files");
  const roles = new Set(); const names = new Set();
  for (const file of manifest.files) {
    assertExactKeys(file, ["role", "name", "sha256", "sizeBytes", "executable", "signing"], "add-on file");
    if (!ROLES.has(file.role) || roles.has(file.role) || !SAFE_NAME.test(file.name) || names.has(file.name)) throw new Error("duplicate or unsafe add-on manifest file");
    if (!/^[0-9a-f]{64}$/i.test(file.sha256) || !Number.isSafeInteger(file.sizeBytes) || file.sizeBytes < 0 || typeof file.executable !== "boolean") throw new Error("invalid add-on file integrity");
    if (file.executable && (!file.signing || file.signing.status !== "verified")) throw new Error(`unsigned executable role: ${file.role}`);
    roles.add(file.role); names.add(file.name);
  }
  for (const role of ["command", "service", "icon", ...LEGAL_ROLES]) if (!roles.has(role)) throw new Error(`missing required add-on role: ${role}`);
  return manifest;
}

export function canonicalAddonManifest(manifest) { validateAddonManifest(manifest); return `${JSON.stringify(manifest)}\n`; }
export function addonManifestSha256(manifest) { return createHash("sha256").update(canonicalAddonManifest(manifest)).digest("hex"); }
export function addonRoutes(manifest) {
  const digest = addonManifestSha256(manifest);
  const root = `${manifest.addon}/addons/${manifest.platform}/sha256/${digest}`;
  return [...manifest.files.map((file) => ({ name: file.name, key: `${root}/${file.name}`, sha256: file.sha256, manifest: false })), { name: "addon-manifest.json", key: `${root}/addon-manifest.json`, sha256: digest, manifest: true }];
}

export async function downloadImmutableAddOn(manifestUrl, destination, { fetchImpl = fetch } = {}) {
  if (!/^https:\/\//.test(manifestUrl) || !manifestUrl.endsWith("/addon-manifest.json") || /[?#]/.test(manifestUrl)) throw new Error("add-on lock manifestUrl must be an immutable HTTPS manifest URL");
  const download = async (url, file) => {
    const response = await fetchImpl(url, { cache: "no-store" });
    if (!response.ok) throw new Error(`immutable add-on download failed: ${response.status} ${url}`);
    writeFileSync(file, Buffer.from(await response.arrayBuffer()));
  };
  await download(manifestUrl, path.join(destination, "addon-manifest.json"));
  const manifest = JSON.parse(readFileSync(path.join(destination, "addon-manifest.json"), "utf8"));
  validateAddonManifest(manifest);
  const base = manifestUrl.slice(0, -"addon-manifest.json".length);
  for (const file of manifest.files) await download(`${base}${encodeURIComponent(file.name)}`, path.join(destination, file.name));
}

function validateBuildInputs(inputs) {
  if (!Array.isArray(inputs?.include) || !inputs.include.length || inputs.include.some((entry) => typeof entry !== "string" || path.isAbsolute(entry) || entry.split(/[\\/]/).includes(".."))) throw new Error("add-on buildInputs.include must be safe and non-empty");
}
function sha256(file) { return createHash("sha256").update(readFileSync(file)).digest("hex"); }
function assertExactKeys(value, keys, label) { if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).sort().join("\0") !== [...keys].sort().join("\0")) throw new Error(`${label} has unsupported fields`); }
