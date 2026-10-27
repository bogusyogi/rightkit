// Declarative assembly of a composite desktop app: one definition per platform (`assembly.mac`, `assembly.win` in the
// release config), shared by `right-release dev` (install in place, install-assembled-app.mjs) and `right-release build`
// (production packaging through `right-release assemble-app`, assemble-app-command.mjs). Key names, shapes and the Pulse
// example are in docs/ASSEMBLED-APP.md.
//
// This module is the pure half: validation, stage-receipt access, bundle/payload layout, version stamps, bindings and the
// structural proof of an assembled bundle. Nothing here signs, installs or contacts the network. Every file-system and process
// effect goes through `deps` (defaults are the real node modules) so tests run against a temp folder with a fake `run`.
import { createHash } from "node:crypto";
import nodeFs from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { resolveTargetRoot } from "./cargo-target.mjs";
import { validateStageSidecarReceipt } from "./stage-sidecar.mjs";

export const ASSEMBLED_APP_KIND = "assembled-app";
export const DEFAULT_STAGE_RECEIPT = "dist/stage/stage-sidecar.json";
export const BUNDLE_VERSION_COMMIT_COUNT = "commit-count";
export const ASSEMBLY_PROFILES = Object.freeze(["dev", "release"]);

export class AssembledAppError extends Error {
  constructor(code, message, details = {}) {
    super(message);
    this.name = "AssembledAppError";
    this.code = code;
    Object.assign(this, details);
  }
}
const refuse = (code, message, details) => { throw new AssembledAppError(code, message, details); };
const invalid = (message) => refuse("ASSEMBLY_CONFIG_INVALID", message);
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const sha256Of = (fs, file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");

/** A contained relative path with `/` separators; `where` names the key in errors. */
export function containedRelative(value, where) {
  if (typeof value !== "string" || !value.trim()) invalid(`${where} must be a non-empty relative path`);
  const normalized = value.replaceAll("\\", "/").replace(/^\.\//, "");
  if (path.isAbsolute(value) || path.win32.isAbsolute(value) || /^[A-Za-z]:/.test(value) || normalized.split("/").some((part) => part === ".." || part === "")) {
    invalid(`${where} must be relative, contained and free of empty or ".." segments: ${value}`);
  }
  return normalized;
}
const nativePath = (base, relative) => path.join(base, ...relative.split("/"));
const assertKeys = (object, allowed, where) => {
  if (!isObject(object)) invalid(`${where} must be an object`);
  const unknown = Object.keys(object).filter((key) => !allowed.includes(key));
  if (unknown.length) invalid(`${where} has unknown key(s): ${unknown.join(", ")} (allowed: ${allowed.join(", ")})`);
};
const list = (value, where) => { if (value === undefined) return []; if (!Array.isArray(value)) invalid(`${where} must be an array`); return value; };
const stringList = (value, where) => list(value, where).map((item, index) => { if (typeof item !== "string" || !item.trim()) invalid(`${where}[${index}] must be a non-empty string`); return item; });
const relativeList = (value, where) => stringList(value, where).map((item, index) => containedRelative(item, `${where}[${index}]`));

function commandList(value, where) {
  return list(value, where).map((item, index) => {
    const label = `${where}[${index}]`;
    assertKeys(item, ["cmd", "args", "cwd", "env", "timeoutMs"], label);
    if (typeof item.cmd !== "string" || !item.cmd.trim()) invalid(`${label}.cmd must be a non-empty string`);
    if (item.args !== undefined && (!Array.isArray(item.args) || item.args.some((arg) => typeof arg !== "string"))) invalid(`${label}.args must be a list of strings`);
    if (item.env !== undefined && (!isObject(item.env) || Object.values(item.env).some((v) => typeof v !== "string"))) invalid(`${label}.env must map names to strings`);
    return { cmd: item.cmd, args: [...(item.args ?? [])], ...(item.cwd === undefined ? {} : { cwd: containedRelative(item.cwd, `${label}.cwd`) }), ...(item.env ? { env: { ...item.env } } : {}), ...(item.timeoutMs === undefined ? {} : { timeoutMs: item.timeoutMs }) };
  });
}
function prepareBlock(value, where) {
  if (value === undefined) return { release: [] };
  assertKeys(value, ["release"], where);
  return { release: commandList(value.release, `${where}.release`) };
}

const stageEntry = (item, where, { defaultTo }) => {
  const hasStage = item.stage !== undefined;
  const hasProduct = item.product !== undefined;
  const hasFrom = item.from !== undefined;
  if (Number(hasStage) + Number(hasProduct) + Number(hasFrom) !== 1) invalid(`${where} needs exactly one of stage, product${defaultTo ? "" : ", from"}`);
  if (item.kind !== undefined && !(hasStage && ["cargo-bin", "remote", "resource", "native-library"].includes(item.kind))) invalid(`${where}.kind applies to a stage row: cargo-bin, remote, resource or native-library`);
  if (item.optional !== undefined && typeof item.optional !== "boolean") invalid(`${where}.optional must be true or false`);
  if (item.ifStaged !== undefined && (typeof item.ifStaged !== "string" || !item.ifStaged)) invalid(`${where}.ifStaged must be a staged row name`);
  const name = item.stage ?? item.product;
  if (name !== undefined && (typeof name !== "string" || !name.trim() || /[\\/]/.test(name))) invalid(`${where} name must be a plain name`);
  const to = item.to === undefined ? (defaultTo && name ? `${defaultTo}/${name}` : undefined) : item.to;
  return {
    ...(hasStage ? { stage: item.stage, kind: item.kind ?? "cargo-bin" } : {}),
    ...(hasProduct ? { product: item.product } : {}),
    ...(hasFrom ? { from: containedRelative(item.from, `${where}.from`) } : {}),
    to: containedRelative(to, `${where}.to`),
    ...(item.optional ? { optional: true } : {}),
    ...(item.ifStaged ? { ifStaged: item.ifStaged } : {}),
  };
};

const BUNDLE_ID = /^[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+$/;
const COMMON_KEYS = ["productName", "targetTriple", "stageReceipt", "devMarker", "stagingOutput", "bindingFile", "prepare"];

/** The normalized `assembly.mac` definition, or an AssembledAppError (ASSEMBLY_CONFIG_INVALID). */
export function validateMacAssembly(spec) {
  assertKeys(spec, [...COMMON_KEYS, "bundleId", "outer", "nested", "helpers", "plugins", "launchDaemons", "launchAgents", "resources", "bundleVersion", "require"], "assembly.mac");
  const where = "assembly.mac";
  if (typeof spec.productName !== "string" || !spec.productName.trim() || /[\\/]/.test(spec.productName)) invalid(`${where}.productName must be a plain name`);
  if (typeof spec.bundleId !== "string" || !BUNDLE_ID.test(spec.bundleId)) invalid(`${where}.bundleId must be the outer bundle identifier`);
  if (typeof spec.targetTriple !== "string" || !/-apple-darwin$/.test(spec.targetTriple)) invalid(`${where}.targetTriple must be an apple-darwin triple`);
  const outer = spec.outer;
  assertKeys(outer, ["tool", "projectSpec", "project", "scheme", "configuration", "product", "unsetEnv", "extraBuildSettings"], `${where}.outer`);
  if (outer.tool !== "xcodebuild") invalid(`${where}.outer.tool must be "xcodebuild"`);
  if (typeof outer.scheme !== "string" || !outer.scheme) invalid(`${where}.outer.scheme is required`);
  const project = containedRelative(outer.project, `${where}.outer.project`);
  if (!/\.xcodeproj$/.test(project)) invalid(`${where}.outer.project must be an .xcodeproj`);
  const settings = outer.extraBuildSettings ?? {};
  if (!isObject(settings) || Object.entries(settings).some(([k, v]) => !/^[A-Z_][A-Z0-9_]*$/.test(k) || typeof v !== "string" || /^CODE_SIGN/.test(k))) invalid(`${where}.outer.extraBuildSettings must map build setting names (not CODE_SIGN*) to strings`);
  const product = outer.product ?? `${spec.productName}.app`;
  if (!/^[^/\\]+\.app$/.test(product)) invalid(`${where}.outer.product must be an <name>.app file name`);
  const nested = list(spec.nested, `${where}.nested`).map((item, index) => {
    const label = `${where}.nested[${index}]`;
    assertKeys(item, ["built", "manifestPath", "bundleDirectory", "productName", "to"], label);
    if (item.built !== "tauri") invalid(`${label}.built must be "tauri"`);
    if ((item.manifestPath === undefined) === (item.bundleDirectory === undefined)) invalid(`${label} needs exactly one of manifestPath, bundleDirectory`);
    if (typeof item.productName !== "string" || !item.productName) invalid(`${label}.productName is required`);
    const to = containedRelative(item.to, `${label}.to`);
    if (!to.endsWith(".app")) invalid(`${label}.to must end in .app`);
    return { built: "tauri", ...(item.manifestPath ? { manifestPath: containedRelative(item.manifestPath, `${label}.manifestPath`) } : { bundleDirectory: containedRelative(item.bundleDirectory, `${label}.bundleDirectory`) }), productName: item.productName, to };
  });
  const helpers = list(spec.helpers, `${where}.helpers`).map((item, index) => {
    assertKeys(item, ["product", "stage", "kind", "to", "optional", "ifStaged"], `${where}.helpers[${index}]`);
    return stageEntry(item, `${where}.helpers[${index}]`, { defaultTo: "Contents/Helpers" });
  });
  const plist = (value, kind) => list(value, `${where}.${kind}`).map((item, index) => {
    const label = `${where}.${kind}[${index}]`;
    assertKeys(item, ["from", "to"], label);
    const from = containedRelative(item.from, `${label}.from`);
    return { from, to: containedRelative(item.to ?? `Contents/Library/${kind === "launchDaemons" ? "LaunchDaemons" : "LaunchAgents"}/${path.posix.basename(from)}`, `${label}.to`) };
  });
  const resources = list(spec.resources, `${where}.resources`).map((item, index) => {
    const label = `${where}.resources[${index}]`;
    assertKeys(item, ["from", "to", "ifStaged"], label);
    return { from: containedRelative(item.from, `${label}.from`), to: containedRelative(item.to, `${label}.to`), ...(item.ifStaged ? { ifStaged: String(item.ifStaged) } : {}) };
  });
  if (spec.bundleVersion !== undefined && spec.bundleVersion !== BUNDLE_VERSION_COMMIT_COUNT) invalid(`${where}.bundleVersion must be "${BUNDLE_VERSION_COMMIT_COUNT}" or unset`);
  const plugins = stringList(spec.plugins, `${where}.plugins`);
  if (plugins.some((name) => /[\\/]/.test(name))) invalid(`${where}.plugins entries are plain file names inside Contents/PlugIns`);
  return {
    platform: "mac", productName: spec.productName, bundleId: spec.bundleId, targetTriple: spec.targetTriple,
    stageReceipt: containedRelative(spec.stageReceipt ?? DEFAULT_STAGE_RECEIPT, `${where}.stageReceipt`),
    outer: { tool: "xcodebuild", ...(outer.projectSpec ? { projectSpec: containedRelative(outer.projectSpec, `${where}.outer.projectSpec`) } : {}), project, scheme: outer.scheme, configuration: outer.configuration ?? "Release", product, unsetEnv: stringList(outer.unsetEnv, `${where}.outer.unsetEnv`), extraBuildSettings: { ...settings } },
    nested, helpers, plugins, launchDaemons: plist(spec.launchDaemons, "launchDaemons"), launchAgents: plist(spec.launchAgents, "launchAgents"), resources,
    bundleVersion: spec.bundleVersion, require: relativeList(spec.require, `${where}.require`),
    ...(spec.devMarker === undefined ? {} : { devMarker: containedRelative(spec.devMarker, `${where}.devMarker`) }),
    stagingOutput: containedRelative(spec.stagingOutput ?? `dist/staging/${spec.productName}.app`, `${where}.stagingOutput`),
    bindingFile: containedRelative(spec.bindingFile ?? `${path.posix.dirname(spec.stagingOutput ?? `dist/staging/${spec.productName}.app`)}/payload-binding.json`, `${where}.bindingFile`),
    prepare: prepareBlock(spec.prepare, `${where}.prepare`),
  };
}

/** The normalized `assembly.win` definition. */
export function validateWindowsAssembly(spec) {
  assertKeys(spec, [...COMMON_KEYS, "payload", "sign", "nsis"], "assembly.win");
  const where = "assembly.win";
  if (typeof spec.productName !== "string" || !spec.productName.trim() || /[\\/]/.test(spec.productName)) invalid(`${where}.productName must be a plain name`);
  if (typeof spec.targetTriple !== "string" || !/-pc-windows-/.test(spec.targetTriple)) invalid(`${where}.targetTriple must be a windows triple`);
  assertKeys(spec.payload, ["files", "rewrites", "require"], `${where}.payload`);
  const files = list(spec.payload.files, `${where}.payload.files`).map((item, index) => {
    assertKeys(item, ["stage", "kind", "from", "to", "optional", "ifStaged"], `${where}.payload.files[${index}]`);
    return stageEntry(item, `${where}.payload.files[${index}]`, { defaultTo: null });
  });
  if (!files.length) invalid(`${where}.payload.files must not be empty`);
  const seen = new Set();
  for (const file of files) { const key = file.to.toLowerCase(); if (seen.has(key) && !file.from) invalid(`${where}.payload.files lists ${file.to} twice (Windows paths are case-insensitive)`); seen.add(key); }
  const rewrites = list(spec.payload.rewrites, `${where}.payload.rewrites`).map((item, index) => {
    const label = `${where}.payload.rewrites[${index}]`;
    assertKeys(item, ["file", "regex", "flags", "replacement", "requireMatch"], label);
    if (typeof item.regex !== "string" || typeof item.replacement !== "string") invalid(`${label} needs regex and replacement strings`);
    try { new RegExp(item.regex, item.flags ?? ""); } catch (error) { invalid(`${label}.regex is not a valid expression: ${error.message}`); }
    return { file: containedRelative(item.file, `${label}.file`), regex: item.regex, flags: item.flags ?? "", replacement: item.replacement, requireMatch: item.requireMatch !== false };
  });
  let nsis;
  if (spec.nsis !== undefined) {
    assertKeys(spec.nsis, ["script", "output", "defines"], `${where}.nsis`);
    const defines = spec.nsis.defines ?? {};
    if (!isObject(defines) || Object.entries(defines).some(([k, v]) => !/^[A-Za-z_][A-Za-z0-9_]*$/.test(k) || typeof v !== "string")) invalid(`${where}.nsis.defines must map names to strings`);
    nsis = { script: containedRelative(spec.nsis.script, `${where}.nsis.script`), output: containedRelative(spec.nsis.output, `${where}.nsis.output`), defines: { ...defines } };
  }
  const stagingOutput = containedRelative(spec.stagingOutput ?? `dist/staging/windows/${spec.productName}`, `${where}.stagingOutput`);
  return {
    platform: "win", productName: spec.productName, targetTriple: spec.targetTriple,
    stageReceipt: containedRelative(spec.stageReceipt ?? DEFAULT_STAGE_RECEIPT, `${where}.stageReceipt`),
    payload: { files, rewrites, require: relativeList(spec.payload.require, `${where}.payload.require`) },
    sign: relativeList(spec.sign, `${where}.sign`),
    ...(spec.devMarker === undefined ? {} : { devMarker: containedRelative(spec.devMarker, `${where}.devMarker`) }),
    stagingOutput, bindingFile: containedRelative(spec.bindingFile ?? `${path.posix.dirname(stagingOutput)}/payload-binding.json`, `${where}.bindingFile`),
    prepare: prepareBlock(spec.prepare, `${where}.prepare`), ...(nsis ? { nsis } : {}),
  };
}

/** `config.assembly.<platform>` validated. Throws when the block is absent. */
export function resolveAssembly(config, platform) {
  const spec = config?.assembly?.[platform];
  if (spec === undefined) invalid(`the release config declares no assembly.${platform}`);
  if (platform === "mac") return validateMacAssembly(spec);
  if (platform === "win") return validateWindowsAssembly(spec);
  return invalid(`assembly.${platform} is not supported (mac or win)`);
}

// ---- stage receipt -----------------------------------------------------------------------------------------------

/**
 * The stage-sidecar receipt of this assembly, validated for its target. `find(name, kind)` returns `{ file, sha256 }` for a row
 * and re-hashes the staged file against the receipt (fails closed on a missing, other-profile or changed file).
 */
export function loadStageReceipt({ root, spec, profile, fs = nodeFs }) {
  const receiptFile = path.resolve(root, spec.stageReceipt);
  let receipt;
  try { receipt = JSON.parse(fs.readFileSync(receiptFile, "utf8")); }
  catch (error) { refuse("ASSEMBLY_RECEIPT_MISSING", `stage receipt unreadable: ${receiptFile} (${error.code ?? error.message}); run right-release stage-sidecar --target ${spec.targetTriple} first`); }
  try { validateStageSidecarReceipt(receipt, { target: spec.targetTriple }); }
  catch (error) { refuse("ASSEMBLY_RECEIPT_INVALID", `${receiptFile}: ${error.message}`); }
  return {
    file: receiptFile,
    has: (name) => receipt.files.some((row) => row.name === name),
    find(name, kind = "cargo-bin") {
      const row = receipt.files.find((candidate) => candidate.kind === kind && candidate.name === name);
      if (!row) refuse("ASSEMBLY_RECEIPT_ROW_MISSING", `stage receipt has no ${kind} row ${name}`);
      if (kind === "cargo-bin" && row.profile !== profile) refuse("ASSEMBLY_RECEIPT_PROFILE", `${name} was staged with Cargo profile ${row.profile}; this ${profile} assembly needs ${profile}`);
      const file = path.resolve(root, row.path);
      let actual;
      try { actual = sha256Of(fs, file); } catch (error) { refuse("ASSEMBLY_RECEIPT_FILE_MISSING", `staged ${name} is missing: ${file} (${error.code ?? error.message})`); }
      if (actual !== row.sha256) refuse("ASSEMBLY_RECEIPT_HASH", `staged ${name} differs from the stage receipt (${actual} vs ${row.sha256}); run stage-sidecar again`);
      return { file, sha256: row.sha256 };
    },
  };
}

// ---- layout ------------------------------------------------------------------------------------------------------

function copyFileTo(fs, source, target, label, { executable = false } = {}) {
  let info;
  try { info = fs.statSync(source); } catch (error) { refuse("ASSEMBLY_SOURCE_MISSING", `${label} is missing: ${source} (${error.code ?? error.message})`); }
  if (!info.isFile()) refuse("ASSEMBLY_SOURCE_INVALID", `${label} is not a regular file: ${source}`);
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.copyFileSync(source, target);
  if (executable) fs.chmodSync(target, 0o755);
}
function copyPathTo(fs, source, target, label) {
  let info;
  try { info = fs.statSync(source); } catch (error) { refuse("ASSEMBLY_SOURCE_MISSING", `${label} is missing: ${source} (${error.code ?? error.message})`); }
  fs.mkdirSync(path.dirname(target), { recursive: true });
  if (info.isDirectory()) fs.cpSync(source, target, { recursive: true, force: true, verbatimSymlinks: true });
  else copyFileTo(fs, source, target, label);
}
function copyBuiltApp(fs, source, target, label) {
  let info;
  try { info = fs.statSync(source); } catch (error) { refuse("ASSEMBLY_SOURCE_MISSING", `${label} is missing: ${source} (${error.code ?? error.message})`); }
  if (!info.isDirectory()) refuse("ASSEMBLY_SOURCE_INVALID", `${label} is not an app bundle: ${source}`);
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.cpSync(source, target, { recursive: true, force: true, verbatimSymlinks: true });
}

/** The marker/binding document: receipt hashes of every staged file placed in the output. */
export function bindingDocument({ profile, targetTriple, bound }) {
  return { schema: 1, profile, devOnly: profile !== "release", target: targetTriple, files: Object.fromEntries(bound.map(({ dest, sha256 }) => [dest, sha256])) };
}

function profileDirectory(profile) { return profile === "release" ? "release" : "debug"; }

/** The built Tauri .app of one `nested` entry (Cargo's own target directory; the product name is the bundle name). */
export function resolveNestedApp({ root, entry, spec, profile, deps = {} }) {
  const fs = deps.fs ?? nodeFs;
  const base = entry.bundleDirectory
    ? path.resolve(root, entry.bundleDirectory)
    : path.join((deps.resolveTargetRoot ?? resolveTargetRoot)(path.resolve(root, entry.manifestPath)), spec.targetTriple, profileDirectory(profile), "bundle", "macos");
  const app = path.join(base, `${entry.productName}.app`);
  if (!fs.existsSync(app)) refuse("ASSEMBLY_SOURCE_MISSING", `built Tauri app not found: ${app}`);
  return app;
}

/** CFBundleVersion: the commit count of the source revision. A shallow clone is refused (no network, so no unshallow). */
export function commitCount({ root, env = process.env, run = spawnSync }) {
  const git = (args) => {
    const result = run("git", args, { cwd: root, env, encoding: "utf8", windowsHide: true, shell: false });
    if (result.error || result.status !== 0) refuse("ASSEMBLY_VERSION_UNAVAILABLE", `git ${args.join(" ")} failed (${result.status ?? result.error?.message})`);
    return String(result.stdout ?? "").trim();
  };
  if (git(["rev-parse", "--is-shallow-repository"]) === "true") refuse("ASSEMBLY_VERSION_UNAVAILABLE", "the repository is shallow; CFBundleVersion needs the full history (fetch it; this tool contacts no network)");
  const count = git(["rev-list", "--count", env.RIGHT_GIT_SOURCE_REVISION || "HEAD"]);
  if (!/^[1-9][0-9]*$/.test(count)) refuse("ASSEMBLY_VERSION_UNAVAILABLE", `could not derive a build number: ${count}`);
  return count;
}

function stampBundleVersion({ bundle, build, env, run }) {
  const plist = path.join(bundle, "Contents", "Info.plist");
  const buddy = (command) => run("/usr/libexec/PlistBuddy", ["-c", command, plist], { env, encoding: "utf8", windowsHide: true, shell: false });
  const set = buddy(`Set :CFBundleVersion ${build}`);
  if (set.status === 0) return;
  const add = buddy(`Add :CFBundleVersion string ${build}`);
  if (add.status !== 0) refuse("ASSEMBLY_VERSION_FAILED", `could not set CFBundleVersion in ${plist}`);
}

/**
 * Lay out the unsigned macOS bundle at `out` from the xcodebuild product (`outerApp`), the built nested apps and the staged
 * rows. Returns `{ bundle, bound }`; `bound` lists every receipt-staged file placed (bundle-relative dest, receipt sha256),
 * each re-hashed after the copy. Does not sign.
 */
export function assembleMacBundle({ root, spec, profile, out, outerApp, nestedApps = [], receipt, env = process.env, deps = {} }) {
  const fs = deps.fs ?? nodeFs;
  const run = deps.run ?? spawnSync;
  fs.rmSync(out, { recursive: true, force: true });
  copyBuiltApp(fs, outerApp, out, "outer app (xcodebuild)");
  const products = path.dirname(outerApp);
  const bound = [];
  const place = (dest, staged) => {
    const actual = sha256Of(fs, nativePath(out, dest));
    if (actual !== staged.sha256) refuse("ASSEMBLY_BOUND_HASH", `${dest} differs from its stage receipt row after the copy (${actual} vs ${staged.sha256})`);
    bound.push({ dest, sha256: actual });
  };
  for (const name of spec.plugins) {
    if (!fs.existsSync(nativePath(out, `Contents/PlugIns/${name}`))) refuse("ASSEMBLY_SOURCE_MISSING", `plug-in ${name} is not in Contents/PlugIns of the xcodebuild product`);
  }
  spec.nested.forEach((entry, index) => copyBuiltApp(fs, nestedApps[index], nativePath(out, entry.to), `nested app ${entry.productName}`));
  for (const helper of spec.helpers) {
    if (helper.ifStaged && !receipt.has(helper.ifStaged)) continue;
    if (helper.product) { copyFileTo(fs, path.join(products, helper.product), nativePath(out, helper.to), `helper ${helper.product} (xcodebuild product)`, { executable: true }); continue; }
    if (helper.optional && !receipt.has(helper.stage)) { (deps.log ?? console.log)(`assemble-app: optional ${helper.stage} is not staged; skipped`); continue; }
    const staged = receipt.find(helper.stage, helper.kind);
    copyFileTo(fs, staged.file, nativePath(out, helper.to), `helper ${helper.stage}`, { executable: true });
    place(helper.to, staged);
  }
  for (const item of [...spec.launchDaemons, ...spec.launchAgents]) copyFileTo(fs, path.resolve(root, item.from), nativePath(out, item.to), `launchd plist ${item.from}`);
  for (const item of spec.resources) {
    if (item.ifStaged && !receipt.has(item.ifStaged)) continue;
    copyPathTo(fs, path.resolve(root, item.from), nativePath(out, item.to), `resource ${item.from}`);
  }
  if (spec.bundleVersion === BUNDLE_VERSION_COMMIT_COUNT) {
    const build = (deps.commitCount ?? commitCount)({ root, env, run });
    for (const bundle of [out, ...spec.nested.map((entry) => nativePath(out, entry.to)), ...spec.plugins.map((name) => nativePath(out, `Contents/PlugIns/${name}`))]) stampBundleVersion({ bundle, build, env, run });
    (deps.log ?? console.log)(`assemble-app: CFBundleVersion ${build}`);
  }
  if (profile === "dev" && spec.devMarker) {
    const marker = nativePath(out, spec.devMarker);
    fs.mkdirSync(path.dirname(marker), { recursive: true });
    fs.writeFileSync(marker, `${JSON.stringify(bindingDocument({ profile, targetTriple: spec.targetTriple, bound }), null, 2)}\n`);
  }
  for (const file of spec.require) if (!fs.existsSync(nativePath(out, file))) refuse("ASSEMBLY_REQUIRED_MISSING", `assembled bundle lacks ${file}`);
  return { bundle: out, bound };
}

/**
 * The structural proof of an assembled (and optionally signed) bundle, read-only: required files, Info.plist identity,
 * launchd `BundleProgram` paths, and (`signed`) `/usr/bin/codesign --verify --strict --deep`. A release bundle must carry no dev marker.
 * Throws AssembledAppError (ASSEMBLY_VERIFY_FAILED) listing every problem.
 */
export function verifyMacBundle({ root, spec, bundle, profile, signed = false, env = process.env, deps = {} }) {
  const fs = deps.fs ?? nodeFs;
  const run = deps.run ?? spawnSync;
  const problems = [];
  const capture = (command, args) => run(command, args, { cwd: root, env, encoding: "utf8", windowsHide: true, shell: false });
  for (const file of spec.require) if (!fs.existsSync(nativePath(bundle, file))) problems.push(`missing ${file}`);
  const id = capture("/usr/bin/plutil", ["-extract", "CFBundleIdentifier", "raw", "-o", "-", path.join(bundle, "Contents", "Info.plist")]);
  const actualId = id.error || id.status !== 0 ? null : String(id.stdout ?? "").trim();
  if (actualId !== spec.bundleId) problems.push(`CFBundleIdentifier ${actualId === null ? "unreadable" : JSON.stringify(actualId)} is not ${spec.bundleId}`);
  for (const item of [...spec.launchDaemons, ...spec.launchAgents]) {
    const file = nativePath(bundle, item.to);
    const program = capture("/usr/bin/plutil", ["-extract", "BundleProgram", "raw", "-o", "-", file]);
    if (program.error || program.status !== 0) continue; // a plist may name ProgramArguments instead
    const value = String(program.stdout ?? "").trim();
    if (!value || value.startsWith("/") || value.split("/").includes("..")) problems.push(`${item.to}: BundleProgram ${JSON.stringify(value)} must be a relative path inside the bundle`);
    else if (!fs.existsSync(nativePath(bundle, value))) problems.push(`${item.to}: BundleProgram ${value} is not in the bundle`);
  }
  if (profile === "release" && spec.devMarker && fs.existsSync(nativePath(bundle, spec.devMarker))) problems.push(`${spec.devMarker} marks a dev-only payload; it must not be signed or shipped for release`);
  if (signed) {
    const strict = capture("/usr/bin/codesign", ["--verify", "--strict", "--deep", bundle]);
    if (strict.error || strict.status !== 0) problems.push(`codesign --verify --strict --deep failed: ${strict.error?.message ?? (`${strict.stdout ?? ""}${strict.stderr ?? ""}`.trim() || `exit ${strict.status}`)}`);
  }
  if (problems.length) refuse("ASSEMBLY_VERIFY_FAILED", `${bundle}: ${problems.length} problem(s): ${problems.join("; ")}`, { problems });
  return { checked: spec.require.length };
}

// ---- Windows payload ----------------------------------------------------------------------------------------------

/** Lay out the unsigned Windows payload at `out`. Returns `{ payload, bound }`. */
export function assembleWindowsPayload({ root, spec, profile, out, receipt, deps = {} }) {
  const fs = deps.fs ?? nodeFs;
  fs.rmSync(out, { recursive: true, force: true });
  fs.mkdirSync(out, { recursive: true });
  const bound = [];
  // Directory copies first, then single files, so a file row may overwrite one inside a copied folder (the Muse hook launchers).
  const ordered = [...spec.payload.files.filter((item) => item.from), ...spec.payload.files.filter((item) => !item.from)];
  for (const item of ordered) {
    if (item.ifStaged && !receipt.has(item.ifStaged)) continue;
    const target = nativePath(out, item.to);
    if (item.from) { copyPathTo(fs, path.resolve(root, item.from), target, `payload ${item.from}`); continue; }
    if (item.optional && !receipt.has(item.stage)) { (deps.log ?? console.log)(`assemble-app: optional ${item.stage} is not staged; skipped`); continue; }
    const staged = receipt.find(item.stage, item.kind);
    copyFileTo(fs, staged.file, target, `payload ${item.stage}`);
    const actual = sha256Of(fs, target);
    if (actual !== staged.sha256) refuse("ASSEMBLY_BOUND_HASH", `${item.to} differs from its stage receipt row after the copy (${actual} vs ${staged.sha256})`);
    bound.push({ dest: item.to, sha256: actual });
  }
  for (const rewrite of spec.payload.rewrites) {
    const file = nativePath(out, rewrite.file);
    let text;
    try { text = fs.readFileSync(file, "utf8"); } catch (error) { refuse("ASSEMBLY_SOURCE_MISSING", `rewrite target ${rewrite.file} is missing (${error.code ?? error.message})`); }
    const pattern = new RegExp(rewrite.regex, rewrite.flags);
    if (!pattern.test(text) && rewrite.requireMatch) refuse("ASSEMBLY_REWRITE_NO_MATCH", `${rewrite.file}: nothing matched ${rewrite.regex}`);
    fs.writeFileSync(file, text.replace(new RegExp(rewrite.regex, rewrite.flags), rewrite.replacement));
  }
  if (profile === "dev" && spec.devMarker) fs.writeFileSync(nativePath(out, spec.devMarker), `${JSON.stringify(bindingDocument({ profile, targetTriple: spec.targetTriple, bound }), null, 2)}\n`);
  for (const file of spec.payload.require) if (!fs.existsSync(nativePath(out, file))) refuse("ASSEMBLY_REQUIRED_MISSING", `assembled payload lacks ${file}`);
  for (const file of spec.sign) if (!fs.existsSync(nativePath(out, file))) refuse("ASSEMBLY_REQUIRED_MISSING", `assembled payload lacks the file to sign: ${file}`);
  return { payload: out, bound };
}

/** The release binding file (outside the signed output). Refuses to write for a dev profile. */
export function writeReleaseBinding({ root, spec, bound, fs = nodeFs }) {
  const file = path.resolve(root, spec.bindingFile);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(bindingDocument({ profile: "release", targetTriple: spec.targetTriple, bound }), null, 2)}\n`);
  return file;
}

/**
 * A release payload needs its binding: present, `devOnly: false`, profile release; `rehash` (before signing) re-reads every bound file.
 * After signing the bytes change by design, so later steps pass rehash false.
 */
export function verifyReleaseBinding({ root, spec, output, rehash = false, fs = nodeFs }) {
  const file = path.resolve(root, spec.bindingFile);
  let binding;
  try { binding = JSON.parse(fs.readFileSync(file, "utf8")); } catch (error) { refuse("ASSEMBLY_BINDING_MISSING", `payload binding unreadable: ${file} (${error.code ?? error.message}); run assemble-app --profile release first`); }
  if (binding.devOnly !== false || binding.profile !== "release") refuse("ASSEMBLY_BINDING_DEV", `${file} marks this payload ${binding.profile ?? "unknown"}-only; only a release assembly may be signed for release`);
  if (rehash) {
    for (const [dest, expected] of Object.entries(binding.files ?? {})) {
      let actual;
      try { actual = sha256Of(fs, nativePath(output, dest)); } catch (error) { refuse("ASSEMBLY_BINDING_FILE", `bound file is missing: ${dest} (${error.code ?? error.message})`); }
      if (actual !== expected) refuse("ASSEMBLY_BINDING_HASH", `bound file ${dest} changed after the receipt check (${actual} vs ${expected})`);
    }
  }
  return binding;
}

/** The structural proof of an assembled Windows payload (required files; release: no dev marker). */
export function verifyWindowsPayload({ spec, payload, profile, fs = nodeFs }) {
  const problems = [];
  for (const file of spec.payload.require) if (!fs.existsSync(nativePath(payload, file))) problems.push(`missing ${file}`);
  for (const file of spec.sign) if (!fs.existsSync(nativePath(payload, file))) problems.push(`missing ${file}`);
  if (profile === "release" && spec.devMarker && fs.existsSync(nativePath(payload, spec.devMarker))) problems.push(`${spec.devMarker} marks a dev-only payload`);
  if (problems.length) refuse("ASSEMBLY_VERIFY_FAILED", `${payload}: ${problems.length} problem(s): ${problems.join("; ")}`, { problems });
  return { checked: spec.payload.require.length + spec.sign.length };
}

export const platformPath = nativePath;
