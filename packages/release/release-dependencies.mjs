// App-specific release dependency policy. Development builds never call this gate.
import { existsSync } from "node:fs";
import path from "node:path";
import { resolveCargoTree } from "./release-switch-scan.mjs";
import { expandRecipeBuildCommands } from "./recipe-build.mjs";
import { nodeHostTriple } from "./stage-sidecar-command.mjs";
import { resolveNativeFinalizerConfig } from "./native-release-finalization.mjs";

function crateNames(names) {
  if (!Array.isArray(names) || names.some((name) => typeof name !== "string" || !/^[A-Za-z0-9_][A-Za-z0-9_-]*$/.test(name))) {
    throw new TypeError("releaseDependencies.forbiddenCrates must be an array of exact Cargo package names");
  }
  return [...new Set(names)];
}

/** Exact package matches from `cargo tree -e normal --prefix none`, deduplicated across versions. */
export function forbiddenCrateFindings(treeText, forbiddenCrates) {
  const denied = new Set(crateNames(forbiddenCrates));
  const found = new Set();
  for (const line of String(treeText).split(/\r?\n/)) {
    const match = line.match(/^([A-Za-z0-9_][A-Za-z0-9_-]*) v\d[^\s]*(?:\s|$)/);
    if (match && denied.has(match[1])) found.add(match[1]);
  }
  return [...found].sort();
}

/**
 * Enforce config.releaseDependencies for one release target before packaging.
 * `commands` may supply already-expanded release hooks; `run` injects the Cargo runner for tests.
 * Returns { skipped } without Cargo when no policy is declared, else { manifest, target, trees, forbiddenCrates }.
 * Cargo errors, empty output, invalid policy & forbidden packages throw; dry runs obey this gate too.
 */
export function assertReleaseDependencies({ root, config = {}, target = {}, commands, run } = {}) {
  const policy = config.releaseDependencies;
  if (policy === undefined) return { skipped: "no releaseDependencies policy" };
  if (!policy || typeof policy !== "object" || Array.isArray(policy)) throw new TypeError("releaseDependencies must be an object");
  const forbiddenCrates = crateNames(policy.forbiddenCrates);
  if (policy.features !== undefined && (!Array.isArray(policy.features) || policy.features.some((feature) => typeof feature !== "string" || !feature.trim() || feature.startsWith("-")))) {
    throw new TypeError("releaseDependencies.features must be an array of Cargo feature names");
  }
  if (policy.noDefaultFeatures !== undefined && typeof policy.noDefaultFeatures !== "boolean") throw new TypeError("releaseDependencies.noDefaultFeatures must be boolean");
  const manifest = policy.manifest ?? target.cargoManifest ?? config.releaseSwitches?.manifest ?? config.nativeAssembly?.cargoManifest ?? config.nativeAssembly?.manifest;
  if (manifest !== undefined && (typeof manifest !== "string" || !manifest || !existsSync(path.resolve(root, manifest)))) {
    throw new Error(`release dependencies: declared Cargo manifest does not exist: ${manifest}`);
  }
  const triple = target.cargoTarget ?? target.targetTriple ?? target.rustTarget ?? nodeHostTriple();
  const commandList = (value) => value == null ? [] : Array.isArray(value) ? value : [value];
  const hooks = commands ?? [target.prePackage, target.package, config.nativeAssembly?.packageHook,
    resolveNativeFinalizerConfig(config.nativeAssembly), ...commandList(target.postSign), target.gradle].filter(Boolean);
  const closure = resolveCargoTree({
    root, manifest: manifest === undefined ? undefined : path.relative(root, path.resolve(root, manifest)), features: [...(config.releaseSwitches?.features ?? []), ...(policy.features ?? [])],
    noDefaultFeatures: policy.noDefaultFeatures ?? false,
    triple, commandTexts: hooks.flatMap((command) => expandRecipeBuildCommands(command, { root })),
    edges: "normal", prefix: "none", label: "release dependencies", run,
  });
  if (!/^[A-Za-z0-9_][A-Za-z0-9_-]* v\d[^\s]*(?:\s|$)/m.test(closure.treeText)) throw new Error("release dependencies: cargo tree returned no package records");
  const found = forbiddenCrateFindings(closure.treeText, forbiddenCrates);
  if (found.length) throw new Error(found.map((crate) => `FAIL: ${crate} is in the release dependency tree (${triple})`).join("\n"));
  return { manifest: closure.manifest, target: triple, trees: closure.trees.length, forbiddenCrates };
}
