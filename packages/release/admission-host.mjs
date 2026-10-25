// The host tier of the admission (admission.mjs): refusals that depend on this machine, not on the committed files. Each row asks
// the same question the build would ask later (a missing tool, a credential provider, a signing identity), through an injectable
// env, spawn and file system, and says NOT RUN with its reason when the probe cannot run on this host (a Windows probe on a Mac).
// NOT RUN is never a pass. Real-credential acceptance runs on the Mac and the Dell; the unit tests use fakes.
import { readFileSync } from "node:fs";
import path from "node:path";
import { brokerManagedHost } from "./cargo-contract.mjs";
import { ManagedHostEnvSecretError, isManagedHost } from "./managed-host-secrets.mjs";
import { resolveMacosDeveloperIdIdentity } from "./macos-signing-identity.mjs";
import { notarytoolAuthArgs } from "./notary-auth.mjs";
import { requiredBuildExecutables } from "./project-type.mjs";
import { resolveReleaseCacheMode } from "./release-cache-mode.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { planMediaSidecar, validateMediaSidecarConfig } from "./media-sidecar.mjs";
import { cargoFfmpegCrate, loadSidecarCatalog } from "./media-sidecar-catalog.mjs";
import { releaseProgressLimits } from "./progress-control.mjs";
import { runConfiguredChecks } from "./release-checks.mjs";
import { MSIX_SIGNING_CONTRACT } from "./msix-config.mjs";

const WINDOWS_PORTABLE_SIGNING_CONTRACT = "windows-raw-exe-authenticode-before-portable-v1";
const WINDOWS_NSIS_SIGNING_CONTRACT = "windows-raw-exe-authenticode-before-nsis-v1";
const UPDATER_ENV = ["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"];
const NOTARY_ENV = ["APPLE_API_KEY_PATH", "APPLE_API_KEY", "APPLE_API_ISSUER"];
class CheckStopped extends Error {}
const asList = (value) => (Array.isArray(value) ? value : []);
const filled = (value) => String(value ?? "").trim() !== "";
const pathFor = (hostPlatform) => (hostPlatform === "win32" ? path.win32 : path.posix);
const tail = (text, lines = 12) => String(text ?? "").trim().split(/\r?\n/).slice(-lines).join("\n");

let bundledFeed;
const feedOf = (ctx) => ctx.feed ?? (bundledFeed ??= JSON.parse(readFileSync(new URL("./rightkit-versions.json", import.meta.url), "utf8")));

/** The first match of `name` on the env's PATH (Windows: with PATHEXT), or null. The probes spawn this path, never a bare name. */
export function resolveExecutable(name, ctx) {
  const join = pathFor(ctx.hostPlatform).join;
  const key = Object.keys(ctx.env).find((candidate) => candidate.toLowerCase() === "path") ?? "PATH";
  const dirs = String(ctx.env[key] ?? "").split(ctx.hostPlatform === "win32" ? ";" : ":").filter(Boolean);
  const extensions = ctx.hostPlatform === "win32" ? ["", ...String(ctx.env.PATHEXT ?? ".COM;.EXE;.BAT;.CMD").split(";").filter(Boolean)] : [""];
  for (const dir of dirs) for (const extension of extensions) {
    const candidate = join(dir, `${name}${extension}`);
    if (ctx.exists(candidate)) return candidate;
  }
  return null;
}

// A version probe that does not answer in this long (a package manager waiting for a download prompt) is a failed probe, not a hang.
// Only the version probes have this cap. The build runs a declared check with no limit of its own (release.mjs runPackageScript);
// the controller's liveness budget for the whole release step is releaseProgressLimits: 4 hours absolute and 10 minutes without
// output, file or CPU progress. The doctor holds a declared check to the absolute limit; it captures the output and cannot see progress.
const PROBE_TIMEOUT_MS = 60_000;
// The build streams a check's output to its console; here it is captured, so the buffer is large enough for any real lint or test run.
const CAPTURE_MAX_BUFFER = 256 * 1024 * 1024;

function probe(ctx, name, args, { cwd = ctx.root, timeout = PROBE_TIMEOUT_MS, maxBuffer } = {}) {
  const file = resolveExecutable(name, ctx);
  if (!file) return { found: false };
  // Windows starts a .cmd or .bat through the shell, which does not quote a path with spaces by itself.
  const shell = ctx.hostPlatform === "win32" && !/\.(?:exe|com)$/i.test(file);
  try {
    const result = ctx.spawn(shell ? `"${file}"` : file, args, { cwd, env: ctx.env, encoding: "utf8", windowsHide: true, shell, ...(timeout ? { timeout } : {}), ...(maxBuffer ? { maxBuffer } : {}) });
    return { found: true, file, status: result.status ?? null, stdout: String(result.stdout ?? ""), stderr: String(result.stderr ?? ""), error: result.error };
  } catch (error) {
    return { found: true, file, status: null, stdout: "", stderr: "", error };
  }
}
const failed = (result) => result.error || result.status !== 0;
const why = (result) => (result.error ? result.error.message : tail(result.stderr || result.stdout, 3));

// A limit in the unit it is meant in: whole minutes as minutes (the 240-minute default), anything else in seconds, so a small
// RIGHT_RELEASE_ABSOLUTE_MS never reads "0 minutes".
function durationText(ms) {
  if (ms >= 60_000 && ms % 60_000 === 0) return ms === 60_000 ? "1 minute" : `${ms / 60_000} minutes`;
  if (ms < 1000) return `${ms} milliseconds`;
  const seconds = Math.round(ms / 1000);
  return seconds === 1 ? "1 second" : `${seconds} seconds`;
}

/**
 * Why a command that ran here did not run to its end, or null. A command stopped by this tool's own buffer or clock did not fail:
 * the build, which streams the output and sets no limit, would have run it on, so it is NOT RUN here and never a finding.
 */
function stoppedByThisTool(result, limitMs) {
  const code = result.error?.code;
  if (code === "ENOBUFS") return `wrote more than ${CAPTURE_MAX_BUFFER / 1024 / 1024} MiB of output, more than this tool captures, and was stopped before it finished`;
  if (code === "ETIMEDOUT") return limitMs ? `ran past ${durationText(limitMs)}, the build's absolute limit for a release step (RIGHT_RELEASE_ABSOLUTE_MS), and was stopped` : "did not finish in the time this tool allows and was stopped";
  return null;
}
// The row outcome for findings and for commands that could not run to their end: findings when there are any (NOT RUN rides along),
// otherwise NOT RUN, otherwise the pass (an empty list).
const verdict = (found, notRun) => (notRun.length === 0 ? found : found.length === 0 ? { notRun: notRun.join("\n") } : { findings: found, notRun: notRun.join("\n") });

function readJson(ctx, file) {
  try { return JSON.parse(ctx.fs.readFileSync(file, "utf8")); } catch { return null; }
}

// The pin the app declares (package.json packageManager), or the feed's when it declares none.
function packageManagerPin(ctx) {
  const declared = readJson(ctx, path.join(ctx.root, "package.json"))?.packageManager;
  return typeof declared === "string" && declared ? { pin: declared, source: "package.json packageManager" } : { pin: feedOf(ctx).packageManager, source: "the RightKit feed (rightkit-versions.json)" };
}

// rust-toolchain(.toml) of the app, or of the nearest directory above it up to the repository: the channel rustup selects.
function rustToolchainChannel(ctx) {
  const join = pathFor(ctx.hostPlatform).join;
  for (let dir = ctx.root; ; dir = path.dirname(dir)) {
    for (const name of ["rust-toolchain.toml", "rust-toolchain"]) {
      const file = join(dir, name);
      if (!ctx.exists(file)) continue;
      const text = ctx.fs.readFileSync(file, "utf8");
      const channel = name.endsWith(".toml") ? /^\s*channel\s*=\s*"([^"]+)"/m.exec(text)?.[1] : text.trim().split(/\r?\n/)[0]?.trim();
      return channel ? { channel, file } : null;
    }
    if (dir === ctx.repoRoot || path.dirname(dir) === dir) return null;
  }
}

/** The refusal for a shared cache without sccache >= 0.15.0: the doctor, the build's admission and the build itself say it alike. */
export function sccacheRefusal(found) {
  return `shared cache mode requires sccache >= 0.15.0${found ? ` (found ${found})` : ""}. Install with: cargo install sccache --locked; then verify: sccache --version`;
}

const signsUpdater = (ctx) => (ctx.platform === "win" || ctx.platform === "mac") && asList(ctx.target?.updater?.artifacts).length > 0 && ctx.target.updater?.kind !== "appinstaller";

// The names sign-windows.mjs reads: each from the process environment first, then from the user's environment (Windows only).
const SIGNING_ENV_NAMES = [
  "AZURE_SIGNTOOL_PATH", "SIGNTOOL_PATH", "LOCALAPPDATA", "ProgramFiles(x86)", "AZURE_CODESIGN_DLIB_PATH", "AZURE_ARTIFACT_SIGNING_DLIB_PATH",
  "AZURE_ARTIFACT_SIGNING_METADATA", "AZURE_SIGNING_METADATA", "AZURE_ARTIFACT_SIGNING_ENDPOINT", "AZURE_SIGNING_ENDPOINT", "AZURE_ENDPOINT",
  "AZURE_ARTIFACT_SIGNING_ACCOUNT", "AZURE_SIGNING_ACCOUNT_NAME", "AZURE_ACCOUNT", "AZURE_ARTIFACT_SIGNING_PROFILE", "AZURE_CERTIFICATE_PROFILE_NAME", "AZURE_PROFILE",
];

function windowsEnv(ctx) {
  let user = null;
  // One PowerShell start answers every name the first time the process environment lacks one.
  const userScope = () => {
    if (user) return user;
    user = new Map();
    const list = SIGNING_ENV_NAMES.map((name) => `'${name.replace(/'/g, "''")}'`).join(",");
    const result = ctx.spawn("powershell", ["-NoProfile", "-Command", `@(${list}) | ForEach-Object { $_ + '=' + [Environment]::GetEnvironmentVariable($_,'User') }`], { encoding: "utf8", windowsHide: true, timeout: PROBE_TIMEOUT_MS });
    if (result.status === 0) for (const line of String(result.stdout ?? "").split(/\r?\n/)) { const at = line.indexOf("="); if (at > 0) user.set(line.slice(0, at), line.slice(at + 1).trim()); }
    return user;
  };
  return (name) => (filled(ctx.env[name]) ? ctx.env[name] : userScope().get(name) ?? "");
}

export const HOST_ROWS = Object.freeze([
  {
    // build-release.mjs assertExecutables / collectToolVersions: the tools the build runs, at the versions it pins.
    id: "host-tools",
    tier: "host",
    run(ctx) {
      const { config, root } = ctx;
      const found = [];
      const packageManager = config.packageManager ?? "pnpm";
      const hasPackageJson = ctx.exists(path.join(root, "package.json"));
      const nativeIos = ctx.nativeIos();
      const required = requiredBuildExecutables({ projectType: ctx.projectType(), packageManager, usesPackageManager: nativeIos ? hasPackageJson : Boolean(config.packageManager) || hasPackageJson, usesCargo: !nativeIos });
      const present = new Set();
      for (const name of required) {
        if (name === "git") continue;
        if (resolveExecutable(name, ctx)) present.add(name);
        else found.push(`required executable not found: ${name}`);
      }
      const engines = /^>=\s*v?(\d+)/.exec(readJson(ctx, path.join(root, "package.json"))?.engines?.node ?? "");
      const nodeMajor = Number((ctx.nodeVersion ?? process.version).replace(/^v/, "").split(".")[0]);
      if (engines && nodeMajor < Number(engines[1])) found.push(`node ${ctx.nodeVersion ?? process.version} is older than the package.json engines.node ${readJson(ctx, path.join(root, "package.json")).engines.node}`);
      if (present.has("pnpm")) {
        const { pin, source } = packageManagerPin(ctx);
        const feedPin = feedOf(ctx).packageManager;
        // The doctor reports the app's pin against the feed with the package rules; the build has no such rule before the worker.
        if (ctx.mode !== "doctor" && pin !== feedPin && source.startsWith("package.json")) found.push(`${config.app ?? "app"} packageManager must be ${feedPin}, got ${pin}`);
        const expected = /^pnpm@(\d+\.\d+\.\d+)/.exec(pin)?.[1];
        const result = probe(ctx, "pnpm", ["--version"]);
        if (failed(result)) found.push(`required command failed: pnpm --version failed: ${why(result)}`);
        else if (expected && result.stdout.trim() !== expected) found.push(`pnpm ${result.stdout.trim()} is installed but ${source} pins pnpm@${expected}; install the pinned version (corepack prepare pnpm@${expected} --activate)`);
      }
      if (present.has("cargo")) {
        const result = probe(ctx, "cargo", ["--version"]);
        if (failed(result)) found.push(`required command failed: cargo --version failed: ${why(result)}`);
      }
      if (present.has("rustc")) {
        const result = probe(ctx, "rustc", ["-vV"]);
        if (failed(result)) found.push(`required command failed: rustc -vV failed: ${why(result)}`);
        else {
          const release = /^release:\s*(\S+)/m.exec(result.stdout)?.[1];
          const pinned = rustToolchainChannel(ctx);
          if (pinned && release && /^\d+\.\d+(?:\.\d+)?$/.test(pinned.channel) && release !== pinned.channel && !release.startsWith(`${pinned.channel}.`)) {
            found.push(`rustc ${release} is installed but ${path.relative(ctx.root, pinned.file).replaceAll("\\", "/") || pinned.file} selects ${pinned.channel}; install it (rustup toolchain install ${pinned.channel})`);
          }
        }
      }
      return found;
    },
  },
  {
    // sign-updater.mjs / notary-auth.mjs / managed-host-secrets.mjs: signing secrets are never taken from the shell environment.
    id: "host-secret-env",
    tier: "host",
    run(ctx) {
      const { env, platform, target } = ctx;
      const managed = isManagedHost(env);
      const found = [];
      const updater = signsUpdater(ctx);
      // notary-auth.mjs reads APPLE_API_* only when no keychain profile is named.
      const notarizes = platform === "mac" && Boolean(target?.notarize) && target.notarize.profile === undefined;
      if (!updater && !notarizes) return { na: "the target signs no updater artifact and names its notary profile" };
      if (updater) {
        for (const name of UPDATER_ENV) {
          for (const key of Object.keys(env).filter((candidate) => candidate.toUpperCase() === name && filled(env[candidate]))) {
            found.push(managed ? new ManagedHostEnvSecretError(name).message : `updater signing key/password must come from the OS credential provider (${key} is set in the environment; unset it)`);
          }
        }
      }
      if (notarizes && managed) for (const name of NOTARY_ENV) if (filled(env[name])) found.push(new ManagedHostEnvSecretError(name).message);
      return found;
    },
  },
  {
    // protected-credentials.mjs execWithUpdaterCredential: the Windows updater key is read by rightkit-credentials, not by this tool.
    id: "host-updater-credential",
    tier: "host",
    run(ctx) {
      if (ctx.platform !== "win" || !signsUpdater(ctx)) return { na: "the target does not sign updater artifacts on Windows" };
      if (ctx.hostPlatform !== "win32") return { notRun: "the updater credential provider is probed on the Windows host that signs" };
      const result = probe(ctx, "rightkit-credentials", ["--version"]);
      if (!result.found || result.error?.code === "ENOENT") return ["OS credential provider unavailable: install rightkit-credentials >=0.2.2 on PATH before updater signing"];
      return undefined;
    },
  },
  {
    // sign-windows.mjs: signtool, the Azure Artifact Signing dlib and the account that signs.
    id: "host-signing-win",
    tier: "host",
    run(ctx) {
      const { platform, target } = ctx;
      const contracts = [WINDOWS_NSIS_SIGNING_CONTRACT, WINDOWS_PORTABLE_SIGNING_CONTRACT, MSIX_SIGNING_CONTRACT];
      if (platform !== "win" || !contracts.includes(target?.signingContract)) return { na: "not a Windows target signed through sign-windows.mjs" };
      if (ctx.hostPlatform !== "win32") return { notRun: "the Windows signing prerequisites (signtool, Azure Artifact Signing) are probed on the Windows host that signs" };
      const env = windowsEnv(ctx);
      const join = path.win32.join;
      const found = [];
      const firstExisting = (candidates) => candidates.find((candidate) => candidate && ctx.exists(candidate));
      const kits = (bin) => {
        if (!ctx.exists(bin)) return [];
        return ctx.fs.readdirSync(bin, { withFileTypes: true }).filter((entry) => entry.isDirectory()).map((entry) => join(bin, entry.name, "x64", "signtool.exe")).sort().reverse();
      };
      const signtool = firstExisting([
        env("AZURE_SIGNTOOL_PATH"), env("SIGNTOOL_PATH"),
        ...kits(join(env("LOCALAPPDATA") || "", "AzureArtifactSigningTools", "Microsoft.Windows.SDK.BuildTools", "bin")),
        ...kits(join(env("ProgramFiles(x86)") || "C:\\Program Files (x86)", "Windows Kits", "10", "bin")),
      ]);
      if (!signtool) found.push("signtool.exe not found; set AZURE_SIGNTOOL_PATH or install Windows SDK Build Tools");
      const dlib = firstExisting([
        env("AZURE_CODESIGN_DLIB_PATH"),
        env("AZURE_ARTIFACT_SIGNING_DLIB_PATH"),
        join(env("LOCALAPPDATA") || "", "AzureArtifactSigningTools", "Microsoft.ArtifactSigning.Client", "bin", "x64", "Azure.CodeSigning.Dlib.dll"),
        "C:\\Program Files\\Microsoft Azure Artifact Signing Client Tools\\x64\\Azure.CodeSigning.Dlib.dll",
        "C:\\Program Files (x86)\\Microsoft\\ArtifactSigningClientTools\\bin\\x64\\Azure.CodeSigning.Dlib.dll",
      ]);
      if (!dlib) found.push("Azure.CodeSigning.Dlib.dll not found; set AZURE_CODESIGN_DLIB_PATH or install Azure Artifact Signing Client Tools");
      const metadata = env("AZURE_ARTIFACT_SIGNING_METADATA") || env("AZURE_SIGNING_METADATA");
      if (metadata) {
        if (!ctx.exists(metadata)) found.push(`metadata file not found: ${metadata}`);
      } else {
        const endpoint = env("AZURE_ARTIFACT_SIGNING_ENDPOINT") || env("AZURE_SIGNING_ENDPOINT") || env("AZURE_ENDPOINT");
        const account = env("AZURE_ARTIFACT_SIGNING_ACCOUNT") || env("AZURE_SIGNING_ACCOUNT_NAME") || env("AZURE_ACCOUNT");
        const profile = env("AZURE_ARTIFACT_SIGNING_PROFILE") || env("AZURE_CERTIFICATE_PROFILE_NAME") || env("AZURE_PROFILE");
        if (!endpoint || !account || !profile) found.push("set AZURE_ARTIFACT_SIGNING_ENDPOINT, AZURE_ARTIFACT_SIGNING_ACCOUNT, and AZURE_ARTIFACT_SIGNING_PROFILE");
      }
      return found;
    },
  },
  {
    // macos-signing-identity.mjs / notary-auth.mjs: one Developer ID identity, and a notary profile the Keychain holds.
    id: "host-signing-mac",
    tier: "host",
    run(ctx) {
      const { platform, target, env } = ctx;
      if (platform !== "mac") return { na: "not a macOS target" };
      const found = [];
      const onMac = ctx.hostPlatform === "darwin";
      if (onMac) { try { resolveMacosDeveloperIdIdentity({ env, run: ctx.spawn }); } catch (error) { found.push(error.message); } }
      let inconclusive = null;
      if (target?.notarize) {
        let args = null;
        // How the profile is chosen needs only the config and the environment, so it is judged on any host. A managed host
        // refusing the APPLE_API_* variables is the host-secret-env finding.
        try { args = notarytoolAuthArgs({ profile: target.notarize.profile, env }); } catch (error) { if (!(error instanceof ManagedHostEnvSecretError)) found.push(error.message); }
        if (onMac && args?.[0] === "--keychain-profile") {
          const profile = args[1];
          const stored = ctx.spawn("security", ["find-generic-password", "-s", `com.apple.gke.notary.tool.saved-creds.${profile}`], { encoding: "utf8", windowsHide: true });
          if (stored.status !== 0) {
            // The Keychain item name is notarytool's own; ask notarytool when it is not found under that name.
            const asked = ctx.spawn("xcrun", ["notarytool", "history", "--keychain-profile", profile, "--output-format", "json"], { encoding: "utf8", windowsHide: true });
            const output = `${asked.stdout ?? ""}\n${asked.stderr ?? ""}`;
            if (asked.status === 0) { /* the profile answers */ }
            else if (/no keychain password item found/i.test(output)) found.push(`notarytool keychain profile ${profile} is not stored in the Keychain; store it once with: xcrun notarytool store-credentials ${profile}`);
            else inconclusive = `notarytool could not be asked about keychain profile ${profile} (${tail(output, 2) || "no answer"}); check it with: xcrun notarytool history --keychain-profile ${profile}`;
          }
        }
      }
      if (found.length > 0) return found;
      if (!onMac) return { notRun: "the Developer ID identity and notary profile are probed on the Mac that signs" };
      return inconclusive ? { notRun: inconclusive } : undefined;
    },
  },
  {
    // build-release.mjs preflight op: target.preflight.env / executables / commands, which the app declares as host preconditions.
    id: "host-preflight-env",
    tier: "host",
    run(ctx) {
      const preflight = ctx.target?.preflight;
      if (!preflight || (!asList(preflight.env).length && !asList(preflight.executables).length && !asList(preflight.commands).length)) return { na: "the target declares no preflight requirements" };
      const found = [];
      const notRun = [];
      for (const name of asList(preflight.executables)) if (!resolveExecutable(name, ctx)) found.push(`required executable not found: ${name}`);
      for (const name of asList(preflight.env)) if (!ctx.env[name]) found.push(`missing required environment variable: ${name}`);
      for (const command of asList(preflight.commands)) {
        // The build runs the command with inherited output and no time limit (build-release.mjs runChecked); the output is captured here.
        const result = ctx.spawn(command.cmd, command.args ?? [], { cwd: path.resolve(ctx.root, command.cwd ?? "."), env: ctx.env, encoding: "utf8", windowsHide: true, shell: false, maxBuffer: CAPTURE_MAX_BUFFER });
        const stopped = stoppedByThisTool(result);
        if (stopped) notRun.push(`preflight command ${command.cmd} ${stopped}`);
        else if (result.error || result.status !== 0) found.push(`${command.cmd} exited ${result.status ?? result.error?.message}`);
      }
      return verdict(found, notRun);
    },
  },
  {
    // build-release.mjs: on mac and win the build defaults to the shared cache, which needs sccache >= 0.15.0; a broker host does not.
    id: "host-cache-mode",
    tier: "host",
    run(ctx) {
      if (ctx.projectType() === "swiftpm" || ctx.nativeIos()) return { na: "SwiftPM and native iOS builds plan their own cache" };
      let mode;
      try { mode = resolveReleaseCacheMode({ requested: ctx.env.RIGHT_RELEASE_CACHE_MODE, platform: ctx.platform, env: ctx.env }); } catch (error) { return [error.message]; }
      if (brokerManagedHost(ctx.env, ctx.hostPlatform, ctx.exists)) return { na: "broker-managed host: the broker owns CARGO_TARGET_DIR and the shared-cache checks are skipped" };
      if (mode !== "shared") return { na: `cache mode ${mode} needs no sccache` };
      const result = probe(ctx, "sccache", ["--version"]);
      const version = /sccache\s+(\d+)\.(\d+)\.(\d+)/i.exec(result.found && !failed(result) ? result.stdout : "");
      if (!version || (Number(version[1]) === 0 && Number(version[2]) < 15)) return [sccacheRefusal(version ? version.slice(1).join(".") : null)];
      return undefined;
    },
  },
  {
    // release.mjs prepareMediaSidecar: with no catalog named in the config, the rightkit-ffmpeg crate the app compiled against is found
    // with `cargo metadata --offline` (the call the worker makes, through the same managed Cargo), and its catalog plans. A catalog
    // the config names, and the missing Cargo manifest, are the media-sidecar-plan files row.
    id: "media-sidecar-catalog",
    tier: "host",
    after: ["build-inputs", "media-sidecar-plan"],
    run(ctx) {
      if (ctx.target.mediaSidecar === undefined || (ctx.platform !== "mac" && ctx.platform !== "win")) return { na: "the target declares no mediaSidecar" };
      let sidecar;
      try { sidecar = validateMediaSidecarConfig(ctx.target.mediaSidecar, { platform: ctx.platform }); } catch { return { skipped: "an invalid mediaSidecar configuration is reported by the media-sidecar rule" }; }
      if (sidecar.catalog) return { na: "the config names the catalog; the media-sidecar-plan row reads it" };
      const inputs = ctx.buildInputs();
      if (!inputs.ok) return { skipped: "build-inputs is reported on its own" };
      const layout = inputs.value.nativeLayout;
      if (!layout) return { skipped: "media-sidecar-plan reports the missing native Cargo manifest" };
      const cargo = (args, options) => runCargoSync(args, { ...options, env: ctx.env }, { platform: ctx.hostPlatform, exists: ctx.exists, spawn: ctx.spawn });
      try {
        const loaded = loadSidecarCatalog({ config: sidecar, root: ctx.root, manifestPath: layout.manifestPath, read: (file) => ctx.fs.readFileSync(file), crateFor: (manifestPath) => cargoFfmpegCrate(manifestPath, { run: cargo }) });
        planMediaSidecar({ config: sidecar, ...loaded, root: ctx.root });
      } catch (error) { return [`${ctx.config?.app ?? "app"} ${ctx.platform} mediaSidecar: [${error.code ?? "ERROR"}] ${error.message}`]; }
      return undefined;
    },
  },
  {
    // release-checks.mjs runs config.checks (lint, typecheck) after the install. The doctor runs them when the dependencies are
    // installed; the build runs them after its own install, so here they are skipped.
    id: "declared-checks-run",
    tier: "host",
    needsTarget: false,
    async run(ctx) {
      const checks = asList(ctx.config.checks).filter((script) => typeof script === "string");
      if (checks.length === 0) return { na: "the config declares no checks" };
      if (ctx.skipChecks) return { na: "the declared checks are skipped" };
      if (ctx.mode !== "doctor") return { skipped: "the build runs the declared checks after its install" };
      const workdir = path.resolve(ctx.root, ctx.config.workdir ?? ".");
      if (!ctx.exists(path.join(workdir, "node_modules"))) return { notRun: `not installed: no node_modules in ${workdir}, so ${checks.join(", ")} cannot run` };
      const packageManager = ctx.config.packageManager ?? "pnpm";
      const { absoluteMs } = releaseProgressLimits(ctx.env);
      const found = [];
      const notRun = [];
      for (const script of checks) {
        try {
          await runConfiguredChecks({
            packageManager, checks: [script], workdir,
            run: async (pm, name, cwd) => {
              // The build runs the script with inherited output and no limit but the release step's (release.mjs runPackageScript): a slow
              // check that passes is green here too, and a check this tool had to stop is not a failed check.
              const result = probe({ ...ctx, root: cwd }, pm, ["run", name], { cwd, timeout: absoluteMs, maxBuffer: CAPTURE_MAX_BUFFER });
              if (!result.found) throw new Error(`required executable not found: ${pm}`);
              const stopped = stoppedByThisTool(result, absoluteMs);
              if (stopped) throw new CheckStopped(`check ${JSON.stringify(name)} ${stopped}; the build runs it to its end, so run it yourself: ${pm} run ${name}`);
              if (failed(result)) throw new Error(`check ${JSON.stringify(name)} failed (${pm} run ${name} exited ${result.status ?? result.error?.message ?? result.signal}):\n${tail(`${result.stdout}\n${result.stderr}`)}`);
            },
          });
        } catch (error) { (error instanceof CheckStopped ? notRun : found).push(error.message); }
      }
      return verdict(found, notRun);
    },
  },
]);
