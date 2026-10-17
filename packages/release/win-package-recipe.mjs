// Declarative default for the Windows two-phase NSIS package step (bogusyogi/rightkit#345).
//
// A target that declares `winPackage: { installer: "nsis", productName, updater, ... }` and no `package`
// script is packaged by this recipe instead of an app-local build-windows-nsis.mjs. The phases are the ones
// the eight app scripts share, in the only order that keeps Authenticode valid:
//
//   build         tauri build --no-bundle                    (raw EXE; no installer yet)
//   marker        patch the Tauri bundle marker to NSS       (BEFORE signing: Tauri rewrites it while bundling)
//   sign-raw      sign every executable the installer wraps  (sidecars first, then the raw EXE)
//   bundle        tauri bundle --bundles nsis                (wraps the signed bytes; never recompiles)
//   verify        raw EXE unchanged on disk + installer embeds exactly the signed EXE
//   sign-installer  Authenticode-sign the installer
//   updater       sign-updater over the SIGNED installer     (a signature made earlier is stale: signing changes bytes)
//
// Every spawn, sign and file-system effect goes through `deps`, so the recipe is testable with fakes only.

import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { verifyNsisEmbeddedBinary } from "./nsis-payload.mjs";
import { signingReceiptPath } from "./signing-receipts.mjs";
import { patchTauriBundleType } from "./tauri-bundle-marker.mjs";

const TOOL_ROOT = path.dirname(fileURLToPath(import.meta.url));
const SIGN_WINDOWS = path.join(TOOL_ROOT, "sign-windows.mjs");
const SIGN_UPDATER = path.join(TOOL_ROOT, "sign-updater.mjs");

export const WIN_PACKAGE_PHASES = Object.freeze(["build", "marker", "sign-raw", "bundle", "verify", "sign-installer", "updater"]);
export const WIN_PACKAGE_DEFAULT_TRIPLE = "x86_64-pc-windows-msvc";
export const WIN_PACKAGE_CONFIG_ERROR = "RK_WIN_PACKAGE_CONFIG";
export const WIN_PACKAGE_PHASE_ERROR = "RK_WIN_PACKAGE_PHASE";

const ALLOWED_KEYS = new Set(["installer", "productName", "sidecars", "updater", "perMachine", "rawExe", "installerFile", "tauriCwd", "triple"]);

/** Invalid `targets.win.winPackage` configuration. `issues` lists every problem found, not just the first. */
export class WinPackageConfigError extends Error {
  constructor(issues) {
    super(`invalid winPackage configuration:\n${issues.map((issue) => `- ${issue}`).join("\n")}`);
    this.name = "WinPackageConfigError";
    this.code = WIN_PACKAGE_CONFIG_ERROR;
    this.issues = issues;
  }
}

/** A phase of a valid recipe failed. `phase` names it; `cause` carries the underlying error when there is one. */
export class WinPackageRecipeError extends Error {
  constructor(phase, message, cause) {
    super(`winPackage ${phase} failed: ${message}`);
    this.name = "WinPackageRecipeError";
    this.code = WIN_PACKAGE_PHASE_ERROR;
    this.phase = phase;
    if (cause) this.cause = cause;
  }
}

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function relativeInside(appRoot, value, label, issues) {
  if (typeof value !== "string" || value.trim() === "" || value !== value.trim()) {
    issues.push(`${label} must be a non-empty path string without surrounding whitespace`);
    return null;
  }
  if (value.includes("\0") || path.win32.isAbsolute(value) || path.posix.isAbsolute(value)) {
    issues.push(`${label} must be relative to the app root, received ${JSON.stringify(value)}`);
    return null;
  }
  const resolved = path.resolve(appRoot, value);
  const rel = path.relative(path.resolve(appRoot), resolved);
  if (rel === "" || rel.startsWith("..") || path.isAbsolute(rel)) {
    issues.push(`${label} must stay inside the app root, received ${JSON.stringify(value)}`);
    return null;
  }
  return resolved;
}

/**
 * Validate `target.winPackage` strictly and return the normalised plan. Throws WinPackageConfigError listing
 * every problem. Unknown keys are refused so a typo cannot silently drop a sidecar or the updater step.
 */
export function validateWinPackage({ appRoot, config = {}, target } = {}) {
  const issues = [];
  if (typeof appRoot !== "string" || appRoot === "") issues.push("appRoot must be a non-empty string");
  if (!isPlainObject(target)) throw new WinPackageConfigError([...issues, "target must be an object"]);
  const spec = target.winPackage;
  if (!isPlainObject(spec)) throw new WinPackageConfigError([...issues, "target.winPackage must be an object"]);
  if (target.package !== undefined) issues.push("target.winPackage and target.package are mutually exclusive; remove the package script");
  for (const key of Object.keys(spec)) if (!ALLOWED_KEYS.has(key)) issues.push(`unknown winPackage key: ${key}`);

  if (spec.installer !== "nsis") issues.push(`winPackage.installer must be "nsis", received ${JSON.stringify(spec.installer)}`);
  if (typeof spec.productName !== "string" || !/^[^\\/:*?"<>|\0-\x1f]{1,64}$/.test(spec.productName) || spec.productName !== spec.productName.trim()) {
    issues.push("winPackage.productName must be 1-64 characters with no path separators, reserved file-name characters or surrounding whitespace");
  }
  if (typeof spec.updater !== "boolean") issues.push("winPackage.updater must be true or false (no default: an updater app must not lose its signature silently)");
  if (spec.perMachine !== undefined && typeof spec.perMachine !== "boolean") issues.push("winPackage.perMachine must be a boolean when present");
  const triple = spec.triple ?? WIN_PACKAGE_DEFAULT_TRIPLE;
  if (typeof triple !== "string" || !/^[A-Za-z0-9_.-]+$/.test(triple) || !/windows/.test(triple)) issues.push(`winPackage.triple must be a Windows target triple, received ${JSON.stringify(spec.triple)}`);
  const packageManager = config.packageManager ?? "pnpm";
  if (packageManager !== "pnpm") issues.push(`winPackage runs Tauri through pnpm; config.packageManager is ${JSON.stringify(packageManager)}`);

  const rawDeclared = spec.rawExe ?? (Array.isArray(target.sign?.prePackageFiles) && target.sign.prePackageFiles.length === 1 ? target.sign.prePackageFiles[0] : undefined);
  const installerDeclared = spec.installerFile ?? (Array.isArray(target.sign?.files) && target.sign.files.length === 1 ? target.sign.files[0] : undefined);
  if (rawDeclared === undefined) issues.push("the raw EXE path is unknown: set winPackage.rawExe or exactly one target.sign.prePackageFiles entry");
  if (installerDeclared === undefined) issues.push("the installer path is unknown: set winPackage.installerFile or exactly one target.sign.files entry");
  const rawExe = rawDeclared === undefined ? null : relativeInside(appRoot, rawDeclared, "rawExe", issues);
  const installerFile = installerDeclared === undefined ? null : relativeInside(appRoot, installerDeclared, "installerFile", issues);
  if (rawExe && !/\.exe$/i.test(rawExe)) issues.push(`rawExe must be an .exe, received ${rawDeclared}`);
  if (installerFile && !/\.exe$/i.test(installerFile)) issues.push(`installerFile must be an .exe, received ${installerDeclared}`);
  if (rawExe && installerFile && rawExe.toLowerCase() === installerFile.toLowerCase()) issues.push("rawExe and installerFile must be different files");
  const tauriCwd = spec.tauriCwd === undefined ? path.resolve(appRoot) : relativeInside(appRoot, spec.tauriCwd, "tauriCwd", issues);

  const sidecars = [];
  if (spec.sidecars !== undefined) {
    if (!Array.isArray(spec.sidecars)) issues.push("winPackage.sidecars must be an array of relative executable paths");
    else {
      const seen = new Set();
      spec.sidecars.forEach((entry, index) => {
        const resolved = relativeInside(appRoot, entry, `sidecars[${index}]`, issues);
        if (!resolved) return;
        if (!/\.(exe|dll)$/i.test(resolved)) issues.push(`sidecars[${index}] must be an .exe or .dll, received ${entry}`);
        const key = resolved.toLowerCase();
        if (seen.has(key)) issues.push(`sidecars[${index}] duplicates an earlier entry: ${entry}`);
        else if (rawExe && key === rawExe.toLowerCase()) issues.push(`sidecars[${index}] is the raw EXE; it is signed anyway`);
        seen.add(key);
        sidecars.push(resolved);
      });
    }
  }
  if (spec.updater === true && target.updater !== undefined && !isPlainObject(target.updater)) issues.push("target.updater must be an object when winPackage.updater is true");

  if (issues.length) throw new WinPackageConfigError(issues);
  return Object.freeze({
    appRoot: path.resolve(appRoot),
    productName: spec.productName,
    updater: spec.updater,
    perMachine: spec.perMachine ?? false,
    triple,
    rawExe,
    installerFile,
    sidecars: Object.freeze(sidecars),
    tauriCwd,
    updaterKeyFile: typeof target.updater?.keyFile === "string" ? target.updater.keyFile : null,
  });
}

function sha256File(file, read) {
  return createHash("sha256").update(read(file)).digest("hex");
}

/**
 * Real effects. Every one is replaceable through `deps`; tests replace all of them.
 * `signWindows(files, { phase })` must throw on any failure and return `[{ file, sha256 }]` of the signed bytes.
 */
export function defaultWinPackageDeps({ appRoot, target, env }) {
  const runSync = (command, args, options) => spawnSync(command, args, options);
  const spawn = (command, args, options) => {
    const shell = process.platform === "win32" && !/\.(?:exe|com)$/i.test(command);
    return runSync(command, args, { ...options, shell, stdio: "inherit" });
  };
  const receiptFor = (phase) => signingReceiptPath({ appRoot, target, platform: "win", phase, stateRoot: env.RIGHT_RELEASE_STATE_ROOT });
  return {
    env,
    platform: process.platform,
    dryRun: false,
    log: (line) => console.log(line),
    spawn,
    signWindows(files, { phase, dryRun = false }) {
      if (dryRun) {
        const dry = spawn("node", [SIGN_WINDOWS, "--dry-run", ...files], { cwd: appRoot, env });
        if (dry.error || dry.status !== 0) throw new Error(`sign-windows --dry-run exited ${dry.status ?? dry.error?.message}`);
        return [];
      }
      const receipt = receiptFor(phase);
      rmSync(receipt, { force: true });
      mkdirSync(path.dirname(receipt), { recursive: true });
      const result = spawn("node", [SIGN_WINDOWS, "--receipt", receipt, ...files], { cwd: appRoot, env });
      if (result.error || result.status !== 0) throw new Error(`sign-windows exited ${result.status ?? result.error?.message}`);
      const evidence = JSON.parse(readFileSync(receipt, "utf8"));
      return evidence.files.map((item) => ({ file: path.resolve(item.file), sha256: item.after?.sha256 }));
    },
    signUpdater(file, { keyFile, dryRun = false }) {
      const childEnv = { ...env };
      delete childEnv.TAURI_SIGNING_PRIVATE_KEY;
      delete childEnv.TAURI_SIGNING_PRIVATE_KEY_PASSWORD;
      const result = spawn("node", [SIGN_UPDATER, "--file", file, ...(dryRun ? ["--dry-run"] : []), ...(keyFile ? ["--key-file", keyFile] : [])], { cwd: appRoot, env: childEnv });
      if (result.error || result.status !== 0) throw new Error(`sign-updater exited ${result.status ?? result.error?.message}`);
    },
    patchMarker: (file, options) => patchTauriBundleType(file, options),
    verifyEmbedded: (options) => verifyNsisEmbeddedBinary(options),
    fs: { existsSync, readFileSync, rmSync, mkdirSync },
  };
}

/** Where a produced artifact really is: the declared path, else the managed artifact directory (same rule as release.mjs). */
function locate(declared, { env, fs }) {
  if (fs.existsSync(declared)) return declared;
  const managed = env.RIGHT_RELEASE_ARTIFACT_DIR;
  if (managed) {
    const candidate = path.join(path.resolve(managed), path.basename(declared));
    if (fs.existsSync(candidate)) return candidate;
  }
  return declared;
}

/**
 * Run the whole recipe for one win target. Returns a receipt; throws WinPackageConfigError before any effect
 * when the configuration is invalid, and WinPackageRecipeError (with `.phase`) when a phase fails. A failed
 * phase stops the recipe: nothing later runs, so a failed signature can never be wrapped into an installer.
 */
export function runWinPackageRecipe({ appRoot, config = {}, target, deps = {} } = {}) {
  const plan = validateWinPackage({ appRoot, config, target });
  const env = deps.env ?? process.env;
  const base = defaultWinPackageDeps({ appRoot: plan.appRoot, target, env });
  const d = { ...base, ...deps, env, fs: { ...base.fs, ...(deps.fs ?? {}) } };
  const { fs } = d;
  const dry = d.dryRun === true;
  const phases = [];
  const phase = (name, work) => {
    d.log(`win-package: ${name}`);
    try {
      const value = work();
      phases.push(name);
      return value;
    } catch (error) {
      if (error instanceof WinPackageRecipeError) throw error;
      throw new WinPackageRecipeError(name, error.message, error);
    }
  };
  const tauri = (args, extraEnv) => {
    if (dry) { d.log(`dry-run: pnpm exec tauri ${args.join(" ")}`); return; }
    const result = d.spawn("pnpm", ["exec", "tauri", ...args], { cwd: plan.tauriCwd, env: { ...env, ...extraEnv } });
    if (result?.error) throw new Error(`could not start tauri: ${result.error.message}`);
    if (result?.status !== 0) throw new Error(`tauri ${args[0]} exited ${result?.status ?? "without a status"}`);
  };
  // `tauri bundle` never signs the updater artifact itself: createUpdaterArtifacts is forced off and sign-updater signs the
  // Authenticode-signed installer. An empty key and password stop Tauri from ever prompting.
  const quiet = { TAURI_SIGNING_PRIVATE_KEY: "", TAURI_SIGNING_PRIVATE_KEY_PASSWORD: env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ?? "" };

  if (d.platform !== "win32" && !dry) throw new WinPackageRecipeError("build", `the Windows package recipe must run on Windows, not ${d.platform}`);

  let raw = plan.rawExe;
  const sidecars = [...plan.sidecars];

  phase("build", () => {
    if (!dry) fs.rmSync(plan.rawExe, { force: true });
    tauri(["build", "--no-bundle", "--target", plan.triple], quiet);
    if (dry) return;
    raw = locate(plan.rawExe, { env, fs });
    if (!fs.existsSync(raw)) throw new Error(`tauri build did not produce the raw EXE: ${plan.rawExe}`);
    for (const sidecar of sidecars) if (!fs.existsSync(sidecar)) throw new Error(`declared sidecar does not exist: ${sidecar}`);
  });

  const marker = phase("marker", () => (dry ? null : d.patchMarker(raw, { bundle: "nsis" })));

  const signedRaw = phase("sign-raw", () => {
    // Sidecars first, raw EXE last, in one signing call: nothing the installer wraps is left unsigned.
    const files = [...sidecars, raw];
    const evidence = d.signWindows(files, { phase: "raw-exe", dryRun: dry });
    if (dry) return null;
    const byFile = new Map((evidence ?? []).map((item) => [path.resolve(item.file).toLowerCase(), item.sha256]));
    for (const file of files) {
      if (!/^[0-9a-f]{64}$/i.test(byFile.get(path.resolve(file).toLowerCase()) ?? "")) throw new Error(`no signed digest recorded for ${file}`);
    }
    return { sha256: byFile.get(path.resolve(raw).toLowerCase()), sidecars: Object.fromEntries(sidecars.map((file) => [file, byFile.get(path.resolve(file).toLowerCase())])) };
  });

  const installerBase = plan.installerFile;
  let installer = installerBase;
  phase("bundle", () => {
    if (!dry) for (const stale of [installerBase, `${installerBase}.sig`, `${installerBase}.zip`, `${installerBase}.zip.sig`]) fs.rmSync(stale, { force: true });
    const override = { bundle: { createUpdaterArtifacts: false, windows: { nsis: { installMode: plan.perMachine ? "perMachine" : "currentUser" } } } };
    tauri(["bundle", "--bundles", "nsis", "--target", plan.triple, "--config", JSON.stringify(override)], quiet);
    if (dry) return;
    installer = locate(installerBase, { env, fs });
    if (!fs.existsSync(installer)) throw new Error(`tauri bundle did not produce the installer: ${installerBase}`);
  });

  const embedded = phase("verify", () => {
    if (dry) return null;
    const current = sha256File(raw, fs.readFileSync);
    if (current !== signedRaw.sha256) {
      throw new Error(`bundling rewrote the signed raw EXE: signed sha256 ${signedRaw.sha256}, on-disk sha256 ${current}; apply every byte patch before signing`);
    }
    return d.verifyEmbedded({ installer, entryName: path.basename(raw), expectedSha256: signedRaw.sha256 });
  });

  const signedInstaller = phase("sign-installer", () => {
    const evidence = d.signWindows([installer], { phase: "installer", dryRun: dry });
    if (dry) return null;
    const digest = (evidence ?? []).find((item) => path.resolve(item.file).toLowerCase() === path.resolve(installer).toLowerCase())?.sha256;
    if (!/^[0-9a-f]{64}$/i.test(digest ?? "")) throw new Error(`no signed digest recorded for ${installer}`);
    return { sha256: digest };
  });

  let updaterSignature = null;
  if (plan.updater) {
    phase("updater", () => {
      if (!dry) fs.rmSync(`${installer}.sig`, { force: true });
      d.signUpdater(installer, { keyFile: plan.updaterKeyFile, dryRun: dry });
      if (dry) return;
      if (!fs.existsSync(`${installer}.sig`)) throw new Error(`sign-updater did not write ${installer}.sig`);
      updaterSignature = `${installer}.sig`;
    });
  }

  return {
    schema: 1,
    productName: plan.productName,
    dryRun: dry,
    phases,
    rawExe: { file: raw, signedSha256: signedRaw?.sha256 ?? null },
    sidecars: signedRaw?.sidecars ?? {},
    marker,
    embedded,
    installer: { file: installer, signedSha256: signedInstaller?.sha256 ?? null },
    updaterSignature,
  };
}
