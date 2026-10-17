// Declarative engine and sidecar staging for Tauri externalBin (bogusyogi/rightkit#345).
//
// Eight apps hand-rolled the same script: build an engine through `rightkit cargo`, find the executable, copy it to
// src-tauri/binaries/<name>-<target-triple>[.exe], stage ffmpeg, stage onnxruntime, copy model resources. This module is
// that script, driven by a `sidecars` list in right-release.config.mjs:
//
//   sidecars: [
//     { kind: "cargo-bin", package: "mailright-engine", bin?, name?, profile?: "release" | { dev, release }, features?, manifestPath?, sha256?, platforms? },
//     { kind: "ffmpeg", variant?, tools?, notices: { dir, sourceOfferContact }, catalog, stage?, bundleDir? },
//     { kind: "onnxruntime", version, catalog, resourceDir?: "src-tauri/resources/runtime", as?, licenseDir?, cacheDir? },
//     { kind: "resource", from, to, sha256?, machO?, platforms? },
//     { kind: "command", name, cmd, args?, cwd?, output, externalBin: true, platforms? },
//   ]
//
// `catalog` is a relative path, an absolute path, { crate: "rightkit-ffmpeg", file? } (resolved through `rightkit cargo
// metadata`) or { npm: "<pkg>/<file>" }. The wrapper form { sidecars, profiles: { dev, release } } sets the default cargo
// profile pair; RIGHTKIT_BUILD_MODE (dev|release, default release) selects. The target `universal-apple-darwin` builds
// both slices of a cargo-bin and lipo-creates `<name>-universal-apple-darwin`.
//
// It never signs (release.mjs signs staged files) and never calls bare cargo. Every effect goes through `deps`.
import { createHash } from "node:crypto";
import nodeFs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync as nodeSpawnSync } from "./background-process.mjs";
import { resolveCommand } from "./development.mjs";
import {
  planMediaSidecar, stagedFileName, validateMediaSidecarConfig, verifyStagedSidecars, writeMediaSidecarNotices,
} from "./media-sidecar.mjs";

export const STAGE_SIDECAR_CONTRACT = "rightkit-stage-sidecar-v1";
export const STAGE_SIDECAR_RECEIPT_NAME = "stage-sidecar.json";
export const DEFAULT_STAGE_DIR = "src-tauri/binaries";
export const DEFAULT_ORT_RESOURCE_DIR = "src-tauri/resources/runtime";

export class StageSidecarError extends Error {
  constructor(code, message, details = {}) {
    super(message, details.cause ? { cause: details.cause } : undefined);
    this.name = "StageSidecarError";
    this.code = code;
    const { cause: _cause, ...rest } = details;
    this.details = rest;
  }
}

const fail = (code, message, details) => { throw new StageSidecarError(code, message, details); };
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const SHA256 = /^[0-9a-f]{64}$/;
const KINDS = new Set(["cargo-bin", "ffmpeg", "onnxruntime", "resource", "command"]);
const UNIVERSAL = "universal-apple-darwin";
const SLICES = Object.freeze(["aarch64-apple-darwin", "x86_64-apple-darwin"]);
const MODES = new Set(["dev", "release"]);
const PROFILE = /^[a-z][a-z0-9_-]*$/;
const PLATFORMS = new Set(["mac", "win", "linux"]);
const NAME = /^[A-Za-z0-9][A-Za-z0-9_.-]*$/;
const KEYS = Object.freeze({
  "cargo-bin": ["kind", "package", "bin", "name", "profile", "features", "manifestPath", "sha256", "platforms"],
  ffmpeg: ["kind", "variant", "tools", "notices", "catalog", "stage", "bundleDir", "platforms"],
  onnxruntime: ["kind", "version", "catalog", "resourceDir", "as", "licenseDir", "cacheDir", "platforms"],
  resource: ["kind", "from", "to", "sha256", "machO", "platforms"],
  command: ["kind", "name", "cmd", "args", "cwd", "output", "externalBin", "platforms"],
});
const TRIPLES = Object.freeze({
  "aarch64-apple-darwin": { platform: "mac", os: "macos", arch: "aarch64" },
  "x86_64-apple-darwin": { platform: "mac", os: "macos", arch: "x86_64" },
  "x86_64-pc-windows-msvc": { platform: "win", os: "windows", arch: "x86_64" },
  "aarch64-pc-windows-msvc": { platform: "win", os: "windows", arch: "aarch64" },
  "x86_64-unknown-linux-gnu": { platform: "linux", os: "linux", arch: "x86_64" },
  "aarch64-unknown-linux-gnu": { platform: "linux", os: "linux", arch: "aarch64" },
});

/** Parse a Rust target triple into { triple, platform, os, arch }. universal-apple-darwin has arch "universal" (cargo-bin lipos both slices). */
export function parseTargetTriple(target) {
  if (target === UNIVERSAL) return { triple: UNIVERSAL, platform: "mac", os: "macos", arch: "universal" };
  const row = typeof target === "string" ? TRIPLES[target] : undefined;
  if (!row) fail("STAGE_SIDECAR_TARGET_UNSUPPORTED", `unsupported target triple ${JSON.stringify(target)} (${Object.keys(TRIPLES).join(", ")})`);
  return { triple: target, ...row };
}

/** `--target`, then TAURI_ENV_TARGET_TRIPLE, then the host triple from `rightkit rustc -vV`. */
export function resolveTargetTriple({ target, env = process.env, run = defaultRun, cwd = process.cwd() } = {}) {
  if (target) return target;
  if (env.TAURI_ENV_TARGET_TRIPLE) return env.TAURI_ENV_TARGET_TRIPLE;
  const result = run("rightkit", ["rustc", "-vV"], { cwd, env });
  const host = result?.stdout?.match(/^host:\s*(.+)$/m)?.[1]?.trim();
  if (!host || result.status !== 0) fail("STAGE_SIDECAR_TARGET_UNKNOWN", "cannot determine the host triple (rightkit rustc -vV failed); pass a target");
  return host;
}

function relativePath(value, label, errors) {
  if (typeof value !== "string" || !value.trim()) { errors.push(`${label} must be a non-empty relative path`); return undefined; }
  const normalized = value.replaceAll("\\", "/");
  if (path.isAbsolute(value) || /^[A-Za-z]:/.test(value) || normalized.split("/").includes("..")) { errors.push(`${label} must be relative and stay inside the app: ${value}`); return undefined; }
  return normalized.replace(/\/+$/, "");
}

/** `<name>-<triple>[.exe]`, with `<name>-universal-apple-darwin` for the universal target. */
const stagedName = (name, t) => (t.arch === "universal" ? `${name}-${UNIVERSAL}` : stagedFileName(name, t.os, t.arch));

/** RIGHTKIT_BUILD_MODE: dev | release (default release). Anything else is refused. */
export function resolveBuildMode(env = process.env) {
  const value = env?.RIGHTKIT_BUILD_MODE;
  if (value === undefined || value === "") return "release";
  if (!MODES.has(value)) fail("STAGE_SIDECAR_CONFIG_INVALID", `RIGHTKIT_BUILD_MODE must be dev or release, got ${JSON.stringify(value)}`);
  return value;
}

/** A profile is a Cargo profile name or exactly { dev, release } of names. Returns the pair or the single name. */
function parseProfile(value, label, errors) {
  if (typeof value === "string") {
    if (!PROFILE.test(value)) errors.push(`${label} must be a Cargo profile name`);
    return value;
  }
  if (!isObject(value)) { errors.push(`${label} must be a Cargo profile name or { dev, release }`); return undefined; }
  for (const key of Object.keys(value)) if (!MODES.has(key)) errors.push(`${label}: unknown key ${JSON.stringify(key)} (allowed: dev, release)`);
  for (const mode of MODES) if (typeof value[mode] !== "string" || !PROFILE.test(value[mode])) errors.push(`${label}.${mode} must be a Cargo profile name`);
  return { dev: value.dev, release: value.release };
}

const CATALOG_DEFAULT_FILE = Object.freeze({ ffmpeg: "catalog/artifacts.json", onnxruntime: "catalog/native-libraries.json" });

/**
 * A catalog reference: a relative path (inside the app), an absolute path, { crate, file? } or { npm: "<pkg>/<file>" }.
 * Problems are pushed to `errors` against this entry only.
 */
function parseCatalogSpec(value, label, errors, kind) {
  if (value === undefined) return undefined;
  if (typeof value === "string") {
    if (path.isAbsolute(value) || /^[A-Za-z]:[\\/]/.test(value)) return { via: "absolute", file: value };
    const file = relativePath(value, label, errors);
    return file ? { via: "path", file } : undefined;
  }
  if (!isObject(value)) { errors.push(`${label} must be a path, { crate }, or { npm }`); return undefined; }
  const keys = Object.keys(value);
  if ("crate" in value) {
    for (const key of keys) if (!["crate", "file"].includes(key)) errors.push(`${label}: unknown key ${JSON.stringify(key)} for { crate, file? }`);
    if (typeof value.crate !== "string" || !NAME.test(value.crate)) { errors.push(`${label}.crate must be a Cargo package name`); return undefined; }
    const file = relativePath(value.file ?? CATALOG_DEFAULT_FILE[kind] ?? "catalog.json", `${label}.file`, errors);
    return file ? { via: "crate", crate: value.crate, file } : undefined;
  }
  if ("npm" in value) {
    for (const key of keys) if (key !== "npm") errors.push(`${label}: unknown key ${JSON.stringify(key)} for { npm }`);
    const match = typeof value.npm === "string" ? /^((?:@[a-z0-9][a-z0-9._-]*\/)?[a-z0-9][a-z0-9._-]*)\/(.+)$/.exec(value.npm) : null;
    if (!match) { errors.push(`${label}.npm must be "<package>/<file>" (scoped packages allowed)`); return undefined; }
    const file = relativePath(match[2], `${label}.npm file`, errors);
    return file ? { via: "npm", pkg: match[1], file } : undefined;
  }
  errors.push(`${label} must be a path, { crate }, or { npm }`);
  return undefined;
}

/**
 * Validate and normalise the manifest. `raw` is the `sidecars` array, or { sidecars, stageDir?, receiptPath?, profiles? }.
 * `mode` (dev | release, default release) selects the profile of a { dev, release } pair.
 * Every problem is reported at once as STAGE_SIDECAR_CONFIG_INVALID. Entries whose `platforms` excludes the target are
 * kept (shape-checked) with skipped: true.
 */
export function validateSidecarManifest(raw, { target, mode = "release" } = {}) {
  const t = parseTargetTriple(target);
  const errors = [];
  if (!MODES.has(mode)) fail("STAGE_SIDECAR_CONFIG_INVALID", `build mode must be dev or release, got ${JSON.stringify(mode)}`);
  const wrapper = Array.isArray(raw) ? { sidecars: raw } : raw;
  if (!isObject(wrapper)) fail("STAGE_SIDECAR_CONFIG_INVALID", "sidecars must be an array (or { sidecars, stageDir?, receiptPath?, profiles? })");
  for (const key of Object.keys(wrapper)) if (!["sidecars", "stageDir", "receiptPath", "profiles"].includes(key)) errors.push(`unknown key ${JSON.stringify(key)}`);
  const defaultProfile = wrapper.profiles === undefined ? undefined : parseProfile(wrapper.profiles, "profiles", errors);
  if (typeof defaultProfile === "string") errors.push("profiles must be { dev, release }");
  if (!Array.isArray(wrapper.sidecars) || !wrapper.sidecars.length) errors.push("sidecars must be a non-empty array");
  const stageDir = relativePath(wrapper.stageDir ?? DEFAULT_STAGE_DIR, "stageDir", errors);
  const receiptPath = wrapper.receiptPath === undefined
    ? (stageDir ? `${stageDir}/${STAGE_SIDECAR_RECEIPT_NAME}` : undefined)
    : relativePath(wrapper.receiptPath, "receiptPath", errors);
  const entries = [];
  const outputs = new Map();
  const claim = (output, label) => {
    const key = output.toLowerCase();
    if (outputs.has(key)) errors.push(`${label} writes ${output}, which ${outputs.get(key)} already writes`);
    else outputs.set(key, label);
  };
  for (const [index, item] of (Array.isArray(wrapper.sidecars) ? wrapper.sidecars : []).entries()) {
    const label = `sidecars[${index}]`;
    if (!isObject(item) || !KINDS.has(item.kind)) { errors.push(`${label}.kind must be one of ${[...KINDS].join(", ")}`); continue; }
    const where = `${label} (${item.kind})`;
    for (const key of Object.keys(item)) if (!KEYS[item.kind].includes(key)) errors.push(`${where}: unknown key ${JSON.stringify(key)} (allowed: ${KEYS[item.kind].join(", ")})`);
    let platforms;
    if (item.platforms !== undefined) {
      if (!Array.isArray(item.platforms) || !item.platforms.length || item.platforms.some((p) => !PLATFORMS.has(p))) errors.push(`${where}: platforms must be a non-empty list drawn from ${[...PLATFORMS].join(", ")}`);
      else platforms = [...item.platforms];
    }
    const skipped = platforms ? !platforms.includes(t.platform) : false;
    const sha = (value, name) => {
      if (value !== undefined && !SHA256.test(value ?? "")) errors.push(`${where}: ${name} must be a lowercase 64-hex SHA-256`);
    };
    const entry = { kind: item.kind, index, skipped, ...(platforms ? { platforms } : {}) };
    if (item.kind === "cargo-bin") {
      if (typeof item.package !== "string" || !NAME.test(item.package)) errors.push(`${where}: package must be a Cargo package name`);
      const bin = item.bin ?? item.package;
      if (typeof bin !== "string" || !NAME.test(bin)) errors.push(`${where}: bin must be a binary target name`);
      const name = item.name ?? bin;
      if (typeof name !== "string" || !NAME.test(name) || /\.exe$/i.test(name) || name !== path.basename(name)) errors.push(`${where}: name must be a bare sidecar name (no path, no .exe, no target triple)`);
      const declared = item.profile !== undefined ? parseProfile(item.profile, `${where}: profile`, errors) : (defaultProfile ?? "release");
      const profile = isObject(declared) ? declared[mode] : declared;
      const features = item.features ?? [];
      if (!Array.isArray(features) || features.some((f) => typeof f !== "string" || !/^[A-Za-z0-9_/-]+$/.test(f))) errors.push(`${where}: features must be a list of feature names (package/feature allowed)`);
      const manifestPath = item.manifestPath === undefined ? undefined : relativePath(item.manifestPath, `${where}: manifestPath`, errors);
      sha(item.sha256, "sha256");
      if (typeof name === "string" && stageDir && !skipped) claim(stagedName(name, t), where);
      Object.assign(entry, { package: item.package, bin, name, profile, features: [...features], ...(manifestPath ? { manifestPath } : {}), ...(item.sha256 ? { sha256: item.sha256 } : {}) });
    } else if (item.kind === "ffmpeg") {
      const { kind: _kind, platforms: _platforms, catalog: rawCatalog, ...rest } = item;
      if (rest.stage !== undefined && isObject(rest.stage) && rest.stage.cmd === "cargo") errors.push(`${where}: stage.cmd must not be bare cargo; use { cmd: "rightkit", args: ["cargo", ...] }`);
      const catalogSpec = parseCatalogSpec(rawCatalog, `${where}: catalog`, errors, "ffmpeg");
      if (catalogSpec) entry.catalogSpec = catalogSpec;
      if (!skipped && t.arch === "universal") errors.push(`${where}: the catalog has no universal row; stage a lipo-merged file with a "command" or "resource" entry (machO: true) instead`);
      else if (!skipped) {
        try {
          const config = validateMediaSidecarConfig({ ...rest, arch: t.arch, stageDir }, { platform: t.platform, hostArch: t.arch });
          for (const tool of config.tools) claim(stagedFileName(tool, t.os, t.arch), where);
          entry.media = config;
        } catch (error) { errors.push(...String(error.message).split("\n").filter((line) => line.startsWith("- ")).map((line) => `${where}: ${line.slice(2)}`).concat(error.message.startsWith("invalid") ? [] : [`${where}: ${error.message}`])); }
      }
    } else if (item.kind === "onnxruntime") {
      if (typeof item.version !== "string" || !/^\d+\.\d+\.\d+$/.test(item.version)) errors.push(`${where}: version must be x.y.z`);
      const resourceDir = relativePath(item.resourceDir ?? DEFAULT_ORT_RESOURCE_DIR, `${where}: resourceDir`, errors);
      const licenseDir = relativePath(item.licenseDir ?? (resourceDir ? `${resourceDir}/LICENSES` : "x"), `${where}: licenseDir`, errors);
      const cacheDir = relativePath(item.cacheDir ?? ".cache/onnxruntime", `${where}: cacheDir`, errors);
      const catalogSpec = parseCatalogSpec(item.catalog, `${where}: catalog`, errors, "onnxruntime");
      if (item.as !== undefined && (typeof item.as !== "string" || !NAME.test(item.as))) errors.push(`${where}: as must be a bare file name`);
      if (!skipped && t.arch === "universal") errors.push(`${where}: the catalog has no universal row; stage a lipo-merged file with a "command" or "resource" entry (machO: true) instead`);
      Object.assign(entry, { version: item.version, resourceDir, licenseDir, cacheDir, ...(catalogSpec ? { catalogSpec } : {}), ...(item.as ? { as: item.as } : {}) });
    } else if (item.kind === "command") {
      const name = item.name;
      if (typeof name !== "string" || !NAME.test(name) || /\.exe$/i.test(name) || name !== path.basename(name)) errors.push(`${where}: name must be a bare sidecar name (no path, no .exe, no target triple)`);
      if (typeof item.cmd !== "string" || !item.cmd.trim() || /\s/.test(item.cmd)) errors.push(`${where}: cmd must be one executable name or path with no spaces (arguments go in args; no shell is used)`);
      const args = item.args ?? [];
      if (!Array.isArray(args) || args.some((a) => typeof a !== "string")) errors.push(`${where}: args must be a list of strings`);
      const cwd = item.cwd === undefined ? undefined : relativePath(item.cwd, `${where}: cwd`, errors);
      const output = relativePath(item.output, `${where}: output`, errors);
      if (item.externalBin !== true) errors.push(`${where}: externalBin must be true (the output is staged as <name>-<triple>)`);
      if (typeof name === "string" && stageDir && !skipped) claim(stagedName(name, t), where);
      Object.assign(entry, { name, cmd: item.cmd, args: Array.isArray(args) ? [...args] : [], ...(cwd ? { cwd } : {}), output });
    } else {
      const from = relativePath(item.from, `${where}: from`, errors);
      const to = relativePath(item.to, `${where}: to`, errors);
      sha(item.sha256, "sha256");
      if (item.machO !== undefined && typeof item.machO !== "boolean") errors.push(`${where}: machO must be a boolean`);
      if (to && !skipped) claim(to, where);
      Object.assign(entry, { from, to, ...(item.sha256 ? { sha256: item.sha256 } : {}), ...(item.machO ? { machO: true } : {}) });
    }
    entries.push(entry);
  }
  if (errors.length) fail("STAGE_SIDECAR_CONFIG_INVALID", `invalid sidecars configuration:\n${errors.map((line) => `- ${line}`).join("\n")}`, { errors });
  return { target: t, stageDir, receiptPath, entries };
}

const sha256Of = (bytes) => createHash("sha256").update(bytes).digest("hex");
const profileDirectory = (profile) => (profile === "dev" || profile === "test" ? "debug" : profile === "bench" ? "release" : profile);
const tail = (text, lines = 12) => String(text ?? "").trim().split("\n").slice(-lines).join("\n");

/** Default process runner: `rightkit` resolves through the installed runtime client (as the apps' scripts did). */
export function defaultRun(command, args, { cwd, env = process.env } = {}) {
  const launch = command === "rightkit" ? resolveCommand("rightkit", args, { root: cwd, env }) : { command, args };
  return nodeSpawnSync(launch.command, launch.args, {
    cwd, env: { ...env, ...(launch.env ?? {}) }, encoding: "utf8", maxBuffer: 512 * 1024 * 1024, shell: false, windowsHide: true, stdio: ["inherit", "pipe", "inherit"],
  });
}

/** Default archive member extraction (bsdtar reads tgz and zip/nupkg): the one member whose basename is `fileName`. */
export function defaultExtractMember({ archive, fileName, destDir, run = defaultRun }) {
  const listing = run("tar", ["-tf", archive], { cwd: path.dirname(archive) });
  if (listing.status !== 0) fail("STAGE_SIDECAR_EXTRACT_FAILED", `cannot list ${archive}`);
  const members = String(listing.stdout).split(/\r?\n/).map((line) => line.trim()).filter((line) => line && path.posix.basename(line) === fileName);
  if (members.length !== 1) fail("STAGE_SIDECAR_EXTRACT_FAILED", `${archive} has ${members.length} members named ${fileName}, expected exactly one`);
  const extracted = run("tar", ["-xf", archive, "-C", destDir, members[0]], { cwd: path.dirname(archive) });
  if (extracted.status !== 0) fail("STAGE_SIDECAR_EXTRACT_FAILED", `cannot extract ${members[0]} from ${archive}`);
  return path.join(destDir, ...members[0].split("/"));
}

async function defaultFetchBytes(url) {
  if (!url.startsWith("https://")) fail("STAGE_SIDECAR_CONFIG_INVALID", `refusing a non-https archive URL: ${url}`);
  const response = await fetch(url);
  if (!response.ok) fail("STAGE_SIDECAR_DOWNLOAD_FAILED", `GET ${url} failed with ${response.status}`);
  return Buffer.from(await response.arrayBuffer());
}

/** Absolute catalog file for a validated spec. Crate resolution reads the rightkit metadata listing; npm walks up node_modules. */
function resolveCatalogFile(spec, ctx, kind) {
  const label = kind === "ffmpeg" ? "rightkit-ffmpeg" : "native-libraries";
  if (spec.via === "absolute") return spec.file;
  if (spec.via === "path") return path.resolve(ctx.appRoot, spec.file);
  if (spec.via === "crate") {
    const meta = ctx.run("rightkit", ["cargo", "metadata", "--format-version", "1"], { cwd: ctx.appRoot, env: ctx.env });
    let packages;
    try { packages = JSON.parse(meta.stdout).packages; } catch { /* refused below */ }
    if (meta?.status !== 0 || !Array.isArray(packages)) fail("STAGE_SIDECAR_CATALOG_UNREADABLE", `the ${label} catalog crate ${spec.crate} cannot be located: the workspace metadata query failed`, { crate: spec.crate });
    const found = packages.filter((pkg) => pkg.name === spec.crate && typeof pkg.manifest_path === "string");
    if (found.length !== 1) fail("STAGE_SIDECAR_CATALOG_UNREADABLE", `the ${label} catalog crate ${spec.crate} matches ${found.length} packages in this workspace's dependency graph, expected exactly one`, { crate: spec.crate });
    return path.join(path.dirname(found[0].manifest_path), ...spec.file.split("/"));
  }
  const parts = spec.file.split("/");
  for (let dir = ctx.appRoot; ; dir = path.dirname(dir)) {
    const candidate = path.join(dir, "node_modules", ...spec.pkg.split("/"), ...parts);
    if (ctx.fs.existsSync(candidate)) return candidate;
    if (path.dirname(dir) === dir) break;
  }
  return fail("STAGE_SIDECAR_CATALOG_UNREADABLE", `the ${label} catalog ${spec.pkg}/${spec.file} was not found in any node_modules above ${ctx.appRoot}`, { pkg: spec.pkg });
}

function readJsonCatalog(entryCatalog, appRoot, fs, what) {
  const file = path.resolve(appRoot, entryCatalog);
  let bytes;
  try { bytes = fs.readFileSync(file); } catch (error) { fail("STAGE_SIDECAR_CATALOG_UNREADABLE", `cannot read the ${what} catalog ${file}`, { cause: error }); }
  try { return { catalog: JSON.parse(String(bytes)), catalogBytes: bytes, crateDir: path.resolve(path.dirname(file), "..") }; } catch (error) { fail("STAGE_SIDECAR_CATALOG_UNREADABLE", `the ${what} catalog ${file} is not JSON`, { cause: error }); }
}

function artifactFromStream(stdout, bin) {
  let found;
  for (const line of String(stdout ?? "").split("\n")) {
    if (!line.startsWith("{")) continue;
    try {
      const message = JSON.parse(line);
      if (message.reason === "compiler-artifact" && message.executable && message.target?.name === bin && (message.target.kind ?? ["bin"]).includes("bin")) found = message.executable;
    } catch { /* not a cargo message */ }
  }
  return found;
}

/** Candidate output paths, in lookup order: the explicit-target layout (broker) first, then the plain layout. */
export function builtBinaryCandidates({ targetDir, triple, profile, bin, windows }) {
  const file = windows ? `${bin}.exe` : bin;
  const dir = profileDirectory(profile);
  return [path.join(targetDir, triple, dir, file), path.join(targetDir, dir, file)];
}

function copyInto(ctx, source, dest, { executable }) {
  const { fs, windows } = ctx;
  const sourceSha256 = sha256Of(fs.readFileSync(source));
  let changed = true;
  if (fs.existsSync(dest) && sha256Of(fs.readFileSync(dest)) === sourceSha256) changed = false;
  if (changed) {
    fs.mkdirSync(path.dirname(dest), { recursive: true });
    const temporary = `${dest}.tmp-${ctx.pid}`;
    fs.copyFileSync(source, temporary);
    if (executable && !windows) fs.chmodSync(temporary, 0o755);
    fs.renameSync(temporary, dest);
  }
  const sha256 = sha256Of(fs.readFileSync(dest));
  if (sha256 !== sourceSha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `staged ${dest} differs from its source ${source}`, { expected: sourceSha256, actual: sha256 });
  return { sha256, sizeBytes: fs.statSync(dest).size, changed };
}

const rel = (ctx, file) => path.relative(ctx.appRoot, file).replaceAll("\\", "/");

/** `lipo -archs <file>` as a list; a failing lipo is a typed error. */
function lipoArchs(ctx, file) {
  const result = ctx.run("lipo", ["-archs", file], { cwd: ctx.appRoot, env: ctx.env });
  if (result?.error || result?.status !== 0) fail("STAGE_SIDECAR_NOT_UNIVERSAL", `lipo -archs ${file} failed (${result?.error?.message ?? `exit ${result?.status}`})`, { file });
  return String(result.stdout ?? "").trim().split(/\s+/).filter(Boolean);
}

/** A universal file must carry both slices; anything thinner is refused. */
function requireUniversalFile(ctx, file, label) {
  const archs = lipoArchs(ctx, file);
  if (!(archs.includes("arm64") && archs.includes("x86_64"))) fail("STAGE_SIDECAR_NOT_UNIVERSAL", `${label} must contain arm64 and x86_64 for universal-apple-darwin, lipo -archs reports ${JSON.stringify(archs.join(" "))}`, { file, archs });
}

function stageCargoBin(entry, ctx) {
  if (ctx.target.arch !== "universal") return stageCargoSlice(entry, ctx, ctx.target.triple, { dest: path.resolve(ctx.appRoot, ctx.stageDir, stagedName(entry.name, ctx.target)) });
  const slices = SLICES.map((triple) => ({ triple, ...stageCargoBuild(entry, ctx, triple) }));
  const { fs, run, appRoot, env } = ctx;
  const dest = path.resolve(appRoot, ctx.stageDir, stagedName(entry.name, ctx.target));
  const temporary = `${dest}.tmp-${ctx.pid}`;
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  const args = ["-create", "-output", temporary, ...slices.map((slice) => slice.source)];
  ctx.log(`stage-sidecar: lipo ${args.join(" ")}`);
  const merged = run("lipo", args, { cwd: appRoot, env });
  if (merged?.error || merged?.status !== 0) fail("STAGE_SIDECAR_BUILD_FAILED", `lipo -create of ${entry.name} failed (${merged?.error?.message ?? `exit ${merged?.status}`})`, { package: entry.package, stderr: tail(merged?.stderr) });
  requireUniversalFile(ctx, temporary, `${entry.name} after lipo`);
  const sha256 = sha256Of(fs.readFileSync(temporary));
  if (entry.sha256 && entry.sha256 !== sha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${entry.bin} lipo-merged to ${sha256}, expected ${entry.sha256}`, { expected: entry.sha256, actual: sha256 });
  const changed = !(fs.existsSync(dest) && sha256Of(fs.readFileSync(dest)) === sha256);
  fs.chmodSync(temporary, 0o755);
  fs.renameSync(temporary, dest);
  return [{ kind: "cargo-bin", name: entry.name, path: rel(ctx, dest), sha256, sizeBytes: fs.statSync(dest).size, changed, sourceSha256: sha256, package: entry.package, bin: entry.bin, profile: entry.profile, features: entry.features, externalBin: true, slices: slices.map((slice) => ({ triple: slice.triple, sha256: slice.sourceSha256 })) }];
}

function stageCargoSlice(entry, ctx, triple, { dest }) {
  const { source, sourceSha256 } = stageCargoBuild(entry, ctx, triple);
  if (entry.sha256 && entry.sha256 !== sourceSha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${entry.bin} built to ${sourceSha256}, expected ${entry.sha256}`, { expected: entry.sha256, actual: sourceSha256 });
  const copied = copyInto(ctx, source, dest, { executable: true });
  return [{ kind: "cargo-bin", name: entry.name, path: rel(ctx, dest), ...copied, sourceSha256, package: entry.package, bin: entry.bin, profile: entry.profile, features: entry.features, externalBin: true }];
}

/** Build one slice through `rightkit cargo` and locate the executable. Returns { source, sourceSha256 }. */
function stageCargoBuild(entry, ctx, triple) {
  const { run, appRoot, env, fs } = ctx;
  const args = ["cargo", "build", "--locked", ...(entry.manifestPath ? ["--manifest-path", path.resolve(appRoot, entry.manifestPath)] : []),
    "-p", entry.package, "--bin", entry.bin, "--target", triple, "--profile", entry.profile,
    ...(entry.features.length ? ["--features", entry.features.join(",")] : []), "--message-format=json-render-diagnostics"];
  ctx.log(`stage-sidecar: rightkit ${args.join(" ")}`);
  const built = run("rightkit", args, { cwd: appRoot, env });
  if (built?.error || built?.status !== 0) fail("STAGE_SIDECAR_BUILD_FAILED", `rightkit cargo build of ${entry.package} failed (${built?.error?.message ?? `exit ${built?.status}`})`, { package: entry.package, stderr: tail(built?.stderr) });
  let source = artifactFromStream(built.stdout, entry.bin);
  if (!source || !fs.existsSync(source)) {
    const meta = run("rightkit", ["cargo", "metadata", "--format-version", "1", "--no-deps", ...(entry.manifestPath ? ["--manifest-path", path.resolve(appRoot, entry.manifestPath)] : [])], { cwd: appRoot, env });
    let targetDir;
    try { targetDir = JSON.parse(meta.stdout).target_directory; } catch { /* refused below */ }
    if (meta.status !== 0 || typeof targetDir !== "string" || !path.isAbsolute(targetDir)) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `cargo reported no ${entry.bin} executable and its target directory cannot be read`, { bin: entry.bin });
    const candidates = builtBinaryCandidates({ targetDir, triple, profile: entry.profile, bin: entry.bin, windows: ctx.windows });
    source = candidates.find((file) => fs.existsSync(file));
    if (!source) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `the build did not produce ${entry.bin} (looked in ${candidates.join(", ")})`, { bin: entry.bin, candidates });
  }
  const sourceSha256 = sha256Of(fs.readFileSync(source));
  return { source, sourceSha256 };
}

async function stageFfmpeg(entry, ctx) {
  const { appRoot, run, env, fs, target } = ctx;
  const config = entry.media;
  try {
    let resolved = await ctx.loadMediaCatalog({ config, appRoot, entry });
    if (!resolved) {
      if (!entry.catalogSpec) fail("STAGE_SIDECAR_CATALOG_UNREADABLE", "ffmpeg needs a catalog (the rightkit-ffmpeg artifacts.json: a path, { crate } or { npm }) or deps.loadMediaCatalog");
      resolved = readJsonCatalog(resolveCatalogFile(entry.catalogSpec, ctx, "ffmpeg"), appRoot, fs, "rightkit-ffmpeg");
    }
    const plan = planMediaSidecar({ config, catalog: resolved.catalog, crateDir: resolved.crateDir, crateVersion: resolved.crateVersion, catalogBytes: resolved.catalogBytes, root: appRoot });
    if (config.stage) {
      ctx.log(`stage-sidecar: ${config.stage.cmd} ${(config.stage.args ?? []).join(" ")}`);
      const staged = run(config.stage.cmd, config.stage.args ?? [], { cwd: appRoot, env });
      if (staged?.error || staged?.status !== 0) fail("STAGE_SIDECAR_BUILD_FAILED", `ffmpeg stage command failed (${staged?.error?.message ?? `exit ${staged?.status}`})`);
    }
    const noticeFiles = writeMediaSidecarNotices(plan, { exists: fs.existsSync, mkdir: fs.mkdirSync, copy: fs.copyFileSync, write: fs.writeFileSync });
    const verified = verifyStagedSidecars(plan, { exists: fs.existsSync, stat: (file) => fs.statSync(file), hashFile: (file) => sha256Of(fs.readFileSync(file)) });
    const notices = noticeFiles.map((file) => ({ name: path.basename(file), path: rel(ctx, file), sha256: sha256Of(fs.readFileSync(file)) }));
    return verified.map((file) => {
      if (file.preSignSha256 !== file.contentSha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${file.tool} staged as ${file.preSignSha256}, the approved catalog row ${file.artifactId} says ${file.contentSha256}`, { expected: file.contentSha256, actual: file.preSignSha256 });
      return { kind: "ffmpeg", name: file.tool, path: rel(ctx, file.stagedPath), sha256: file.preSignSha256, sizeBytes: file.preSignSizeBytes, changed: null, catalogRow: file.artifactId, version: file.version, catalogContentSha256: file.contentSha256, externalBin: true, license: "LGPL-2.1-or-later", notices };
    });
  } catch (error) {
    if (error instanceof StageSidecarError) throw error;
    return fail("STAGE_SIDECAR_FFMPEG_FAILED", `ffmpeg sidecar (${target.triple}): ${error.message}`, { cause: error, causeCode: error.code });
  }
}

async function stageOnnxRuntime(entry, ctx) {
  const { appRoot, fs, target } = ctx;
  const loaded = await (ctx.loadNativeCatalog({ entry, appRoot }) ?? (entry.catalogSpec ? readJsonCatalog(resolveCatalogFile(entry.catalogSpec, ctx, "onnxruntime"), appRoot, fs, "native-libraries") : undefined));
  if (!loaded) fail("STAGE_SIDECAR_CATALOG_UNREADABLE", "onnxruntime needs a catalog path (the rightkit-ffmpeg native-libraries.json) or deps.loadNativeCatalog");
  const rows = (loaded.catalog?.libraries ?? []).filter((row) => row.library === "onnxruntime" && row.version === entry.version && row.os === target.os && row.arch === target.arch);
  const where = `onnxruntime ${entry.version} ${target.os}/${target.arch}`;
  if (rows.length === 0) fail("STAGE_SIDECAR_NOT_APPROVED", `the native catalog has no ${where} row`);
  if (rows.length > 1) fail("STAGE_SIDECAR_NOT_APPROVED", `the native catalog has ${rows.length} ${where} rows`);
  const [row] = rows;
  if (!["approved", "pinned"].includes(row.status)) fail("STAGE_SIDECAR_NOT_APPROVED", `the ${where} row ${row.id} is ${row.status}, not approved or pinned`);
  for (const key of ["sha256", "artifactFileName"]) if (typeof row[key] !== "string" || !row[key]) fail("STAGE_SIDECAR_NOT_APPROVED", `the ${where} row ${row.id} has no ${key}`);
  if (!SHA256.test(row.sha256) || !SHA256.test(row.archive?.sha256 ?? "") || typeof row.archive?.url !== "string") fail("STAGE_SIDECAR_NOT_APPROVED", `the ${where} row ${row.id} lacks a valid library digest or archive url and digest`);
  if (!Array.isArray(row.noticeFiles) || !row.noticeFiles.length) fail("STAGE_SIDECAR_NOTICE_MISSING", `the ${where} row ${row.id} lists no notice files; the licence must ship with the library`);
  const crateDir = loaded.crateDir;
  const notices = row.noticeFiles.map((file) => {
    const normalized = String(file).replaceAll("\\", "/");
    if (path.isAbsolute(file) || normalized.split("/").includes("..")) fail("STAGE_SIDECAR_NOTICE_MISSING", `notice file path escapes the catalog crate: ${file}`);
    return { source: path.join(crateDir, ...normalized.split("/")), dest: path.resolve(appRoot, entry.licenseDir, path.posix.basename(normalized)) };
  });
  for (const { source } of notices) if (!fs.existsSync(source)) fail("STAGE_SIDECAR_NOTICE_MISSING", `notice file not found: ${source}`);

  const outName = entry.as ?? row.artifactFileName;
  const dest = path.resolve(appRoot, entry.resourceDir, outName);
  if (fs.existsSync(dest) && sha256Of(fs.readFileSync(dest)) === row.sha256) {
    ctx.log(`stage-sidecar: ${rel(ctx, dest)} already staged`);
  } else {
    const archive = path.resolve(appRoot, entry.cacheDir, path.posix.basename(new URL(row.archive.url).pathname));
    if (!(fs.existsSync(archive) && sha256Of(fs.readFileSync(archive)) === row.archive.sha256)) {
      const bytes = await ctx.fetchBytes(row.archive.url);
      const actual = sha256Of(bytes);
      if (actual !== row.archive.sha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${row.archive.url} downloaded as ${actual}, the catalog pins ${row.archive.sha256}; nothing staged`, { expected: row.archive.sha256, actual });
      fs.mkdirSync(path.dirname(archive), { recursive: true });
      fs.writeFileSync(archive, bytes);
    }
    const scratch = ctx.makeTempDir();
    try {
      const library = await ctx.extractMember({ archive, fileName: row.artifactFileName, destDir: scratch });
      const actual = sha256Of(fs.readFileSync(library));
      if (actual !== row.sha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${row.artifactFileName} extracted as ${actual}, the catalog pins ${row.sha256}; nothing staged`, { expected: row.sha256, actual });
      copyInto(ctx, library, dest, { executable: false });
    } finally {
      ctx.removeDir(scratch);
    }
  }
  const noticeOut = notices.map(({ source, dest: out }) => {
    fs.mkdirSync(path.dirname(out), { recursive: true });
    fs.copyFileSync(source, out);
    return { name: path.basename(out), path: rel(ctx, out), sha256: sha256Of(fs.readFileSync(out)) };
  });
  return [{ kind: "onnxruntime", name: outName, path: rel(ctx, dest), sha256: row.sha256, sizeBytes: fs.statSync(dest).size, changed: null, catalogRow: row.id, version: row.version, archiveSha256: row.archive.sha256, license: row.licenseSpdx ?? null, notices: noticeOut, externalBin: false }];
}

function stageResource(entry, ctx) {
  const { appRoot, fs } = ctx;
  const source = path.resolve(appRoot, entry.from);
  if (!fs.existsSync(source) || !fs.statSync(source).isFile()) fail("STAGE_SIDECAR_RESOURCE_MISSING", `resource ${entry.from} is not a file (directories are not staged; list each file)`, { from: entry.from });
  const actual = sha256Of(fs.readFileSync(source));
  if (entry.sha256 && entry.sha256 !== actual) fail("STAGE_SIDECAR_HASH_MISMATCH", `${entry.from} is ${actual}, expected ${entry.sha256}`, { expected: entry.sha256, actual });
  if (entry.machO && ctx.target.arch === "universal") requireUniversalFile(ctx, source, `resource ${entry.from}`);
  const dest = path.resolve(appRoot, entry.to);
  const copied = copyInto(ctx, source, dest, { executable: false });
  return [{ kind: "resource", name: path.basename(dest), path: rel(ctx, dest), ...copied, sourceSha256: actual, from: entry.from, externalBin: false }];
}

/** Run the entry's command (argv, no shell), then stage its output as <name>-<triple>. */
function stageCommand(entry, ctx) {
  const { appRoot, fs, run, env } = ctx;
  const cwd = entry.cwd ? path.resolve(appRoot, entry.cwd) : appRoot;
  ctx.log(`stage-sidecar: ${entry.cmd} ${entry.args.join(" ")}`);
  const built = run(entry.cmd, entry.args, { cwd, env });
  if (built?.error || built?.status !== 0) fail("STAGE_SIDECAR_BUILD_FAILED", `command ${entry.name} (${entry.cmd}) failed (${built?.error?.message ?? `exit ${built?.status}`})`, { name: entry.name, stderr: tail(built?.stderr) });
  const source = path.resolve(appRoot, entry.output);
  if (!fs.existsSync(source) || !fs.statSync(source).isFile()) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `command ${entry.name} did not produce ${entry.output}`, { name: entry.name, output: entry.output });
  if (ctx.target.arch === "universal") requireUniversalFile(ctx, source, `command ${entry.name} output ${entry.output}`);
  const sourceSha256 = sha256Of(fs.readFileSync(source));
  const dest = path.resolve(appRoot, ctx.stageDir, stagedName(entry.name, ctx.target));
  const copied = copyInto(ctx, source, dest, { executable: true });
  return [{ kind: "command", name: entry.name, path: rel(ctx, dest), ...copied, sourceSha256, output: entry.output, externalBin: true }];
}

/**
 * Stage every configured sidecar for `target` (a Rust triple) and write the staging receipt.
 * Returns { receipt, receiptPath, files, externalBinNames }. `deps` (all optional) injects every effect:
 * fs, run, fetchBytes, extractMember, loadMediaCatalog, loadNativeCatalog, makeTempDir, removeDir, now, pid, log, env.
 */
export async function stageSidecars({ appRoot, manifest, target, deps = {} } = {}) {
  if (typeof appRoot !== "string" || !path.isAbsolute(appRoot)) fail("STAGE_SIDECAR_CONFIG_INVALID", "appRoot must be an absolute path");
  const env = deps.env ?? process.env;
  const plan = validateSidecarManifest(manifest, { target, mode: resolveBuildMode(env) });
  const fs = deps.fs ?? nodeFs;
  const run = deps.run ?? defaultRun;
  const ctx = {
    appRoot, target: plan.target, stageDir: plan.stageDir, fs, env, run,
    windows: plan.target.os === "windows",
    pid: deps.pid ?? process.pid,
    log: deps.log ?? (() => {}),
    fetchBytes: deps.fetchBytes ?? defaultFetchBytes,
    extractMember: deps.extractMember ?? ((args) => defaultExtractMember({ ...args, run })),
    loadMediaCatalog: deps.loadMediaCatalog ?? (() => undefined),
    loadNativeCatalog: deps.loadNativeCatalog ?? (() => undefined),
    makeTempDir: deps.makeTempDir ?? (() => nodeFs.mkdtempSync(path.join(os.tmpdir(), "rightkit-stage-"))),
    removeDir: deps.removeDir ?? ((dir) => nodeFs.rmSync(dir, { recursive: true, force: true })),
  };
  const files = [];
  const skipped = [];
  for (const entry of plan.entries) {
    if (entry.skipped) { skipped.push({ kind: entry.kind, index: entry.index, platforms: entry.platforms }); continue; }
    const staged = entry.kind === "cargo-bin" ? stageCargoBin(entry, ctx)
      : entry.kind === "ffmpeg" ? await stageFfmpeg(entry, ctx)
        : entry.kind === "onnxruntime" ? await stageOnnxRuntime(entry, ctx)
          : entry.kind === "command" ? stageCommand(entry, ctx)
            : stageResource(entry, ctx);
    files.push(...staged);
  }
  const receipt = {
    schema: 1,
    contract: STAGE_SIDECAR_CONTRACT,
    target: plan.target.triple,
    os: plan.target.os,
    arch: plan.target.arch,
    stagedAt: (deps.now ?? (() => new Date().toISOString()))(),
    files,
    skipped,
  };
  validateStageSidecarReceipt(receipt, { target: plan.target.triple });
  const receiptFile = path.resolve(appRoot, plan.receiptPath);
  fs.mkdirSync(path.dirname(receiptFile), { recursive: true });
  fs.writeFileSync(receiptFile, `${JSON.stringify(receipt, null, 2)}\n`);
  return { receipt, receiptPath: receiptFile, files, externalBinNames: files.filter((file) => file.externalBin).map((file) => file.name) };
}

/** Structural check of a staging receipt; `expected` may pin target and names. */
export function validateStageSidecarReceipt(receipt, expected = {}) {
  const bad = (message) => fail("STAGE_SIDECAR_RECEIPT_INVALID", `stage sidecar receipt: ${message}`);
  if (!isObject(receipt) || receipt.schema !== 1 || receipt.contract !== STAGE_SIDECAR_CONTRACT) bad("unrecognized contract");
  if (!Array.isArray(receipt.files)) bad("no files array");
  for (const file of receipt.files) {
    if (typeof file.name !== "string" || !file.name || typeof file.path !== "string" || !file.path) bad("file without a name or path");
    if (!SHA256.test(file.sha256 ?? "")) bad(`${file.name}.sha256 must be a SHA-256`);
    if (!Number.isSafeInteger(file.sizeBytes) || file.sizeBytes <= 0) bad(`${file.name}.sizeBytes must be positive`);
  }
  if (new Set(receipt.files.map((file) => file.path)).size !== receipt.files.length) bad("duplicate staged path");
  if (expected.target && receipt.target !== expected.target) bad(`target ${receipt.target} is not ${expected.target}`);
  if (expected.names && JSON.stringify([...expected.names].sort()) !== JSON.stringify(receipt.files.filter((file) => file.externalBin).map((file) => file.name).sort())) bad("externalBin names differ from the configuration");
  return receipt;
}
