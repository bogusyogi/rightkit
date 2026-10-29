// The door check for an app's own build step: a command its right-release.config.mjs declares (a payload assembler, an installer
// packager) that right-release runs inside a stage. The step proceeds only when a live `rightkit-gate --to` run for HEAD launched it,
// the same check right-release makes for itself (gate-stages.mjs requireDoorRun), so `node scripts/release/<step>.mjs` run by hand is
// refused instead of becoming a side door around class A.
import { spawnSync } from "./background-process.mjs";
import { requireDoorRun } from "./gate-stages.mjs";
import { doorContextFor } from "./source-split.mjs";

/** null when a live door run for HEAD (or, inside a source split, for the split's commit) launched this process, else the refusal message. */
export function doorRefusal({ cwd = process.cwd(), env = process.env, command = "this build step", run = spawnSync, alive } = {}) {
  let context;
  try { context = doorContextFor({ cwd, env, run }); } catch (error) { return `${command}: ${error.code ?? "RK_SOURCE_SPLIT"}: ${error.message}`; }
  if (!context) return `${command} runs only in a git checkout, through the one door: rightkit-gate --to <stage>`;
  return requireDoorRun({ env, commonDir: context.commonDir, commit: context.commit, command, ...(alive ? { alive } : {}) });
}

/** Exits 2 with the refusal unless a live door run launched this process. */
export function requireDoor(command, options = {}) {
  const refusal = doorRefusal({ ...options, command });
  if (!refusal) return;
  process.stderr.write(`${refusal}\n`);
  process.exit(2);
}
