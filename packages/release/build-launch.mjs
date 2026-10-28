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
// - anything else               -> refused on a managed host: no broker adapter runs it yet, and running it directly would bypass the
//                                  broker (fail closed; the per-app impact is mapped in rightkit#1241)
// A launch from inside a broker job is routed the same way: the broker attributes it to its parent job by the parent's own
// capability token (never by an environment marker this module reads), so no launch here decides to skip the broker.
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { resolveExecutor } from "./gate-executor.mjs";
import { resolveExecutable } from "./admission-host.mjs";

export const BUILD_LAUNCH_REFUSED = "RK_BUILD_LAUNCH_REFUSED";
const OWN_RIGHT_RELEASE = fileURLToPath(new URL("./cli/right-release.mjs", import.meta.url));
// Node flags that change no code path; everything else before the script (-e, --eval, -p, --import, -r, --require, --loader, …) refuses.
const NODE_FLAGS = /^--(?:disable-warning=[A-Za-z0-9_-]+|no-experimental-webstorage|no-warnings|enable-source-maps|max-old-space-size=\d+|stack-size=\d+)$/;
const name = (program) => path.basename(String(program ?? "").replaceAll("\\", "/")).replace(/\.(exe|cmd|bat)$/i, "").toLowerCase();

function refuse(message) {
  const error = new Error(message);
  error.code = BUILD_LAUNCH_REFUSED;
  throw error;
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
  const client = () => lookup("rustright")[0] ?? refuse(`rustright is not on PATH: on a broker-managed host ${program} runs only as a broker job`);
  if (program === "cargo") return { command: client(), args: ["cargo", ...args.map(String)] };
  if (program === "rustright") return { command: client(), args: args.map(String) };
  if (program === "rightkit-qa") return { command: client(), args: ["run", "rightkit-qa", "--", ...args.map(String)] };
  if (program === "swift") return { command: client(), args: ["run", "rightkit-swift", "--", ...args.map(String)] };
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
    return { command: client(), args: ["run", "rightkit-node", "--", ...flags, script.replaceAll("\\", "/"), ...args.slice(at + 1).map(String)] };
  }
  if (program === "pnpm") {
    return { command: client(), args: ["run", "rightkit-pnpm", "--", ...args.map(String)] };
  }
  return refuse(`${program} ${args.join(" ")}: no broker adapter runs this command yet, and on a broker-managed host a build or test command never runs around the broker (rightkit#1241)`);
}
