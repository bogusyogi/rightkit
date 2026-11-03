#!/usr/bin/env node
// `right-release nsis-pack --script <installer.nsi> --payload <dir> --output <Setup.exe> [--define NAME=VALUE]...` (rightkit#428, #871).
//
// The packing step of the Windows payload contract (`signingContract: "windows-payload-authenticode-before-nsis-v1"`): an app that is NOT a
// Tauri bundle (Pulse: eight native executables and its own `.nsi`) signs every payload file, then calls this to compile the installer from the
// signed bytes. It runs `makensis` with no shell and no network, passes the payload directory and the output file as `PAYLOAD_DIR` and `OUTFILE`
// defines (the script reads `${PAYLOAD_DIR}` and writes `OutFile "${OUTFILE}"`), adds each `--define`, and then refuses unless the installer
// exists at `--output` and is newer than the script. Signing and the embedded-bytes proof stay in release.mjs: this step never signs.
import { existsSync, mkdirSync, rmSync, statSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";

export class NsisPackError extends Error {
  constructor(message) { super(message); this.name = "NsisPackError"; this.code = "RK_NSIS_PACK"; }
}
const fail = (message) => { throw new NsisPackError(message); };
const DEFINE_NAME = /^[A-Z][A-Z0-9_]{0,63}$/;
const RESERVED = new Set(["PAYLOAD_DIR", "OUTFILE"]);

const MAKENSIS_CANDIDATES = [
  "C:\\Program Files (x86)\\NSIS\\makensis.exe",
  "C:\\Program Files\\NSIS\\makensis.exe",
  "/opt/homebrew/bin/makensis",
  "/usr/local/bin/makensis",
  "/usr/bin/makensis",
];

/** `RIGHT_RELEASE_MAKENSIS` (must exist), else the first known install path, else `makensis` on PATH. */
export function resolveMakensis({ env = process.env, exists = existsSync } = {}) {
  const explicit = env.RIGHT_RELEASE_MAKENSIS;
  if (explicit) {
    if (!exists(explicit)) fail(`RIGHT_RELEASE_MAKENSIS does not exist: ${explicit}`);
    return explicit;
  }
  return MAKENSIS_CANDIDATES.find((candidate) => exists(candidate)) ?? "makensis";
}

export function parseNsisPackArgs(argv) {
  const opts = { script: null, payload: null, output: null, defines: [] };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    const value = argv[i + 1];
    if (!["--script", "--payload", "--output", "--define"].includes(flag)) fail(`unknown argument: ${flag}`);
    if (value === undefined || value.startsWith("--")) fail(`${flag} needs a value`);
    i += 1;
    if (flag === "--define") {
      const at = value.indexOf("=");
      const name = at < 0 ? value : value.slice(0, at);
      const text = at < 0 ? "1" : value.slice(at + 1);
      if (!DEFINE_NAME.test(name)) fail(`--define name must be UPPER_SNAKE_CASE: ${name}`);
      if (RESERVED.has(name)) fail(`--define ${name} is set by nsis-pack itself`);
      if (/[\r\n\0]/.test(text)) fail(`--define ${name} value must be one line`);
      opts.defines.push([name, text]);
    } else opts[flag.slice(2)] = value;
  }
  for (const key of ["script", "payload", "output"]) if (!opts[key]) fail(`--${key} is required`);
  return opts;
}

/** Compile the installer. Injectable effects: `run`, `exists`, `stat`, `remove`, `makeDir`, `log`, `env`. */
export function packNsisPayload(opts, { run = spawnSync, exists = existsSync, stat = statSync, remove = rmSync, makeDir = mkdirSync, log = (line) => console.log(line), env = process.env, cwd = process.cwd() } = {}) {
  const script = path.resolve(cwd, opts.script);
  const payload = path.resolve(cwd, opts.payload);
  const output = path.resolve(cwd, opts.output);
  if (!/\.nsi$/i.test(script) || !exists(script)) fail(`NSIS script not found: ${script}`);
  if (!exists(payload) || !stat(payload).isDirectory()) fail(`payload directory not found: ${payload}`);
  if (!/\.exe$/i.test(output)) fail(`output must be an .exe installer path: ${output}`);
  const inside = path.relative(payload, output);
  if (inside === "" || (!inside.startsWith("..") && !path.isAbsolute(inside))) fail("output must not be inside the payload directory (the installer would wrap itself)");
  makeDir(path.dirname(output), { recursive: true });
  remove(output, { force: true }); // a stale installer is never an acceptable result
  const args = ["/V2", `/DPAYLOAD_DIR=${payload}`, `/DOUTFILE=${output}`, ...opts.defines.map(([name, value]) => `/D${name}=${value}`), script];
  const makensis = resolveMakensis({ env, exists });
  log(`right-release nsis-pack: ${path.basename(makensis)} ${path.basename(script)} -> ${output}`);
  const result = run(makensis, args, { cwd: path.dirname(script), stdio: "inherit", shell: false, windowsHide: true, env });
  if (result?.error) fail(`could not start makensis (${result.error.message}); install NSIS or set RIGHT_RELEASE_MAKENSIS`);
  if (result?.status !== 0) fail(`makensis exited ${result?.status ?? "without a status"}`);
  if (!exists(output)) fail(`makensis finished but wrote no installer at ${output}; the script must use OutFile "\${OUTFILE}"`);
  if (stat(output).mtimeMs < stat(script).mtimeMs) fail(`the installer at ${output} is older than its script: it is not this run's output`);
  return { script, payload, output, sizeBytes: stat(output).size };
}

export function main(argv = process.argv.slice(2)) {
  try {
    const result = packNsisPayload(parseNsisPackArgs(argv));
    console.log(`right-release nsis-pack: wrote ${result.output} (${result.sizeBytes} bytes); sign it with the installer signing step`);
    return 0;
  } catch (error) {
    console.error(`right-release nsis-pack: ${error.message}`);
    return error instanceof NsisPackError ? 1 : 2;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) process.exitCode = main();
