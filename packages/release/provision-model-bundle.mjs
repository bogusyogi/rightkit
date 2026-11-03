#!/usr/bin/env node
// `right-release provision-model [--config <file>] [--id <bundle>] [--dry-run]`: the pinned, verified build-time fetch of a catalog model
// bundle (#993), replacing the per-app fetch scripts. The app declares what to provision:
//
//   modelBundles: [{
//     id: "embeddinggemma2-q4",                                     // the bundle id in the rightkit-models catalog
//     dest: "src-tauri/resources/embed_model",                      // app-relative directory the files are provisioned into
//     pins: [{ path: "model.onnx", sha256: "<64 hex>", size: 123 }, ...],   // the catalog's own pins for the bundle: nothing is fetched unpinned
//     source: { baseUrl: "https://host/models" }                    // https; each file is <baseUrl>/<id>/<path> (a pin may carry its own `url`)
//           | { r2: { bucket: "rightsites-updates", prefix: "models" } }   // S3-signed GET of <prefix>/<id>/<path>; credentials derived in memory from CLOUDFLARE_API_TOKEN
//   }]
//
// Every file is downloaded into `<file>.part` (resumed with a Range request when a partial copy exists), then its size and sha256 are checked
// against the pin before it is renamed into place; a file that already matches its pin is not fetched again; a mismatch deletes the partial copy
// and fails, and a file in place is never replaced by unverified bytes. The pins come from the app's config, which takes them from the
// `rightkit-models` catalog spec for the bundle (crates/rightkit-models/src/suite/catalog.rs): this module judges bytes against pins, it does
// not read the catalog. Nothing is signed, uploaded or written outside `dest`.
import { createHash } from "node:crypto";
import { createReadStream, createWriteStream, existsSync, mkdirSync, renameSync, rmSync, statSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { once } from "node:events";
import { CommandSpecError, loadAppConfig, relativeInside } from "./command-spec.mjs";
import { defaultReleaseConfigFile } from "./release-config.mjs";
import { deriveR2S3Credentials, getObject } from "./r2-s3-multipart.mjs";

const fail = (message) => { throw new CommandSpecError(message); };
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const SHA256 = /^[0-9a-f]{64}$/;
const BUNDLE_KEYS = ["id", "dest", "pins", "source"];
const PIN_KEYS = ["path", "sha256", "size", "url"];
const SAFE_ID = /^[a-z0-9][a-z0-9._-]*$/;
const R2_NAME = /^[a-z0-9][a-z0-9.-]{2,62}$/;

/** Validate one bundle declaration; returns it with pins' paths normalised. Throws CommandSpecError. */
export function validateModelBundle(bundle, label = "modelBundles entry") {
  if (!isObject(bundle)) fail(`${label} must be an object`);
  const unknown = Object.keys(bundle).filter((key) => !BUNDLE_KEYS.includes(key));
  if (unknown.length) fail(`${label} has unknown key(s): ${unknown.join(", ")} (allowed: ${BUNDLE_KEYS.join(", ")})`);
  if (typeof bundle.id !== "string" || !SAFE_ID.test(bundle.id)) fail(`${label}.id must be a bundle id (lowercase letters, digits, . _ -)`);
  const dest = relativeInside(bundle.dest, `${label}.dest`);
  if (["", ".", "./"].includes(dest) || /^(?:\.git|src|src-tauri|node_modules)\/?$/.test(dest)) fail(`${label}.dest ${dest} is not a dedicated model directory`);
  if (!Array.isArray(bundle.pins) || bundle.pins.length === 0) fail(`${label}.pins must list every file of the bundle with its sha256 and size`);
  const seen = new Set();
  const pins = bundle.pins.map((pin, index) => {
    const where = `${label}.pins[${index}]`;
    if (!isObject(pin)) fail(`${where} must be an object`);
    const bad = Object.keys(pin).filter((key) => !PIN_KEYS.includes(key));
    if (bad.length) fail(`${where} has unknown key(s): ${bad.join(", ")} (allowed: ${PIN_KEYS.join(", ")})`);
    const file = relativeInside(pin.path, `${where}.path`);
    if (seen.has(file)) fail(`${where}.path ${file} is pinned twice`);
    seen.add(file);
    if (typeof pin.sha256 !== "string" || !SHA256.test(pin.sha256)) fail(`${where}.sha256 must be 64 lowercase hex characters`);
    if (!Number.isSafeInteger(pin.size) || pin.size < 1) fail(`${where}.size must be a positive integer (bytes)`);
    if (pin.url !== undefined && !/^https:\/\/[^\s]+$/.test(pin.url)) fail(`${where}.url must be an https URL`);
    return { ...pin, path: file };
  });
  const source = bundle.source;
  if (!isObject(source)) fail(`${label}.source must be { baseUrl } or { r2: { bucket, prefix } }`);
  if (source.baseUrl !== undefined && source.r2 !== undefined) fail(`${label}.source: give baseUrl or r2, not both`);
  if (source.baseUrl !== undefined) {
    if (typeof source.baseUrl !== "string" || !/^https:\/\/[^\s?#]+$/.test(source.baseUrl)) fail(`${label}.source.baseUrl must be an https URL without a query`);
  } else if (source.r2 !== undefined) {
    if (!isObject(source.r2) || typeof source.r2.bucket !== "string" || !R2_NAME.test(source.r2.bucket)) fail(`${label}.source.r2.bucket must be an R2 bucket name`);
    if (source.r2.prefix !== undefined) relativeInside(source.r2.prefix, `${label}.source.r2.prefix`);
  } else fail(`${label}.source must be { baseUrl } or { r2: { bucket, prefix } }`);
  return { id: bundle.id, dest, pins, source };
}

async function digestOfFile(file) {
  const hash = createHash("sha256");
  let size = 0;
  for await (const chunk of createReadStream(file)) { hash.update(chunk); size += chunk.length; }
  return { sha256: hash.digest("hex"), size };
}

async function appendBody(response, part, append) {
  const out = createWriteStream(part, { flags: append ? "a" : "w" });
  try {
    for await (const piece of response.body) if (!out.write(piece)) await once(out, "drain");
  } finally {
    out.end();
    await once(out, "close");
  }
}

/**
 * Provision `bundle` (validated) under `appRoot`. `fetchImpl` is fetch; `env` carries CLOUDFLARE_API_TOKEN for an r2 source. Resolves to
 * { id, dest, files: [{ path, sha256, size, status: "present" | "fetched" }] }.
 */
export async function provisionModelBundle({ id, pins, dest, source }, { appRoot = process.cwd(), env = process.env, fetchImpl = fetch, log = () => {}, dryRun = false } = {}) {
  const bundle = validateModelBundle({ id, pins, dest, source });
  const root = path.resolve(appRoot, bundle.dest);
  const files = [];
  let credentials = null;
  for (const pin of bundle.pins) {
    const target = path.join(root, ...pin.path.split("/"));
    const part = `${target}.part`;
    if (existsSync(target) && statSync(target).size === pin.size && (await digestOfFile(target)).sha256 === pin.sha256) {
      log(`model ${bundle.id}: ${pin.path} present and verified`);
      files.push({ path: pin.path, sha256: pin.sha256, size: pin.size, status: "present" });
      continue;
    }
    if (dryRun) { files.push({ path: pin.path, sha256: pin.sha256, size: pin.size, status: "would-fetch" }); continue; }
    mkdirSync(path.dirname(target), { recursive: true });
    const have = existsSync(part) ? statSync(part).size : 0;
    if (have > pin.size) rmSync(part, { force: true });
    const offset = existsSync(part) ? statSync(part).size : 0;
    if (offset < pin.size) {
      let response;
      try {
        if (bundle.source.r2) {
          credentials ??= await deriveR2S3Credentials(env.CLOUDFLARE_API_TOKEN);
          const key = [bundle.source.r2.prefix, bundle.id, pin.path].filter(Boolean).join("/");
          response = await getObject({ bucket: bundle.source.r2.bucket, key, credentials, fetchImpl, ...(offset ? { range: `bytes=${offset}-` } : {}) });
        } else {
          const url = pin.url ?? `${bundle.source.baseUrl.replace(/\/+$/, "")}/${[bundle.id, ...pin.path.split("/")].map(encodeURIComponent).join("/")}`;
          response = await fetchImpl(url, { headers: offset ? { range: `bytes=${offset}-` } : {} });
          if (!response.ok) throw new Error(`HTTP ${response.status}`);
        }
      } catch (error) { throw new Error(`model ${bundle.id}: fetching ${pin.path} failed (${error.message})`); }
      if (!response.body) throw new Error(`model ${bundle.id}: fetching ${pin.path} returned no body`);
      // A server that ignored the Range header sends the whole file: start the partial copy over.
      await appendBody(response, part, offset > 0 && response.status === 206);
    }
    const got = await digestOfFile(part);
    if (got.size !== pin.size || got.sha256 !== pin.sha256) {
      rmSync(part, { force: true });
      throw new Error(`model ${bundle.id}: ${pin.path} does not match its pin (size ${got.size} of ${pin.size}, sha256 ${got.sha256.slice(0, 16)}... of ${pin.sha256.slice(0, 16)}...); the partial copy was removed`);
    }
    renameSync(part, target);
    log(`model ${bundle.id}: ${pin.path} fetched and verified (${pin.size} bytes)`);
    files.push({ path: pin.path, sha256: pin.sha256, size: pin.size, status: "fetched" });
  }
  return { id: bundle.id, dest: bundle.dest, files };
}

export function parseProvisionArgs(argv) {
  const opts = { config: defaultReleaseConfigFile(), id: undefined, dryRun: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--config" || arg === "--id") {
      const value = argv[i + 1];
      if (!value || value.startsWith("--")) throw new Error(`${arg} needs a value`);
      opts[arg.slice(2)] = value;
      i += 1;
    } else if (arg === "--dry-run") opts.dryRun = true;
    else throw new Error(`unknown provision-model argument: ${arg}`);
  }
  return opts;
}

export async function main(argv = process.argv.slice(2)) {
  const opts = parseProvisionArgs(argv);
  const { config, appRoot } = await loadAppConfig(opts.config);
  if (!Array.isArray(config.modelBundles) || config.modelBundles.length === 0) fail("the release config declares no modelBundles");
  const bundles = config.modelBundles.map((entry, index) => validateModelBundle(entry, `modelBundles[${index}]`));
  const chosen = opts.id ? bundles.filter((bundle) => bundle.id === opts.id) : bundles;
  if (chosen.length === 0) fail(`no modelBundles entry has id ${opts.id}`);
  for (const bundle of chosen) {
    const result = await provisionModelBundle(bundle, { appRoot, log: (line) => console.log(line), dryRun: opts.dryRun });
    console.log(`provision-model ${result.id}: ${result.files.length} file(s) in ${result.dest}${opts.dryRun ? " (dry run: nothing fetched)" : ""}`);
  }
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().then((code) => { process.exitCode = code; }, (error) => {
    console.error(`right-release provision-model: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  });
}
