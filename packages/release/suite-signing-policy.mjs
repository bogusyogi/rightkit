// The signing policy of a gate-run test or dev build (rightkit#1303, GATE-CONTRACT "One signing identity per product, every build"). Pure: it reads
// nothing but what its caller hands it, so the gate's route decision (rightkit-gate.mjs routePlan, from the commit's own bytes), the `sign` step
// (suite-signing.mjs, from the snapshot) and the tests all judge the same text.
//
//   - The identity is derived, never declared: the app's tauri.conf.json `identifier` is the one id. Mac install.bundleId must equal it, and a
//     dev or test Tauri `--config` that overrides it is refused. The team is DEFAULT_TEAM, the signer subject WINDOWS_SIGNER_SUBJECT.
//   - A suite that builds a product (tauri build/bundle, or a cargo build of the tauri package or a sidecar package) must be named by a later `sign`
//     step; a Windows dev `install.kind: "tauri"` is refused (`tauri-authenticode` is the signed route); `sign-qa-bundle` and RIGHTKIT_DEV_SIGNING
//     (ad hoc) are refused.
//   - These refusals apply on Mac and Dell only (process.platform darwin or win32). Linux builds sign nothing and hosted CI is exempt.
import path from "node:path";
import { DEFAULT_TEAM, WINDOWS_SIGNER_SUBJECT } from "./signing-identity.mjs";

export const SIGN_TEAM = DEFAULT_TEAM;
export const SIGN_SUBJECT = WINDOWS_SIGNER_SUBJECT;
/** The environment variable a successful `sign` step sets for the suites after it: the signed copy of the first file. */
export const QA_APP_BINARY_ENV = "RIGHTKIT_QA_APP_BINARY";
/** The environment variable a successful `sign` step sets beside it: the path of the signing evidence file (signed files, sha256 before and after, identity). */
export const QA_SIGNING_EVIDENCE_ENV = "RIGHTKIT_QA_SIGNING_EVIDENCE";
export const SIGN_STEP_KEYS = Object.freeze(["name", "sign"]);
export const WINDOWS_TAURI_KIND = "tauri";
export const WINDOWS_SIGNED_KIND = "tauri-authenticode";

export class SuiteSigningError extends Error {
  constructor(code, message, details = {}) {
    super(message);
    this.name = "SuiteSigningError";
    this.code = code;
    Object.assign(this, details);
  }
}
const refuse = (code, message, details) => { throw new SuiteSigningError(code, message, details); };
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const SIGNING_ID = /^[A-Za-z0-9][A-Za-z0-9.-]{0,254}$/;

/** A relative path with no `..`, no drive, no root: the form a sign file path takes inside a receipt's output directory. Returns it with `/`. */
export function containedRelative(value, label) {
  if (typeof value !== "string" || !value.trim()) refuse("SIGN_STEP_PATH", `${label} must be a non-empty relative path`);
  const normalized = value.replaceAll("\\", "/");
  if (path.isAbsolute(value) || path.win32.isAbsolute(value) || /^[A-Za-z]:/.test(value) || normalized.startsWith("/") || normalized.split("/").includes("..") || normalized.split("/").includes("")) {
    refuse("SIGN_STEP_PATH", `${label} must be a relative path inside the earlier suite's output directory (no absolute path, no .., no empty segment): ${JSON.stringify(value)}`);
  }
  return normalized;
}

/**
 * Validate one `sign` spec: `{ files: [{ from, path }] }`. `from` names an EARLIER suite of the same list (`earlier` is the set of names declared
 * before this step), `path` is relative to that suite's carried-receipt output directory. Returns the spec with normalized paths.
 */
export function validateSignSpec(sign, label, earlier) {
  if (!isObject(sign)) refuse("SIGN_STEP_SHAPE", `${label} must be an object { files: [{ from, path }] }`);
  const unknown = Object.keys(sign).filter((key) => key !== "files");
  if (unknown.length) refuse("SIGN_STEP_SHAPE", `${label} has unknown key(s): ${unknown.join(", ")} (allowed: files). The identity is derived from the product, never declared here`);
  if (!Array.isArray(sign.files) || sign.files.length === 0) refuse("SIGN_STEP_SHAPE", `${label}.files must be a non-empty list of { from, path }`);
  const seen = new Set();
  const files = sign.files.map((item, index) => {
    const at = `${label}.files[${index}]`;
    if (!isObject(item)) refuse("SIGN_STEP_SHAPE", `${at} must be { from, path }`);
    const extra = Object.keys(item).filter((key) => key !== "from" && key !== "path");
    if (extra.length) refuse("SIGN_STEP_SHAPE", `${at} has unknown key(s): ${extra.join(", ")} (allowed: from, path)`);
    if (typeof item.from !== "string" || !NAME.test(item.from)) refuse("SIGN_STEP_FROM", `${at}.from must name an earlier suite`);
    if (!earlier.has(item.from)) refuse("SIGN_STEP_FROM", `${at}.from ${JSON.stringify(item.from)} is not a suite declared before this step (${[...earlier].join(", ") || "none"}); a sign step signs what an earlier suite built`);
    const rel = containedRelative(item.path, `${at}.path`);
    const key = `${item.from}\0${rel}`;
    if (seen.has(key)) refuse("SIGN_STEP_SHAPE", `${at} repeats ${item.from}:${rel}`);
    seen.add(key);
    return { from: item.from, path: rel };
  });
  return { files };
}

// ---- what a step is -------------------------------------------------------------------------------------------------------------

const base = (program) => path.basename(String(program ?? "").replaceAll("\\", "/")).replace(/\.(exe|cmd|bat)$/i, "").toLowerCase();
/** A config word as text: a string, or a typed `{ "$json": value }` template (the raw config a commit holds) as the JSON it expands to. */
const wordText = (word) => (typeof word === "string" ? word : isObject(word) && Object.hasOwn(word, "$json") ? JSON.stringify(word.$json) : JSON.stringify(word));

/** The words a step runs: its program, then its arguments, as text. */
export function stepWords(step) {
  const own = step?.node !== undefined ? ["node", String(step.node)] : step?.cmd !== undefined ? [String(step.cmd)] : [];
  return [...own, ...(Array.isArray(step?.args) ? step.args.map(wordText) : [])];
}

/** A suite entry merged with its runner's command and arguments (enough to read what it runs; journeys and env are not needed here). */
export function suiteStep(entry, runners = {}) {
  const runner = typeof entry.runner === "string" && isObject(runners[entry.runner]) ? runners[entry.runner] : {};
  return {
    cmd: entry.cmd ?? (entry.node === undefined ? runner.cmd : undefined),
    node: entry.node ?? (entry.cmd === undefined ? runner.node : undefined),
    args: [...(runner.args ?? []), ...(entry.args ?? [])],
    env: { ...(runner.env ?? {}), ...(entry.env ?? {}) },
  };
}

const firstWord = (words) => words.find((word) => !word.startsWith("-") && !word.startsWith("+"));

/** The package names and manifest paths a cargo build step names: `-p x`, `--package x`, `--package=x`, `--manifest-path p`. */
function cargoTargets(words) {
  const packages = []; const manifests = [];
  words.forEach((word, i) => {
    if (word === "-p" || word === "--package") packages.push(words[i + 1]);
    else if (word.startsWith("--package=")) packages.push(word.slice("--package=".length));
    else if (word === "--manifest-path") manifests.push(words[i + 1]);
    else if (word.startsWith("--manifest-path=")) manifests.push(word.slice("--manifest-path=".length));
  });
  return { packages: packages.filter(Boolean), manifests: manifests.filter(Boolean) };
}

/** The `name` of a Cargo manifest's [package] table, or null. A line scan, not a TOML parser: the route runs from an installed package with no dependencies. */
export function cargoPackageName(text) {
  let inPackage = false;
  for (const line of String(text).split(/\r?\n/)) {
    const table = /^\s*\[([^\]]*)\]\s*(?:#.*)?$/.exec(line);
    if (table) { inPackage = table[1].trim() === "package"; continue; }
    const named = inPackage ? /^\s*name\s*=\s*"([^"]+)"\s*(?:#.*)?$/.exec(line) : null;
    if (named) return named[1];
  }
  return null;
}

/** `{ packages, manifests }`: what counts as a product for a cargo build (the Tauri package and every sidecar package). */
export function productTargets({ config = {}, readFile = () => null, devPlatform = null } = {}) {
  const manifests = new Set(); const packages = new Set();
  const norm = (p) => path.posix.normalize(String(p).replaceAll("\\", "/"));
  const installs = Object.entries(config?.development?.targets ?? {}).filter(([name]) => devPlatform === null || name === devPlatform).map(([, target]) => target?.install).filter(isObject);
  const tauriManifests = new Set([...installs.map((install) => install.manifestPath).filter((p) => typeof p === "string"), "src-tauri/Cargo.toml"]);
  for (const manifest of tauriManifests) {
    manifests.add(norm(manifest));
    const text = readFile(manifest);
    const name = typeof text === "string" ? cargoPackageName(text) : null;
    if (name) packages.add(name);
  }
  const sidecars = Array.isArray(config?.sidecars?.sidecars) ? config.sidecars.sidecars : Array.isArray(config?.sidecars) ? config.sidecars : [];
  for (const sidecar of sidecars) {
    if (!isObject(sidecar)) continue;
    if (typeof sidecar.package === "string") packages.add(sidecar.package);
    if (typeof sidecar.manifestPath === "string") manifests.add(norm(sidecar.manifestPath));
  }
  return { packages, manifests };
}

/**
 * Whether a step builds a product: a Tauri `build` or `bundle`, or a cargo build (`build`, `b`, `rustc`, `install`) of the Tauri package or a
 * sidecar package. A cargo build that names a package (`-p`, `--package`, `--manifest-path`) that is neither is not one; one that names none
 * builds the workspace's defaults, which include the product, so it is.
 */
export function isProductBuildStep(step, targets = { packages: new Set(), manifests: new Set() }) {
  const words = stepWords(step);
  if (!words.length) return false;
  const program = base(words[0]);
  const rest = words.slice(1);
  // The Tauri CLI by any spelling a step uses: `tauri` (pnpm exec), `cargo-tauri`, or its script (`node node_modules/@tauri-apps/cli/tauri.js`).
  const isTauriWord = (word) => word === "tauri" || base(word) === "cargo-tauri" || /(^|\/)tauri\.js$/.test(String(word).replaceAll("\\", "/"));
  const tauriAt = rest.findIndex(isTauriWord);
  if (program === "cargo-tauri" || tauriAt >= 0) {
    const after = program === "cargo-tauri" ? rest : rest.slice(tauriAt + 1);
    const command = firstWord(after);
    if (command === "build" || command === "bundle") return true;
  }
  const cargoWords = program === "rustright" && base(rest[0]) === "cargo" ? rest.slice(1) : program === "cargo" ? rest : null;
  if (cargoWords) {
    const command = firstWord(cargoWords);
    if (!["build", "b", "rustc", "install"].includes(command)) return false;
    const named = cargoTargets(cargoWords);
    if (!named.packages.length && !named.manifests.length) return true;
    const norm = (p) => path.posix.normalize(String(p).replaceAll("\\", "/"));
    return named.packages.some((name) => targets.packages.has(name)) || named.manifests.some((manifest) => targets.manifests.has(norm(manifest)));
  }
  return false;
}

/** Why a step asks for ad hoc signing (the QA bundle signer, or the ad hoc opt-in), or null. */
export function adHocReason(step) {
  const words = stepWords(step);
  const bad = words.find((word) => /sign-qa-bundle/.test(word));
  if (bad !== undefined) return `${JSON.stringify(bad)} runs the ad hoc QA bundle signer (sign-qa-bundle)`;
  const named = words.find((word) => /RIGHTKIT_DEV_SIGNING/.test(word));
  if (named !== undefined) return `${JSON.stringify(named)} names RIGHTKIT_DEV_SIGNING`;
  if (isObject(step?.env) && Object.keys(step.env).some((key) => key === "RIGHTKIT_DEV_SIGNING")) return "its env sets RIGHTKIT_DEV_SIGNING (ad hoc signing)";
  return null;
}

/** The Tauri `--config`/`-c` values of a step (inline JSON text or a path). */
export function tauriConfigValues(step) {
  const words = stepWords(step);
  const values = [];
  words.forEach((word, i) => {
    if (word === "--config" || word === "-c") { if (words[i + 1] !== undefined) values.push(words[i + 1]); }
    else if (word.startsWith("--config=")) values.push(word.slice("--config=".length));
  });
  return values;
}

/** True when a parsed Tauri config (or merge patch) sets `identifier`. */
const setsIdentifier = (value) => isObject(value) && Object.hasOwn(value, "identifier");

/** Why a step's Tauri `--config` overrides the identifier, or null. `readFile(path)` reads an app-relative file's text (null when it cannot). */
export function identifierOverrideReason(step, readFile = () => null) {
  for (const value of tauriConfigValues(step)) {
    const text = String(value).trim();
    let parsed;
    if (text.startsWith("{")) {
      try { parsed = JSON.parse(text); } catch { return `a --config value is JSON that does not parse, so it cannot be shown not to set the identifier: ${text.slice(0, 60)}`; }
    } else {
      const body = readFile(text);
      if (typeof body !== "string") return `the --config file ${JSON.stringify(text)} cannot be read, so it cannot be shown not to set the identifier`;
      try { parsed = JSON.parse(body); } catch { return `the --config file ${JSON.stringify(text)} is not JSON, so it cannot be shown not to set the identifier`; }
    }
    if (setsIdentifier(parsed)) return `a --config sets the bundle identifier (${JSON.stringify(parsed.identifier)}); a dev or test build carries the product's one identifier`;
  }
  return null;
}

// ---- the product's one identifier -----------------------------------------------------------------------------------------------

/**
 * The product's bundle identifier: `identifier` of tauri.conf.json beside the Tauri manifest. A platform file (tauri.macos.conf.json,
 * tauri.windows.conf.json) that sets a different one is refused. `readFile(path)` reads an app-relative file's text (null when absent).
 */
export function productIdentifier(options) { return productIdentity(options).identifier; }

/** The identifier plus the product's declared main binary name (tauri.conf.json `mainBinaryName`, or null when it declares none). */
export function productIdentity({ readFile, manifestPath = "src-tauri/Cargo.toml" }) {
  const dir = path.posix.dirname(String(manifestPath).replaceAll("\\", "/"));
  const at = (name) => (dir === "." ? name : `${dir}/${name}`);
  const text = readFile(at("tauri.conf.json"));
  if (typeof text !== "string") refuse("SIGN_IDENTIFIER_UNREADABLE", `${at("tauri.conf.json")} cannot be read: the product's identifier is derived from it`);
  let identifier; let mainBinaryName = null;
  try { const parsed = JSON.parse(text); identifier = parsed.identifier; if (typeof parsed.mainBinaryName === "string" && parsed.mainBinaryName.trim()) mainBinaryName = parsed.mainBinaryName.trim(); } catch { refuse("SIGN_IDENTIFIER_UNREADABLE", `${at("tauri.conf.json")} is not JSON`); }
  if (typeof identifier !== "string" || !SIGNING_ID.test(identifier)) refuse("SIGN_IDENTIFIER_UNREADABLE", `${at("tauri.conf.json")} has no usable identifier (letters, digits, '.' and '-'): ${JSON.stringify(identifier)}`);
  for (const name of ["tauri.macos.conf.json", "tauri.windows.conf.json", "tauri.linux.conf.json"]) {
    const platform = readFile(at(name));
    if (typeof platform !== "string") continue;
    let other;
    try { other = JSON.parse(platform)?.identifier; } catch { refuse("SIGN_IDENTIFIER_UNREADABLE", `${at(name)} is not JSON`); }
    if (other !== undefined && other !== identifier) refuse("SIGN_IDENTIFIER_OVERRIDE", `${at(name)} sets identifier ${JSON.stringify(other)}, not the product's ${JSON.stringify(identifier)}`);
  }
  return { identifier, mainBinaryName };
}

/** The mac dev install must name the same id: `install.bundleId`, when set, equals the Tauri identifier. */
export function assertBundleIdMatches(config, identifier) {
  const bundleId = config?.development?.targets?.mac?.install?.bundleId;
  if (bundleId !== undefined && bundleId !== identifier) refuse("SIGN_IDENTIFIER_MISMATCH", `development.targets.mac.install.bundleId ${JSON.stringify(bundleId)} differs from the tauri.conf.json identifier ${JSON.stringify(identifier)}; one id per product, derived from tauri.conf.json`);
}

// ---- the gate's refusals --------------------------------------------------------------------------------------------------------

/** The codes of the gate's signing refusals (the route probe lists them beside its own). */
export const SIGNING_REFUSAL_CODES = Object.freeze(["RK_SUITE_UNSIGNED_BUILD", "RK_SUITE_ADHOC_SIGNING", "RK_SUITE_IDENTIFIER_OVERRIDE", "RK_SUITE_IDENTIFIER_MISMATCH", "RK_DEV_WIN_UNSIGNED_INSTALL"]);

/** Hosts these refusals apply to: the Mac and the Dell. Linux and hosted CI are exempt. */
export const signingRefusalHost = (platform, env = {}) => (platform === "darwin" || platform === "win32") && env.GITHUB_ACTIONS !== "true";

/**
 * Every signing refusal for an app's config on this host, `[{ code, message }]` (empty when the host is exempt or the config is clean).
 * `to` is the stage (`dev`, `test`); `devPlatform` the `--platform` of a dev run (default: the host's); `readFile(rel)` reads an app-relative file of the
 * commit (text, or null).
 */
export function suiteSigningRefusals({ config, platform, env = {}, to, devPlatform = null, readFile = () => null }) {
  if (!signingRefusalHost(platform, env) || !isObject(config)) return [];
  const out = [];
  const add = (code, message) => out.push({ code, message });
  const hostDev = platform === "win32" ? "win" : "mac";
  const dev = devPlatform ?? hostDev;
  const targets = productTargets({ config, readFile, devPlatform: null });
  const runners = isObject(config.tests?.runners) ? config.tests.runners : {};
  const suites = Array.isArray(config.tests?.suites) ? config.tests.suites.filter(isObject) : [];
  // Suites run by `right-release test`; development builds run by `right-release dev`.
  const judged = to === "dev" ? [] : suites;
  const signs = [];
  judged.forEach((entry, index) => {
    if (entry.sign !== undefined) {
      const files = Array.isArray(entry.sign?.files) ? entry.sign.files : [];
      signs.push({ index, from: new Set(files.filter(isObject).map((file) => file.from)) });
    }
  });
  judged.forEach((entry, index) => {
    if (entry.sign !== undefined) return;
    const step = suiteStep(entry, runners);
    const label = `tests.suites[${index}] (${entry.name ?? "?"})`;
    const adhoc = adHocReason(step);
    if (adhoc) add("RK_SUITE_ADHOC_SIGNING", `${label}: ${adhoc}; a ${platform === "darwin" ? "Mac" : "Dell"} build is signed with the product's one identity (Developer ID, team ${SIGN_TEAM}; Azure Artifact Signing ${SIGN_SUBJECT}), never ad hoc`);
    if (isProductBuildStep(step, targets)) {
      const override = identifierOverrideReason(step, readFile);
      if (override) add("RK_SUITE_IDENTIFIER_OVERRIDE", `${label}: ${override}`);
      if (!signs.some((sign) => sign.index > index && sign.from.has(entry.name))) add("RK_SUITE_UNSIGNED_BUILD", `${label} builds a product and no later { sign: { files: [{ from: ${JSON.stringify(entry.name)}, path }] } } step names it; a ${platform === "darwin" ? "Mac" : "Dell"} test build is signed with the product's one identity before anything runs it`);
    }
  });
  // Development targets (`right-release dev`).
  const target = config.development?.targets?.[dev];
  if (target && (to === "dev" || to === undefined || to === null)) {
    const install = isObject(target.install) ? target.install : {};
    (Array.isArray(target.build) ? target.build : []).forEach((step, index) => {
      if (!isObject(step)) return;
      const label = `development.targets.${dev}.build[${index}]`;
      const adhoc = adHocReason(step);
      if (adhoc) add("RK_SUITE_ADHOC_SIGNING", `${label}: ${adhoc}`);
      const override = isProductBuildStep(step, targets) ? identifierOverrideReason(step, readFile) : null;
      if (override) add("RK_SUITE_IDENTIFIER_OVERRIDE", `${label}: ${override}`);
    });
    if (install.signing !== undefined) add("RK_SUITE_ADHOC_SIGNING", `development.targets.${dev}.install.signing ${JSON.stringify(install.signing)} asks for ad hoc signing; a ${platform === "darwin" ? "Mac" : "Dell"} dev build is signed with the product's one identity`);
    if (dev === "win" && install.kind === WINDOWS_TAURI_KIND) add("RK_DEV_WIN_UNSIGNED_INSTALL", `development.targets.win.install.kind "tauri" installs an unsigned installer; use "${WINDOWS_SIGNED_KIND}" (build --no-bundle, sign the exe, bundle, sign the installer, install)`);
    if (dev === "mac" && install.bundleId !== undefined) {
      try { assertBundleIdMatches(config, productIdentifier({ readFile, manifestPath: install.manifestPath ?? "src-tauri/Cargo.toml" })); }
      catch (error) { if (!(error instanceof SuiteSigningError)) throw error; add("RK_SUITE_IDENTIFIER_MISMATCH", error.message); }
    }
  }
  return out;
}

/**
 * The same policy at run time, on the config a run actually loaded, whatever its format (right-release.config.json or a legacy right-release.config.mjs).
 * The route probe (routePlan) reads only the commit's JSON config, so a .mjs config is enforced here: the suite runner before any suite runs and the
 * dev runner before a target builds. Throws SuiteSigningError with the first refusal's code (RK_SUITE_*); a no-op off the Mac and the Dell.
 */
export function assertSuiteSigningPolicy({ config, platform = process.platform, env = process.env, to, devPlatform = null, readFile = () => null }) {
  const [first] = suiteSigningRefusals({ config, platform, env, to, devPlatform, readFile });
  if (first) refuse(first.code, first.message);
}
