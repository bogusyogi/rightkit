import path from "node:path";
import { managedHost } from "../managed-cargo.mjs";

const AGENT_BIN = /(?:\.rightkit-managed|rightkitmanagedagent|rightkit[/\\]build-control)[/\\]agent-bin[/\\]?$/i;

// A child env that is explicitly NOT a managed RightKit host, whatever host runs the suite:
// no broker socket and no agent-bin PATH entries. Tests that spawn production scripts use this so the
// managed-host secret refusal (which stays fully in force in production) never depends on the real host.
export function unmanagedEnv(base = process.env, extra = {}) {
  const env = { ...base };
  delete env.RIGHTKIT_BUILD_BROKER_SOCKET;
  for (const key of Object.keys(env)) {
    if (key.toUpperCase() !== "PATH") continue;
    env[key] = String(env[key] ?? "").split(path.delimiter).filter((entry) => !AGENT_BIN.test(entry)).join(path.delimiter);
  }
  Object.assign(env, extra);
  if (managedHost(env)) throw new Error("unmanagedEnv: env still looks like a managed host");
  return env;
}
