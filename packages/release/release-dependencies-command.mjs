#!/usr/bin/env node
import path from "node:path";
import { fileURLToPath } from "node:url";
import { defaultReleaseConfigFile, importReleaseConfig } from "./release-config.mjs";
import { assertReleaseDependencies } from "./release-dependencies.mjs";
import { nodeHostTriple } from "./stage-sidecar-command.mjs";
import { expandPackageRecipeTarget } from "./package-recipe-targets.mjs";

/** Standalone policy check: host target by default, all declared release targets on request. */
export async function main(argv, { cwd = process.cwd(), platform = process.platform, arch = process.arch, run } = {}) {
  let configPath = defaultReleaseConfigFile(cwd), selected, allTargets = false;
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (["--config", "--platform"].includes(arg)) {
      const value = argv[++i];
      if (!value || value.startsWith("--")) throw new Error(`${arg} needs a value`);
      if (arg === "--config") configPath = value; else selected = value;
    } else if (arg === "--all-targets") allTargets = true;
    else throw new Error(`unknown check-release-dependencies argument: ${arg}`);
  }
  if (allTargets && selected) throw new Error("use --platform or --all-targets");
  const file = path.resolve(cwd, configPath);
  const root = path.dirname(file);
  const config = await importReleaseConfig(file);
  if (config?.releaseDependencies === undefined) throw new Error("check-release-dependencies requires releaseDependencies in config");
  const host = { darwin: "mac", win32: "win", linux: "linux" }[platform];
  const platforms = allTargets ? Object.keys(config.targets ?? {}) : [selected ?? host];
  if (!platforms.length) throw new Error("no release targets declared");
  const results = [];
  for (const name of platforms) {
    let target = config.targets?.[name];
    if (!name || !target) throw new Error(`no release target for ${name ?? platform}; use --platform or --all-targets`);
    target = expandPackageRecipeTarget(target, { platform: name, appRoot: root, config });
    if (name !== host && !(target.cargoTarget ?? target.targetTriple ?? target.rustTarget)) throw new Error(`${name} must declare cargoTarget, targetTriple or rustTarget`);
    if (!allTargets && !selected) target = { ...target, cargoTarget: nodeHostTriple(platform, arch) };
    results.push(assertReleaseDependencies({ root, config, target, run }));
  }
  return results;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) {
  main(process.argv.slice(2)).then((results) => console.log(JSON.stringify(results, null, 2))).catch((error) => {
    console.error(`right-release check-release-dependencies: ${error.message}`);
    process.exitCode = 1;
  });
}
