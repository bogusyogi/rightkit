#!/usr/bin/env node
// `node carried-adapter-run.mjs <adapter> -- <args>`: one pnpm, node or QA step of a snapshot stage on a broker host, carried
// (stage-seam.mjs runStageAdapter) from this process's working directory. build-launch.mjs hands such a step to this entry instead of
// to the RustRight client, so the step is one seam adapter call, filed in the stage's ledger, with its compiles under it, the
// counterpart of carried-cargo-run.mjs for Cargo. Both are the process entries of the stage seam (stage-seam.mjs).
//
// The step's own environment arrives in this process's environment: build-launch.mjs sets RUSTRIGHT_ADAPTER_ENV to exactly the names the step
// declares (adapter-env.mjs adapterEnvLaunch) and those variables' values; runStageAdapter sends that environment to the client unchanged and
// the broker judges it. A step that declares no env has no RUSTRIGHT_ADAPTER_ENV, so the request carries an empty environment.
//
// `node carried-adapter-run.mjs --skip-install <pnpm args>` runs nothing: a snapshot stage's dependencies were installed by the gate, so a
// step that only installs them is skipped with a log line (build-launch.mjs snapshotInstallSkip).
import { carriedCargoMapOf, runStageAdapter } from "./stage-seam.mjs";

const [first, ...rest] = process.argv.slice(2);
if (first === "--skip-install") {
  process.stderr.write(`rightkit-gate: snapshot route: skipped \`pnpm ${rest.join(" ")}\`: the gate already installed this snapshot's dependencies (a stage never installs into the commit's snapshot)\n`);
  process.exit(0);
}
const separator = rest.indexOf("--");
const adapter = first;
const args = separator >= 0 ? rest.slice(separator + 1) : [];
const carried = carriedCargoMapOf(process.env);
if (!adapter || separator !== 0 || !carried?.map) {
  process.stderr.write(`rightkit-gate: RK_ADAPTER_CARRIER_MISSING: ${!adapter || separator !== 0 ? "usage: carried-adapter-run.mjs <adapter> -- <args>" : (carried?.error ?? "no carried map in the environment")}; a snapshot stage's step is never sent to the client around its carrier\n`);
  process.exit(101);
}
const result = runStageAdapter(adapter, args, { cwd: process.cwd(), env: process.env, stdio: "inherit" }, { map: carried.map });
if (result.error) process.stderr.write(`rightkit-gate: ${result.error.message}\n`);
process.exit(result.status ?? 1);
