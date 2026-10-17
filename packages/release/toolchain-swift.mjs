#!/usr/bin/env node
// `right-release toolchain swift --version <x.y[.z]> --sha256 <pin> [--url <https url>] [--github-env] [--json]`
// (Orthic-Labs/rightkit#436): the one owner of the pinned Swift toolchain install on Windows, replacing the swift.org installer
// block copied into SwiftRight's workflows and release script.
//
//   1. download the swift.org Windows release installer (default URL derived from the version), or reuse a file already on disk;
//   2. refuse unless its sha256 equals the pin (checked before anything runs, and a bad download is deleted);
//   3. skip the install when the pinned version is already the swift.exe on PATH, else run it silently (`/quiet /norestart`;
//      exit 0 and 3010 pass, anything else prints the log tail and fails);
//   4. find swift.exe (the refreshed Machine/User PATH, then the installer's Toolchains folders), require `swift --version` to
//      report the pinned version, and find the Swift runtime bin (`Runtimes\\<ver>\\usr\\bin`, marked by swiftCore.dll);
//   5. with `--github-env` (GITHUB_ENV / GITHUB_PATH set), export what the installer changed: variables it added or changed in
//      the Machine/User environment (SDKROOT and friends; values are never printed, only names) and the new PATH entries,
//      the swift directory and the runtime bin.
//
// Windows only. It never signs, never builds and reads no secret; the sha256 pin is the trust anchor, not the URL.
import { createHash } from "node:crypto";
import nodeFs from "node:fs";
import os from "node:os";
import nodePath from "node:path";
import { pathToFileURL } from "node:url";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { spawnSync as nodeSpawnSync } from "./background-process.mjs";

const win = nodePath.win32;

export class ToolchainError extends Error {
  constructor(code, message, details = {}) { super(message); this.name = "ToolchainError"; this.code = code; this.details = details; }
}
const fail = (code, message, details) => { throw new ToolchainError(code, message, details); };

const VERSION = /^\d+\.\d+(?:\.\d+)?$/;
const SHA256 = /^[0-9A-Fa-f]{64}$/;
/** Variables the installer may legitimately set; these names are never exported to later steps even when they changed. */
const NEVER_EXPORT = /^(?:GITHUB_|RUNNER_|ACTIONS_|NODE_OPTIONS$|BASH_ENV$|ENV$|COMSPEC$|PATHEXT$|TEMP$|TMP$|PATH$)/i;

/** swift.org release installer URL for a version (x86_64 Windows 10+). */
export function swiftInstallerUrl(version) {
  return `https://download.swift.org/swift-${version}-release/windows10/swift-${version}-RELEASE/swift-${version}-RELEASE-windows10.exe`;
}

/** Validate the options; returns the normalized { version, sha256 (lower case), url, githubEnv, json }. */
export function resolveSwiftOptions(raw) {
  const errors = [];
  if (typeof raw.version !== "string" || !VERSION.test(raw.version)) errors.push("--version must be a Swift release such as 6.4.0");
  if (typeof raw.sha256 !== "string" || !SHA256.test(raw.sha256)) errors.push("--sha256 is required: the 64-hex sha256 of the swift.org installer for this version");
  if (raw.url !== undefined && !/^https:\/\/[^\s/]+\/\S*$/.test(raw.url)) errors.push("--url must be an https:// URL");
  if (errors.length) fail("TOOLCHAIN_OPTIONS_INVALID", errors.join("; "));
  return { version: raw.version, sha256: raw.sha256.toLowerCase(), url: raw.url ?? swiftInstallerUrl(raw.version), githubEnv: raw.githubEnv === true, json: raw.json === true };
}

export function parseToolchainArgs(argv) {
  if (argv[0] !== "swift") throw new Error("expected: toolchain swift --version <x.y.z> --sha256 <pin> [--url <https url>] [--github-env] [--json]");
  const opts = {};
  for (let i = 1; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--version" || arg === "--sha256" || arg === "--url") {
      const value = argv[i + 1];
      if (!value || value.startsWith("--")) throw new Error(`${arg} needs a value`);
      opts[arg.slice(2)] = value;
      i += 1;
    } else if (arg === "--github-env") opts.githubEnv = true;
    else if (arg === "--json") opts.json = true;
    else throw new Error(`unknown toolchain argument: ${arg}`);
  }
  return opts;
}

/**
 * What an installer run changed: variables whose Machine or User value is new or different (never PATH, never a denied name,
 * never a name or value GITHUB_ENV cannot carry on one line), and the PATH entries it added. Pure.
 */
export function diffInstallerEnvironment(before, after) {
  const changed = {};
  for (const level of ["Machine", "User"]) {
    for (const [name, value] of Object.entries(after?.[level] ?? {})) {
      if (/^path$/i.test(name) || NEVER_EXPORT.test(name) || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) continue;
      if (typeof value !== "string" || /[\r\n]/.test(value)) continue;
      if (before?.[level]?.[name] !== value) changed[name] = value;
    }
  }
  const entries = (environment) => ["Machine", "User"].flatMap((level) => String(Object.entries(environment?.[level] ?? {}).find(([name]) => /^path$/i.test(name))?.[1] ?? "").split(";")).map((entry) => entry.trim()).filter(Boolean);
  const known = new Set(entries(before).map((entry) => entry.toLowerCase()));
  const addedPath = [...new Set(entries(after))].filter((entry) => !known.has(entry.toLowerCase()));
  return { changed, addedPath };
}

const READ_ENVIRONMENT = "$ErrorActionPreference='Stop'; $r=@{}; foreach($l in 'Machine','User'){ $h=@{}; foreach($e in [Environment]::GetEnvironmentVariables($l).GetEnumerator()){ $h[[string]$e.Key]=[string]$e.Value }; $r[$l]=$h }; $r | ConvertTo-Json -Compress -Depth 4";

/** The Machine and User environment as the registry holds it now ({ Machine: {...}, User: {...} }). */
export function readInstallerEnvironment(run = nodeSpawnSync) {
  const result = run("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", READ_ENVIRONMENT], { encoding: "utf8", windowsHide: true, shell: false });
  if (result?.error || result?.status !== 0) fail("TOOLCHAIN_ENVIRONMENT_UNREADABLE", `cannot read the Machine/User environment (${result?.error?.message ?? `exit ${result?.status}`})`);
  try { return JSON.parse(String(result.stdout)); } catch { return fail("TOOLCHAIN_ENVIRONMENT_UNREADABLE", "the environment listing was not JSON"); }
}

/** Depth-limited search for `name` below `root` (the installer's Toolchains folder). */
function findBelow(fs, root, name, depth = 6) {
  if (depth < 0 || !fs.existsSync(root)) return undefined;
  let entries;
  try { entries = [...fs.readdirSync(root)].sort(); } catch { return undefined; }
  if (entries.includes(name)) return win.join(root, name);
  for (const entry of entries) {
    const child = win.join(root, entry);
    let isDirectory = false;
    try { isDirectory = fs.statSync(child).isDirectory(); } catch { isDirectory = false; }
    if (isDirectory) { const hit = findBelow(fs, child, name, depth - 1); if (hit) return hit; }
  }
  return undefined;
}

async function defaultDownload(url, file, { fs = nodeFs } = {}) {
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok || !response.body) fail("TOOLCHAIN_DOWNLOAD_FAILED", `GET ${url} failed with ${response.status}`, { url });
  if (typeof response.url === "string" && response.url && !response.url.startsWith("https://")) fail("TOOLCHAIN_DOWNLOAD_FAILED", `${url} redirected to a non-https URL`, { url });
  const hash = createHash("sha256");
  const temporary = `${file}.download`;
  const sink = fs.createWriteStream(temporary);
  await pipeline(Readable.fromWeb(response.body), async function* tap(source) { for await (const chunk of source) { hash.update(chunk); yield chunk; } }, sink);
  fs.renameSync(temporary, file);
  return hash.digest("hex");
}

const sha256OfFile = (fs, file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");

/**
 * Install (or confirm) the pinned Swift toolchain. Every effect is injectable: platform, env, fs, run, download(url, file) ->
 * sha256 hex, tmpdir, log. Returns { version, swift, runtimeBin, installed, exported: { variables: [names], path: [entries] } }.
 */
export async function installSwiftToolchain(rawOptions, deps = {}) {
  const options = resolveSwiftOptions(rawOptions);
  const platform = deps.platform ?? process.platform;
  if (platform !== "win32") fail("TOOLCHAIN_HOST_UNSUPPORTED", `right-release toolchain swift installs the Windows toolchain only (host: ${platform}); install Swift ${options.version} with the platform's own tooling`);
  const env = deps.env ?? process.env;
  const fs = deps.fs ?? nodeFs;
  const run = deps.run ?? nodeSpawnSync;
  const log = deps.log ?? (() => {});
  const swiftVersionOk = (swift, pathEnv) => {
    const result = run(swift, ["--version"], { encoding: "utf8", windowsHide: true, shell: false, env: { ...env, Path: pathEnv, PATH: pathEnv } });
    const text = `${result?.stdout ?? ""}${result?.stderr ?? ""}`;
    if (result?.error || result?.status !== 0) return { ok: false, text };
    return { ok: new RegExp(`Swift version ${options.version.replaceAll(".", "\\.")}(?!\\d)`).test(text), text };
  };
  const currentPath = env.Path ?? env.PATH ?? "";
  const onPath = (pathEnv) => pathEnv.split(";").map((dir) => dir.trim()).filter(Boolean).map((dir) => win.join(dir, "swift.exe")).find((candidate) => fs.existsSync(candidate));

  // Already the pinned version: nothing to install. The pin is still the contract for a fresh install below.
  let installed = false;
  let swift = onPath(currentPath);
  let pathEnv = currentPath;
  let before;
  let after;
  if (swift && swiftVersionOk(swift, currentPath).ok) log(`swift ${options.version} already installed at ${swift}`);
  else {
    swift = undefined;
    const temp = deps.tmpdir ?? env.RUNNER_TEMP ?? os.tmpdir();
    const installer = win.join(temp, `swift-${options.version}-RELEASE-windows10.exe`);
    if (!(fs.existsSync(installer) && sha256OfFile(fs, installer) === options.sha256)) {
      log(`downloading ${options.url}`);
      const download = deps.download ?? ((url, file) => defaultDownload(url, file, { fs }));
      const actual = String(await download(options.url, installer)).toLowerCase();
      if (actual !== options.sha256) {
        try { fs.rmSync(installer, { force: true }); } catch { /* best effort */ }
        fail("TOOLCHAIN_HASH_MISMATCH", `Swift installer sha256 ${actual} does not match the pin ${options.sha256}; the download was removed and nothing was installed`, { expected: options.sha256, actual });
      }
    }
    // Hash the bytes that will run, not the bytes that were streamed: a file reused from disk was checked above.
    const onDisk = sha256OfFile(fs, installer);
    if (onDisk !== options.sha256) fail("TOOLCHAIN_HASH_MISMATCH", `Swift installer on disk sha256 ${onDisk} does not match the pin ${options.sha256}`, { expected: options.sha256, actual: onDisk });
    before = readInstallerEnvironment(run);
    const installLog = win.join(temp, "swift-install.log");
    const result = run(installer, ["/quiet", "/norestart", "/log", installLog], { encoding: "utf8", windowsHide: true, shell: false });
    if (result?.error) fail("TOOLCHAIN_INSTALL_FAILED", `could not start the Swift installer: ${result.error.message}`);
    if (result.status !== 0 && result.status !== 3010) {
      let tail = "";
      try { tail = String(fs.readFileSync(installLog)).split(/\r?\n/).slice(-60).join("\n"); } catch { /* no log */ }
      fail("TOOLCHAIN_INSTALL_FAILED", `Swift installer failed with exit code ${result.status}${tail ? `\n${tail}` : ""}`, { exitCode: result.status });
    }
    installed = true;
    after = readInstallerEnvironment(run);
    const refreshed = diffInstallerEnvironment(before, after).addedPath;
    pathEnv = [...new Set([...currentPath.split(";").filter(Boolean), ...["Machine", "User"].flatMap((level) => String(Object.entries(after[level] ?? {}).find(([name]) => /^path$/i.test(name))?.[1] ?? "").split(";").filter(Boolean)), ...refreshed])].join(";");
    swift = onPath(pathEnv);
    if (!swift) {
      for (const root of [win.join(env.LOCALAPPDATA ?? "", "Programs", "Swift"), win.join(env.ProgramFiles ?? "C:\\Program Files", "Swift")]) {
        swift = findBelow(fs, win.join(root, "Toolchains"), "swift.exe");
        if (swift) { pathEnv = `${win.dirname(swift)};${pathEnv}`; break; }
      }
    }
    if (!swift) fail("TOOLCHAIN_SWIFT_NOT_FOUND", "swift.exe was not found after the Swift installer ran");
    const probe = swiftVersionOk(swift, pathEnv);
    if (!probe.ok) fail("TOOLCHAIN_VERSION_MISMATCH", `the installed swift.exe does not report Swift version ${options.version}: ${probe.text.trim().split(/\r?\n/)[0] ?? "no output"}`, { swift });
  }

  // Runtimes\<version>\usr\bin is a sibling of Toolchains under the install root (swiftCore.dll marks it).
  let installRoot;
  for (let cursor = win.dirname(swift); cursor && win.dirname(cursor) !== cursor; cursor = win.dirname(cursor)) {
    if (win.basename(cursor).toLowerCase() === "toolchains") { installRoot = win.dirname(cursor); break; }
  }
  let runtimeBin;
  if (installRoot) {
    const runtimes = win.join(installRoot, "Runtimes");
    let versions = [];
    try { versions = [...fs.readdirSync(runtimes)].sort(); } catch { versions = []; }
    runtimeBin = versions.map((entry) => win.join(runtimes, entry, "usr", "bin")).find((dir) => fs.existsSync(win.join(dir, "swiftCore.dll")));
  }
  if (!runtimeBin) fail("TOOLCHAIN_RUNTIME_NOT_FOUND", `no Swift runtime directory (swiftCore.dll) beside ${swift}${installRoot ? ` under ${win.join(installRoot, "Runtimes")}` : " (swift.exe is not under a Toolchains folder)"}`);

  const exported = { variables: [], path: [] };
  if (options.githubEnv) {
    if (!env.GITHUB_ENV || !env.GITHUB_PATH) fail("TOOLCHAIN_GITHUB_ENV_MISSING", "--github-env needs GITHUB_ENV and GITHUB_PATH (run it inside a GitHub Actions step)");
    const { changed, addedPath } = installed ? diffInstallerEnvironment(before, after) : { changed: {}, addedPath: [] };
    const lines = Object.entries(changed).map(([name, value]) => `${name}=${value}`);
    if (lines.length) fs.appendFileSync(env.GITHUB_ENV, `${lines.join("\n")}\n`);
    exported.variables = Object.keys(changed);
    exported.path = [...new Set([...addedPath, win.dirname(swift), runtimeBin])];
    fs.appendFileSync(env.GITHUB_PATH, `${exported.path.join("\n")}\n`);
  }
  return { version: options.version, swift, runtimeBin, installed, exported };
}

export async function main(argv = process.argv.slice(2)) {
  const opts = parseToolchainArgs(argv);
  const result = await installSwiftToolchain(opts, { log: (line) => console.error(`right-release toolchain: ${line}`) });
  if (opts.json) console.log(JSON.stringify(result, null, 2));
  else {
    console.log(`right-release toolchain: Swift ${result.version} ${result.installed ? "installed" : "already present"}; swift.exe ${result.swift}; runtime ${result.runtimeBin}`);
    if (result.exported.variables.length || result.exported.path.length) console.log(`right-release toolchain: exported variables ${JSON.stringify(result.exported.variables)} and ${result.exported.path.length} PATH entr${result.exported.path.length === 1 ? "y" : "ies"}`);
  }
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().then((code) => { process.exitCode = code; }, (error) => {
    console.error(`right-release toolchain: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  });
}
