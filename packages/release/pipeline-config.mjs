// M24: every command a release config hands to RightKit is judged here, before anything runs.
//
// A release config is data. The places where it can name an executable are the dev build and install commands, the package, signing
// and publish hooks, the recipe `prepare` / `postBuild` lists, the native assembly hooks, the sidecar `command` entries and the web
// build steps. `collectConfigCommands` finds every object that carries `cmd` or `node` under those roots (so a command hidden in a
// new key is still seen), and `validateConfigPipeline` runs R1 admission on each one (R2 inside Tauri `--config` overrides, R4 for a
// `node` generator). Each finding names its JSON pointer, its line in the config file, its pipeline stage and who owns the step.
// `tests` and `qa` are not pipeline commands (M20, M21) and are not walked.
import { admitHookString, admitInvocation } from "./pipeline-admission.mjs";
import { makeGeneratorAdmitter } from "./pipeline-generators.mjs";

const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const escapePointer = (key) => String(key).replaceAll("~", "~0").replaceAll("/", "~1");

/** Config roots that can carry a command. Everything else (checks, buildInputs, legal, distribution, qa, tests) is typed data. */
export const PIPELINE_CONFIG_ROOTS = Object.freeze(["development", "targets", "nativeAssembly", "sidecars", "web"]);
/** The only keys of a command object that can change what runs; every other key on the node is typed host metadata. */
const COMMAND_KEYS = ["cmd", "node", "args", "cwd", "timeoutMs", "nodeFlags", "env", "nodeOptions", "envFile", "shell"];

/** The pipeline stage a command pointer belongs to. */
export function stageOfPointer(pointer) {
  const parts = pointer.split("/").slice(1);
  if (parts[0] === "development") {
    if (parts[3] === "install") return "dev-install";
    return "dev-build";
  }
  if (parts[0] === "targets") {
    if (parts[2] === "gradle") return "android-build";
    if (["winPackage", "macPackage", "iosPackage"].includes(parts[2])) return "recipe-prepare";
    const hook = parts[2];
    if (hook === "prePackage") return "build";
    if (hook === "package") return "package";
    if (hook === "postSign") return "post-sign";
    if (hook === "publish" || hook === "upload") return "publish";
    return "build";
  }
  if (parts[0] === "nativeAssembly") return "native-assembly";
  if (parts[0] === "sidecars") return "sidecar-stage";
  if (parts[0] === "web") return "web-build";
  return "pipeline";
}

/** Every command node under the pipeline roots: `[{ pointer, stage, step }]`, plus string-form hooks (`nativeAssembly.finalizer`). */
export function collectConfigCommands(config) {
  const found = [];
  const visit = (node, pointer, depth) => {
    if (depth > 32) return;
    if (Array.isArray(node)) { node.forEach((item, index) => visit(item, `${pointer}/${index}`, depth + 1)); return; }
    if (!isObject(node)) return;
    if (typeof node.cmd === "string" || typeof node.node === "string") {
      const step = {};
      for (const key of COMMAND_KEYS) if (key in node) step[key] = node[key];
      found.push({ pointer, stage: stageOfPointer(pointer), step });
    }
    for (const [key, value] of Object.entries(node)) {
      if (key === "args" || key === "nodeFlags" || key === "env") continue;
      visit(value, `${pointer}/${escapePointer(key)}`, depth + 1);
    }
  };
  for (const root of PIPELINE_CONFIG_ROOTS) if (config?.[root] !== undefined) visit(config[root], `/${root}`, 0);
  if (typeof config?.nativeAssembly?.finalizer === "string") found.push({ pointer: "/nativeAssembly/finalizer", stage: "native-assembly", string: config.nativeAssembly.finalizer });
  return found;
}

const UNKNOWN_CODE = /(?:unknown|unresolved|cycle|depth|unregistered|containment)/;
/** Who owns a refused step: `unknown` when ownership could not be established (it blocks), else the app that wrote it. */
export const ownershipOfCode = (code) => (UNKNOWN_CODE.test(code) ? "unknown" : "app-owned");
export const ruleOfCode = (code) => String(code).split("-")[0];

/**
 * Judge every command in a release config. `options`: `scripts` (the app's package.json scripts), `registry` + `appRoot` (the shared
 * generator registry for a `node` step), `lineOf(pointer)` (config line numbers), `containment`, `readBytes`, `readText` (injectable).
 * Returns the findings; an empty list means every command is an admitted RightKit invocation.
 */
export function validateConfigPipeline(config, options = {}) {
  const findings = [];
  const app = typeof config?.app === "string" ? config.app : "";
  const admitGenerator = options.registry && options.appRoot
    ? makeGeneratorAdmitter({ registry: options.registry, app, appRoot: options.appRoot, ...(options.containment ? { containment: options.containment } : {}), ...(options.readBytes ? { readBytes: options.readBytes } : {}), ...(options.readText ? { readText: options.readText } : {}) })
    : undefined;
  for (const entry of collectConfigCommands(config)) {
    const common = { scripts: options.scripts, stage: entry.stage, label: entry.pointer, admitGenerator };
    const result = entry.string !== undefined ? admitHookString(entry.string, common) : admitInvocation(entry.step, common);
    if (result.admitted) continue;
    findings.push({
      rule: ruleOfCode(result.code),
      code: result.code,
      pointer: entry.pointer,
      line: options.lineOf ? options.lineOf(entry.pointer) : 1,
      stage: entry.stage,
      ownership: ownershipOfCode(result.code),
      message: result.reason,
    });
  }
  return findings;
}

export function formatFindings(findings, file = "right-release.config.json") {
  return findings.map((finding) => `${file}:${finding.line} [${finding.code}] ${finding.stage} (${finding.ownership}) ${finding.pointer}: ${finding.message}`);
}
