// Source-level release gates that run before a build starts. The build (release.mjs) and `doctor --gates` call the
// same function, so the doctor can never pass a config the build refuses. Gate order is part of the contract.
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { assertReleaseDependencies } from "./release-dependencies.mjs";
import { assertNoForbiddenBuildFeatures } from "./qa-build-features.mjs";
import { expandRecipeBuildCommands } from "./recipe-build.mjs";
import { assertNoReleaseSwitches, commandTokens, localCrateDirsFromTree, resolveFeatureClosure, shippedRustTriples, validateNonShipped } from "./release-switch-scan.mjs";
import { iosOwnershipScope } from "./ownership-scope.mjs";
import { devtoolsEvidence, resolveDevtoolsAllowance } from "./devtools-policy.mjs";
import { releaseSwitchInputs } from "./ios-build-context.mjs";
import { productionModeProblems } from "./production-mode.mjs";

function commandList(value) {
  if (value == null) return [];
  return Array.isArray(value) ? value : [value];
}

function git(cwd, runArgs) {
  const result = spawnSync("git", runArgs, { cwd, encoding: "utf8", windowsHide: true });
  if (result.status !== 0) throw new Error(`git ${runArgs.join(" ")} failed: ${(result.stderr ?? result.stdout ?? "").trim()}`);
  return result.stdout.trim();
}

/**
 * Run the pre-build source gates in build order and stop at the first finding.
 * `detail` is the exact refusal text the build prints (after "right-release: ").
 * `strict` is true for a real release build (a missing switches.json or an unrunnable feature closure fails);
 * a dry run passes false and only reports the closure problem.
 * Also returns `devtoolsAllowance` and `inventoryText` (the switches.json text or null) for the later binary scans.
 * @returns {{ findings: { name: string, detail: string }[], treeText: string | undefined, devtoolsAllowance: object, inventoryText: string | null }}
 */
export function evaluatePrebuildGates({ root, config, target, platform, iosSpec, nativeIos, nativeAssembly, nativeFinalizer, strict = true }) {
  const state = { devtoolsAllowance: { mode: "denied" }, inventoryText: null };
  const out = (findings, treeText) => ({ findings, treeText, ...state });
  // 1. what the release-switch scan can see: the Rust a build compiles is checked from the manifest it compiles.
  const switchInputs = releaseSwitchInputs({ appRoot: root, config, target, prepare: iosSpec?.prepare ?? [], nativeIos });
  if (switchInputs.problems.length) {
    return out([{ name: "switch-inputs", detail: `${config.app ?? "app"} ios release-switch scan cannot see what is built:\n- ${switchInputs.problems.join("\n- ")}` }]);
  }
  // 2. devtools allowance and forbidden build features.
  let devtoolsAllowance = state.devtoolsAllowance;
  try {
    devtoolsAllowance = state.devtoolsAllowance = resolveDevtoolsAllowance({ config, root });
    if (devtoolsAllowance.mode === "content-only") console.log(`right-release: devtools allowance recorded: ${JSON.stringify(devtoolsEvidence(devtoolsAllowance))}`);
    assertNoForbiddenBuildFeatures({ root, tier: "release/update", devtools: devtoolsAllowance, cargoManifest: switchInputs.cargoManifest, manifest: config.releaseSwitches?.manifest, commands: [
      ...switchInputs.commands,
      { label: "target.prePackage", command: target.prePackage },
      { label: "target.package", command: target.package },
      { label: "nativeAssembly.packageHook", command: nativeAssembly?.packageHook },
      { label: "nativeAssembly.finalizer", command: nativeFinalizer },
      ...commandList(target.postSign).map((command, index) => ({ label: Array.isArray(target.postSign) ? `target.postSign[${index}]` : "target.postSign", command })),
      { label: "target.gradle", command: target.gradle },
    ] });
  } catch (error) { return out([{ name: "build-features", detail: error.message }]); }
  // 3. M12: the frontend build resolves in production mode only.
  {
    const modeProblems = productionModeProblems({ appRoot: root });
    if (modeProblems.length) return out([{ name: "production-mode", detail: `${config.app ?? "app"} release frontend is not production-only:\n- ${modeProblems.join("\n- ")}` }]);
  }
  // 4. D4 (M21/M23): dependencies, the resolved feature graph and the source switch inventory.
  const releaseSwitchInventoryPath = path.resolve(root, config.releaseSwitches?.inventory ?? "switches.json");
  const releaseSwitchInventoryText = state.inventoryText = existsSync(releaseSwitchInventoryPath) ? readFileSync(releaseSwitchInventoryPath, "utf8") : null;
  let treeText;
  let stage = "release-dependencies";
  try {
    const closureCommands = [target.prePackage, target.package, nativeAssembly?.packageHook, nativeFinalizer, ...commandList(target.postSign), target.gradle, ...switchInputs.cargoCommands].filter(Boolean).flatMap((c) => expandRecipeBuildCommands(c, { root }));
    assertReleaseDependencies({ root, config, target, commands: closureCommands });
    stage = "feature-closure";
    try {
      const closure = resolveFeatureClosure({
        root,
        manifest: config.releaseSwitches?.manifest,
        cargoManifest: switchInputs.cargoManifest,
        rust: config.releaseSwitches?.rust,
        features: config.releaseSwitches?.features ?? [],
        // The shipped triples (never "all": the build broker refuses it); evaluated on the raw config target.
        triples: () => shippedRustTriples({ platform, target: config.targets?.[platform] }),
        commandTexts: closureCommands,
      });
      if (closure.skipped) console.log(`right-release: feature closure skipped (${closure.skipped})`);
      else treeText = closure.treeText;
    } catch (error) {
      if (strict) throw error;
      console.log(`right-release: dry run: ${error.message}`);
    }
    stage = "release-switches";
    const closureInput = { treeText, devtools: devtoolsAllowance, remoteHost: config.releaseSwitches?.remoteHost, remoteLoopback: config.releaseSwitches?.remoteLoopback, commands: closureCommands.map((c) => commandTokens(c)) };
    if (releaseSwitchInventoryText != null) {
      let sourceRoots;
      if (platform === "ios") {
        const repoTop = git(root, ["rev-parse", "--show-toplevel"]);
        const swiftSide = (iosOwnershipScope({ repoRoot: repoTop, appRoot: root, target: config.targets?.ios }) ?? []).filter((dir) => dir !== ".rightkit").map((dir) => path.join(repoTop, dir));
        sourceRoots = [...new Set([...swiftSide, ...localCrateDirsFromTree(treeText, repoTop)])].sort();
        for (const step of config.targets?.ios?.iosPackage?.prepare ?? []) {
          const cwd = path.resolve(root, step?.cwd ?? ".");
          if (!sourceRoots.some((dir) => cwd === dir || cwd.startsWith(dir + path.sep))) throw new Error(`source switch inventory: targets.ios.iosPackage.prepare runs in ${path.relative(repoTop, cwd) || "."}, outside the iOS scan roots (${sourceRoots.map((dir) => path.relative(repoTop, dir) || ".").join(", ")}); list that directory in targets.ios.ownershipScope`);
        }
        console.log(`right-release: source switch scan (ios): ${sourceRoots.map((dir) => path.relative(repoTop, dir) || ".").join(", ")}`);
      }
      // Non-iOS: the whole app root minus the app's declared non-shipped tooling dirs (releaseSwitches.nonShipped, validated).
      let skipRoots;
      if (platform !== "ios") {
        const declared = validateNonShipped({ root, treeText, nonShipped: config.releaseSwitches?.nonShipped, recipeCommands: closureCommands });
        if (declared.findings.length) throw new Error(`source switch inventory: ${declared.findings.length} invalid releaseSwitches.nonShipped entr${declared.findings.length === 1 ? "y" : "ies"}:\n${declared.findings.map((f) => `- ${f.reason}`).join("\n")}`);
        skipRoots = declared.roots;
        if (skipRoots.length) console.log(`right-release: source switch scan skips non-shipped: ${skipRoots.join(", ")}`);
      }
      assertNoReleaseSwitches({ ...closureInput, sourceRoot: root, sourceRoots, skipRoots, inventoryText: releaseSwitchInventoryText, appForbid: config.releaseSwitches?.forbid ?? [] }, "source switch inventory");
    } else if (treeText != null || closureInput.commands.length) {
      assertNoReleaseSwitches({ ...closureInput, appForbid: config.releaseSwitches?.forbid ?? [] }, "feature closure");
    }
    if (releaseSwitchInventoryText == null && strict) {
      stage = "switch-inventory-missing";
      throw new Error(`source switch inventory: ${path.relative(root, releaseSwitchInventoryPath) || releaseSwitchInventoryPath} is missing; a release build needs the app's switches.json (see packages/release/README.md, "switches.json")`);
    }
  } catch (error) { return out([{ name: stage, detail: error.message }], treeText); }
  return out([], treeText);
}
