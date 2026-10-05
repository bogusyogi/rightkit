import { randomUUID } from "node:crypto";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";

export function managedRuntimeClient(env = process.env, platform = process.platform, exists = existsSync) {
  const roots = [env.RIGHTKIT_MANAGED_ROOT, env.RIGHTKIT_EXTERNAL_ROOT,
    platform === "win32" ? "D:\\.rightkit-managed" : platform === "darwin" ? "/Volumes/D/.rightkit-managed" : undefined]
    .filter((root) => typeof root === "string" && root.length > 0);
  for (const root of roots) {
    const client = path.join(root, "runtime-current", "build-control-clients", "src", "rightkit.mjs");
    if (exists(client)) return client;
  }
  return undefined;
}

/** Hold a client-run lease that blocks broker runtime cutover until release() (see
 * proveDevelopmentRunsDrained). No managed runtime → no lease; lease I/O failure warns and proceeds. */
export function acquireManagedRunLease({ command, env = process.env, platform = process.platform, exists = existsSync, pid = process.pid } = {}) {
  const client = managedRuntimeClient(env, platform, exists);
  if (!client) return { path: null, release() {} };
  const runs = path.join(path.dirname(path.dirname(path.dirname(path.dirname(client)))), "rightkit-build-control", "global", "runs");
  const file = path.join(runs, `${pid}-${randomUUID()}.json`);
  try {
    mkdirSync(runs, { recursive: true });
    writeFileSync(file, `${JSON.stringify({ pid, command, startedAt: new Date().toISOString() })}\n`, { flag: "wx" });
  } catch (error) {
    console.warn(`right-release: could not record managed run lease (${error.message}); runtime cutover is not blocked for this run`);
    return { path: null, release() {} };
  }
  return { path: file, release() { rmSync(file, { force: true }); } };
}

function managedHost(env) {
  return Boolean(env.RIGHTKIT_BUILD_BROKER_SOCKET)
    || /(?:^|[;:])[^;:]*[/\\](?:\.rightkit-managed|rightkitmanagedagent)[/\\]agent-bin(?:[;:]|$)/i.test(String(env.PATH ?? ""));
}

export function runCargoSync(args, options = {}) {
  const env = options.env ?? process.env;
  const client = managedRuntimeClient(env);
  if (client) return spawnSync(process.execPath, [client, "cargo", ...args], { ...options, cwd: process.cwd() });
  if (managedHost(env) && process.platform === "win32") {
    return { status: null, stdout: "", stderr: "", error: new Error("installed RightKit runtime client is unavailable for managed Cargo metadata") };
  }
  return spawnSync("cargo", args, options);
}
