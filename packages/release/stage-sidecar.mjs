// Declarative engine and sidecar staging for Tauri externalBin (bogusyogi/rightkit#345).
//
// Eight apps hand-rolled the same script: build an engine through `rustright cargo`, find the executable, copy it to
// src-tauri/binaries/<name>-<target-triple>[.exe], stage ffmpeg, stage onnxruntime, copy model resources. This module is
// that script, driven by a `sidecars` list in right-release.config.mjs:
//
//   sidecars: [
//     { kind: "cargo-bin", package: "mailright-engine", bin?, name?, profile?: "release" | { dev, release }, features?, manifestPath?, sha256?, dest?, clean?, platforms? },
//     { kind: "ffmpeg", variant?, variants?, tools?, notices: { dir, sourceOfferContact }, catalog, stage?, bundleDir?, resourceDir?, sources?, probe? },
//     { kind: "onnxruntime", version, catalog, resourceDir?: "src-tauri/resources/runtime", as?, licenseDir?, cacheDir?, companions? },   // the alias of native-library { library: "onnxruntime" }
//     { kind: "native-library", library: "onnxruntime" | "directml" | "pdfium" | "whisper-cpp" | "ggml" | "sherpa-onnx", version, catalog, resourceDir?, licenseDir?, cacheDir?, as?, files?, companions?, source?: { dir }, platforms? },
//     { kind: "resource", from, to, sha256?, machO?, clean?, platforms? },   // `from` may be a directory (copied recursively, every file hashed)
//     { kind: "command", name, cmd, args?, cwd?, output, externalBin: true | (false with dest), dest?, clean?, platforms? },
//     { kind: "remote", name, url (https), sha256, dest | to, size?, extract?, memberSha256?, executable?, clean?, platforms? },
//     { kind: "cmake", name, repo (https), commit (40 hex), target?, output?, cmakeArgs?, platformArgs?, generator?, platformGenerators?, architecture?, cmakeBin?, smoke?, sha256?, dest?, clean?, platforms? },
//     { kind: "prebuilt", name, from? (a path, or { aarch64, x86_64 } for universal-apple-darwin), manifest: { file, entry, sha256?, approvalSha256?, approvalAlgorithm? }, dest?, platforms? },
//   ]
//
// `dest` (cargo-bin, command, remote) is a contained app-relative path (never the app root, the stage dir, the receipt, the cache, .git, src, src-tauri, node_modules or package.json; outputs may not overlap) used instead of <stageDir>/<name>-<triple>; the file is
// then not an externalBin. `clean: true` removes a stale file at the destination before staging. The wrapper key
// `wipeStageDir: true` removes stale files named exactly `<name>-<current triple>[.exe]` for a declared name this run no longer produces.
//
// `catalog` is a relative path, an absolute path, { crate: "rightkit-ffmpeg", file? } (resolved through `rustright cargo
// metadata`) or { npm: "<pkg>/<file>" }. The wrapper form { sidecars, profiles: { dev, release } } sets the default cargo
// profile pair; RIGHTKIT_BUILD_MODE (dev|release, default release) selects. The target `universal-apple-darwin` builds
// both slices of a cargo-bin and lipo-creates `<name>-universal-apple-darwin`. `ffmpeg`, `onnxruntime` and `native-library` do the same from their
// per-arch catalog rows (each slice staged under .right-release/slices/<triple>, then lipo-merged; a `universal` macOS row is staged once, lipo -archs checked, no merge), a `cmake` entry builds with
// CMAKE_OSX_ARCHITECTURES="arm64;x86_64". A `prebuilt` entry does it from per-arch host-built inputs (`from: { aarch64, x86_64 }`, or the
// staged `<name>-<slice triple>` files), each verified against its own manifest entry and then lipo-merged. An `ffmpeg` source may be a
// local path (`sources.ffmpeg: { path: "../vendor/ffmpeg-lgpl" }`) instead of a pinned https archive. An `ffmpeg` entry with `resourceDir`
// stages `<resourceDir>/<tool>[.exe]` (no target triple, not an externalBin) from the same rows, with the same lipo merge, notices, digest
// check, sources, probe and receipt; `bundleDir` belongs to the externalBin layout, so the two keys are refused together.
//
// It never signs (release.mjs signs staged files) and never calls bare cargo. Every effect goes through `deps`.
import { createHash } from "node:crypto";
import nodeFs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync as nodeSpawnSync } from "./background-process.mjs";
import { resolveCommand } from "./development.mjs";
import { carriedCargoMapOf } from "./carried-cargo.mjs";
import { BUILD_LAUNCH_REFUSED, buildLaunch } from "./build-launch.mjs";
import { directPlatform, managedHost, runCargoSync } from "./managed-cargo.mjs";
import { APPROVAL_MACHO_STRIP_LINKEDIT_V1, APPROVAL_RAW_SHA256, approvalDigest } from "./macho-approval-hash.mjs";
import {
  bundledFileName, planMediaSidecar, sourceOfferText, stagedFileName, validateMediaSidecarConfig, verifyStagedSidecars, writeMediaSidecarNotices,
} from "./media-sidecar.mjs";
import { ContentDigestError, contentIdentity, digestFor } from "./content-digest.mjs";

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
const KINDS = new Set(["cargo-bin", "ffmpeg", "onnxruntime", "native-library", "resource", "command", "remote", "cmake", "prebuilt"]);
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
/** One path segment of an archive member path: no glob metacharacters, no separators, no drive colon. */
const MEMBER_SEGMENT = /^[A-Za-z0-9_.+@~-]+$/;

/**
 * Normalize and guard a requested archive member. A bare file name (no `/`) selects the one member with that base name; a path
 * (contains `/`) selects the one member with exactly that archive path. A leading `./` is stripped. Absolute paths, drive
 * prefixes, backslashes, empty, `.` and `..` segments and glob metacharacters are refused. Returns { value, byPath } or
 * { error }. The member never influences where the extracted file is written.
 */
export function normalizeArchiveMember(member) {
  if (typeof member !== "string" || !member) return { error: "must be a non-empty string" };
  if (member.includes("\\")) return { error: "must not contain a backslash" };
  const value = member.startsWith("./") ? member.slice(2) : member;
  if (value.startsWith("/")) return { error: "must not be an absolute path" };
  if (/^[A-Za-z]:/.test(value)) return { error: "must not start with a drive prefix" };
  const segments = value.split("/");
  if (segments.some((segment) => segment === "")) return { error: "must not contain an empty segment" };
  if (segments.some((segment) => segment === "." || segment === "..")) return { error: "must not contain a '.' or '..' segment" };
  // A bare file name keeps today's matching (any characters but the refused ones above); a path is held to a strict segment alphabet so it can never act as a tar glob.
  if (segments.length > 1 && !segments.every((segment) => MEMBER_SEGMENT.test(segment))) return { error: "has a segment with characters outside A-Z a-z 0-9 _ . + @ ~ -" };
  return { value, byPath: segments.length > 1 };
}
/** Native-catalog libraries a `native-library` entry stages from rows (dynamic libraries); the others are served by another route. */
const NATIVE_LIBRARIES = Object.freeze(["onnxruntime", "directml", "pdfium", "whisper-cpp", "ggml", "sherpa-onnx"]);
const STATIC_CRATE_ROUTE = "is a static crate: depend on it through Cargo (its catalog row pins the crate checksum); there is no file to stage";
const NATIVE_OTHER_ROUTE = Object.freeze({ ffmpeg: "has its own kind: use { kind: \"ffmpeg\", ... }", "sqlite-vec": STATIC_CRATE_ROUTE, libgit2: STATIC_CRATE_ROUTE });
/** A catalog version token (1.24.4, chromium/8021): only compared with the rows, never used as a path. */
const NATIVE_VERSION = /^[A-Za-z0-9][A-Za-z0-9._+/-]{0,63}$/;
const KEYS = Object.freeze({
  "cargo-bin": ["kind", "package", "bin", "name", "profile", "features", "manifestPath", "sha256", "dest", "clean", "platforms"],
  ffmpeg: ["kind", "variant", "variants", "tools", "notices", "catalog", "stage", "bundleDir", "resourceDir", "sources", "probe", "platforms"],
  onnxruntime: ["kind", "version", "catalog", "resourceDir", "as", "licenseDir", "cacheDir", "companions", "platforms"],
  "native-library": ["kind", "library", "version", "catalog", "resourceDir", "licenseDir", "cacheDir", "as", "files", "companions", "source", "platforms"],
  resource: ["kind", "from", "to", "sha256", "machO", "clean", "platforms"],
  command: ["kind", "name", "cmd", "args", "cwd", "output", "externalBin", "dest", "clean", "platforms"],
  remote: ["kind", "name", "url", "sha256", "dest", "to", "size", "extract", "extractKind", "memberSha256", "executable", "clean", "platforms"],
  cmake: ["kind", "name", "repo", "commit", "target", "output", "cmakeArgs", "platformArgs", "generator", "platformGenerators", "architecture", "cmakeBin", "smoke", "sha256", "dest", "clean", "platforms"],
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

/** `--target`, then TAURI_ENV_TARGET_TRIPLE, then the host triple from `rustright rustc -vV`. */
export function resolveTargetTriple({ target, env = process.env, run = defaultRun, cwd = process.cwd() } = {}) {
  if (target) return target;
  if (env.TAURI_ENV_TARGET_TRIPLE) return env.TAURI_ENV_TARGET_TRIPLE;
  const result = run("rustright", ["rustc", "-vV"], { cwd, env });
  const host = result?.stdout?.match(/^host:\s*(.+)$/m)?.[1]?.trim();
  if (!host || result.status !== 0) fail("STAGE_SIDECAR_TARGET_UNKNOWN", "cannot determine the host triple (rustright rustc -vV failed); pass a target");
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
/** The claim in `claimed` that `key` equals, contains or sits inside (outputs may not be nested inside each other). */
const findClaimClash = (claimed, key) => claimed.find((other) => other.key === key || other.key.startsWith(`${key}/`) || key.startsWith(`${other.key}/`));
/** `output` equals `protectedPath` or is an ancestor of it (so writing or cleaning `output` would reach it). */
const reaches = (output, protectedPath) => output === protectedPath || protectedPath.startsWith(`${output}/`);

/**
 * Paths stage-sidecar never writes to or cleans, app-relative: the stage directory, the receipt, the remote cache and the
 * `.right-release` state, onnxruntime and native-library cache directories, and the app's source and dependency roots. A destination that equals
 * or contains one of these is refused (validation, and again just before any delete).
 */
function protectedOutputPaths({ stageDir, receiptPath, entries = [] }) {
  const cacheDirs = entries.filter((entry) => (entry.kind === "onnxruntime" || entry.kind === "native-library") && entry.cacheDir).map((entry) => entry.cacheDir);
  return [stageDir, receiptPath, ".right-release", REMOTE_CACHE_DIR, ".git", "src", "src-tauri", "node_modules", "package.json", ...cacheDirs].filter(Boolean).map(canonicalOutput);
}

/** `<name>-<triple>[.exe]`, with `<name>-universal-apple-darwin` for the universal target. */
const stagedName = (name, t) => (t.arch === "universal" ? `${name}-${UNIVERSAL}` : stagedFileName(name, t.os, t.arch));

/** An ffmpeg tool's app-relative output: `<resourceDir>/<tool>[.exe]` for a resourceDir entry (no triple, not an externalBin), else `<stageDir>/<tool>-<triple>[.exe]`. */
const ffmpegOutput = (entry, stageDir, tool, t) => (entry.resourceDir ? `${entry.resourceDir}/${bundledFileName(tool, t.os)}` : `${stageDir}/${stagedName(tool, t)}`);

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

const CATALOG_DEFAULT_FILE = Object.freeze({ ffmpeg: "catalog/artifacts.json", onnxruntime: "catalog/native-libraries.json", "native-library": "catalog/native-libraries.json" });

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

const CMAKE_GENERATOR = /^[A-Za-z0-9][A-Za-z0-9 .+-]{0,60}$/;
const CMAKE_ARCHITECTURE = /^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$/;
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
 * A local ffmpeg source: `{ path, member? }`. `path` is relative to the app root and MAY step out of it (a vendored build such as
 * ../vendor/ffmpeg-lgpl); it names the binary itself, or a directory holding `<member>` (or `bin/<member>`). Nothing is downloaded and no
 * archive digest exists: the file's digest must still equal the APPROVED catalog row, exactly like a fetched member.
 */
function parseLocalSource(value, label, errors) {
  for (const key of Object.keys(value)) if (!["path", "member"].includes(key)) errors.push(`${label}: unknown key ${JSON.stringify(key)} for a local { path, member? } source`);
  const raw = value.path;
  const normalized = typeof raw === "string" ? raw.replaceAll("\\", "/") : "";
  if (typeof raw !== "string" || !raw.trim() || path.isAbsolute(raw) || /^[A-Za-z]:/.test(raw) || /^[a-z][a-z0-9+.-]*:\/\//i.test(normalized)) { errors.push(`${label}.path must be a relative path (it may start with ../); a URL belongs in { url, sha256, member }`); return undefined; }
  if (!normalized.split("/").some((segment) => segment !== "" && segment !== "." && segment !== "..")) { errors.push(`${label}.path must name a file or directory, not the app root or a bare parent: ${raw}`); return undefined; }
  if (value.member !== undefined && (typeof value.member !== "string" || !NAME.test(value.member))) errors.push(`${label}.member must be the bare file name of the binary inside the directory`);
  return { path: normalized.replace(/\/+$/, ""), ...(typeof value.member === "string" ? { member: value.member } : {}) };
}

/**
 * A `native-library` `source.dir`: the directory holding the shipped file under its catalog artifactFileName (rows with no archive,
 * measured from a retained build). Relative to the app root and MAY step out of it, or absolute. Nothing is downloaded from it.
 */
function parseSourceDir(raw, label, errors) {
  const normalized = typeof raw === "string" ? raw.replaceAll("\\", "/") : "";
  if (typeof raw !== "string" || !raw.trim()) { errors.push(`${label} must be a non-empty directory path (relative to the app root, or absolute)`); return undefined; }
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(normalized)) { errors.push(`${label} must be a directory path, not a URL (a source dir is never downloaded)`); return undefined; }
  if (path.isAbsolute(raw) || /^[A-Za-z]:[\\/]/.test(raw)) return raw;
  if (/^[A-Za-z]:/.test(raw) || !normalized.split("/").some((segment) => segment !== "" && segment !== "." && segment !== "..")) { errors.push(`${label} must name a directory (relative to the app root, or absolute), not the app root, a bare parent or a drive-relative path: ${raw}`); return undefined; }
  return normalized.replace(/\/+$/, "");
}

const parseFfmpegSource = (value, label, errors) => (isObject(value) && "path" in value ? parseLocalSource(value, label, errors) : parseArchiveSource(value, label, errors));

/**
 * `ffmpeg.sources`: where the bytes of a catalog row come from (the catalog pins digests, not locations). A tool maps to one
 * source (a pinned https archive, or a local path), or to `{ aarch64: source, x86_64: source }`; a universal target needs the per-arch form for every tool.
 */
function parseFfmpegSources(raw, where, errors, { universal }) {
  if (raw === undefined) return undefined;
  if (!isObject(raw) || !Object.keys(raw).length) { errors.push(`${where}: sources must be an object mapping ffmpeg or ffprobe to an archive source`); return undefined; }
  const out = {};
  for (const [tool, value] of Object.entries(raw)) {
    const label = `${where}: sources.${tool}`;
    if (!["ffmpeg", "ffprobe"].includes(tool)) { errors.push(`${label}: not ffmpeg or ffprobe`); continue; }
    const perArch = isObject(value) && SOURCE_ARCHES.some((arch) => arch in value) && !("url" in value) && !("path" in value);
    if (universal && !perArch) { errors.push(`${label} needs { aarch64, x86_64 } sources for universal-apple-darwin (each slice is a thin file)`); continue; }
    if (perArch) {
      for (const key of Object.keys(value)) if (!SOURCE_ARCHES.includes(key)) errors.push(`${label}: unknown arch ${JSON.stringify(key)}`);
      out[tool] = Object.fromEntries(SOURCE_ARCHES.filter((arch) => arch in value).map((arch) => [arch, parseFfmpegSource(value[arch], `${label}.${arch}`, errors)]));
    } else out[tool] = parseFfmpegSource(value, label, errors);
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
    const clash = findClaimClash(claimed, key);
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
      const { kind: _kind, platforms: _platforms, catalog: rawCatalog, sources: rawSources, probe: rawProbe, resourceDir: rawResourceDir, ...rest } = item;
      if (rest.stage !== undefined && isObject(rest.stage) && rest.stage.cmd === "cargo") errors.push(`${where}: stage.cmd must not be bare cargo; use { cmd: "rustright", args: ["cargo", ...] }`);
      if (rest.stage !== undefined && isObject(rest.stage) && rest.stage.cmd === "rightkit") errors.push(`${where}: stage.cmd "rightkit" is the retired name of the RustRight client (rustright#91); use { cmd: "rustright", args: ["cargo", ...] }`);
      const catalogSpec = parseCatalogSpec(rawCatalog, `${where}: catalog`, errors, "ffmpeg");
      if (catalogSpec) entry.catalogSpec = catalogSpec;
      if (rawProbe !== undefined && typeof rawProbe !== "boolean") errors.push(`${where}: probe must be a boolean`);
      if (rawProbe === true) entry.probe = true;
      const sources = parseFfmpegSources(rawSources, where, errors, { universal: t.arch === "universal" });
      if (sources) entry.sources = sources;
      // `resourceDir` stages flat files (<resourceDir>/ffmpeg[.exe]); `bundleDir` is the key of the externalBin layout, so the two never combine.
      const resourceDir = rawResourceDir === undefined ? undefined : relativePath(rawResourceDir, `${where}: resourceDir`, errors);
      if (rawResourceDir !== undefined && item.bundleDir !== undefined) errors.push(`${where}: bundleDir and resourceDir cannot be combined: bundleDir places an externalBin in the app bundle, resourceDir stages flat files (drop bundleDir)`);
      if (resourceDir) entry.resourceDir = resourceDir;
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
            for (const tool of entry.media.tools) claim(ffmpegOutput(entry, stageDir, tool, t), where);
          } catch (error) { mediaErrors(error); }
        } else {
          try {
            const config = validateMediaSidecarConfig({ ...rest, arch: t.arch, stageDir }, { platform: t.platform, hostArch: t.arch });
            for (const tool of config.tools) claim(ffmpegOutput(entry, stageDir, tool, t), where);
            entry.media = config;
          } catch (error) { mediaErrors(error); }
        }
      }
    } else if (item.kind === "onnxruntime" || item.kind === "native-library") {
      // `onnxruntime` is the alias of `native-library` with library onnxruntime: one code path, the alias keeps its own keys and receipt kind.
      const native = item.kind === "native-library";
      const library = native ? item.library : "onnxruntime";
      if (native) {
        if (typeof library !== "string" || !library) errors.push(`${where}: library is required (one of ${NATIVE_LIBRARIES.join(", ")})`);
        else if (Object.hasOwn(NATIVE_OTHER_ROUTE, library)) errors.push(`${where}: library ${library} ${NATIVE_OTHER_ROUTE[library]}`);
        else if (!NATIVE_LIBRARIES.includes(library)) errors.push(`${where}: unknown library ${JSON.stringify(library)} (one of ${NATIVE_LIBRARIES.join(", ")})`);
        if (typeof item.version !== "string" || !NATIVE_VERSION.test(item.version)) errors.push(`${where}: version must be a catalog version token (letters, digits and . _ + / -), such as 1.24.4 or chromium/8021`);
      } else if (typeof item.version !== "string" || !/^\d+\.\d+\.\d+$/.test(item.version)) errors.push(`${where}: version must be x.y.z`);
      const resourceDir = relativePath(item.resourceDir ?? DEFAULT_ORT_RESOURCE_DIR, `${where}: resourceDir`, errors);
      const licenseDir = relativePath(item.licenseDir ?? (resourceDir ? `${resourceDir}/LICENSES` : "x"), `${where}: licenseDir`, errors);
      const cacheDir = relativePath(item.cacheDir ?? (native ? ".cache/native-library" : ".cache/onnxruntime"), `${where}: cacheDir`, errors);
      const catalogSpec = parseCatalogSpec(item.catalog, `${where}: catalog`, errors, item.kind);
      if (item.as !== undefined && (typeof item.as !== "string" || !NAME.test(item.as))) errors.push(`${where}: as must be a bare file name`);
      if (item.companions !== undefined && typeof item.companions !== "boolean") errors.push(`${where}: companions must be a boolean`);
      let files;
      let source;
      if (native) {
        if (item.files !== undefined) {
          if (!Array.isArray(item.files) || !item.files.length || item.files.some((file) => typeof file !== "string" || !NAME.test(file))) errors.push(`${where}: files must be a non-empty list of bare file names (catalog artifactFileName values)`);
          else if (new Set(item.files).size !== item.files.length) errors.push(`${where}: files names a file twice`);
          else files = [...item.files];
          if (item.as !== undefined && Array.isArray(item.files) && item.files.length !== 1) errors.push(`${where}: as renames one file, so files must list exactly one (it lists ${item.files.length})`);
        }
        if (item.source !== undefined) {
          if (!isObject(item.source)) errors.push(`${where}: source must be { dir }`);
          else {
            for (const key of Object.keys(item.source)) if (key !== "dir") errors.push(`${where}: source: unknown key ${JSON.stringify(key)} (allowed: dir)`);
            source = parseSourceDir(item.source.dir, `${where}: source.dir`, errors);
            if (source) source = { dir: source };
          }
        }
        // Only the names known now are claimed; a catalog-driven entry (no files, no as) writes whatever its rows name.
        if (!skipped && resourceDir) for (const name of typeof item.as === "string" ? [item.as] : (files ?? [])) claim(`${resourceDir}/${name}`, where);
      }
      Object.assign(entry, { library, version: item.version, resourceDir, licenseDir, cacheDir, companions: item.companions !== false, ...(catalogSpec ? { catalogSpec } : {}), ...(item.as ? { as: item.as } : {}), ...(files ? { files } : {}), ...(source ? { source } : {}) });
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
      if (item.extract !== undefined) { const member = normalizeArchiveMember(item.extract); if (member.error || (!member.byPath && !NAME.test(member.value))) errors.push(`${where}: extract must be the bare file name of one archive member or its archive path (posix, no leading /, no .. segment): ${member.error ?? "invalid file name"}`); }
      if (item.memberSha256 !== undefined && (item.extract === undefined || !SHA256.test(item.memberSha256 ?? ""))) errors.push(`${where}: memberSha256 must be a lowercase 64-hex SHA-256 and needs extract`);
      if (item.extractKind !== undefined && item.extractKind !== "nsis") errors.push(`${where}: extractKind must be "nsis" (omit it for tar-readable zip/tgz/nupkg archives)`);
      if (item.extractKind === "nsis") {
        if (item.extract === undefined) errors.push(`${where}: extractKind "nsis" needs extract (the installer member path, e.g. bin/smartctl.exe)`);
        if (typeof item.memberSha256 !== "string" || !SHA256.test(item.memberSha256)) errors.push(`${where}: extractKind "nsis" requires memberSha256 (the extracted member's lowercase 64-hex SHA-256); an NSIS member is never staged unpinned`);
      }
      if (item.executable !== undefined && typeof item.executable !== "boolean") errors.push(`${where}: executable must be a boolean`);
      const clean = cleanOf();
      if (dest && !skipped) claim(dest, where);
      Object.assign(entry, { name, url: item.url, sha256: item.sha256, dest, ...(item.extractKind ? { extractKind: item.extractKind } : {}), ...(item.size ? { size: item.size } : {}), ...(item.extract ? { extract: item.extract } : {}), ...(item.memberSha256 ? { memberSha256: item.memberSha256 } : {}), ...(item.executable ? { executable: true } : {}), ...(clean ? { clean } : {}) });
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
      if (item.generator !== undefined && (typeof item.generator !== "string" || !CMAKE_GENERATOR.test(item.generator))) errors.push(`${where}: generator must be a generator name such as "Ninja"`);
      const platformGenerators = {};
      if (item.platformGenerators !== undefined) {
        if (!isObject(item.platformGenerators)) errors.push(`${where}: platformGenerators must be an object keyed by mac, win or linux`);
        else for (const [key, generator] of Object.entries(item.platformGenerators)) {
          if (!PLATFORMS.has(key)) errors.push(`${where}: platformGenerators.${key}: platforms are ${[...PLATFORMS].join(", ")}`);
          else if (typeof generator !== "string" || !CMAKE_GENERATOR.test(generator)) errors.push(`${where}: platformGenerators.${key} must be a generator name such as "Visual Studio 17 2022"`);
          else platformGenerators[key] = generator;
        }
      }
      if (item.architecture !== undefined && (typeof item.architecture !== "string" || !CMAKE_ARCHITECTURE.test(item.architecture))) errors.push(`${where}: architecture must be a cmake -A platform such as x64, Win32 or ARM64`);
      if (item.cmakeBin !== undefined && (typeof item.cmakeBin !== "string" || !item.cmakeBin.trim() || /[\0\r\n]/.test(item.cmakeBin))) errors.push(`${where}: cmakeBin must be a cmake executable name or path`);
      if (item.smoke !== undefined && (!Array.isArray(item.smoke) || item.smoke.some((arg) => typeof arg !== "string"))) errors.push(`${where}: smoke must be a list of argument strings`);
      sha(item.sha256, "sha256");
      const dest = destOf(item.dest);
      const clean = cleanOf();
      if (!skipped) { if (dest) claim(dest, where); else if (typeof name === "string" && stageDir) claim(stagedClaim(name), where); }
      Object.assign(entry, { name, repo: item.repo, commit: item.commit, target, ...(output ? { output } : {}), cmakeArgs, platformArgs, ...(item.generator ? { generator: item.generator } : {}), ...(Object.keys(platformGenerators).length ? { platformGenerators } : {}), ...(item.architecture ? { architecture: item.architecture } : {}), ...(item.cmakeBin ? { cmakeBin: item.cmakeBin.trim() } : {}), ...(item.smoke ? { smoke: [...item.smoke] } : {}), ...(item.sha256 ? { sha256: item.sha256 } : {}), ...(dest ? { dest } : {}), ...(clean ? { clean } : {}) });
    } else if (item.kind === "prebuilt") {
      const name = item.name;
      if (typeof name !== "string" || !NAME.test(name) || /\.exe$/i.test(name) || name !== path.basename(name)) errors.push(`${where}: name must be a bare sidecar name (no path, no .exe, no target triple)`);
      let from;
      if (isObject(item.from)) {
        // Per-arch inputs: each slice is verified against its own manifest entry; universal-apple-darwin lipo-merges them.
        from = {};
        for (const [arch, value] of Object.entries(item.from)) {
          if (!SOURCE_ARCHES.includes(arch)) errors.push(`${where}: from.${arch}: arches are ${SOURCE_ARCHES.join(", ")}`);
          else { const file = relativePath(value, `${where}: from.${arch}`, errors); if (file) from[arch] = file; }
        }
        if (!Object.keys(item.from).length) errors.push(`${where}: from must name at least one of ${SOURCE_ARCHES.join(", ")}`);
        if (!skipped && t.arch === "universal" && !SOURCE_ARCHES.every((arch) => arch in item.from)) errors.push(`${where}: from needs both aarch64 and x86_64 for universal-apple-darwin (or omit from to use the staged <name>-<slice triple> files)`);
      } else from = item.from === undefined ? undefined : relativePath(item.from, `${where}: from`, errors);
      const manifest = parsePrebuiltManifest(item.manifest, where, errors);
      if (!skipped && t.arch === "universal" && typeof item.from === "string") errors.push(`${where}: from must be { aarch64, x86_64 } for universal-apple-darwin (each slice is verified against its own manifest entry, then lipo-merged)`);
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
  return { target: t, stageDir, receiptPath, wipeStageDir: wrapper.wipeStageDir === true, entries, claims: claimed };
}

const sha256Of = (bytes) => createHash("sha256").update(bytes).digest("hex");
/**
 * Signature-independent catalog digest of `bytes` for `arch` (content-digest.mjs, port of
 * rightkit-ffmpeg content_digest.rs). Catalog `contentSha256` rows pin THIS digest, never a
 * whole-file SHA-256: signed & unsigned copies of one build agree. A malformed recognized
 * format throws ContentDigestError (never a plain-hash fallback). Whole-file hashes elsewhere
 * (archive/download pins, verifyStagedSidecars preSignSha256 signing evidence) stay separate.
 */
function catalogContentDigest(bytes, arch) {
  const identity = contentIdentity(bytes);
  return digestFor(identity, arch) ?? (identity.slices.length === 1 ? identity.slices[0].sha256 : null);
}
const profileDirectory = (profile) => (profile === "dev" || profile === "test" ? "debug" : profile === "bench" ? "release" : profile);
const tail = (text, lines = 12) => String(text ?? "").trim().split("\n").slice(-lines).join("\n");

/** Default process runner: `rustright` resolves through the installed runtime client (its Windows shim is rustright.cmd). */
export function defaultRun(command, args, { cwd, env = process.env } = {}) {
  if (command === "rightkit") throw new Error("`rightkit` is the retired name of the RustRight client (rustright#91); use rustright (stage: { cmd: \"rustright\" })");
  // Every `rustright cargo` of this module is a right-release Cargo call: the one carried seam (managed-cargo.mjs runCargoSync) takes it,
  // so a snapshot stage's call is carried and verified, and any other host runs it as before.
  if (command === "rustright" && args[0] === "cargo") return runCargoSync(args.slice(1), { cwd, env, encoding: "utf8", maxBuffer: 512 * 1024 * 1024, shell: false, windowsHide: true, stdio: ["inherit", "pipe", "inherit"] });
  const launch = command === "rustright" ? resolveCommand(command, args, { root: cwd, env }) : { command, args };
  return nodeSpawnSync(launch.command, launch.args, {
    cwd, env: { ...env, ...(launch.env ?? {}) }, encoding: "utf8", maxBuffer: 512 * 1024 * 1024, shell: false, windowsHide: true, stdio: ["inherit", "pipe", "inherit"],
  });
}

/**
 * Default archive member extraction (bsdtar reads tgz and zip/nupkg). A bare `fileName` selects the one member whose basename it is;
 * a `fileName` containing `/` selects the one member whose full archive path equals it (case-sensitive, leading `./` ignored on both
 * sides), so archives with the same base name in several directories (ORT DirectML nupkgs) can be addressed. The returned file is
 * always under `destDir`; callers copy it to their own declared output name, so the member path never decides where anything is written.
 * A bare name ignores members inside a `.dSYM` bundle: Apple archives (ORT osx tgz) ship the DWARF copy under the same base name.
 */
const isDebugSymbolMember = (member) => member.split("/").some((part) => part.toLowerCase().endsWith(".dsym"));

export function defaultExtractMember({ archive, fileName, destDir, run = defaultRun }) {
  const wanted = normalizeArchiveMember(fileName);
  if (wanted.error) fail("STAGE_SIDECAR_EXTRACT_FAILED", `archive member ${JSON.stringify(fileName)} ${wanted.error}`);
  const listing = run("tar", ["-tf", archive], { cwd: path.dirname(archive) });
  if (listing.status !== 0) fail("STAGE_SIDECAR_EXTRACT_FAILED", `cannot list ${archive}`);
  const lines = String(listing.stdout).split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  const members = wanted.byPath
    ? lines.filter((line) => (line.startsWith("./") ? line.slice(2) : line) === wanted.value)
    : lines.filter((line) => path.posix.basename(line) === wanted.value && !isDebugSymbolMember(line));
  if (members.length !== 1) fail("STAGE_SIDECAR_EXTRACT_FAILED", `${archive} has ${members.length} members ${wanted.byPath ? "at the path" : "named"} ${wanted.value}, expected exactly one`);
  const extracted = run("tar", ["-xf", archive, "-C", destDir, members[0]], { cwd: path.dirname(archive) });
  if (extracted.status !== 0) fail("STAGE_SIDECAR_EXTRACT_FAILED", `cannot extract ${members[0]} from ${archive}`);
  const out = path.join(destDir, ...members[0].split("/"));
  const inside = path.relative(path.resolve(destDir), path.resolve(out));
  if (!inside || inside === ".." || inside.startsWith(`..${path.sep}`) || path.isAbsolute(inside)) fail("STAGE_SIDECAR_EXTRACT_FAILED", `member ${members[0]} of ${archive} resolves outside the extraction directory`);
  return out;
}

/** 7-Zip candidates: an explicit path (deps.sevenZip or RIGHTKIT_SEVEN_ZIP), then `7z` on PATH, then 7z.exe under Program Files on win32. */
export function sevenZipCandidates({ explicit, env = process.env, platform = process.platform } = {}) {
  const list = [];
  if (explicit) list.push(explicit);
  else if (env.RIGHTKIT_SEVEN_ZIP) list.push(env.RIGHTKIT_SEVEN_ZIP);
  list.push("7z");
  if (platform === "win32") for (const base of [env.ProgramFiles ?? "C:\\Program Files", env["ProgramFiles(x86)"] ?? "C:\\Program Files (x86)"]) list.push(path.win32.join(base, "7-Zip", "7z.exe"));
  return [...new Set(list)];
}

/**
 * Extract one member of an NSIS installer with 7-Zip (`x -y -o<dir> <installer> <member>`; nothing in the installer is run), then
 * require its SHA-256 to equal the pinned one. Fails closed, naming the installer digest, member, expected and actual digests, on a
 * missing tool, a failed extraction, a missing member or any mismatch. Returns the extracted file path under destDir.
 */
export function defaultExtractNsisMember({ archive, fileName, destDir, expectedSha256, installerSha256, run = defaultRun, sevenZip, env = process.env, platform = process.platform, readFile = nodeFs.readFileSync, exists = nodeFs.existsSync }) {
  const wanted = normalizeArchiveMember(fileName);
  const who = `NSIS installer ${installerSha256 ?? archive} member ${JSON.stringify(fileName)}`;
  if (wanted.error) fail("STAGE_SIDECAR_EXTRACT_FAILED", `${who} ${wanted.error}`);
  if (!SHA256.test(expectedSha256 ?? "")) fail("STAGE_SIDECAR_EXTRACT_FAILED", `${who}: a pinned member sha256 is required`);
  let tool;
  let lastError = "not found";
  for (const candidate of sevenZipCandidates({ explicit: sevenZip, env, platform })) {
    const result = run(candidate, ["x", "-y", `-o${destDir}`, archive, wanted.value], { cwd: path.dirname(archive) });
    if (result.error || result.status === null || result.status === undefined) { lastError = result.error?.message ?? "could not start"; continue; }
    tool = candidate;
    if (result.status !== 0) fail("STAGE_SIDECAR_EXTRACT_FAILED", `${who}: ${candidate} exited ${result.status}; nothing staged`);
    break;
  }
  if (!tool) fail("STAGE_SIDECAR_EXTRACT_FAILED", `${who}: 7-Zip is required and was not found (${lastError}); install 7-Zip or set RIGHTKIT_SEVEN_ZIP; nothing staged`);
  const out = path.join(destDir, ...wanted.value.split("/"));
  const inside = path.relative(path.resolve(destDir), path.resolve(out));
  if (!inside || inside === ".." || inside.startsWith(`..${path.sep}`) || path.isAbsolute(inside)) fail("STAGE_SIDECAR_EXTRACT_FAILED", `${who} resolves outside the extraction directory`);
  if (!exists(out)) fail("STAGE_SIDECAR_EXTRACT_FAILED", `${who}: member not found after extraction; nothing staged`);
  const actual = sha256Of(readFile(out));
  if (actual !== expectedSha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${who}: sha256 is ${actual}, expected ${expectedSha256}; nothing staged`, { expected: expectedSha256, actual });
  return out;
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
  if (spec.via === "absolute") return requireSnapshotSource(ctx, spec.file, `the ${label} catalog`);
  if (spec.via === "path") return requireSnapshotSource(ctx, path.resolve(ctx.appRoot, spec.file), `the ${label} catalog`);
  if (spec.via === "crate") {
    const meta = ctx.run("rustright", ["cargo", "metadata", "--frozen", "--format-version", "1"], { cwd: ctx.appRoot, env: ctx.env });
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

/**
 * Warm-cache rule (GATE-CONTRACT "rebuild only what changed"): a staged file carries the mtime of the cache file it was copied from, a
 * lipo or merge output the newest mtime of its inputs, so a re-stage after the snapshot sync (which removes every ledger entry) does not
 * bump the mtime and re-trigger the app crate (tauri-build emits rerun-if-changed for the staged sidecar). Bytes are bound by the
 * ledger, never by mtime. A file system without `utimesSync` or `statSync().mtime` (a test double) is left as written.
 */
function carryMtime(ctx, dest, sources) {
  const { fs } = ctx;
  if (typeof fs.utimesSync !== "function") return;
  // Millisecond precision (a Date), FLOORED: Node's `stat().mtime` rounds the file's sub-millisecond part to the nearest millisecond, so a source whose mtime is x.6 ms would be carried
  // as x+1 ms, newer than the source it was copied from; floored, the carried mtime is never newer than the source and less than a millisecond older. `mtimeMs` carries the
  // fraction; a file system double that gives only a `mtime` Date is carried as that Date.
  let newest = null;
  for (const source of sources) {
    let st; try { st = fs.statSync(source); } catch { continue; }
    const ms = Number.isFinite(st?.mtimeMs) ? Math.floor(st.mtimeMs) : st?.mtime instanceof Date ? st.mtime.getTime() : null;
    if (ms !== null && (newest === null || ms > newest)) newest = ms;
  }
  if (newest !== null) { const carried = new Date(newest); try { fs.utimesSync(dest, carried, carried); } catch { /* the mtime is a cache hint, never a binding */ } }
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
  carryMtime(ctx, dest, [source]);
  return { sha256, sizeBytes: fs.statSync(dest).size, changed };
}

const rel = (ctx, file) => path.relative(ctx.appRoot, file).replaceAll("\\", "/");

/**
 * The boundary of a split run (row 7.05a): the only checkout bytes it may read are cache entries whose digest the commit pins or
 * tool-invalidated build caches, both under `ctx.cacheRoot`. A SOURCE the config names (a local `source.dir`, an ffmpeg `path`) must
 * resolve inside the snapshot's app root, never into the checkout or anywhere else.
 */
function requireSnapshotSource(ctx, file, label) {
  if (!ctx.split) return file;
  const relative = path.relative(ctx.appRoot, path.resolve(file));
  if (relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) fail("STAGE_SIDECAR_SOURCE_OUTSIDE_SNAPSHOT", `${label} resolves to ${file}, outside the snapshot's app root ${ctx.appRoot}; a split run reads sources only from the snapshot (checkout bytes enter only as digest-pinned cache entries)`, { file });
  return file;
}
/**
 * The Cargo output directory of the workspace a split run builds in, as the checkout's absolute path, or `{ error }`. Resolved from the SNAPSHOT: `cargo
 * metadata --offline --no-deps` runs in the snapshot's app root (with `--manifest-path` the snapshot's manifest when the entry names one), so the
 * committed manifests and Cargo configuration decide, and a path it reports under the snapshot is mapped lexically to the same relative path under the
 * checkout (the output directory is the checkout's, GATE-CONTRACT "Output directory & cache"; cargoOutputPin does the same for the stage's own calls). The
 * checkout is never asked: a newer HEAD that deleted the crate's directory, a dirty manifest or another build.target-dir there changes nothing (audit GB-16).
 * A path outside the snapshot (an absolute CARGO_TARGET_DIR, a target-dir set in the user's Cargo home) stands as reported.
 */
function snapshotTargetDirectory(ctx, env, manifest) {
  const resolved = ctx.run("rustright", ["cargo", "metadata", "--offline", "--format-version", "1", "--no-deps", ...(manifest ? ["--manifest-path", manifest] : [])], { cwd: ctx.appRoot, env });
  let dir;
  try { dir = JSON.parse(resolved?.stdout).target_directory; } catch { /* reported below */ }
  if (resolved?.error || resolved?.status !== 0 || typeof dir !== "string" || !path.isAbsolute(dir)) return { error: resolved?.error?.message ?? `exit ${resolved?.status}` };
  const roots = splitRoots(ctx);
  for (const snapshot of roots.snapshots) {
    const rel = path.relative(snapshot, dir);
    if (rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel))) return { dir: path.join(roots.checkout, rel) };
  }
  return { dir: path.resolve(dir) };
}

/** The snapshot's and the checkout's roots of a split run: `appRoot` and `cacheRoot` are the same app directory in each, so what their paths do not share is the root. */
function splitRoots(ctx) {
  const a = path.resolve(ctx.appRoot).split(path.sep); const c = path.resolve(ctx.cacheRoot).split(path.sep);
  let n = 0;
  while (n < a.length - 1 && n < c.length - 1 && a[a.length - 1 - n] === c[c.length - 1 - n]) n += 1;
  const snapshot = a.slice(0, a.length - n).join(path.sep) || path.sep;
  const snapshots = [snapshot];
  try { snapshots.push(nodeFs.realpathSync.native(snapshot)); } catch { /* the path as given */ }
  return { snapshots, checkout: c.slice(0, c.length - n).join(path.sep) || path.sep };
}

/**
 * The environment a split run hands to the commands it starts (a `command` entry, an ffmpeg `stage` command): Cargo's output directory is
 * the checkout's, resolved there once and passed absolute, so a command that reaches Cargo (`rustright cargo run -p ...`) never writes
 * `target/` into the snapshot. Without a split, or when the checkout holds no Cargo workspace, the environment is returned as it is.
 */
function splitCommandEnv(ctx) {
  if (!ctx.split) return ctx.env;
  // A carried stage (carried-cargo.mjs): the broker pins every Cargo output, so nothing is pinned here and the commands inherit the carrier.
  if (carriedCargoMapOf(ctx.env)?.map) return ctx.env;
  if (ctx.splitEnv === undefined) {
    const pinned = snapshotTargetDirectory(ctx, ctx.env, null); // no Cargo workspace in the snapshot: nothing to pin
    ctx.splitEnv = pinned.dir ? { ...ctx.env, CARGO_TARGET_DIR: pinned.dir } : ctx.env;
  }
  return ctx.splitEnv;
}

/**
 * One step command of a config (`stage.cmd`, a `command` entry's `cmd`, the cmake tool). In a snapshot stage with a carrier (the gate's
 * carrier map in the environment) every compile is carried or nested under a carried adapter job, so such a step goes through the door
 * (build-launch.mjs): `pnpm`, `node` and `swift` become carried adapter jobs, `rightkit-qa` runs direct (it compiles nothing), `rustright cargo` keeps the carried seam
 * (defaultRun), this package's own `right-release` runs as it is, and anything else (`cmake`, a bare tool, `rustright <not cargo>`) is
 * refused with STAGE_SIDECAR_STEP_REFUSED, naming the step: it would otherwise run uncarried around the broker. Without a carrier the
 * step runs exactly as before. Utility commands that compile nothing (`tar`, `codesign`) never come through here.
 */
export function runStep(ctx, command, args, options = {}) {
  const env = options.env ?? ctx.env;
  const carried = carriedCargoMapOf(env);
  if (!carried || (command === "rustright" && String(args[0]) === "cargo")) return ctx.run(command, args, options);
  const describe = `${command} ${args.join(" ")}`.trim();
  if (carried.error) fail("STAGE_SIDECAR_STEP_REFUSED", `step \`${describe}\`: ${carried.error}; a snapshot stage's step is never run around its carrier`, { command });
  const own = command === "right-release" || (command === "pnpm" && ((args[0] === "exec" && args[1] === "right-release") || args[0] === "right-release"));
  let launch;
  try {
    launch = own ? resolveCommand(command, args, { root: options.cwd, env }) : buildLaunch(command, args, { env, cwd: options.cwd, exists: ctx.fs?.existsSync });
  } catch (error) {
    if (error?.code === BUILD_LAUNCH_REFUSED) fail("STAGE_SIDECAR_STEP_REFUSED", `step \`${describe}\` of a snapshot stage is refused: ${error.message}`, { command });
    throw error;
  }
  return ctx.run(launch.command, launch.args, { ...options, env: launch.env ? { ...env, ...launch.env } : env });
}

/** A cache directory the config names (`cacheDir`): the checkout's `.right-release` under a split, the app root otherwise. */
const cacheDirFor = (ctx, dir) => (ctx.split ? path.resolve(ctx.cacheRoot, ".right-release", dir) : path.resolve(ctx.appRoot, dir));
/** The per-arch slice directory of a universal staging: absolute under the checkout's cache root on a split, app-relative otherwise. */
const sliceDirFor = (ctx, triple) => (ctx.split ? path.resolve(ctx.cacheRoot, SLICE_ROOT, triple) : `${SLICE_ROOT}/${triple}`);

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
  carryMtime(ctx, dest, slices);
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
  carryMtime(ctx, dest, slices.map((slice) => slice.source));
  return [{ kind: "cargo-bin", name: entry.name, path: rel(ctx, dest), sha256, sizeBytes: fs.statSync(dest).size, changed, sourceSha256: sha256, package: entry.package, bin: entry.bin, profile: entry.profile, features: entry.features, externalBin: !entry.dest, slices: slices.map((slice) => ({ triple: slice.triple, sha256: slice.sourceSha256 })) }];
}

function stageCargoSlice(entry, ctx, triple, { dest }) {
  const { source, sourceSha256 } = stageCargoBuild(entry, ctx, triple);
  if (entry.sha256 && entry.sha256 !== sourceSha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${entry.bin} built to ${sourceSha256}, expected ${entry.sha256}`, { expected: entry.sha256, actual: sourceSha256 });
  const copied = copyInto(ctx, source, dest, { executable: true });
  return [{ kind: "cargo-bin", name: entry.name, path: rel(ctx, dest), ...copied, sourceSha256, package: entry.package, bin: entry.bin, profile: entry.profile, features: entry.features, externalBin: !entry.dest }];
}

/** Build one slice through `rustright cargo` and locate the executable. Returns { source, sourceSha256 }. */
function stageCargoBuild(entry, ctx, triple) {
  const { run, appRoot, env, fs } = ctx;
  const args = ["cargo", "build", "--locked", ...(entry.manifestPath ? ["--manifest-path", path.resolve(appRoot, entry.manifestPath)] : []),
    "-p", entry.package, "--bin", entry.bin, "--target", triple, "--profile", entry.profile,
    ...(entry.features.length ? ["--features", entry.features.join(",")] : []), "--message-format=json-render-diagnostics"];
  ctx.log(`stage-sidecar: rightkit ${args.join(" ")}`);
  // From a snapshot, Cargo would resolve `target/` (or a relative build.target-dir) against the snapshot: its output directory is
  // resolved in the checkout instead and passed as an absolute CARGO_TARGET_DIR, so the warm outputs stay the checkout's and the
  // snapshot holds no target/ (docs/GATE-CONTRACT.md "Output directory & cache").
  let buildEnv = env;
  // A carried stage (carried-cargo.mjs, a broker host): the broker pins the output directory and the call passes no CARGO_TARGET_DIR;
  // the directory the binary is read from is the signed receipt's `outputDir`, never a carrier-less `cargo metadata`.
  const carried = Boolean(carriedCargoMapOf(env)?.map);
  if (ctx.split && !carried) {
    const pinned = snapshotTargetDirectory(ctx, env, entry.manifestPath ? path.resolve(appRoot, entry.manifestPath) : null);
    if (!pinned.dir) fail("STAGE_SIDECAR_BUILD_FAILED", `the cargo output directory for ${entry.package} cannot be resolved from the snapshot (${pinned.error}); nothing was built`, { package: entry.package });
    buildEnv = { ...env, CARGO_TARGET_DIR: pinned.dir };
  }
  const built = run("rustright", args, { cwd: appRoot, env: buildEnv });
  if (built?.error || built?.status !== 0) fail("STAGE_SIDECAR_BUILD_FAILED", `rustright cargo build of ${entry.package} failed (${built?.error?.message ?? `exit ${built?.status}`})`, { package: entry.package, stderr: tail(built?.stderr) });
  let source = artifactFromStream(built.stdout, entry.bin);
  if (carried && (!source || !fs.existsSync(source))) {
    const targetDir = typeof built.outputDir === "string" && path.isAbsolute(built.outputDir) ? built.outputDir : undefined;
    if (!targetDir) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `cargo reported no ${entry.bin} executable and the receipt names no output directory`, { bin: entry.bin });
    const candidates = builtBinaryCandidates({ targetDir, targetDirs: [], triple, profile: entry.profile, bin: entry.bin, windows: ctx.windows });
    source = candidates.find((file) => fs.existsSync(file));
    if (!source) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `the build did not produce ${entry.bin} (looked in ${candidates.join(", ")})`, { bin: entry.bin, candidates });
  } else if (!source || !fs.existsSync(source)) {
    const meta = run("rustright", ["cargo", "metadata", "--format-version", "1", "--no-deps", ...(entry.manifestPath ? ["--manifest-path", path.resolve(appRoot, entry.manifestPath)] : [])], { cwd: appRoot, env: buildEnv });
    let targetDir;
    try { targetDir = JSON.parse(meta.stdout).target_directory; } catch { /* refused below */ }
    if (meta?.status !== 0 || typeof targetDir !== "string" || !path.isAbsolute(targetDir)) targetDir = undefined;
    // A managed target root: the broker/cargo metadata target_directory (which cargo derives from CARGO_TARGET_DIR) is
    // authoritative; an explicit CARGO_TARGET_DIR in this environment is tried too, resolved against the app root.
    const envDir = typeof buildEnv.CARGO_TARGET_DIR === "string" && buildEnv.CARGO_TARGET_DIR.trim() ? path.resolve(appRoot, buildEnv.CARGO_TARGET_DIR) : undefined;
    if (!targetDir && !envDir) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `cargo reported no ${entry.bin} executable and its target directory cannot be read`, { bin: entry.bin });
    const candidates = builtBinaryCandidates({ targetDir, targetDirs: envDir ? [envDir] : [], triple, profile: entry.profile, bin: entry.bin, windows: ctx.windows });
    source = candidates.find((file) => fs.existsSync(file));
    if (!source) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `the build did not produce ${entry.bin} (looked in ${candidates.join(", ")})`, { bin: entry.bin, candidates });
  }
  const sourceSha256 = sha256Of(fs.readFileSync(source));
  return { source, sourceSha256 };
}

/** The archive source for one tool at one arch: a single source, or the per-arch form. */
const ffmpegSourceFor = (sources, tool, arch) => { const spec = sources?.[tool]; return spec?.url || spec?.path ? spec : spec?.[arch]; };

/** The binary a local ffmpeg source names: the file itself, or `<dir>/<member>` / `<dir>/bin/<member>` (`.exe` added for a Windows target). */
function resolveLocalFfmpegSource(ctx, spec, tool) {
  const { fs } = ctx;
  const base = requireSnapshotSource(ctx, path.resolve(ctx.appRoot, spec.path), `the ffmpeg ${tool} source path`);
  if (fs.existsSync(base) && fs.statSync(base).isFile()) return base;
  const member = spec.member ?? `${tool}${ctx.windows ? ".exe" : ""}`;
  const candidates = [path.join(base, member), path.join(base, "bin", member)];
  const found = candidates.find((candidate) => fs.existsSync(candidate) && fs.statSync(candidate).isFile());
  if (!found) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `the local ${tool} source ${spec.path} has no ${member} (looked in ${candidates.join(", ")}); vendor the build first, nothing is downloaded for a path source`, { tool, candidates });
  return found;
}

/**
 * Fetch the bytes of the catalog rows that `ffmpeg.sources` names (a local `path` is copied, never downloaded): an archive pinned by digest, one member unpacked, the
 * member's digest checked against the APPROVED catalog row (so a swapped archive can never stage). A staged file that already
 * matches its row is left alone.
 */
async function fetchFfmpegSources(entry, ctx, plan, arch) {
  if (!entry.sources) return;
  const { fs } = ctx;
  for (const file of plan.files) {
    const spec = ffmpegSourceFor(entry.sources, file.tool, arch);
    if (!spec) continue;
    if (fs.existsSync(file.stagedPath) && catalogContentDigest(fs.readFileSync(file.stagedPath), arch) === file.contentSha256) {
      ctx.log(`stage-sidecar: ${rel(ctx, file.stagedPath)} already staged`);
      if (!ctx.windows) fs.chmodSync(file.stagedPath, 0o755);
      continue;
    }
    if (spec.path) {
      const local = resolveLocalFfmpegSource(ctx, spec, file.tool);
      const actual = catalogContentDigest(fs.readFileSync(local), arch);
      if (actual !== file.contentSha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${local} is ${actual}, the approved catalog row ${file.artifactId} says ${file.contentSha256}; nothing staged`, { expected: file.contentSha256, actual });
      copyInto(ctx, local, file.stagedPath, { executable: true });
      continue;
    }
    const { cached } = await ensureCachedDownload(ctx, spec);
    const scratch = ctx.makeTempDir();
    try {
      const member = await ctx.extractMember({ archive: cached, fileName: spec.member, destDir: scratch });
      const actual = catalogContentDigest(fs.readFileSync(member), arch);
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
 * `flat` (a thin target of a `resourceDir` entry) moves each file to `<resourceDir>/<tool>[.exe]`; a universal slice stays
 * `<SLICE_ROOT>/<triple>/<tool>-<triple>[.exe]` and only its merge result is flat. Returns { plan, rows }; each row is a receipt row.
 */
async function stageFfmpegSlice(entry, ctx, resolved, { config, t, writeNotices, flat = false }) {
  const { appRoot, run, env, fs } = ctx;
  // A universal slice is staged under SLICE_ROOT, which a split keeps in the checkout's cache root, never in the snapshot.
  const sliceRoot = ctx.split && String(config.stageDir ?? "").startsWith(SLICE_ROOT) ? ctx.cacheRoot : appRoot;
  const planned = planMediaSidecar({ config, catalog: resolved.catalog, crateDir: resolved.crateDir, crateVersion: resolved.crateVersion, catalogBytes: resolved.catalogBytes, root: sliceRoot });
  const plan = flat ? { ...planned, files: planned.files.map((file) => ({ ...file, stagedPath: path.resolve(appRoot, entry.resourceDir, file.bundledName) })) } : planned;
  await fetchFfmpegSources(entry, ctx, plan, t.arch);
  if (config.stage) {
    const universal = ctx.target.arch === "universal";
    // A universal entry stages once per slice: `{arch}`, `{triple}` and `{stageDir}` in the stage command's arguments name the slice.
    const args = (config.stage.args ?? []).map((arg) => (universal ? String(arg).replaceAll("{arch}", t.arch).replaceAll("{triple}", t.triple).replaceAll("{stageDir}", sliceRoot === appRoot ? config.stageDir : path.resolve(sliceRoot, config.stageDir)) : arg));
    ctx.log(`stage-sidecar: ${config.stage.cmd} ${args.join(" ")}`);
    const staged = runStep(ctx, config.stage.cmd, args, { cwd: appRoot, env: splitCommandEnv(ctx) });
    if (staged?.error || staged?.status !== 0) fail("STAGE_SIDECAR_BUILD_FAILED", `ffmpeg stage command failed (${staged?.error?.message ?? `exit ${staged?.status}`})`);
  }
  const noticeFiles = writeNotices ? writeMediaSidecarNotices(plan, { exists: fs.existsSync, mkdir: fs.mkdirSync, copy: fs.copyFileSync, write: fs.writeFileSync }) : [];
  const verified = verifyStagedSidecars(plan, { exists: fs.existsSync, stat: (file) => fs.statSync(file), hashFile: (file) => sha256Of(fs.readFileSync(file)) });
  const notices = noticeFiles.map((file) => ({ name: path.basename(file), path: rel(ctx, file), sha256: sha256Of(fs.readFileSync(file)) }));
  const rows = verified.map((file) => {
    // Catalog rows pin the signature-independent content digest; preSignSha256 stays whole-file signing evidence.
    const contentDigest = catalogContentDigest(fs.readFileSync(file.stagedPath), t.arch);
    if (contentDigest !== file.contentSha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${file.tool} staged with content digest ${contentDigest}, the approved catalog row ${file.artifactId} says ${file.contentSha256}`, { expected: file.contentSha256, actual: contentDigest });
    const probe = entry.probe ? probeFfmpegBinary(ctx, { file: file.stagedPath, tool: file.tool, target: t }) : undefined;
    return { kind: "ffmpeg", name: file.tool, path: rel(ctx, file.stagedPath), sha256: file.preSignSha256, sizeBytes: file.preSignSizeBytes, changed: null, catalogRow: file.artifactId, version: file.version, catalogContentSha256: file.contentSha256, ...(file.variant ? { variant: file.variant } : {}), externalBin: !flat, license: "LGPL-2.1-or-later", notices, ...(probe ? { probe } : {}) };
  });
  return { plan, rows };
}

/** universal-apple-darwin: stage each arch slice under SLICE_ROOT, then lipo-merge every tool into <stageDir>/<tool>-universal-apple-darwin (<resourceDir>/<tool> for a resourceDir entry). */
async function stageFfmpegUniversal(entry, ctx, resolved) {
  const { appRoot, fs } = ctx;
  const results = [];
  for (const slice of entry.slices) {
    results.push({ slice, ...(await stageFfmpegSlice(entry, ctx, resolved, { config: slice.media, t: parseTargetTriple(slice.triple), writeNotices: false })) });
  }
  // One notice set and one source offer that names the rows of every slice (a per-slice offer would overwrite the other).
  // A split plans each slice under the checkout's cache root (SLICE_ROOT), so the slice plans name their notices there too. The notices are FINAL
  // outputs and belong in the snapshot beside the merged file: they are rebased onto the app root before anything is written, never into the checkout.
  const toApp = (file) => {
    if (!ctx.split) return file;
    const relative = path.relative(ctx.cacheRoot, file);
    return relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative) ? file : path.resolve(appRoot, relative);
  };
  const noticeByDest = new Map();
  for (const { plan } of results) for (const notice of plan.notices) noticeByDest.set(toApp(notice.dest), { ...notice, dest: toApp(notice.dest) });
  const rowsById = new Map();
  for (const { plan } of results) for (const row of plan.rows) rowsById.set(row.id, row);
  const combined = {
    notices: [...noticeByDest.values()],
    offerPath: toApp(results[0].plan.offerPath),
    offer: sourceOfferText({ rows: [...rowsById.values()], contact: entry.media.notices.sourceOfferContact, crateVersion: resolved.crateVersion }),
  };
  const noticeFiles = writeMediaSidecarNotices(combined, { exists: fs.existsSync, mkdir: fs.mkdirSync, copy: fs.copyFileSync, write: fs.writeFileSync });
  const notices = noticeFiles.map((file) => ({ name: path.basename(file), path: rel(ctx, file), sha256: sha256Of(fs.readFileSync(file)) }));
  return entry.media.tools.map((tool) => {
    const parts = results.map(({ slice, rows }) => ({ slice, row: rows.find((row) => row.name === tool) }));
    const dest = path.resolve(appRoot, ffmpegOutput(entry, ctx.stageDir, tool, ctx.target));
    const merged = lipoMergeInto(ctx, parts.map(({ row }) => path.resolve(appRoot, row.path)), dest, `ffmpeg sidecar ${tool}`, { executable: true });
    const probe = entry.probe ? probeFfmpegBinary(ctx, { file: dest, tool, target: ctx.target }) : undefined;
    return {
      kind: "ffmpeg", name: tool, path: rel(ctx, dest), ...merged, sourceSha256: merged.sha256, catalogRow: parts.map(({ row }) => row.catalogRow).join("+"), version: parts[0].row.version,
      externalBin: !entry.resourceDir, license: "LGPL-2.1-or-later", notices, ...(probe ? { probe } : {}),
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
    return (await stageFfmpegSlice(entry, ctx, resolved, { config, t: target, writeNotices: true, flat: Boolean(entry.resourceDir) })).rows;
  } catch (error) {
    if (error instanceof StageSidecarError || error instanceof ContentDigestError) throw error;
    return fail("STAGE_SIDECAR_FFMPEG_FAILED", `ffmpeg sidecar (${target.triple}): ${error.message}`, { cause: error, causeCode: error.code });
  }
}

/** Catalog statuses that may serve a staged library; `stale-unverified` and `planned` rows never do. */
const SERVING_STATUS = new Set(["approved", "pinned"]);

/** An aarch64 or x86_64 macOS target: a `universal` row serves it when no exact row does. */
const appleThin = (t) => t.os === "macos" && (t.arch === "aarch64" || t.arch === "x86_64");
/** Does `row` serve target `t`: the exact (os, arch), or a macOS `universal` row for an Apple thin target. */
const rowServes = (row, t) => row.os === t.os && (row.arch === t.arch || (appleThin(t) && row.arch === "universal"));

/**
 * The catalog rows of the entry's library and version that serve target `t` (mirrors rightkit-onnx provision.rs select()): the approved or
 * pinned exact-(os, arch) rows win; only when none exists do the macOS `universal` rows (both Apple slices) serve an aarch64 or x86_64
 * target. A stale-unverified or planned row is never selected. With nothing serving, the typed error names the exact-arch row's status
 * when there is one, else the missing row. The onnxruntime alias stages one file, so it takes exactly one row.
 */
function selectNativeRows(loaded, entry, t, where) {
  const candidates = (loaded.catalog?.libraries ?? []).filter((row) => row.library === entry.library && row.version === entry.version && row.os === t.os);
  const serving = candidates.filter((row) => SERVING_STATUS.has(row.status));
  const exact = serving.filter((row) => row.arch === t.arch);
  const hits = exact.length ? exact : appleThin(t) ? serving.filter((row) => row.arch === "universal") : [];
  if (hits.length > 1 && entry.kind === "onnxruntime") return fail("STAGE_SIDECAR_NOT_APPROVED", `the native catalog has ${hits.length} ${where} rows`);
  if (hits.length) return hits;
  const unserved = candidates.filter((row) => row.arch === t.arch);
  if (unserved.length === 0) return fail("STAGE_SIDECAR_NOT_APPROVED", `the native catalog has no ${where} row`);
  if (unserved.length === 1) return fail("STAGE_SIDECAR_NOT_APPROVED", `the ${where} row ${unserved[0].id} is ${unserved[0].status}, not approved or pinned`);
  return fail("STAGE_SIDECAR_NOT_APPROVED", `the ${where} rows ${unserved.map((row) => `${row.id} (${row.status})`).join(", ")} are not approved or pinned`);
}

/** The rows a row's `companions` name (staged beside it): each must exist, serve the same os and arch, and be approved or pinned. */
function nativeCompanionRows(loaded, parent, t) {
  if (parent.companions === undefined) return [];
  if (!Array.isArray(parent.companions) || parent.companions.some((id) => typeof id !== "string" || !id)) return fail("STAGE_SIDECAR_NOT_APPROVED", `the row ${parent.id} lists companions that are not catalog row ids`);
  return parent.companions.map((id) => {
    const found = (loaded.catalog?.libraries ?? []).filter((row) => row.id === id);
    if (found.length !== 1) return fail("STAGE_SIDECAR_NOT_APPROVED", `the companion ${id} of ${parent.id} matches ${found.length} native catalog rows, expected exactly one`);
    const [row] = found;
    if (!rowServes(row, t)) return fail("STAGE_SIDECAR_NOT_APPROVED", `the companion ${id} of ${parent.id} is a ${row.os}/${row.arch} row, it does not serve ${t.os}/${t.arch}`);
    if (!SERVING_STATUS.has(row.status)) return fail("STAGE_SIDECAR_NOT_APPROVED", `the companion ${id} of ${parent.id} is ${row.status}, not approved or pinned`);
    if (Array.isArray(row.companions) && row.companions.length) return fail("STAGE_SIDECAR_NOT_APPROVED", `the companion ${id} of ${parent.id} lists companions of its own, which are not staged`);
    return row;
  });
}

/**
 * A catalog row that cannot be staged is refused before anything is downloaded or written. The onnxruntime alias checks the whole-file
 * sha256 only (its rows must carry one); a `native-library` row may pin sha256, contentSha256 or contentSha256ByArch (universal rows) and
 * needs at least one, so a row with no digest never serves. hostProof is only "never-run", and never on an approved row.
 */
function checkNativeRow(entry, row, where) {
  const bad = (message) => fail("STAGE_SIDECAR_NOT_APPROVED", `the ${where} row ${row.id} ${message}`);
  const native = entry.kind === "native-library";
  if (row.hostProof !== undefined && (row.hostProof !== "never-run" || row.status === "approved")) bad(`carries hostProof ${JSON.stringify(row.hostProof)}; only "never-run" is defined, and never on an approved row`);
  if (!native && row.sha256 === undefined) bad("has no sha256");
  if (row.artifactFileName === undefined) bad("has no artifactFileName");
  if (typeof row.artifactFileName !== "string" || !NAME.test(row.artifactFileName)) bad(`has an artifactFileName that is not a bare file name: ${JSON.stringify(row.artifactFileName)}`);
  const digest = (value, name) => { if (value !== undefined && !(typeof value === "string" && SHA256.test(value))) bad(`${name} is not a lowercase 64-hex SHA-256`); };
  digest(row.sha256, "sha256");
  if (native) {
    digest(row.contentSha256, "contentSha256");
    if (row.contentSha256ByArch !== undefined) {
      if (!isObject(row.contentSha256ByArch) || Object.keys(row.contentSha256ByArch).sort().join() !== SOURCE_ARCHES.join()) bad("contentSha256ByArch must be exactly { aarch64, x86_64 }");
      for (const arch of SOURCE_ARCHES) digest(row.contentSha256ByArch[arch], `contentSha256ByArch.${arch}`);
    }
    if (row.arch === "universal" && row.contentSha256 !== undefined) bad("is a universal row: it pins contentSha256ByArch, not contentSha256");
    if (row.arch !== "universal" && row.contentSha256ByArch !== undefined) bad("pins contentSha256ByArch, which only a universal row carries");
    if (row.sha256 === undefined && row.contentSha256 === undefined && row.contentSha256ByArch === undefined) bad("pins no digest (sha256, contentSha256 or contentSha256ByArch), so it never serves");
  }
  // A native-library entry may stage from source.dir, so an archive is optional there (and checked when present); the alias always downloads.
  if (row.archive !== undefined || !native) {
    const { archive } = row;
    if (!isObject(archive) || typeof archive.url !== "string" || !/^https:\/\/[^\s/]+\/\S*$/.test(archive.url) || typeof archive.sha256 !== "string" || !SHA256.test(archive.sha256)) bad("has no valid archive (an https url and a sha256 digest)");
    if (!archiveFileName(archive.url)) bad("has an archive url that names no file");
  }
}

/** The cached archive's file name: the last segment of the URL path ("" when it has none). */
function archiveFileName(url) {
  try { return path.posix.basename(new URL(url).pathname); } catch { return ""; }
}

/**
 * The archive member of a catalog row: archive.member when present; else, for a macOS `.zip` archive, `<archive name without
 * .zip>/lib/<artifactFileName>` (as provision.rs derives it); else the bare artifactFileName.
 */
function nativeArchiveMember(row) {
  if (row.archive.member !== undefined) return row.archive.member;
  const archiveName = archiveFileName(row.archive.url);
  if (row.os === "macos" && archiveName.endsWith(".zip")) return `${archiveName.slice(0, -".zip".length)}/lib/${row.artifactFileName}`;
  return row.artifactFileName;
}

/**
 * Plan one target's files without any effect: the rows to stage (every serving row of the library, or the `files` named, each exactly
 * once), then the companion rows of each. Returns [{ row, outName, where, companionOf? }]; every row has passed checkNativeRow.
 * `as` renames the one file the library stages (a companion keeps its catalog name: it is loaded by that name).
 */
function planNativeSet(entry, ctx, loaded, t) {
  const where = `${entry.library} ${entry.version} ${t.os}/${t.arch}`;
  const hits = selectNativeRows(loaded, entry, t, where);
  let primary = hits;
  if (!entry.files) {
    // Every serving row is staged; a row of the same library and arch that does not serve is left out, and said so (an incomplete set is not silent).
    for (const row of loaded.catalog?.libraries ?? []) {
      if (row.library === entry.library && row.version === entry.version && row.os === t.os && row.arch === t.arch && !SERVING_STATUS.has(row.status)) ctx.warn(`stage-sidecar: not staging ${row.id} (${row.status}); only approved or pinned rows are staged`);
    }
  } else {
    primary = entry.files.map((name) => {
      const named = hits.filter((row) => row.artifactFileName === name);
      if (named.length === 1) return named[0];
      if (named.length > 1) return fail("STAGE_SIDECAR_NOT_APPROVED", `the native catalog has ${named.length} ${where} rows for ${name}`);
      const unserved = (loaded.catalog?.libraries ?? []).find((row) => row.library === entry.library && row.version === entry.version && row.os === t.os && row.arch === t.arch && row.artifactFileName === name);
      return fail("STAGE_SIDECAR_NOT_APPROVED", unserved ? `the ${where} row ${unserved.id} for ${name} is ${unserved.status}, not approved or pinned` : `the native catalog has no ${where} row for ${name}`);
    });
  }
  for (const row of primary) {
    const same = primary.filter((other) => (other.artifactFileName ?? other.id) === (row.artifactFileName ?? row.id));
    if (same.length > 1) return fail("STAGE_SIDECAR_NOT_APPROVED", `the native catalog has ${same.length} ${where} rows for ${row.artifactFileName}`);
  }
  if (entry.as && primary.length > 1) fail("STAGE_SIDECAR_CONFIG_INVALID", `as ${JSON.stringify(entry.as)} renames one file, but the ${where} rows stage ${primary.length} (${primary.map((row) => row.artifactFileName).join(", ")}); drop as, or name one file in files`);
  const items = primary.map((row) => ({ row, outName: entry.as ?? row.artifactFileName, where }));
  if (entry.companions) {
    for (const parent of primary) {
      for (const row of nativeCompanionRows(loaded, parent, t)) {
        if (!items.some((item) => item.row.id === row.id)) items.push({ row, outName: row.artifactFileName, where: `${row.library} ${row.version} ${t.os}/${t.arch}`, companionOf: parent.id });
      }
    }
  }
  for (const item of items) checkNativeRow(entry, item.row, item.where);
  for (const item of items) {
    const clash = items.find((other) => other !== item && other.outName.toLowerCase() === item.outName.toLowerCase());
    if (clash) fail("STAGE_SIDECAR_NOT_APPROVED", `the ${where} rows ${item.row.id} and ${clash.row.id} both stage ${item.outName}`);
  }
  return items;
}

/**
 * The digest `bytes` must match for `row`, or the first disagreement { what, expected, actual }. `fromSource` is a copy read from
 * source.dir (not byte-identical to the pinned archive by construction, and possibly re-signed): it is held to the content digest when
 * the row pins one, else to the whole-file sha256. A file extracted from the pinned archive must equal the whole-file sha256 the row
 * has AND its content digest. The onnxruntime alias keeps the whole-file check only. A malformed recognized format is a mismatch.
 */
function nativeDigestMismatch(entry, row, bytes, { fromSource }) {
  const checkContent = entry.kind === "native-library" && (row.contentSha256 !== undefined || row.contentSha256ByArch !== undefined);
  if (row.sha256 !== undefined && !(fromSource && checkContent)) {
    const actual = sha256Of(bytes);
    if (actual !== row.sha256) return { what: "sha256", expected: row.sha256, actual };
  }
  if (!checkContent) return undefined;
  try {
    if (row.contentSha256ByArch) {
      const identity = contentIdentity(bytes);
      for (const arch of SOURCE_ARCHES) {
        const actual = digestFor(identity, arch);
        if (actual !== row.contentSha256ByArch[arch]) return { what: `${arch} content digest`, expected: row.contentSha256ByArch[arch], actual };
      }
      return undefined;
    }
    // A thin row pins one slice of one thin file: a universal Mach-O (a good slice beside a foreign one) is not that file.
    const identity = contentIdentity(bytes);
    if (identity.format === "macho-universal" || identity.slices.length !== 1) {
      return { what: "file identity", expected: `a thin ${row.arch} file with content digest ${row.contentSha256}`, actual: `a ${identity.format === "macho-universal" ? "universal Mach-O" : "file"} with ${identity.slices.length} slices (${identity.slices.map((slice) => slice.arch ?? "none").join(", ")}); a thin row accepts exactly one slice` };
    }
    const actual = digestFor(identity, row.arch) ?? identity.slices[0].sha256;
    return actual === row.contentSha256 ? undefined : { what: "content digest", expected: row.contentSha256, actual };
  } catch (error) {
    if (!(error instanceof ContentDigestError)) throw error;
    return { what: "content digest", expected: row.contentSha256 ?? SOURCE_ARCHES.map((arch) => row.contentSha256ByArch[arch]).join("+"), reason: error.message };
  }
}

const failNativeMismatch = (row, label, mismatch) => fail("STAGE_SIDECAR_HASH_MISMATCH",
  `${label} has ${mismatch.what} ${mismatch.actual ?? `unreadable (${mismatch.reason ?? "no matching slice"})`}, the catalog row ${row.id} pins ${mismatch.expected}; nothing staged`, { expected: mismatch.expected, actual: mismatch.actual ?? null });

/**
 * Verify one planned file and find its bytes; nothing is written except the verified archive cache. A file already staged is kept only
 * when it passes the same digest check as a fresh one (never trusted because it exists). Otherwise the bytes come from `source.dir`
 * (read and checked against the row before anything is copied), or from the row's pinned archive (digest checked before use, one member
 * unpacked into a scratch directory the caller removes). Returns the item with its notice copies, destination and `from` (undefined when already staged).
 */
async function prepareNativeFile(entry, ctx, loaded, item, { destDir, scratchDirs }) {
  const { appRoot, fs } = ctx;
  const { row, outName, where } = item;
  if (!Array.isArray(row.noticeFiles) || !row.noticeFiles.length) fail("STAGE_SIDECAR_NOTICE_MISSING", `the ${where} row ${row.id} lists no notice files; the licence must ship with the library`);
  const notices = row.noticeFiles.map((file) => {
    const normalized = String(file).replaceAll("\\", "/");
    if (path.isAbsolute(file) || normalized.split("/").includes("..")) fail("STAGE_SIDECAR_NOTICE_MISSING", `notice file path escapes the catalog crate: ${file}`);
    return { source: path.join(loaded.crateDir, ...normalized.split("/")), dest: path.resolve(appRoot, entry.licenseDir, path.posix.basename(normalized)) };
  });
  for (const { source } of notices) if (!fs.existsSync(source)) fail("STAGE_SIDECAR_NOTICE_MISSING", `notice file not found: ${source}`);

  // The archive member is the catalog's archive.member (an archive path) when present, else the library file name; the output name never comes from it.
  let member;
  if (!entry.source && row.archive) {
    member = nativeArchiveMember(row);
    const memberCheck = normalizeArchiveMember(member);
    if (memberCheck.error) fail("STAGE_SIDECAR_NOT_APPROVED", `the ${where} row ${row.id} has an invalid archive member ${JSON.stringify(member)}: ${memberCheck.error}`);
  }
  const dest = path.resolve(appRoot, destDir ?? entry.resourceDir, outName);
  const prepared = { ...item, dest, notices };
  if (fs.existsSync(dest) && !nativeDigestMismatch(entry, row, fs.readFileSync(dest), { fromSource: Boolean(entry.source) })) return prepared;

  if (entry.source) {
    const file = requireSnapshotSource(ctx, path.resolve(appRoot, entry.source.dir, row.artifactFileName), `source.dir of the ${where} row ${row.id}`);
    if (!(fs.existsSync(file) && fs.statSync(file).isFile())) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `the ${where} row ${row.id} is staged from source.dir ${entry.source.dir}, which has no ${row.artifactFileName} (looked at ${file}); nothing is downloaded for a source dir`, { file });
    const mismatch = nativeDigestMismatch(entry, row, fs.readFileSync(file), { fromSource: true });
    if (mismatch) failNativeMismatch(row, file, mismatch);
    return { ...prepared, from: file };
  }
  if (!row.archive) fail("STAGE_SIDECAR_CONFIG_INVALID", `the ${where} row ${row.id} has no archive, so it is staged from a local build: set source: { dir } on this entry to the directory holding ${row.artifactFileName}`);
  const archive = path.join(cacheDirFor(ctx, entry.cacheDir), archiveFileName(row.archive.url));
  if (!(fs.existsSync(archive) && sha256Of(fs.readFileSync(archive)) === row.archive.sha256)) {
    const bytes = await ctx.fetchBytes(row.archive.url);
    const actual = sha256Of(bytes);
    if (actual !== row.archive.sha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${row.archive.url} downloaded as ${actual}, the catalog pins ${row.archive.sha256}; nothing staged`, { expected: row.archive.sha256, actual });
    fs.mkdirSync(path.dirname(archive), { recursive: true });
    fs.writeFileSync(archive, bytes);
  }
  const scratch = ctx.makeTempDir();
  scratchDirs.push(scratch);
  const library = await ctx.extractMember({ archive, fileName: member, destDir: scratch });
  const mismatch = nativeDigestMismatch(entry, row, fs.readFileSync(library), { fromSource: false });
  if (mismatch) failNativeMismatch(row, member, mismatch);
  return { ...prepared, from: library };
}

/** Write one verified file (unless it is already staged) and its notices; the staged bytes are re-verified against the row and re-read for the receipt. */
function commitNativeFile(entry, ctx, prepared) {
  const { fs } = ctx;
  const { row, dest, from } = prepared;
  if (from) {
    copyInto(ctx, from, dest, { executable: false });
    // The source was read again for the copy: hold the staged bytes to the row too, so a file that changed after it was verified is never left staged.
    const mismatch = nativeDigestMismatch(entry, row, fs.readFileSync(dest), { fromSource: Boolean(entry.source) });
    if (mismatch) {
      fs.rmSync(dest, { force: true });
      failNativeMismatch(row, `${rel(ctx, dest)} (copied from ${from}, which changed after it was verified)`, mismatch);
    }
  } else ctx.log(`stage-sidecar: ${rel(ctx, dest)} already staged`);
  if (row.hostProof === "never-run") ctx.warn(`stage-sidecar: warning: ${row.id} has hostProof never-run: its bytes were pinned but never loaded or run on a ${row.os} host, so ${path.basename(dest)} is staged but not host-proven`);
  const staged = fs.readFileSync(dest);
  const notices = prepared.notices.map(({ source, dest: out }) => {
    fs.mkdirSync(path.dirname(out), { recursive: true });
    fs.copyFileSync(source, out);
    return { name: path.basename(out), path: rel(ctx, out), sha256: sha256Of(fs.readFileSync(out)) };
  });
  return { ...prepared, sha256: sha256Of(staged), sizeBytes: staged.length, notices };
}

/**
 * Stage one planned set (one target or one universal slice) in two phases: every file is verified first (`beforeWrite` may veto a
 * verified file), then everything is written, so a bad file leaves nothing of the set behind. Scratch directories are always removed.
 */
async function stageNativeSet(entry, ctx, loaded, items, { destDir, beforeWrite } = {}) {
  const scratchDirs = [];
  try {
    const prepared = [];
    for (const item of items) prepared.push(await prepareNativeFile(entry, ctx, loaded, item, { destDir, scratchDirs }));
    if (beforeWrite) for (const file of prepared) beforeWrite(file);
    return prepared.map((file) => commitNativeFile(entry, ctx, file));
  } finally {
    for (const dir of scratchDirs) ctx.removeDir(dir);
  }
}

/** Is this file's receipt row evidence from the pinned archive (not source.dir)? */
const fromArchive = (entry, row) => !entry.source && row.archive !== undefined;

/** The digests a `native-library` row pins, for its receipt row. */
const pinnedContent = (row) => ({ ...(row.contentSha256 ? { contentSha256: row.contentSha256 } : {}), ...(row.contentSha256ByArch ? { contentSha256ByArch: { ...row.contentSha256ByArch } } : {}) });

/** The receipt row of one staged file (a thin target, or a universal row staged once). The alias keeps its original row shape. */
function nativeReceiptRow(entry, ctx, file, extra = {}) {
  const { row } = file;
  const alias = entry.kind === "onnxruntime";
  return {
    kind: entry.kind, ...(alias ? {} : { library: row.library }), name: file.outName, path: rel(ctx, file.dest), sha256: file.sha256, sizeBytes: file.sizeBytes, changed: null,
    catalogRow: row.id, version: row.version, ...(fromArchive(entry, row) ? { archiveSha256: row.archive.sha256 } : {}), ...(alias ? {} : pinnedContent(row)),
    license: row.licenseSpdx ?? null, notices: file.notices, externalBin: false,
    ...(row.hostProof ? { hostProof: row.hostProof } : {}), ...(file.companionOf ? { companionOf: file.companionOf } : {}), ...extra,
  };
}

/** The native catalog of an entry, loaded once per run (the claim pre-pass and the staging share it). */
async function loadNativeCatalogOf(entry, ctx) {
  if (ctx.nativeCatalogs.has(entry)) return ctx.nativeCatalogs.get(entry);
  const { appRoot, fs } = ctx;
  const loaded = await (ctx.loadNativeCatalog({ entry, appRoot }) ?? (entry.catalogSpec ? readJsonCatalog(resolveCatalogFile(entry.catalogSpec, ctx, entry.kind), appRoot, fs, "native-libraries") : undefined));
  if (!loaded) fail("STAGE_SIDECAR_CATALOG_UNREADABLE", `${entry.kind} needs a catalog path (the rightkit-ffmpeg native-libraries.json) or deps.loadNativeCatalog`);
  ctx.nativeCatalogs.set(entry, loaded);
  return loaded;
}

/**
 * Validation claims only the file names a config states, but a native-library or onnxruntime entry writes what its catalog rows name (and
 * the alias stages the DirectML companion beside the library by default), so another entry can write the same file. Before anything is
 * written, plan each such entry without effects (a catalog or row problem is skipped here and raised in order by the staging itself), claim
 * the planned file names beside the validation claims, and refuse a file two entries write, naming both and the remedy.
 */
async function claimNativeOutputs(plan, ctx) {
  const natives = plan.entries.filter((entry) => !entry.skipped && (entry.kind === "onnxruntime" || entry.kind === "native-library"));
  if (!natives.length) return;
  const claimed = [...plan.claims];
  const quiet = { ...ctx, log: () => {}, warn: () => {} };
  const errors = [];
  for (const entry of natives) {
    const label = `sidecars[${entry.index}] (${entry.kind})`;
    let items;
    try {
      const loaded = await loadNativeCatalogOf(entry, ctx);
      items = (ctx.target.arch === "universal" ? SLICES.map((triple) => parseTargetTriple(triple)) : [ctx.target]).flatMap((t) => planNativeSet(entry, quiet, loaded, t));
    } catch { continue; }
    for (const item of items) {
      const output = `${entry.resourceDir}/${item.outName}`;
      const key = canonicalOutput(output);
      const clash = findClaimClash(claimed, key);
      if (!clash) { claimed.push({ key, output, label, ...(item.companionOf ? { companion: true } : {}) }); continue; }
      if (clash.label === label) continue;
      // The alias and native-library stage companions by default: whichever side is one is told how to stand down.
      const companionLabel = item.companionOf ? label : clash.companion ? clash.label : undefined;
      const remedy = companionLabel ? `; drop the other entry's copy, or set companions: false on ${companionLabel} to stage the library alone` : "";
      const what = item.companionOf ? `the ${item.row.library} companion ${item.outName} of ${item.companionOf}` : `${item.outName} (catalog row ${item.row.id})`;
      errors.push(`${label} stages ${what} into ${entry.resourceDir}, which ${clash.label} also writes (${clash.output})${remedy}`);
    }
  }
  if (errors.length) fail("STAGE_SIDECAR_CONFIG_INVALID", `invalid sidecars configuration:\n${[...new Set(errors)].map((line) => `- ${line}`).join("\n")}`, { errors });
}

async function stageNativeLibrary(entry, ctx) {
  const { appRoot, fs, target } = ctx;
  const loaded = await loadNativeCatalogOf(entry, ctx);
  const rowOf = (file, extra) => nativeReceiptRow(entry, ctx, file, extra);
  if (target.arch !== "universal") {
    const staged = await stageNativeSet(entry, ctx, loaded, planNativeSet(entry, ctx, loaded, target));
    return staged.map((file) => rowOf(file));
  }
  const plans = SLICES.map((triple) => ({ triple, items: planNativeSet(entry, ctx, loaded, parseTargetTriple(triple)) }));
  const rows = plans.flatMap((plan) => plan.items.map((item) => item.row));
  // universal-apple-darwin: `universal` rows serving both slices are staged once, in place, with no lipo merge (nothing mixes with exact-arch rows).
  if (rows.some((row) => row.arch === "universal")) {
    if (!rows.every((row) => row.arch === "universal") || plans[0].items.map((item) => item.row.id).join() !== plans[1].items.map((item) => item.row.id).join()) {
      fail("STAGE_SIDECAR_NOT_APPROVED", `the native catalog mixes a universal row with exact-arch rows (${[...new Set(rows.map((row) => row.id))].join(", ")}); universal-apple-darwin takes either one universal row or one row per arch`);
    }
    const label = (file) => `${file.row.library} ${file.row.version} ${file.row.id}`;
    const staged = await stageNativeSet(entry, ctx, loaded, plans[0].items, { beforeWrite: (file) => requireUniversalFile(ctx, file.from ?? file.dest, label(file)) });
    return staged.map((file) => rowOf(file, {
      sourceSha256: file.sha256,
      slices: SLICES.map((triple, index) => ({ triple, sha256: file.sha256, catalogRow: file.row.id, ...(fromArchive(entry, file.row) ? { archiveSha256: file.row.archive.sha256 } : {}), ...(entry.kind === "native-library" && file.row.contentSha256ByArch ? { contentSha256: file.row.contentSha256ByArch[SOURCE_ARCHES[index]] } : {}) })),
    }));
  }
  // Otherwise each arch's files are staged thin under SLICE_ROOT, then merged file by file into the entry's resource directory.
  const slices = [];
  for (const plan of plans) slices.push({ triple: plan.triple, files: await stageNativeSet(entry, ctx, loaded, plan.items, { destDir: sliceDirFor(ctx, plan.triple) }) });
  const [first, second] = slices.map((slice) => slice.files.map((file) => file.outName));
  if ([...first].sort().join("\n") !== [...second].sort().join("\n")) fail("STAGE_SIDECAR_NOT_APPROVED", `the arch rows name different library files (aarch64: ${first.join(", ")}; x86_64: ${second.join(", ")}); each file needs one name on both arches (a single file can be renamed with "as")`);
  return first.map((name) => {
    const parts = slices.map((slice) => ({ triple: slice.triple, file: slice.files.find((file) => file.outName === name) }));
    const dest = path.resolve(appRoot, entry.resourceDir, name);
    const merged = lipoMergeInto(ctx, parts.map(({ file }) => file.dest), dest, `${parts[0].file.row.library} ${parts[0].file.row.version}`);
    const hostProof = parts.find(({ file }) => file.row.hostProof)?.file.row.hostProof;
    const alias = entry.kind === "onnxruntime";
    return {
      kind: entry.kind, ...(alias ? {} : { library: parts[0].file.row.library }), name, path: rel(ctx, dest), ...merged, sourceSha256: merged.sha256, catalogRow: parts.map(({ file }) => file.row.id).join("+"), version: parts[0].file.row.version,
      license: parts[0].file.row.licenseSpdx ?? null, notices: [...new Map(parts.flatMap(({ file }) => file.notices).map((notice) => [notice.path, notice])).values()], externalBin: false,
      ...(hostProof ? { hostProof } : {}), ...(parts[0].file.companionOf ? { companionOf: parts.map(({ file }) => file.companionOf).join("+") } : {}),
      slices: parts.map(({ triple, file }) => ({ triple, sha256: file.sha256, catalogRow: file.row.id, ...(fromArchive(entry, file.row) ? { archiveSha256: file.row.archive.sha256 } : {}), ...(!alias && file.row.contentSha256 ? { contentSha256: file.row.contentSha256 } : {}) })),
    };
  });
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
  const built = runStep(ctx, entry.cmd, entry.args, { cwd, env: splitCommandEnv(ctx) });
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
  const { fs } = ctx;
  const cached = path.resolve(ctx.cacheRoot, REMOTE_CACHE_DIR, sha256);
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
 * (zip, tgz): the one member whose base name is `extract` (or, when `extract` contains `/`, whose archive path equals it) is unpacked (digest-checked against `memberSha256` when given) and
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
      source = entry.extractKind === "nsis"
        ? await ctx.extractNsisMember({ archive: cached, fileName: entry.extract, destDir: scratch, expectedSha256: entry.memberSha256, installerSha256: entry.sha256 })
        : await ctx.extractMember({ archive: cached, fileName: entry.extract, destDir: scratch });
      memberSha256 = sha256Of(fs.readFileSync(source));
      if (entry.memberSha256 && memberSha256 !== entry.memberSha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${entry.extract} from ${entry.url} is ${memberSha256}, expected ${entry.memberSha256}; nothing staged`, { expected: entry.memberSha256, actual: memberSha256 });
    }
    // Download and verify first: a failed fetch or a hash mismatch must never leave a cleaned destination behind.
    cleanDest(ctx, entry, dest);
    const copied = copyInto(ctx, source, dest, { executable: entry.executable === true });
    const expected = entry.extract ? entry.memberSha256 : entry.sha256;
    if (expected && copied.sha256 !== expected) fail("STAGE_SIDECAR_HASH_MISMATCH", `staged ${entry.dest} is ${copied.sha256}, expected ${expected}`, { expected, actual: copied.sha256 });
    return [{ kind: "remote", name: entry.name, path: rel(ctx, dest), ...copied, sourceSha256: memberSha256 ?? entry.sha256, url: entry.url, cached: fromCache, externalBin: false, ...(entry.extract ? { archiveSha256: entry.sha256, member: entry.extract, memberSha256 } : {}), ...(entry.extractKind ? { extractKind: entry.extractKind } : {}) }];
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

/** Fixed Visual Studio Installer (vswhere) location under %ProgramFiles(x86)%, and the cmake Visual Studio bundles under an installationPath. */
const VSWHERE_PARTS = ["Microsoft Visual Studio", "Installer", "vswhere.exe"];
const VS_CMAKE_PARTS = ["Common7", "IDE", "CommonExtensions", "Microsoft", "CMake", "CMake", "bin", "cmake.exe"];

/**
 * Which cmake to run. Order: the entry's `cmakeBin` (a bare name is looked up on PATH; a path with separators is resolved against the
 * app root); off a Windows host plain `cmake`; on a Windows host `cmake` when `where cmake` finds one, else the Visual Studio-bundled
 * cmake that vswhere locates (installationPath\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe), else plain `cmake`
 * (which then fails with the usual spawn error naming it).
 */
function resolveCmakeBinary(ctx, entry) {
  if (entry.cmakeBin) return /[\\/]/.test(entry.cmakeBin) ? path.resolve(ctx.appRoot, entry.cmakeBin) : entry.cmakeBin;
  if (ctx.host.os !== "windows") return "cmake";
  const probe = ctx.run("where", ["cmake"], { cwd: ctx.appRoot, env: ctx.env });
  if (!probe?.error && probe?.status === 0 && String(probe.stdout ?? "").trim()) return "cmake";
  const programFiles = ctx.env["ProgramFiles(x86)"];
  if (typeof programFiles !== "string" || !programFiles) return "cmake";
  const vswhere = path.win32.join(programFiles, ...VSWHERE_PARTS);
  if (!ctx.fs.existsSync(vswhere)) return "cmake";
  const found = ctx.run(vswhere, ["-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.CMake.Project", "-property", "installationPath"], { cwd: ctx.appRoot, env: ctx.env });
  const installation = found?.error || found?.status !== 0 ? "" : String(found.stdout ?? "").split(/\r?\n/).map((line) => line.trim()).find(Boolean) ?? "";
  if (!installation) return "cmake";
  const bundled = path.win32.join(installation, ...VS_CMAKE_PARTS);
  return ctx.fs.existsSync(bundled) ? bundled : "cmake";
}

/**
 * Build a pinned git commit with the build tool (no shell) and stage the result as <name>-<triple>[.exe]: fetch the commit, configure,
 * build `target`, locate the executable, check it carries both archs for universal-apple-darwin, optionally pin its digest and
 * smoke-run it. Per-platform flags (for example Metal on macOS) come from `platformArgs`; the source and build trees live at the
 * fixed `.right-release/cmake/<name>/{src,build}`.
 */
function stageCmake(entry, ctx) {
  const { appRoot, fs, run, env, target } = ctx;
  const cmake = resolveCmakeBinary(ctx, entry);
  const generator = entry.platformGenerators?.[target.platform] ?? entry.generator;
  const root = path.resolve(ctx.cacheRoot, CMAKE_ROOT, entry.name);
  const srcDir = path.join(root, "src");
  const buildDir = path.join(root, "build");
  const dest = destFor(entry, ctx);
  cleanDest(ctx, entry, dest);
  checkoutPinnedSource(ctx, entry, srcDir);
  const archFlag = target.platform !== "mac" ? undefined
    : target.arch === "universal" ? "-DCMAKE_OSX_ARCHITECTURES=arm64;x86_64"
      : `-DCMAKE_OSX_ARCHITECTURES=${target.arch === "aarch64" ? "arm64" : "x86_64"}`;
  const configure = ["-S", srcDir, "-B", buildDir, "-DCMAKE_BUILD_TYPE=Release", ...(generator ? ["-G", generator] : []), ...(entry.architecture ? ["-A", entry.architecture] : []), ...(archFlag ? [archFlag] : []), ...(entry.platformArgs[target.platform] ?? []), ...entry.cmakeArgs];
  const step = (args, label) => {
    ctx.log(`stage-sidecar: ${cmake} ${args.join(" ")}`);
    const result = runStep(ctx, cmake, args, { cwd: appRoot, env });
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
  return [{ kind: "cmake", name: entry.name, path: rel(ctx, dest), ...copied, sourceSha256, repo: entry.repo, commit: entry.commit, cmakeTarget: entry.target, ...(cmake !== "cmake" ? { cmakeBin: cmake } : {}), ...(generator ? { generator } : {}), flags: configure.filter((arg) => arg.startsWith("-D")), externalBin: !entry.dest }];
}

/** `{platform}` in a prebuilt manifest entry: macos-aarch64, macos-x86_64, windows-x86_64, linux-x86_64, ... */
const prebuiltPlatformKey = (t) => `${t.os}-${t.arch}`;

/**
 * Verify one host-built binary `source` for `slice` (a parsed target: its os and arch name the manifest entry) against the digest in
 * an app manifest file: its raw sha256, and when the manifest names an approval hash, the signing-independent approval digest
 * (`macho-codesign-strip-linkedit-v1` on macOS, `raw-sha256` elsewhere). Nothing is built or downloaded: a missing file or
 * a mismatch fails closed. Returns { platformKey, actual, approval? }.
 */
async function verifyPrebuilt(entry, ctx, source, slice) {
  const { appRoot, fs } = ctx;
  const platformKey = prebuiltPlatformKey(slice);
  const manifestFile = path.resolve(appRoot, entry.manifest.file);
  let manifest;
  try { manifest = JSON.parse(String(fs.readFileSync(manifestFile))); } catch (error) { fail("STAGE_SIDECAR_CATALOG_UNREADABLE", `cannot read the prebuilt manifest ${manifestFile} as JSON`, { cause: error }); }
  let node = manifest;
  for (const part of entry.manifest.entry.replaceAll("{platform}", platformKey).split(".")) node = isObject(node) ? node[part] : undefined;
  if (!isObject(node)) fail("STAGE_SIDECAR_NOT_APPROVED", `${entry.manifest.file} has no ${entry.manifest.entry.replaceAll("{platform}", platformKey)} entry for ${entry.name} on ${platformKey}`);
  const pinned = node[entry.manifest.sha256];
  if (typeof pinned !== "string" || !SHA256.test(pinned)) fail("STAGE_SIDECAR_NOT_APPROVED", `${entry.manifest.file} ${platformKey}.${entry.manifest.sha256} is not a lowercase 64-hex SHA-256`);
  if (!fs.existsSync(source) || !fs.statSync(source).isFile()) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `prebuilt ${entry.name} for ${platformKey} is missing: ${source}. Build it on its native host first; packaging never substitutes a stock binary.`, { name: entry.name, source });
  const actual = sha256Of(fs.readFileSync(source));
  if (actual !== pinned) fail("STAGE_SIDECAR_HASH_MISMATCH", `${entry.name} integrity failure: ${source} is ${actual}, ${entry.manifest.file} pins ${pinned}`, { expected: pinned, actual });
  let approval;
  if (entry.manifest.approvalSha256) {
    const expected = node[entry.manifest.approvalSha256];
    if (typeof expected !== "string" || !SHA256.test(expected)) fail("STAGE_SIDECAR_NOT_APPROVED", `${entry.manifest.file} ${platformKey}.${entry.manifest.approvalSha256} is not a lowercase 64-hex SHA-256`);
    const algorithm = entry.manifest.approvalAlgorithm ? node[entry.manifest.approvalAlgorithm] : (slice.os === "macos" ? APPROVAL_MACHO_STRIP_LINKEDIT_V1 : APPROVAL_RAW_SHA256);
    if (typeof algorithm !== "string") fail("STAGE_SIDECAR_NOT_APPROVED", `${entry.manifest.file} ${platformKey}.${entry.manifest.approvalAlgorithm} does not name an approval hash algorithm`);
    let digest;
    try { digest = await ctx.approvalDigest(source, algorithm); } catch (error) { fail("STAGE_SIDECAR_APPROVAL_FAILED", `${entry.name} approval hash (${algorithm}): ${error.message}`, { cause: error, causeCode: error.code }); }
    if (digest !== expected) fail("STAGE_SIDECAR_HASH_MISMATCH", `${entry.name} signing-independent integrity failure: ${algorithm} is ${digest}, ${entry.manifest.file} pins ${expected}`, { expected, actual: digest });
    approval = { algorithm, sha256: digest };
  }
  return { platformKey, actual, ...(approval ? { approval } : {}) };
}

/**
 * Verify a host-built binary (already staged at <stageDir>/<name>-<triple>, or copied from `from`) against the app manifest and stage it.
 * `from` is a path, or `{ aarch64, x86_64 }` (the entry for this arch is used). `universal-apple-darwin`: each slice (its `from.<arch>`, else the
 * staged `<stageDir>/<name>-<slice triple>`) is verified against its own manifest entry (macos-aarch64, macos-x86_64), then the two are
 * lipo-merged by the same helper the ffmpeg, onnxruntime and cmake kinds use into `<name>-universal-apple-darwin`; a thin merge result is refused.
 */
async function stagePrebuilt(entry, ctx) {
  const { fs, target, appRoot } = ctx;
  const dest = destFor(entry, ctx);
  if (target.arch === "universal") {
    const parts = [];
    for (const triple of SLICES) {
      const slice = parseTargetTriple(triple);
      const from = entry.from?.[slice.arch];
      const source = from ? path.resolve(appRoot, from) : path.resolve(appRoot, ctx.stageDir, stagedName(entry.name, slice));
      parts.push({ triple, source, ...(await verifyPrebuilt(entry, ctx, source, slice)) });
    }
    const merged = lipoMergeInto(ctx, parts.map((part) => part.source), dest, `prebuilt ${entry.name}`, { executable: true });
    return [{ kind: "prebuilt", name: entry.name, path: rel(ctx, dest), ...merged, sourceSha256: merged.sha256, manifest: entry.manifest.file, platform: "macos-universal", externalBin: !entry.dest, slices: parts.map((part) => ({ triple: part.triple, sha256: part.actual, platform: part.platformKey, ...(part.approval ? { approvalAlgorithm: part.approval.algorithm, approvalSha256: part.approval.sha256 } : {}) })) }];
  }
  const declared = isObject(entry.from) ? entry.from[target.arch] : entry.from;
  if (isObject(entry.from) && !declared) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `prebuilt ${entry.name} declares from for ${Object.keys(entry.from).join(", ")}, not ${target.arch}`, { name: entry.name });
  const source = declared ? path.resolve(appRoot, declared) : dest;
  const { platformKey, actual, approval } = await verifyPrebuilt(entry, ctx, source, target);
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
 * or now placed at `dest` or an ffmpeg `resourceDir`. Never recursive; other triples, notices, directories, temporaries and unrelated files
 * are left alone, and it only ever lists stageDir, so a `resourceDir` file is never a candidate.
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
export function isNestedBrokerContext(env = process.env, platform = process.platform) {
  // A direct (Linux) host has no broker, so no outer broker build to nest in, whatever flag or socket the env carries (heavy-command sets
  // RIGHTSUITE_HEAVY_WORK_OWNER on every platform).
  if (directPlatform(platform)) return false;
  if (env?.RIGHTKIT_STAGE_SIDECAR_ALLOW_NESTED === "1") return false;
  const insideTauri = Boolean(env?.TAURI_ENV_PLATFORM || env?.TAURI_ENV_TARGET_TRIPLE);
  return insideTauri && (managedHost(env ?? {}, platform) || env?.RIGHTSUITE_HEAVY_WORK_OWNER === "1");
}

/** True when staging would call `rustright cargo` for a non-skipped entry. */
const needsBroker = (plan) => plan.entries.some((entry) => !entry.skipped && (entry.kind === "cargo-bin" || entry.catalogSpec?.via === "crate"));

function assertNotNested(plan, env) {
  if (needsBroker(plan) && isNestedBrokerContext(env)) {
    fail(NESTED_BROKER_CODE, "stage-sidecar needs `rustright cargo`, but it is running inside an outer broker build (a Tauri beforeBuildCommand/beforeDevCommand on a managed host), where a nested broker call exits 125. Run stage-sidecar in the recipe `prepare` step, before the Tauri build (or call it from your own script before `tauri build`), not from beforeBuildCommand.");
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
    // Without a catalog at check time: the files named, else the one name `as` gives, else what the catalog row will say.
    if (entry.kind === "onnxruntime" || entry.kind === "native-library") return (entry.files && !entry.as ? entry.files : [entry.as ?? "<catalog artifactFileName>"]).map((name) => `${entry.resourceDir}/${name}`);
    return (entry.media?.tools ?? []).map((tool) => ffmpegOutput(entry, plan.stageDir, tool, t));
  };
  const entries = plan.entries.map((entry) => ({
    index: entry.index, kind: entry.kind, skipped: entry.skipped, ...(entry.platforms ? { platforms: entry.platforms } : {}),
    ...(entry.skipped ? {} : { outputs: outputsOf(entry), externalBin: ["cargo-bin", "command", "cmake", "prebuilt"].includes(entry.kind) ? !entry.dest : entry.kind === "ffmpeg" && !entry.resourceDir }),
    ...(entry.kind === "cargo-bin" ? { package: entry.package, bin: entry.bin, profile: entry.profile } : {}),
    ...(entry.kind === "remote" ? { url: entry.url } : {}),
    ...(entry.kind === "native-library" ? { library: entry.library, version: entry.version } : {}),
    ...(entry.kind === "cmake" ? { repo: entry.repo, commit: entry.commit } : {}),
    ...(entry.clean ? { clean: true } : {}),
  }));
  return { target: t.triple, platform: t.platform, mode: resolveBuildMode(env), stageDir: plan.stageDir, receiptPath: plan.receiptPath, wipeStageDir: plan.wipeStageDir, nestedBrokerRisk: needsBroker(plan) && isNestedBrokerContext(env), entries };
}

/**
 * Stage every configured sidecar for `target` (a Rust triple) and write the staging receipt.
 * Returns { receipt, receiptPath, files, externalBinNames }. `deps` (all optional) injects every effect:
 * fs (existsSync, mkdirSync, readFileSync, writeFileSync, copyFileSync, renameSync, chmodSync, statSync, readdirSync, rmSync), run, fetch (https downloads of `remote`), fetchBytes, extractMember, loadMediaCatalog, loadNativeCatalog, makeTempDir, removeDir, now, pid, log (progress), warn (warnings; defaults to log), env.
 * The API is quiet by default; the `right-release stage-sidecar` command passes `warn` so the CLI prints warnings on stderr.
 */
export async function stageSidecars({ appRoot, manifest, target, cacheRoot, deps = {} } = {}) {
  if (typeof appRoot !== "string" || !path.isAbsolute(appRoot)) fail("STAGE_SIDECAR_CONFIG_INVALID", "appRoot must be an absolute path");
  if (cacheRoot !== undefined && (typeof cacheRoot !== "string" || !path.isAbsolute(cacheRoot))) fail("STAGE_SIDECAR_CONFIG_INVALID", "cacheRoot must be an absolute path");
  const env = deps.env ?? process.env;
  const plan = validateSidecarManifest(manifest, { target, mode: resolveBuildMode(env) });
  assertNotNested(plan, env);
  // A source split (source-split.mjs; rightkit-gate's snapshot route): `appRoot` is the snapshot of the commit, where the declared
  // sources are read and where the staged files and the receipt go (the Tauri build script reads them there). `cacheRoot` is the
  // checkout's app directory, which keeps every cache, archive, slice and build tree right-release owns (`.right-release/...`) and
  // where Cargo's target directory is resolved: only final outputs and the receipt are written under the snapshot (row 7.05a). Checkout
  // bytes enter a split run only as cache entries whose digest the commit pins, or as tool-invalidated build caches.
  const split = cacheRoot !== undefined && path.resolve(cacheRoot) !== path.resolve(appRoot);
  const fs = deps.fs ?? nodeFs;
  const run = deps.run ?? defaultRun;
  const ctx = {
    appRoot, cacheRoot: split ? path.resolve(cacheRoot) : appRoot, split, target: plan.target, stageDir: plan.stageDir, fs, env, run, protectedPaths: protectedOutputPaths(plan),
    windows: plan.target.os === "windows",
    pid: deps.pid ?? process.pid,
    host: deps.host ?? { os: { darwin: "macos", win32: "windows", linux: "linux" }[process.platform] ?? process.platform, arch: { arm64: "aarch64", x64: "x86_64" }[process.arch] ?? process.arch },
    approvalDigest: deps.approvalDigest ?? ((file, algorithm) => approvalDigest(file, algorithm)),
    log: deps.log ?? (() => {}),
    // Warnings (a never-run row, a row left out) go to deps.warn; without one they join the progress log, so a caller that sets only `log` still sees them.
    warn: deps.warn ?? deps.log ?? (() => {}),
    nativeCatalogs: new Map(),
    fetchBytes: deps.fetchBytes ?? defaultFetchBytes,
    fetch: deps.fetch ?? ((...args) => globalThis.fetch(...args)),
    extractMember: deps.extractMember ?? ((args) => defaultExtractMember({ ...args, run })),
    extractNsisMember: deps.extractNsisMember ?? ((args) => defaultExtractNsisMember({ ...args, run, env, sevenZip: deps.sevenZip, readFile: fs.readFileSync, exists: fs.existsSync })),
    loadMediaCatalog: deps.loadMediaCatalog ?? (() => undefined),
    loadNativeCatalog: deps.loadNativeCatalog ?? (() => undefined),
    makeTempDir: deps.makeTempDir ?? (() => nodeFs.mkdtempSync(path.join(os.tmpdir(), "rightkit-stage-"))),
    removeDir: deps.removeDir ?? ((dir) => nodeFs.rmSync(dir, { recursive: true, force: true })),
  };
  await claimNativeOutputs(plan, ctx);
  const files = [];
  const skipped = [];
  for (const entry of plan.entries) {
    if (entry.skipped) { skipped.push({ kind: entry.kind, index: entry.index, platforms: entry.platforms }); continue; }
    const staged = entry.kind === "cargo-bin" ? stageCargoBin(entry, ctx)
      : entry.kind === "ffmpeg" ? await stageFfmpeg(entry, ctx)
        : entry.kind === "onnxruntime" || entry.kind === "native-library" ? await stageNativeLibrary(entry, ctx)
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
    if (file.kind === "native-library" && (typeof file.library !== "string" || !file.library)) bad(`${file.name} has no library`);
    if (file.hostProof !== undefined && file.hostProof !== "never-run") bad(`${file.name}.hostProof must be "never-run"`);
    if (file.companionOf !== undefined && (typeof file.companionOf !== "string" || !file.companionOf)) bad(`${file.name}.companionOf must be a catalog row id`);
  }
  if (new Set(receipt.files.map((file) => file.path)).size !== receipt.files.length) bad("duplicate staged path");
  if (expected.target && receipt.target !== expected.target) bad(`target ${receipt.target} is not ${expected.target}`);
  if (expected.names && JSON.stringify([...expected.names].sort()) !== JSON.stringify(receipt.files.filter((file) => file.externalBin).map((file) => file.name).sort())) bad("externalBin names differ from the configuration");
  return receipt;
}
