// `msix` target schema for right-release configs (targets.win.msix) and the
// `windows-msix-signed-v1` signing contract. Pure validation; no I/O.
//
// targets.win = {
//   signed: true, signingContract: "windows-msix-signed-v1",
//   projectType: "swiftpm",                      // optional; auto-detected from Package.swift
//   prePackage: { cmd, args },                   // SwiftPM build + stage (swift-package windows-stage)
//   msix: {
//     stageDir, out,                             // flat payload dir produced by prePackage; .msix path
//     identity: { name, publisher, publisherDisplayName, displayName, description?, version, architecture, executable },
//     capabilities: ["internetClient"], minWindowsVersion?, maxWindowsVersionTested?,
//     virtualization: { fileSystemWrite: "enabled|disabled", registryWrite: "enabled|disabled" },  // explicit, required
//     assets: { storeLogo, square150, square44 },
//     runtime: { dlls: ["swiftCore.dll", ...], systemDlls?: [], dependencies?: [{ name, minVersion, provides: [] }] },
//     appInstaller?: { uri, msixUri, out, hoursBetweenUpdateChecks?, showPrompt?, updateBlocksActivation? (needs showPrompt), automaticBackgroundTask? (default true) },
//   },
//   updater?: { kind: "tauri" | "appinstaller", artifacts: [...] },   // tauri stays the default
//   installer: { artifacts: [{ file: "<out>.msix", key }] },
// }

export const MSIX_SIGNING_CONTRACT = "windows-msix-signed-v1";
export const UPDATER_KINDS = Object.freeze(["tauri", "appinstaller"]);
export const VIRTUALIZATION_VALUES = Object.freeze(["enabled", "disabled"]);

export const isMsixTarget = (target) => target?.signingContract === MSIX_SIGNING_CONTRACT;
export const updaterKind = (target) => target?.updater?.kind ?? "tauri";

const isHttps = (value) => { try { return new URL(value).protocol === "https:"; } catch { return false; } };

const versionedUri = (value, version) => { try { return decodeURIComponent(new URL(value).pathname).includes(version); } catch { return false; } };

/** Returns an array of human-readable problems (empty when valid). */
export function validateMsixTarget(target, app = "app") {
  const e = [];
  const at = (message) => e.push(`${app} win msix: ${message}`);
  if (!isMsixTarget(target)) at(`signingContract must be ${MSIX_SIGNING_CONTRACT}`);
  const m = target?.msix;
  if (!m || typeof m !== "object") { at("msix block is required"); return e; }
  if (!target.prePackage?.cmd) at("prePackage is required (SwiftPM build + payload staging)");
  if (target.sign?.prePackageFiles?.length || target.sign?.files?.length) at("omit sign.prePackageFiles/sign.files: the msix step signs payload PE files, then the package");
  if (!m.stageDir) at("msix.stageDir is required");
  if (!/\.msix$/i.test(m.out ?? "")) at("msix.out must end with .msix");
  const id = m.identity ?? {};
  for (const key of ["name", "publisher", "publisherDisplayName", "displayName", "version", "architecture", "executable"]) if (!id[key]) at(`msix.identity.${key} is required`);
  const v = m.virtualization ?? {};
  for (const key of ["fileSystemWrite", "registryWrite"]) {
    if (!VIRTUALIZATION_VALUES.includes(v[key])) at(`msix.virtualization.${key} must be explicitly "enabled" or "disabled"`);
  }
  for (const key of ["storeLogo", "square150", "square44"]) if (!m.assets?.[key]) at(`msix.assets.${key} (PNG) is required`);
  const dlls = m.runtime?.dlls;
  if (!Array.isArray(dlls)) at("msix.runtime.dlls (explicit runtime dependency inventory; use [] only for a dependency-free app) is required");
  else if (dlls.some((d) => !/\.dll$/i.test(d) || /[\\/]/.test(d))) at("msix.runtime.dlls entries must be flat .dll names");
  const kind = updaterKind(target);
  if (!UPDATER_KINDS.includes(kind)) at(`updater.kind must be one of ${UPDATER_KINDS.join(", ")}`);
  if (kind === "appinstaller") {
    const a = m.appInstaller;
    if (!a) at("updater.kind appinstaller requires msix.appInstaller");
    else {
      if (!isHttps(a.uri)) at("msix.appInstaller.uri must be an https URL");
      if (!isHttps(a.msixUri)) at("msix.appInstaller.msixUri must be an https URL");
      else if (id.version && !versionedUri(a.msixUri, id.version)) at(`msix.appInstaller.msixUri must contain the package version ${id.version} in its path so each release keeps its own install source`);
      if (a.updateBlocksActivation && !a.showPrompt) at("msix.appInstaller.updateBlocksActivation requires showPrompt: true");
      if (!/\.appinstaller$/i.test(a.out ?? "")) at("msix.appInstaller.out must end with .appinstaller");
    }
  }
  const outName = String(m.out ?? "").split(/[\\/]/).pop();
  if (outName && !(target.installer?.artifacts ?? []).some((a) => String(a.file).split(/[\\/]/).pop() === outName)) at("installer.artifacts must list the .msix so it is sealed");
  return e;
}

export function assertValidMsixTarget(target, app) {
  const errors = validateMsixTarget(target, app);
  if (errors.length) throw Object.assign(new Error(errors.join("; ")), { code: "MSIX_CONFIG_INVALID" });
}
