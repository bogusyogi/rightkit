#!/usr/bin/env node
// `right-release stage-sidecar [--config right-release.config.mjs] [--target <triple>] [--check] [--json]` (bogusyogi/rightkit#345):
// stages the config's declarative `sidecars` into src-tauri/binaries with Tauri externalBin naming. `--check` validates
// the manifest and prints the plan without building, downloading, copying or deleting anything (for template gates).
import path from "node:path";
import { pathToFileURL } from "node:url";
import { planSidecarStaging, resolveTargetTriple, stageSidecars } from "./stage-sidecar.mjs";

export function parseStageSidecarArgs(argv) {
  const opts = { config: "right-release.config.mjs", target: undefined, json: false, check: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--config" || arg === "--target") {
      const value = argv[i + 1];
      if (!value || value.startsWith("--")) throw new Error(`${arg} needs a value`);
      opts[arg.slice(2)] = value;
      i += 1;
    } else if (arg === "--json") opts.json = true;
    else if (arg === "--check") opts.check = true;
    else throw new Error(`unknown stage-sidecar argument: ${arg}`);
  }
  return opts;
}

const NODE_ARCH = { x64: "x86_64", arm64: "aarch64" };
const NODE_OS = { darwin: "apple-darwin", win32: "pc-windows-msvc", linux: "unknown-linux-gnu" };

/** Host triple from Node alone (no subprocess), so `--check` stays free of effects. */
export function nodeHostTriple(platform = process.platform, arch = process.arch) {
  const cpu = NODE_ARCH[arch];
  const system = NODE_OS[platform];
  if (!cpu || !system) throw new Error(`cannot derive a target triple from ${platform}/${arch}; pass --target`);
  return `${cpu}-${system}`;
}

/** Human-readable plan lines for `--check`. */
export function formatSidecarPlan(plan) {
  const lines = [`right-release stage-sidecar --check: target ${plan.target}, mode ${plan.mode}, stageDir ${plan.stageDir}${plan.wipeStageDir ? " (wipeStageDir)" : ""}`];
  for (const entry of plan.entries) {
    if (entry.skipped) lines.push(`  [${entry.index}] ${entry.kind}: skipped (platforms ${entry.platforms.join(",")})`);
    else lines.push(`  [${entry.index}] ${entry.kind}: ${entry.outputs.join(", ")}${entry.externalBin ? " (externalBin)" : ""}${entry.clean ? " (clean)" : ""}${entry.url ? ` <- ${entry.url}` : ""}`);
  }
  if (plan.nestedBrokerRisk) lines.push("  warning: running inside an outer broker build; staging would fail with STAGE_SIDECAR_NESTED_BROKER (run stage-sidecar in recipe prepare, before the Tauri build)");
  lines.push("right-release stage-sidecar --check: manifest is valid; nothing was staged");
  return lines;
}

async function main() {
  const opts = parseStageSidecarArgs(process.argv.slice(2));
  const configPath = path.resolve(opts.config);
  const appRoot = path.dirname(configPath);
  const loaded = await import(pathToFileURL(configPath).href);
  const config = loaded.default ?? loaded.config ?? loaded;
  if (config?.sidecars === undefined) throw new Error(`${opts.config} declares no sidecars`);
  if (opts.check) {
    const target = opts.target ?? process.env.TAURI_ENV_TARGET_TRIPLE ?? nodeHostTriple();
    const plan = planSidecarStaging({ manifest: config.sidecars, target });
    if (opts.json) console.log(JSON.stringify(plan, null, 2));
    else for (const line of formatSidecarPlan(plan)) console.log(line);
    return;
  }
  const target = resolveTargetTriple({ target: opts.target, cwd: appRoot });
  // Warnings (a never-run row, a catalog row that is not staged) go to stderr; stdout stays the staged-file list or the --json document.
  const result = await stageSidecars({ appRoot, manifest: config.sidecars, target, deps: { warn: (line) => console.error(line) } });
  if (opts.json) console.log(JSON.stringify(result, null, 2));
  else {
    for (const file of result.files ?? []) console.log(`right-release stage-sidecar: staged ${typeof file === "string" ? file : file.path ?? JSON.stringify(file)}`);
    console.log(`right-release stage-sidecar: externalBin ${JSON.stringify(result.externalBinNames ?? [])}; receipt ${result.receiptPath}`);
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().catch((error) => { console.error(`right-release stage-sidecar: ${error.code ? `[${error.code}] ` : ""}${error.message}`); process.exitCode = 1; });
}
