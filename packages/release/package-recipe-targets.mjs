// Declarative package targets (bogusyogi/rightkit#345). `targets.win.winPackage` and `targets.mac.macPackage` replace
// the per-app NSIS / DMG hook scripts. They expand to the SAME hook shape release.mjs already runs (prePackage,
// package, sign, notarize, dmg), so every existing gate still applies unchanged: QA-control scans, the bundle
// marker patch before signing, raw-EXE and installer Authenticode, the NSIS embedding verification, Developer ID
// signing of the .app and its nested code, app notarization before packing, the signed DMG and its notarization.
// The standalone recipes (win-package-recipe.mjs, mac-package-recipe.mjs) hold the same steps for direct callers.
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { resolveTargetRoot as cargoResolveTargetRoot } from "./cargo-target.mjs";
import { defaultManagedRoots, isUnderManagedRoot } from "./managed-target-stage.mjs";
import { validateWinPackage, WinPackageConfigError } from "./win-package-recipe.mjs";
import { IOS_SIGNING_CONTRACT, IosPackageError, validateIosPackageConfig } from "./ios-package-recipe.mjs";
import { IosUploadError, validateIosDistribution } from "./ios-upload.mjs";

export class PackageRecipeTargetError extends Error {
  constructor(message) { super(message); this.name = "PackageRecipeTargetError"; this.code = "RK_PACKAGE_RECIPE_TARGET"; }
}

const fail = (message) => { throw new PackageRecipeTargetError(message); };
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const rel = (appRoot, file) => path.relative(appRoot, file).split(path.sep).join("/");
// Tauri never signs or creates updater artifacts here: release.mjs signs, and sign-updater signs the signed bytes.
const withoutRecipe = (target) => {
  if (!isObject(target) || !("recipe" in target)) return target;
  const { recipe: _ignored, ...rest } = target;
  return rest;
};
const QUIET_TAURI_ENV = { TAURI_SIGNING_PRIVATE_KEY: "", TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "" };
const RECIPE_BUILD = fileURLToPath(new URL("./recipe-build.mjs", import.meta.url));
const CREATE_MAC_UPDATER = fileURLToPath(new URL("./create-mac-updater.mjs", import.meta.url));
const PROFILE = /^[a-z][a-z0-9_-]*$/;

function relativeInside(value, label) {
  if (typeof value !== "string" || !value.trim()) fail(`${label} must be a non-empty relative path`);
  const normalized = value.replaceAll("\\", "/");
  if (path.isAbsolute(value) || /^[A-Za-z]:/.test(value) || normalized.split("/").includes("..")) fail(`${label} must be relative and stay inside the app: ${value}`);
  return normalized;
}

/** `prepare` / `postBuild`: a list of { cmd, args?, cwd? } argv commands (no shell). */
function commandList(value, label) {
  if (!Array.isArray(value)) fail(`${label} must be an array of { cmd, args?, cwd? }`);
  return value.map((item, index) => {
    const where = `${label}[${index}]`;
    if (!isObject(item)) fail(`${where} must be an object`);
    const unknown = Object.keys(item).filter((key) => !["cmd", "args", "cwd"].includes(key));
    if (unknown.length) fail(`${where} has unknown key(s): ${unknown.join(", ")} (allowed: cmd, args, cwd)`);
    if (typeof item.cmd !== "string" || !item.cmd.trim()) fail(`${where}.cmd must be a non-empty string`);
    if (item.args !== undefined && (!Array.isArray(item.args) || item.args.some((arg) => typeof arg !== "string"))) fail(`${where}.args must be a list of strings`);
    return { cmd: item.cmd, args: [...(item.args ?? [])], ...(item.cwd === undefined ? {} : { cwd: relativeInside(item.cwd, `${where}.cwd`) }) };
  });
}

/** The keys shared by winPackage and macPackage: profile, tauriConfig, prepare (and postBuild where allowed). */
function buildExtras(spec, label, { postBuild }) {
  if (spec.profile !== undefined && (typeof spec.profile !== "string" || !PROFILE.test(spec.profile))) fail(`${label}.profile must be a Cargo profile name`);
  const tauriConfig = spec.tauriConfig === undefined ? undefined : relativeInside(spec.tauriConfig, `${label}.tauriConfig`);
  if (tauriConfig && !/\.json$/i.test(tauriConfig)) fail(`${label}.tauriConfig must be a .json file`);
  return {
    profile: spec.profile,
    tauriConfig,
    prepare: spec.prepare === undefined ? [] : commandList(spec.prepare, `${label}.prepare`),
    postBuild: !postBuild || spec.postBuild === undefined ? [] : commandList(spec.postBuild, `${label}.postBuild`),
  };
}

/** Tauri v2 passes Cargo flags after `--`; the profile also decides where Tauri looks for the built binary. */
const profileArgs = (extras) => (extras.profile ? ["--", "--profile", extras.profile] : []);
/** The build command also takes `--jobs N` (Cargo parallelism), after the same `--`. */
const buildCargoArgs = (extras, jobs) => {
  const flags = [...(extras.profile ? ["--profile", extras.profile] : []), ...(jobs ? ["--jobs", String(jobs)] : [])];
  return flags.length ? ["--", ...flags] : [];
};
const configArgs = (extras, appRoot, cwd) => (extras.tauriConfig ? ["--config", path.relative(path.resolve(appRoot, cwd), path.resolve(appRoot, extras.tauriConfig)).split(path.sep).join("/")] : []);

/**
 * release.mjs runs ONE prePackage command: node recipe-build.mjs runs the built-in cleanup, prepare, the Tauri build,
 * the artifact stability poll and postBuild in order. `cleanup` and `stability` are recipe-build plan sections.
 */
function recipeBuildCommand({ extras, build, env, cleanup, stability, stage, timeoutMs }) {
  const steps = [
    ...extras.prepare.map((step) => ({ phase: "prepare", ...step })),
    { phase: "build", cmd: build.cmd, args: build.args, cwd: build.cwd },
    ...extras.postBuild.map((step) => ({ phase: "postBuild", ...step })),
  ];
  const plan = { version: 1, steps, ...(cleanup ? { cleanup } : {}), ...(stability ? { stability } : {}), ...(stage && (stage.pre?.length || stage.post?.length) ? { stage } : {}) };
  return { cmd: process.execPath, args: [RECIPE_BUILD, "--plan", JSON.stringify(plan)], env, ...(timeoutMs ? { timeoutMs } : {}) };
}

const DEFAULT_WIN_TIMEOUT_MS = 20 * 60 * 1000;
const positiveInt = (value) => Number.isInteger(value) && value > 0;

/**
 * Managed Cargo target roots. Artifact paths in a target are declared (and default) under `<cargo dir>/target/...`.
 * With `cargoTargetRoot` (true: <tauriCwd>/src-tauri/Cargo.toml; or an app-relative Cargo.toml path) or CARGO_TARGET_DIR set
 * (default manifest), `cargo metadata`'s target_directory is resolved and every declared path under the default
 * `<cargo dir>/target/` is moved to the real directory. release.mjs needs app-relative paths with no `..`, so a target
 * directory outside the app fails closed (use RIGHT_RELEASE_ARTIFACT_DIR for copies, see the docs), EXCEPT a directory under
 * a broker-managed root (the RustRight data root, bogusyogi/rightkit#924): the declared path then stays at its default
 * app-relative location and the mapper records `{ from: <real path>, to: <declared path> }` so the caller plans a verified
 * copy step (managed-target-stage.mjs). `mapPath.staged` lists the recorded entries; `mapPath.sourceOf(declared)` finds one.
 */
function targetRootMapper({ spec, appRoot, cwd, env, resolveTargetRoot, label, managedRoots = defaultManagedRoots(env) }) {
  const staged = new Map();
  const mapper = buildTargetRootMapper({ spec, appRoot, cwd, env, resolveTargetRoot, label, managedRoots, staged });
  mapper.staged = staged;
  mapper.sourceOf = (declared) => staged.get(declared);
  return mapper;
}

function buildTargetRootMapper({ spec, appRoot, cwd, env, resolveTargetRoot, label, managedRoots, staged }) {
  const explicit = spec.cargoTargetRoot;
  if (explicit !== undefined && explicit !== true && explicit !== false && typeof explicit !== "string") fail(`${label}.cargoTargetRoot must be true or an app-relative Cargo.toml path`);
  const envDir = typeof env.CARGO_TARGET_DIR === "string" && env.CARGO_TARGET_DIR.trim() !== "";
  if (explicit === false || (!explicit && !envDir)) return (declared) => declared;
  const manifestRel = typeof explicit === "string" ? relativeInside(explicit, `${label}.cargoTargetRoot`) : path.posix.join(cwd === "." ? "" : cwd, "src-tauri", "Cargo.toml");
  const manifest = path.resolve(appRoot, manifestRel);
  if (typeof explicit !== "string" && explicit !== true && !existsSync(manifest)) return (declared) => declared;
  let targetDir;
  try { targetDir = resolveTargetRoot(manifest); } catch (error) { fail(`${label}: cannot resolve the Cargo target directory (${error.message})`); }
  const prefix = path.posix.join(path.posix.dirname(manifestRel).replace(/^\.$/, ""), "target");
  return (declared) => {
    const normalized = String(declared).replaceAll("\\", "/");
    if (!normalized.startsWith(`${prefix}/`)) return declared;
    const moved = path.resolve(targetDir, normalized.slice(prefix.length + 1));
    const relative = path.relative(appRoot, moved).split(path.sep).join("/");
    if (relative === "" || relative.startsWith("..") || path.isAbsolute(relative) || /^[A-Za-z]:/.test(relative)) {
      if (isUnderManagedRoot(targetDir, managedRoots)) {
        if (normalized.split("/").includes("..")) fail(`${label}: ${declared} must not contain ..`);
        // The default app-relative location under the app's ignored target directory; the copy is planned by the caller.
        staged.set(normalized, { from: moved, to: normalized });
        return normalized;
      }
      fail(`${label}: the Cargo target directory ${targetDir} is outside the app, so ${declared} cannot be addressed by an app-relative path; point CARGO_TARGET_DIR inside the app or declare the artifact paths yourself`);
    }
    return relative;
  };
}

function winPrePackage({ extras, cwd, appRoot, triple, jobs, timeoutMs, cleanup, stability, stage }) {
  const args = ["exec", "tauri", "build", "--no-bundle", "--target", triple, ...configArgs(extras, appRoot, cwd), ...buildCargoArgs(extras, jobs)];
  return recipeBuildCommand({ extras, build: { cmd: "pnpm", args, cwd }, env: QUIET_TAURI_ENV, cleanup, stability, stage, timeoutMs });
}

/** `targets.win.winPackage` → prePackage (`tauri build --no-bundle`) and package (`tauri bundle --bundles nsis`). */
export function expandWinPackageTarget(target, { appRoot, config = {}, env = process.env, resolveTargetRoot = cargoResolveTargetRoot, managedRoots }) {
  if (!target?.winPackage) return withoutRecipe(target);
  if (target.prePackage !== undefined) fail("targets.win.winPackage replaces prePackage; remove the prePackage script");
  if (target.winPackage.sidecars !== undefined) fail("targets.win.winPackage.sidecars is not supported in a right-release target: declare sidecars through externalBin or mediaSidecar, which release.mjs signs with the raw EXE");
  if (!isObject(target.winPackage)) fail("targets.win.winPackage must be an object");
  const { profile: _p, tauriConfig: _t, prepare: _pr, postBuild, jobs, timeoutMs, cleanStale, cargoTargetRoot: _c, ...coreSpec } = target.winPackage;
  if (postBuild !== undefined) fail("targets.win.winPackage.postBuild is not supported: release.mjs signs the raw EXE between the build and the bundle; use prepare");
  if (jobs !== undefined && !positiveInt(jobs)) fail("targets.win.winPackage.jobs must be a positive integer (Cargo --jobs)");
  if (timeoutMs !== undefined && !positiveInt(timeoutMs)) fail("targets.win.winPackage.timeoutMs must be a positive integer (milliseconds)");
  if (cleanStale !== undefined && typeof cleanStale !== "boolean") fail("targets.win.winPackage.cleanStale must be a boolean");
  const extras = buildExtras(target.winPackage, "targets.win.winPackage", { postBuild: false });
  let plan;
  try { plan = validateWinPackage({ appRoot, config, target: { ...target, winPackage: coreSpec } }); } catch (error) {
    if (error instanceof WinPackageConfigError) fail(`targets.win.winPackage: ${(error.issues ?? [error.message]).join("; ")}`);
    throw error;
  }
  if (target.signingContract !== undefined && target.signingContract !== "windows-raw-exe-authenticode-before-nsis-v1") fail(`targets.win.winPackage builds an NSIS installer; signingContract must be windows-raw-exe-authenticode-before-nsis-v1, got ${target.signingContract}`);
  const cwd = rel(plan.appRoot, plan.tauriCwd) || ".";
  const override = { bundle: { createUpdaterArtifacts: false, windows: { nsis: { installMode: plan.perMachine ? "perMachine" : "currentUser" } } } };
  const mapPath = targetRootMapper({ spec: target.winPackage, appRoot: plan.appRoot, cwd, env, resolveTargetRoot, label: "targets.win.winPackage", ...(managedRoots ? { managedRoots } : {}) });
  const rawExe = mapPath(rel(plan.appRoot, plan.rawExe));
  const installerFile = mapPath(rel(plan.appRoot, plan.installerFile));
  // Stale outputs are removed before the build, and the raw EXE is polled until it stops changing after it (in a managed
  // target directory the poll covers the real EXE before it is copied; the in-app copy is polled again after the copy).
  const cleanup = cleanStale === false ? undefined : { remove: [rawExe, installerFile] };
  const timeout = timeoutMs ?? DEFAULT_WIN_TIMEOUT_MS;
  // Broker-managed target directory: the real raw EXE is deleted before the build (it holds the previous signed copy) and
  // the fresh one is copied out once stable; after signing, the signed copy goes back
  // for `tauri bundle` (which embeds the EXE it finds in the real target directory) and the installer is copied out.
  const rawSource = mapPath.sourceOf(rawExe);
  const installerSource = mapPath.sourceOf(installerFile);
  const prePackageStage = rawSource ? { post: [{ from: rawSource.from, to: rawExe, fresh: true }] } : undefined;
  const packageStage = rawSource || installerSource ? {
    ...(rawSource ? { pre: [{ from: rawExe, to: rawSource.from }] } : {}),
    ...(installerSource ? { post: [{ from: installerSource.from, to: installerFile, fresh: true }] } : {}),
  } : undefined;
  const bundleArgs = ["exec", "tauri", "bundle", "--bundles", "nsis", "--target", plan.triple, ...configArgs(extras, plan.appRoot, cwd), "--config", JSON.stringify(override), ...profileArgs(extras)];
  const bundleCommand = packageStage
    ? recipeBuildCommand({ extras: { prepare: [], postBuild: [] }, build: { cmd: "pnpm", args: bundleArgs, cwd }, env: QUIET_TAURI_ENV, stage: packageStage, timeoutMs: timeout })
    : { cmd: "pnpm", args: bundleArgs, cwd, env: QUIET_TAURI_ENV, timeoutMs: timeout };
  // `recipe` is set only here: a hand-written one is dropped so it can never select a recipe path.
  const { winPackage, recipe: _ignoredWin, ...rest } = target;
  return {
    ...rest,
    signingContract: "windows-raw-exe-authenticode-before-nsis-v1",
    prePackage: winPrePackage({ extras, cwd, appRoot: plan.appRoot, triple: plan.triple, jobs, timeoutMs: timeout, cleanup, stability: { files: [rawExe] }, stage: prePackageStage }),
    package: bundleCommand,
    sign: { ...(target.sign ?? {}), prePackageFiles: [rawExe], files: [installerFile] },
    recipe: { kind: "winPackage", productName: plan.productName, updater: plan.updater },
  };
}

const MAC_KEYS = new Set(["universal", "triple", "tauriCwd", "features", "profile", "tauriConfig", "prepare", "postBuild", "cleanup", "cargoTargetRoot", "updater"]);
const MAC_TRIPLES = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
const MAC_CLEANUP_KEYS = new Set(["staleApps", "staleDmgVolumes", "lsclean"]);

function macCleanupSpec(value) {
  if (value === undefined) return {};
  if (!isObject(value)) fail("targets.mac.macPackage.cleanup must be an object");
  const unknown = Object.keys(value).filter((key) => !MAC_CLEANUP_KEYS.has(key));
  if (unknown.length) fail(`targets.mac.macPackage.cleanup has unknown key(s): ${unknown.join(", ")} (allowed: ${[...MAC_CLEANUP_KEYS].join(", ")})`);
  for (const key of MAC_CLEANUP_KEYS) if (value[key] !== undefined && typeof value[key] !== "boolean") fail(`targets.mac.macPackage.cleanup.${key} must be a boolean`);
  return value;
}

/**
 * `targets.mac.macPackage` → prePackage (`tauri build --bundles app`, universal by default), the .app as the
 * Developer ID signing set, app notarization before packing, and right-release's own signed DMG from the stapled app.
 * The app path and DMG path are `notarize.app` and `notarize.file`; DMG branding is `targets.mac.dmg`.
 * Architecture: `triple` (an explicit arch triple) beats `universal`; `universal: false` means this host's arch.
 * `cleanup` runs built-in steps in recipe-build before the build (stale DMG volumes, stale .app bundles, lsregister).
 * `updater: true | "<app-relative .tar.gz>"` emits the Tauri updater tarball and its signature from the signed, stapled .app:
 * it prepends a `create-mac-updater` command to `postSign` (default output `<App>.app.tar.gz` beside the app's working root).
 */
export function expandMacPackageTarget(target, { appRoot, env = process.env, resolveTargetRoot = cargoResolveTargetRoot, managedRoots }) {
  if (!target?.macPackage) return withoutRecipe(target);
  const spec = target.macPackage;
  if (!isObject(spec)) fail("targets.mac.macPackage must be an object");
  const unknown = Object.keys(spec).filter((key) => !MAC_KEYS.has(key));
  if (unknown.length) fail(`targets.mac.macPackage has unknown key(s): ${unknown.join(", ")} (allowed: ${[...MAC_KEYS].join(", ")})`);
  if (target.prePackage !== undefined || target.package !== undefined) fail("targets.mac.macPackage replaces prePackage and package; remove both scripts");
  if (typeof target.notarize?.app !== "string" || !target.notarize.app.endsWith(".app")) fail("targets.mac.macPackage needs notarize.app: the built .app path");
  if (typeof target.notarize?.file !== "string" || !target.notarize.file.endsWith(".dmg")) fail("targets.mac.macPackage needs notarize.file: the .dmg path right-release builds");
  if (target.dmg === false) fail("targets.mac.macPackage always builds a DMG; remove dmg: false");
  if (spec.universal !== undefined && typeof spec.universal !== "boolean") fail("targets.mac.macPackage.universal must be a boolean");
  if (spec.features !== undefined && (!Array.isArray(spec.features) || spec.features.some((f) => typeof f !== "string" || !/^[A-Za-z0-9_,:-]+$/.test(f)))) fail("targets.mac.macPackage.features must be Cargo feature names");
  if (spec.triple !== undefined && !MAC_TRIPLES.includes(spec.triple)) fail(`targets.mac.macPackage.triple must be one of ${MAC_TRIPLES.join(", ")}`);
  const cwd = spec.tauriCwd ?? ".";
  if (typeof cwd !== "string" || path.isAbsolute(cwd) || cwd.split(/[\\/]/).includes("..")) fail("targets.mac.macPackage.tauriCwd must be a relative path inside the app");
  const cleanupSpec = macCleanupSpec(spec.cleanup);
  if (spec.updater !== undefined && spec.updater !== true && spec.updater !== false && typeof spec.updater !== "string") fail("targets.mac.macPackage.updater must be true or an app-relative .tar.gz path");
  if (typeof spec.updater === "string" && !/\.tar\.gz$/i.test(relativeInside(spec.updater, "targets.mac.macPackage.updater"))) fail("targets.mac.macPackage.updater must end in .tar.gz");
  // Tauri's universal-apple-darwin target builds both slices and lipos them itself.
  const triple = spec.triple ?? (spec.universal === false ? (process.arch === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin") : "universal-apple-darwin");
  if (spec.triple !== undefined) {
    const wrong = [...MAC_TRIPLES, "universal-apple-darwin"].find((other) => other !== triple && target.notarize.app.replaceAll("\\", "/").split("/").includes(other));
    if (wrong) fail(`targets.mac.macPackage.triple is ${triple} but notarize.app is under ${wrong}; the built .app lives in the ${triple} directory`);
  }
  const override = { bundle: { createUpdaterArtifacts: false, macOS: { signingIdentity: null } } };
  const extras = buildExtras(spec, "targets.mac.macPackage", { postBuild: true });
  const args = ["exec", "tauri", "build", "--target", triple, "--bundles", "app", "--no-sign", ...configArgs(extras, appRoot, cwd), "--config", JSON.stringify(override)];
  if (spec.features?.length) args.push("--features", spec.features.join(","));
  args.push(...profileArgs(extras));
  const mapPath = targetRootMapper({ spec, appRoot, cwd, env, resolveTargetRoot, label: "targets.mac.macPackage", ...(managedRoots ? { managedRoots } : {}) });
  const app = mapPath(target.notarize.app);
  const entryFile = (entry) => (typeof entry === "string" ? entry : entry?.file);
  const declaredFiles = target.sign?.prePackageFiles ?? [target.notarize.app];
  if (!declaredFiles.some((entry) => entryFile(entry) === target.notarize.app)) fail("targets.mac.macPackage: sign.prePackageFiles must include notarize.app");
  const prePackageFiles = declaredFiles.map((entry) => (typeof entry === "string" ? mapPath(entry) : { ...entry, file: mapPath(entry.file) }));
  // Built-in cleanup before the build: the bundle dir is where a stale .app lives; DMG volumes are named after the product.
  const appPosix = app.replaceAll("\\", "/");
  const productName = path.posix.basename(appPosix, ".app");
  const volumes = [...new Set([productName, ...(isObject(target.dmg) && typeof target.dmg.volname === "string" ? [target.dmg.volname] : [])])];
  const cleanup = cleanupSpec.staleApps || cleanupSpec.staleDmgVolumes || cleanupSpec.lsclean
    ? {
      ...(cleanupSpec.staleDmgVolumes ? { dmgVolumes: volumes } : {}),
      ...(cleanupSpec.staleApps || cleanupSpec.lsclean ? { apps: { dir: path.posix.dirname(appPosix), remove: cleanupSpec.staleApps === true, unregister: cleanupSpec.lsclean === true } } : {}),
    }
    : undefined;
  // Broker-managed target directory: the built .app (and any other declared file under the target) is copied to its app-relative location.
  // An entry inside another staged entry (a file within the .app) is covered by copying the outer one.
  const macEntries = [...mapPath.staged.values()].filter((entry) => ![...mapPath.staged.values()].some((outer) => outer !== entry && entry.to.startsWith(`${outer.to}/`)));
  const macStage = macEntries.length ? { post: macEntries.map((entry) => ({ ...entry, fresh: true })) } : undefined;
  const wrapped = extras.prepare.length || extras.postBuild.length || cleanup || macStage;
  const { macPackage, recipe: _ignoredMac, ...rest } = target;
  // The updater tarball is made from the signed .app, so it runs in postSign, ahead of any postSign the app declares.
  const updaterOutput = spec.updater === true ? `${productName}.app.tar.gz` : typeof spec.updater === "string" ? relativeInside(spec.updater, "targets.mac.macPackage.updater") : undefined;
  const postSign = updaterOutput === undefined ? {} : {
    postSign: [
      { cmd: process.execPath, args: [CREATE_MAC_UPDATER, "--app", app, "--output", updaterOutput] },
      ...(target.postSign === undefined ? [] : Array.isArray(target.postSign) ? target.postSign : [target.postSign]),
    ],
  };
  return {
    ...rest,
    ...postSign,
    prePackage: wrapped ? recipeBuildCommand({ extras, build: { cmd: "pnpm", args, cwd }, env: QUIET_TAURI_ENV, cleanup, stage: macStage }) : { cmd: "pnpm", args, cwd, env: QUIET_TAURI_ENV },
    sign: { ...(target.sign ?? {}), prePackageFiles },
    notarize: { ...target.notarize, app, appBeforePackage: true },
    dmg: isObject(target.dmg) ? target.dmg : {},
    recipe: { kind: "macPackage", universal: triple === "universal-apple-darwin" },
  };
}

const IOS_TARGET_KEYS = new Set(["signed", "signingContract", "iosPackage", "artifacts", "hardening", "hardeningAllowances", "qaControlScan", "publishBlocked", "distribution", "ownershipScope", "cargoManifest", "cargoTarget", "recipe"]);

/**
 * `targets.ios.iosPackage` (bogusyogi/rightkit#349) is validated strictly and kept as `recipe: { kind: "iosPackage", spec }`.
 * Unlike macPackage there is no hook to expand into: xcodebuild archive and export run in ios-package-recipe.mjs (macOS only),
 * which release.mjs calls like the Android lane. The target may carry only the keys in IOS_TARGET_KEYS: a hand-written
 * prePackage / package / sign / notarize / gradle script, or an App Store Connect credential, is refused.
 * `artifacts` must list the .ipa so the IPA is checked and sealed. `distribution` (the App Store Connect target of an app that
 * also ships desktop) is validated here exactly as the upload validates it, so a credential in it fails the build.
 */
export function expandIosPackageTarget(target, { appRoot: _appRoot } = {}) {
  if (!isObject(target) || target.iosPackage === undefined) return withoutRecipe(target);
  const unknown = Object.keys(target).filter((key) => !IOS_TARGET_KEYS.has(key));
  if (unknown.length) fail(`targets.ios has unknown key(s): ${unknown.join(", ")} (allowed: ${[...IOS_TARGET_KEYS].filter((key) => key !== "recipe").join(", ")}); the iOS recipe replaces prePackage, package, sign and notarize scripts, and App Store Connect credentials never belong in a target`);
  if (target.distribution !== undefined) {
    try { validateIosDistribution(target.distribution); } catch (error) {
      if (error instanceof IosUploadError) fail(`targets.ios.${error.message.replace(/^\[[A-Z0-9_]+\] /, "")}`);
      throw error;
    }
  }
  if (target.ownershipScope !== undefined && (!Array.isArray(target.ownershipScope) || target.ownershipScope.some((entry) => typeof entry !== "string" || !entry.trim()))) fail("targets.ios.ownershipScope must be an array of repository-relative directory strings");
  // The Rust the app links (for the feature-closure scan): an app-relative Cargo.toml and its iPhone triple.
  if (target.cargoManifest !== undefined && (typeof target.cargoManifest !== "string" || !target.cargoManifest.endsWith("Cargo.toml"))) fail("targets.ios.cargoManifest must be an app-relative path to a Cargo.toml");
  if (target.cargoTarget !== undefined && !/^aarch64-apple-ios(-sim)?$/.test(target.cargoTarget)) fail("targets.ios.cargoTarget must be aarch64-apple-ios or aarch64-apple-ios-sim");
  if (target.signed !== true) fail("targets.ios must declare signed: true");
  if (target.signingContract !== undefined && target.signingContract !== IOS_SIGNING_CONTRACT) fail(`targets.ios.iosPackage signs with Xcode automatic signing; signingContract must be ${IOS_SIGNING_CONTRACT}, got ${target.signingContract}`);
  let spec;
  try { spec = validateIosPackageConfig(target.iosPackage); } catch (error) {
    if (error instanceof IosPackageError) fail(`targets.ios.${error.message}`);
    throw error;
  }
  const declared = (target.artifacts ?? []).map((artifact) => String(typeof artifact === "string" ? artifact : artifact?.file).replaceAll("\\", "/").replace(/^\.\//, ""));
  if (!declared.includes(spec.ipa)) fail("targets.ios.artifacts must list iosPackage.ipa so the IPA is checked and sealed");
  const { iosPackage, recipe: _ignored, ...rest } = target;
  return { ...rest, signingContract: IOS_SIGNING_CONTRACT, recipe: { kind: "iosPackage", spec } };
}

/** Expand whichever declarative package key the target carries. */
export function expandPackageRecipeTarget(target, { platform, appRoot, config, env, resolveTargetRoot, managedRoots }) {
  const injected = { ...(env ? { env } : {}), ...(resolveTargetRoot ? { resolveTargetRoot } : {}), ...(managedRoots ? { managedRoots } : {}) };
  if (platform === "win") return expandWinPackageTarget(target, { appRoot, config, ...injected });
  if (platform === "mac") return expandMacPackageTarget(target, { appRoot, config, ...injected });
  if (platform === "ios") return expandIosPackageTarget(target, { appRoot });
  if (target?.winPackage || target?.macPackage || target?.iosPackage) fail(`winPackage / macPackage / iosPackage are not valid on ${platform}`);
  return target;
}
