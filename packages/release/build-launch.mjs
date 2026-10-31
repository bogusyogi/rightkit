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
//                                  it. Any other QA step whose argv would build (`journeys run`, `--build*`) is refused (`qaBuildReason`), as
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
// - xcodegen, xcodebuild, gradlew -> <rustright> run rightkit-xcodegen|rightkit-xcodebuild|rightkit-gradle -- <args> (B5/B6: GATE-CONTRACT requires
//                                  every Mac and Dell build to be a broker job, so no direct-session exemption exists). Each is a registered, constrained
//                                  adapter: the door admits only the argv XCODEGEN_/XCODEBUILD_/GRADLE_ grammars list, and a snapshot stage carries it
//                                  as a snapshot-bound adapter job. An unregistered adapter is the broker's refusal (RK_ADAPTER_UNKNOWN), never a bypass.
// - cmake <args>               -> <rustright> run rightkit-cmake -- <args> (a snapshot-bound adapter job on a snapshot stage), only for the argv
//                                  CMAKE_FLAGS admit: configure (-S -B -D -G -A) or build (--build --config --target --parallel); -E, -P, --install,
//                                  --preset and any other flag are refused here (M28)
// - this package's recipe-build.mjs and create-mac-updater.mjs (a package target's prePackage and postSign) -> in process like right-release: they
//                                  orchestrate and each build they start comes back through this door; TAURI_SIGNING_* never rides a broker request (M4)
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
import { adapterEnvLaunch } from "./adapter-env.mjs";
import { PNPM_ADAPTER_GRAMMAR, liveAdapterGrammar, pnpmGrammarRefusal } from "./pnpm-adapter-grammar.mjs";
import { QA_VALUE_OPTIONS, hasListing, isBuildFlag, ownWords, qaPathReason, qaSnapshotPlan } from "./qa-snapshot.mjs";

/** The environment variable the door sets for a stage that runs from a commit snapshot (gate-stages.mjs CHECKOUT_ENV). */
const SNAPSHOT_STAGE_ENV = "RK_GATE_CHECKOUT";

export const BUILD_LAUNCH_REFUSED = "RK_BUILD_LAUNCH_REFUSED";
/** The program name a sign step would have if it were a command: the door refuses it on every route. */
export const SIGN_STEP_COMMAND = "rightkit-sign-step";
const OWN_RIGHT_RELEASE = fileURLToPath(new URL("./cli/right-release.mjs", import.meta.url));
const CARRIED_RUN = fileURLToPath(new URL("./carried-cargo-run.mjs", import.meta.url));
const CARRIED_ADAPTER_RUN = fileURLToPath(new URL("./carried-adapter-run.mjs", import.meta.url));
const QA_SNAPSHOT_RUN = fileURLToPath(new URL("./qa-snapshot-run.mjs", import.meta.url));
// This package's own orchestrators that a package target's hooks name by absolute path (package-recipe-targets.mjs): they run in process, as right-release does.
const OWN_SCRIPTS = Object.freeze([OWN_RIGHT_RELEASE, fileURLToPath(new URL("./recipe-build.mjs", import.meta.url)), fileURLToPath(new URL("./create-mac-updater.mjs", import.meta.url))]);
// pnpm options that take the next argument as their value (so that value is not the command): every registered value flag of the adapter, plus the
// two global options the adapter does not register (so a step naming them still finds its command token and is refused by the grammar check, never
// misread: `--dir a --store-dir s exec node -e` must not read `s` as the command and skip the exec target rule).
const PNPM_VALUE_OPTIONS = new Set([...Object.entries(PNPM_ADAPTER_GRAMMAR.flags).filter(([, kind]) => kind !== "none").map(([flag]) => flag), "--prefix", "--config"]);
// Node flags that change no code path; everything else before the script (-e, --eval, -p, --import, -r, --require, --loader, …) refuses.
const NODE_FLAGS = /^--(?:disable-warning=[A-Za-z0-9_-]+|no-experimental-webstorage|no-warnings|enable-source-maps|max-old-space-size=\d+|stack-size=\d+)$/;
/** The pnpm `exec` targets the door admits (every app config's `pnpm exec <target>`: tauri and right-release; vite for a web build). */
export const EXEC_TARGETS = Object.freeze(["tauri", "right-release", "vite"]);
/** The pnpm commands a snapshot-bound broker call admits (the registration's snapshotSubcommands; `install` is skipped before this). */
const SNAPSHOT_PNPM_COMMANDS = new Set(["run", "exec", "pack"]);
/** The broker adapter a `rustright run <adapter> -- ...` names, and the program whose door rules judge the step. */
const ADAPTER_PROGRAMS = Object.freeze({ "rightkit-pnpm": "pnpm", "rightkit-node": "node", "rightkit-qa": "rightkit-qa", "rightkit-swift": "swift", "rightkit-cmake": "cmake", "rightkit-xcodegen": "xcodegen", "rightkit-xcodebuild": "xcodebuild", "rightkit-gradle": "gradlew" });
/** The program of a step, to the adapter that runs it (the inverse of ADAPTER_PROGRAMS for the toolchain programs added by B5/B6). */
const TOOLCHAIN_ADAPTERS = Object.freeze({ xcodegen: "rightkit-xcodegen", xcodebuild: "rightkit-xcodebuild", gradlew: "rightkit-gradle" });
/** The programs the door admits at all (a `command` sidecar of a snapshot stage must be one of these, stage-sidecar.mjs). */
export const DOOR_PROGRAMS = Object.freeze(["cargo", "pnpm", "node", "swift", "rustright", "rightkit-qa", "cmake", "right-release", "xcodegen", "xcodebuild", "gradlew"]);
/** xcodegen: `generate --spec <file> --project <dir>` only. */
export const XCODEGEN_FLAGS = Object.freeze({ subcommands: ["generate"], value: ["--spec", "--project"] });
/** xcodebuild: these flags (value flags take the next word), these actions, and `NAME=value` build settings that are not code or path overrides. */
export const XCODEBUILD_FLAGS = Object.freeze({
  value: ["-project", "-workspace", "-scheme", "-configuration", "-destination", "-derivedDataPath", "-resultBundlePath", "-testPlan", "-sdk", "-archivePath", "-exportPath", "-exportOptionsPlist", "-xcconfig"],
  plain: ["-allowProvisioningUpdates", "-allowProvisioningDeviceRegistration", "-quiet", "-exportArchive"],
  actions: ["build", "test", "build-for-testing", "test-without-building", "clean", "archive"],
  forbiddenSettings: /^(?:OTHER_[A-Z_]*FLAGS|[A-Z_]*_SCRIPT[A-Z_]*|SHELL|PATH|DYLD_[A-Z_]*|LD_[A-Z_]*|CC|CXX|LD|SWIFT_EXEC)$/,
});
/** gradlew: task names and these flags; no property (-P/-D), init script, project-dir or user-home redirect. */
export const GRADLE_FLAGS = Object.freeze({ plain: ["--no-daemon", "--offline", "--stacktrace", "--info", "--warn", "--quiet", "-q", "--no-build-cache", "--rerun-tasks"], prefixed: ["--console=", "--warning-mode="] });
/** The cmake arguments the `rightkit-cmake` adapter takes: a configure (-S -B -D -G -A) or a build (--build --config --target --parallel). */
export const CMAKE_FLAGS = Object.freeze({ value: ["-S", "-B", "-G", "-A", "-D", "--build", "--config", "--target"], optionalNumber: ["--parallel"] });
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
  if (words[0] === "journeys" && words[1] === "run") return "`rightkit-qa journeys run` builds through the broker before it launches, and rightkit-qa has no prebuilt mode for it (it spawns its own `rustright cargo ... --no-run`)";
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

/**
 * Why a cmake argv is not one the `rightkit-cmake` adapter takes, or null (M28). A configure is `-S <src> -B <build> [-D<NAME>=<VALUE>]... [-G <gen>] [-A <arch>]`,
 * a build is `--build <dir> [--config <cfg>] [--target <t>] [--parallel [N]]`; `-E` (command mode), `-P` (script mode), `--install`, `--preset`, `-C`,
 * `--toolchain` and every other flag or positional run code or move the build elsewhere and are refused.
 */
export function cmakeRefusal(args) {
  const words = args.map(String);
  if (words.length === 0) return "cmake needs a configure (-S -B) or a build (--build <dir>)";
  const admissible = "admitted: -S -B -D -G -A (configure) and --build --config --target --parallel (build)";
  for (let i = 0; i < words.length; i += 1) {
    const word = words[i];
    if (word.includes("\0")) return "an argument contains NUL";
    if (word === "-D") { if (i + 1 >= words.length) return "-D needs NAME=VALUE"; i += 1; continue; }
    if (word.startsWith("-D")) continue;
    if (["-S", "-B", "-G", "-A"].some((flag) => word.startsWith(flag) && word.length > flag.length)) continue; // -Ssrc, -Bbuild, -GNinja
    const name = word.startsWith("--") && word.includes("=") ? word.slice(0, word.indexOf("=")) : word;
    if (CMAKE_FLAGS.value.includes(name)) { if (name === word) { if (i + 1 >= words.length) return `${word} needs a value`; i += 1; } continue; }
    if (CMAKE_FLAGS.optionalNumber.includes(word)) { if (/^\d+$/.test(words[i + 1] ?? "")) i += 1; continue; }
    return `${word} is not a cmake argument the rightkit-cmake adapter takes (${admissible}); -E, -P, --install, --preset and any positional are refused`;
  }
  return null;
}

const flagNameOf = (word) => (word.startsWith("--") && word.includes("=") ? word.slice(0, word.indexOf("=")) : word);

/**
 * The arguments AFTER the pnpm command token, judged as the broker's `scan` does (adapter.rs): every flag before the opaque tail is a registered flag
 * with the value its kind needs; `--` ends the flags; after `pnpm exec <target>` the rest is the target's own. `pnpm run <script> --flag` is refused
 * because `run` is not opaque after its script: write `pnpm run <script> -- --flag` (M13). Null when the tail is admissible.
 */
export function pnpmTailRefusal(words, grammar = PNPM_ADAPTER_GRAMMAR) {
  const args = words.map(String);
  const first = args[0];
  if (first === undefined) return null;
  let i = first.startsWith("-") && Object.hasOwn(grammar.flags, first) ? 0 : 1;
  let awaiting = grammar.opaqueAfterTarget.includes(first);
  let commandSeen = !first.startsWith("-");
  let afterSeparator = false;
  let command = commandSeen ? first : null;
  while (i < args.length) {
    const word = args[i];
    i += 1;
    if (!afterSeparator && word === "--") { afterSeparator = true; continue; }
    if (afterSeparator || !word.startsWith("-") || word === "-") {
      if (awaiting) return null; // the target: everything after it is its own
      if (!commandSeen) { commandSeen = true; command = word; awaiting = grammar.opaqueAfterTarget.includes(word); }
      continue;
    }
    const name = flagNameOf(word);
    if (!Object.hasOwn(grammar.flags, name)) {
      const hint = command === "run" ? `; to pass it to the script write \`pnpm run <script> -- ${word}\` (run is not opaque after its script, so the broker refuses a bare flag there)` : "";
      return `flag ${name} is not a registered flag of rightkit-pnpm${hint}`;
    }
    const inline = word.startsWith("--") && word.includes("=");
    const kind = grammar.flags[name];
    if (kind === "none") { if (inline) return `flag ${name} takes no value`; continue; }
    if (!inline) { if (i >= args.length) return `flag ${name} needs a value`; i += 1; }
  }
  return awaiting ? "pnpm exec needs a target" : null;
}

/**
 * `cargo ndk` carries no `--locked` of its own: a stale Cargo.lock would be rewritten inside the snapshot and the gate would void the job (M31). The door
 * adds `--locked` right after cargo-ndk's cargo subcommand (`build`, `check`, ...), before any `--`, unless `--locked` or `--frozen` is already there.
 * cargo-ndk's own options come first: `--platform|-p`, `--target|-t` and `--output-dir|-o` take a value.
 */
export function lockedNdkArgs(args) {
  const words = args.map(String);
  if (words[0] !== "ndk") return words;
  const valueOptions = new Set(["-t", "--target", "-p", "--platform", "-o", "--output-dir"]);
  let i = 1;
  for (; i < words.length; i += 1) {
    if (words[i] === "--") return words;
    if (valueOptions.has(words[i])) { i += 1; continue; }
    if (words[i].startsWith("-")) continue;
    break;
  }
  if (i >= words.length) return words;
  const end = words.indexOf("--", i);
  if ((end < 0 ? words : words.slice(0, end)).some((w) => w === "--locked" || w === "--frozen")) return words;
  return [...words.slice(0, i + 1), "--locked", ...words.slice(i + 1)];
}

/** Why an xcodegen argv is not `generate --spec <file> --project <dir>`, or null (B5). */
export function xcodegenRefusal(args) {
  const words = args.map(String);
  if (!XCODEGEN_FLAGS.subcommands.includes(words[0] ?? "")) return `xcodegen takes the subcommand ${XCODEGEN_FLAGS.subcommands.join(", ")} only`;
  for (let i = 1; i < words.length; i += 1) {
    if (!XCODEGEN_FLAGS.value.includes(words[i])) return `${words[i]} is not an xcodegen argument the rightkit-xcodegen adapter takes (admitted: ${XCODEGEN_FLAGS.value.join(" ")})`;
    if (i + 1 >= words.length) return `${words[i]} needs a value`;
    i += 1;
  }
  return null;
}

/** Why an xcodebuild argv is not one the `rightkit-xcodebuild` adapter takes, or null (B5): listed flags and actions, and plain build settings. */
export function xcodebuildRefusal(args) {
  const words = args.map(String);
  if (words.length === 0) return "xcodebuild needs an action";
  let action = false;
  for (let i = 0; i < words.length; i += 1) {
    const word = words[i];
    if (word.includes("\0")) return "an argument contains NUL";
    if (XCODEBUILD_FLAGS.value.includes(word)) { if (i + 1 >= words.length) return `${word} needs a value`; i += 1; continue; }
    if (XCODEBUILD_FLAGS.plain.includes(word) || word.startsWith("-only-testing:") || word.startsWith("-skip-testing:")) continue;
    if (XCODEBUILD_FLAGS.actions.includes(word)) { action = true; continue; }
    const setting = /^([A-Z][A-Z0-9_]*)=/.exec(word);
    if (setting) { if (XCODEBUILD_FLAGS.forbiddenSettings.test(setting[1])) return `build setting ${setting[1]} injects code or moves a tool and is refused`; continue; }
    return `${word} is not an xcodebuild argument the rightkit-xcodebuild adapter takes (flags: ${[...XCODEBUILD_FLAGS.value, ...XCODEBUILD_FLAGS.plain].join(" ")}; actions: ${XCODEBUILD_FLAGS.actions.join(" ")}; NAME=value settings)`;
  }
  return action || words.includes("-exportArchive") ? null : "xcodebuild names no action (build, test, archive, ...)";
}

/** Why a gradlew argv is not one the `rightkit-gradle` adapter takes, or null (B6): task names and the listed flags; no -P/-D, init script, project dir or user home. */
export function gradleRefusal(args) {
  const words = args.map(String);
  if (words.length === 0) return "gradlew needs a task";
  let task = false;
  for (const word of words) {
    if (word.includes("\0")) return "an argument contains NUL";
    if (GRADLE_FLAGS.plain.includes(word) || GRADLE_FLAGS.prefixed.some((p) => word.startsWith(p))) continue;
    if (/^:?[A-Za-z0-9_-]+(?::[A-Za-z0-9_-]+)*$/.test(word)) { task = true; continue; }
    return `${word} is not a gradlew argument the rightkit-gradle adapter takes (task names, and ${[...GRADLE_FLAGS.plain, ...GRADLE_FLAGS.prefixed.map((p) => `${p}<v>`)].join(" ")}); -P, -D, --init-script, --project-dir and --gradle-user-home are refused`;
  }
  return task ? null : "gradlew names no task";
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
  // A snapshot stage's `cargo` and `rustright cargo` steps are carried (carried-cargo.mjs), never sent to the client: one signed, ledgered
  // request per call, from the step's working directory, on every platform (rightkit#1301 F3).
  const carriedArgs = program === "cargo" ? args : program === "rustright" && String(args[0]) === "cargo" ? args.slice(1) : null;
  const carried = carriedArgs ? carriedCargoMapOf(env) : null;
  // A carrier map that cannot be read is a refusal, never a fallback to the uncarried client (rightkit#1301 M1).
  if (carried?.error) refuse(`${name(command)} ${args.join(" ")}: ${carried.error}; a snapshot stage's Cargo step is never sent to the client around its carrier`);
  if (carriedArgs && carried?.map) return { command: process.execPath, args: [CARRIED_RUN, ...lockedNdkArgs(carriedArgs)] };
  const client = () => lookup("rustright")[0] ?? refuse(`rustright is not on PATH: on a broker-managed host ${program} runs only as a broker job`);
  // A snapshot stage's pnpm, node and swift steps are carried adapter jobs (carried-adapter-run.mjs); anywhere else they are the client's `run`.
  // A QA step is not one of them: it runs an already-built artifact (see the rightkit-qa branch below).
  const adapterMap = ["pnpm", "node", "swift", "cmake", ...Object.keys(TOOLCHAIN_ADAPTERS)].includes(program) ? carriedCargoMapOf(env) : null;
  if (adapterMap?.error) refuse(`${name(command)} ${args.join(" ")}: ${adapterMap.error}; a snapshot stage's step is never sent to the client around its carrier`);
  // The step's own environment rides the adapter request (adapter-env.mjs): RUSTRIGHT_ADAPTER_ENV names exactly what the step declares.
  // TAURI_SIGNING_* never rides a broker request (M4): the quiet values (empty key, empty password) only blank what a direct child would inherit, and a
  // broker job's environment already holds none of them; a non-empty one is a signing secret and is refused, never forwarded.
  const adapterStepEnv = () => {
    const declared = host.stepEnv;
    if (!declared || typeof declared !== "object") return declared;
    const kept = {};
    for (const [key, value] of Object.entries(declared)) {
      if (!/^TAURI_SIGNING_/i.test(key)) { kept[key] = value; continue; }
      if (String(value ?? "") !== "") {
        const refusal = new Error(`${name(command)} ${args.join(" ")}: step env ${key} carries a signing secret, and no broker request may (signing belongs to the gate's session, never a broker job)`);
        refusal.code = BUILD_LAUNCH_REFUSED;
        refusal.reason = "RK_ADAPTER_ENV_SECRET";
        throw refusal;
      }
    }
    return kept;
  };
  const stepEnvLaunch = () => {
    try { return adapterEnvLaunch(adapterStepEnv()); } catch (error) {
      if (error.code === BUILD_LAUNCH_REFUSED) throw error;
      const refusal = new Error(`${name(command)} ${args.join(" ")}: ${error.message}`);
      refusal.code = BUILD_LAUNCH_REFUSED;
      refusal.reason = error.code;
      throw refusal;
    }
  };
  const adapterLaunch = (adapter, adapterArgs) => {
    const stepLaunchEnv = stepEnvLaunch();
    const launchEnv = Object.keys(stepLaunchEnv).length ? { env: stepLaunchEnv } : {};
    return adapterMap?.map ? { command: process.execPath, args: [CARRIED_ADAPTER_RUN, adapter, "--", ...adapterArgs], ...launchEnv } : { command: client(), args: ["run", adapter, "--", ...adapterArgs], ...launchEnv };
  };
  if (program === "cargo") return { command: client(), args: ["cargo", ...args.map(String)] };
  if (program === "rustright") {
    const snapshotMap = carriedCargoMapOf(env);
    if (!snapshotMap) return { command: client(), args: args.map(String) };
    if (snapshotMap.error) refuse(`rustright ${args.join(" ")}: ${snapshotMap.error}; a snapshot stage's step is never sent to the client around its carrier`);
    // A snapshot stage: `rustright run <adapter> -- <args>` is that adapter's step (judged by the door's rules for its program, then carried);
    // any other `rustright <command>` is an uncarried client call that no registered adapter takes, so it is refused, naming the step.
    const adapterProgram = args[0] === "run" && args[2] === "--" ? ADAPTER_PROGRAMS[String(args[1])] : undefined;
    if (!adapterProgram) refuse(`rustright ${args.join(" ")}: a snapshot stage carries every compile, and this client call maps to no registered adapter (rustright run rightkit-pnpm|rightkit-node|rightkit-qa|rightkit-swift|rightkit-cmake -- <args>); it is never sent to the client uncarried`);
    return buildLaunch(adapterProgram, args.slice(3), host);
  }
  if (program === "rightkit-qa") {
    // On a snapshot stage QA is a direct, gate-run step against the already-built artifact: no broker job (a QA run needs the GUI session and
    // the unscrubbed environment the broker's job lacks), and nothing in it may compile (the broker refuses an uncarried compile from the snapshot).
    const snapshotMap = carriedCargoMapOf(env);
    if (snapshotMap?.error) refuse(`rightkit-qa ${args.join(" ")}: ${snapshotMap.error}; a snapshot stage's step is never run around its carrier`);
    if (!snapshotMap) return adapterLaunch("rightkit-qa", args.map(String));
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
    const plan = qaSnapshotPlan(args);
    if (plan?.refusal) refuse(`rightkit-qa ${args.join(" ")}: ${plan.refusal}`);
    if (plan) return { command: process.execPath, args: [QA_SNAPSHOT_RUN, qa, "--", ...args.map(String)] };
    return { command: qa, args: args.map(String) };
  }
  if (Object.hasOwn(TOOLCHAIN_ADAPTERS, program)) {
    // B5/B6: every Mac and Dell build is a broker job (GATE-CONTRACT), so Xcode and Gradle are registered, constrained adapters like pnpm and swift:
    // the door admits only the argv the adapter's grammar lists, and a snapshot stage carries the step as a snapshot-bound adapter job.
    const judge = program === "xcodegen" ? xcodegenRefusal : program === "xcodebuild" ? xcodebuildRefusal : gradleRefusal;
    const problem = judge(args);
    if (problem) refuse(`${program} ${args.join(" ")}: ${problem} (rightkit#1241)`);
    return adapterLaunch(TOOLCHAIN_ADAPTERS[program], args.map(String));
  }
  if (program === "cmake") {
    const problem = cmakeRefusal(args);
    if (problem) refuse(`cmake ${args.join(" ")}: ${problem} (rightkit#1241)`);
    return adapterLaunch("rightkit-cmake", args.map(String));
  }
  if (program === "swift") return adapterLaunch("rightkit-swift", args.map(String));
  // This package's own right-release, run by this very Node binary (not any executable named node), with no startup code added by
  // the caller: NODE_OPTIONS is empty or exactly this process's own string (a hook's env could otherwise --import code that builds
  // outside the broker; reordered tokens can re-pair an option with another value, so nothing short of equality is accepted).
  if (command === process.execPath && OWN_SCRIPTS.includes(path.resolve(String(args[0] ?? "")))) {
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
    // The broker's rightkit-pnpm grammar, mirrored (pnpm-adapter-grammar.mjs; the live registration when this host has it): a first argument the
    // adapter does not register (a leading --if-present, --prefix, --config, ...) or a global option it does not register is refused here, naming the step.
    const grammarProblem = pnpmGrammarRefusal(words, (host.pnpmGrammar ?? liveAdapterGrammar({ env, platform, readFile: host.readFile })) ?? PNPM_ADAPTER_GRAMMAR);
    if (grammarProblem) refuse(`pnpm ${words.join(" ")}: ${grammarProblem}; the broker's rightkit-pnpm adapter would refuse it (rightkit#1241)`);
    const tailProblem = pnpmTailRefusal(words, (host.pnpmGrammar ?? liveAdapterGrammar({ env, platform, readFile: host.readFile })) ?? PNPM_ADAPTER_GRAMMAR);
    if (tailProblem) refuse(`pnpm ${words.join(" ")}: ${tailProblem}; the broker's rightkit-pnpm adapter would refuse it (rightkit#1241)`);
    return adapterLaunch("rightkit-pnpm", words);
  }
  return refuse(`${program} ${args.join(" ")}: no broker adapter runs this command yet, and on a broker-managed host a build or test command never runs around the broker (rightkit#1241)`);
}
