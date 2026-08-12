import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { catalogScaleChurn, nativeModuleCatalog, packageBenchmark, phase0, supplementalPolicyPackage } from "./windows-phase0.mjs";

const command = process.argv[2];
if (!new Set(["spike", "catalog", "package", "benchmark"]).has(command)) throw new Error("usage: node src/cli.mjs <spike|catalog|package|benchmark> [output.json]");
const output = resolve(process.argv[3] || `windows-phase0-${command}.json`);
let result;
if (command === "spike") result = await phase0();
else {
  if (process.platform !== "win32") throw new Error("catalog/package/benchmark must run on Windows");
  const catalog = await nativeModuleCatalog();
  const load = catalogScaleChurn(catalog);
  result = command === "catalog" ? catalog : command === "package" ? supplementalPolicyPackage(load) : packageBenchmark(load);
}
await mkdir(dirname(output), { recursive: true });
await writeFile(output, `${JSON.stringify(result, null, 2)}\n`);
process.stdout.write(`${result.outcome} ${output}\n`);
