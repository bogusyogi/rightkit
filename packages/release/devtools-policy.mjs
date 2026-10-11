// Release-side half of the DevTools policy (the runtime half is crates/rightkit-shell/src/devtools.rs).
//
// Owner's fixed design: every Right app has absolutely NO access to DevTools. The single exception is ViewRight,
// which opens HTML documents and may inspect that opened content only, never its own UI or shell.
//
// Rules, applied to every release and update build before anything is built:
//   - `devtools` is refused for every app (qa-build-features.mjs, qa-control-scan.mjs).
//   - Exception: an app whose right-release config declares `devtools: "content-only"` AND is listed in
//     devtools-policy.json (`contentOnlyApps`, today only `viewright`) AND whose Cargo manifest depends on
//     rightkit-shell with the `content-devtools` feature (the only way to select DevToolsPolicy::ContentOnly)
//     AND whose build.rs declares an application ACL manifest (without one Tauri lets local-origin content call
//     every app command, so a content webview would not be IPC-isolated).
//   - A manifest that turns on `content-devtools` without the declaration is refused as well.
//   - The allowance is recorded in the release evidence (the sealed manifest, field `devtools`).
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseCargoFeaturesAndDependencies } from "./qa-build-features.mjs";

// Keep identical to `CONTENT_ONLY_MARKER` in crates/rightkit-shell/src/devtools.rs and qa-control-scan.mjs.
export const CONTENT_ONLY_MARKER = "RIGHTKIT-SHELL-DEVTOOLS-CONTENT-ONLY-COMPILED";
export const DEVTOOLS_POLICY_FILE = path.join(path.dirname(fileURLToPath(import.meta.url)), "devtools-policy.json");
const SHELL_CRATE = "rightkit-shell";
const SHELL_FEATURE = "content-devtools";

export function readContentOnlyApps(file = DEVTOOLS_POLICY_FILE) {
  const parsed = JSON.parse(readFileSync(file, "utf8"));
  if (parsed.schema !== 1 || !Array.isArray(parsed.contentOnlyApps) || parsed.contentOnlyApps.some((a) => typeof a !== "string")) {
    throw new Error(`${file}: expected { schema: 1, contentOnlyApps: [string] }`);
  }
  return Object.freeze([...parsed.contentOnlyApps]);
}

// "denied" | "content-only"; anything else is a configuration error.
export function declaredDevtools(config) {
  const value = config?.devtools;
  if (value === undefined || value === null || value === "denied") return "denied";
  if (value === "content-only") return "content-only";
  throw new Error(`right-release config: devtools must be "denied" or "content-only", got ${JSON.stringify(value)}`);
}

// Does the manifest turn on rightkit-shell's content-devtools (directly or through an app feature it lists)?
export function manifestSelectsContentDevtools(manifestText) {
  const { features, dependencies } = parseCargoFeaturesAndDependencies(manifestText);
  const shell = dependencies.get(SHELL_CRATE);
  if (!shell) return false;
  if (shell.features.includes(SHELL_FEATURE)) return true;
  // An app feature such as `content-devtools = ["rightkit-shell/content-devtools"]` or a `devtools` alias.
  for (const items of features.values()) {
    if (items.some((item) => item === `${SHELL_CRATE}/${SHELL_FEATURE}` || item === `${SHELL_CRATE}?/${SHELL_FEATURE}`)) return true;
  }
  return false;
}

function appManifestDeclared(buildRsText) {
  return /\bAppManifest\b/.test(buildRsText) && /\.commands\s*\(/.test(buildRsText);
}

function readIfExists(file) {
  return existsSync(file) ? readFileSync(file, "utf8") : null;
}

// Returns { mode: "denied" } or { mode: "content-only", app, allowListed, shellFeature, appAclManifest, evidence }.
// Throws (a refusal) when the declaration and the allow-list or the sources disagree. `contentOnlyApps` is injectable
// so a test can prove that any other app declaring the allowance is refused.
export function resolveDevtoolsAllowance({ config, root, contentOnlyApps = readContentOnlyApps() }) {
  const declared = declaredDevtools(config);
  const app = config?.app;
  const manifests = ["src-tauri/Cargo.toml", "Cargo.toml"].map((rel) => ({ rel, text: readIfExists(path.join(root, rel)) })).filter((m) => m.text !== null);
  const selecting = manifests.filter((m) => manifestSelectsContentDevtools(m.text));
  if (declared === "denied") {
    if (selecting.length) {
      throw new Error(`${app ?? "app"}: ${selecting.map((m) => m.rel).join(", ")} enables rightkit-shell \`${SHELL_FEATURE}\` but the right-release config does not declare devtools: "content-only"; no app ships DevTools without that declaration`);
    }
    return { mode: "denied" };
  }
  const problems = [];
  if (typeof app !== "string" || !contentOnlyApps.includes(app)) {
    problems.push(`app \`${app ?? "<unnamed>"}\` is not on the content-only DevTools allow-list (${contentOnlyApps.join(", ") || "empty"}); only ViewRight may inspect opened content, and no Right app may inspect its own UI or shell`);
  }
  if (selecting.length === 0) {
    problems.push(`the Cargo manifest does not depend on ${SHELL_CRATE} with the \`${SHELL_FEATURE}\` feature, so rightkit-shell's DevToolsPolicy::ContentOnly cannot be selected`);
  }
  const buildRs = readIfExists(path.join(root, "src-tauri", "build.rs")) ?? readIfExists(path.join(root, "build.rs"));
  if (buildRs === null || !appManifestDeclared(buildRs)) {
    problems.push("build.rs declares no application ACL manifest (tauri_build AppManifest with commands(..)); without one local content can call every app command");
  }
  if (problems.length) throw new Error(`devtools: "content-only" refused:\n- ${problems.join("\n- ")}`);
  return {
    mode: "content-only",
    app,
    allowListed: true,
    shellFeature: `${SHELL_CRATE}/${SHELL_FEATURE}`,
    appAclManifest: true,
    marker: CONTENT_ONLY_MARKER,
    evidence: `${app} is allow-listed for content-only DevTools (devtools-policy.json); ${selecting[0].rel} selects ${SHELL_CRATE}/${SHELL_FEATURE}; build.rs declares an application ACL manifest`,
  };
}

// What the sealed manifest records.
export function devtoolsEvidence(allowance) {
  return allowance?.mode === "content-only"
    ? { mode: "content-only", app: allowance.app, shellFeature: allowance.shellFeature, appAclManifest: true, marker: allowance.marker, evidence: allowance.evidence }
    : { mode: "denied" };
}

// Re-checked at seal verification and upload: the manifest alone is not trusted, the allow-list is read again.
export function sealedManifestAllowsContentDevtools(manifest, contentOnlyApps = readContentOnlyApps()) {
  const devtools = manifest?.devtools;
  return devtools?.mode === "content-only" && typeof manifest.app === "string" && devtools.app === manifest.app && contentOnlyApps.includes(manifest.app);
}
