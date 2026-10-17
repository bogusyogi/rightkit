#!/usr/bin/env node
// right-release engine-locate: the verified route to the CutRight and GenRight engines (Orthic-Labs/rightkit#316).
// Callers such as PostRight get the engine path ONLY from here. The route is fixed per platform (the installed app's stable
// `current` folder, see in-place-install.mjs) and every candidate is verified before its path is returned: a regular file (no
// symlink or junction), signed by the Orthic Labs release identity (macOS: Developer ID, hardened runtime, pinned Team ID and
// Identifier, not ad hoc; Windows: valid Authenticode, pinned signer organisation), and, when the caller pins one, an exact sha256.
// There is no environment override, no path argument and no PATH search: a path the user or a build supplies is never trusted.
import { createHash } from "node:crypto";
import { existsSync, lstatSync, readFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { assertPackagedIdentity, parseCodesignDisplay } from "./packaged-app-identity.mjs";

export const ENGINE_TEAM_IDENTIFIER = "6KLGD3LLKF";
export const ENGINE_SIGNER_ORGANIZATION = "Orthic Labs";

/** engine id -> installed app folder, executable name (no extension) and macOS codesign Identifier. */
export const ENGINES = Object.freeze({
  cutright: Object.freeze({ app: "CutRight", exe: "cutright", identifier: "com.orthiclabs.cutright.engine" }),
  genright: Object.freeze({ app: "GenRight", exe: "genright", identifier: "com.orthiclabs.genright.engine" }),
});

export class EngineLocateError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "EngineLocateError";
    this.code = code;
  }
}

const fail = (code, message) => { throw new EngineLocateError(code, message); };

/** The single fixed path for an engine on a platform. Reads no override variable: `localAppData` / `home` only name the user's profile roots. */
export function engineRoute(engine, { platform = process.platform, home = os.homedir(), localAppData = process.env.LOCALAPPDATA } = {}) {
  const spec = ENGINES[engine];
  if (!spec) fail("ENGINE_UNKNOWN", `unknown engine ${JSON.stringify(engine)} (cutright or genright)`);
  if (platform === "win32") {
    if (!localAppData) fail("ENGINE_ROUTE_UNAVAILABLE", "LOCALAPPDATA is not set, so the Windows engine route cannot be formed");
    return path.win32.join(localAppData, "Orthic Labs", spec.app, "current", "bin", `${spec.exe}.exe`);
  }
  if (platform === "darwin") return path.posix.join(home, "Library", "Application Support", "Orthic Labs", spec.app, "current", "bin", spec.exe);
  return fail("ENGINE_PLATFORM_UNSUPPORTED", `no engine route for platform ${platform}`);
}

const capture = (command, args, run = spawnSync) => {
  const result = run(command, args, { encoding: "utf8", windowsHide: true, shell: false });
  return { status: result.status ?? 1, output: `${result.stdout ?? ""}\n${result.stderr ?? ""}` };
};

const defaultVerifyMac = (file, spec, run) => {
  const strict = capture("codesign", ["--verify", "--strict", file], run);
  if (strict.status !== 0) fail("ENGINE_SIGNATURE_INVALID", `codesign --verify --strict failed for ${file}: ${strict.output.trim()}`);
  const display = capture("codesign", ["-dv", "--verbose=4", file], run);
  if (display.status !== 0) fail("ENGINE_SIGNATURE_INVALID", `codesign could not read ${file}: ${display.output.trim()}`);
  const failures = assertPackagedIdentity(parseCodesignDisplay(display.output), [], { identifier: spec.identifier, team: ENGINE_TEAM_IDENTIFIER });
  if (failures.length) fail("ENGINE_IDENTITY_MISMATCH", `${file}: ${failures.join("; ")}`);
};

const defaultVerifyWindows = (file, _spec, run) => {
  const script = "$s = Get-AuthenticodeSignature -LiteralPath $args[0]; [pscustomobject]@{ status = [string]$s.Status; subject = if ($s.SignerCertificate) { $s.SignerCertificate.Subject } else { '' } } | ConvertTo-Json -Compress";
  const result = capture("powershell", ["-NoProfile", "-NonInteractive", "-Command", script, file], run);
  let parsed;
  try { parsed = JSON.parse(result.output.trim().split(/\r?\n/).filter(Boolean).at(-1)); } catch { fail("ENGINE_SIGNATURE_INVALID", `Authenticode could not be read for ${file}: ${result.output.trim()}`); }
  if (parsed.status !== "Valid") fail("ENGINE_SIGNATURE_INVALID", `${file}: Authenticode status is ${parsed.status}, not Valid`);
  if (!new RegExp(`(^|,\\s*)O=${ENGINE_SIGNER_ORGANIZATION}(,|$)`).test(parsed.subject)) fail("ENGINE_IDENTITY_MISMATCH", `${file}: signer ${JSON.stringify(parsed.subject)} is not ${ENGINE_SIGNER_ORGANIZATION}`);
};

/**
 * Locate and verify an engine. Returns { engine, path, sha256, platform }. Throws EngineLocateError (ENGINE_NOT_INSTALLED,
 * ENGINE_NOT_REGULAR_FILE, ENGINE_SIGNATURE_INVALID, ENGINE_IDENTITY_MISMATCH, ENGINE_HASH_MISMATCH, ...). Injectable for tests:
 * `route`, `verifySignature`, `lstat`, `readBytes`. `expectedSha256` is an optional exact pin from the caller's own release record.
 */
export function locateEngine(engine, {
  platform = process.platform, home, localAppData, expectedSha256, run = spawnSync,
  route = engineRoute(engine, { platform, ...(home ? { home } : {}), ...(localAppData ? { localAppData } : {}) }),
  verifySignature, lstat = lstatSync, readBytes = readFileSync, exists = existsSync,
} = {}) {
  const spec = ENGINES[engine];
  if (!spec) fail("ENGINE_UNKNOWN", `unknown engine ${JSON.stringify(engine)} (cutright or genright)`);
  if (!exists(route)) fail("ENGINE_NOT_INSTALLED", `${spec.app} engine is not installed at ${route}; install ${spec.app} from RightApps (there is no path override)`);
  const info = lstat(route);
  if (info.isSymbolicLink() || !info.isFile()) fail("ENGINE_NOT_REGULAR_FILE", `${route} must be a regular file, not a symlink, junction or directory`);
  const verify = verifySignature ?? (platform === "win32" ? defaultVerifyWindows : platform === "darwin" ? defaultVerifyMac : () => fail("ENGINE_PLATFORM_UNSUPPORTED", `no engine verification for platform ${platform}`));
  verify(route, spec, run);
  const sha256 = createHash("sha256").update(readBytes(route)).digest("hex");
  if (expectedSha256 !== undefined) {
    if (!/^[0-9a-f]{64}$/.test(expectedSha256)) fail("ENGINE_HASH_MISMATCH", `expectedSha256 must be a lowercase SHA-256: ${JSON.stringify(expectedSha256)}`);
    if (sha256 !== expectedSha256) fail("ENGINE_HASH_MISMATCH", `${route}: sha256 ${sha256} is not the pinned ${expectedSha256}`);
  }
  return { engine, path: route, sha256, platform };
}

export function main(args = process.argv.slice(2), options = {}) {
  try {
    const known = new Set(["--engine", "--sha256", "--json"]);
    for (const arg of args) if (arg.startsWith("--") && !known.has(arg)) fail("ENGINE_USAGE", `unknown option ${arg} (there is deliberately no path option)`);
    const take = (flag) => {
      const index = args.indexOf(flag);
      if (index < 0) return undefined;
      const value = args[index + 1];
      if (!value || value.startsWith("--")) fail("ENGINE_USAGE", `${flag} requires a value`);
      return value;
    };
    const engine = take("--engine");
    if (!engine) fail("ENGINE_USAGE", "usage: right-release engine-locate --engine cutright|genright [--sha256 <hex>] [--json]");
    const result = locateEngine(engine, { expectedSha256: take("--sha256"), ...options });
    console.log(args.includes("--json") ? JSON.stringify(result) : result.path);
  } catch (error) {
    console.error(`right-release engine-locate: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) main();
