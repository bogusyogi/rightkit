#!/usr/bin/env node
// `right-release assemble-app`: the production (and dev-artifact) face of the declarative app assembly (`assembly.mac`,
// `assembly.win`; assembled-app.mjs). The same definition drives `right-release dev` (install-assembled-app.mjs).
//
//   assemble-app --platform mac|win --profile release [--build] [--out <dir>] [--config <file>]
//       lays out the unsigned bundle/payload at assembly.<p>.stagingOutput (the existing sign matrix, notarization and DMG/NSIS
//       steps read it), writes the release binding (receipt hashes) beside it and verifies both. --build first runs
//       assembly.<p>.prepare.release and (mac) xcodegen + xcodebuild of the outer app. --profile dev needs --out.
//   assemble-app --platform mac|win --verify-staged [--signed]
//       read-only structural proof of the staged output (required files, launchd BundleProgram, bundle id, no dev marker,
//       the release binding; --signed also runs `codesign --verify --strict --deep`, and skips the byte re-hash).
//   assemble-app --platform win --package
//       makensis from assembly.win.nsis over the staged payload, then Authenticode-signs the installer (sign-windows).
import { spawnSync } from "./background-process.mjs";
import nodeFs, { existsSync, realpathSync } from "node:fs";
import { BUILD_LAUNCH_REFUSED, buildLaunch } from "./build-launch.mjs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { assembleMacBundle, assembleWindowsPayload, AssembledAppError, loadStageReceipt, resolveAssembly, resolveNestedApp, verifyMacBundle, verifyReleaseBinding, verifyWindowsPayload, writeReleaseBinding } from "./assembled-app.mjs";
import { buildOuterApp, outerProductPath, resolveAssemblyStorage } from "./install-assembled-app.mjs";

const SIGN_WINDOWS = fileURLToPath(new URL("./sign-windows.mjs", import.meta.url));
/** This package's own right-release entry: it comes back through the door for its own children (development.mjs OWN_RIGHT_RELEASE). */
const OWN_RIGHT_RELEASE = fileURLToPath(new URL("./cli/right-release.mjs", import.meta.url));
const refuse = (code, message) => { throw new AssembledAppError(code, message); };

export function parseAssembleArgs(argv) {
  const opts = { platform: undefined, profile: "release", build: false, verifyStaged: false, signed: false, package: false, config: undefined, out: undefined };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const value = () => { const next = argv[i + 1]; if (!next || next.startsWith("--")) throw new Error(`${arg} needs a value`); i += 1; return next; };
    if (arg === "--platform") opts.platform = value();
    else if (arg === "--profile") opts.profile = value();
    else if (arg === "--config") opts.config = value();
    else if (arg === "--out") opts.out = value();
    else if (arg === "--build") opts.build = true;
    else if (arg === "--verify-staged") opts.verifyStaged = true;
    else if (arg === "--signed") opts.signed = true;
    else if (arg === "--package") opts.package = true;
    else throw new Error(`unknown assemble-app argument: ${arg}`);
  }
  if (!["mac", "win"].includes(opts.platform)) throw new Error("assemble-app requires --platform mac|win");
  if (!["dev", "release"].includes(opts.profile)) throw new Error("--profile must be dev or release");
  if (opts.signed && !opts.verifyStaged) throw new Error("--signed only applies to --verify-staged");
  if (opts.package && opts.platform !== "win") throw new Error("--package applies to --platform win (the mac DMG is packaged by the existing dmg step)");
  if (Number(opts.verifyStaged) + Number(opts.package) > 1) throw new Error("--verify-staged and --package are separate steps");
  if (opts.profile === "dev" && !opts.out && !opts.verifyStaged && !opts.package) throw new Error("--profile dev needs --out <dir>: a dev payload never lands in the release staging folder");
  return opts;
}

/**
 * GB-59 (GATE commit/broker entry; BR20): every compile-capable prepare step goes through the one build door (build-launch.mjs), like a dev build step: on a
 * broker host it is a carried or adapter job with the step's declared env (a refusal comes before anything is spawned), anywhere else the plain command.
 * This package's own `right-release` keeps its process order (its children come back through the door). Trusted release orchestration stays session-side.
 */
function runPrepare({ root, spec, env, run, resolveCommand, log, door = buildLaunch, exists = existsSync }) {
  for (const command of spec.prepare.release) {
    const args = (command.args ?? []).map(String);
    const cwd = path.resolve(root, command.cwd ?? ".");
    const ownReleaseStep = (args[0] === "exec" && args[1] === "right-release") || args[0] === "right-release";
    const resolved = command.cmd === "pnpm" && !ownReleaseStep ? { command: "pnpm", args } : resolveCommand(command.cmd, args, { root, env });
    const own = resolved.command === process.execPath && resolved.args[0] === OWN_RIGHT_RELEASE;
    let launch = resolved;
    if (!own) {
      try { launch = { ...resolved, ...door(command.cmd === "pnpm" ? "pnpm" : resolved.command, command.cmd === "pnpm" ? args : resolved.args, { env: resolved.env ?? env, stepEnv: command.env, cwd, exists }) }; }
      catch (error) { if (error?.code === BUILD_LAUNCH_REFUSED) refuse("ASSEMBLY_BUILD_REFUSED", `prepare command refused before it started: ${error.message}`); throw error; }
    }
    log(`assemble-app: prepare ${command.cmd} ${args.join(" ")}`);
    const baseEnv = launch.env && launch.env !== resolved.env ? { ...(resolved.env ?? env), ...launch.env } : (launch.env ?? env);
    const result = run(launch.command, launch.args, { cwd, env: { ...baseEnv, ...(command.env ?? {}) }, stdio: "inherit", windowsHide: true, shell: false, timeout: command.timeoutMs });
    if (result.error || result.status !== 0) refuse("ASSEMBLY_BUILD_FAILED", `prepare command failed (${result.status ?? result.error?.message}): ${command.cmd} ${args.join(" ")}`);
  }
}

/** Where the output goes: --out (relative to the app root) or the declared staging folder. */
const outputOf = ({ root, spec, opts }) => path.resolve(root, opts.out ?? spec.stagingOutput);

export function findMakensis({ env = process.env, fs = nodeFs, run = spawnSync } = {}) {
  const local = env.LOCALAPPDATA ?? "";
  const candidates = [env.RIGHTKIT_MAKENSIS, env.MAKENSIS, "C:\\Program Files (x86)\\NSIS\\makensis.exe", "C:\\Program Files\\NSIS\\makensis.exe", local && path.join(local, "tauri", "NSIS", "makensis.exe")];
  const found = candidates.find((candidate) => candidate && fs.existsSync(candidate));
  if (found) return found;
  const where = run("where", ["makensis"], { encoding: "utf8", windowsHide: true, shell: false });
  if (where.status === 0 && String(where.stdout ?? "").trim()) return String(where.stdout).split(/\r?\n/)[0].trim();
  return refuse("ASSEMBLY_TOOL_MISSING", "makensis (NSIS 3) was not found; install NSIS on the packaging host or set RIGHTKIT_MAKENSIS");
}

/** The makensis argv for assembly.win.nsis: `{payload}`, `{output}` and `{version}` are substituted in every define. */
export function nsisArguments({ root, spec, payload, output, version }) {
  const subst = (text) => text.replaceAll("{payload}", payload).replaceAll("{output}", output).replaceAll("{version}", version);
  return ["/V2", ...Object.entries(spec.nsis.defines).map(([name, value]) => `/D${name}=${subst(value)}`), path.resolve(root, spec.nsis.script)];
}

/** Run one assemble-app invocation. `deps` replaces the process and file-system effects (tests). */
export async function assembleApp({ root, config, opts, env = process.env, deps = {} }) {
  const d = { fs: nodeFs, run: spawnSync, log: (line) => console.log(line), storage: resolveAssemblyStorage, buildOuter: buildOuterApp, ...deps };
  if (!d.resolveCommand) ({ resolveCommand: d.resolveCommand } = await import("./development.mjs"));
  const spec = resolveAssembly(config, opts.platform);
  const out = outputOf({ root, spec, opts });
  const profile = opts.profile;

  if (opts.verifyStaged) {
    if (spec.platform === "mac") {
      if (profile === "release") verifyReleaseBinding({ root, spec, output: out, rehash: !opts.signed, fs: d.fs });
      verifyMacBundle({ root, spec, bundle: out, profile, signed: opts.signed, env, deps: { fs: d.fs, run: d.run } });
    } else {
      if (profile === "release") verifyReleaseBinding({ root, spec, output: out, rehash: false, fs: d.fs });
      verifyWindowsPayload({ spec, payload: out, profile, fs: d.fs });
    }
    d.log(`assemble-app: ${out} verified`);
    return { verified: out };
  }

  if (opts.package) {
    if (!spec.nsis) refuse("ASSEMBLY_CONFIG_INVALID", "assembly.win.nsis is not declared");
    verifyReleaseBinding({ root, spec, output: out, rehash: false, fs: d.fs });
    verifyWindowsPayload({ spec, payload: out, profile: "release", fs: d.fs });
    const output = path.resolve(root, spec.nsis.output);
    d.fs.mkdirSync(path.dirname(output), { recursive: true });
    d.fs.rmSync(output, { force: true });
    const makensis = (d.findMakensis ?? findMakensis)({ env, fs: d.fs, run: d.run });
    const made = d.run(makensis, nsisArguments({ root, spec, payload: out, output, version: String(config.version ?? "") }), { cwd: root, env, stdio: "inherit", windowsHide: true, shell: false });
    if (made.error || made.status !== 0) refuse("ASSEMBLY_BUILD_FAILED", `makensis failed (${made.status ?? made.error?.message})`);
    if (!d.fs.existsSync(output)) refuse("ASSEMBLY_BUILD_FAILED", `makensis did not write ${output}`);
    // The payload exes were signed by the release sign step; the installer is signed here with RightKit's own signer.
    const signed = d.run(process.execPath, [SIGN_WINDOWS, output], { cwd: root, env, stdio: "inherit", windowsHide: true, shell: false });
    if (signed.error || signed.status !== 0) refuse("ASSEMBLY_SIGNING_FAILED", `sign-windows failed on ${output} (${signed.status ?? signed.error?.message})`);
    d.log(`assemble-app: installer ${output}`);
    return { installer: output };
  }

  if (opts.build) runPrepare({ root, spec, env, run: d.run, resolveCommand: d.resolveCommand, log: d.log, ...(d.door ? { door: d.door } : {}), ...(d.exists ? { exists: d.exists } : {}) });
  const receipt = loadStageReceipt({ root, spec, profile, fs: d.fs });
  if (spec.platform === "mac") {
    const storage = d.storage({ root, profile, env, run: d.run });
    if (opts.build) d.buildOuter({ root, spec, storage, profile, env, run: d.run });
    const outerApp = outerProductPath({ spec, storage });
    const nestedApps = spec.nested.map((entry) => resolveNestedApp({ root, entry, spec, profile, deps: { fs: d.fs, ...(d.resolveTargetRoot ? { resolveTargetRoot: d.resolveTargetRoot } : {}) } }));
    const { bound } = assembleMacBundle({ root, spec, profile, out, outerApp, nestedApps, receipt, env, deps: { fs: d.fs, run: d.run, log: d.log, ...(d.commitCount ? { commitCount: d.commitCount } : {}) } });
    if (profile === "release") {
      writeReleaseBinding({ root, spec, bound, fs: d.fs });
      verifyReleaseBinding({ root, spec, output: out, rehash: true, fs: d.fs });
    }
    verifyMacBundle({ root, spec, bundle: out, profile, signed: false, env, deps: { fs: d.fs, run: d.run } });
  } else {
    const { bound } = assembleWindowsPayload({ root, spec, profile, out, receipt, deps: { fs: d.fs, log: d.log } });
    if (profile === "release") {
      writeReleaseBinding({ root, spec, bound, fs: d.fs });
      verifyReleaseBinding({ root, spec, output: out, rehash: true, fs: d.fs });
    }
    verifyWindowsPayload({ spec, payload: out, profile, fs: d.fs });
  }
  d.log(`assemble-app: ${profile} ${spec.platform} output ${out}`);
  return { output: out };
}

export async function main(argv = process.argv.slice(2)) {
  const opts = parseAssembleArgs(argv);
  const { defaultReleaseConfigFile, importReleaseConfig } = await import("./release-config.mjs");
  const configPath = path.resolve(opts.config ?? defaultReleaseConfigFile());
  const config = await importReleaseConfig(configPath, { lenient: true });
  return assembleApp({ root: path.dirname(configPath), config, opts, env: process.env });
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  main().catch((error) => { console.error(`right-release assemble-app: ${error.code ? `[${error.code}] ` : ""}${error.message}`); process.exitCode = 1; });
}
