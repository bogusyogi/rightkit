// One resolver for every declared artifact path (item 7 row 7.04c; signed/item7-sage-decisions-3.md R-T1 to R-T5; docs/GATE-CONTRACT.md
// "Commit-only build" and "Output directory & cache").
//
// A release config names its outputs relative to the app: `src-tauri/target/release/bundle/...`, a root `App.dmg`. Checkout mode reaches
// them through the app root (and the managed artifact directory), exactly as it always did. A door split has no `target/` in its
// snapshot (a link into the editable checkout is refused, GC:24, and a copy is the rightkit#924 bridge), so the same declared path must
// mean the Cargo target the gate bound for that manifest. This module is the one place that decides which:
//
//   checkout  today's `resolveReleaseArtifactPath`, byte for byte: `<app root>/<declared>` when it exists, else
//             `RIGHT_RELEASE_ARTIFACT_DIR/<basename>` when that exists, else `<app root>/<declared>`.
//   split     R-T2:
//     1. A relative path starting with a target prefix maps to `<bound target>/<rest>`. No fallback to the snapshot, the checkout or the artifact directory.
//     2. An absolute path is accepted only inside a bound target or inside RIGHT_RELEASE_ARTIFACT_DIR (realpath; case-folded on Windows).
//        Anything else is RK_SPLIT_TARGET_PATH, a path into the editable checkout other than its bound target included.
//     3. A relative path with a `..` segment is refused.
//     4. Any other relative path resolves to `<snapshot app root>/P` when it exists, then `RIGHT_RELEASE_ARTIFACT_DIR/<basename>` when
//        that exists, else `<snapshot app root>/P`. It never resolves to the checkout app root: a stale DMG left in the editable checkout
//        must never be sealed.
//     5. Outputs written under rule 4 into the snapshot are Q1 `generated` ledger entries (stage-ledger.mjs); the door refuses a
//        non-ignored one at the end of the stage.
//
// The bound target of a Cargo manifest is `resolveTargetRoot(<snapshot manifest>)` (cargo-target.mjs, the published resolver), resolved
// once. On Linux it equals the absolute CARGO_TARGET_DIR the gate resolved in the checkout and passed (row 7.09); on Mac and Dell it is the
// snapshot carrier's answer. A manifest whose target cannot be resolved is RK_SPLIT_TARGET_UNRESOLVED: there is never a snapshot-relative
// `target/` fallback (R7). The door cross-checks the recorded targets against its own resolution (RK_SPLIT_TARGET_DRIFT).
import { existsSync, realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { resolveTargetRoot as cargoResolveTargetRoot } from "./cargo-target.mjs";
import { expandRecipeBuildCommands } from "./recipe-build.mjs";
import { SourceSplitError, readSourceSplit } from "./source-split.mjs";

export const SPLIT_TARGET_PATH_CODE = "RK_SPLIT_TARGET_PATH";
export const SPLIT_TARGET_UNRESOLVED_CODE = "RK_SPLIT_TARGET_UNRESOLVED";
export const SPLIT_TARGET_DRIFT_CODE = "RK_SPLIT_TARGET_DRIFT";

const DEFAULT_MANIFEST = "src-tauri/Cargo.toml";
const TOOL_ROOT = path.dirname(fileURLToPath(import.meta.url));

/** A declared artifact path or bound target a split refuses. `code` is one of the three codes above. */
export class ArtifactPathError extends Error {
  constructor(code, message, problems = []) { super(`${code}: ${message}`); this.name = "ArtifactPathError"; this.code = code; this.problems = problems; }
}

const posix = (value) => String(value).replaceAll("\\", "/");
const folded = (value) => (process.platform === "win32" ? value.toLowerCase() : value);
const isAbsoluteDeclared = (value) => path.isAbsolute(value) || /^[A-Za-z]:[\\/]/.test(value) || /^[\\/]{2}/.test(value);

/** The real path of the deepest existing ancestor of `file`, with the missing tail appended: comparable before the file exists. */
function canonical(file) {
  const absolute = path.resolve(file);
  let head = absolute;
  const tail = [];
  for (;;) {
    try { return folded(path.join(realpathSync.native(head), ...tail.reverse())); } catch { /* not there yet */ }
    const parent = path.dirname(head);
    if (parent === head) return folded(absolute);
    tail.push(path.basename(head));
    head = parent;
  }
}

const insideCanonical = (child, parent) => {
  const relative = path.relative(parent, child);
  return relative === "" || (relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative));
};

/** True when this process is a child of a door run (`RK_GATE_CHECKOUT` names the checkout): artifacts resolve through the split. */
export const isSplitArtifactEnv = (env = process.env) => typeof env?.RK_GATE_CHECKOUT === "string" && env.RK_GATE_CHECKOUT !== "";

/** `<dir>/` form, posix, relative: the prefix a declared path starts with. */
function prefixOf(appRoot, dir) {
  const relative = posix(path.relative(appRoot, dir)).replace(/\/+$/, "");
  return relative && relative !== "." && !relative.startsWith("..") ? `${relative}/` : "";
}

/**
 * Every Cargo manifest the app declares, with the target prefix its declared paths use (R-T1): the native layout's prefixes, each recipe
 * `cargoTargetRoot` manifest, and the default `<cargoDir>/target/`. `[{ prefix, manifest }]`, longest prefix first, one row per prefix.
 */
export function declaredTargetManifests({ appRoot, nativeLayout = null, target = null, exists = existsSync } = {}) {
  const rows = [];
  const add = (prefix, manifest) => { if (prefix && !rows.some((row) => row.prefix === prefix)) rows.push({ prefix, manifest: path.resolve(manifest) }); };
  if (nativeLayout?.manifestPath) for (const prefix of nativeLayout.targetPrefixes ?? [nativeLayout.targetPrefix]) add(posix(prefix ?? "").replace(/\/*$/, "/").replace(/^\/$/, ""), nativeLayout.manifestPath);
  // A declarative recipe: `cargoTargetRoot: true` is `<tauriCwd>/src-tauri/Cargo.toml`, a string is an app-relative Cargo.toml.
  const targets = Array.isArray(target) ? target : [target];
  for (const spec of targets.flatMap((entry) => [entry?.winPackage, entry?.macPackage])) {
    if (!spec || typeof spec !== "object") continue;
    const explicit = spec.cargoTargetRoot;
    if (explicit !== true && typeof explicit !== "string") continue;
    const cwd = typeof spec.tauriCwd === "string" && spec.tauriCwd !== "." ? posix(spec.tauriCwd) : "";
    const manifestRel = typeof explicit === "string" ? posix(explicit) : path.posix.join(cwd, DEFAULT_MANIFEST);
    const manifest = path.resolve(appRoot, manifestRel);
    add(prefixOf(appRoot, path.join(path.dirname(manifest), "target")), manifest);
  }
  // The historical default: an app with `src-tauri/Cargo.toml` and no declared layout still addresses `src-tauri/target/`.
  const fallback = path.resolve(appRoot, DEFAULT_MANIFEST);
  if (!rows.length && exists(fallback)) add("src-tauri/target/", fallback);
  return rows.sort((left, right) => right.prefix.length - left.prefix.length);
}

/**
 * The artifact-path context of this process.
 *   `{ mode: "checkout", appRoot, artifactDir, env }`                                   (no door run: nothing changes)
 *   `{ mode: "split", snapshotAppRoot, boundTargets: [{ prefix, manifest, dir }], artifactDir, ... }`
 * `cwd` is the app root (the snapshot's in a split). `nativeLayout` is resolveNativeCargoLayout's answer (null for a Cargo-less app), `target`
 * the UNEXPANDED `config.targets[platform]` (the recipe keys are gone after expansion; a list of targets binds each). `resolve: false` skips the Cargo calls (the prefixes
 * alone, for the command scan). `resolveTargetRoot`, `exists` and `snapshotRoot` are injectable.
 * Throws ArtifactPathError RK_SPLIT_TARGET_UNRESOLVED when a manifest's target cannot be resolved or resolves inside the snapshot, and
 * RK_SPLIT_TARGET_DRIFT when it differs from the absolute CARGO_TARGET_DIR the gate handed this process.
 */
export function splitArtifactContext({
  cwd, env = process.env, nativeLayout = null, config = {}, target = null, resolveTargetRoot = cargoResolveTargetRoot,
  exists = existsSync, snapshotRoot, resolve = true,
} = {}) {
  void config;
  if (!cwd) throw new TypeError("splitArtifactContext requires cwd (the app root)");
  const appRoot = path.resolve(cwd);
  const artifactDir = env.RIGHT_RELEASE_ARTIFACT_DIR ? path.resolve(env.RIGHT_RELEASE_ARTIFACT_DIR) : null;
  if (!isSplitArtifactEnv(env)) return Object.freeze({ mode: "checkout", appRoot, snapshotAppRoot: null, boundTargets: Object.freeze([]), prefixes: Object.freeze([]), artifactDir, env, exists });
  const declared = declaredTargetManifests({ appRoot, nativeLayout, target, exists });
  let root = snapshotRoot ? path.resolve(snapshotRoot) : null;
  if (!root) { try { root = readSourceSplit({ env, cwd: appRoot })?.snapshot ?? null; } catch (error) { if (!(error instanceof SourceSplitError)) throw error; } }
  const snapshot = canonical(root ?? appRoot);
  const pinned = typeof env.CARGO_TARGET_DIR === "string" && path.isAbsolute(env.CARGO_TARGET_DIR) ? canonical(env.CARGO_TARGET_DIR) : null;
  const cache = new Map();
  const boundTargets = declared.map(({ prefix, manifest }) => {
    if (!resolve) return Object.freeze({ prefix, manifest, dir: null });
    if (!cache.has(manifest)) cache.set(manifest, bindManifest({ manifest, appRoot, snapshot, pinned, resolveTargetRoot, exists }));
    return Object.freeze({ prefix, manifest, dir: cache.get(manifest) });
  });
  return Object.freeze({
    mode: "split", appRoot, snapshotAppRoot: appRoot, snapshotRoot: root ?? appRoot, boundTargets: Object.freeze(boundTargets),
    prefixes: Object.freeze(declared.map((row) => row.prefix)), artifactDir, env, exists,
  });
}

function bindManifest({ manifest, appRoot, snapshot, pinned, resolveTargetRoot, exists }) {
  if (!exists(manifest)) throw new ArtifactPathError(SPLIT_TARGET_UNRESOLVED_CODE, `the Cargo manifest ${manifest} is not in the snapshot, so its target directory cannot be bound; a split never falls back to a snapshot-relative target/`);
  let dir;
  try { dir = resolveTargetRoot(manifest); } catch (error) {
    throw new ArtifactPathError(SPLIT_TARGET_UNRESOLVED_CODE, `cargo metadata for ${manifest} reported no target directory (${error.message}); a split never falls back to a snapshot-relative target/ (the snapshot carrier must serve cargo metadata for a split child)`);
  }
  if (typeof dir !== "string" || !path.isAbsolute(dir)) throw new ArtifactPathError(SPLIT_TARGET_UNRESOLVED_CODE, `cargo metadata for ${manifest} did not report an absolute target directory`);
  const real = canonical(dir);
  if (insideCanonical(real, snapshot) || insideCanonical(real, canonical(appRoot))) {
    throw new ArtifactPathError(SPLIT_TARGET_UNRESOLVED_CODE, `the Cargo target of ${manifest} resolves inside the snapshot (${dir}); the gate binds a split's target outside it (CARGO_TARGET_DIR, row 7.09) and never to a snapshot-relative target/`);
  }
  if (pinned && real !== pinned) throw new ArtifactPathError(SPLIT_TARGET_DRIFT_CODE, `the Cargo target of ${manifest} is ${dir}, not the CARGO_TARGET_DIR the gate bound for this stage`);
  return path.resolve(dir);
}

/**
 * The context for a RightKit subcommand that gets only its argv and cwd (create-mac-updater, sign-updater, mirror-root-artifact, win-package-recipe):
 * it reads the release config in `cwd`, so the target prefixes are the app's own, for every platform's target. Checkout mode needs no config.
 */
export async function loadSplitArtifactContext({ cwd = process.cwd(), env = process.env, configName, ...inject } = {}) {
  if (!isSplitArtifactEnv(env)) return splitArtifactContext({ cwd, env, ...inject });
  const { defaultReleaseConfigFile, importReleaseConfig } = await import("./release-config.mjs");
  const { resolveNativeCargoLayout, nativeCargoDeclared } = await import("./native-cargo-layout.mjs");
  const appRoot = path.resolve(cwd);
  let config = {};
  try { config = await importReleaseConfig(path.resolve(appRoot, configName ?? defaultReleaseConfigFile(appRoot)), { lenient: true }); } catch { /* an app with no readable config binds the default manifest only */ }
  let nativeLayout = null;
  try { if (nativeCargoDeclared({ appRoot, config })) nativeLayout = resolveNativeCargoLayout({ appRoot, config }); } catch { nativeLayout = null; }
  return splitArtifactContext({ cwd: appRoot, env, nativeLayout, config, target: Object.values(config?.targets ?? {}), ...inject });
}

const insideBound = (file, ctx) => ctx.boundTargets.find((bound) => bound.dir && insideCanonical(canonical(file), canonical(bound.dir)));

/**
 * The file a declared artifact path means (R-T2). Checkout mode is the historical rule; see the module header for the split rules.
 * Throws ArtifactPathError RK_SPLIT_TARGET_PATH for a split path the rules refuse.
 */
export function resolveArtifactPath(declared, ctx, { write = false } = {}) {
  if (typeof declared !== "string" || !declared) throw new TypeError("a declared artifact path is required");
  if (!ctx || (ctx.mode !== "split" && ctx.mode !== "checkout")) throw new TypeError("resolveArtifactPath requires a context from splitArtifactContext");
  if (ctx.mode === "checkout") {
    const local = path.resolve(ctx.appRoot, declared);
    if (ctx.exists(local)) return local;
    const managed = ctx.env.RIGHT_RELEASE_ARTIFACT_DIR;
    if (managed) {
      const candidate = path.join(path.resolve(managed), path.basename(declared));
      if (ctx.exists(candidate)) return candidate;
    }
    return local;
  }
  const normalized = posix(declared);
  if (isAbsoluteDeclared(declared)) {
    const absolute = path.resolve(declared);
    if (insideBound(absolute, ctx)) return absolute;
    if (ctx.artifactDir && insideCanonical(canonical(absolute), canonical(ctx.artifactDir))) return absolute;
    throw new ArtifactPathError(SPLIT_TARGET_PATH_CODE, `${declared} is neither inside a bound Cargo target (${ctx.boundTargets.map((bound) => bound.dir).filter(Boolean).join(", ") || "none"}) nor inside RIGHT_RELEASE_ARTIFACT_DIR; a split reads declared artifacts only there, never from the editable checkout or an unrelated directory`);
  }
  if (normalized.split("/").includes("..")) throw new ArtifactPathError(SPLIT_TARGET_PATH_CODE, `${declared} contains a .. segment; a split resolves a declared artifact path only below the snapshot app root or a bound target`);
  const relative = path.posix.normalize(normalized).replace(/^\.\//, "");
  for (const bound of ctx.boundTargets) {
    const prefix = bound.prefix.replace(/\/+$/, "");
    if (relative === prefix || relative.startsWith(`${prefix}/`)) {
      if (!bound.dir) throw new ArtifactPathError(SPLIT_TARGET_UNRESOLVED_CODE, `${declared} is below ${prefix}/, whose Cargo target was not resolved`);
      return path.join(bound.dir, relative.slice(prefix.length).replace(/^\/+/, ""));
    }
  }
  const local = path.resolve(ctx.snapshotAppRoot, relative);
  // A writer (a file the step is about to create) never lands in the managed artifact directory by accident of a stale copy there.
  if (write) return local;
  if (ctx.exists(local)) {
    // A link out of the snapshot would be a checkout read in disguise.
    if (!insideCanonical(canonical(local), canonical(ctx.snapshotRoot ?? ctx.snapshotAppRoot))) throw new ArtifactPathError(SPLIT_TARGET_PATH_CODE, `${declared} resolves outside the snapshot (${local}); a split never reads the editable checkout`);
    return local;
  }
  if (ctx.artifactDir) {
    const candidate = path.join(ctx.artifactDir, path.basename(relative));
    if (ctx.exists(candidate)) return candidate;
  }
  return local;
}

/**
 * Where a step writes a declared output (create-mac-updater's tarball, a DMG): the same refusals and target mapping as `resolveArtifactPath`, but a
 * relative non-target path is always `<snapshot app root>/P`. Checkout mode is `path.resolve(app root, declared)`, as those writers always did.
 */
export function resolveArtifactWritePath(declared, ctx) {
  if (ctx?.mode !== "split") return path.resolve(ctx?.appRoot ?? process.cwd(), declared);
  return resolveArtifactPath(declared, ctx, { write: true });
}

/** What the sealed manifest records so the door can cross-check it (`boundTargets`, R-T1): `[{ prefix, manifest, dir }]`, manifest posix-relative to the app. */
export function boundTargetsEvidence(ctx) {
  if (ctx?.mode !== "split") return [];
  return ctx.boundTargets.map((bound) => ({ prefix: bound.prefix, manifest: posix(path.relative(ctx.snapshotAppRoot, bound.manifest)), dir: bound.dir }));
}

/**
 * The door's cross-check (R-T1, 7.08): every target the child recorded must be the one the door resolved itself (`expected`, absolute
 * directories: on Linux the gate's 7.09 resolution, on Mac and Dell the verified receipts' `outputDir`s). Returns problems, [] when equal.
 * A recorded list that is missing or empty while the door expected a target is a problem too: the child's value is never trusted alone.
 */
export function crossCheckBoundTargets({ recorded, expected }) {
  const want = [...new Set((expected ?? []).filter(Boolean).map((dir) => canonical(dir)))];
  const got = Array.isArray(recorded) ? recorded.map((entry) => (entry?.dir ? canonical(entry.dir) : null)) : [];
  const problems = [];
  if (want.length && !got.length) problems.push(`the sealed manifest records no bound Cargo target, but the gate bound ${want.join(", ")}`);
  for (const [index, dir] of got.entries()) {
    if (!dir) problems.push(`boundTargets[${index}] records no directory`);
    else if (!want.includes(dir)) problems.push(`boundTargets[${index}] is ${recorded[index].dir}, not the directory the gate bound (${want.join(", ") || "none"})`);
  }
  return problems;
}

// ---- R-T4: no target-prefixed relative path in a command a RightKit entry does not own -------------------------------------------

const RIGHTKIT_BINS = new Set(["right-release", "rightkit-qa"]);
const RIGHTKIT_SCRIPTS = new Set(["recipe-build.mjs", "create-mac-updater.mjs", "sign-updater.mjs", "mirror-root-artifact.mjs", "win-package-recipe.mjs", "mac-package-recipe.mjs", "right-release.mjs"]);
const binName = (token) => path.posix.basename(posix(token)).replace(/\.(?:cmd|exe|ps1)$/i, "");

/** True for `right-release ...`, `rightkit-qa ...`, `pnpm exec right-release ...` and `node <this package's own script> ...`: commands RightKit resolves itself. */
export function isRightKitEntry(command) {
  const cmd = typeof command === "string" ? command : command?.cmd;
  const args = typeof command === "string" ? [] : (command?.args ?? []).map(String);
  if (typeof cmd !== "string") return false;
  if (RIGHTKIT_BINS.has(binName(cmd))) return true;
  if (["pnpm", "npx"].includes(binName(cmd)) && args[0] === "exec" && RIGHTKIT_BINS.has(binName(args[1] ?? ""))) return true;
  if (["pnpm", "npx"].includes(binName(cmd)) && RIGHTKIT_BINS.has(binName(args[0] ?? ""))) return true;
  if (binName(cmd) === "node" && args[0]) {
    const script = path.resolve(args[0]);
    if (!RIGHTKIT_SCRIPTS.has(path.basename(script))) return false;
    const dir = path.dirname(script);
    return dir === TOOL_ROOT || dir === path.join(TOOL_ROOT, "cli") || /[\\/]@rightkit[\\/]release(?:[\\/]cli)?$/.test(dir);
  }
  return false;
}

const escapeRegExp = (value) => String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** The first target-prefixed relative path in `text` (at token start, or after `=`, a quote, `:`, `,` or whitespace; `\` read as `/`), or null. */
export function findTargetPrefixedPath(text, prefixes) {
  const value = posix(text);
  for (const prefix of prefixes) {
    const stem = escapeRegExp(prefix.replace(/\/+$/, ""));
    const match = new RegExp(`(?:^|[=:"',\\s])(?:\\./)?(${stem}(?:/[^\\s"',]*)?)(?=$|[\\s"',])`).exec(value);
    if (match) return match[1];
  }
  return null;
}

const COMMAND_KEYS = ["prePackage", "package", "postBuild", "postSign", "preSeal", "publish"];

/**
 * Every command step a release runs for `target` (R-T4): prePackage, package, postBuild, postSign, preSeal and publish (each one command or a
 * list), the native package hook, and the prepare / postBuild steps inside a declarative recipe's `recipe-build` plan (the wrapper itself is a
 * RightKit entry; the steps it runs are the app's). `[{ name, cmd, args?, env? }]`. A recipe plan that cannot be read throws: fail closed.
 */
export function releaseCommandSteps({ target, config = {}, root = process.cwd() }) {
  const steps = [];
  const add = (name, command) => {
    if (!command || typeof command !== "object") return;
    for (const [index, step] of expandRecipeBuildCommands(command, { root }).entries()) steps.push({ name: index === 0 ? name : `${name} (recipe step ${index})`, ...step });
  };
  for (const key of COMMAND_KEYS) {
    const value = target?.[key];
    (Array.isArray(value) ? value : [value]).forEach((command, index, list) => add(list.length > 1 ? `${key}[${index}]` : key, command));
  }
  add("nativeAssembly.packageHook", config?.nativeAssembly?.packageHook);
  // A declarative recipe still unexpanded (the controller reads the raw config): the steps it will run around the Tauri build.
  for (const recipe of ["winPackage", "macPackage"]) {
    for (const phase of ["prepare", "postBuild"]) {
      const list = target?.[recipe]?.[phase];
      if (Array.isArray(list)) list.forEach((command, index) => add(`${recipe}.${phase}[${index}]`, command));
    }
  }
  return steps;
}

/**
 * R-T4: a split's command step must not name a target-prefixed relative path (`src-tauri/target/...`): the snapshot has no such directory, and a
 * non-RightKit command cannot be remapped. `steps` are `{ name?, cmd, args?, env? }` (or a command string); RightKit entries are skipped.
 * Throws ArtifactPathError RK_SPLIT_TARGET_PATH naming each step and the fix, before anything builds. A checkout-mode context checks nothing.
 */
export function assertSplitCommandPaths(steps, ctx) {
  if (!ctx || ctx.mode !== "split") return;
  const prefixes = ctx.prefixes ?? ctx.boundTargets.map((bound) => bound.prefix);
  if (!prefixes.length) return;
  const problems = [];
  steps.forEach((step, index) => {
    const command = typeof step === "string" ? { cmd: step } : step;
    if (!command || isRightKitEntry(command)) return;
    const name = command.name ?? `step ${index + 1}`;
    const values = [command.cmd, ...(command.args ?? []), ...Object.values(command.env ?? {})].filter((value) => typeof value === "string");
    for (const value of values) {
      const hit = findTargetPrefixedPath(value, prefixes);
      if (hit) {
        problems.push(`${name} (${command.cmd}) names the target-relative path ${hit}`);
        break;
      }
    }
  });
  if (problems.length) {
    throw new ArtifactPathError(SPLIT_TARGET_PATH_CODE, `${problems.join("; ")}. A door split has no target directory in its snapshot, and only RightKit's own steps are remapped. Pass the path absolute from resolveTargetRoot (@rightkit/release/cargo-target.mjs), as postright and cutright do; nothing was built`, problems);
  }
}
