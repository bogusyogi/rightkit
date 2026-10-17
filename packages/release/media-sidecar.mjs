// Bundle the rightkit-ffmpeg decode sidecar (ffmpeg + ffprobe) into a desktop app and seal its identity.
//
//   targets.mac.mediaSidecar = {
//     arch: "aarch64",                       // catalog arch (default: build host); one arch per build
//     variant: "decode",                     // catalog variant (default "decode")
//     tools: ["ffmpeg", "ffprobe"],          // default both
//     stageDir: "src-tauri/binaries",        // Tauri externalBin directory (default)
//     bundleDir: "Contents/MacOS",           // mac only: where Tauri puts externalBin (or "Contents/Resources")
//     notices: { dir: "src-tauri/resources/third-party/ffmpeg", sourceOfferContact: "https://..." },
//     catalog: "path/to/artifacts.json",     // optional; default: the rightkit-ffmpeg crate Cargo resolves
//     stage: { cmd: "cargo", args: [...] },  // optional: runs rightkit_ffmpeg::stage_external_bin
//   }
//
// This module owns the config shape, catalog row selection, the notice set, the pre-sign checks and the receipt.
// It never signs and never hashes the catalog's content digest (the crate's stage_external_bin does that when it
// copies the binary in); release.mjs signs the files with the app and build-release.mjs seals the receipt.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync, copyFileSync } from "node:fs";
import path from "node:path";

export const MEDIA_SIDECAR_CONTRACT = "rightkit-ffmpeg-sidecar-v1";
export const MEDIA_SIDECAR_RECEIPT_NAME = "media-sidecar.json";
export const MEDIA_SIDECAR_OFFER_NAME = "FFMPEG-SOURCE-OFFER.txt";
export const MEDIA_SIDECAR_TOOLS = Object.freeze(["ffmpeg", "ffprobe"]);

const PLATFORM_OS = Object.freeze({ mac: "macos", win: "windows", linux: "linux" });
const ARCHES = new Set(["aarch64", "x86_64"]);
const SHA256 = /^[0-9a-f]{64}$/;
const CONFIG_KEYS = new Set(["arch", "variant", "tools", "stageDir", "bundleDir", "notices", "catalog", "stage"]);
const BUNDLE_DIRS = new Set(["Contents/MacOS", "Contents/Resources"]);

const invalid = (code, message) => Object.assign(new Error(message), { code });
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const sha256Bytes = (bytes) => createHash("sha256").update(bytes).digest("hex");
const sha256File = (file) => sha256Bytes(readFileSync(file));

/** Tauri target triple for a catalog os/arch pair (same table as rightkit-ffmpeg `tauri_target_triple`). */
export function sidecarTargetTriple(os, arch) {
  const table = {
    "macos/aarch64": "aarch64-apple-darwin",
    "macos/x86_64": "x86_64-apple-darwin",
    "windows/x86_64": "x86_64-pc-windows-msvc",
    "windows/aarch64": "aarch64-pc-windows-msvc",
    "linux/x86_64": "x86_64-unknown-linux-gnu",
    "linux/aarch64": "aarch64-unknown-linux-gnu",
  };
  const triple = table[`${os}/${arch}`];
  if (!triple) throw invalid("MEDIA_SIDECAR_PLATFORM", `no Tauri target triple for ${os}/${arch}`);
  return triple;
}

/** `ffmpeg-<triple>[.exe]`: the file Tauri resolves for `externalBin: ["binaries/ffmpeg"]`. */
export function stagedFileName(tool, os, arch) {
  return `${tool}-${sidecarTargetTriple(os, arch)}${os === "windows" ? ".exe" : ""}`;
}

/** The name Tauri gives the binary inside the bundle (no triple). */
export function bundledFileName(tool, os) {
  return `${tool}${os === "windows" ? ".exe" : ""}`;
}

export function hostCatalogArch(nodeArch = process.arch) {
  return nodeArch === "arm64" ? "aarch64" : nodeArch === "x64" ? "x86_64" : nodeArch;
}

/**
 * Validate and normalise `targets.<platform>.mediaSidecar`. Every problem is reported at once.
 * Returns { os, arch, variant, tools, stageDir, bundleDir, notices: { dir, sourceOfferContact }, catalog?, stage? }.
 */
export function validateMediaSidecarConfig(raw, { platform, hostArch = hostCatalogArch() } = {}) {
  const errors = [];
  const os = PLATFORM_OS[platform];
  if (!os) throw invalid("MEDIA_SIDECAR_CONFIG_INVALID", `mediaSidecar is not supported on platform ${JSON.stringify(platform)} (mac, win or linux)`);
  if (!isObject(raw)) throw invalid("MEDIA_SIDECAR_CONFIG_INVALID", "mediaSidecar must be an object");
  for (const key of Object.keys(raw)) if (!CONFIG_KEYS.has(key)) errors.push(`unknown key ${JSON.stringify(key)} (allowed: ${[...CONFIG_KEYS].join(", ")})`);
  const relativePath = (value, label) => {
    if (typeof value !== "string" || !value.trim()) { errors.push(`${label} must be a non-empty relative path`); return undefined; }
    const normalized = value.replaceAll("\\", "/");
    if (path.isAbsolute(value) || /^[A-Za-z]:/.test(value) || normalized.split("/").includes("..")) { errors.push(`${label} must be relative and stay inside the app: ${value}`); return undefined; }
    return normalized.replace(/\/+$/, "");
  };
  const arch = raw.arch ?? hostArch;
  if (!ARCHES.has(arch)) errors.push(`arch must be one of ${[...ARCHES].join(", ")} (one architecture per build): ${JSON.stringify(arch)}`);
  const variant = raw.variant ?? "decode";
  if (typeof variant !== "string" || !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(variant)) errors.push(`variant must be a catalog variant slug: ${JSON.stringify(variant)}`);
  const tools = raw.tools ?? [...MEDIA_SIDECAR_TOOLS];
  if (!Array.isArray(tools) || !tools.length || new Set(tools).size !== tools.length || tools.some((tool) => !MEDIA_SIDECAR_TOOLS.includes(tool))) {
    errors.push(`tools must be a non-empty list without duplicates drawn from ${MEDIA_SIDECAR_TOOLS.join(", ")}`);
  }
  const stageDir = relativePath(raw.stageDir ?? "src-tauri/binaries", "stageDir");
  let bundleDir;
  if (os === "macos") {
    bundleDir = raw.bundleDir ?? "Contents/MacOS";
    if (!BUNDLE_DIRS.has(bundleDir)) errors.push(`bundleDir must be ${[...BUNDLE_DIRS].join(" or ")}`);
  } else if (raw.bundleDir !== undefined) errors.push("bundleDir applies to macOS only");
  let notices;
  if (!isObject(raw.notices)) errors.push("notices must be { dir, sourceOfferContact }: the LGPL text and source offer must ship with the app");
  else {
    for (const key of Object.keys(raw.notices)) if (!["dir", "sourceOfferContact"].includes(key)) errors.push(`notices: unknown key ${JSON.stringify(key)}`);
    const dir = relativePath(raw.notices.dir, "notices.dir");
    const contact = raw.notices.sourceOfferContact;
    if (typeof contact !== "string" || !/^(https:\/\/\S+|[^\s@]+@[^\s@]+\.[^\s@]+)$/.test(contact)) errors.push("notices.sourceOfferContact must be an https URL or an email address where the source can be requested");
    if (dir) notices = { dir, sourceOfferContact: contact };
  }
  const catalog = raw.catalog === undefined ? undefined : relativePath(raw.catalog, "catalog");
  let stage;
  if (raw.stage !== undefined) {
    if (!isObject(raw.stage) || typeof raw.stage.cmd !== "string" || !raw.stage.cmd || (raw.stage.args !== undefined && !Array.isArray(raw.stage.args))) errors.push("stage must be { cmd, args? }");
    else stage = raw.stage;
  }
  if (errors.length) throw invalid("MEDIA_SIDECAR_CONFIG_INVALID", `invalid mediaSidecar configuration:\n${errors.map((line) => `- ${line}`).join("\n")}`);
  return { os, arch, variant, tools: [...tools], stageDir, ...(bundleDir ? { bundleDir } : {}), notices, ...(catalog ? { catalog } : {}), ...(stage ? { stage } : {}) };
}

/** `sign.nestedCode` entries for the macOS bundle: the sidecar executables, hardened runtime, no entitlements. */
export function mediaSidecarNestedCode(config) {
  if (config.os !== "macos") return {};
  return Object.fromEntries(config.tools.map((tool) => [`${config.bundleDir}/${bundledFileName(tool, "macos")}`, { hardenedRuntime: true }]));
}

/**
 * Select the one approved catalog row per tool for this variant/os/arch. A missing row, a pending or stale row, or an
 * approved row without a content digest refuses the release: Linux has no approved row yet, so a Linux plan fails
 * here with MEDIA_SIDECAR_NO_ROW instead of shipping an unverified binary.
 */
export function selectSidecarRows(catalog, config) {
  if (!isObject(catalog) || !Array.isArray(catalog.artifacts)) throw invalid("MEDIA_SIDECAR_CATALOG_INVALID", "catalog has no artifacts array");
  return config.tools.map((tool) => {
    const matches = catalog.artifacts.filter((row) => row.tool === tool && (row.variant ?? "decode") === config.variant
      && String(row.os).toLowerCase() === config.os && String(row.arch).toLowerCase() === config.arch);
    const where = `${tool} ${config.variant} ${config.os}/${config.arch}`;
    if (matches.length === 0) throw invalid("MEDIA_SIDECAR_NO_ROW", `the rightkit-ffmpeg catalog has no ${where} row`);
    if (matches.length > 1) throw invalid("MEDIA_SIDECAR_AMBIGUOUS_ROW", `the rightkit-ffmpeg catalog has ${matches.length} ${where} rows`);
    const [row] = matches;
    if (row.status !== "approved") throw invalid("MEDIA_SIDECAR_NOT_APPROVED", `the ${where} row ${row.id} is ${row.status}, not approved`);
    if (!SHA256.test(row.contentSha256 ?? "")) throw invalid("MEDIA_SIDECAR_NOT_APPROVED", `the ${where} row ${row.id} is approved but has no content digest`);
    if (!Array.isArray(row.attribution?.noticeFiles) || !row.attribution.noticeFiles.length || typeof row.attribution.sourceUrl !== "string") {
      throw invalid("MEDIA_SIDECAR_NOTICE_MISSING", `the ${where} row ${row.id} lacks notice files or a source URL`);
    }
    return row;
  });
}

/**
 * Plan the sidecar for one build. `catalog` is the parsed artifacts.json, `crateDir` the rightkit-ffmpeg crate
 * directory (notice files are relative to it), `root` the app root.
 */
export function planMediaSidecar({ config, catalog, crateDir, crateVersion, catalogBytes, root }) {
  const rows = selectSidecarRows(catalog, config);
  const files = rows.map((row) => ({
    tool: row.tool,
    artifactId: row.id,
    version: row.version,
    contentSha256: row.contentSha256,
    stagedPath: path.resolve(root, config.stageDir, stagedFileName(row.tool, config.os, config.arch)),
    bundledName: bundledFileName(row.tool, config.os),
    ...(config.bundleDir ? { bundlePath: `${config.bundleDir}/${bundledFileName(row.tool, config.os)}` } : {}),
  }));
  const noticeSources = new Map();
  for (const row of rows) for (const notice of row.attribution.noticeFiles) {
    const normalized = String(notice).replaceAll("\\", "/");
    if (path.isAbsolute(notice) || normalized.split("/").includes("..")) throw invalid("MEDIA_SIDECAR_NOTICE_MISSING", `notice file path escapes the crate: ${notice}`);
    noticeSources.set(normalized, path.join(crateDir, ...normalized.split("/")));
  }
  const noticeDir = path.resolve(root, config.notices.dir);
  const notices = [...noticeSources].map(([relative, source]) => ({ source, dest: path.join(noticeDir, path.basename(relative)) }));
  const names = notices.map((item) => path.basename(item.dest));
  if (new Set(names).size !== names.length) throw invalid("MEDIA_SIDECAR_NOTICE_MISSING", "two notice files share a basename");
  return {
    config,
    rows,
    files,
    notices,
    offerPath: path.join(noticeDir, MEDIA_SIDECAR_OFFER_NAME),
    offer: sourceOfferText({ rows, contact: config.notices.sourceOfferContact, crateVersion }),
    catalog: { crateVersion: crateVersion ?? null, schemaVersion: catalog.schemaVersion ?? null, ...(catalogBytes ? { sha256: sha256Bytes(catalogBytes) } : {}) },
  };
}

/** The written LGPL source offer: where the exact FFmpeg source, the build recipe and the licence text are. */
export function sourceOfferText({ rows, contact, crateVersion }) {
  const lines = [
    "FFmpeg source offer",
    "",
    "This application includes unmodified FFmpeg programs (ffmpeg and ffprobe) built from the FFmpeg source below. FFmpeg is licensed under the",
    "GNU Lesser General Public License version 2.1 or later (LGPL-2.1-or-later); the build is configured with --disable-gpl and --disable-nonfree.",
    "The licence text and build notes are in this folder. You may replace these programs with your own build of the same source.",
    "",
    `Source code and build recipe: ${rows[0].attribution.sourceUrl}`,
    `Build recipes, pinned sources and notes: https://github.com/bogusyogi/rightkit/tree/main/crates/rightkit-ffmpeg${crateVersion ? ` (crate rightkit-ffmpeg ${crateVersion}, published on crates.io)` : ""}`,
    `To request the corresponding source on physical media, or if a link is unavailable, write to: ${contact}`,
    "",
    "Programs shipped",
  ];
  for (const row of rows) {
    lines.push(`- ${row.tool} ${row.version} (${row.id})`);
    lines.push(`  licence: ${row.attribution.licenseSpdx}`);
    lines.push(`  source: ${row.attribution.sourceUrl}`);
    lines.push(`  source archive sha256: ${row.sourceSha256}`);
    lines.push(`  build notes: ${row.attribution.buildProvenance}`);
    for (const component of row.attribution.components ?? []) lines.push(`  component ${component.name} ${component.version}: ${component.licenseSpdx} (${component.sourceUrl})`);
  }
  return `${lines.join("\n")}\n`;
}

/** Copy the catalog's notice files and write the source offer into the app's notice directory. Idempotent. */
export function writeMediaSidecarNotices(plan, { exists = existsSync, mkdir = mkdirSync, copy = copyFileSync, write = writeFileSync } = {}) {
  for (const { source } of plan.notices) {
    if (!exists(source)) throw invalid("MEDIA_SIDECAR_NOTICE_MISSING", `notice file not found in the rightkit-ffmpeg crate: ${source}`);
  }
  mkdir(path.dirname(plan.offerPath), { recursive: true });
  for (const { source, dest } of plan.notices) copy(source, dest);
  write(plan.offerPath, plan.offer);
  return [...plan.notices.map((item) => item.dest), plan.offerPath];
}

/** Before signing: every staged binary exists, is a non-empty file, and its hash is recorded. */
export function verifyStagedSidecars(plan, { exists = existsSync, stat = (file) => statSync(file), hashFile = sha256File } = {}) {
  return plan.files.map((file) => {
    if (!exists(file.stagedPath)) throw invalid("MEDIA_SIDECAR_NOT_STAGED", `${file.tool} is not staged: expected ${file.stagedPath} (rightkit_ffmpeg::stage_external_bin writes it; configure mediaSidecar.stage or stage it in prePackage)`);
    const info = stat(file.stagedPath);
    if (!info.isFile() || info.size <= 0) throw invalid("MEDIA_SIDECAR_NOT_STAGED", `${file.stagedPath} is not a non-empty file`);
    return { ...file, preSignSha256: hashFile(file.stagedPath), preSignSizeBytes: info.size };
  });
}

/** After signing: the receipt that build-release.mjs seals into the release manifest. `signed` maps tool to { sha256, sizeBytes }. */
export function buildMediaSidecarReceipt(plan, preSign, signed, { notices = [], hashFile = sha256File, signing } = {}) {
  const config = plan.config;
  return {
    schema: 1,
    contract: MEDIA_SIDECAR_CONTRACT,
    variant: config.variant,
    os: config.os,
    arch: config.arch,
    signing: signing ?? (config.os === "macos" ? "developer-id-hardened-runtime" : config.os === "windows" ? "authenticode" : "none"),
    catalog: plan.catalog,
    files: preSign.map((file) => {
      const after = signed[file.tool];
      if (!after || !SHA256.test(after.sha256 ?? "")) throw invalid("MEDIA_SIDECAR_RECEIPT_INVALID", `no signed digest for ${file.tool}`);
      return {
        tool: file.tool,
        artifactId: file.artifactId,
        version: file.version,
        catalogContentSha256: file.contentSha256,
        preSignSha256: file.preSignSha256,
        signedSha256: after.sha256,
        sizeBytes: after.sizeBytes,
        ...(file.bundlePath ? { bundlePath: file.bundlePath } : { bundledName: file.bundledName }),
      };
    }),
    notices: [...plan.notices.map((item) => item.dest), plan.offerPath].map((file) => ({ name: path.basename(file), sha256: hashFile(file) })),
    sourceOfferContact: config.notices.sourceOfferContact,
    ...(notices.length ? { extra: notices } : {}),
  };
}

/** Structural check used by the seal and by publication. `expected` optionally pins variant, os and tools. */
export function validateMediaSidecarReceipt(receipt, expected = {}) {
  const fail = (message) => { throw invalid("MEDIA_SIDECAR_RECEIPT_INVALID", `media sidecar receipt: ${message}`); };
  if (!isObject(receipt) || receipt.schema !== 1 || receipt.contract !== MEDIA_SIDECAR_CONTRACT) fail("unrecognized contract");
  if (!Array.isArray(receipt.files) || !receipt.files.length) fail("no files");
  for (const file of receipt.files) {
    if (!MEDIA_SIDECAR_TOOLS.includes(file.tool) || typeof file.artifactId !== "string" || !file.artifactId) fail(`bad file entry ${JSON.stringify(file.tool)}`);
    for (const key of ["catalogContentSha256", "preSignSha256", "signedSha256"]) if (!SHA256.test(file[key] ?? "")) fail(`${file.tool}.${key} must be a SHA-256`);
    if (!Number.isSafeInteger(file.sizeBytes) || file.sizeBytes <= 0) fail(`${file.tool}.sizeBytes must be positive`);
    if (receipt.signing !== "none" && file.preSignSha256 === file.signedSha256) fail(`${file.tool} was not changed by signing`);
  }
  if (new Set(receipt.files.map((file) => file.tool)).size !== receipt.files.length) fail("duplicate tool");
  if (!["developer-id-hardened-runtime", "authenticode", "none"].includes(receipt.signing)) fail("unknown signing mode");
  if (!Array.isArray(receipt.notices) || !receipt.notices.some((item) => item.name === MEDIA_SIDECAR_OFFER_NAME) || receipt.notices.some((item) => !SHA256.test(item.sha256 ?? ""))) fail("notices must include the source offer with digests");
  if (!receipt.notices.some((item) => /LGPL/i.test(item.name))) fail("notices must include the LGPL text");
  if (expected.variant && receipt.variant !== expected.variant) fail(`variant ${receipt.variant} is not ${expected.variant}`);
  if (expected.os && receipt.os !== expected.os) fail(`os ${receipt.os} is not ${expected.os}`);
  if (expected.tools && JSON.stringify([...expected.tools].sort()) !== JSON.stringify(receipt.files.map((file) => file.tool).sort())) fail("tools differ from the configuration");
  return receipt;
}

/** macOS: the nested signing receipt must show every sidecar signed (strict-valid) with the digest now on disk. */
export function macSidecarSigningProblems(nestedReceipt, signedFiles) {
  const problems = [];
  const byFile = new Map((nestedReceipt?.files ?? []).map((item) => [path.resolve(item.file), item]));
  for (const { file, sha256 } of signedFiles) {
    const item = byFile.get(path.resolve(file));
    if (!item) problems.push(`${file}: not in the nested signing receipt`);
    else if (item.codesign !== "strict-valid") problems.push(`${file}: codesign is ${item.codesign ?? "missing"}, not strict-valid`);
    else if (item.after?.sha256 !== sha256) problems.push(`${file}: bytes changed after signing`);
  }
  return problems;
}

export function sidecarFileEvidence(file, { hashFile = sha256File, stat = (item) => statSync(item) } = {}) {
  return { sha256: hashFile(file), sizeBytes: stat(file).size };
}

/** Windows: each sidecar's signed digest must be one the raw-EXE signing receipt recorded as Valid after signing. */
export function windowsSidecarSigningProblems(rawReceipt, receipt) {
  const problems = [];
  const signed = new Map((rawReceipt?.files ?? []).map((item) => [item.after?.sha256, item]));
  for (const file of receipt.files) {
    const item = signed.get(file.signedSha256);
    if (!item) problems.push(`${file.tool}: signed digest ${file.signedSha256} is not in the raw-EXE signing receipt`);
    else if (item.authenticode !== "Valid" || item.timestampPresent !== true) problems.push(`${file.tool}: Authenticode evidence is not Valid with a timestamp`);
  }
  return problems;
}
