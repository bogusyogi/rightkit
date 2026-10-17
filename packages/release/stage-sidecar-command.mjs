#!/usr/bin/env node
// `right-release stage-sidecar [--config right-release.config.mjs] [--target <triple>]` (bogusyogi/rightkit#345):
// stages the config's declarative `sidecars` into src-tauri/binaries with Tauri externalBin naming.
import path from "node:path";
import { pathToFileURL } from "node:url";
import { resolveTargetTriple, stageSidecars } from "./stage-sidecar.mjs";

export function parseStageSidecarArgs(argv) {
  const opts = { config: "right-release.config.mjs", target: undefined, json: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--config" || arg === "--target") {
      const value = argv[i + 1];
      if (!value || value.startsWith("--")) throw new Error(`${arg} needs a value`);
      opts[arg.slice(2)] = value;
      i += 1;
    } else if (arg === "--json") opts.json = true;
    else throw new Error(`unknown stage-sidecar argument: ${arg}`);
  }
  return opts;
}

async function main() {
  const opts = parseStageSidecarArgs(process.argv.slice(2));
  const configPath = path.resolve(opts.config);
  const appRoot = path.dirname(configPath);
  const loaded = await import(pathToFileURL(configPath).href);
  const config = loaded.default ?? loaded.config ?? loaded;
  if (config?.sidecars === undefined) throw new Error(`${opts.config} declares no sidecars`);
  const target = resolveTargetTriple({ target: opts.target, cwd: appRoot });
  const result = await stageSidecars({ appRoot, manifest: config.sidecars, target });
  if (opts.json) console.log(JSON.stringify(result, null, 2));
  else {
    for (const file of result.files ?? []) console.log(`right-release stage-sidecar: staged ${typeof file === "string" ? file : file.path ?? JSON.stringify(file)}`);
    console.log(`right-release stage-sidecar: externalBin ${JSON.stringify(result.externalBinNames ?? [])}; receipt ${result.receiptPath}`);
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().catch((error) => { console.error(`right-release stage-sidecar: ${error.code ? `[${error.code}] ` : ""}${error.message}`); process.exitCode = 1; });
}
