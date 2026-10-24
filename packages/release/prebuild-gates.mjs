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

const BUILTIN_ERRORS = new Set([TypeError, RangeError, ReferenceError, SyntaxError, EvalError, URIError]);

// The text of a thrown value as a finding. A gate refuses by throwing an ordinary Error (its message is the exact refusal
// text); a built-in runtime error (TypeError, ...) is a defect in a gate or its input, so the finding says which gate threw.
function thrownText(name, error) {
  if (error instanceof Error) return BUILTIN_ERRORS.has(error.constructor) ? `gate ${name} threw ${error.name}: ${error.message}` : error.message;
  return `gate ${name} threw a non-Error value: ${String(error)}`;
}

/**
 * Run the pre-build source gates in build order and collect EVERY independent finding (never stop at the first).
 * A gate whose input comes from an earlier gate that failed is not run on that bad input: it is reported as
 * `BLOCKED by <gate>`. An exception inside a gate is a finding, never a crash and never a pass.
 *
 * Dependencies (a gate lists the gates whose output it consumes):
 *   switch-inputs        -
 *   devtools-allowance   -
 *   build-features       switch-inputs, devtools-allowance   (commands and cargo manifest from the scan inputs)
 *   production-mode      -
 *   closure-commands     switch-inputs                       (the recipe commands every closure gate reads)
 *   release-dependencies closure-commands
 *   feature-closure      closure-commands                    (strict only: a dry run only reports the closure problem)
 *   switch-inventory     -                                   (reads switches.json; absent is not a failure here)
 *   source-switch-roots  closure-commands, feature-closure when strict (ios: git + ownership scope + the closure's crates; others: releaseSwitches.nonShipped)
 *   source-switch-prepare source-switch-roots                (ios prepare directories must sit inside the scan roots)
 *   switch-inventory-missing -                               (strict only)
 *   release-switches     closure-commands, devtools-allowance, switch-inventory, source-switch-roots, and feature-closure when strict
 * `detail` is the exact refusal text the build prints (after "right-release: ").
 * `strict` is true for a real release build (a missing switches.json or an unrunnable feature closure fails);
 * a dry run passes false and only reports the closure problem.
 * Also returns `devtoolsAllowance` and `inventoryText` (the switches.json text or null) for the later binary scans.
 * @returns {{ findings: { name: string, detail: string }[], treeText: string | undefined, devtoolsAllowance: object, inventoryText: string | null }}
 */
export function evaluatePrebuildGates({ root, config, target, platform, iosSpec, nativeIos, nativeAssembly, nativeFinalizer, strict = true }) {
  const state = { devtoolsAllowance: { mode: "denied" }, inventoryText: null };
  const findings = [];
  const failed = new Set();
  // Run one gate: `after` names the gates it depends on. Returns { ok, value }; a blocked or failed gate is a finding.
  const gate = (name, after, run) => {
    const blocker = after.find((dependency) => failed.has(dependency));
    if (blocker) {
      failed.add(name);
      findings.push({ name, detail: `BLOCKED by ${blocker}` });
      return { ok: false };
    }
    try {
      return { ok: true, value: run() };
    } catch (error) {
      failed.add(name);
      findings.push({ name, detail: thrownText(name, error) });
      return { ok: false };
    }
  };

  // 1. what the release-switch scan can see: the Rust a build compiles is checked from the manifest it compiles.
  const inputs = gate("switch-inputs", [], () => {
    const switchInputs = releaseSwitchInputs({ appRoot: root, config, target, prepare: iosSpec?.prepare ?? [], nativeIos });
    if (switchInputs.problems.length) throw new Error(`${config.app ?? "app"} ios release-switch scan cannot see what is built:\n- ${switchInputs.problems.join("\n- ")}`);
    return switchInputs;
  });
  const switchInputs = inputs.value;
  // 2. devtools allowance and forbidden build features.
  gate("devtools-allowance", [], () => {
    state.devtoolsAllowance = resolveDevtoolsAllowance({ config, root });
    if (state.devtoolsAllowance.mode === "content-only") console.log(`right-release: devtools allowance recorded: ${JSON.stringify(devtoolsEvidence(state.devtoolsAllowance))}`);
  });
  gate("build-features", ["switch-inputs", "devtools-allowance"], () => {
    assertNoForbiddenBuildFeatures({ root, tier: "release/update", devtools: state.devtoolsAllowance, cargoManifest: switchInputs.cargoManifest, manifest: config.releaseSwitches?.manifest, commands: [
      ...switchInputs.commands,
      { label: "target.prePackage", command: target.prePackage },
      { label: "target.package", command: target.package },
      { label: "nativeAssembly.packageHook", command: nativeAssembly?.packageHook },
      { label: "nativeAssembly.finalizer", command: nativeFinalizer },
      ...commandList(target.postSign).map((command, index) => ({ label: Array.isArray(target.postSign) ? `target.postSign[${index}]` : "target.postSign", command })),
      { label: "target.gradle", command: target.gradle },
    ] });
  });
  // 3. M12: the frontend build resolves in production mode only.
  gate("production-mode", [], () => {
    const modeProblems = productionModeProblems({ appRoot: root });
    if (modeProblems.length) throw new Error(`${config.app ?? "app"} release frontend is not production-only:\n- ${modeProblems.join("\n- ")}`);
  });
  // 4. D4 (M21/M23): dependencies, the resolved feature graph and the source switch inventory.
  let releaseSwitchInventoryText = null;
  const inventory = gate("switch-inventory", [], () => {
    const inventoryPath = path.resolve(root, config.releaseSwitches?.inventory ?? "switches.json");
    return { inventoryPath, text: existsSync(inventoryPath) ? readFileSync(inventoryPath, "utf8") : null };
  });
  const releaseSwitchInventoryPath = inventory.ok ? inventory.value.inventoryPath : path.resolve(root, "switches.json");
  if (inventory.ok) releaseSwitchInventoryText = state.inventoryText = inventory.value.text;
  const commands = gate("closure-commands", ["switch-inputs"], () => {
    const closureCommands = [target.prePackage, target.package, nativeAssembly?.packageHook, nativeFinalizer, ...commandList(target.postSign), target.gradle, ...switchInputs.cargoCommands].filter(Boolean).flatMap((c) => expandRecipeBuildCommands(c, { root }));
    return closureCommands;
  });
  const closureCommands = commands.value;
  gate("release-dependencies", ["closure-commands"], () => {
    assertReleaseDependencies({ root, config, target, commands: closureCommands });
  });
  let treeText;
  gate("feature-closure", ["closure-commands"], () => {
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
  });
  // The scan roots: iOS scans its Xcode project and local packages; other platforms scan the app root minus the declared non-shipped dirs.
  let sourceRoots;
  let skipRoots;
  let repoTop;
  if (releaseSwitchInventoryText != null) {
    gate("source-switch-roots", ["closure-commands", ...(strict ? ["feature-closure"] : [])], () => {
      if (platform === "ios") {
        repoTop = git(root, ["rev-parse", "--show-toplevel"]);
        const swiftSide = (iosOwnershipScope({ repoRoot: repoTop, appRoot: root, target: config.targets?.ios }) ?? []).filter((dir) => dir !== ".rightkit").map((dir) => path.join(repoTop, dir));
        sourceRoots = [...new Set([...swiftSide, ...localCrateDirsFromTree(treeText, repoTop)])].sort();
        console.log(`right-release: source switch scan (ios): ${sourceRoots.map((dir) => path.relative(repoTop, dir) || ".").join(", ")}`);
      } else {
        const declared = validateNonShipped({ root, treeText, nonShipped: config.releaseSwitches?.nonShipped, recipeCommands: closureCommands ?? [] });
        if (declared.findings.length) throw new Error(`source switch inventory: ${declared.findings.length} invalid releaseSwitches.nonShipped entr${declared.findings.length === 1 ? "y" : "ies"}:\n${declared.findings.map((f) => `- ${f.reason}`).join("\n")}`);
        skipRoots = declared.roots;
        if (skipRoots.length) console.log(`right-release: source switch scan skips non-shipped: ${skipRoots.join(", ")}`);
      }
    });
    if (platform === "ios") {
      gate("source-switch-prepare", ["source-switch-roots"], () => {
        for (const step of config.targets?.ios?.iosPackage?.prepare ?? []) {
          const cwd = path.resolve(root, step?.cwd ?? ".");
          if (!sourceRoots.some((dir) => cwd === dir || cwd.startsWith(dir + path.sep))) throw new Error(`source switch inventory: targets.ios.iosPackage.prepare runs in ${path.relative(repoTop, cwd) || "."}, outside the iOS scan roots (${sourceRoots.map((dir) => path.relative(repoTop, dir) || ".").join(", ")}); list that directory in targets.ios.ownershipScope`);
        }
      });
    }
  }
  // The scan itself reads the closure commands, the devtools allowance, the tree (strict: a failed closure is no input) and the roots.
  const scanAfter = ["closure-commands", "devtools-allowance", "switch-inventory", ...(strict ? ["feature-closure"] : []), ...(releaseSwitchInventoryText != null ? ["source-switch-roots"] : [])];
  gate("release-switches", scanAfter, () => {
    const closureInput = { treeText, devtools: state.devtoolsAllowance, remoteHost: config.releaseSwitches?.remoteHost, remoteLoopback: config.releaseSwitches?.remoteLoopback, commands: closureCommands.map((c) => commandTokens(c)) };
    if (releaseSwitchInventoryText != null) {
      assertNoReleaseSwitches({ ...closureInput, sourceRoot: root, sourceRoots, skipRoots, inventoryText: releaseSwitchInventoryText, appForbid: config.releaseSwitches?.forbid ?? [] }, "source switch inventory");
    } else if (treeText != null || closureInput.commands.length) {
      assertNoReleaseSwitches({ ...closureInput, appForbid: config.releaseSwitches?.forbid ?? [] }, "feature closure");
    }
  });
  if (releaseSwitchInventoryText == null && strict && !failed.has("switch-inventory")) {
    gate("switch-inventory-missing", [], () => {
      throw new Error(`source switch inventory: ${path.relative(root, releaseSwitchInventoryPath) || releaseSwitchInventoryPath} is missing; a release build needs the app's switches.json (see packages/release/README.md, "switches.json")`);
    });
  }
  return { findings, treeText, ...state };
}
