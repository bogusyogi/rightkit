// The Windows package-identity (sparse MSIX) producer: the pure half. An app declares it in its release config as
//
//   windows: { identity: {
//     manifest: "src-tauri/windows/identity/AppxManifest.xml",     // checked-in template; only Identity/@Version is stamped
//     out: "src-tauri/windows/identity/dist",                       // gets <package> and external-assets/Assets/*; must lie under a declared compile input
//     package: "HeardRight.Identity.msix",
//     version: "0.1.33",                                            // optional; default is the config's `version` (3 or 4 numeric parts)
//     assets: {
//       files: [{ from: "src-tauri/icons/StoreLogo.png", to: "HeardRightStoreLogo.png" }, ...],   // into the package AND external-assets/Assets
//       scaled: { svg: "src-tauri/icons/heardright-icon.svg", sizes: [16, 20, 24], name: "HeardRightSquare44x44Logo.targetsize-{size}_altform-unplated.png" },
//     },
//   } }
//
// The gate runs it (windows-identity.mjs) inside the door's snapshot before the compile, on Windows only. This module holds no I/O of its own
// beyond config text matching: validation, version stamping, the step order, the expected package contents, and the write attribution.

import path from "node:path";
import { diffEntries } from "./stage-ledger.mjs";

export const WINDOWS_IDENTITY_KEYS = Object.freeze(["manifest", "out", "package", "version", "assets"]);
export const WINDOWS_IDENTITY_STEPS = Object.freeze(["stage-manifest", "stage-assets", "render-scaled", "makepri-config", "makepri-new", "pack", "unpack-roundtrip", "verify-roundtrip", "verify-pri", "sign", "cleanup"]);
/** Scratch lives under `out` and is removed before the gate walks it. */
export const WORK_DIR = ".identity-work";

const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const posix = (v) => String(v).replaceAll("\\", "/");
const insideApp = (v) => typeof v === "string" && v.trim() !== "" && !path.isAbsolute(v) && !/^[A-Za-z]:/.test(v) && !posix(v).split("/").includes("..");
const flatName = (v, ext) => typeof v === "string" && /^[A-Za-z0-9._{}-]+$/.test(v) && v.toLowerCase().endsWith(ext) && !v.startsWith(".");

/** Does this config text declare `windows.identity`? Text match like committedWebBuildConfigs: a `.mjs` config is code and is never imported by the gate. */
export const declaresWindowsIdentity = (text) => /(^|[\s,{])["']?windows["']?\s*:\s*\{[^]*?["']?identity["']?\s*:\s*\{/.test(String(text));

/** The release configs of `commit` under `appDir` that declare `windows.identity`, read from `base` (the commit's config decides, never the checkout's). */
export function committedWindowsIdentityConfigs({ readFile, listConfigs, base }) {
  return listConfigs.filter((f) => { try { return declaresWindowsIdentity(readFile(path.join(base, f))); } catch { return false; } });
}

/** `x.y.z` or `x.y.z.w` numeric (each <= 65535) to the four-part MSIX version; anything else (a prerelease suffix) is refused. */
export function toMsixVersion(version) {
  const parts = String(version ?? "").split(".");
  if ((parts.length !== 3 && parts.length !== 4) || !parts.every((p) => /^\d{1,5}$/.test(p) && Number(p) <= 65535)) {
    throw Object.assign(new Error(`version "${version}" is not 3 or 4 numeric segments (<=65535); cannot derive an MSIX package version (no prerelease or build suffix)`), { code: "RK_WINDOWS_IDENTITY" });
  }
  return (parts.length === 4 ? parts : [...parts, "0"]).join(".");
}

/** Stamp the real version into the template's `<Identity>` element only; returns `{ xml, name, publisher }`. */
export function stampManifest(xml, msixVersion) {
  const tag = String(xml).match(/<Identity\b[^>]*\/>/)?.[0];
  if (!tag) throw Object.assign(new Error("the manifest has no <Identity ... /> element"), { code: "RK_WINDOWS_IDENTITY" });
  if (!/\bVersion="[^"]*"/.test(tag)) throw Object.assign(new Error("the manifest <Identity> has no Version attribute"), { code: "RK_WINDOWS_IDENTITY" });
  const name = tag.match(/\bName="([^"]+)"/)?.[1];
  if (!name) throw Object.assign(new Error("the manifest <Identity> has no Name attribute"), { code: "RK_WINDOWS_IDENTITY" });
  return { xml: String(xml).replace(tag, tag.replace(/\bVersion="[^"]*"/, `Version="${msixVersion}"`)), name, publisher: tag.match(/\bPublisher="([^"]*)"/)?.[1] ?? null };
}

/** The declared block, validated. Returns `{ problems, plan }` (plan null with problems). Paths stay app-relative posix. */
export function planWindowsIdentity(config) {
  const problems = [];
  const block = config?.windows?.identity;
  if (!isObject(block)) return { problems: ["the config declares no windows.identity"], plan: null };
  if (isObject(config.windows)) for (const k of Object.keys(config.windows).filter((k) => k !== "identity")) problems.push(`windows has unknown key ${k} (allowed: identity)`);
  for (const k of Object.keys(block).filter((k) => !WINDOWS_IDENTITY_KEYS.includes(k))) problems.push(`windows.identity has unknown key ${k} (allowed: ${WINDOWS_IDENTITY_KEYS.join(", ")})`);
  if (!insideApp(block.manifest)) problems.push("windows.identity.manifest must be a relative path inside the app");
  if (!insideApp(block.out)) problems.push("windows.identity.out must be a relative path inside the app");
  if (!flatName(block.package, ".msix")) problems.push("windows.identity.package must be a flat .msix file name");
  const version = block.version ?? config?.version;
  let msixVersion = null;
  try { msixVersion = toMsixVersion(version); } catch (e) { problems.push(`windows.identity version: ${e.message}`); }
  const files = block.assets?.files;
  if (!Array.isArray(files) || !files.length) problems.push("windows.identity.assets.files must list at least one { from, to } PNG");
  const seen = new Set();
  for (const [i, f] of (Array.isArray(files) ? files : []).entries()) {
    if (!isObject(f) || !insideApp(f.from) || !flatName(f.to, ".png")) { problems.push(`windows.identity.assets.files[${i}] needs from (app-relative) and to (flat .png name)`); continue; }
    if (seen.has(f.to.toLowerCase())) problems.push(`windows.identity.assets.files[${i}].to ${f.to} repeats`);
    seen.add(f.to.toLowerCase());
  }
  const scaled = block.assets?.scaled;
  if (scaled !== undefined) {
    if (!isObject(scaled) || !insideApp(scaled.svg) || !Array.isArray(scaled.sizes) || !scaled.sizes.length || scaled.sizes.some((s) => !Number.isInteger(s) || s < 1 || s > 1024) || !flatName(scaled.name, ".png") || !String(scaled.name).includes("{size}")) {
      problems.push("windows.identity.assets.scaled needs svg (app-relative), sizes (integers 1..1024) and name (flat .png containing {size})");
    } else for (const s of scaled.sizes) if (seen.has(scaled.name.replace("{size}", s).toLowerCase())) problems.push(`windows.identity.assets.scaled name for size ${s} collides with a files entry`);
  }
  if (problems.length) return { problems, plan: null };
  return {
    problems,
    plan: {
      manifest: posix(block.manifest), out: posix(block.out).replace(/\/+$/, ""), package: block.package, msixVersion,
      files: files.map((f) => ({ from: posix(f.from), to: f.to })),
      scaled: scaled ? { svg: posix(scaled.svg), sizes: [...scaled.sizes], name: scaled.name } : null,
    },
  };
}

/** Every package-relative file the staged tree must contain (and the external Assets must mirror, minus the manifest and PRI). */
export const scaledNames = (plan) => (plan.scaled ? plan.scaled.sizes.map((s) => plan.scaled.name.replace("{size}", String(s))) : []);
export const assetNames = (plan) => [...plan.files.map((f) => f.to), ...scaledNames(plan)];
export const expectedPackageFiles = (plan) => ["AppxManifest.xml", "resources.pri", ...assetNames(plan).map((n) => `Assets/${n}`)];
/** The PRI resource URIs the manifest-backed (base) assets must resolve to: the taskbar resolves them, presence alone is not enough. */
export const expectedPriUris = (plan, identityName) => plan.files.map((f) => `ms-resource://${identityName}/Files/Assets/${f.to}`);

/** The ordered steps; `scaled` steps are omitted when no scaled renders are declared. */
export const windowsIdentitySteps = (plan) => WINDOWS_IDENTITY_STEPS.filter((s) => s !== "render-scaled" || plan.scaled);

/**
 * Write attribution: everything the producer added or changed lies under its `out` (which must itself lie under a declared compile input),
 * nothing was removed, and the package exists. `declared` and `out` are snapshot-relative posix paths; `out` is the producer's out dir.
 */
export function identityOutputProblems({ before, after, out, declared, packageFile }) {
  const o = posix(out).replace(/\/+$/, "");
  const roots = declared.map((x) => posix(x).replace(/\/+$/, ""));
  const problems = [];
  if (!roots.some((r) => o === r || o.startsWith(`${r}/`))) problems.push(`windows.identity.out ${o} lies under no declared compile input (declare it in .rightkit/compile-inputs.json)`);
  const d = diffEntries(before, after);
  for (const p of [...d.added, ...d.changed]) if (!(p === o || p.startsWith(`${o}/`))) problems.push(`${p} was written by the Windows identity build but lies outside its out ${o}`);
  for (const p of d.removed) if (!(p === o || p.startsWith(`${o}/`))) problems.push(`${p} was removed by the Windows identity build but lies outside its out ${o}`);
  if (packageFile && !after.some((e) => e.path === `${o}/${packageFile}`)) problems.push(`${o}/${packageFile} was not produced`);
  return problems;
}
