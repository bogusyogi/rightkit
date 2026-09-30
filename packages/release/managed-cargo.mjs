import { existsSync } from "node:fs";
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
