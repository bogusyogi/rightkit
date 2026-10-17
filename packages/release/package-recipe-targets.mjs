// Declarative package targets (bogusyogi/rightkit#345). `targets.win.winPackage` and `targets.mac.macPackage` replace
// the per-app NSIS / DMG hook scripts. They expand to the SAME hook shape release.mjs already runs (prePackage,
// package, sign, notarize, dmg), so every existing gate still applies unchanged: QA-control scans, the bundle
// marker patch before signing, raw-EXE and installer Authenticode, the NSIS embedding verification, Developer ID
// signing of the .app and its nested code, app notarization before packing, the signed DMG and its notarization.
// The standalone recipes (win-package-recipe.mjs, mac-package-recipe.mjs) hold the same steps for direct callers.
import path from "node:path";
import { validateWinPackage, WinPackageConfigError } from "./win-package-recipe.mjs";

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

/** `targets.win.winPackage` → prePackage (`tauri build --no-bundle`) and package (`tauri bundle --bundles nsis`). */
export function expandWinPackageTarget(target, { appRoot, config = {} }) {
  if (!target?.winPackage) return withoutRecipe(target);
  if (target.prePackage !== undefined) fail("targets.win.winPackage replaces prePackage; remove the prePackage script");
  if (target.winPackage.sidecars !== undefined) fail("targets.win.winPackage.sidecars is not supported in a right-release target: declare sidecars through externalBin or mediaSidecar, which release.mjs signs with the raw EXE");
  let plan;
  try { plan = validateWinPackage({ appRoot, config, target }); } catch (error) {
    if (error instanceof WinPackageConfigError) fail(`targets.win.winPackage: ${(error.issues ?? [error.message]).join("; ")}`);
    throw error;
  }
  if (target.signingContract !== undefined && target.signingContract !== "windows-raw-exe-authenticode-before-nsis-v1") fail(`targets.win.winPackage builds an NSIS installer; signingContract must be windows-raw-exe-authenticode-before-nsis-v1, got ${target.signingContract}`);
  const cwd = rel(plan.appRoot, plan.tauriCwd) || ".";
  const override = { bundle: { createUpdaterArtifacts: false, windows: { nsis: { installMode: plan.perMachine ? "perMachine" : "currentUser" } } } };
  // `recipe` is set only here: a hand-written one is dropped so it can never select a recipe path.
  const { winPackage, recipe: _ignoredWin, ...rest } = target;
  return {
    ...rest,
    signingContract: "windows-raw-exe-authenticode-before-nsis-v1",
    prePackage: { cmd: "pnpm", args: ["exec", "tauri", "build", "--no-bundle", "--target", plan.triple], cwd, env: QUIET_TAURI_ENV },
    package: { cmd: "pnpm", args: ["exec", "tauri", "bundle", "--bundles", "nsis", "--target", plan.triple, "--config", JSON.stringify(override)], cwd, env: QUIET_TAURI_ENV },
    sign: { ...(target.sign ?? {}), prePackageFiles: [rel(plan.appRoot, plan.rawExe)], files: [rel(plan.appRoot, plan.installerFile)] },
    recipe: { kind: "winPackage", productName: plan.productName, updater: plan.updater },
  };
}

const MAC_KEYS = new Set(["universal", "tauriCwd", "features"]);

/**
 * `targets.mac.macPackage` → prePackage (`tauri build --bundles app`, universal by default), the .app as the
 * Developer ID signing set, app notarization before packing, and right-release's own signed DMG from the stapled app.
 * The app path and DMG path are `notarize.app` and `notarize.file`; DMG branding is `targets.mac.dmg`.
 */
export function expandMacPackageTarget(target, { appRoot }) {
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
  const cwd = spec.tauriCwd ?? ".";
  if (typeof cwd !== "string" || path.isAbsolute(cwd) || cwd.split(/[\\/]/).includes("..")) fail("targets.mac.macPackage.tauriCwd must be a relative path inside the app");
  // Tauri's universal-apple-darwin target builds both slices and lipos them itself.
  const triple = spec.universal === false ? (process.arch === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin") : "universal-apple-darwin";
  const override = { bundle: { createUpdaterArtifacts: false, macOS: { signingIdentity: null } } };
  const args = ["exec", "tauri", "build", "--target", triple, "--bundles", "app", "--no-sign", "--config", JSON.stringify(override)];
  if (spec.features?.length) args.push("--features", spec.features.join(","));
  const prePackageFiles = target.sign?.prePackageFiles ?? [target.notarize.app];
  if (!prePackageFiles.some((entry) => (typeof entry === "string" ? entry : entry?.file) === target.notarize.app)) fail("targets.mac.macPackage: sign.prePackageFiles must include notarize.app");
  const { macPackage, recipe: _ignoredMac, ...rest } = target;
  return {
    ...rest,
    prePackage: { cmd: "pnpm", args, cwd, env: QUIET_TAURI_ENV },
    sign: { ...(target.sign ?? {}), prePackageFiles },
    notarize: { ...target.notarize, appBeforePackage: true },
    dmg: isObject(target.dmg) ? target.dmg : {},
    recipe: { kind: "macPackage", universal: triple === "universal-apple-darwin" },
  };
}

/** Expand whichever declarative package key the target carries. */
export function expandPackageRecipeTarget(target, { platform, appRoot, config }) {
  if (platform === "win") return expandWinPackageTarget(target, { appRoot, config });
  if (platform === "mac") return expandMacPackageTarget(target, { appRoot, config });
  if (target?.winPackage || target?.macPackage) fail(`winPackage / macPackage are not valid on ${platform}`);
  return target;
}
