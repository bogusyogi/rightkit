// The one entry every app declares (Adrian, 2026-10-28: "every app has its single entry into the pipeline in its build script"):
// the gate:* scripts `rightkit-gate adopt` writes into the app's package.json, and that the app rule G23 (single-entry) requires.
// Each runs `rightkit-gate` from the app's own install (node_modules/.bin), which on a managed host hands the run to the host's
// pinned gate (bin/rightkit-gate.mjs deferToHostGate), so the entry is the same on the Mac, the Dell, a cloud session and CI.
// The names carry the `gate:` prefix on purpose: the gate itself runs `pnpm run test` and `run build` of the units it checks and
// the agent hook denies a runner script whose name says it builds (door-policy.mjs SCRIPT_BUILDS); neither can meet a gate:* name.
export const GATE_SCRIPT_PREFIX = "gate:";
export const GATE_SCRIPTS = Object.freeze({
  // Full class A on the working tree: every rule and scan, the ones that execute repository code included; compiles nothing.
  "gate:check": "rightkit-gate check --worktree --full-a",
  // Class A, then right-release dev|test on the working tree. The platform follows: `pnpm gate:dev --platform mac`.
  "gate:dev": "rightkit-gate --to dev --",
  "gate:test": "rightkit-gate --to test --",
  // The committed stages, in order, each after the one before: `pnpm gate:package --platform mac`, `pnpm gate:qualify --platform win`.
  "gate:compile": "rightkit-gate --to compile",
  "gate:package": "rightkit-gate --to package",
  "gate:qualify": "rightkit-gate --to qualify",
  // Hands the qualified sealed set to the trusted publisher: `pnpm gate:stage --platform mac --tier patch`.
  "gate:stage": "rightkit-gate stage",
});
export const GATE_SCRIPT_NAMES = Object.freeze(Object.keys(GATE_SCRIPTS));

/** Whether a package.json script command is the gate: `rightkit-gate …`, or `pnpm exec rightkit-gate …` (the same binary, by pnpm). */
export function isGateCommand(command) {
  return typeof command === "string" && /^\s*(?:pnpm\s+exec\s+)?rightkit-gate(?:\s|$)/.test(command);
}
