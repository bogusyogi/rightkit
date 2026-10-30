// `development.targets.win.install.kind: "tauri-authenticode"` (rightkit#1303): the Windows dev install that signs what it installs. The unsigned
// kind (`tauri`: build the NSIS installer and run it) is refused on the Dell, because a dev build must carry the product's one signing identity
// (Azure Artifact Signing, subject CN=Damned Ventures LLC) exactly as the release does. This kind reuses the release's order
// (package-recipe-targets.mjs winPackage, tauri-bundle-marker.mjs, nsis-payload.mjs, release.mjs) and its signer (sign-windows.mjs):
//   1. build the app without bundling    (`target.build`: a `tauri build ... --no-bundle`; runDevelopment runs it before this module)
//   2. patch the bundle marker           (UNK -> NSS BEFORE signing: Tauri rewrites it during bundling, and doing that to signed bytes breaks Authenticode)
//   3. sign the exe and its sidecars, then verify them (signer subject, timestamp)
//   4. carried `tauri bundle --bundles nsis --debug`   (the installer embeds the exe as signed)
//   5. check the payload                 (the exe inside the installer is byte for byte the signed one)
//   6. sign the installer, then verify it
//   7. install                           (the silent installer, as the unsigned kind runs it)
//   8. verify                            (the installed exe equals the build, and its Authenticode subject is the signer's)
// Nothing installs before step 6 has verified, so no unsigned installer is ever run. Every host effect is a dependency, so the order runs with fakes.
import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { patchTauriBundleType } from "./tauri-bundle-marker.mjs";
import { verifyNsisEmbeddedBinary } from "./nsis-payload.mjs";
import { SIGN_SUBJECT, SuiteSigningError, WINDOWS_SIGNED_KIND, identifierOverrideReason, isProductBuildStep, stepWords } from "./suite-signing-policy.mjs";

export const TAURI_AUTHENTICODE_KIND = WINDOWS_SIGNED_KIND;
/** The steps in the order this module runs them (1 is runDevelopment's build). Exported so a test can prove the order. */
export const TAURI_AUTHENTICODE_ORDER = Object.freeze(["patch-marker", "sign-exe", "bundle", "check-payload", "sign-installer", "install", "verify"]);
const KEYS = new Set(["kind", "productName", "targetTriple", "manifestPath", "expectedInstalledPath", "sidecars", "tauriCwd"]);

export class TauriAuthenticodeError extends SuiteSigningError {
  constructor(code, message, details) { super(code, message, details); this.name = "TauriAuthenticodeError"; }
}
const refuse = (code, message, details) => { throw new TauriAuthenticodeError(code, message, details); };
const sha256 = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");
const relativeInside = (value, label) => {
  const text = String(value ?? "").replaceAll("\\", "/");
  if (!text || path.isAbsolute(text) || path.win32.isAbsolute(text) || /^[A-Za-z]:/.test(text) || text.split("/").includes("..")) refuse("TAURI_AUTHENTICODE_CONFIG", `${label} must be a relative path that stays inside its directory: ${JSON.stringify(value)}`);
  return text;
};

/** Validate the descriptor and return it normalized. No other key is accepted: the identity is derived, so there is nothing to declare it with. */
export function resolveAuthenticodeDescriptor(descriptor, label = "development.targets.win.install") {
  if (!descriptor || typeof descriptor !== "object" || Array.isArray(descriptor)) refuse("TAURI_AUTHENTICODE_CONFIG", `${label} must be an object`);
  const unknown = Object.keys(descriptor).filter((key) => !KEYS.has(key));
  if (unknown.length) refuse("TAURI_AUTHENTICODE_CONFIG", `${label} has unknown key(s): ${unknown.join(", ")} (allowed: ${[...KEYS].join(", ")}). The signer is the product's one identity (${SIGN_SUBJECT}); it is never declared`);
  if (descriptor.kind !== TAURI_AUTHENTICODE_KIND) refuse("TAURI_AUTHENTICODE_CONFIG", `${label}.kind must be ${TAURI_AUTHENTICODE_KIND}`);
  for (const key of ["productName", "targetTriple", "expectedInstalledPath"]) if (typeof descriptor[key] !== "string" || !descriptor[key].trim()) refuse("TAURI_AUTHENTICODE_CONFIG", `${label}.${key} is required`);
  if (!/-pc-windows-msvc$/.test(descriptor.targetTriple)) refuse("TAURI_AUTHENTICODE_CONFIG", `${label}.targetTriple must be a windows msvc triple, got ${descriptor.targetTriple}`);
  if (typeof descriptor.manifestPath !== "string" || !descriptor.manifestPath.trim()) refuse("TAURI_AUTHENTICODE_CONFIG", `${label}.manifestPath is required (the Tauri manifest, to find the build output)`);
  relativeInside(descriptor.manifestPath, `${label}.manifestPath`);
  if (descriptor.tauriCwd !== undefined) relativeInside(descriptor.tauriCwd, `${label}.tauriCwd`);
  const sidecars = descriptor.sidecars ?? [];
  if (!Array.isArray(sidecars)) refuse("TAURI_AUTHENTICODE_CONFIG", `${label}.sidecars must be a list of paths relative to the build's output directory`);
  return { ...descriptor, sidecars: sidecars.map((item, index) => relativeInside(item, `${label}.sidecars[${index}]`)) };
}

/**
 * The build must not bundle: a `tauri build` that also bundles would embed the unsigned exe in the installer. Every Tauri build of the target needs
 * `--no-bundle` and none names `--bundles`; a `--config` that sets the identifier is refused. `readFile(rel)` reads an app-relative file (text or null).
 */
export function validateAuthenticodeBuild(build, label = "development.targets.win.build", readFile = () => null) {
  let builds = 0;
  (Array.isArray(build) ? build : []).forEach((step, index) => {
    if (!isProductBuildStep(step)) return;
    const words = stepWords(step);
    const at = `${label}[${index}]`;
    const tauriBuild = words.some((word) => word === "tauri") && words.includes("build");
    if (words.includes("bundle")) refuse("TAURI_AUTHENTICODE_BUILD", `${at} runs \`tauri bundle\`; the bundle is this install kind's own step 4, after the exe is signed`);
    if (tauriBuild) {
      builds += 1;
      if (!words.includes("--no-bundle") || words.includes("--bundles") || words.some((word) => word.startsWith("--bundles="))) refuse("TAURI_AUTHENTICODE_BUILD", `${at} must be \`tauri build --no-bundle\` (and name no --bundles): bundling here would embed the unsigned exe in the installer`);
    }
    const override = identifierOverrideReason(step, readFile);
    if (override) refuse("TAURI_AUTHENTICODE_BUILD", `${at}: ${override}`);
  });
  if (!builds) refuse("TAURI_AUTHENTICODE_BUILD", `${label} has no \`tauri build --no-bundle\` step: this install kind signs the raw exe the build leaves in its output directory`);
}

/** The carried bundle command (step 4): the same Tauri bundle the release makes, debug profile, no Tauri-side signing (the installer is signed by step 6). */
export function authenticodeBundleCommand(descriptor) {
  const override = { bundle: { createUpdaterArtifacts: false, windows: { signCommand: null } } };
  return { cmd: "pnpm", args: ["exec", "tauri", "bundle", "--bundles", "nsis", "--debug", "--target", descriptor.targetTriple, "--config", JSON.stringify(override)], ...(descriptor.tauriCwd ? { cwd: descriptor.tauriCwd } : {}), name: "tauri bundle" };
}

/**
 * Steps 2 to 8 (step 1, the build, has run). `debugDir` is the build's output directory holding the raw exe; deps:
 *   patchMarker(file) signWindows(files) -> evidence[] verifyWindows(files) -> evidence[] bundle(command) findInstaller() -> path
 *   verifyPayload({ installer, entryName, expectedSha256 }) install(installer) installedExe() -> path
 * Returns `{ order, exe, installer, installed, signing }` where `signing` records the sha256 of each signed file before and after.
 */
export function installTauriAuthenticode({ descriptor: raw, debugDir, deps = {} }) {
  const descriptor = resolveAuthenticodeDescriptor(raw);
  const order = [];
  const step = (name, run) => { order.push(name); return run(); };
  const exe = path.join(debugDir, path.basename(descriptor.expectedInstalledPath));
  if (!existsSync(exe) || !statSync(exe).isFile() || statSync(exe).size === 0) refuse("TAURI_AUTHENTICODE_BUILD_MISSING", `the build left no ${path.basename(exe)} in ${debugDir}`);
  const sidecars = descriptor.sidecars.map((item) => path.resolve(debugDir, item));
  for (const sidecar of sidecars) if (path.relative(debugDir, sidecar).startsWith("..") || !existsSync(sidecar)) refuse("TAURI_AUTHENTICODE_BUILD_MISSING", `sidecar ${sidecar} is not in the build's output directory`);
  const patchMarker = deps.patchMarker ?? ((file) => patchTauriBundleType(file, { bundle: "nsis" }));
  const verifyPayload = deps.verifyPayload ?? verifyNsisEmbeddedBinary;
  const recorded = (evidence, files) => {
    if (!Array.isArray(evidence) || evidence.length !== files.length) refuse("TAURI_AUTHENTICODE_SIGN", `the signer returned ${Array.isArray(evidence) ? evidence.length : "no"} record(s) for ${files.length} file(s)`);
    evidence.forEach((item) => { if (item.subject !== SIGN_SUBJECT || item.authenticode !== "Valid") refuse("TAURI_AUTHENTICODE_SIGN", `${item.file}: signer subject ${JSON.stringify(item.subject)} / ${JSON.stringify(item.authenticode)}, expected ${SIGN_SUBJECT} / Valid`); });
    return evidence;
  };
  // 2. The marker reaches its final value before any byte is signed.
  step("patch-marker", () => patchMarker(exe));
  // 3. Sign and verify the exe and its sidecars.
  const signedFiles = [exe, ...sidecars];
  const signedExe = step("sign-exe", () => {
    const signed = recorded(deps.signWindows(signedFiles), signedFiles);
    recorded(deps.verifyWindows(signedFiles), signedFiles);
    return signed;
  });
  const signedSha = sha256(exe);
  // 4. The carried bundle.
  step("bundle", () => deps.bundle(authenticodeBundleCommand(descriptor)));
  const installer = deps.findInstaller();
  // 5. The installer must embed the signed exe, byte for byte.
  const payload = step("check-payload", () => verifyPayload({ installer, entryName: path.basename(exe), expectedSha256: signedSha }));
  // 6. Sign and verify the installer.
  const signedInstaller = step("sign-installer", () => {
    const signed = recorded(deps.signWindows([installer]), [installer]);
    recorded(deps.verifyWindows([installer]), [installer]);
    return signed;
  });
  // 7. Install (only now), then 8. verify what was installed.
  step("install", () => deps.install(installer));
  const installed = deps.installedExe();
  step("verify", () => recorded(deps.verifyWindows([installed]), [installed]));
  return {
    order, exe, installer, installed, payload,
    signing: { subject: SIGN_SUBJECT, files: [...signedExe, ...signedInstaller].map((item) => ({ file: item.file, sha256Before: item.before?.sha256, sha256After: item.after?.sha256, subject: item.subject })) },
  };
}
