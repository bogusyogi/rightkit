// The Rust half and the web half of the QA surface must not drift (rightkit#1480). One cargo feature, `qa-native`, builds the native QA
// control server; one Vite define, `__RIGHTKIT_QA__` (`rightkitQaDefine()` in @rightkit/config, true only when the dev lane's
// RIGHTKIT_BUILD_MODE=dev), builds the web QA code, which carries the fixed string `RIGHTKIT-QA-WEB-COMPILED`.
//
//   Release lane: the release web scan (`assertNoWebBundleMocks`, qa-control-scan.mjs) refuses the marker, the define and the retired flags in the
//                 frontend bundle, and the binary scans refuse the native control marker, so neither half can ship. Nothing else is needed here.
//   Dev lane:     `devWebQaPairingFindings` runs after the dev build. When the build command enables `qa-native`, an app that declares
//                 `releaseSwitches.webQa: true` (it adopted the shared define) must have a frontend bundle that carries the marker; and a bundle
//                 that carries the marker without `qa-native` in the Cargo features is drift in the other direction, refused for every app.
//                 The app declaration keeps an app that has not adopted the define yet building; it does not weaken the release lane.
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { featuresInTokens, scriptTarget } from "./qa-build-features.mjs";
import { WEB_QA_MARKER, scanWebBundle } from "./qa-control-scan.mjs";
import { commandTokens } from "./release-switch-scan.mjs";

export const NATIVE_QA_FEATURE = "qa-native";

/** Does any of the build `commands` ({ cmd, args, env? } or shell strings) enable cargo feature `feature`, directly or through a package.json script (three levels)? */
export function commandsEnableFeature(commands, { root, feature = NATIVE_QA_FEATURE } = {}) {
  let scripts = {};
  try { scripts = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8")).scripts ?? {}; } catch { /* no package.json: no scripts to follow */ }
  const visit = (command, depth, seen) => {
    const tokens = commandTokens(command);
    if (featuresInTokens(tokens).enabled.includes(feature)) return true;
    if (command && typeof command === "object") {
      for (const [key, value] of Object.entries(command.env ?? {})) if (/FEATURES$/i.test(key) && String(value).split(/[,\s]+/).includes(feature)) return true;
    }
    const script = scriptTarget(tokens);
    if (script && depth < 3 && Object.hasOwn(scripts, script) && !seen.has(script)) return visit(String(scripts[script]), depth + 1, new Set([...seen, script]));
    return false;
  };
  return commands.filter(Boolean).some((command) => visit(command, 0, new Set()));
}

/** The Tauri `frontendDist` bundle directories of an app (absolute), or [] when it has none or serves a URL. */
export function frontendBundleDirectories(root) {
  const confPath = path.join(root, "src-tauri", "tauri.conf.json");
  if (!existsSync(confPath)) return [];
  const build = JSON.parse(readFileSync(confPath, "utf8")).build ?? {};
  const dist = build.frontendDist ?? build.distDir;
  if (typeof dist !== "string" || /^[a-z][a-z0-9+.-]*:\/\//i.test(dist)) return [];
  return [path.resolve(root, "src-tauri", dist)];
}

/** Does any web file under `dirs` carry the web QA marker? Returns { files, carries }. */
export function webBundleCarriesQaMarker(dirs) {
  const { findings, files } = scanWebBundle(dirs, { requireFiles: true });
  return { files, carries: findings.some((finding) => finding.marker === "web-qa-marker"), unscannable: findings.filter((finding) => finding.rule === "unscannable") };
}

/**
 * Pure pairing rule. `nativeQa`: the dev build enables qa-native. `webQa`: the frontend bundle carries the marker. `webQaDeclared`: the app
 * declares releaseSwitches.webQa: true.
 */
export function devWebQaPairingFindings({ nativeQa, webQa, webQaDeclared }) {
  const findings = [];
  if (nativeQa && webQaDeclared && !webQa) findings.push({ rule: "web-qa-missing", reason: `web-qa-missing: the dev build enables ${NATIVE_QA_FEATURE} and the app declares releaseSwitches.webQa, but the frontend bundle does not carry ${WEB_QA_MARKER} (build the bundle with \`define: { ...rightkitQaDefine() }\`, which is true when RIGHTKIT_BUILD_MODE=dev)` });
  if (!nativeQa && webQa) findings.push({ rule: "web-qa-unpaired", reason: `web-qa-unpaired: the frontend bundle carries ${WEB_QA_MARKER} but the build does not enable ${NATIVE_QA_FEATURE}; the web and native QA halves must be built together` });
  return findings;
}

/**
 * The dev-lane pairing check for an app root: reads the build commands' features, the declaration and the built frontend bundle. Apps with no
 * Tauri frontend bundle (native-only apps) have no web half and return no findings.
 */
export function devWebQaPairing({ root, config, commands }) {
  const dirs = frontendBundleDirectories(root).filter((dir) => existsSync(dir));
  if (dirs.length === 0) return { checked: false, findings: [] };
  const declared = config?.releaseSwitches?.webQa;
  if (declared !== undefined && typeof declared !== "boolean") throw new Error(`releaseSwitches.webQa must be true or false, got ${JSON.stringify(declared)}`);
  const bundle = webBundleCarriesQaMarker(dirs);
  const unscannable = bundle.unscannable.map((finding) => ({ rule: "unscannable", reason: `unscannable: ${finding.file}: ${finding.reason}` }));
  const nativeQa = commandsEnableFeature(commands, { root });
  return { checked: true, nativeQa, webQa: bundle.carries, findings: [...unscannable, ...devWebQaPairingFindings({ nativeQa, webQa: bundle.carries, webQaDeclared: declared === true })] };
}
