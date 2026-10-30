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
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { neutralIgnored } from "./stage-ledger.mjs";
import { patchTauriBundleType } from "./tauri-bundle-marker.mjs";
import { listNsisPayload, verifyNsisEmbeddedBinary } from "./nsis-payload.mjs";
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

const exeBase = (name) => String(name).replace(/\.exe$/i, "");
const readJson = (readFile, file) => { const text = readFile(file); if (text === null || text === undefined) return null; try { return JSON.parse(text); } catch (error) { return refuse("TAURI_AUTHENTICODE_CONFIG", `${file} is not valid JSON: ${error.message}`); } };
const readText = (file) => (existsSync(file) ? readFileSync(file, "utf8") : null);
const isFile = (file) => existsSync(file) && statSync(file).isFile() && statSync(file).size > 0;

/** The `tauri.conf.json` directory: the Tauri manifest's directory under `appRoot`, else the parent of the build's `target/` ancestor. */
function resolveTauriDir({ descriptor, debugDir, appRoot }) {
  if (appRoot) return path.dirname(path.resolve(appRoot, descriptor.manifestPath));
  let dir = path.resolve(debugDir);
  while (path.dirname(dir) !== dir) { if (path.basename(dir) === "target") return path.dirname(dir); dir = path.dirname(dir); }
  return refuse("TAURI_AUTHENTICODE_CONFIG", `cannot find the Tauri directory from ${debugDir}: pass appRoot`);
}

/** The sidecar names the release's config.sidecars (right-release.config.mjs `sidecars`) stages as Tauri externalBin for Windows. */
export function configSidecarNames(releaseConfig) {
  const raw = releaseConfig?.sidecars;
  const list = Array.isArray(raw) ? raw : Array.isArray(raw?.sidecars) ? raw.sidecars : [];
  const names = [];
  for (const item of list) {
    if (!item || typeof item !== "object" || !["cargo-bin", "command", "remote", "cmake", "prebuilt"].includes(item.kind)) continue;
    if (Array.isArray(item.platforms) && !item.platforms.includes("win")) continue;
    if (item.kind === "command" && item.externalBin === false) continue;
    const name = item.name ?? item.bin ?? item.package;
    if (typeof name === "string" && name) names.push(exeBase(name));
  }
  return names;
}

/**
 * The required sidecar set, derived from `bundle.externalBin` (tauri.windows.conf.json replaces tauri.conf.json's list, as Tauri merges it) and
 * `config.sidecars`. A declared `install.sidecars` list must EQUAL the derived set, refused in either direction; with no list the derived set stands.
 * Each sidecar has up to two copies: the build output (`debugDir/<name>.exe`, tauri-build's copy) and the bundler source
 * (`<tauriDir>/<externalBin>-<triple>.exe`, which `tauri bundle` reads). Both are signed; the payload must match one of them.
 */
export function deriveRequiredSidecars({ descriptor, debugDir, tauriDir, releaseConfig, readFile = readText }) {
  const derived = new Map();
  const add = (name, { bundlerSource, buildOutput }) => {
    const key = name.toLowerCase();
    const entry = derived.get(key) ?? { name, file: `${name}.exe`, candidates: [] };
    for (const [role, file] of [["build-output", buildOutput], ["bundler-source", bundlerSource]]) if (file && !entry.candidates.some((c) => c.path === file)) entry.candidates.push({ role, path: file });
    derived.set(key, entry);
  };
  const base = readJson(readFile, path.join(tauriDir, "tauri.conf.json"));
  const windows = readJson(readFile, path.join(tauriDir, "tauri.windows.conf.json"));
  if (!base && !windows) refuse("TAURI_AUTHENTICODE_CONFIG", `no tauri.conf.json under ${tauriDir}: the sidecar set cannot be derived`);
  const externalBin = windows?.bundle?.externalBin ?? base?.bundle?.externalBin ?? [];
  if (!Array.isArray(externalBin)) refuse("TAURI_AUTHENTICODE_CONFIG", "bundle.externalBin must be a list");
  for (const item of externalBin) {
    const rel = relativeInside(item, "bundle.externalBin entry");
    const name = exeBase(path.posix.basename(rel));
    add(name, { bundlerSource: path.join(tauriDir, ...rel.split("/").slice(0, -1), `${name}-${descriptor.targetTriple}.exe`), buildOutput: path.join(debugDir, `${name}.exe`) });
  }
  for (const name of configSidecarNames(releaseConfig)) add(name, { bundlerSource: path.join(tauriDir, "binaries", `${name}-${descriptor.targetTriple}.exe`), buildOutput: path.join(debugDir, `${name}.exe`) });
  if (descriptor.sidecars.length) {
    const listed = new Map(descriptor.sidecars.map((item) => [exeBase(path.posix.basename(item)).toLowerCase(), item]));
    for (const [key, entry] of derived) if (!listed.has(key)) refuse("TAURI_AUTHENTICODE_SIDECAR_UNLISTED", `sidecar ${entry.name} is derived from bundle.externalBin / config.sidecars but install.sidecars leaves it out`, { sidecar: entry.name });
    for (const [key, item] of listed) if (!derived.has(key)) refuse("TAURI_AUTHENTICODE_SIDECAR_UNDERIVED", `install.sidecars lists ${item}, which neither bundle.externalBin nor config.sidecars declares`, { sidecar: item });
    for (const [key, item] of listed) add(derived.get(key).name, { buildOutput: path.resolve(debugDir, item) });
  }
  const sidecars = [...derived.values()];
  for (const sidecar of sidecars) {
    sidecar.copies = sidecar.candidates.filter((c) => isFile(c.path));
    if (!sidecar.copies.length) refuse("TAURI_AUTHENTICODE_SIDECAR_MISSING", `sidecar ${sidecar.name} is in neither the build output nor the bundler source (${sidecar.candidates.map((c) => c.path).join(", ")})`, { sidecar: sidecar.name });
  }
  return sidecars;
}

const isPlainObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const mergePatch = (base, over) => {
  if (over === undefined) return base;
  if (isPlainObject(base) && isPlainObject(over)) {
    const out = { ...base };
    for (const [key, value] of Object.entries(over)) { if (value === null) delete out[key]; else out[key] = mergePatch(base[key], value); }
    return out;
  }
  return over;
};
const globRegex = (segment) => new RegExp(`^${segment.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".")}$`, "i");
const collectExecutables = (dir, rel = "") => {
  const found = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const next = rel ? `${rel}/${entry.name}` : entry.name;
    if (entry.isDirectory()) found.push(...collectExecutables(path.join(dir, entry.name), next));
    else if (/\.exe$/i.test(entry.name)) found.push({ abs: path.join(dir, entry.name), rel: next });
  }
  return found;
};

/**
 * Every `.exe` the installer ships as a resource, from `bundle.resources` (base and windows config merged as Tauri does): a list of paths, or a map
 * of source to destination. Sources may be a file, a directory (searched recursively) or a glob (`*`, `?`, `**` segments). Only existing files count.
 * Release signs every executable the installer wraps (win-package-recipe.mjs sign-raw), so a resource executable carries the same identity here.
 * Each returned entry is `{ name, file, installRel, copies: [{ role: "resource", path }] }`; `installRel` is its path under the install directory.
 */
export function deriveResourceExecutables({ tauriDir, readFile = readText }) {
  const base = readJson(readFile, path.join(tauriDir, "tauri.conf.json"));
  const windows = readJson(readFile, path.join(tauriDir, "tauri.windows.conf.json"));
  const resources = mergePatch(base?.bundle?.resources, windows?.bundle?.resources);
  if (resources === undefined || resources === null) return [];
  const pairs = Array.isArray(resources)
    ? resources.map((src) => [src, null])
    : isPlainObject(resources) ? Object.entries(resources) : refuse("TAURI_AUTHENTICODE_CONFIG", "bundle.resources must be a list or a map");
  const found = new Map();
  for (const [src, destValue] of pairs) {
    const source = String(src).replaceAll("\\", "/").replace(/^\.\//, "");
    const segments = source.split("/");
    const wild = segments.findIndex((segment) => /[*?]/.test(segment));
    const prefixSegments = wild === -1 ? segments : segments.slice(0, wild);
    const prefix = path.resolve(tauriDir, ...prefixSegments);
    const hits = [];
    const walk = (dir, rest, rel) => {
      if (!existsSync(dir) || !statSync(dir).isDirectory()) return;
      const [head, ...tail] = rest;
      if (head === undefined) { for (const item of collectExecutables(dir)) hits.push({ abs: item.abs, rel: rel ? `${rel}/${item.rel}` : item.rel }); return; }
      if (head === "**") { walk(dir, tail, rel); for (const entry of readdirSync(dir, { withFileTypes: true })) if (entry.isDirectory()) walk(path.join(dir, entry.name), rest, rel ? `${rel}/${entry.name}` : entry.name); return; }
      const matcher = globRegex(head);
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        if (!matcher.test(entry.name)) continue;
        const next = rel ? `${rel}/${entry.name}` : entry.name;
        if (entry.isDirectory()) walk(path.join(dir, entry.name), tail.length ? tail : [], next);
        else if (tail.length === 0 && /\.exe$/i.test(entry.name)) hits.push({ abs: path.join(dir, entry.name), rel: next });
      }
    };
    if (wild !== -1) walk(prefix, segments.slice(wild), "");
    else if (existsSync(prefix) && statSync(prefix).isDirectory()) walk(prefix, [], "");
    else if (/\.exe$/i.test(prefix) && isFile(prefix)) hits.push({ abs: prefix, rel: path.posix.basename(source) });
    const isDir = wild !== -1 || (existsSync(prefix) && statSync(prefix).isDirectory());
    for (const hit of hits) {
      let dest;
      if (destValue === null || destValue === undefined) dest = source.replaceAll("../", "_up_/");
      else if (isDir || String(destValue).endsWith("/")) dest = `${String(destValue).replace(/\/+$/, "")}${destValue === "" ? "" : "/"}${isDir ? hit.rel : path.posix.basename(source)}`;
      else dest = String(destValue);
      dest = dest.replaceAll("\\", "/").replace(/^\.?\//, "");
      if (destValue === null || destValue === undefined) dest = isDir ? `${dest.replace(/\/+$/, "")}/${hit.rel}`.replace(/^\/+/, "") : dest;
      if (!isFile(hit.abs) || found.has(hit.abs.toLowerCase())) continue;
      found.set(hit.abs.toLowerCase(), { name: dest, file: path.posix.basename(dest), installRel: dest, kind: "resource", copies: [{ role: "resource", path: hit.abs }] });
    }
  }
  return [...found.values()];
}

/** `Map(file -> "tracked" | "ignored" | "unignored")` through git: `ls-files --error-unmatch` for tracked, then the commit's own ignore rules (stage-ledger.mjs neutralIgnored). Throws when git cannot answer. */
export function classifyStagedFiles(files, { appRoot, tauriDir }) {
  const out = new Map();
  for (const file of files) {
    const dir = path.dirname(path.resolve(file));
    const top = spawnSync("git", ["-C", dir, "rev-parse", "--show-toplevel"], { encoding: "utf8", windowsHide: true });
    if (top.status !== 0) refuse("TAURI_AUTHENTICODE_TRACKED_RESOURCE", `cannot tell whether ${file} is tracked: git rev-parse failed under ${appRoot ?? tauriDir}`, { file });
    const root = String(top.stdout).trim();
    const rel = path.relative(root, path.resolve(file)).split(path.sep).join("/");
    const tracked = spawnSync("git", ["-C", root, "ls-files", "--error-unmatch", "--", rel], { encoding: "utf8", windowsHide: true });
    if (tracked.status === 0) { out.set(file, "tracked"); continue; }
    if (tracked.status !== 1) refuse("TAURI_AUTHENTICODE_TRACKED_RESOURCE", `cannot tell whether ${file} is tracked: git ls-files exited ${tracked.status}`, { file });
    out.set(file, neutralIgnored({ dir: root, paths: [rel] }).has(rel) ? "ignored" : "unignored");
  }
  return out;
}

/**
 * Steps 2 to 8 (step 1, the build, has run). `debugDir` is the build's output directory holding the raw exe. `appRoot` (to find the Tauri directory;
 * else derived from `debugDir`'s `target/` ancestor) and `releaseConfig` (for config.sidecars) feed the sidecar derivation. deps:
 *   patchMarker(file) signWindows(files) -> evidence[] verifyWindows(files) -> evidence[] bundle(command) findInstaller() -> path
 *   listPayload(installer) -> [{ name }]  verifyPayload({ installer, entryName, expectedSha256 })  install(installer)  installedExe() -> path
 *   classifyStaged(files) -> Map(file -> tracked|ignored|unignored)  installedSidecar(file) -> path (default: next to the installed exe)  readFile(path) -> text | null
 * Returns `{ order, exe, installer, installed, payload, sidecars, signing }`; `signing` records the sha256 of each signed file before and after.
 */
export function installTauriAuthenticode({ descriptor: raw, debugDir, appRoot, releaseConfig, deps = {} }) {
  const descriptor = resolveAuthenticodeDescriptor(raw);
  const order = [];
  const step = (name, run) => { order.push(name); return run(); };
  const exe = path.join(debugDir, path.basename(descriptor.expectedInstalledPath));
  if (!existsSync(exe) || !statSync(exe).isFile() || statSync(exe).size === 0) refuse("TAURI_AUTHENTICODE_BUILD_MISSING", `the build left no ${path.basename(exe)} in ${debugDir}`);
  const tauriDir = resolveTauriDir({ descriptor, debugDir, appRoot });
  const sidecars = [...deriveRequiredSidecars({ descriptor, debugDir, tauriDir, releaseConfig, readFile: deps.readFile }), ...deriveResourceExecutables({ tauriDir, readFile: deps.readFile })];
  const patchMarker = deps.patchMarker ?? ((file) => patchTauriBundleType(file, { bundle: "nsis" }));
  const verifyPayload = deps.verifyPayload ?? verifyNsisEmbeddedBinary;
  const listPayload = deps.listPayload ?? ((installer) => listNsisPayload(installer));
  const sidecarNames = new Set(sidecars.map((sidecar) => sidecar.file.toLowerCase()));
  const recorded = (evidence, files) => {
    if (!Array.isArray(evidence) || evidence.length !== files.length) refuse("TAURI_AUTHENTICODE_SIGN", `the signer returned ${Array.isArray(evidence) ? evidence.length : "no"} record(s) for ${files.length} file(s)`);
    evidence.forEach((item) => {
      if (item.subject === SIGN_SUBJECT && item.authenticode === "Valid") return;
      const code = sidecarNames.has(path.basename(String(item.file)).toLowerCase()) ? "TAURI_AUTHENTICODE_SIDECAR_UNSIGNED" : "TAURI_AUTHENTICODE_SIGN";
      refuse(code, `${item.file}: signer subject ${JSON.stringify(item.subject)} / ${JSON.stringify(item.authenticode)}, expected ${SIGN_SUBJECT} / Valid`);
    });
    return evidence;
  };
  // Signing a copy in place rewrites it. A copy outside the build's output directory (src-tauri/binaries, src-tauri/resources) must be a git-ignored stage output
  // (the gate's stage-output ledger records it); a TRACKED one would change the commit's tree, so it is refused before anything is signed.
  const inPlace = sidecars.flatMap((sidecar) => sidecar.copies.map((copy) => copy.path)).filter((file) => path.relative(path.resolve(debugDir), path.resolve(file)).startsWith(".."));
  const classify = deps.classifyStaged ?? classifyStagedFiles;
  const classes = inPlace.length ? classify(inPlace, { appRoot, tauriDir }) : new Map();
  for (const file of inPlace) {
    const kind = classes.get(file) ?? classes.get(path.resolve(file));
    if (kind === "tracked") refuse("TAURI_AUTHENTICODE_TRACKED_RESOURCE", `${file} is tracked by git, and signing it in place would change the commit's tree: stage it (or git-ignore it) the way release stages it`, { file });
    if (kind !== "ignored") refuse("TAURI_AUTHENTICODE_TRACKED_RESOURCE", `${file} is not a git-ignored stage output (${kind ?? "unknown"}): it cannot be signed in place`, { file, state: kind ?? "unknown" });
  }
  // 2. The marker reaches its final value before any byte is signed.
  step("patch-marker", () => patchMarker(exe));
  // 3. Sign and verify the exe and every copy of every sidecar, and record each digest.
  const signedFiles = [exe, ...sidecars.flatMap((sidecar) => sidecar.copies.map((copy) => copy.path))];
  const signedExe = step("sign-exe", () => {
    const signed = recorded(deps.signWindows(signedFiles), signedFiles);
    recorded(deps.verifyWindows(signedFiles), signedFiles);
    return signed;
  });
  const signedSha = sha256(exe);
  for (const sidecar of sidecars) {
    sidecar.digests = new Set();
    for (const copy of sidecar.copies) {
      const record = signedExe.find((item) => path.resolve(item.file) === path.resolve(copy.path));
      copy.sha256After = record?.after?.sha256;
      if (!copy.sha256After || copy.sha256After !== sha256(copy.path)) refuse("TAURI_AUTHENTICODE_SIDECAR_STALE", `${copy.path}: the signer recorded sha256After ${copy.sha256After}, the file is ${sha256(copy.path)}`, { sidecar: sidecar.name });
      sidecar.digests.add(copy.sha256After);
    }
  }
  // 4. The carried bundle; a signed copy that changed since signing is stale.
  step("bundle", () => deps.bundle(authenticodeBundleCommand(descriptor)));
  for (const sidecar of sidecars) for (const copy of sidecar.copies) if (!isFile(copy.path) || sha256(copy.path) !== copy.sha256After) refuse("TAURI_AUTHENTICODE_SIDECAR_STALE", `${copy.path} changed after it was signed (signed ${copy.sha256After})`, { sidecar: sidecar.name });
  const installer = deps.findInstaller();
  // 5. The installer must embed the signed exe and each signed sidecar (the bundler's copy or the build output's), byte for byte, and no other executable.
  const payload = step("check-payload", () => {
    const main = verifyPayload({ installer, entryName: path.basename(exe), expectedSha256: signedSha });
    const embedded = sidecars.map((sidecar) => {
      const failures = [];
      for (const digest of sidecar.digests) {
        try { return { name: sidecar.name, ...verifyPayload({ installer, entryName: sidecar.file, expectedSha256: digest }) }; } catch (error) { failures.push(error.message); }
      }
      return refuse(failures.every((message) => /does not embed/.test(message)) ? "TAURI_AUTHENTICODE_SIDECAR_STALE" : "TAURI_AUTHENTICODE_PAYLOAD", `the installer does not carry the signed ${sidecar.file}: ${failures.join("; ")}`, { sidecar: sidecar.name });
    });
    const known = new Set([path.basename(exe).toLowerCase(), ...sidecarNames]);
    for (const entry of listPayload(installer)) {
      const name = String(entry.name).replaceAll("\\", "/");
      const leaf = path.posix.basename(name).toLowerCase();
      if (!leaf.endsWith(".exe") || name.startsWith("$PLUGINSDIR") || leaf === "uninstall.exe" || known.has(leaf)) continue;
      refuse("TAURI_AUTHENTICODE_PAYLOAD_UNSIGNED_EXECUTABLE", `the installer carries an executable that is not signed here: ${entry.name}`, { entry: entry.name });
    }
    return { ...main, sidecars: embedded };
  });
  // 6. Sign and verify the installer.
  const signedInstaller = step("sign-installer", () => {
    const signed = recorded(deps.signWindows([installer]), [installer]);
    recorded(deps.verifyWindows([installer]), [installer]);
    return signed;
  });
  // 7. Install (only now), then 8. verify what was installed: the exe and each sidecar, by signature and by digest.
  step("install", () => deps.install(installer));
  const installed = deps.installedExe();
  const installedSidecars = sidecars.map((sidecar) => ({ sidecar, path: deps.installedSidecar ? deps.installedSidecar(sidecar.file, sidecar) : path.join(path.dirname(installed), sidecar.installRel ?? sidecar.file) }));
  for (const item of installedSidecars) if (!isFile(item.path)) refuse("TAURI_AUTHENTICODE_SIDECAR_MISSING", `sidecar ${item.sidecar.name} is not installed at ${item.path}`, { sidecar: item.sidecar.name });
  const installedFiles = [installed, ...installedSidecars.map((item) => item.path)];
  step("verify", () => recorded(deps.verifyWindows(installedFiles), installedFiles));
  for (const item of installedSidecars) {
    item.sha256 = sha256(item.path);
    if (!item.sidecar.digests.has(item.sha256)) refuse("TAURI_AUTHENTICODE_SIDECAR_STALE", `installed ${item.path} (sha256 ${item.sha256}) is not the signed ${item.sidecar.file}`, { sidecar: item.sidecar.name });
  }
  return {
    order, exe, installer, installed, payload,
    // In-place signed stage outputs (git-ignored): the gate's ledger walk attributes them from `files` (stage-ledger.mjs reportedPaths); paths are absolute here, the caller makes them app-relative.
    stageOutputs: { files: inPlace.map((file) => ({ path: file.replaceAll("\\", "/"), kind: "signed-in-place", sha256: sha256(file) })) },
    sidecars: installedSidecars.map((item) => ({ name: item.sidecar.name, copies: item.sidecar.copies.map((copy) => ({ role: copy.role, path: copy.path, sha256After: copy.sha256After })), installed: { path: item.path, sha256: item.sha256 } })),
    signing: { subject: SIGN_SUBJECT, files: [...signedExe, ...signedInstaller].map((item) => ({ file: item.file, sha256Before: item.before?.sha256, sha256After: item.after?.sha256, subject: item.subject })) },
  };
}
