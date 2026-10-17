// Declarative engine and sidecar staging for Tauri externalBin (bogusyogi/rightkit#345).
//
// Eight apps hand-rolled the same script: build an engine through `rightkit cargo`, find the executable, copy it to
// src-tauri/binaries/<name>-<target-triple>[.exe], stage ffmpeg, stage onnxruntime, copy model resources. This module is
// that script, driven by a `sidecars` list in right-release.config.mjs:
//
//   sidecars: [
//     { kind: "cargo-bin", package: "mailright-engine", bin?, name?, profile?: "release", features?, manifestPath?, sha256?, platforms? },
//     { kind: "ffmpeg", variant?, tools?, notices: { dir, sourceOfferContact }, catalog, stage?, bundleDir? },
//     { kind: "onnxruntime", version, catalog, resourceDir?: "src-tauri/resources/runtime", as?, licenseDir?, cacheDir? },
//     { kind: "resource", from, to, sha256?, platforms? },
//   ]
//
// It never signs (release.mjs signs staged files) and never calls bare cargo. Every effect goes through `deps`.
import { createHash } from "node:crypto";
import nodeFs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync as nodeSpawnSync } from "node:child_process";
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
const KINDS = new Set(["cargo-bin", "ffmpeg", "onnxruntime", "resource"]);
const PLATFORMS = new Set(["mac", "win", "linux"]);
const NAME = /^[A-Za-z0-9][A-Za-z0-9_.-]*$/;
const KEYS = Object.freeze({
  "cargo-bin": ["kind", "package", "bin", "name", "profile", "features", "manifestPath", "sha256", "platforms"],
  ffmpeg: ["kind", "variant", "tools", "notices", "catalog", "stage", "bundleDir", "platforms"],
  onnxruntime: ["kind", "version", "catalog", "resourceDir", "as", "licenseDir", "cacheDir", "platforms"],
  resource: ["kind", "from", "to", "sha256", "platforms"],
});
const TRIPLES = Object.freeze({
  "aarch64-apple-darwin": { platform: "mac", os: "macos", arch: "aarch64" },
  "x86_64-apple-darwin": { platform: "mac", os: "macos", arch: "x86_64" },
  "x86_64-pc-windows-msvc": { platform: "win", os: "windows", arch: "x86_64" },
  "aarch64-pc-windows-msvc": { platform: "win", os: "windows", arch: "aarch64" },
  "x86_64-unknown-linux-gnu": { platform: "linux", os: "linux", arch: "x86_64" },
  "aarch64-unknown-linux-gnu": { platform: "linux", os: "linux", arch: "aarch64" },
});

/** Parse a Rust target triple into { triple, platform, os, arch }. universal-apple-darwin is refused (stage per arch, lipo is the app's). */
export function parseTargetTriple(target) {
  if (target === "universal-apple-darwin") fail("STAGE_SIDECAR_TARGET_UNSUPPORTED", "universal-apple-darwin is not staged here: stage aarch64-apple-darwin and x86_64-apple-darwin separately and merge with lipo");
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

/**
 * Validate and normalise the manifest. `raw` is the `sidecars` array, or { sidecars, stageDir?, receiptPath? }.
 * Every problem is reported at once as STAGE_SIDECAR_CONFIG_INVALID. Entries whose `platforms` excludes the target are
 * kept (shape-checked) with skipped: true.
 */
export function validateSidecarManifest(raw, { target } = {}) {
  const t = parseTargetTriple(target);
  const errors = [];
  const wrapper = Array.isArray(raw) ? { sidecars: raw } : raw;
  if (!isObject(wrapper)) fail("STAGE_SIDECAR_CONFIG_INVALID", "sidecars must be an array (or { sidecars, stageDir?, receiptPath? })");
  for (const key of Object.keys(wrapper)) if (!["sidecars", "stageDir", "receiptPath"].includes(key)) errors.push(`unknown key ${JSON.stringify(key)}`);
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
      const profile = item.profile ?? "release";
      if (typeof profile !== "string" || !/^[a-z][a-z0-9_-]*$/.test(profile)) errors.push(`${where}: profile must be a Cargo profile name`);
      const features = item.features ?? [];
      if (!Array.isArray(features) || features.some((f) => typeof f !== "string" || !/^[A-Za-z0-9_/-]+$/.test(f))) errors.push(`${where}: features must be a list of feature names (package/feature allowed)`);
      const manifestPath = item.manifestPath === undefined ? undefined : relativePath(item.manifestPath, `${where}: manifestPath`, errors);
      sha(item.sha256, "sha256");
      if (typeof name === "string" && stageDir && !skipped) claim(stagedFileName(name, t.os, t.arch), where);
      Object.assign(entry, { package: item.package, bin, name, profile, features: [...features], ...(manifestPath ? { manifestPath } : {}), ...(item.sha256 ? { sha256: item.sha256 } : {}) });
    } else if (item.kind === "ffmpeg") {
      const { kind: _kind, platforms: _platforms, ...rest } = item;
      if (rest.stage !== undefined && isObject(rest.stage) && rest.stage.cmd === "cargo") errors.push(`${where}: stage.cmd must not be bare cargo; use { cmd: "rightkit", args: ["cargo", ...] }`);
      if (!skipped) {
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
      const catalog = item.catalog === undefined ? undefined : relativePath(item.catalog, `${where}: catalog`, errors);
      if (item.as !== undefined && (typeof item.as !== "string" || !NAME.test(item.as))) errors.push(`${where}: as must be a bare file name`);
      Object.assign(entry, { version: item.version, resourceDir, licenseDir, cacheDir, ...(catalog ? { catalog } : {}), ...(item.as ? { as: item.as } : {}) });
    } else {
      const from = relativePath(item.from, `${where}: from`, errors);
      const to = relativePath(item.to, `${where}: to`, errors);
      sha(item.sha256, "sha256");
      if (to && !skipped) claim(to, where);
      Object.assign(entry, { from, to, ...(item.sha256 ? { sha256: item.sha256 } : {}) });
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

function stageCargoBin(entry, ctx) {
  const { target, run, appRoot, env, fs } = ctx;
  const args = ["cargo", "build", "--locked", ...(entry.manifestPath ? ["--manifest-path", path.resolve(appRoot, entry.manifestPath)] : []),
    "-p", entry.package, "--bin", entry.bin, "--target", target.triple, "--profile", entry.profile,
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
    const candidates = builtBinaryCandidates({ targetDir, triple: target.triple, profile: entry.profile, bin: entry.bin, windows: ctx.windows });
    source = candidates.find((file) => fs.existsSync(file));
    if (!source) fail("STAGE_SIDECAR_BINARY_NOT_FOUND", `the build did not produce ${entry.bin} (looked in ${candidates.join(", ")})`, { bin: entry.bin, candidates });
  }
  const sourceSha256 = sha256Of(fs.readFileSync(source));
  if (entry.sha256 && entry.sha256 !== sourceSha256) fail("STAGE_SIDECAR_HASH_MISMATCH", `${entry.bin} built to ${sourceSha256}, expected ${entry.sha256}`, { expected: entry.sha256, actual: sourceSha256 });
  const dest = path.resolve(appRoot, ctx.stageDir, stagedFileName(entry.name, target.os, target.arch));
  const copied = copyInto(ctx, source, dest, { executable: true });
  return [{ kind: "cargo-bin", name: entry.name, path: rel(ctx, dest), ...copied, sourceSha256, package: entry.package, bin: entry.bin, profile: entry.profile, features: entry.features, externalBin: true }];
}

async function stageFfmpeg(entry, ctx) {
  const { appRoot, run, env, fs, target } = ctx;
  const config = entry.media;
  try {
    let resolved = await ctx.loadMediaCatalog({ config, appRoot, entry });
    if (!resolved) {
      if (!config.catalog) fail("STAGE_SIDECAR_CATALOG_UNREADABLE", "ffmpeg needs a catalog path (the rightkit-ffmpeg artifacts.json) or deps.loadMediaCatalog");
      resolved = readJsonCatalog(config.catalog, appRoot, fs, "rightkit-ffmpeg");
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
  const loaded = await (ctx.loadNativeCatalog({ entry, appRoot }) ?? (entry.catalog ? readJsonCatalog(entry.catalog, appRoot, fs, "native-libraries") : undefined));
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
  const dest = path.resolve(appRoot, entry.to);
  const copied = copyInto(ctx, source, dest, { executable: false });
  return [{ kind: "resource", name: path.basename(dest), path: rel(ctx, dest), ...copied, sourceSha256: actual, from: entry.from, externalBin: false }];
}

/**
 * Stage every configured sidecar for `target` (a Rust triple) and write the staging receipt.
 * Returns { receipt, receiptPath, files, externalBinNames }. `deps` (all optional) injects every effect:
 * fs, run, fetchBytes, extractMember, loadMediaCatalog, loadNativeCatalog, makeTempDir, removeDir, now, pid, log, env.
 */
export async function stageSidecars({ appRoot, manifest, target, deps = {} } = {}) {
  if (typeof appRoot !== "string" || !path.isAbsolute(appRoot)) fail("STAGE_SIDECAR_CONFIG_INVALID", "appRoot must be an absolute path");
  const plan = validateSidecarManifest(manifest, { target });
  const fs = deps.fs ?? nodeFs;
  const env = deps.env ?? process.env;
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
