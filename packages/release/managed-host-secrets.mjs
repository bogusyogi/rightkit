import { managedHost } from "./managed-cargo.mjs";

export const MANAGED_HOST_ENV_SECRET_REFUSED = "MANAGED_HOST_ENV_SECRET_REFUSED";

export class ManagedHostEnvSecretError extends Error {
  constructor(name) {
    super(`${name} is refused on a managed RightKit host; load the secret from the provisioned OS credential store (macOS Keychain or protected token file) instead`);
    this.name = "ManagedHostEnvSecretError";
    this.code = MANAGED_HOST_ENV_SECRET_REFUSED;
    this.variable = name;
  }
}

export function isManagedHost(env = process.env) {
  return managedHost(env);
}

// Throws unconditionally; the message names the variable only, never its value.
export function refuseManagedEnvSecret(name) {
  throw new ManagedHostEnvSecretError(name);
}

// On a managed host, refuse any named environment secret that is set (non-blank).
// Unmanaged hosts are unaffected.
export function refuseManagedEnvSecrets(env = process.env, names = []) {
  if (!isManagedHost(env)) return;
  for (const name of names) {
    if (String(env[name] ?? "").trim()) refuseManagedEnvSecret(name);
  }
}
