#!/usr/bin/env node
import * as fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { parseCodesignArgv } from "./codesign-argv.mjs";
import { detectMacSigningSession } from "./macos-signing-session.mjs";
import { requestSigningAgent } from "./signing-agent-client.mjs";
import { defaultSigningRoots, privatePath, signingAgentPaths, SigningAgentError } from "./rightkit-signing-agent.mjs";

export async function runCodesignShim(argv, { cwd = process.cwd(), env = process.env, home = os.userInfo().homedir,
  io = fs, run = spawnSync, detect = detectMacSigningSession, request = requestSigningAgent } = {}) {
  if (!detect({ env, run }).guiRunner) {
    // Installed shell shim uses exec here; this branch also supports API callers.
    const result = run("/usr/bin/codesign", argv, { cwd, env, stdio: "inherit", shell: false, windowsHide: true });
    if (result.error) throw result.error;
    return { status: result.status ?? 1, stdout: "", stderr: "" };
  }
  const paths = signingAgentPaths(home);
  let roots = defaultSigningRoots(home, env.RIGHTKIT_MANAGED_ROOT);
  if (io.existsSync(paths.config)) {
    privatePath(paths.config, 0o600, { io });
    roots = JSON.parse(io.readFileSync(paths.config, "utf8")).roots;
  }
  const { targets } = parseCodesignArgv(argv, cwd);
  const report = await request({ schema: 1, type: "codesign-argv", argv, cwd, targets,
    allowedRoots: roots.filter((root) => io.existsSync(root)), timeoutMs: 180000 }, { paths });
  const result = report.results?.[0];
  if (report.schema !== 1 || report.ok !== true || report.results?.length !== 1
    || !Number.isInteger(result?.status) || result.status < 0 || result.status > 255
    || typeof result.stdout !== "string" || typeof result.stderr !== "string") {
    throw new SigningAgentError("MAC_SIGNING_AGENT_PROTOCOL", "Invalid codesign result");
  }
  return result;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    // Only the managed shell wrapper invokes this entry: session detection was
    // already performed by right-release before selecting its wrapper directory.
    const result = await runCodesignShim(process.argv.slice(2), { detect: () => ({ guiRunner: true }) });
    process.stdout.write(result.stdout);
    process.stderr.write(result.stderr);
    process.exitCode = result.status;
  } catch (error) {
    process.stderr.write(`rightkit codesign: [${error.code ?? "MAC_SIGNING_AGENT_JOB_FAILED"}] ${error.message}\n`);
    process.exitCode = 1;
  }
}
