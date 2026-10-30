// The one door for the build and test commands RightKit launches for an app or a unit (rightkit#1241): on a broker-managed host
// every such command is a broker job, so class A admission, the one-build-per-app claim, the in-place freeze and the host's
// resource limits apply to it as they do to a cargo build; on a host without a broker it runs directly, as before.
//
// - pnpm <args>                 -> <rustright> run rightkit-pnpm -- <args>   (the broker's pnpm adapter; compile-flagged on the broker)
// - node <script> <args>        -> <rustright> run rightkit-node -- <script> <args>   (a script inside the job's workspace, relative path)
// - cargo <args>               -> <rustright> cargo <args>   (by the resolved broker client, whatever path the caller named: a real
//                                  cargo binary is never launched around the broker)
// - rustright <args>           -> <rustright> <args>         (the resolved broker client)
// - rightkit-qa <args>         -> <rustright> run rightkit-qa -- <args>   (the whole QA run, its test binaries included, is one broker
//                                  job holding the app's claim; its own `rustright cargo test --no-run` runs nested under that job)
// - this package's right-release -> unchanged (same NODE_OPTIONS as this process): it orchestrates, and every build it starts comes back
//                                  through this door
// A SNAPSHOT STAGE (the gate's carrier map is in the environment, RIGHTKIT_CARRIED_CARGO_MAP) carries every one of those steps itself: a
// `cargo` step, and a pnpm, node, swift or rightkit-qa step as a snapshot-bound adapter job (carried-adapter-run.mjs, rustright#190), one signed,
// ledgered request each, with the compiles of the step's children (the Tauri CLI) nested under it. A pnpm `install` step of a snapshot
// stage is skipped: the gate already installed the snapshot's dependencies.
// - swift <args>                -> <rustright> run rightkit-swift -- <args>   (a native helper's SwiftPM build; carried like the others on a snapshot stage)
// - rustright run <adapter> -- <args> on a snapshot stage -> the same step as the adapter's own program (pnpm, node, rightkit-qa, swift), so
//                                  the door's rules apply to it; any other `rustright <command>` there is refused (RK_BUILD_LAUNCH_REFUSED)
// - pnpm exec <target>          -> only a target in EXEC_TARGETS (tauri, right-release, vite) on a broker host: the broker's pnpm entry
//                                  has no target allowlist, so `pnpm exec node -e ...` would otherwise pass the node flag ban
// - anything else               -> refused on a managed host: no broker adapter runs it yet, and running it directly would bypass the
//                                  broker (fail closed; the per-app impact is mapped in rightkit#1241)
// A launch from inside a broker job is routed the same way: the broker attributes it to its parent job by the parent's own
// capability token (never by an environment marker this module reads), so no launch here decides to skip the broker.
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { resolveExecutor } from "./gate-executor.mjs";
import { resolveExecutable } from "./admission-host.mjs";
import { CARRIED_CARGO_MAP_ENV as CARRIED_MAP_ENV, carriedCargoMapOf } from "./carried-cargo.mjs";

/** The environment variable the door sets for a stage that runs from a commit snapshot (gate-stages.mjs CHECKOUT_ENV). */
const SNAPSHOT_STAGE_ENV = "RK_GATE_CHECKOUT";

export const BUILD_LAUNCH_REFUSED = "RK_BUILD_LAUNCH_REFUSED";
const OWN_RIGHT_RELEASE = fileURLToPath(new URL("./cli/right-release.mjs", import.meta.url));
const CARRIED_RUN = fileURLToPath(new URL("./carried-cargo-run.mjs", import.meta.url));
const CARRIED_ADAPTER_RUN = fileURLToPath(new URL("./carried-adapter-run.mjs", import.meta.url));
// pnpm options that take the next argument as their value (so that value is not the command).
const PNPM_VALUE_OPTIONS = new Set(["--dir", "-C", "--filter", "-F", "--prefix", "--config", "--reporter", "--workspace-concurrency"]);
// Node flags that change no code path; everything else before the script (-e, --eval, -p, --import, -r, --require, --loader, …) refuses.
const NODE_FLAGS = /^--(?:disable-warning=[A-Za-z0-9_-]+|no-experimental-webstorage|no-warnings|enable-source-maps|max-old-space-size=\d+|stack-size=\d+)$/;
/** The pnpm `exec` targets the door admits (every app config's `pnpm exec <target>`: tauri and right-release; vite for a web build). */
export const EXEC_TARGETS = Object.freeze(["tauri", "right-release", "vite"]);
/** The pnpm commands a snapshot-bound broker call admits (the registration's snapshotSubcommands; `install` is skipped before this). */
const SNAPSHOT_PNPM_COMMANDS = new Set(["run", "exec", "pack"]);
/** The broker adapter a `rustright run <adapter> -- ...` names, and the program whose door rules judge the step. */
const ADAPTER_PROGRAMS = Object.freeze({ "rightkit-pnpm": "pnpm", "rightkit-node": "node", "rightkit-qa": "rightkit-qa", "rightkit-swift": "swift" });
const name = (program) => path.basename(String(program ?? "").replaceAll("\\", "/")).replace(/\.(exe|cmd|bat)$/i, "").toLowerCase();

function refuse(message) {
  const error = new Error(message);
  error.code = BUILD_LAUNCH_REFUSED;
  throw error;
}

/** The index of the pnpm command in an argv (the first word that is not an option or an option's value), or -1. */
function pnpmCommandAt(words) {
  for (let i = 0; i < words.length; i += 1) {
    const w = words[i];
    if (w === "--") return -1;
    if (w.startsWith("-")) { if (PNPM_VALUE_OPTIONS.has(w)) i += 1; continue; }
    return i;
  }
  return -1;
}

/** The pnpm command of an argv (the first word that is not an option or an option's value), or null. */
export function pnpmCommandOf(args) {
  const words = args.map(String);
  const at = pnpmCommandAt(words);
  return at < 0 ? null : words[at];
}

/** Whether this environment belongs to a snapshot stage: the gate's carrier map or its source split is in it. */
export const onSnapshotRoute = (env) => Boolean(env?.[CARRIED_MAP_ENV] || env?.[SNAPSHOT_STAGE_ENV]);

/**
 * Why a pnpm step is skipped on the snapshot route, or null: an `install` step (`pnpm install`, `pnpm --dir hub install --frozen-lockfile`)
 * would write into the commit's snapshot after the gate installed its dependencies, and the broker's pnpm adapter refuses it there. The
 * gate's own install is the one the stage runs on (snapshot-check.mjs installSnapshotDependencies).
 */
export function snapshotInstallSkip(command, args, env) {
  if (name(command) !== "pnpm" || !onSnapshotRoute(env)) return null;
  const sub = pnpmCommandOf(args);
  return sub === "install" || sub === "i" ? `pnpm ${args.join(" ")}: the gate already installed this snapshot's dependencies` : null;
}

/**
 * `{ command, args }` to launch for a build or test command, or a refusal. `host` carries env, platform, exists, which and readFile
 * (resolveExecutor's inputs); `which` defaults to a PATH lookup over `env`.
 */
export function buildLaunch(command, args = [], host = {}) {
  const env = host.env ?? process.env;
  const platform = host.platform ?? process.platform;
  const exists = host.exists;
  const lookup = (cmd) => { const found = resolveExecutable(cmd, { env, hostPlatform: platform, exists: exists ?? (() => false) }); return found ? [found] : []; };
  // A snapshot stage never installs into the snapshot (the gate did): the step is a logged no-op, on every host.
  if (snapshotInstallSkip(command, args, env)) return { command: process.execPath, args: [CARRIED_ADAPTER_RUN, "--skip-install", ...args.map(String)] };
  const executor = resolveExecutor({ env, platform, exists, which: host.which ?? lookup, readFile: host.readFile ?? ((file) => readFileSync(file, "utf8")) });
  if (executor.kind === "refuse") refuse(`${name(command)} ${args.join(" ")}: ${executor.problem}`);
  // A direct (Linux) host: the real toolchain, RustRight shim directories off PATH (`env`, to merge into the launch env); a configured
  // `rustright cargo|rustc|rustdoc …` runs that tool directly; any other `rustright` command has no direct form and is refused.
  if (executor.kind === "direct") {
    const extra = executor.env ? { env: executor.env } : {};
    if (name(command) === "rustright") {
      if (!["cargo", "rustc", "rustdoc"].includes(String(args[0]))) refuse(`rustright ${args.join(" ")}: no broker on this ${platform} host, and only rustright cargo|rustc|rustdoc map to the toolchain`);
      return { command: String(args[0]), args: args.slice(1).map(String), ...extra };
    }
    return { command, args, ...extra };
  }
  const program = name(command);
  // A snapshot stage's `cargo` and `rustright cargo` steps are carried (carried-cargo.mjs), never sent to the client: one signed, ledgered
  // request per call, from the step's working directory, on every platform (rightkit#1301 F3).
  const carriedArgs = program === "cargo" ? args : program === "rustright" && String(args[0]) === "cargo" ? args.slice(1) : null;
  const carried = carriedArgs ? carriedCargoMapOf(env) : null;
  // A carrier map that cannot be read is a refusal, never a fallback to the uncarried client (rightkit#1301 M1).
  if (carried?.error) refuse(`${name(command)} ${args.join(" ")}: ${carried.error}; a snapshot stage's Cargo step is never sent to the client around its carrier`);
  if (carriedArgs && carried?.map) return { command: process.execPath, args: [CARRIED_RUN, ...carriedArgs.map(String)] };
  const client = () => lookup("rustright")[0] ?? refuse(`rustright is not on PATH: on a broker-managed host ${program} runs only as a broker job`);
  // A snapshot stage's pnpm, node and QA steps are carried adapter jobs (carried-adapter-run.mjs); anywhere else they are the client's `run`.
  const adapterMap = ["pnpm", "node", "rightkit-qa", "swift"].includes(program) ? carriedCargoMapOf(env) : null;
  if (adapterMap?.error) refuse(`${name(command)} ${args.join(" ")}: ${adapterMap.error}; a snapshot stage's step is never sent to the client around its carrier`);
  const adapterLaunch = (adapter, adapterArgs) => (adapterMap?.map ? { command: process.execPath, args: [CARRIED_ADAPTER_RUN, adapter, "--", ...adapterArgs] } : { command: client(), args: ["run", adapter, "--", ...adapterArgs] });
  if (program === "cargo") return { command: client(), args: ["cargo", ...args.map(String)] };
  if (program === "rustright") {
    const snapshotMap = carriedCargoMapOf(env);
    if (!snapshotMap) return { command: client(), args: args.map(String) };
    if (snapshotMap.error) refuse(`rustright ${args.join(" ")}: ${snapshotMap.error}; a snapshot stage's step is never sent to the client around its carrier`);
    // A snapshot stage: `rustright run <adapter> -- <args>` is that adapter's step (judged by the door's rules for its program, then carried);
    // any other `rustright <command>` is an uncarried client call that no registered adapter takes, so it is refused, naming the step.
    const adapterProgram = args[0] === "run" && args[2] === "--" ? ADAPTER_PROGRAMS[String(args[1])] : undefined;
    if (!adapterProgram) refuse(`rustright ${args.join(" ")}: a snapshot stage carries every compile, and this client call maps to no registered adapter (rustright run rightkit-pnpm|rightkit-node|rightkit-qa|rightkit-swift -- <args>); it is never sent to the client uncarried`);
    return buildLaunch(adapterProgram, args.slice(3), host);
  }
  if (program === "rightkit-qa") return adapterLaunch("rightkit-qa", args.map(String));
  if (program === "swift") return adapterLaunch("rightkit-swift", args.map(String));
  // This package's own right-release, run by this very Node binary (not any executable named node), with no startup code added by
  // the caller: NODE_OPTIONS is empty or exactly this process's own string (a hook's env could otherwise --import code that builds
  // outside the broker; reordered tokens can re-pair an option with another value, so nothing short of equality is accepted).
  if (command === process.execPath && path.resolve(String(args[0] ?? "")) === OWN_RIGHT_RELEASE) {
    const options = String(env.NODE_OPTIONS ?? "");
    if (options.trim() !== "" && options !== String(process.env.NODE_OPTIONS ?? "")) refuse(`right-release ${args.slice(1).join(" ")}: NODE_OPTIONS is neither empty nor exactly this process's own; on a broker-managed host the door's own right-release runs with no added startup code (rightkit#1241)`);
    return { command, args };
  }
  if (program === "node") {
    // Leading node flags are limited to ones that change no code path (warnings, web storage, heap/stack size); an eval, import,
    // require or loader flag would run code that is not the script, so it is refused. The adapter takes the script as a path
    // relative to the job's workspace (its cwd); an absolute or escaping path is refused here and by the broker's relpath check.
    const at = args.findIndex((a) => !String(a).startsWith("-"));
    const flags = (at < 0 ? args : args.slice(0, at)).map(String);
    const bad = flags.find((f) => !NODE_FLAGS.test(f));
    if (at < 0 || bad !== undefined) refuse(`node ${args.join(" ")}: ${bad !== undefined ? `node flag ${bad} is not allowed` : "no script"}; on a broker-managed host node runs only a workspace script as a broker job (rightkit-node, rightkit#1241)`);
    const cwd = host.cwd ?? process.cwd();
    const script = path.isAbsolute(String(args[at])) ? path.relative(cwd, String(args[at])) : String(args[at]);
    if (!script || script.startsWith("..") || path.isAbsolute(script)) refuse(`node ${args[0]}: the script is outside the job's workspace (${cwd}); on a broker-managed host node runs only workspace scripts as broker jobs (rightkit-node, rightkit#1241)`);
    return adapterLaunch("rightkit-node", [...flags, script.replaceAll("\\", "/"), ...args.slice(at + 1).map(String)]);
  }
  if (program === "pnpm") {
    const words = args.map(String);
    const at = pnpmCommandAt(words);
    // `pnpm exec <target> ...`: the broker registers `exec` with an opaque tail and no target allowlist, so the door admits only the targets the
    // apps' configs run. `pnpm exec node -e ...` (or any other binary) would otherwise run code the node flag ban exists to stop.
    if (at >= 0 && words[at] === "exec" && !EXEC_TARGETS.includes(words[at + 1] ?? "")) refuse(`pnpm ${words.join(" ")}: pnpm exec target ${JSON.stringify(words[at + 1] ?? null)} is not allowed (allowed: ${EXEC_TARGETS.join(", ")}); on a broker-managed host pnpm exec runs only those, as a broker job (rightkit#1241)`);
    // A snapshot-bound broker call admits run, exec and pack only; a bare script name (\`pnpm build\`) is refused here, naming the step, not by the broker.
    if (adapterMap?.map && at >= 0 && !SNAPSHOT_PNPM_COMMANDS.has(words[at])) refuse(`pnpm ${words.join(" ")}: a snapshot stage runs pnpm run, exec and pack only (the broker's snapshotSubcommands); write \`pnpm run ${words[at]}\` for a package script`);
    return adapterLaunch("rightkit-pnpm", words);
  }
  return refuse(`${program} ${args.join(" ")}: no broker adapter runs this command yet, and on a broker-managed host a build or test command never runs around the broker (rightkit#1241)`);
}
