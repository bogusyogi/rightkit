// Shared argv command shape for the declarative `tests` and `web` config sections (`right-release test`, `web-build`).
// A step is one command run without a shell:
//   { cmd, args?, cwd?, env? }                  any executable ("node" means this Node binary)
//   { node, args?, nodeFlags?, nodeOptions? }   a Node script path inside the app, run by this Node binary
// `cwd` and `node` are app-relative and may not leave the app. `nodeFlags` go before the script, `nodeOptions` are merged
// (de-duplicated) into NODE_OPTIONS for the child. Unknown keys are refused so a typo cannot silently change a run.
import path from "node:path";
import { defaultReleaseConfigFile, importReleaseConfig } from "./release-config.mjs";

/** Load an app's release config (`right-release.config.json`, or the legacy `.mjs` while it is the only one); the app root is the config file's directory. */
export async function loadAppConfig(configFile = defaultReleaseConfigFile()) {
  const configPath = path.resolve(configFile);
  return { config: await importReleaseConfig(configPath, { lenient: true }), appRoot: path.dirname(configPath), configPath };
}

export class CommandSpecError extends Error {
  constructor(message) { super(message); this.name = "CommandSpecError"; this.code = "RK_COMMAND_SPEC"; }
}

const fail = (message) => { throw new CommandSpecError(message); };
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
export const STEP_KEYS = ["cmd", "node", "args", "cwd", "env", "nodeFlags", "nodeOptions"];

export function relativeInside(value, label) {
  if (typeof value !== "string" || !value.trim()) fail(`${label} must be a non-empty relative path`);
  const normalized = value.replaceAll("\\", "/");
  if (path.isAbsolute(value) || /^[A-Za-z]:/.test(value) || normalized.split("/").includes("..")) fail(`${label} must be relative and stay inside the app: ${value}`);
  return normalized;
}

const stringList = (value, label) => {
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string")) fail(`${label} must be a list of strings`);
  return [...value];
};

/** Validate one step; `extraKeys` are accepted and left to the caller. Returns the step. */
export function validateStep(step, label, extraKeys = []) {
  if (!isObject(step)) fail(`${label} must be an object`);
  const allowed = new Set([...STEP_KEYS, ...extraKeys]);
  const unknown = Object.keys(step).filter((key) => !allowed.has(key));
  if (unknown.length) fail(`${label} has unknown key(s): ${unknown.join(", ")} (allowed: ${[...allowed].join(", ")})`);
  if (step.cmd !== undefined && step.node !== undefined) fail(`${label} sets both cmd and node; use one`);
  if (step.cmd !== undefined && (typeof step.cmd !== "string" || !step.cmd.trim())) fail(`${label}.cmd must be a non-empty string`);
  if (step.node !== undefined) relativeInside(step.node, `${label}.node`);
  if (step.cwd !== undefined) relativeInside(step.cwd, `${label}.cwd`);
  if (step.args !== undefined) stringList(step.args, `${label}.args`);
  if (step.nodeFlags !== undefined) stringList(step.nodeFlags, `${label}.nodeFlags`);
  if (step.nodeOptions !== undefined) stringList(step.nodeOptions, `${label}.nodeOptions`);
  if (step.env !== undefined && (!isObject(step.env) || Object.values(step.env).some((value) => typeof value !== "string"))) fail(`${label}.env must be an object of strings`);
  return step;
}

/** Add flags to a NODE_OPTIONS string without repeating one already present. */
export function mergeNodeOptions(existing, flags) {
  const present = (existing ?? "").trim().split(/\s+/).filter(Boolean);
  for (const flag of flags) if (!present.includes(flag)) present.push(flag);
  return present.join(" ");
}

/**
 * Resolve a validated step to what to spawn: `{ cmd, args, cwd, env }`. `extraArgs` are appended after the step's own
 * arguments (the `--` passthrough of `right-release test`). `cmd: "node"` and `node:` use `process.execPath`.
 */
export function resolveInvocation(step, { appRoot, baseEnv = process.env, extraArgs = [] }) {
  const args = [...(step.args ?? []), ...extraArgs];
  let cmd;
  let argv;
  if (step.node !== undefined) {
    cmd = process.execPath;
    argv = [...(step.nodeFlags ?? []), path.resolve(appRoot, step.node), ...args];
  } else {
    if (typeof step.cmd !== "string" || !step.cmd.trim()) fail("a step needs cmd or node");
    cmd = step.cmd === "node" ? process.execPath : step.cmd;
    argv = [...(step.cmd === "node" ? (step.nodeFlags ?? []) : []), ...args];
  }
  const env = { ...baseEnv, ...(step.env ?? {}) };
  if (step.nodeOptions?.length) env.NODE_OPTIONS = mergeNodeOptions(env.NODE_OPTIONS, step.nodeOptions);
  return { cmd, args: argv, cwd: path.resolve(appRoot, step.cwd ?? "."), env };
}
