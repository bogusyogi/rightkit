#!/usr/bin/env node
// Private-app Linux publication and presigned URLs (rightkit#1230; Pulse guest link, ADR-0004 I1: a pinned sha256, no signature scheme).
//
//   right-release linux-publish publish --app <slug> --out-dir <.right-release/linux> --approve <manifest sha256> [--dry-run]
//   right-release linux-publish presign --app <slug> --version <v> --arch <x86_64|aarch64> [--expires <seconds, 60..86400>]
//
// publish   reads `linux-artifacts.json` (linux-artifacts.mjs: one tarball per native architecture with sha256, size and glibc floor), re-hashes every
//           tarball against it, and uploads tarballs and the manifest to the PRIVATE R2 bucket under `<app>/linux/<version>/`. Each upload is read back
//           through the streamed sha256 and size check (r2-verified-upload.mjs), so a remote object that differs from the manifest is refused.
//           Publication of any build needs Adrian's go naming it: `--approve` takes the sha256 of the manifest file he named, and a manifest with any
//           other digest is refused before anything leaves the host. Only the local release host that built the artifacts runs this.
// presign   prints, as JSON, a time-limited GET URL for ONE artifact together with its pinned sha256, size and glibc floor, so Pulse can put them in a
//           one-use guest pass. The URL is the only place a signature appears; credentials are derived in memory from the Cloudflare token (the
//           credential store, never an environment value printed) and never written anywhere.
// Native builds only (aarch64 on a Linux VM on the Mac, x86_64 on a Linux VM on the Dell): the manifest holds whichever architectures were packed.
import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { LINUX_ARCHES, LINUX_ARTIFACTS_SCHEMA } from "./linux-artifacts.mjs";
import { PublicationError } from "./publication-artifacts.mjs";
import { resolveCloudflareApiToken } from "./protected-credentials.mjs";
import { deriveR2S3Credentials, presignGetUrl } from "./r2-s3-multipart.mjs";
import { uploadVerifiedR2File } from "./r2-verified-upload.mjs";

export const PRIVATE_LINUX_BUCKET = "rightapps-updates";
const SLUG = /^[a-z][a-z0-9-]{1,40}$/;
const SEMVER = /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/;
const HEX64 = /^[0-9a-f]{64}$/;

export class LinuxPublishError extends Error {
  constructor(message) { super(message); this.name = "LinuxPublishError"; this.code = "RK_LINUX_PUBLISH"; }
}
const fail = (message) => { throw new LinuxPublishError(message); };

/** The R2 key of one object of a Linux release: `<app>/linux/<version>/<file>`. */
export function linuxObjectKey(app, version, file) {
  if (!SLUG.test(app ?? "")) fail(`app ${JSON.stringify(app)} is not an app slug`);
  if (!SEMVER.test(version ?? "")) fail(`version ${JSON.stringify(version)} is not semver`);
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(file ?? "") || file.includes("..")) fail(`file ${JSON.stringify(file)} is not a plain file name`);
  return `${app}/linux/${version}/${file}`;
}

/** Read and validate `linux-artifacts.json`: its schema, app, version, commit, and every architecture entry. Returns { manifest, manifestPath, sha256 }. */
export function readLinuxManifest({ outDir, app, fs = { existsSync, readFileSync, statSync } }) {
  const manifestPath = path.resolve(outDir, "linux-artifacts.json");
  if (!fs.existsSync(manifestPath)) fail(`no linux-artifacts.json in ${path.resolve(outDir)}; pack the Linux artifacts first (development.targets.linux.archive)`);
  const bytes = fs.readFileSync(manifestPath);
  let manifest;
  try { manifest = JSON.parse(String(bytes)); } catch (error) { fail(`linux-artifacts.json is not JSON: ${error.message}`); }
  if (manifest?.schema !== LINUX_ARTIFACTS_SCHEMA) fail(`linux-artifacts.json schema is ${JSON.stringify(manifest?.schema)}, expected ${LINUX_ARTIFACTS_SCHEMA}`);
  if (manifest.app !== app) fail(`linux-artifacts.json is for ${JSON.stringify(manifest.app)}, not ${JSON.stringify(app)}`);
  if (!SEMVER.test(manifest.version ?? "") || !/^[0-9a-f]{40}$/.test(manifest.commit ?? "")) fail("linux-artifacts.json has no valid version and commit");
  const arches = Object.keys(manifest.artifacts ?? {});
  if (!arches.length || arches.some((arch) => !LINUX_ARCHES.includes(arch))) fail(`linux-artifacts.json must list one or more of ${LINUX_ARCHES.join(", ")}`);
  for (const arch of arches) {
    const entry = manifest.artifacts[arch];
    if (!entry || typeof entry.file !== "string" || !HEX64.test(entry.sha256 ?? "") || !Number.isSafeInteger(entry.size) || entry.size <= 0) fail(`linux-artifacts.json ${arch} needs file, sha256 and size`);
  }
  return { manifest, manifestPath, sha256: createHash("sha256").update(bytes).digest("hex") };
}

/** Publish every architecture of the manifest. `approve` must equal the manifest's sha256 (Adrian's named build). Returns { keys, version }. */
export async function publishLinuxArtifacts({ outDir, app, approve, dryRun = false, env = process.env, credentialStore, upload = uploadVerifiedR2File, fs = { existsSync, readFileSync, statSync }, log = (line) => console.log(line) }) {
  if (!HEX64.test(approve ?? "")) fail("--approve must be the sha256 of the linux-artifacts.json Adrian named (publication of any build needs his go naming it)");
  const { manifest, manifestPath, sha256 } = readLinuxManifest({ outDir, app, fs });
  if (sha256 !== approve) fail(`linux-artifacts.json is ${sha256}, not the approved ${approve}; nothing was uploaded`);
  const root = path.resolve(outDir);
  const planned = Object.values(manifest.artifacts).map((entry) => {
    const file = path.join(root, entry.file);
    if (!fs.existsSync(file)) fail(`${entry.file} is missing next to the manifest`);
    const bytes = fs.readFileSync(file);
    const actual = createHash("sha256").update(bytes).digest("hex");
    if (actual !== entry.sha256 || bytes.length !== entry.size) fail(`${entry.file} is ${actual} (${bytes.length} bytes), the manifest says ${entry.sha256} (${entry.size}); nothing was uploaded`);
    return { file, evidence: { sha256: entry.sha256, sizeBytes: entry.size }, key: linuxObjectKey(app, manifest.version, entry.file) };
  });
  planned.push({ file: manifestPath, evidence: { sha256, sizeBytes: statSync(manifestPath).size }, key: linuxObjectKey(app, manifest.version, "linux-artifacts.json") });
  if (dryRun) { for (const item of planned) log(`right-release linux-publish: dry-run ${PRIVATE_LINUX_BUCKET}/${item.key}`); return { version: manifest.version, keys: planned.map((item) => item.key), dryRun: true }; }
  const token = resolveCloudflareApiToken({ env, credentialStore, required: true });
  const uploadEnv = { ...env, CLOUDFLARE_API_TOKEN: token };
  // Tarballs first, the manifest last: a manifest in R2 always names objects that are already there and verified.
  for (const item of planned) {
    await upload(item.file, item.evidence, item.key, "private", { env: uploadEnv });
    log(`right-release linux-publish: ${PRIVATE_LINUX_BUCKET}/${item.key} uploaded and read back (sha256 ${item.evidence.sha256.slice(0, 16)}...)`);
  }
  return { version: manifest.version, keys: planned.map((item) => item.key) };
}

/** A presigned URL for one architecture of a published release, with the digest Pulse pins. The manifest is read back from R2 (not trusted from disk). */
export async function presignLinuxArtifact({ app, version, arch, expiresSeconds = 3600, env = process.env, credentialStore, fetchImpl = fetch, now = new Date() }) {
  if (!LINUX_ARCHES.includes(arch)) fail(`arch must be one of ${LINUX_ARCHES.join(", ")}`);
  const token = resolveCloudflareApiToken({ env, credentialStore, required: true });
  const credentials = await deriveR2S3Credentials(token, { account: env.CLOUDFLARE_ACCOUNT_ID, fetchImpl });
  const manifestKey = linuxObjectKey(app, version, "linux-artifacts.json");
  const manifestUrl = presignGetUrl({ bucket: PRIVATE_LINUX_BUCKET, key: manifestKey, credentials, account: env.CLOUDFLARE_ACCOUNT_ID || undefined, expiresSeconds: 60, now });
  const response = await fetchImpl(manifestUrl.url);
  if (!response.ok) fail(`the published manifest ${manifestKey} could not be read (HTTP ${response.status}); publish it first`);
  const manifest = JSON.parse(await response.text());
  const entry = manifest?.artifacts?.[arch];
  if (manifest?.schema !== LINUX_ARTIFACTS_SCHEMA || manifest.app !== app || manifest.version !== version || !entry) fail(`the published manifest has no ${arch} artifact for ${app} ${version}`);
  const signed = presignGetUrl({ bucket: PRIVATE_LINUX_BUCKET, key: linuxObjectKey(app, version, entry.file), credentials, account: env.CLOUDFLARE_ACCOUNT_ID || undefined, expiresSeconds, now });
  return { app, version, arch, file: entry.file, sha256: entry.sha256, size: entry.size, glibc: entry.glibc ?? null, url: signed.url, expiresAt: signed.expiresAt };
}

export async function main(argv = process.argv.slice(2)) {
  const [command, ...rest] = argv;
  const flags = {};
  for (let i = 0; i < rest.length; i += 1) {
    const name = rest[i];
    if (name === "--dry-run") { flags.dryRun = true; continue; }
    if (!["--app", "--out-dir", "--approve", "--version", "--arch", "--expires"].includes(name) || rest[i + 1] === undefined) { console.error(`right-release linux-publish: unknown or incomplete argument ${name}`); return 2; }
    flags[name.slice(2).replace(/-([a-z])/g, (_, c) => c.toUpperCase())] = rest[i + 1];
    i += 1;
  }
  try {
    if (command === "publish") {
      if (!flags.app || !flags.outDir) { console.error("usage: right-release linux-publish publish --app <slug> --out-dir <dir> --approve <manifest sha256> [--dry-run]"); return 2; }
      const result = await publishLinuxArtifacts({ outDir: flags.outDir, app: flags.app, approve: flags.approve, dryRun: flags.dryRun === true });
      console.log(JSON.stringify(result));
      return 0;
    }
    if (command === "presign") {
      if (!flags.app || !flags.version || !flags.arch) { console.error("usage: right-release linux-publish presign --app <slug> --version <v> --arch <x86_64|aarch64> [--expires <seconds>]"); return 2; }
      const result = await presignLinuxArtifact({ app: flags.app, version: flags.version, arch: flags.arch, ...(flags.expires ? { expiresSeconds: Number(flags.expires) } : {}) });
      console.log(JSON.stringify(result));
      return 0;
    }
    console.error("usage: right-release linux-publish publish|presign ...");
    return 2;
  } catch (error) {
    console.error(`right-release linux-publish: ${error.message}`);
    return error instanceof LinuxPublishError || error instanceof PublicationError ? 1 : 2;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main().then((code) => { process.exitCode = code; });
