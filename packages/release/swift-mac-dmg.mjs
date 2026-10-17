// SwiftPM macOS DMG ordering. With `notarize.appBeforePackage` the .app is notarized and stapled first;
// right-release then builds the DMG itself (dmg-build.mjs, as Tauri does) so it packs the stapled app,
// signs it, and the existing archive notarization staples the DMG.
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { buildDmg } from "./dmg-build.mjs";
import { readDmgSpec } from "./dmg-spec.mjs";

const fail = (code, message) => Object.assign(new Error(message), { code });
const DMG_HOOK = /dmg|hdiutil/i;

/** True when the prePackage hook looks like it builds a DMG (heuristic on command and arguments). */
export const prePackageBuildsDmg = (prePackage) => Boolean(prePackage?.cmd) && DMG_HOOK.test([prePackage.cmd, ...(prePackage.args ?? [])].join(" "));

/** Config error for swiftpm mac targets, or null. */
export function swiftMacDmgConfigError({ target, projectType, appName = "app" }) {
  if (projectType !== "swiftpm" || !target?.notarize?.appBeforePackage) return null;
  if (prePackageBuildsDmg(target.prePackage)) {
    return `${appName} mac prePackage builds a DMG while notarize.appBeforePackage is on, so the DMG would pack the unstapled .app. Drop DMG creation from prePackage: right-release builds the DMG after the .app is notarized and stapled (configure targets.mac.dmg for volname/background), or set targets.mac.dmg: false and build it in targets.mac.package.`;
  }
  if (target.dmg !== false && !target.package?.cmd && !String(target.notarize.file ?? "").endsWith(".dmg")) {
    return `${appName} mac notarize.file must be a .dmg when right-release builds the DMG (notarize.appBeforePackage with no targets.mac.package)`;
  }
  return null;
}

/** Plan for the automatic DMG, or null when the app config builds its own (package step, dmg: false, or no appBeforePackage). */
export function swiftMacDmgPlan({ target, projectType, appName = "app", resolve = (p) => path.resolve(p) }) {
  // SwiftPM apps, and Tauri apps whose targets.mac.macPackage expanded (package-recipe-targets.mjs, #345).
  if ((projectType !== "swiftpm" && target?.recipe?.kind !== "macPackage") || !target?.notarize?.appBeforePackage || target.dmg === false || target.package?.cmd) return null;
  const d = target.dmg ?? {};
  return {
    app: resolve(target.notarize.app),
    out: resolve(d.out ?? target.notarize.file),
    // `spec` is an appdmg-style JSON file (dmg-spec.mjs); explicit options here win over it, and its title is the volume name.
    fallbackVolname: appName,
    options: { volname: d.volname ?? (d.spec ? undefined : appName), spec: d.spec && resolve(d.spec), background: d.background && resolve(d.background), iconSize: d.iconSize, appPosition: d.appPosition,
      applicationsPosition: d.applicationsPosition, window: d.window, volumeIcon: d.volumeIcon && resolve(d.volumeIcon) },
  };
}

const definedOnly = (o) => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined));

/**
 * Build the DMG from the stapled app, then sign it. Refuses to pack an app whose notarization ticket is not
 * stapled. Injectable: run (stapler), build, signDmg, log.
 */
export async function buildStapledSwiftMacDmg(plan, { run = spawnSync, build = buildDmg, signDmg, log = console.log, remove = () => {} } = {}) {
  const probe = run("xcrun", ["stapler", "validate", plan.app], { encoding: "utf8" });
  if (probe.error || probe.status !== 0) throw fail("SWIFT_DMG_APP_NOT_STAPLED", `refusing to build the DMG: ${plan.app} is not stapled (xcrun stapler validate failed). Notarize and staple the app before packaging.`);
  remove(plan.out);
  log(`right-release: building macOS DMG from stapled app ${plan.app}`);
  let options = definedOnly(plan.options);
  if (options.spec && !options.volname) options = { ...options, volname: readDmgSpec(options.spec, { app: plan.app }).volname ?? plan.fallbackVolname };
  const built = build({ app: plan.app, out: plan.out, ...options });
  if (signDmg) await signDmg(plan.out);
  return built;
}
