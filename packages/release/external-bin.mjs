// Stage, sign and receipt any number of Tauri externalBin sidecars (Orthic-Labs/rightkit#270).
//
//   targets.mac.externalBin = {
//     names: ["mailright-engine", "mailright-core"],  // bare names, as in tauri.conf externalBin "binaries/<name>"
//     arch: "aarch64",                                // default: build host; one arch per build
//     stageDir: "src-tauri/binaries",                 // default; files are <name>-<target triple>[.exe]
//     bundleDir: "Contents/MacOS",                    // mac only (or "Contents/Resources")
//     stage: { cmd: "node", args: ["scripts/build-engine.mjs"] },   // optional: the app's own build of the files
//   }
//
// This is the generic form of the ffmpeg sidecar from #246 and reuses its naming, hashing, staged-file checks and signing
// evidence checks (media-sidecar.mjs); it adds no catalog, notices or release-manifest seal. It never signs: release.mjs
// signs the staged files with the app (Windows raw-EXE step, macOS nested code) and writes external-bin.json.
import path from "node:path";
import {
  bundledFileName, hostCatalogArch, sidecarTargetTriple, stagedFileName, verifyStagedSidecars,
} from "./media-sidecar.mjs";

export const EXTERNAL_BIN_CONTRACT = "rightkit-external-bin-v1";
export const EXTERNAL_BIN_RECEIPT_NAME = "external-bin.json";

const PLATFORM_OS = Object.freeze({ mac: "macos", win: "windows", linux: "linux" });
const ARCHES = new Set(["aarch64", "x86_64"]);
const CONFIG_KEYS = new Set(["names", "arch", "stageDir", "bundleDir", "stage"]);
const BUNDLE_DIRS = new Set(["Contents/MacOS", "Contents/Resources"]);
const SHA256 = /^[0-9a-f]{64}$/;

const invalid = (code, message) => Object.assign(new Error(message), { code });
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

/** Validate and normalise `targets.<platform>.externalBin`. Every problem is reported at once. */
export function validateExternalBinConfig(raw, { platform, hostArch = hostCatalogArch(), reservedNames = [] } = {}) {
  const errors = [];
  const os = PLATFORM_OS[platform];
  if (!os) throw invalid("EXTERNAL_BIN_CONFIG_INVALID", `externalBin is not supported on platform ${JSON.stringify(platform)} (mac, win or linux)`);
  if (!isObject(raw)) throw invalid("EXTERNAL_BIN_CONFIG_INVALID", "externalBin must be an object");
  for (const key of Object.keys(raw)) if (!CONFIG_KEYS.has(key)) errors.push(`unknown key ${JSON.stringify(key)} (allowed: ${[...CONFIG_KEYS].join(", ")})`);
  const names = raw.names;
  if (!Array.isArray(names) || !names.length || new Set(names).size !== names.length
    || names.some((name) => typeof name !== "string" || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(name) || name !== path.basename(name) || /\.exe$/i.test(name))) {
    errors.push("names must be a non-empty list of unique bare sidecar names (no path, no .exe, no target triple)");
  } else {
    for (const name of names) if (reservedNames.includes(name)) errors.push(`${name} is already staged by mediaSidecar`);
  }
  const arch = raw.arch ?? hostArch;
  if (!ARCHES.has(arch)) errors.push(`arch must be one of ${[...ARCHES].join(", ")} (one architecture per build): ${JSON.stringify(arch)}`);
  else {
    try { sidecarTargetTriple(os, arch); } catch (error) { errors.push(error.message); }
  }
  const stageDirRaw = raw.stageDir ?? "src-tauri/binaries";
  let stageDir;
  if (typeof stageDirRaw !== "string" || !stageDirRaw.trim()) errors.push("stageDir must be a non-empty relative path");
  else {
    const normalized = stageDirRaw.replaceAll("\\", "/");
    if (path.isAbsolute(stageDirRaw) || /^[A-Za-z]:/.test(stageDirRaw) || normalized.split("/").includes("..")) errors.push(`stageDir must be relative and stay inside the app: ${stageDirRaw}`);
    else stageDir = normalized.replace(/\/+$/, "");
  }
  let bundleDir;
  if (os === "macos") {
    bundleDir = raw.bundleDir ?? "Contents/MacOS";
    if (!BUNDLE_DIRS.has(bundleDir)) errors.push(`bundleDir must be ${[...BUNDLE_DIRS].join(" or ")}`);
  } else if (raw.bundleDir !== undefined) errors.push("bundleDir applies to macOS only");
  let stage;
  if (raw.stage !== undefined) {
    if (!isObject(raw.stage) || typeof raw.stage.cmd !== "string" || !raw.stage.cmd || (raw.stage.args !== undefined && !Array.isArray(raw.stage.args))) errors.push("stage must be { cmd, args? }");
    else stage = raw.stage;
  }
  if (errors.length) throw invalid("EXTERNAL_BIN_CONFIG_INVALID", `invalid externalBin configuration:\n${errors.map((line) => `- ${line}`).join("\n")}`);
  return { os, arch, names: [...names], stageDir, ...(bundleDir ? { bundleDir } : {}), ...(stage ? { stage } : {}) };
}

/** The staged file (and, on macOS, bundle path) for every configured sidecar. Same shape `verifyStagedSidecars` reads. */
export function planExternalBin({ config, root }) {
  return {
    config,
    files: config.names.map((name) => ({
      tool: name,
      stagedPath: path.resolve(root, config.stageDir, stagedFileName(name, config.os, config.arch)),
      bundledName: bundledFileName(name, config.os),
      ...(config.bundleDir ? { bundlePath: `${config.bundleDir}/${bundledFileName(name, config.os)}` } : {}),
    })),
  };
}

/** `sign.nestedCode` entries for the macOS bundle: the sidecar executables, hardened runtime, no entitlements. */
export function externalBinNestedCode(config) {
  if (config.os !== "macos") return {};
  return Object.fromEntries(config.names.map((name) => [`${config.bundleDir}/${bundledFileName(name, "macos")}`, { hardenedRuntime: true }]));
}

/** Before signing: every staged file exists, is non-empty, and its hash is recorded (reuses the ffmpeg check). */
export function verifyStagedExternalBins(plan, deps) {
  return verifyStagedSidecars(plan, deps).map((file) => ({ ...file }));
}

/** After signing: `signed` maps name to { sha256, sizeBytes }. */
export function buildExternalBinReceipt(plan, preSign, signed) {
  const { config } = plan;
  return {
    schema: 1,
    contract: EXTERNAL_BIN_CONTRACT,
    os: config.os,
    arch: config.arch,
    signing: config.os === "macos" ? "developer-id-hardened-runtime" : config.os === "windows" ? "authenticode" : "none",
    files: preSign.map((file) => {
      const after = signed[file.tool];
      if (!after || !SHA256.test(after.sha256 ?? "")) throw invalid("EXTERNAL_BIN_RECEIPT_INVALID", `no signed digest for ${file.tool}`);
      return {
        name: file.tool,
        preSignSha256: file.preSignSha256,
        signedSha256: after.sha256,
        sizeBytes: after.sizeBytes,
        ...(file.bundlePath ? { bundlePath: file.bundlePath } : { bundledName: file.bundledName }),
      };
    }),
  };
}

export function validateExternalBinReceipt(receipt, expected = {}) {
  const fail = (message) => { throw invalid("EXTERNAL_BIN_RECEIPT_INVALID", `external bin receipt: ${message}`); };
  if (!isObject(receipt) || receipt.schema !== 1 || receipt.contract !== EXTERNAL_BIN_CONTRACT) fail("unrecognized contract");
  if (!Array.isArray(receipt.files) || !receipt.files.length) fail("no files");
  for (const file of receipt.files) {
    if (typeof file.name !== "string" || !file.name) fail("file without a name");
    for (const key of ["preSignSha256", "signedSha256"]) if (!SHA256.test(file[key] ?? "")) fail(`${file.name}.${key} must be a SHA-256`);
    if (!Number.isSafeInteger(file.sizeBytes) || file.sizeBytes <= 0) fail(`${file.name}.sizeBytes must be positive`);
    if (file.preSignSha256 === file.signedSha256) fail(`${file.name} was not changed by signing`);
  }
  if (new Set(receipt.files.map((file) => file.name)).size !== receipt.files.length) fail("duplicate name");
  if (expected.os && receipt.os !== expected.os) fail(`os ${receipt.os} is not ${expected.os}`);
  if (expected.names && JSON.stringify([...expected.names].sort()) !== JSON.stringify(receipt.files.map((file) => file.name).sort())) fail("names differ from the configuration");
  return receipt;
}
