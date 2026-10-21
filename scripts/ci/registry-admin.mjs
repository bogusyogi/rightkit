// Owner-authorized registry administration: yank one crates.io version or deprecate one npm
// version, then verify by registry readback. Inputs come from workflow_dispatch env only;
// tokens are read from env and never printed. Exactly one action per run.
import { spawnSync } from "node:child_process";

const ACTIONS = ["yank-cargo", "deprecate-npm"];
const NAME = /^(?:@[a-z0-9][a-z0-9._-]*\/)?[a-z0-9][a-z0-9._-]*$/;
const VERSION = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;

export function adminPlan(env) {
  const { RK_ADMIN_ACTION: action, RK_ADMIN_PACKAGE: name, RK_ADMIN_VERSION: version, RK_ADMIN_MESSAGE: message = "" } = env;
  if (!ACTIONS.includes(action)) throw new Error(`unknown registry admin action: ${action}`);
  if (!NAME.test(name ?? "")) throw new Error(`invalid package name: ${name}`);
  if (!VERSION.test(version ?? "")) throw new Error(`invalid version: ${version}`);
  if (action === "yank-cargo" && name.startsWith("@")) throw new Error("cargo package must not be scoped");
  if (action === "deprecate-npm" && (typeof message !== "string" || message.trim().length < 8 || message.length > 200 || /[\r\n]/.test(message))) throw new Error("deprecation message must be one line, 8-200 chars");
  return { action, name, version, message: action === "deprecate-npm" ? message.trim() : "" };
}

function run(command, args, env) {
  const child = spawnSync(command, args, { env, encoding: "utf8", shell: false, timeout: 120_000 });
  const out = `${child.stdout ?? ""}${child.stderr ?? ""}`.trim();
  if (child.error || child.status !== 0) throw new Error(`${command} ${args[0]} failed (status ${child.status}): ${out.slice(-500)}`);
  return out;
}

async function fetchJson(url) {
  const response = await fetch(url, { headers: { "user-agent": "rightkit-registry-admin", accept: "application/json" } });
  if (!response.ok) throw new Error(`GET ${url} -> ${response.status}`);
  return response.json();
}

export async function verifyCargoYank({ name, version }, fetchImpl = fetchJson) {
  const data = await fetchImpl(`https://crates.io/api/v1/crates/${name}/${version}`);
  if (data?.version?.yanked !== true) throw new Error(`${name}@${version} is not yanked after admin action`);
  return `${name}@${version}: yanked=true (crates.io readback)`;
}

export async function verifyNpmDeprecated({ name, version, message }, fetchImpl = fetchJson) {
  const data = await fetchImpl(`https://registry.npmjs.org/${name.replace("/", "%2f")}/${version}`);
  if (data?.deprecated !== message) throw new Error(`${name}@${version} deprecation readback mismatch: ${JSON.stringify(data?.deprecated)}`);
  return `${name}@${version}: deprecated=${JSON.stringify(message)} (npm readback)`;
}

export async function main(env = process.env) {
  const plan = adminPlan(env);
  if (plan.action === "yank-cargo") {
    if (!env.CARGO_REGISTRY_TOKEN) throw new Error("CARGO_REGISTRY_TOKEN missing");
    run("cargo", ["yank", "--vers", plan.version, plan.name], env);
    console.log(await verifyCargoYank(plan));
  } else {
    if (!env.NODE_AUTH_TOKEN) throw new Error("NODE_AUTH_TOKEN missing");
    run("npm", ["deprecate", `${plan.name}@${plan.version}`, plan.message, "--registry", "https://registry.npmjs.org/"], env);
    console.log(await verifyNpmDeprecated(plan));
  }
}

if (import.meta.url === `file://${process.argv[1]}` || process.argv[1]?.endsWith("registry-admin.mjs")) {
  main().catch((error) => { console.error(`registry-admin: ${error.message}`); process.exitCode = 1; });
}
