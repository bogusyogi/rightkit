// Declarative engine and sidecar staging for Tauri externalBin (bogusyogi/rightkit#345).
//
// Eight apps hand-rolled the same script: build an engine through `rightkit cargo`, find the executable, copy it to
// src-tauri/binaries/<name>-<target-triple>[.exe], stage ffmpeg, stage onnxruntime, copy model resources. This module is
// that script, driven by a `sidecars` list in right-release.config.mjs:
//
//   sidecars: [
//     { kind: "cargo-bin", package: "mailright-engine", bin?, name?, profile?: "release" | { dev, release }, features?, manifestPath?, sha256?, dest?, clean?, platforms? },
//     { kind: "ffmpeg", variant?, tools?, notices: { dir, sourceOfferContact }, catalog, stage?, bundleDir? },
//     { kind: "onnxruntime", version, catalog, resourceDir?: "src-tauri/resources/runtime", as?, licenseDir?, cacheDir? },
//     { kind: "resource", from, to, sha256?, machO?, clean?, platforms? },   // `from` may be a directory (copied recursively, every file hashed)
//     { kind: "command", name, cmd, args?, cwd?, output, externalBin: true | (false with dest), dest?, clean?, platforms? },
//     { kind: "remote", name, url (https), sha256, dest | to, size?, extract?, memberSha256?, executable?, clean?, platforms? },
//     { kind: "cmake", name, repo (https), commit (40 hex), target?, output?, cmakeArgs?, platformArgs?, generator?, smoke?, sha256?, dest?, clean?, platforms? },
//     { kind: "prebuilt", name, from?, manifest: { file, entry, sha256?, approvalSha256?, approvalAlgorithm? }, dest?, platforms? },
//   ]
//
// `dest` (cargo-bin, command, remote) is a contained app-relative path (never the app root, the stage dir, the receipt, the cache, .git, src, src-tauri, node_modules or package.json; outputs may not overlap) used instead of <stageDir>/<name>-<triple>; the file is
// then not an externalBin. `clean: true` removes a stale file at the destination before staging. The wrapper key
// `wipeStageDir: true` removes stale files named exactly `<name>-<current triple>[.exe]` for a declared name this run no longer produces.
//
// `catalog` is a relative path, an absolute path, { crate: "rightkit-ffmpeg", file? } (resolved through `rightkit cargo
// metadata`) or { npm: "<pkg>/<file>" }. The wrapper form { sidecars, profiles: { dev, release } } sets the default cargo
// profile pair; RIGHTKIT_BUILD_MODE (dev|release, default release) selects. The target `universal-apple-darwin` builds
// both slices of a cargo-bin and lipo-creates `<name>-universal-apple-darwin`. `ffmpeg` and `onnxruntime` do the same from their
// per-arch catalog rows (each slice staged under .right-release/slices/<triple>, then lipo-merged), a `cmake` entry builds with
// CMAKE_OSX_ARCHITECTURES="arm64;x86_64".
//
// It never signs (release.mjs signs staged files) and never calls bare cargo. Every effect goes through `deps`.
import { createHash } from "node:crypto";
import nodeFs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync as nodeSpawnSync } from "./background-process.mjs";
import { resolveCommand } from "./development.mjs";
import { managedHost } from "./managed-cargo.mjs";
import { APPROVAL_MACHO_STRIP_LINKEDIT_V1, APPROVAL_RAW_SHA256, approvalDigest } from "./macho-approval-hash.mjs";
import {
  planMediaSidecar, sourceOfferText, stagedFileName, validateMediaSidecarConfig, verifyStagedSidecars, writeMediaSidecarNotices,
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
const KINDS = new Set(["cargo-bin", "ffmpeg", "onnxruntime", "resource", "command", "remote", "cmake", "prebuilt"]);
export const NESTED_BROKER_CODE = "STAGE_SIDECAR_NESTED_BROKER";
export const REMOTE_CACHE_DIR = ".right-release/cache/remote";
/** Per-arch staging for a universal ffmpeg or onnxruntime entry: `<SLICE_ROOT>/<triple>/...`, merged with lipo afterwards. */
export const SLICE_ROOT = ".right-release/slices";
/** Pinned-commit source checkouts and build trees of `cmake` entries: `<CMAKE_ROOT>/<name>/{src,build}` (fixed paths, in place). */
export const CMAKE_ROOT = ".right-release/cmake";
const UNIVERSAL = "universal-apple-darwin";
const SLICES = Object.freeze(["aarch64-apple-darwin", "x86_64-apple-darwin"]);
const MODES = new Set(["dev", "release"]);
const PROFILE = /^[a-z][a-z0-9_-]*$/;
const PLATFORMS = new Set(["mac", "win", "linux"]);
const NAME = /^[A-Za-z0-9][A-Za-z0-9_.-]*$/;
const KEYS = Object.freeze({
  "cargo-bin": ["kind", "package", "bin", "name", "profile", "features", "manifestPath", "sha256", "dest", "clean", "platforms"],
  ffmpeg: ["kind", "variant", "variants", "tools", "notices", "catalog", "stage", "bundleDir", "sources", "probe", "platforms"],
  onnxruntime: ["kind", "version", "catalog", "resourceDir", "as", "licenseDir", "cacheDir", "platforms"],
  resource: ["kind", "from", "to", "sha256", "machO", "clean", "platforms"],
  command: ["kind", "name", "cmd", "args", "cwd", "output", "externalBin", "dest", "clean", "platforms"],
  remote: ["kind", "name", "url", "sha256", "dest", "to", "size", "extract", "memberSha256", "executable", "clean", "platforms"],
  cmake: ["kind", "name", "repo", "commit", "target", "output", "cmakeArgs", "platformArgs", "generator", "smoke", "sha256", "dest", "clean", "platforms"],
  prebuilt: ["kind", "name", "from", "manifest", "dest", "platforms"],
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

function relativePath(value, label, errors, { allowRoot = false } = {}) {
  if (typeof value !== "string" || !value.trim()) { errors.push(`${label} must be a non-empty relative path`); return undefined; }
  const normalized = value.replaceAll("\\", "/");
  if (path.isAbsolute(value) || /^[A-Za-z]:/.test(value) || normalized.split("/").includes("..")) { errors.push(`${label} must be relative and stay inside the app: ${value}`); return undefined; }
  // "." and "./" name the app root itself: only a working directory may do that, never a destination or a directory to clean.
  if (!normalized.split("/").some((segment) => segment !== "" && segment !== ".")) {
    if (allowRoot) return ".";
    errors.push(`${label} must name a path inside the app (at least one normal segment), not the app root: ${value}`);
    return undefined;
  }
  return normalized.replace(/\/+$/, "");
}

/** A relative output path in comparison form: no "." or empty segments, lower case (Windows and macOS are case-insensitive). */
const canonicalOutput = (value) => String(value).replaceAll("\\", "/").split("/").filter((segment) => segment !== "" && segment !== ".").join("/").toLowerCase();
/** `output` equals `protectedPath` or is an ancestor of it (so writing or cleaning `output` would reach it). */
const reaches = (output, protectedPath) => output === protectedPath || protectedPath.startsWith(`${output}/`);

/**
 * Paths stage-sidecar never writes to or cleans, app-relative: the stage directory, the receipt, the remote cache and the
 * `.right-release` state, onnxruntime cache directories, and the app's source and dependency roots. A destination that equals
 * or contains one of these is refused (validation, and again just before any delete).
 */
function protectedOutputPaths({ stageDir, receiptPath, entries = [] }) {
  const cacheDirs = entries.filter((entry) => entry.kind === "onnxruntime" && entry.cacheDir).map((entry) => entry.cacheDir);
  return [stageDir, receiptPath, ".right-release", REMOTE_CACHE_DIR, ".git", "src", "src-tauri", "node_modules", "package.json", ...cacheDirs].filter(Boolean).map(canonicalOutput);
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

const CMAKE_DEFINE = /^-D[A-Za-z0-9_]+(?::[A-Z]+)?=[^\s]*$/;
const SOURCE_ARCHES = ["aarch64", "x86_64"];

/** One pinned https archive: { url, sha256, member, size? }. Problems are pushed to `errors`. */
function parseArchiveSource(value, label, errors) {
  if (!isObject(value)) { errors.push(`${label} must be { url, sha256, member, size? }`); return undefined; }
  for (const key of Object.keys(value)) if (!["url", "sha256", "member", "size"].includes(key)) errors.push(`${label}: unknown key ${JSON.stringify(key)}`);
  if (typeof value.url !== "string" || !/^https:\/\/[^\s/]+\/\S*$/.test(value.url)) errors.push(`${label}.url must be an https:// URL (no other scheme is fetched)`);
  if (typeof value.sha256 !== "string" || !SHA256.test(value.sha256)) errors.push(`${label}.sha256 is required and must be a lowercase 64-hex SHA-256 of the archive`);
  if (typeof value.member !== "string" || !NAME.test(value.member)) errors.push(`${label}.member must be the bare file name of one archive member (for example ffmpeg or ffmpeg.exe)`);
  if (value.size !== undefined && (!Number.isSafeInteger(value.size) || value.size <= 0)) errors.push(`${label}.size must be a positive integer (bytes)`);
  return { url: value.url, sha256: value.sha256, member: value.member, ...(value.size ? { size: value.size } : {}) };
}

/**
 * `ffmpeg.sources`: where to download the bytes of a catalog row (the catalog pins digests, not URLs). A tool maps to one
 * archive source, or to `{ aarch64: source, x86_64: source }`; a universal target needs the per-arch form for every tool.
 */
function parseFfmpegSources(raw, where, errors, { universal }) {
  if (raw === undefined) return undefined;
  if (!isObject(raw) || !Object.keys(raw).length) { errors.push(`${where}: sources must be an object mapping ffmpeg or ffprobe to an archive source`); return undefined; }
  const out = {};
  for (const [tool, value] of Object.entries(raw)) {
    const label = `${where}: sources.${tool}`;
    if (!["ffmpeg", "ffprobe"].includes(tool)) { errors.push(`${label}: not ffmpeg or ffprobe`); continue; }
    const perArch = isObject(value) && SOURCE_ARCHES.some((arch) => arch in value) && !("url" in value);
    if (universal && !perArch) { errors.push(`${label} needs { aarch64, x86_64 } sources for universal-apple-darwin (each slice is a thin file)`); continue; }
    if (perArch) {
      for (const key of Object.keys(value)) if (!SOURCE_ARCHES.includes(key)) errors.push(`${label}: unknown arch ${JSON.stringify(key)}`);
      out[tool] = Object.fromEntries(SOURCE_ARCHES.filter((arch) => arch in value).map((arch) => [arch, parseArchiveSource(value[arch], `${label}.${arch}`, errors)]));
    } else out[tool] = parseArchiveSource(value, label, errors);
  }
  return out;
}

/**
 * `prebuilt.manifest`: { file, entry, sha256?, approvalSha256?, approvalAlgorithm? }. `file` is a JSON file inside the app, `entry`
 * a dot path to the object for this target (`{platform}` becomes macos-aarch64, windows-x86_64 and so on), and the three keys name
 * the fields inside it (defaults: sha256 -> "sha256"; approval fields only when named).
 */
function parsePrebuiltManifest(raw, where, errors) {
  if (!isObject(raw)) { errors.push(`${where}: manifest must be { file, entry, sha256?, approvalSha256?, approvalAlgorithm? }`); return undefined; }
  for (const key of Object.keys(raw)) if (!["file", "entry", "sha256", "approvalSha256", "approvalAlgorithm"].includes(key)) errors.push(`${where}: manifest: unknown key ${JSON.stringify(key)}`);
  const file = relativePath(raw.file, `${where}: manifest.file`, errors);
  if (typeof raw.entry !== "string" || !/^[A-Za-z0-9_{}.-]+$/.test(raw.entry) || raw.entry.split(".").some((part) => part === "")) errors.push(`${where}: manifest.entry must be a dot path such as binaries.{platform}`);
  for (const key of ["sha256", "approvalSha256", "approvalAlgorithm"]) if (raw[key] !== undefined && (typeof raw[key] !== "string" || !/^[A-Za-z0-9_-]+$/.test(raw[key]))) errors.push(`${where}: manifest.${key} must be a field name`);
  if (raw.approvalAlgorithm !== undefined && raw.approvalSha256 === undefined) errors.push(`${where}: manifest.approvalAlgorithm needs approvalSha256`);
  if (!file) return undefined;
  return { file, entry: raw.entry, sha256: raw.sha256 ?? "sha256", ...(raw.approvalSha256 ? { approvalSha256: raw.approvalSha256 } : {}), ...(raw.approvalAlgorithm ? { approvalAlgorithm: raw.approvalAlgorithm } : {}) };
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
  for (const key of Object.keys(wrapper)) if (!["sidecars", "stageDir", "receiptPath", "profiles", "wipeStageDir"].includes(key)) errors.push(`unknown key ${JSON.stringify(key)}`);
  if (wrapper.wipeStageDir !== undefined && typeof wrapper.wipeStageDir !== "boolean") errors.push("wipeStageDir must be a boolean");
  const defaultProfile = wrapper.profiles === undefined ? undefined : parseProfile(wrapper.profiles, "profiles", errors);
  if (typeof defaultProfile === "string") errors.push("profiles must be { dev, release }");
  if (!Array.isArray(wrapper.sidecars) || !wrapper.sidecars.length) errors.push("sidecars must be a non-empty array");
  const stageDir = relativePath(wrapper.stageDir ?? DEFAULT_STAGE_DIR, "stageDir", errors);
  const receiptPath = wrapper.receiptPath === undefined
    ? (stageDir ? `${stageDir}/${STAGE_SIDECAR_RECEIPT_NAME}` : undefined)
    : relativePath(wrapper.receiptPath, "receiptPath", errors);
  const entries = [];
  const claimed = [];
  const claim = (output, label) => {
    const key = canonicalOutput(output);
    const clash = claimed.find((other) => other.key === key || other.key.startsWith(`${key}/`) || key.startsWith(`${other.key}/`));
    if (clash?.key === key) errors.push(`${label} writes ${output}, which ${clash.label} already writes`);
    else if (clash) errors.push(`${label} writes ${output}, which overlaps ${clash.output} that ${clash.label} writes (outputs may not be nested inside each other)`);
    else claimed.push({ key, output, label });
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
    const destOf = (value, field = "dest") => (value === undefined ? undefined : relativePath(value, `${where}: ${field}`, errors));
    const cleanOf = () => {
      if (item.clean !== undefined && typeof item.clean !== "boolean") errors.push(`${where}: clean must be a boolean`);
      return item.clean === true;
    };
    const stagedClaim = (name) => `${stageDir}/${stagedName(name, t)}`;
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
      const dest = destOf(item.dest);
      const clean = cleanOf();
      if (!skipped) { if (dest) claim(dest, where); else if (typeof name === "string" && stageDir) claim(stagedClaim(name), where); }
      Object.assign(entry, { package: item.package, bin, name, profile, features: [...features], ...(manifestPath ? { manifestPath } : {}), ...(item.sha256 ? { sha256: item.sha256 } : {}), ...(dest ? { dest } : {}), ...(clean ? { clean } : {}) });
    } else if (item.kind === "ffmpeg") {
      const { kind: _kind, platforms: _platforms, catalog: rawCatalog, sources: rawSources, probe: rawProbe, ...rest } = item;
      if (rest.stage !== undefined && isObject(rest.stage) && rest.stage.cmd === "cargo") errors.push(`${where}: stage.cmd must not be bare cargo; use { cmd: "rightkit", args: ["cargo", ...] }`);
      const catalogSpec = parseCatalogSpec(rawCatalog, `${where}: catalog`, errors, "ffmpeg");
      if (catalogSpec) entry.catalogSpec = catalogSpec;
      if (rawProbe !== undefined && typeof rawProbe !== "boolean") errors.push(`${where}: probe must be a boolean`);
      if (rawProbe === true) entry.probe = true;
      const sources = parseFfmpegSources(rawSources, where, errors, { universal: t.arch === "universal" });
      if (sources) entry.sources = sources;
      if (!skipped) {
        const mediaErrors = (error) => errors.push(...String(error.message).split("\n").filter((line) => line.startsWith("- ")).map((line) => `${where}: ${line.slice(2)}`).concat(error.message.startsWith("invalid") ? [] : [`${where}: ${error.message}`]));
        if (t.arch === "universal") {
          // One staging pass per arch slice (under SLICE_ROOT), then a lipo merge into <stageDir>/<tool>-universal-apple-darwin.
          try {
            entry.slices = SLICES.map((triple) => {
              const slice = parseTargetTriple(triple);
              return { triple, arch: slice.arch, media: validateMediaSidecarConfig({ ...rest, arch: slice.arch, stageDir: `${SLICE_ROOT}/${triple}` }, { platform: t.platform, hostArch: slice.arch }) };
            });
            entry.media = entry.slices[0].media;
            for (const tool of entry.media.tools) claim(stagedClaim(tool), where);
          } catch (error) { mediaErrors(error); }
        } else {
          try {
            const config = validateMediaSidecarConfig({ ...rest, arch: t.arch, stageDir }, { platform: t.platform, hostArch: t.arch });
            for (const tool of config.tools) claim(`${stageDir}/${stagedFileName(tool, t.os, t.arch)}`, where);
            entry.media = config;
          } catch (error) { mediaErrors(error); }
        }
      }
    } else if (item.kind === "onnxruntime") {
      if (typeof item.version !== "string" || !/^\d+\.\d+\.\d+$/.test(item.version)) errors.push(`${where}: version must be x.y.z`);
      const resourceDir = relativePath(item.resourceDir ?? DEFAULT_ORT_RESOURCE_DIR, `${where}: resourceDir`, errors);
      const licenseDir = relativePath(item.licenseDir ?? (resourceDir ? `${resourceDir}/LICENSES` : "x"), `${where}: licenseDir`, errors);
      const cacheDir = relativePath(item.cacheDir ?? ".cache/onnxruntime", `${where}: cacheDir`, errors);
      const catalogSpec = parseCatalogSpec(item.catalog, `${where}: catalog`, errors, "onnxruntime");
      if (item.as !== undefined && (typeof item.as !== "string" || !NAME.test(item.as))) errors.push(`${where}: as must be a bare file name`);
      Object.assign(entry, { version: item.version, resourceDir, licenseDir, cacheDir, ...(catalogSpec ? { catalogSpec } : {}), ...(item.as ? { as: item.as } : {}) });
    } else if (item.kind === "command") {
      const name = item.name;
      if (typeof name !== "string" || !NAME.test(name) || /\.exe$/i.test(name) || name !== path.basename(name)) errors.push(`${where}: name must be a bare sidecar name (no path, no .exe, no target triple)`);
      if (typeof item.cmd !== "string" || !item.cmd.trim() || /\s/.test(item.cmd)) errors.push(`${where}: cmd must be one executable name or path with no spaces (arguments go in args; no shell is used)`);
      const args = item.args ?? [];
      if (!Array.isArray(args) || args.some((a) => typeof a !== "string")) errors.push(`${where}: args must be a list of strings`);
      const cwd = item.cwd === undefined ? undefined : relativePath(item.cwd, `${where}: cwd`, errors, { allowRoot: true });
      const output = relativePath(item.output, `${where}: output`, errors);
      const dest = destOf(item.dest);
      const clean = cleanOf();
      if (dest) { if (item.externalBin === true) errors.push(`${where}: externalBin must be false or omitted with dest (a dest file is not staged as <name>-<triple>)`); }
      else if (item.externalBin !== true) errors.push(`${where}: externalBin must be true (the output is staged as <name>-<triple>)`);
      if (!skipped) { if (dest) claim(dest, where); else if (typeof name === "string" && stageDir) claim(stagedClaim(name), where); }
      Object.assign(entry, { name, cmd: item.cmd, args: Array.isArray(args) ? [...args] : [], ...(cwd ? { cwd } : {}), output, ...(dest ? { dest } : {}), ...(clean ? { clean } : {}) });
    } else if (item.kind === "remote") {
      const name = item.name;
      if (typeof name !== "string" || !NAME.test(name)) errors.push(`${where}: name must be a bare label`);
      if (typeof item.url !== "string" || !/^https:\/\/[^\s/]+\/\S*$/.test(item.url)) errors.push(`${where}: url must be an https:// URL (no other scheme is fetched)`);
      if (typeof item.sha256 !== "string" || !SHA256.test(item.sha256)) errors.push(`${where}: sha256 is required and must be a lowercase 64-hex SHA-256`);
      if (item.dest !== undefined && item.to !== undefined) errors.push(`${where}: give dest or to, not both`);
      if (item.dest === undefined && item.to === undefined) errors.push(`${where}: dest (or to) is required`);
      const dest = destOf(item.dest ?? item.to, item.dest === undefined ? "to" : "dest");
      if (item.size !== undefined && (!Number.isSafeInteger(item.size) || item.size <= 0)) errors.push(`${where}: size must be a positive integer (bytes)`);
      if (item.extract !== undefined && (typeof item.extract !== "string" || !NAME.test(item.extract))) errors.push(`${where}: extract must be the bare file name of one archive member (no path)`);
      if (item.memberSha256 !== undefined && (item.extract === undefined || !SHA256.test(item.memberSha256 ?? ""))) errors.push(`${where}: memberSha256 must be a lowercase 64-hex SHA-256 and needs extract`);
      if (item.executable !== undefined && typeof item.executable !== "boolean") errors.push(`${where}: executable must be a boolean`);
      const clean = cleanOf();
      if (dest && !skipped) claim(dest, where);
      Object.assign(entry, { name, url: item.url, sha256: item.sha256, dest, ...(item.size ? { size: item.size } : {}), ...(item.extract ? { extract: item.extract } : {}), ...(item.memberSha256 ? { memberSha256: item.memberSha256 } : {}), ...(item.executable ? { executable: true } : {}), ...(clean ? { clean } : {}) });
    } else if (item.kind === "cmake") {
      const name = item.name;
      if (typeof name !== "string" || !NAME.test(name) || /\.exe$/i.test(name) || name !== path.basename(name)) errors.push(`${where}: name must be a bare sidecar name (no path, no .exe, no target triple)`);
      if (typeof item.repo !== "string" || !/^https:\/\/[^\s/]+\/\S*$/.test(item.repo)) errors.push(`${where}: repo must be an https:// URL (no other scheme is fetched)`);
      if (typeof item.commit !== "string" || !/^[0-9a-f]{40}$/.test(item.commit)) errors.push(`${where}: commit must be a full lowercase 40-hex commit id (a branch or tag is not a pin)`);
      const target = item.target ?? name;
      if (typeof target !== "string" || !NAME.test(target)) errors.push(`${where}: target must be a build target name`);
      let output;
      if (item.output !== undefined) output = relativePath(item.output, `${where}: output`, errors);
      const flagList = (value, label) => {
        if (value === undefined) return [];
        if (!Array.isArray(value) || value.some((arg) => typeof arg !== "string" || !CMAKE_DEFINE.test(arg))) { errors.push(`${where}: ${label} must be a list of -DNAME=value definitions (no shell, no other flags)`); return []; }
        return [...value];
      };
      const cmakeArgs = flagList(item.cmakeArgs, "cmakeArgs");
      const platformArgs = {};
      if (item.platformArgs !== undefined) {
        if (!isObject(item.platformArgs)) errors.push(`${where}: platformArgs must be an object keyed by mac, win or linux`);
        else for (const [key, list] of Object.entries(item.platformArgs)) {
          if (!PLATFORMS.has(key)) errors.push(`${where}: platformArgs.${key}: platforms are ${[...PLATFORMS].join(", ")}`);
          else platformArgs[key] = flagList(list, `platformArgs.${key}`);
        }
      }
      if (item.generator !== undefined && (typeof item.generator !== "string" || !/^[A-Za-z0-9][A-Za-z0-9 .+-]{0,60}$/.test(item.generator))) errors.push(`${where}: generator must be a generator name such as "Ninja"`);
      if (item.smoke !== undefined && (!Array.isArray(item.smoke) || item.smoke.some((arg) => typeof arg !== "string"))) errors.push(`${where}: smoke must be a list of argument strings`);
      sha(item.sha256, "sha256");
      const dest = destOf(item.dest);
      const clean = cleanOf();
      if (!skipped) { if (dest) claim(dest, where); else if (typeof name === "string" && stageDir) claim(stagedClaim(name), where); }
      Object.assign(entry, { name, repo: item.repo, commit: item.commit, target, ...(output ? { output } : {}), cmakeArgs, platformArgs, ...(item.generator ? { generator: item.generator } : {}), ...(item.smoke ? { smoke: [...item.smoke] } : {}), ...(item.sha256 ? { sha256: item.sha256 } : {}), ...(dest ? { dest } : {}), ...(clean ? { clean } : {}) });
    } else if (item.kind === "prebuilt") {
      const name = item.name;
      if (typeof name !== "string" || !NAME.test(name) || /\.exe$/i.test(name) || name !== path.basename(name)) errors.push(`${where}: name must be a bare sidecar name (no path, no .exe, no target triple)`);
      const from = item.from === undefined ? undefined : relativePath(item.from, `${where}: from`, errors);
      const manifest = parsePrebuiltManifest(item.manifest, where, errors);
      if (!skipped && t.arch === "universal") errors.push(`${where}: a prebuilt binary is verified per architecture; there is no universal manifest entry`);
      const dest = destOf(item.dest);
      if (!skipped) { if (dest) claim(dest, where); else if (typeof name === "string" && stageDir) claim(stagedClaim(name), where); }
      Object.assign(entry, { name, ...(from ? { from } : {}), ...(manifest ? { manifest } : {}), ...(dest ? { dest } : {}) });
    } else {
      const from = relativePath(item.from, `${where}: from`, errors);
      const to = relativePath(item.to, `${where}: to`, errors);
      sha(item.sha256, "sha256");
      if (item.machO !== undefined && typeof item.machO !== "boolean") errors.push(`${where}: machO must be a boolean`);
      const clean = cleanOf();
      if (to && !skipped) claim(to, where);
      Object.assign(entry, { from, to, ...(clean ? { clean } : {}),...(item.sha256 ? { sha256: item.sha256 } : {}), ...(item.machO ? { machO: true } : {}) });
    }
    entries.push(entry);
  }
  const protectedPaths = protectedOutputPaths({ stageDir, receiptPath, entries });
  for (const { key, output, label } of claimed) {
    const hit = protectedPaths.find((protectedPath) => reaches(key, protectedPath));
    if (hit) errors.push(`${label} writes ${output}, which is or contains ${hit}; stage-sidecar never overwrites or cleans the app's stage directory, receipt, cache, .git, src, src-tauri, node_modules or package.json`);
  }
  if (errors.length) fail("STAGE_SIDECAR_CONFIG_INVALID", `invalid sidecars configuration:\n${errors.map((line) => `- ${line}`).join("\n")}`, { errors });
  return { target: t, stageDir, receiptPath, wipeStageDir: wrapper.wipeStageDir === true, entries };
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
export function builtBinaryCandidates({ targetDir, targetDirs, triple, profile, bin, windows }) {
  const file = windows ? `${bin}.exe` : bin;
  const dir = profileDirectory(profile);
  const roots = [...new Set([...(targetDirs ?? []), ...(targetDir ? [targetDir] : [])])];
  return roots.flatMap((root) => [path.join(root, triple, dir, file), path.join(root, dir, file)]);
}

/**
 * `clean: true` removes a stale file at the destination before staging. A directory is removed recursively only when the
 * entry is a directory resource (`directory: true`); any other destination is removed non-recursively. Refused (typed,
 * nothing deleted): the app root or anything outside it, a path equal to or containing a protected path (the stage
 * directory, receipt, cache, .right-release, .git, src, src-tauri, node_modules, package.json), and a destination whose
 * parent resolves (through a symlink) outside the app root or onto a protected path.
 */
function cleanDest(ctx, entry, dest, { directory = false } = {}) {
  if (!entry.clean) return;
  const refuse = (reason) => fail("STAGE_SIDECAR_CLEAN_REFUSED", `refusing to clean ${dest}: ${reason}`, { dest });
  const protectedPaths = ctx.protectedPaths ?? [];
  const insideApp = (root, file) => { const relative = path.relative(root, file); return relative !== "" && relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative) ? relative : undefined; };
  const check = (relative, what) => {
    const key = canonicalOutput(relative.split(path.sep).join("/"));
    const hit = protectedPaths.find((protectedPath) => reaches(key, protectedPath));
    if (hit) refuse(`${what} is or contains the protected path ${hit}`);
  };
  const relative = insideApp(ctx.appRoot, dest);
  if (!relative) refuse("it is the app root or outside the app");
  check(relative, "it");
  const realpath = ctx.fs.realpathSync;
  if (typeof realpath === "function") {
    const resolveExisting = (start) => {
      for (let current = start; ; current = path.dirname(current)) {
        try { return realpath(current); } catch { if (path.dirname(current) === current) return undefined; }
      }
    };
    const realRoot = resolveExisting(ctx.appRoot);
    const realParent = resolveExisting(path.dirname(dest));
    if (!realRoot || !realParent) refuse("its parent directory cannot be resolved");
    if (realParent !== realRoot && !insideApp(realRoot, realParent)) refuse("its parent directory resolves outside the app (a symlink)");
    const realRelative = insideApp(realRoot, path.join(realParent, path.basename(dest)));
    if (!realRelative) refuse("it resolves to the app root");
    check(realRelative, "its resolved location");
  }
  ctx.log(`stage-sidecar: clean ${rel(ctx, dest)}`);
  try { ctx.fs.rmSync(dest, { recursive: directory, force: true }); } catch (error) { fail("STAGE_SIDECAR_CLEAN_REFUSED", `cannot clean ${dest} (${error.message}); a directory is only removed for a directory resource`, { dest, cause: error }); }
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
  } else if (executable && !windows) fs.chmodSync(dest, 0o755);
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

/**
 * lipo-create `slices` into `dest` (a universal Mach-O): temp file, both archs required, written only when the bytes
 * changed (an identical file is not rewritten, so Tauri's rerun-if-changed is not triggered needlessly).
 */
function lipoMergeInto(ctx, slices, dest, label, { executable = false } = {}) {
  const { fs, run, appRoot, env } = ctx;
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  const temporary = `${dest}.tmp-${ctx.pid}`;
  const args = ["-create", "-output", temporary, ...slices];
  ctx.log(`stage-sidecar: lipo ${args.join(" ")}`);
  const merged = run("lipo", args, { cwd: appRoot, env });
  if (merged?.error || merged?.status !== 0) fail("STAGE_SIDECAR_BUILD_FAILED", `lipo -create of ${label} failed (${merged?.error?.message ?? `exit ${merged?.status}`})`, { stderr: tail(merged?.stderr) });
  requireUniversalFile(ctx, temporary, `${label} after lipo`);
  const sha256 = sha256Of(fs.readFileSync(temporary));
  const changed = !(fs.existsSync(dest) && sha256Of(fs.readFileSync(dest)) === sha256);
  if (executable) fs.chmodSync(temporary, 0o755);
  if (changed) fs.renameSync(temporary, dest);
  else {
    fs.rmSync(temporary, { force: true });
    if (executable) fs.chmodSync(dest, 0o755);
  }
  return { sha256, sizeBytes: fs.statSync(dest).size, changed };
}

/** Can a binary built for `t` run on this host (so a probe or smoke run is meaningful)? Cross-target builds are not run. */
function canRunTarget(ctx, t) {
  const host = ctx.host;
  if (t.arch === "universal") return host.os === "macos";
  return t.os === host.os && t.arch === host.arch;
}

const destFor = (entry, ctx) => (entry.dest ? path.resolve(ctx.appRoot, entry.dest) : path.resolve(ctx.appRoot, ctx.stageDir, stagedName(entry.name, ctx.target)));

function stageCargoBin(entry, ctx) {
  const dest = destFor(entry, ctx);
  cleanDest(ctx, entry, dest);
  if (ctx.target.arch !== "universal") return stageCargoSlice(entry, ctx, ctx.target.triple, { dest });
  const slices = SLICES.map((triple) => ({ triple, ...stageCargoBuild(entry, ctx, triple) }));
  const { fs, run, appRoot, env } = ctx;
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
  return [{ kind: "cargo-bin", name: entry.name, path: rel(ctx, dest), sha256, sizeBytes: fs.statSync(dest).size, changed, sourceSha256: sha256, package: entry.package, bin: entry.bin, profile: entry.profile, features: entry.features, externalBin: !entry.dest, slices: slices.map((slice) => ({ triple: slice.triple, sha256: slice.sourceSha256 })) }];
}

function stageCargoSlice(entry, ctx, triple, { dest }) {
  const { source, sourceSha256 } = stageCargoBuild(entry, ctx, triple);
  if (entry.sha256 && entry.sha256 !== sourceSha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${entry.bin} built to ${sourceSha256}, expected ${entry.sha256}`, { expected: entry.sha256, actual: sourceSha256 });
  const copied = copyInto(ctx, source, dest, { executable: true });
  return [{ kind: "cargo-bin", name: entry.name, path: rel(ctx, dest), ...copied, sourceSha256, package: entry.package, bin: entry.bin, profile: entry.profile, features: entry.features, externalBin: !entry.dest }];
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
    if (meta?.status !== 0 || typeof targetDir !== "string" || !path.isAbsolute(targetDir)) targetDir = undefined;
    // A managed target root: the broker/cargo metadata target_directory (which cargo derives from CARGO_TARGET_DIR) is
    // authoritative; an explicit CARGO_TARGET_DIR in this environment is tried too, resolved against the app root.
    const envDir = typeof env.CARGO_TARGET_DIR === "string" && env.CARGO_TARGET_DIR.trim() ? path.resolve(appRoot, env.CARGO_TARGET_DIR) : undefined;
    if (!targetDir && !envDir) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `cargo reported no ${entry.bin} executable and its target directory cannot be read`, { bin: entry.bin });
    const candidates = builtBinaryCandidates({ targetDir, targetDirs: envDir ? [envDir] : [], triple, profile: entry.profile, bin: entry.bin, windows: ctx.windows });
    source = candidates.find((file) => fs.existsSync(file));
    if (!source) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `the build did not produce ${entry.bin} (looked in ${candidates.join(", ")})`, { bin: entry.bin, candidates });
  }
  const sourceSha256 = sha256Of(fs.readFileSync(source));
  return { source, sourceSha256 };
}

/** The archive source for one tool at one arch: a single source, or the per-arch form. */
const ffmpegSourceFor = (sources, tool, arch) => { const spec = sources?.[tool]; return spec?.url ? spec : spec?.[arch]; };

/**
 * Download the bytes of the catalog rows that `ffmpeg.sources` names: an archive pinned by digest, one member unpacked, the
 * member's digest checked against the APPROVED catalog row (so a swapped archive can never stage). A staged file that already
 * matches its row is left alone.
 */
async function fetchFfmpegSources(entry, ctx, plan, arch) {
  if (!entry.sources) return;
  const { fs } = ctx;
  for (const file of plan.files) {
    const spec = ffmpegSourceFor(entry.sources, file.tool, arch);
    if (!spec) continue;
    if (fs.existsSync(file.stagedPath) && sha256Of(fs.readFileSync(file.stagedPath)) === file.contentSha256) { ctx.log(`stage-sidecar: ${rel(ctx, file.stagedPath)} already staged`); continue; }
    const { cached } = await ensureCachedDownload(ctx, spec);
    const scratch = ctx.makeTempDir();
    try {
      const member = await ctx.extractMember({ archive: cached, fileName: spec.member, destDir: scratch });
      const actual = sha256Of(fs.readFileSync(member));
      if (actual !== file.contentSha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${spec.member} from ${spec.url} is ${actual}, the approved catalog row ${file.artifactId} says ${file.contentSha256}; nothing staged`, { expected: file.contentSha256, actual });
      copyInto(ctx, member, file.stagedPath, { executable: true });
    } finally {
      ctx.removeDir(scratch);
    }
  }
}

/**
 * Optional (`probe: true`) LGPL and encoder probes of a staged ffmpeg or ffprobe, restored from the apps' own stage scripts: the
 * `configuration:` line of `-version` must say --disable-gpl and --disable-nonfree and must not enable GPL, nonfree or the GPL
 * encoders; ffmpeg's `-encoders` list must not carry libx264, libx265, libxvid or libfdk_aac. A binary built for another
 * architecture or OS than this host is not run: the row says `skipped-cross-target`. Returns "passed" or "skipped-cross-target".
 */
function probeFfmpegBinary(ctx, { file, tool, target }) {
  if (!canRunTarget(ctx, target)) return "skipped-cross-target";
  const text = (result, what) => {
    if (result?.error || result?.status !== 0) fail("STAGE_SIDECAR_FFMPEG_PROBE_FAILED", `${tool} ${what} failed (${result?.error?.message ?? `exit ${result?.status}`})`, { file });
    return String(result.stdout ?? "");
  };
  const version = text(ctx.run(file, ["-version"], { cwd: ctx.appRoot, env: ctx.env }), "-version");
  const configuration = /^configuration:(.*)$/m.exec(version)?.[1] ?? "";
  for (const flag of ["--disable-gpl", "--disable-nonfree"]) if (!configuration.includes(flag)) fail("STAGE_SIDECAR_FFMPEG_PROBE_FAILED", `${tool} was not built with ${flag}; the LGPL redistribution terms do not hold`, { file });
  const enabled = /--enable-(gpl|nonfree|libx264|libx265|libxvid|libfdk-aac)\b/.exec(configuration);
  if (enabled) fail("STAGE_SIDECAR_FFMPEG_PROBE_FAILED", `${tool} was built with --enable-${enabled[1]}; it cannot ship under the LGPL`, { file });
  if (tool === "ffmpeg") {
    const encoders = text(ctx.run(file, ["-hide_banner", "-encoders"], { cwd: ctx.appRoot, env: ctx.env }), "-encoders");
    const gpl = /\b(libx264|libx264rgb|libx265|libxvid|libfdk_aac)\b/.exec(encoders);
    if (gpl) fail("STAGE_SIDECAR_FFMPEG_PROBE_FAILED", `ffmpeg lists the ${gpl[1]} encoder; it cannot ship under the LGPL`, { file });
  }
  return "passed";
}

/**
 * Stage one architecture of an ffmpeg entry: plan the catalog rows, fetch declared sources, run the optional stage command,
 * write the notices (unless a universal merge writes them once for every slice), verify each staged file against its row.
 * Returns { plan, rows }; each row is a receipt row.
 */
async function stageFfmpegSlice(entry, ctx, resolved, { config, t, writeNotices }) {
  const { appRoot, run, env, fs } = ctx;
  const plan = planMediaSidecar({ config, catalog: resolved.catalog, crateDir: resolved.crateDir, crateVersion: resolved.crateVersion, catalogBytes: resolved.catalogBytes, root: appRoot });
  await fetchFfmpegSources(entry, ctx, plan, t.arch);
  if (config.stage) {
    const universal = ctx.target.arch === "universal";
    // A universal entry stages once per slice: `{arch}`, `{triple}` and `{stageDir}` in the stage command's arguments name the slice.
    const args = (config.stage.args ?? []).map((arg) => (universal ? String(arg).replaceAll("{arch}", t.arch).replaceAll("{triple}", t.triple).replaceAll("{stageDir}", config.stageDir) : arg));
    ctx.log(`stage-sidecar: ${config.stage.cmd} ${args.join(" ")}`);
    const staged = run(config.stage.cmd, args, { cwd: appRoot, env });
    if (staged?.error || staged?.status !== 0) fail("STAGE_SIDECAR_BUILD_FAILED", `ffmpeg stage command failed (${staged?.error?.message ?? `exit ${staged?.status}`})`);
  }
  const noticeFiles = writeNotices ? writeMediaSidecarNotices(plan, { exists: fs.existsSync, mkdir: fs.mkdirSync, copy: fs.copyFileSync, write: fs.writeFileSync }) : [];
  const verified = verifyStagedSidecars(plan, { exists: fs.existsSync, stat: (file) => fs.statSync(file), hashFile: (file) => sha256Of(fs.readFileSync(file)) });
  const notices = noticeFiles.map((file) => ({ name: path.basename(file), path: rel(ctx, file), sha256: sha256Of(fs.readFileSync(file)) }));
  const rows = verified.map((file) => {
    if (file.preSignSha256 !== file.contentSha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${file.tool} staged as ${file.preSignSha256}, the approved catalog row ${file.artifactId} says ${file.contentSha256}`, { expected: file.contentSha256, actual: file.preSignSha256 });
    const probe = entry.probe ? probeFfmpegBinary(ctx, { file: file.stagedPath, tool: file.tool, target: t }) : undefined;
    return { kind: "ffmpeg", name: file.tool, path: rel(ctx, file.stagedPath), sha256: file.preSignSha256, sizeBytes: file.preSignSizeBytes, changed: null, catalogRow: file.artifactId, version: file.version, catalogContentSha256: file.contentSha256, ...(file.variant ? { variant: file.variant } : {}), externalBin: true, license: "LGPL-2.1-or-later", notices, ...(probe ? { probe } : {}) };
  });
  return { plan, rows };
}

/** universal-apple-darwin: stage each arch slice under SLICE_ROOT, then lipo-merge every tool into <stageDir>/<tool>-universal-apple-darwin. */
async function stageFfmpegUniversal(entry, ctx, resolved) {
  const { appRoot, fs } = ctx;
  const results = [];
  for (const slice of entry.slices) {
    results.push({ slice, ...(await stageFfmpegSlice(entry, ctx, resolved, { config: slice.media, t: parseTargetTriple(slice.triple), writeNotices: false })) });
  }
  // One notice set and one source offer that names the rows of every slice (a per-slice offer would overwrite the other).
  const noticeByDest = new Map();
  for (const { plan } of results) for (const notice of plan.notices) noticeByDest.set(notice.dest, notice);
  const rowsById = new Map();
  for (const { plan } of results) for (const row of plan.rows) rowsById.set(row.id, row);
  const combined = {
    notices: [...noticeByDest.values()],
    offerPath: results[0].plan.offerPath,
    offer: sourceOfferText({ rows: [...rowsById.values()], contact: entry.media.notices.sourceOfferContact, crateVersion: resolved.crateVersion }),
  };
  const noticeFiles = writeMediaSidecarNotices(combined, { exists: fs.existsSync, mkdir: fs.mkdirSync, copy: fs.copyFileSync, write: fs.writeFileSync });
  const notices = noticeFiles.map((file) => ({ name: path.basename(file), path: rel(ctx, file), sha256: sha256Of(fs.readFileSync(file)) }));
  return entry.media.tools.map((tool) => {
    const parts = results.map(({ slice, rows }) => ({ slice, row: rows.find((row) => row.name === tool) }));
    const dest = path.resolve(appRoot, ctx.stageDir, stagedName(tool, ctx.target));
    const merged = lipoMergeInto(ctx, parts.map(({ row }) => path.resolve(appRoot, row.path)), dest, `ffmpeg sidecar ${tool}`, { executable: true });
    const probe = entry.probe ? probeFfmpegBinary(ctx, { file: dest, tool, target: ctx.target }) : undefined;
    return {
      kind: "ffmpeg", name: tool, path: rel(ctx, dest), ...merged, sourceSha256: merged.sha256, catalogRow: parts.map(({ row }) => row.catalogRow).join("+"), version: parts[0].row.version,
      externalBin: true, license: "LGPL-2.1-or-later", notices, ...(probe ? { probe } : {}),
      slices: parts.map(({ slice, row }) => ({ triple: slice.triple, sha256: row.sha256, catalogRow: row.catalogRow, catalogContentSha256: row.catalogContentSha256 })),
    };
  });
}

async function stageFfmpeg(entry, ctx) {
  const { appRoot, fs, target } = ctx;
  const config = entry.media;
  try {
    let resolved = await ctx.loadMediaCatalog({ config, appRoot, entry });
    if (!resolved) {
      if (!entry.catalogSpec) fail("STAGE_SIDECAR_CATALOG_UNREADABLE", "ffmpeg needs a catalog (the rightkit-ffmpeg artifacts.json: a path, { crate } or { npm }) or deps.loadMediaCatalog");
      resolved = readJsonCatalog(resolveCatalogFile(entry.catalogSpec, ctx, "ffmpeg"), appRoot, fs, "rightkit-ffmpeg");
    }
    if (target.arch === "universal") return await stageFfmpegUniversal(entry, ctx, resolved);
    return (await stageFfmpegSlice(entry, ctx, resolved, { config, t: target, writeNotices: true })).rows;
  } catch (error) {
    if (error instanceof StageSidecarError) throw error;
    return fail("STAGE_SIDECAR_FFMPEG_FAILED", `ffmpeg sidecar (${target.triple}): ${error.message}`, { cause: error, causeCode: error.code });
  }
}

/**
 * Stage one architecture of onnxruntime: the approved or pinned catalog row for `t`, its archive (digest-checked), the library
 * (digest-checked) into `destDir` (default the entry's resourceDir), and the notices. Returns { row, dest, outName, notices }.
 */
async function stageOnnxRuntimeSlice(entry, ctx, loaded, t, { destDir } = {}) {
  const { appRoot, fs } = ctx;
  const rows = (loaded.catalog?.libraries ?? []).filter((row) => row.library === "onnxruntime" && row.version === entry.version && row.os === t.os && row.arch === t.arch);
  const where = `onnxruntime ${entry.version} ${t.os}/${t.arch}`;
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
  const dest = path.resolve(appRoot, destDir ?? entry.resourceDir, outName);
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
  return { row, dest, outName, notices: noticeOut };
}

async function stageOnnxRuntime(entry, ctx) {
  const { appRoot, fs, target } = ctx;
  const loaded = await (ctx.loadNativeCatalog({ entry, appRoot }) ?? (entry.catalogSpec ? readJsonCatalog(resolveCatalogFile(entry.catalogSpec, ctx, "onnxruntime"), appRoot, fs, "native-libraries") : undefined));
  if (!loaded) fail("STAGE_SIDECAR_CATALOG_UNREADABLE", "onnxruntime needs a catalog path (the rightkit-ffmpeg native-libraries.json) or deps.loadNativeCatalog");
  if (target.arch !== "universal") {
    const { row, dest, outName, notices } = await stageOnnxRuntimeSlice(entry, ctx, loaded, target);
    return [{ kind: "onnxruntime", name: outName, path: rel(ctx, dest), sha256: row.sha256, sizeBytes: fs.statSync(dest).size, changed: null, catalogRow: row.id, version: row.version, archiveSha256: row.archive.sha256, license: row.licenseSpdx ?? null, notices, externalBin: false }];
  }
  // universal-apple-darwin: each arch row is staged thin under SLICE_ROOT, then merged into the entry's resource file.
  const slices = [];
  for (const triple of SLICES) slices.push({ triple, ...(await stageOnnxRuntimeSlice(entry, ctx, loaded, parseTargetTriple(triple), { destDir: `${SLICE_ROOT}/${triple}` })) });
  const names = [...new Set(slices.map((slice) => slice.outName))];
  if (names.length !== 1) fail("STAGE_SIDECAR_NOT_APPROVED", `the arch rows name different library files (${names.join(", ")}); set "as" so one universal file name is staged`);
  const dest = path.resolve(appRoot, entry.resourceDir, names[0]);
  const merged = lipoMergeInto(ctx, slices.map((slice) => slice.dest), dest, `onnxruntime ${entry.version}`);
  const notices = [...new Map(slices.flatMap((slice) => slice.notices).map((notice) => [notice.path, notice])).values()];
  return [{
    kind: "onnxruntime", name: names[0], path: rel(ctx, dest), ...merged, sourceSha256: merged.sha256, catalogRow: slices.map((slice) => slice.row.id).join("+"), version: entry.version,
    license: slices[0].row.licenseSpdx ?? null, notices, externalBin: false,
    slices: slices.map((slice) => ({ triple: slice.triple, sha256: slice.row.sha256, catalogRow: slice.row.id, archiveSha256: slice.row.archive.sha256 })),
  }];
}

/** Every file under `dir`, sorted, as paths relative to it (forward slashes). */
function listFilesRecursive(fs, dir, prefix = "") {
  const out = [];
  for (const name of [...fs.readdirSync(path.join(dir, prefix))].sort()) {
    const relative = prefix ? `${prefix}/${name}` : name;
    const stat = fs.statSync(path.join(dir, relative));
    if (stat.isDirectory()) out.push(...listFilesRecursive(fs, dir, relative));
    else if (stat.isFile()) out.push(relative);
  }
  return out;
}

/** A directory resource: copy recursively, hash and verify every file, one receipt row per file. */
function stageResourceDirectory(entry, ctx, source) {
  const { appRoot, fs } = ctx;
  if (entry.sha256) fail("STAGE_SIDECAR_CONFIG_INVALID", `resource ${entry.from} is a directory; sha256 pins one file, so drop it (every file is hashed into the receipt)`, { from: entry.from });
  if (entry.machO) fail("STAGE_SIDECAR_CONFIG_INVALID", `resource ${entry.from} is a directory; machO applies to one file`, { from: entry.from });
  const members = listFilesRecursive(fs, source);
  if (!members.length) fail("STAGE_SIDECAR_RESOURCE_MISSING", `resource directory ${entry.from} contains no files`, { from: entry.from });
  const destRoot = path.resolve(appRoot, entry.to);
  cleanDest(ctx, entry, destRoot, { directory: true });
  return members.map((member) => {
    const from = path.join(source, ...member.split("/"));
    const dest = path.join(destRoot, ...member.split("/"));
    const sourceSha256 = sha256Of(fs.readFileSync(from));
    const copied = copyInto(ctx, from, dest, { executable: false });
    return { kind: "resource", name: path.posix.basename(member), path: rel(ctx, dest), ...copied, sourceSha256, from: `${entry.from}/${member}`, externalBin: false };
  });
}

function stageResource(entry, ctx) {
  const { appRoot, fs } = ctx;
  const source = path.resolve(appRoot, entry.from);
  if (fs.existsSync(source) && fs.statSync(source).isDirectory()) return stageResourceDirectory(entry, ctx, source);
  if (!fs.existsSync(source) || !fs.statSync(source).isFile()) fail("STAGE_SIDECAR_RESOURCE_MISSING", `resource ${entry.from} is neither a file nor a directory`, { from: entry.from });
  const actual = sha256Of(fs.readFileSync(source));
  if (entry.sha256 && entry.sha256 !== actual) fail("STAGE_SIDECAR_HASH_MISMATCH", `${entry.from} is ${actual}, expected ${entry.sha256}`, { expected: entry.sha256, actual });
  if (entry.machO && ctx.target.arch === "universal") requireUniversalFile(ctx, source, `resource ${entry.from}`);
  const dest = path.resolve(appRoot, entry.to);
  cleanDest(ctx, entry, dest);
  const copied = copyInto(ctx, source, dest, { executable: false });
  return [{ kind: "resource", name: path.basename(dest), path: rel(ctx, dest), ...copied, sourceSha256: actual, from: entry.from, externalBin: false }];
}

/** Run the entry's command (argv, no shell), then stage its output as <name>-<triple>. */
function stageCommand(entry, ctx) {
  const { appRoot, fs, run, env } = ctx;
  const cwd = entry.cwd ? path.resolve(appRoot, entry.cwd) : appRoot;
  cleanDest(ctx, entry, destFor(entry, ctx));
  ctx.log(`stage-sidecar: ${entry.cmd} ${entry.args.join(" ")}`);
  const built = run(entry.cmd, entry.args, { cwd, env });
  if (built?.error || built?.status !== 0) fail("STAGE_SIDECAR_BUILD_FAILED", `command ${entry.name} (${entry.cmd}) failed (${built?.error?.message ?? `exit ${built?.status}`})`, { name: entry.name, stderr: tail(built?.stderr) });
  const source = path.resolve(appRoot, entry.output);
  if (!fs.existsSync(source) || !fs.statSync(source).isFile()) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `command ${entry.name} did not produce ${entry.output}`, { name: entry.name, output: entry.output });
  if (ctx.target.arch === "universal") requireUniversalFile(ctx, source, `command ${entry.name} output ${entry.output}`);
  const sourceSha256 = sha256Of(fs.readFileSync(source));
  const dest = destFor(entry, ctx);
  const copied = copyInto(ctx, source, dest, { executable: true });
  return [{ kind: "command", name: entry.name, path: rel(ctx, dest), ...copied, sourceSha256, output: entry.output, externalBin: !entry.dest }];
}

/**
 * Download `{ url, sha256, size? }` into the content-addressed cache (`.right-release/cache/remote/<sha256>`). The bytes are
 * hashed before anything is written: the cache entry exists only from verified bytes. A mismatch (or a non-https redirect)
 * fails closed with nothing staged. Returns { cached, fromCache }.
 */
async function ensureCachedDownload(ctx, { url, sha256, size }) {
  const { appRoot, fs } = ctx;
  const cached = path.resolve(appRoot, REMOTE_CACHE_DIR, sha256);
  if (fs.existsSync(cached) && sha256Of(fs.readFileSync(cached)) === sha256) return { cached, fromCache: true };
  ctx.log(`stage-sidecar: GET ${url}`);
  let response;
  try { response = await ctx.fetch(url, { redirect: "follow" }); } catch (error) { fail("STAGE_SIDECAR_DOWNLOAD_FAILED", `GET ${url} failed: ${error.message}`, { cause: error, url }); }
  if (!response?.ok) fail("STAGE_SIDECAR_DOWNLOAD_FAILED", `GET ${url} failed with ${response?.status}`, { url });
  if (typeof response.url === "string" && response.url && !response.url.startsWith("https://")) fail("STAGE_SIDECAR_DOWNLOAD_FAILED", `${url} redirected to a non-https URL: ${response.url}`, { url });
  const bytes = Buffer.from(await response.arrayBuffer());
  if (size !== undefined && bytes.length !== size) fail("STAGE_SIDECAR_HASH_MISMATCH", `${url} is ${bytes.length} bytes, expected ${size}; nothing staged`, { expectedSize: size, actualSize: bytes.length });
  const actual = sha256Of(bytes);
  if (actual !== sha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${url} downloaded as ${actual}, expected ${sha256}; nothing staged`, { expected: sha256, actual });
  fs.mkdirSync(path.dirname(cached), { recursive: true });
  const temporary = `${cached}.tmp-${ctx.pid}`;
  fs.writeFileSync(temporary, bytes);
  fs.renameSync(temporary, cached);
  return { cached, fromCache: false };
}

/**
 * A pinned https download. `dest` is copied only from the verified cache entry. With `extract` the download is an archive
 * (zip, tgz): the one member whose base name is `extract` is unpacked (digest-checked against `memberSha256` when given) and
 * only that member is staged, so no `command` entry or shell is needed to unpack it.
 */
async function stageRemote(entry, ctx) {
  const { appRoot, fs } = ctx;
  const dest = path.resolve(appRoot, entry.dest);
  const { cached, fromCache } = await ensureCachedDownload(ctx, entry);
  let source = cached;
  let scratch;
  try {
    let memberSha256;
    if (entry.extract) {
      scratch = ctx.makeTempDir();
      source = await ctx.extractMember({ archive: cached, fileName: entry.extract, destDir: scratch });
      memberSha256 = sha256Of(fs.readFileSync(source));
      if (entry.memberSha256 && memberSha256 !== entry.memberSha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${entry.extract} from ${entry.url} is ${memberSha256}, expected ${entry.memberSha256}; nothing staged`, { expected: entry.memberSha256, actual: memberSha256 });
    }
    // Download and verify first: a failed fetch or a hash mismatch must never leave a cleaned destination behind.
    cleanDest(ctx, entry, dest);
    const copied = copyInto(ctx, source, dest, { executable: entry.executable === true });
    const expected = entry.extract ? entry.memberSha256 : entry.sha256;
    if (expected && copied.sha256 !== expected) fail("STAGE_SIDECAR_HASH_MISMATCH", `staged ${entry.dest} is ${copied.sha256}, expected ${expected}`, { expected, actual: copied.sha256 });
    return [{ kind: "remote", name: entry.name, path: rel(ctx, dest), ...copied, sourceSha256: memberSha256 ?? entry.sha256, url: entry.url, cached: fromCache, externalBin: false, ...(entry.extract ? { archiveSha256: entry.sha256, member: entry.extract } : {}) }];
  } finally {
    if (scratch) ctx.removeDir(scratch);
  }
}

/**
 * Bring the fixed source checkout `srcDir` to the pinned commit: `git init` once, then fetch exactly that commit (git verifies the
 * object hash, so the tree is pinned with no repackable tarball) and detach onto it. A checkout already at the commit is reused;
 * one with local edits is refused rather than built.
 */
function checkoutPinnedSource(ctx, entry, srcDir) {
  const { fs, run, env } = ctx;
  const git = (args) => {
    const result = run("git", args, { cwd: srcDir, env });
    if (result?.error || result?.status !== 0) fail("STAGE_SIDECAR_BUILD_FAILED", `git ${args[0]} for ${entry.name} failed (${result?.error?.message ?? `exit ${result?.status}`})`, { name: entry.name, stderr: tail(result?.stderr) });
    return result;
  };
  const head = () => { const result = run("git", ["rev-parse", "HEAD"], { cwd: srcDir, env }); return result?.status === 0 ? String(result.stdout ?? "").trim() : ""; };
  if (!fs.existsSync(path.join(srcDir, ".git"))) {
    fs.mkdirSync(srcDir, { recursive: true });
    git(["init", "-q"]);
    git(["remote", "add", "origin", entry.repo]);
  } else git(["remote", "set-url", "origin", entry.repo]);
  // Cleanliness is judged BEFORE any destructive source operation: tracked edits, untracked files and ignored files are all
  // build inputs (CMake globbing), so any of them refuses the build and nothing is checked out over them.
  const assertClean = (when) => {
    const dirty = run("git", ["status", "--porcelain", "--ignored"], { cwd: srcDir, env });
    if (dirty?.status !== 0 || String(dirty.stdout ?? "").trim()) fail("STAGE_SIDECAR_SOURCE_MISMATCH", `${entry.name} source checkout ${srcDir} has local, untracked or ignored files ${when}; refusing to build a tree that is not the pinned commit`, { name: entry.name, status: tail(dirty?.stdout) });
  };
  assertClean("before checkout");
  if (head() !== entry.commit) {
    ctx.log(`stage-sidecar: git fetch ${entry.repo} ${entry.commit}`);
    git(["fetch", "--depth", "1", "origin", entry.commit]);
    git(["checkout", "-q", "--force", "--detach", "FETCH_HEAD"]);
    const actual = head();
    if (actual !== entry.commit) fail("STAGE_SIDECAR_SOURCE_MISMATCH", `${entry.name} checkout is ${actual || "unreadable"}, the pin is ${entry.commit}`, { expected: entry.commit, actual });
  }
  assertClean("after checkout");
  if (fs.existsSync(path.join(srcDir, ".gitmodules"))) {
    // Submodule commits are pinned by the superproject tree: bring them to exactly those commits, then require every one to match
    // (no `+` moved, `-` missing or `U` conflicted prefix) and be clean, so no unpinned submodule source reaches the build.
    git(["submodule", "update", "--init", "--recursive", "--checkout"]);
    const modules = git(["submodule", "status", "--recursive"]);
    const bad = String(modules.stdout ?? "").split("\n").filter((line) => /^[-+U]/.test(line));
    if (bad.length) fail("STAGE_SIDECAR_SOURCE_MISMATCH", `${entry.name} submodules are not at their pinned commits: ${bad.join("; ")}`, { name: entry.name });
    const inner = git(["submodule", "foreach", "--recursive", "git status --porcelain --ignored"]);
    if (String(inner.stdout ?? "").split("\n").some((line) => line.trim() && !/^Entering /.test(line))) fail("STAGE_SIDECAR_SOURCE_MISMATCH", `${entry.name} submodule checkout has local, untracked or ignored files; refusing to build`, { name: entry.name });
    assertClean("after submodule update");
  }
}

/**
 * Build a pinned git commit with the build tool (no shell) and stage the result as <name>-<triple>[.exe]: fetch the commit, configure,
 * build `target`, locate the executable, check it carries both archs for universal-apple-darwin, optionally pin its digest and
 * smoke-run it. Per-platform flags (for example Metal on macOS) come from `platformArgs`; the source and build trees live at the
 * fixed `.right-release/cmake/<name>/{src,build}`.
 */
function stageCmake(entry, ctx) {
  const { appRoot, fs, run, env, target } = ctx;
  const root = path.resolve(appRoot, CMAKE_ROOT, entry.name);
  const srcDir = path.join(root, "src");
  const buildDir = path.join(root, "build");
  const dest = destFor(entry, ctx);
  cleanDest(ctx, entry, dest);
  checkoutPinnedSource(ctx, entry, srcDir);
  const archFlag = target.platform !== "mac" ? undefined
    : target.arch === "universal" ? "-DCMAKE_OSX_ARCHITECTURES=arm64;x86_64"
      : `-DCMAKE_OSX_ARCHITECTURES=${target.arch === "aarch64" ? "arm64" : "x86_64"}`;
  const configure = ["-S", srcDir, "-B", buildDir, "-DCMAKE_BUILD_TYPE=Release", ...(entry.generator ? ["-G", entry.generator] : []), ...(archFlag ? [archFlag] : []), ...(entry.platformArgs[target.platform] ?? []), ...entry.cmakeArgs];
  const step = (args, label) => {
    ctx.log(`stage-sidecar: cmake ${args.join(" ")}`);
    const result = run("cmake", args, { cwd: appRoot, env });
    if (result?.error || result?.status !== 0) fail("STAGE_SIDECAR_BUILD_FAILED", `cmake ${label} of ${entry.name} failed (${result?.error?.message ?? `exit ${result?.status}`})`, { name: entry.name, stderr: tail(result?.stderr) });
  };
  step(configure, "configure");
  step(["--build", buildDir, "--config", "Release", "--target", entry.target, "--parallel"], "build");
  const exe = ctx.windows ? ".exe" : "";
  const output = entry.output ?? `bin/${entry.target}`;
  const dir = path.posix.dirname(output);
  const file = `${path.posix.basename(output)}${exe}`;
  const candidates = [...new Set([path.join(buildDir, dir, file), path.join(buildDir, dir, "Release", file), path.join(buildDir, "Release", file)])];
  const source = candidates.find((candidate) => fs.existsSync(candidate) && fs.statSync(candidate).isFile());
  if (!source) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `the build of ${entry.name} did not produce ${output} (looked in ${candidates.join(", ")})`, { name: entry.name, candidates });
  if (target.arch === "universal") requireUniversalFile(ctx, source, `cmake build ${entry.name}`);
  const sourceSha256 = sha256Of(fs.readFileSync(source));
  if (entry.sha256 && entry.sha256 !== sourceSha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${entry.name} built to ${sourceSha256}, expected ${entry.sha256}`, { expected: entry.sha256, actual: sourceSha256 });
  const copied = copyInto(ctx, source, dest, { executable: true });
  if (entry.smoke && canRunTarget(ctx, target)) {
    const smoked = run(dest, entry.smoke, { cwd: appRoot, env });
    if (smoked?.error || smoked?.status !== 0) fail("STAGE_SIDECAR_BUILD_FAILED", `${entry.name} does not run (${entry.smoke.join(" ")}): ${smoked?.error?.message ?? `exit ${smoked?.status}`}`, { name: entry.name });
  }
  return [{ kind: "cmake", name: entry.name, path: rel(ctx, dest), ...copied, sourceSha256, repo: entry.repo, commit: entry.commit, cmakeTarget: entry.target, flags: configure.filter((arg) => arg.startsWith("-D")), externalBin: !entry.dest }];
}

/** `{platform}` in a prebuilt manifest entry: macos-aarch64, macos-x86_64, windows-x86_64, linux-x86_64, ... */
const prebuiltPlatformKey = (t) => `${t.os}-${t.arch}`;

/**
 * Verify a host-built binary (already staged at <stageDir>/<name>-<triple>, or copied from `from`) against the digest in an app
 * manifest file: its raw sha256, and when the manifest names an approval hash, the signing-independent approval digest
 * (`macho-codesign-strip-linkedit-v1` on macOS, `raw-sha256` elsewhere). Nothing is built or downloaded: a missing file or
 * a mismatch fails closed.
 */
async function stagePrebuilt(entry, ctx) {
  const { appRoot, fs, target } = ctx;
  const dest = destFor(entry, ctx);
  const platformKey = prebuiltPlatformKey(target);
  const manifestFile = path.resolve(appRoot, entry.manifest.file);
  let manifest;
  try { manifest = JSON.parse(String(fs.readFileSync(manifestFile))); } catch (error) { fail("STAGE_SIDECAR_CATALOG_UNREADABLE", `cannot read the prebuilt manifest ${manifestFile} as JSON`, { cause: error }); }
  let node = manifest;
  for (const part of entry.manifest.entry.replaceAll("{platform}", platformKey).split(".")) node = isObject(node) ? node[part] : undefined;
  if (!isObject(node)) fail("STAGE_SIDECAR_NOT_APPROVED", `${entry.manifest.file} has no ${entry.manifest.entry.replaceAll("{platform}", platformKey)} entry for ${entry.name} on ${platformKey}`);
  const pinned = node[entry.manifest.sha256];
  if (typeof pinned !== "string" || !SHA256.test(pinned)) fail("STAGE_SIDECAR_NOT_APPROVED", `${entry.manifest.file} ${platformKey}.${entry.manifest.sha256} is not a lowercase 64-hex SHA-256`);
  const source = entry.from ? path.resolve(appRoot, entry.from) : dest;
  if (!fs.existsSync(source) || !fs.statSync(source).isFile()) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `prebuilt ${entry.name} for ${platformKey} is missing: ${source}. Build it on its native host first; packaging never substitutes a stock binary.`, { name: entry.name, source });
  const actual = sha256Of(fs.readFileSync(source));
  if (actual !== pinned) fail("STAGE_SIDECAR_HASH_MISMATCH", `${entry.name} integrity failure: ${source} is ${actual}, ${entry.manifest.file} pins ${pinned}`, { expected: pinned, actual });
  let approval;
  if (entry.manifest.approvalSha256) {
    const expected = node[entry.manifest.approvalSha256];
    if (typeof expected !== "string" || !SHA256.test(expected)) fail("STAGE_SIDECAR_NOT_APPROVED", `${entry.manifest.file} ${platformKey}.${entry.manifest.approvalSha256} is not a lowercase 64-hex SHA-256`);
    const algorithm = entry.manifest.approvalAlgorithm ? node[entry.manifest.approvalAlgorithm] : (target.os === "macos" ? APPROVAL_MACHO_STRIP_LINKEDIT_V1 : APPROVAL_RAW_SHA256);
    if (typeof algorithm !== "string") fail("STAGE_SIDECAR_NOT_APPROVED", `${entry.manifest.file} ${platformKey}.${entry.manifest.approvalAlgorithm} does not name an approval hash algorithm`);
    let digest;
    try { digest = await ctx.approvalDigest(source, algorithm); } catch (error) { fail("STAGE_SIDECAR_APPROVAL_FAILED", `${entry.name} approval hash (${algorithm}): ${error.message}`, { cause: error, causeCode: error.code }); }
    if (digest !== expected) fail("STAGE_SIDECAR_HASH_MISMATCH", `${entry.name} signing-independent integrity failure: ${algorithm} is ${digest}, ${entry.manifest.file} pins ${expected}`, { expected, actual: digest });
    approval = { algorithm, sha256: digest };
  }
  let copied;
  if (path.resolve(source) === path.resolve(dest)) {
    if (!ctx.windows) fs.chmodSync(dest, 0o755);
    copied = { sha256: actual, sizeBytes: fs.statSync(dest).size, changed: false };
  } else copied = copyInto(ctx, source, dest, { executable: true });
  return [{ kind: "prebuilt", name: entry.name, path: rel(ctx, dest), ...copied, sourceSha256: actual, manifest: entry.manifest.file, platform: platformKey, ...(approval ? { approvalAlgorithm: approval.algorithm, approvalSha256: approval.sha256 } : {}), externalBin: !entry.dest }];
}

/**
 * `wipeStageDir`: remove top-level files in stageDir named exactly `<name>-<current triple>[.exe]` for a name this manifest
 * declares (a cargo-bin, command or ffmpeg tool) that this run did not produce, such as an entry now skipped on this platform
 * or now placed at `dest`. Never recursive; other triples, notices, directories, temporaries and unrelated files are left alone.
 */
function wipeStaleStageDir(ctx, plan, produced) {
  const dir = path.resolve(ctx.appRoot, ctx.stageDir);
  let listing;
  try { listing = [...ctx.fs.readdirSync(dir)]; } catch { return []; }
  const keep = new Set([...produced, path.resolve(ctx.appRoot, plan.receiptPath)].map((file) => path.resolve(file).toLowerCase()));
  const names = plan.entries.flatMap((entry) => (entry.kind === "ffmpeg" ? (entry.media?.tools ?? []) : entry.kind === "cargo-bin" || entry.kind === "command" || entry.kind === "cmake" || entry.kind === "prebuilt" ? [entry.name] : []));
  const exact = new Set(names.flatMap((name) => [`${name}-${ctx.target.triple}`, `${name}-${ctx.target.triple}.exe`]));
  const removed = [];
  for (const name of listing.sort()) {
    const file = path.join(dir, name);
    if (keep.has(file.toLowerCase()) || !exact.has(name)) continue;
    let regular = false;
    try { regular = ctx.fs.statSync(file).isFile(); } catch { regular = false; }
    if (!regular) continue;
    ctx.log(`stage-sidecar: wipe stale ${rel(ctx, file)}`);
    ctx.fs.rmSync(file, { recursive: false, force: true });
    removed.push(rel(ctx, file));
  }
  return removed;
}

/**
 * Nested broker context: Tauri exports TAURI_ENV_* to beforeBuildCommand/beforeDevCommand, and on a managed host that
 * command runs inside the outer broker build. A broker call from there exits 125. There is no lease or run-id a nested
 * call can reuse (acquireManagedRunLease records a per-process lease file; no RK_* variable carries it to a child), so
 * the only safe behaviour is to refuse early. RIGHTKIT_STAGE_SIDECAR_ALLOW_NESTED=1 opts out.
 */
export function isNestedBrokerContext(env = process.env) {
  if (env?.RIGHTKIT_STAGE_SIDECAR_ALLOW_NESTED === "1") return false;
  const insideTauri = Boolean(env?.TAURI_ENV_PLATFORM || env?.TAURI_ENV_TARGET_TRIPLE);
  return insideTauri && (managedHost(env ?? {}) || env?.RIGHTSUITE_HEAVY_WORK_OWNER === "1");
}

/** True when staging would call `rightkit cargo` for a non-skipped entry. */
const needsBroker = (plan) => plan.entries.some((entry) => !entry.skipped && (entry.kind === "cargo-bin" || entry.catalogSpec?.via === "crate"));

function assertNotNested(plan, env) {
  if (needsBroker(plan) && isNestedBrokerContext(env)) {
    fail(NESTED_BROKER_CODE, "stage-sidecar needs `rightkit cargo`, but it is running inside an outer broker build (a Tauri beforeBuildCommand/beforeDevCommand on a managed host), where a nested broker call exits 125. Run stage-sidecar in the recipe `prepare` step, before the Tauri build (or call it from your own script before `tauri build`), not from beforeBuildCommand.");
  }
}

/**
 * Dry plan for `stage-sidecar --check`: validates the manifest and describes what staging would write. No effects.
 */
export function planSidecarStaging({ manifest, target, env = process.env } = {}) {
  const plan = validateSidecarManifest(manifest, { target, mode: resolveBuildMode(env) });
  const t = plan.target;
  const outputsOf = (entry) => {
    if (["cargo-bin", "command", "cmake", "prebuilt"].includes(entry.kind)) return [entry.dest ?? `${plan.stageDir}/${stagedName(entry.name, t)}`];
    if (entry.kind === "remote") return [entry.dest];
    if (entry.kind === "resource") return [entry.to];
    if (entry.kind === "onnxruntime") return [`${entry.resourceDir}/${entry.as ?? "<catalog artifactFileName>"}`];
    return (entry.media?.tools ?? []).map((tool) => `${plan.stageDir}/${stagedName(tool, t)}`);
  };
  const entries = plan.entries.map((entry) => ({
    index: entry.index, kind: entry.kind, skipped: entry.skipped, ...(entry.platforms ? { platforms: entry.platforms } : {}),
    ...(entry.skipped ? {} : { outputs: outputsOf(entry), externalBin: ["cargo-bin", "command", "cmake", "prebuilt"].includes(entry.kind) ? !entry.dest : entry.kind === "ffmpeg" }),
    ...(entry.kind === "cargo-bin" ? { package: entry.package, bin: entry.bin, profile: entry.profile } : {}),
    ...(entry.kind === "remote" ? { url: entry.url } : {}),
    ...(entry.kind === "cmake" ? { repo: entry.repo, commit: entry.commit } : {}),
    ...(entry.clean ? { clean: true } : {}),
  }));
  return { target: t.triple, platform: t.platform, mode: resolveBuildMode(env), stageDir: plan.stageDir, receiptPath: plan.receiptPath, wipeStageDir: plan.wipeStageDir, nestedBrokerRisk: needsBroker(plan) && isNestedBrokerContext(env), entries };
}

/**
 * Stage every configured sidecar for `target` (a Rust triple) and write the staging receipt.
 * Returns { receipt, receiptPath, files, externalBinNames }. `deps` (all optional) injects every effect:
 * fs (existsSync, mkdirSync, readFileSync, writeFileSync, copyFileSync, renameSync, chmodSync, statSync, readdirSync, rmSync), run, fetch (https downloads of `remote`), fetchBytes, extractMember, loadMediaCatalog, loadNativeCatalog, makeTempDir, removeDir, now, pid, log, env.
 */
export async function stageSidecars({ appRoot, manifest, target, deps = {} } = {}) {
  if (typeof appRoot !== "string" || !path.isAbsolute(appRoot)) fail("STAGE_SIDECAR_CONFIG_INVALID", "appRoot must be an absolute path");
  const env = deps.env ?? process.env;
  const plan = validateSidecarManifest(manifest, { target, mode: resolveBuildMode(env) });
  assertNotNested(plan, env);
  const fs = deps.fs ?? nodeFs;
  const run = deps.run ?? defaultRun;
  const ctx = {
    appRoot, target: plan.target, stageDir: plan.stageDir, fs, env, run, protectedPaths: protectedOutputPaths(plan),
    windows: plan.target.os === "windows",
    pid: deps.pid ?? process.pid,
    host: deps.host ?? { os: { darwin: "macos", win32: "windows", linux: "linux" }[process.platform] ?? process.platform, arch: { arm64: "aarch64", x64: "x86_64" }[process.arch] ?? process.arch },
    approvalDigest: deps.approvalDigest ?? ((file, algorithm) => approvalDigest(file, algorithm)),
    log: deps.log ?? (() => {}),
    fetchBytes: deps.fetchBytes ?? defaultFetchBytes,
    fetch: deps.fetch ?? ((...args) => globalThis.fetch(...args)),
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
            : entry.kind === "remote" ? await stageRemote(entry, ctx)
              : entry.kind === "cmake" ? stageCmake(entry, ctx)
                : entry.kind === "prebuilt" ? await stagePrebuilt(entry, ctx)
                  : stageResource(entry, ctx);
    files.push(...staged);
  }
  const wiped = plan.wipeStageDir ? wipeStaleStageDir(ctx, plan, files.map((file) => path.resolve(appRoot, file.path))) : [];
  const receipt = {
    schema: 1,
    contract: STAGE_SIDECAR_CONTRACT,
    target: plan.target.triple,
    os: plan.target.os,
    arch: plan.target.arch,
    stagedAt: (deps.now ?? (() => new Date().toISOString()))(),
    files,
    skipped,
    ...(plan.wipeStageDir ? { wiped } : {}),
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
    if (!Number.isSafeInteger(file.sizeBytes) || file.sizeBytes < (file.kind === "resource" ? 0 : 1)) bad(`${file.name}.sizeBytes must be positive`);
  }
  if (new Set(receipt.files.map((file) => file.path)).size !== receipt.files.length) bad("duplicate staged path");
  if (expected.target && receipt.target !== expected.target) bad(`target ${receipt.target} is not ${expected.target}`);
  if (expected.names && JSON.stringify([...expected.names].sort()) !== JSON.stringify(receipt.files.filter((file) => file.externalBin).map((file) => file.name).sort())) bad("externalBin names differ from the configuration");
  return receipt;
}
