// M24 R1 (invocation admission) and the R2 hook-string rules.
//
// A command that a release config, a package alias or a framework hook asks RightKit to run is admitted only as one COMPLETE approved
// invocation: an exact executable and an argument vector that matches a typed entry below. There is no shell, so there is nothing to
// quote: an operator, an environment injection, a substitution or a chained second command in any string is refused outright, and
// `pnpm exec` runs only an approved installed runtime. Aliases (`pnpm run x`, `pnpm x`) are followed through package.json with a
// depth bound; a cycle, an unknown alias or any unadmitted step anywhere in the chain blocks the whole invocation. Caller strings and
// process identity never establish "official" origin: a step carries no origin field (an unknown key is refused), and only the
// executable and arguments are judged.
//
// Everything here is a pure function over the step and an optional `scripts` map; it reads no file and runs nothing. The scanner
// (`pipeline-scan.mjs`) supplies the app's package.json scripts and the shared-generator registry.
import path from "node:path";
import { classifyTokens, commandBaseName } from "./pipeline-tools.mjs";

export class PipelineAdmissionError extends Error {
  constructor(code, message, findings = []) {
    super(message);
    this.name = "PipelineAdmissionError";
    this.code = code;
    this.findings = findings;
  }
}

const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const refuse = (code, reason) => ({ admitted: false, code, reason });
const admit = (id, via, extra = {}) => ({ admitted: true, id, via, ...extra });

export const MAX_ALIAS_DEPTH = 8;
export const MAX_COMMAND_TIMEOUT_MS = 6 * 60 * 60 * 1000;

/** Keys a command step may carry. `env` and `nodeOptions` are environment injection; there is no `shell`, `origin` or `official` key. */
export const STEP_ALLOWED_KEYS = Object.freeze(["cmd", "node", "args", "cwd", "timeoutMs", "name", "nodeFlags"]);
const ENV_INJECTION_KEYS = new Set(["env", "nodeOptions", "envFile", "shell"]);

/** Operators, redirects, substitutions and variable expansion: anything a shell or a script runner could re-interpret. */
const SHELL_META = /[\0\r\n]|&&|\|\||[;|<>`]|\$\(|\$\{|\$[A-Za-z_]|%[A-Za-z_][A-Za-z0-9_]*%|\^\^/;
export const hasShellMeta = (text) => SHELL_META.test(String(text));

/** right-release subcommands a config may invoke (the CLI's own commands; unknown names are refused). */
export const OWNER_SUBCOMMANDS = Object.freeze(new Set([
  "release", "build", "package", "publish", "upload", "deps", "publish-update", "hardening", "harden", "hardeningscan", "sign-windows",
  "sign-updater", "sign-dev-artifact", "sign-qa-bundle", "sign-candidate", "engine-locate", "stage-sidecar", "init", "pe", "icons", "recipe-build",
  "signing-agent", "create-mac-updater", "mirror-root-artifact", "test", "web-build", "license-notices", "toolchain", "github",
  "cargo-target", "qa", "ownership", "ownership-approval", "ownership-report", "ownership-mandatory", "adoption-evidence", "swift-package", "bundle-scan",
  "check-release-dependencies", "dist-scan", "identity-scan", "apple-profile-create", "ios-credentials", "dev", "doctor", "hooks",
  "suite-doctor", "cache", "disk", "install-app", "dmg", "apple", "inventory", "model", "addon", "lsclean", "generate-dmg-background",
  "pipeline",
]));

/** right-release subcommands that start another pipeline run: a generator or a framework hook may never run one (R2, R4). */
export const PIPELINE_DISPATCH_SUBCOMMANDS = Object.freeze(new Set([
  "release", "build", "package", "publish", "upload", "publish-update", "github", "dev", "install-app", "recipe-build", "sign-windows",
  "sign-updater", "sign-dev-artifact", "sign-candidate", "create-mac-updater", "swift-package", "dmg", "addon", "apple",
]));

const INTERPRETERS = new Set(["bash", "sh", "zsh", "dash", "fish", "ksh", "csh", "tcsh", "cmd", "powershell", "pwsh", "python", "python3", "py", "ruby", "perl", "php", "osascript", "wsl", "env", "sudo", "xargs", "wscript", "cscript", "deno", "bun"]);
const PACKAGE_MANAGERS = new Set(["npm", "npx", "yarn", "corepack", "pnpx"]);
const PNPM_BUILTINS = new Set(["add", "install", "i", "remove", "rm", "uninstall", "update", "up", "upgrade", "exec", "run", "run-script", "dlx", "publish", "pack", "rebuild", "store", "config", "link", "ln", "unlink", "list", "ls", "outdated", "audit", "why", "root", "bin", "create", "deploy", "patch", "patch-commit", "import", "init", "licenses", "prune", "fetch", "env", "setup", "self-update", "completion", "approve-builds", "dedupe", "doctor", "server", "cache", "recursive", "-r", "--filter", "-F", "--version", "-v", "--help", "-h"]);
/** pnpm subcommands that run a package.json script of the same name. */
const PNPM_SCRIPT_SHORTHANDS = new Set(["test", "t", "start", "stop", "restart"]);
const CARGO_SUBCOMMANDS = new Set(["build", "check", "test", "clippy", "fmt", "metadata", "tree", "doc", "bench", "about", "fetch", "vendor"]);
const NODE_FLAG = /^(?:--disable-warning=[A-Za-z0-9_-]+|--no-experimental-webstorage|--no-warnings|--max-old-space-size=\d{3,6})$/;
const SAFE_VALUE = /^[A-Za-z0-9._:/@=+,#-]+$/;
const TRIPLE = /^[A-Za-z0-9_]+(?:-[A-Za-z0-9_.]+){1,4}$/;
const PROFILE = /^[a-z][a-z0-9_-]*$/;
const LEGACY_CONFIG = /\.(?:[cm]?js|[cm]?ts)$/i;

function relativeInside(value) {
  if (typeof value !== "string" || !value.trim() || path.isAbsolute(value) || path.win32.isAbsolute(value) || /^[A-Za-z]:/.test(value)) return null;
  const normalized = value.replaceAll("\\", "/").replace(/^(?:\.\/)+/, "");
  return normalized.split("/").includes("..") || normalized === "" ? null : normalized;
}

/**
 * Split one command line into tokens (quotes group, a backslash escapes outside single quotes). Returns `{ tokens }` or
 * `{ error }`; an unquoted operator is an error, so a chained second command can never be hidden in the text.
 */
export function tokenizeCommandLine(text) {
  const input = String(text);
  if (/[\0\r\n]/.test(input)) return { error: "a command line may not contain a newline or NUL" };
  const tokens = [];
  let current = "";
  let started = false;
  let quote = null;
  for (let i = 0; i < input.length; i += 1) {
    const ch = input[i];
    if (quote) {
      if (ch === quote) quote = null;
      else if (ch === "\\" && quote === '"' && (input[i + 1] === '"' || input[i + 1] === "\\")) { current += input[i + 1]; i += 1; }
      else current += ch;
      continue;
    }
    if (ch === '"' || ch === "'") { quote = ch; started = true; continue; }
    if (/\s/.test(ch)) { if (started) { tokens.push(current); current = ""; started = false; } continue; }
    if (/[;|&<>`()]/.test(ch) || (ch === "$" && /[({A-Za-z_]/.test(input[i + 1] ?? ""))) return { error: `the operator or substitution "${ch}" is not allowed in an admitted command` };
    current += ch;
    started = true;
  }
  if (quote) return { error: "unterminated quote" };
  if (started) tokens.push(current);
  return { tokens };
}

function shellSafeArgs(args, label) {
  const bad = args.find((arg) => hasShellMeta(arg));
  return bad === undefined ? null : refuse("R1-operators", `${label}: argument ${JSON.stringify(bad)} contains an operator, substitution, environment reference or newline`);
}

/** Validate a step's shape and return `{ cmd, args, cwd }` (a `node` step becomes `cmd: "node"`), or a refusal. */
export function normalizeStep(step) {
  if (!isObject(step)) return refuse("R1-shape", "a command must be an object { cmd, args? }");
  for (const key of Object.keys(step)) {
    if (ENV_INJECTION_KEYS.has(key)) return refuse("R1-env-injection", `the "${key}" key injects environment or a shell and is not allowed in an admitted command`);
    if (!STEP_ALLOWED_KEYS.includes(key)) return refuse("R1-shape", `unknown command key "${key}" (allowed: ${STEP_ALLOWED_KEYS.join(", ")}); a caller-supplied origin or identity is never accepted`);
  }
  if (step.cmd !== undefined && step.node !== undefined) return refuse("R1-shape", "a command sets both cmd and node");
  const argList = step.args === undefined ? [] : step.args;
  if (!Array.isArray(argList) || argList.some((arg) => typeof arg !== "string")) return refuse("R1-shape", "args must be a list of strings");
  if (argList.some((arg) => /[\0\r\n]/.test(arg))) return refuse("R1-operators", "an argument contains a newline or NUL");
  if (step.cwd !== undefined && relativeInside(step.cwd) === null) return refuse("R1-cwd", `cwd must be relative and stay inside the app: ${JSON.stringify(step.cwd)}`);
  if (step.timeoutMs !== undefined && !(Number.isInteger(step.timeoutMs) && step.timeoutMs > 0 && step.timeoutMs <= MAX_COMMAND_TIMEOUT_MS)) return refuse("R1-shape", "timeoutMs must be a positive integer of at most six hours");
  if (step.nodeFlags !== undefined && (!Array.isArray(step.nodeFlags) || step.nodeFlags.some((flag) => typeof flag !== "string" || !NODE_FLAG.test(flag)))) return refuse("R1-node-flags", "nodeFlags allow only --disable-warning=<id>, --no-experimental-webstorage, --no-warnings and --max-old-space-size=<n>");
  if (step.node !== undefined) {
    if (typeof step.node !== "string") return refuse("R1-shape", "node must be an app-relative script path");
    return { cmd: "node", args: [...(step.nodeFlags ?? []), step.node, ...argList], cwd: step.cwd, nodeScript: true };
  }
  if (step.nodeFlags !== undefined) return refuse("R1-node-flags", "nodeFlags apply only to a node script step");
  if (typeof step.cmd !== "string" || !step.cmd.trim()) return refuse("R1-shape", "cmd must be a non-empty string");
  return { cmd: step.cmd, args: argList, cwd: step.cwd };
}

/** The approved installed runtimes a `pnpm exec` (or `node node_modules/...`) may start. */
const flagTable = (spec) => (rest, label) => {
  for (let i = 0; i < rest.length; i += 1) {
    const arg = rest[i];
    const flag = arg.includes("=") && arg.startsWith("--") ? arg.slice(0, arg.indexOf("=")) : arg;
    const kind = spec[flag];
    if (kind === undefined) return refuse("R1-option", `${label}: option ${JSON.stringify(arg)} is not an approved option`);
    if (kind === "none") { if (arg !== flag) return refuse("R1-option", `${label}: ${flag} takes no value`); continue; }
    const value = arg !== flag ? arg.slice(flag.length + 1) : rest[(i += 1)];
    if (value === undefined) return refuse("R1-option", `${label}: ${flag} needs a value`);
    if (kind === "value" && !SAFE_VALUE.test(value)) return refuse("R1-option", `${label}: ${flag} value ${JSON.stringify(value)} is not a plain token`);
    if (kind === "triple" && !TRIPLE.test(value)) return refuse("R1-option", `${label}: ${flag} must name a target triple`);
    if (kind === "profile" && !PROFILE.test(value)) return refuse("R1-option", `${label}: ${flag} must name a Cargo profile`);
    if (kind === "path" && relativeInside(value) === null) return refuse("R1-option", `${label}: ${flag} must be a relative path inside the app`);
    if (kind === "int" && !/^\d{1,3}$/.test(value)) return refuse("R1-option", `${label}: ${flag} must be a small integer`);
  }
  return null;
};

const TAURI_COMMANDS = new Set(["build", "bundle", "dev", "info"]);
const TAURI_OPTIONS = flagTable({
  "--target": "triple", "--bundles": "value", "--no-sign": "none", "--no-bundle": "none", "--ci": "none", "--verbose": "none", "-v": "none",
  "--debug": "none", "-d": "none", "--features": "value", "--config": "config", "-c": "config",
});
const TAURI_PASSTHROUGH = flagTable({ "--profile": "profile", "--locked": "none", "--features": "value", "--no-default-features": "none", "--jobs": "int", "--frozen": "none", "--offline": "none" });

/** `--config <json | path>`: an inline override may set only the typed keys below, and each hook inside it passes R2. */
export function validateTauriConfigOverride(text, options = {}, label = "tauri --config") {
  const value = String(text).trim();
  if (!value.startsWith("{")) {
    const normalized = relativeInside(value);
    if (normalized === null || !/\.json$/i.test(normalized)) return refuse("R2-tauri-config", `${label} must be inline JSON or a relative .json file: ${JSON.stringify(value)}`);
    return null;
  }
  let parsed;
  try { parsed = JSON.parse(value); } catch { return refuse("R2-tauri-config", `${label} is not valid JSON`); }
  if (!isObject(parsed)) return refuse("R2-tauri-config", `${label} must be a JSON object`);
  for (const key of Object.keys(parsed)) if (key !== "build" && key !== "bundle") return refuse("R2-tauri-config", `${label}: top-level key "${key}" is not allowed (identity and bundle id belong to RightKit; allowed: build, bundle)`);
  const build = parsed.build ?? {};
  if (!isObject(build)) return refuse("R2-tauri-config", `${label}.build must be an object`);
  for (const [key, entry] of Object.entries(build)) {
    if (["beforeDevCommand", "beforeBuildCommand", "beforeBundleCommand"].includes(key)) {
      const result = admitTauriHook(entry, options, `${label}.build.${key}`);
      if (result) return result;
    } else if (!["frontendDist", "devUrl"].includes(key)) return refuse("R2-tauri-config", `${label}.build.${key} is not allowed`);
  }
  const bundle = parsed.bundle ?? {};
  if (!isObject(bundle)) return refuse("R2-tauri-config", `${label}.bundle must be an object`);
  for (const [key, entry] of Object.entries(bundle)) {
    if (key === "createUpdaterArtifacts") { if (typeof entry !== "boolean" && entry !== "v1Compatible") return refuse("R2-tauri-config", `${label}.bundle.createUpdaterArtifacts must be a boolean`); }
    else if (key === "windows") { if (!isObject(entry) || Object.keys(entry).some((k) => k !== "signCommand" || entry[k] !== null)) return refuse("R3-tauri-sign-hook", `${label}.bundle.windows may only set signCommand to null: a sign hook is not allowed`); }
    else if (key === "macOS") { if (!isObject(entry) || Object.keys(entry).some((k) => k !== "signingIdentity" || entry[k] !== "-")) return refuse("R3-tauri-sign-hook", `${label}.bundle.macOS may only set signingIdentity to "-" (ad hoc): signing belongs to RightKit`); }
    else return refuse("R2-tauri-config", `${label}.bundle.${key} is not allowed`);
  }
  return null;
}

/** A Tauri `before*Command` value: a string or `{ script, cwd }`; one admitted command, no chain. */
export function admitTauriHook(entry, options = {}, label = "tauri hook") {
  const script = typeof entry === "string" ? entry : isObject(entry) && typeof entry.script === "string" ? entry.script : null;
  if (script === null) return refuse("R2-tauri-hook", `${label} must be a command string or { script, cwd }`);
  if (isObject(entry) && entry.cwd !== undefined && relativeInside(entry.cwd) === null) return refuse("R2-tauri-hook", `${label}.cwd must stay inside the app`);
  if (script.trim() === "") return null;
  const result = admitHookString(script, options);
  return result.admitted ? null : refuse(result.code, `${label}: ${result.reason}`);
}

function admitTauri(rest, options, label) {
  const sub = rest[0];
  if (sub === "signer" || sub === "icon" || sub === "init" || sub === "add" || sub === "migrate") return refuse("R3-tauri-subcommand", `${label}: tauri ${sub} is not an admitted pipeline command`);
  if (!TAURI_COMMANDS.has(sub) && sub !== "--version" && sub !== "-V") return refuse("R1-option", `${label}: tauri subcommand ${JSON.stringify(sub)} is not admitted`);
  if (sub === "--version" || sub === "-V") return rest.length === 1 ? admit("tauri-version", "runtime") : refuse("R1-option", `${label}: --version takes no further arguments`);
  const dash = rest.indexOf("--");
  const own = dash >= 0 ? rest.slice(1, dash) : rest.slice(1);
  const pass = dash >= 0 ? rest.slice(dash + 1) : [];
  // `--config` values carry JSON that the generic flag table cannot judge: validate them here, then check the remaining flags.
  const plain = [];
  for (let i = 0; i < own.length; i += 1) {
    if (own[i] === "--config" || own[i] === "-c" || own[i].startsWith("--config=")) {
      const value = own[i].startsWith("--config=") ? own[i].slice("--config=".length) : own[(i += 1)];
      if (value === undefined) return refuse("R1-option", `${label}: --config needs a value`);
      const problem = validateTauriConfigOverride(value, options, `${label} --config`);
      if (problem) return problem;
    } else plain.push(own[i]);
  }
  const problem = TAURI_OPTIONS(plain.map((arg) => arg), label);
  if (problem) return problem;
  const passProblem = TAURI_PASSTHROUGH(pass, `${label} --`);
  if (passProblem) return passProblem;
  return admit(`tauri-${sub}`, "runtime");
}

const VITE_OPTIONS = flagTable({ "--config": "path", "-c": "path", "--outDir": "path", "--base": "value", "--emptyOutDir": "none" });

/** `pnpm exec <bin>` and `node node_modules/...` runtimes. */
function admitRuntime(bin, rest, options, label) {
  switch (bin) {
    case "right-release": return admitRightRelease(rest, label);
    case "tauri": return admitTauri(rest, options, label);
    case "vite": return rest[0] === "build" ? (VITE_OPTIONS(rest.slice(1), label) ?? admit("vite-build", "runtime")) : rest.length === 1 && rest[0] === "--version" ? admit("vite-version", "runtime") : refuse("R1-option", `${label}: only vite build and vite --version are admitted`);
    case "rightkit-gate": return admitRightKitGate(rest, label);
    case "tsc": return shellSafeArgs(rest, label) ?? admit("tsc", "runtime");
    case "vitest": return rest[0] === "run" || (rest.length === 1 && rest[0] === "--version") ? (shellSafeArgs(rest, label) ?? admit("vitest", "runtime")) : refuse("R1-option", `${label}: only vitest run is admitted`);
    case "eslint": return shellSafeArgs(rest, label) ?? admit("eslint", "runtime");
    default: return refuse("R1-runtime", `${label}: "${bin}" is not an approved installed runtime for pnpm exec`);
  }
}

/**
 * `rightkit-gate install-hook`: retires the git hooks an earlier release wrote and registers the agent hook, so an app's `prepare` script may run it.
 * It builds, signs, packages and uploads nothing. The gate itself (`rightkit-gate`, `--base`) compiles and is never run by a release
 * config; only the installer and the version probe are admitted.
 */
function admitRightKitGate(args, label) {
  if (args.length === 1 && (args[0] === "install-hook" || args[0] === "--version")) return admit(`rightkit-gate-${args[0].replace(/^--/, "")}`, "owner");
  return refuse("R1-owner-subcommand", `${label}: only rightkit-gate install-hook (and --version) is an admitted pipeline command; the gate itself is run through the door, never by a release config`);
}

function admitRightRelease(args, label) {
  const sub = args[0];
  if (!OWNER_SUBCOMMANDS.has(sub)) return refuse("R1-owner-subcommand", `${label}: right-release ${JSON.stringify(sub ?? "")} is not a known RightKit command`);
  const meta = shellSafeArgs(args, label);
  if (meta) return meta;
  for (let i = 1; i < args.length; i += 1) {
    const value = args[i] === "--config" ? args[i + 1] : args[i].startsWith("--config=") ? args[i].slice("--config=".length) : null;
    if (value !== null && value !== undefined && LEGACY_CONFIG.test(value)) return refuse("R1-legacy-config", `${label}: ${value} is a legacy executable config; RightKit reads only right-release.config.json`);
  }
  return admit(`right-release-${sub}`, "owner");
}

const NODE_MODULE_ENTRIES = Object.freeze({
  "node_modules/typescript/bin/tsc": "tsc",
  "node_modules/typescript/lib/tsc.js": "tsc",
  "node_modules/vite/bin/vite.js": "vite",
  "node_modules/vitest/vitest.mjs": "vitest",
  "node_modules/@tauri-apps/cli/tauri.js": "tauri",
  "node_modules/@rightkit/release/cli/right-release.mjs": "right-release",
});

function admitNode(args, options, label) {
  let index = 0;
  while (index < args.length && args[index].startsWith("-")) {
    if (!NODE_FLAG.test(args[index])) return refuse("R1-interpreter", `${label}: node option ${JSON.stringify(args[index])} is not admitted (no -e, --eval, --import or --require)`);
    index += 1;
  }
  const script = relativeInside(args[index]);
  if (script === null) return refuse("R1-interpreter", `${label}: node needs a script path inside the app`);
  const rest = args.slice(index + 1);
  const runtime = NODE_MODULE_ENTRIES[script];
  if (runtime) return admitRuntime(runtime, rest, options, label);
  const generator = options.admitGenerator?.({ script, args: rest, cwd: options.cwd });
  if (generator?.admitted) return admit(generator.id, "generator", { generator: generator.id });
  return refuse(generator?.code ?? "R4-unregistered-generator", `${label}: ${script} is app code, not an admitted shared generator${generator?.reason ? ` (${generator.reason})` : ""}`);
}

const PNPM_INSTALL_FLAGS = new Set(["--frozen-lockfile", "--ignore-scripts", "--prefer-offline", "--offline"]);

function admitPnpm(args, options, label, chain) {
  const sub = args[0];
  if (sub === undefined) return refuse("R1-pnpm", `${label}: pnpm needs a subcommand`);
  if (sub === "exec") {
    const bin = args[1];
    if (bin === undefined || bin.startsWith("-")) return refuse("R1-pnpm-exec", `${label}: pnpm exec needs an installed runtime name first (no shell mode or option before it)`);
    return admitRuntime(bin, args.slice(2), options, `${label} exec ${bin}`);
  }
  if (sub === "install" || sub === "i") {
    const flags = args.slice(1);
    if (flags.some((flag) => !PNPM_INSTALL_FLAGS.has(flag)) || !flags.includes("--frozen-lockfile")) return refuse("R1-pnpm", `${label}: pnpm install is admitted only as install --frozen-lockfile [--ignore-scripts]`);
    return admit("pnpm-install", "runtime");
  }
  if (sub === "run" || sub === "run-script") {
    const name = args[1];
    if (name === undefined || name.startsWith("-")) return refuse("R1-pnpm", `${label}: pnpm run needs a script name first`);
    const meta = shellSafeArgs(args.slice(2), label);
    if (meta) return meta;
    return admitAlias(name, options, chain);
  }
  if (PNPM_SCRIPT_SHORTHANDS.has(sub)) {
    const meta = shellSafeArgs(args.slice(1), label);
    return meta ?? admitAlias(sub === "t" ? "test" : sub, options, chain);
  }
  if (PNPM_BUILTINS.has(sub)) return refuse("R1-pnpm", `${label}: pnpm ${sub} is not an admitted pipeline command`);
  const meta = shellSafeArgs(args.slice(1), label);
  return meta ?? admitAlias(sub, options, chain);
}

/** Follow a package.json alias (and its pre/post hooks): every step must be admitted; a cycle or an unknown name blocks. */
export function admitAlias(name, options = {}, chain = []) {
  const scripts = options.scripts;
  if (!isObject(scripts)) return refuse("R1-alias-unresolved", `package script "${name}" cannot be resolved (no package.json scripts were supplied); an unknown reachable owner blocks`);
  if (chain.includes(name)) return refuse("R1-alias-cycle", `package script alias cycle: ${[...chain, name].join(" -> ")}`);
  if (chain.length >= MAX_ALIAS_DEPTH) return refuse("R1-alias-depth", `package script alias chain deeper than ${MAX_ALIAS_DEPTH}: ${[...chain, name].join(" -> ")}`);
  if (typeof scripts[name] !== "string") return refuse("R1-alias-unknown", `package script "${name}" does not exist`);
  const next = [...chain, name];
  for (const hook of [`pre${name}`, name, `post${name}`]) {
    if (scripts[hook] === undefined) continue;
    if (typeof scripts[hook] !== "string") return refuse("R1-alias-unknown", `package script "${hook}" is not a string`);
    // The hook name joins the chain so `prebuild: pnpm run build` is a cycle, not an unbounded descent.
    const result = admitHookString(scripts[hook], options, hook === name ? next : [...next, hook]);
    if (!result.admitted) return refuse(result.code, `script "${hook}": ${result.reason}`);
  }
  return admit(`alias:${name}`, "alias", { chain: next });
}

/** One command line (a package script, a Tauri hook, a workflow step, an Xcode phase): a single admitted command, no chain. */
export function admitHookString(text, options = {}, chain = []) {
  const tokenized = tokenizeCommandLine(text);
  if (tokenized.error) return refuse("R1-operators", tokenized.error);
  if (!tokenized.tokens.length) return refuse("R1-shape", "an empty command line");
  const [cmd, ...args] = tokenized.tokens;
  return admitInvocation({ cmd, args }, { ...options, aliasChain: chain });
}

/**
 * R1: is this one step an approved invocation? Returns `{ admitted: true, id, via }` or `{ admitted: false, code, reason }`.
 * `options`: `scripts` (package.json scripts, for alias traversal), `stage` (reported only), `label` (message prefix),
 * `admitGenerator({ script, args, cwd })` (the shared-generator registry check; absent means no generator is admitted).
 */
export function admitInvocation(step, options = {}) {
  const label = options.label ?? "command";
  const normalized = normalizeStep(step);
  if (normalized.admitted === false) return normalized;
  const { cmd, args, cwd } = normalized;
  const chain = options.aliasChain ?? [];
  if (/^\.[\\/]gradlew(?:\.bat)?$/i.test(cmd)) return admitGradle(args, options, label);
  if (!/^[A-Za-z0-9._@+-]+$/.test(cmd)) return refuse("R1-executable", `${label}: executable ${JSON.stringify(cmd)} must be a bare name (a path, a space or an operator makes it app-owned code)`);
  const name = commandBaseName(cmd);
  const inner = { ...options, cwd };
  const tool = classifyTokens([name, ...args]);
  if (tool) return refuse("R3-pipeline-tool", `${label}: ${tool.tool} is a ${tool.stage} tool; build, sign, notarize, package and upload run only inside RightKit`);
  if (name === "right-release") return admitRightRelease(args, label);
  if (name === "rightkit-gate") return admitRightKitGate(args, label);
  if (name === "pnpm") return admitPnpm(args, inner, label, chain);
  if (name === "node") return admitNode(args, inner, label);
  if (name === "rightkit") return refuse("R1-retired-client", `${label}: rightkit is the retired name of the RustRight client (rustright#91); use rustright ${args.slice(0, 2).join(" ")}`);
  if (name === "rustright") {
    if (args[0] !== "cargo" || !CARGO_SUBCOMMANDS.has(args[1])) return refuse("R1-managed-client", `${label}: only rustright cargo ${[...CARGO_SUBCOMMANDS].join("|")} is admitted`);
    return shellSafeArgs(args, label) ?? admit(`rustright-cargo-${args[1]}`, "managed-cargo");
  }
  if (name === "cargo" || name === "rustc" || name === "rustdoc") return refuse("R1-bare-cargo", `${label}: run Cargo through the managed client (rustright cargo ...), never bare ${name}`);
  if (name === "tauri") return refuse("R1-direct-tauri", `${label}: run the Tauri CLI as pnpm exec tauri ..., or through right-release; a bare tauri is not an approved installed runtime`);
  if (PACKAGE_MANAGERS.has(name)) return refuse("R1-package-manager", `${label}: ${name} is not allowed; the workspace uses pnpm`);
  if (INTERPRETERS.has(name)) return refuse("R1-interpreter", `${label}: ${name} runs script text or a script file the app owns; only an admitted shared generator may do that`);
  return refuse("R1-unknown-executable", `${label}: ${JSON.stringify(cmd)} is not an admitted RightKit step; unknown pipeline ownership blocks`);
}

const GRADLE_ARG = /^(?::?[A-Za-z][A-Za-z0-9:_-]*|--(?:no-daemon|stacktrace|offline|no-build-cache|warning-mode=(?:all|summary|none|fail)|console=plain)|-P[A-Za-z][A-Za-z0-9._]*=[A-Za-z0-9._:/-]+)$/;
function admitGradle(args, options, label) {
  const stage = options.stage ?? "";
  if (!["android-build", "dev-build"].includes(stage)) return refuse("R1-executable", `${label}: ./gradlew is admitted only as the Android build (targets.android.gradle) or an Android dev build`);
  if (!args.length || args.some((arg) => !GRADLE_ARG.test(arg))) return refuse("R1-option", `${label}: gradle arguments must be plain tasks and approved flags`);
  const task = args.find((arg) => !arg.startsWith("-"));
  if (!task) return refuse("R1-option", `${label}: gradle needs a task`);
  if (/(?<![A-Za-z])(?:publish|upload|sign)(?![a-z])/.test(args.filter((arg) => !arg.startsWith("-")).join(" "))) return refuse("R3-pipeline-tool", `${label}: a gradle publish, upload or sign task is a release effect that belongs to RightKit`);
  return admit("gradle-build", "runtime");
}
