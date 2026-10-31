// The one door for the build and test commands RightKit launches for an app or a unit (rightkit#1241): on a broker-managed host
// every such command is a broker job, so class A admission, the one-build-per-app claim, the in-place freeze and the host's
// resource limits apply to it as they do to a cargo build; on a host without a broker it runs directly, as before.
//
// - pnpm <args>                 -> <rustright> run rightkit-pnpm -- <args>   (the broker's pnpm adapter; compile-flagged on the broker)
// - node <script> <args>        -> <rustright> run rightkit-node -- <script> <args>   (a script inside the job's workspace, relative path)
// - cargo <args>               -> <rustright> cargo <args>   (by the resolved broker client, whatever path the caller named: a real
//                                  cargo binary is never launched around the broker)
// - rustright <args>           -> <rustright> <args>         (the resolved broker client)
// - rightkit-qa <args>         -> <rustright> run rightkit-qa -- <args>   (off the snapshot route: the whole QA run, its test binaries included,
//                                  is one broker job holding the app's claim). ON a snapshot stage it is NOT an adapter job: QA runs an
//                                  already-built artifact and compiles nothing, so it stays a direct, gate-run step with its GUI session and
//                                  unscrubbed environment. A `run-tests` step with no `--from-cargo-json` is two parts (qa-snapshot-run.mjs): a
//                                  carried test build whose listing lands under the carrier's managed directory, then QA directly against
//                                  it. A `journeys run` step is the same two parts (qa-journeys-prebuilt-run.mjs, GB-79: the gate carries the build, signs the products, hands QA the verified input) when rightkit-qa advertises `journeys-prebuilt-input`, else refused naming it. Any other QA step whose argv would build (`--build*`) is refused (`qaBuildReason`), as
//                                  is a QA directory outside the snapshot or an input that resolves into the primary checkout (`qaPathReason`)
// - this package's right-release -> unchanged (same NODE_OPTIONS as this process): it orchestrates, and every build it starts comes back
//                                  through this door
// A SNAPSHOT STAGE (the gate's carrier map is in the environment, RIGHTKIT_CARRIED_CARGO_MAP) carries every one of those steps itself: a
// `cargo` step, and a pnpm, node or swift step as a snapshot-bound adapter job (carried-adapter-run.mjs, rustright#190), one signed,
// ledgered request each, with the compiles of the step's children (the Tauri CLI) nested under it. A pnpm `install` step of a snapshot
// stage is skipped: the gate already installed the snapshot's dependencies.
// - swift <args>                -> <rustright> run rightkit-swift -- <args>   (a native helper's SwiftPM build; carried like the others on a snapshot stage)
// A carried adapter step's own environment (the `env` the step declares, passed as `host.stepEnv`) is forwarded as the adapter request's
// environment: the door sets RUSTRIGHT_ADAPTER_ENV to exactly those names (adapter-env.mjs), the broker accepts the names the adapter's
// registration lists in `env`, and the values are bound into the signed request. A reserved name is refused here; a step with no env gets no
// RUSTRIGHT_ADAPTER_ENV.
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
import { ADAPTER_ENV_MANIFEST, QA_HANDOFF_ENV, adapterEnvLaunch } from "./adapter-env.mjs";
import { cargoSubcommandOf, canonicalCargoCommand } from "./cargo-shapes.mjs";
import { swiftScratchRefusal } from "./swift-scratch.mjs";
import { NATIVE_ADAPTERS, nativeToolLaunch, nativeToolOf } from "./native-adapters.mjs";
import { PNPM_ADAPTER_GRAMMAR, adapterArgvRefusal, liveAdapterGrammar, scanAdapterArgv } from "./pnpm-adapter-grammar.mjs";
import { isJourneysRun, journeysCapabilityRefusal, journeysRunRefusal, qaPrebuiltCapability } from "./qa-journeys-prebuilt.mjs";
import { QA_VALUE_OPTIONS, hasListing, isBuildFlag, ownWords, qaPathReason, qaSnapshotPlan } from "./qa-snapshot.mjs";

/** The environment variable the door sets for a stage that runs from a commit snapshot (gate-stages.mjs CHECKOUT_ENV). */
const SNAPSHOT_STAGE_ENV = "RK_GATE_CHECKOUT";

export const BUILD_LAUNCH_REFUSED = "RK_BUILD_LAUNCH_REFUSED";
/** The program name a sign step would have if it were a command: the door refuses it on every route. */
export const SIGN_STEP_COMMAND = "rightkit-sign-step";
const OWN_RIGHT_RELEASE = fileURLToPath(new URL("./cli/right-release.mjs", import.meta.url));
// GB-06: the helpers the package recipes generate (package-recipe-targets.mjs) are this package's own release operations, run by this very Node binary at their exact installed
// paths, like right-release itself: they orchestrate in the gate's own session, and every build-capable child they start comes back through this door as a carried step.
// Only these exact files qualify; an edited app or a copy under its node_modules is a different path and is refused like any other script outside the job's workspace.
const RECIPE_BUILD_HELPER = fileURLToPath(new URL("./recipe-build.mjs", import.meta.url));
const CREATE_MAC_UPDATER_HELPER = fileURLToPath(new URL("./create-mac-updater.mjs", import.meta.url));
export const INTERNAL_RELEASE_HELPERS = Object.freeze([OWN_RIGHT_RELEASE, RECIPE_BUILD_HELPER, CREATE_MAC_UPDATER_HELPER]);
const CARRIED_RUN = fileURLToPath(new URL("./carried-cargo-run.mjs", import.meta.url));
const CARRIED_ADAPTER_RUN = fileURLToPath(new URL("./carried-adapter-run.mjs", import.meta.url));
const QA_SNAPSHOT_RUN = fileURLToPath(new URL("./qa-snapshot-run.mjs", import.meta.url));
const QA_JOURNEYS_RUN = fileURLToPath(new URL("./qa-journeys-prebuilt-run.mjs", import.meta.url));
// pnpm options that take the next argument as their value (so that value is not the command).
const PNPM_VALUE_OPTIONS = new Set(["--dir", "-C", "--filter", "-F", "--prefix", "--config", "--reporter", "--workspace-concurrency"]);
// Node flags that change no code path; everything else before the script (-e, --eval, -p, --import, -r, --require, --loader, …) refuses.
const NODE_FLAGS = /^--(?:disable-warning=[A-Za-z0-9_-]+|no-experimental-webstorage|no-warnings|enable-source-maps|max-old-space-size=\d+|stack-size=\d+)$/;
/** The pnpm `exec` targets the door admits (every app config's `pnpm exec <target>`: tauri and right-release; vite for a web build). */
export const EXEC_TARGETS = Object.freeze(["tauri", "right-release", "vite"]);
/** The broker adapter a `rustright run <adapter> -- ...` names, and the program whose door rules judge the step. */
const ADAPTER_PROGRAMS = Object.freeze({ "rightkit-pnpm": "pnpm", "rightkit-node": "node", "rightkit-qa": "rightkit-qa", "rightkit-swift": "swift", "rightkit-xcodegen": "xcodegen", "rightkit-xcodebuild": "xcodebuild", "rightkit-gradle": "gradle", "rightkit-cmake": "cmake" });
/** The tools a QA step must not start: running one is a build, and a QA step compiles nothing. */
const QA_BUILD_PROGRAMS = new Set(["cargo", "rustc", "rustdoc", "rustright", "rightkit", "pnpm", "npm", "npx", "yarn", "swift", "swiftc", "xcodebuild", "gradle", "gradlew", "make", "cmake", "node-gyp"]);
export { QA_VALUE_OPTIONS };
/**
 * Why a `rightkit-qa` argv would build, or null. A QA step on a snapshot stage runs an already-built artifact: `run-tests` compiles its
 * test binaries unless it is handed an existing listing (`--from-cargo-json`, read in flag position only), `journeys run` builds through the
 * broker, `run --build`, `--build-ui` and `--build-with` build, and any named toolchain command is a build. `carried` says the gate carries a
 * `run-tests` step's build itself (qa-snapshot.mjs), so that one is not reported; `journeys run` has no prebuilt form and always is.
 */
export function qaBuildReason(args, { carried = false } = {}) {
  const words = args.map(String);
  // run-tests has its own parser (words after `--` are test arguments); every other command's Args reads the whole argv (rightkit-qa.rs
  // Args::has / value ignore `--`), so for those a build flag or tool anywhere is the build.
  const own = words[0] === "run-tests" ? ownWords(words) : words;
  if (words[0] === "run-tests" && !hasListing(words) && !carried) return "`rightkit-qa run-tests` without --from-cargo-json compiles its test binaries (`cargo test --no-run`)";
  // GB-79: `journeys run` spawns its own `rustright cargo ... --no-run`. On a gate-carried stage the gate carries that build itself and hands QA the verified, signed input
  // (qa-journeys-prebuilt.mjs, capability journeys-prebuilt-input), so there it is not a build; anywhere else it is, and a listing or attestation the step names is unverified input.
  if (words[0] === "journeys" && words[1] === "run" && !carried) return "`rightkit-qa journeys run` builds through the broker before it launches (it spawns its own `rustright cargo ... --no-run`); only a gate-carried stage runs it, on the gate's own verified prebuilt input (capability journeys-prebuilt-input)";
  if (words[0] === "journeys" && words[1] === "run") { const own = journeysRunRefusal(words); if (own) return own; }
  if (own.some(isBuildFlag)) return "a --build, --build-ui or --build-with flag makes the QA run build";
  const tool = own.find((w) => QA_BUILD_PROGRAMS.has(name(w)));
  if (tool !== undefined) return `the argument ${JSON.stringify(tool)} names a build tool (${name(tool)}); a QA step compiles nothing`;
  return null;
}
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

/**
 * The pnpm command of an argv read LENIENTLY (the first word that is not an option or an option's value), or null. Only the install-skip of a snapshot
 * stage uses it (a stage that skips `install` runs nothing, so a lenient reading can only skip more); everything the door SENDS is judged by the
 * registration's own parser (pnpm-adapter-grammar.mjs scanAdapterArgv, GB-03).
 */
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
  // A `sign` step of tests.suites (suite-signing.mjs) runs in-process in the gate's own session: it is no command, no broker job and never carried.
  if (command === SIGN_STEP_COMMAND || host.signStep) refuse("a sign step runs in-process in the gate's own session (signing agent, Keychain and Azure signer are the session's, not a broker job's); it never reaches the carrier (rightkit#1303)");
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
  const handoffEnv = () => {
    const found = {};
    for (const key of QA_HANDOFF_ENV) if (typeof env[key] === "string" && env[key] !== "") found[key] = env[key];
    return found;
  };
  // GB-05: a Cargo suite consumer (`cargo test|bench|run|nextest`) receives the signed handoff names the same way, through RUSTRIGHT_FORWARD_ENV
  // (the client forwards the caller's value for each name, for these runtime commands only; the repository declares both names in `forwardEnv` of its
  // `.rustright/broker.json`, or the host lists them in `buildRules.forwardTestEnv`). Nothing here carries a signing key or credential.
  const cargoHandoffEnv = (cargoArgs) => {
    const present = Object.keys(handoffEnv());
    if (!present.length || !["test", "bench", "run", "nextest"].includes(canonicalCargoCommand(cargoSubcommandOf(cargoArgs.map(String))))) return {};
    for (const key of present) {
      if (!(path.isAbsolute(env[key]) || path.win32.isAbsolute(env[key])) || env[key].split(/[\\/]/).includes("..")) refuse(`${name(command)} ${args.join(" ")}: ${key} must be an absolute path with no '..' (the signed artifact's exact path); it is not forwarded`);
    }
    const existing = String(env.RUSTRIGHT_FORWARD_ENV ?? "").split(",").map((n) => n.trim()).filter(Boolean);
    return { env: { RUSTRIGHT_FORWARD_ENV: [...new Set([...existing, ...present])].join(",") } };
  };
  // A snapshot stage's `cargo` and `rustright cargo` steps are carried (carried-cargo.mjs), never sent to the client: one signed, ledgered
  // request per call, from the step's working directory, on every platform (rightkit#1301 F3).
  const carriedArgs = program === "cargo" ? args : program === "rustright" && String(args[0]) === "cargo" ? args.slice(1) : null;
  const carried = carriedArgs ? carriedCargoMapOf(env) : null;
  // A carrier map that cannot be read is a refusal, never a fallback to the uncarried client (rightkit#1301 M1).
  if (carried?.error) refuse(`${name(command)} ${args.join(" ")}: ${carried.error}; a snapshot stage's Cargo step is never sent to the client around its carrier`);
  if (carriedArgs && carried?.map) return { command: process.execPath, args: [CARRIED_RUN, ...carriedArgs.map(String)], ...cargoHandoffEnv(carriedArgs) };
  const client = () => lookup("rustright")[0] ?? refuse(`rustright is not on PATH: on a broker-managed host ${program} runs only as a broker job`);
  // A snapshot stage's pnpm, node and swift steps are carried adapter jobs (carried-adapter-run.mjs); anywhere else they are the client's `run`.
  // A QA step is not one of them: it runs an already-built artifact (see the rightkit-qa branch below).
  const adapterMap = ["pnpm", "node", "swift"].includes(program) || nativeToolOf(command) !== null ? carriedCargoMapOf(env) : null;
  if (adapterMap?.error) refuse(`${name(command)} ${args.join(" ")}: ${adapterMap.error}; a snapshot stage's step is never sent to the client around its carrier`);
  // The step's own environment rides the adapter request (adapter-env.mjs): RUSTRIGHT_ADAPTER_ENV names exactly what the step declares, plus the
  // signed-artifact handoff (GB-05) when a sign step set it in this environment. Every name is judged against the adapter's registered `env`
  // (the live registration, else the candidate manifest) BEFORE launch (GB-04): an unregistered key refuses, naming it and the registered set.
  const stepEnvLaunch = (adapter) => {
    try {
      const registered = host.registeredEnv?.[adapter] ?? liveAdapterGrammar({ env, platform, readFile: host.readFile, adapter })?.env ?? ADAPTER_ENV_MANIFEST[adapter] ?? [];
      return adapterEnvLaunch({ ...(host.stepEnv ?? {}), ...handoffEnv() }, { registered });
    } catch (error) {
      const refusal = new Error(`${name(command)} ${args.join(" ")}: ${error.message}`);
      refusal.code = BUILD_LAUNCH_REFUSED;
      refusal.reason = error.code;
      throw refusal;
    }
  };
  const adapterLaunch = (adapter, adapterArgs) => {
    const stepLaunchEnv = stepEnvLaunch(adapter);
    const launchEnv = Object.keys(stepLaunchEnv).length ? { env: stepLaunchEnv } : {};
    return adapterMap?.map ? { command: process.execPath, args: [CARRIED_ADAPTER_RUN, adapter, "--", ...adapterArgs], ...launchEnv } : { command: client(), args: ["run", adapter, "--", ...adapterArgs], ...launchEnv };
  };
  if (program === "cargo") return { command: client(), args: ["cargo", ...args.map(String)], ...cargoHandoffEnv(args) };
  if (program === "rustright") {
    const snapshotMap = carriedCargoMapOf(env);
    if (!snapshotMap) {
      // GB-27: the client spelling of the same unregistered adapter is refused as well (no snapshot carrier, no rightkit-qa route).
      if (args[0] === "run" && String(args[1]) === "rightkit-qa") refuse(`rustright ${args.join(" ")}: a QA run is a direct, noncompiling step of a snapshot stage under the door; the broker registers no rightkit-qa adapter, so outside a snapshot stage there is no uncarried route`);
      return { command: client(), args: args.map(String) };
    }
    if (snapshotMap.error) refuse(`rustright ${args.join(" ")}: ${snapshotMap.error}; a snapshot stage's step is never sent to the client around its carrier`);
    // A snapshot stage: `rustright run <adapter> -- <args>` is that adapter's step (judged by the door's rules for its program, then carried);
    // any other `rustright <command>` is an uncarried client call that no registered adapter takes, so it is refused, naming the step.
    const adapterProgram = args[0] === "run" && args[2] === "--" ? ADAPTER_PROGRAMS[String(args[1])] : undefined;
    if (!adapterProgram) refuse(`rustright ${args.join(" ")}: a snapshot stage carries every compile, and this client call maps to no registered adapter (rustright run rightkit-pnpm|rightkit-node|rightkit-qa|rightkit-swift -- <args>); it is never sent to the client uncarried`);
    return buildLaunch(adapterProgram, args.slice(3), host);
  }
  if (program === "rightkit-qa") {
    // On a snapshot stage QA is a direct, gate-run step against the already-built artifact: no broker job (a QA run needs the GUI session and
    // the unscrubbed environment the broker's job lacks), and nothing in it may compile (the broker refuses an uncarried compile from the snapshot).
    const snapshotMap = carriedCargoMapOf(env);
    if (snapshotMap?.error) refuse(`rightkit-qa ${args.join(" ")}: ${snapshotMap.error}; a snapshot stage's step is never run around its carrier`);
    // GB-27 (GATE one entry): the broker registers no `rightkit-qa` adapter, so a QA step with no snapshot carrier has no route: it is
    // refused here, naming the step, never sent as `rustright run rightkit-qa` (an unregistered capability the broker would refuse after the
    // gate had marked the call submitted). QA runs only as the direct, noncompiling step of a snapshot stage under the door.
    if (!snapshotMap) refuse(`rightkit-qa ${args.join(" ")}: a QA run is a direct, noncompiling step of a snapshot stage under the door; the broker registers no rightkit-qa adapter, so outside a snapshot stage there is no uncarried route (and any build the run needs is its own carried build step)`);
    // The step runs from the snapshot and reads only the snapshot and managed output: its directory is inside the snapshot, and nothing it
    // names (the listing, --root, --cwd, --manifest-path) resolves into the primary checkout.
    const section = snapshotMap.map.broker ?? {};
    const checkouts = [section.checkout, env[SNAPSHOT_STAGE_ENV]].filter((c) => typeof c === "string" && c);
    const placed = qaPathReason(args, { cwd: host.cwd ?? process.cwd(), snapshot: section.snapshot?.path, checkouts, env: { ...env, ...host.stepEnv } });
    if (placed) refuse(`rightkit-qa ${args.join(" ")}: ${placed}`);
    const reason = qaBuildReason(args, { carried: true });
    if (reason) refuse(`rightkit-qa ${args.join(" ")}: ${reason}; a QA step of a snapshot stage runs the already-built artifact and compiles nothing, so a build it needs is its own carried build step (a test build with --no-run --message-format=json, then run-tests --from-cargo-json)`);
    const qa = lookup("rightkit-qa")[0] ?? "rightkit-qa";
    // `run-tests` with no listing: the gate carries its test build, then runs QA directly against the listing (qa-snapshot-run.mjs).
    // GB-79: a `journeys run` step is the gate's build-once route (carry the build, sign the products, hand QA the verified input) when QA implements the prebuilt input.
    if (isJourneysRun(args)) {
      const capable = (host.qaPrebuiltCapability ?? ((binary) => qaPrebuiltCapability(binary, { env: { ...env, ...host.stepEnv } })))(qa);
      if (!capable) refuse(journeysCapabilityRefusal(`rightkit-qa ${args.join(" ")}`));
      return { command: process.execPath, args: [QA_JOURNEYS_RUN, qa, "--", ...args.map(String)] };
    }
    const plan = qaSnapshotPlan(args);
    if (plan?.refusal) refuse(`rightkit-qa ${args.join(" ")}: ${plan.refusal}`);
    if (plan) return { command: process.execPath, args: [QA_SNAPSHOT_RUN, qa, "--", ...args.map(String)] };
    return { command: qa, args: args.map(String) };
  }
  // GB-51: a declared native build tool (xcodegen, xcodebuild, Gradle and its wrapper, CMake) is a snapshot-bound adapter job of its own constrained adapter (native-adapters.mjs): its argv is
  // judged by the registration BEFORE the send (an output override or an unregistered flag is refused here, naming the capability), paths are made relative to the step's directory, and the
  // step carries no environment of its own. Nothing runs the tool around the broker.
  const nativeTool = nativeToolOf(command);
  if (nativeTool !== null) {
    const launch = nativeToolLaunch(nativeTool, args, { cwd: host.cwd ?? process.cwd() });
    if (launch.refusal) refuse(`${launch.refusal.code}: ${launch.refusal.message}`);
    return adapterLaunch(NATIVE_ADAPTERS[nativeTool].adapter, launch.args);
  }
  if (program === "swift") {
    // GB-67: the broker owns SwiftPM's scratch and build path; a caller that names one is refused here, typed, before anything is sent.
    const owned = swiftScratchRefusal(args.map(String));
    if (owned) refuse(`${owned.code}: ${owned.message}`);
    return adapterLaunch("rightkit-swift", args.map(String));
  }
  // This package's own right-release, run by this very Node binary (not any executable named node), with no startup code added by
  // the caller: NODE_OPTIONS is empty or exactly this process's own string (a hook's env could otherwise --import code that builds
  // outside the broker; reordered tokens can re-pair an option with another value, so nothing short of equality is accepted).
  if (command === process.execPath && INTERNAL_RELEASE_HELPERS.includes(path.resolve(String(args[0] ?? "")))) {
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
    // GB-03: the registration's own parser decides, before anything is sent (the live registration when this host has it, else the reference registration
    // RustRight published): the command token, the leading global flags, `--if-present` after `run`, the opaque tail of `run` and `exec`, the registered exec
    // targets, the pack destination, and in a snapshot-bound call the snapshot subcommands. No leading option is skipped to find the command, and nothing is rewritten.
    const grammar = (host.pnpmGrammar ?? liveAdapterGrammar({ env, platform, readFile: host.readFile })) ?? PNPM_ADAPTER_GRAMMAR;
    const grammarProblem = adapterArgvRefusal(words, grammar, { snapshot: Boolean(adapterMap?.map) });
    if (grammarProblem) refuse(`pnpm ${words.join(" ")}: ${grammarProblem}; the broker's rightkit-pnpm adapter would refuse it (rightkit#1241), so it is not sent`);
    // `pnpm exec <target> ...`: the door admits only the targets the apps' configs run, and only those the registration admits (`opaqueTargets.exec`).
    const scanned = scanAdapterArgv(words, grammar);
    if (scanned.command === "exec" && !EXEC_TARGETS.includes(scanned.target ?? "")) refuse(`pnpm ${words.join(" ")}: pnpm exec target ${JSON.stringify(scanned.target ?? null)} is not allowed (allowed: ${EXEC_TARGETS.join(", ")}); on a broker-managed host pnpm exec runs only those, as a broker job (rightkit#1241)`);
    return adapterLaunch("rightkit-pnpm", words);
  }
  return refuse(`${program} ${args.join(" ")}: no broker adapter runs this command yet, and on a broker-managed host a build or test command never runs around the broker (rightkit#1241)`);
}
