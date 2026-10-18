import { readFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { isManagedHost, refuseManagedEnvSecrets } from "./managed-host-secrets.mjs";

// Protected credential loaders for release sites that previously read only
// ambient environment variables. The storage pattern matches release-token.mjs:
// macOS Keychain generic passwords (account $USER) and, on Windows, a protected
// token file under %APPDATA%\RightKit for legacy non-updater sites. Updater
// signing uses M11's native OS-store child-exec boundary. Other values return to the caller only;
// they are never logged, and diagnostics name the secret, never its value.
export const PROTECTED_SECRET_MISSING = "PROTECTED_SECRET_MISSING";

// Apple App Store Connect values are not stored here: the key ID and issuer come
// from the release config or environment, and the .p8 is read from its file.
export const PROTECTED_SECRETS = Object.freeze({
  "cloudflare-api-token": { label: "Cloudflare API token", keychain: "rightkit-cloudflare-api-token", file: "cloudflare-api-token.token" },
  "qa-receipt-key": { label: "RightKit QA receipt key", keychain: null, file: "qa-receipt-key.token", darwinFile: true },
  "updater-signing-key": { label: "RightKit updater signing key", keychain: "rightsuite-updater-key", providerExec: true },
});

export class ProtectedSecretMissingError extends Error {
  constructor(name, storeCommand) {
    const { label } = PROTECTED_SECRETS[name];
    super(storeCommand
      ? `${label} is not in the protected credential store; store it once with: ${storeCommand}`
      : `${label} is not in a protected credential store on this platform`);
    this.name = "ProtectedSecretMissingError";
    this.code = PROTECTED_SECRET_MISSING;
    this.secret = name;
    this.storeCommand = storeCommand ?? null;
  }
}

// One-line command the operator runs once per host. macOS prompts for the value
// (-w with no argument); Windows prompts with Read-Host -AsSecureString.
export function protectedSecretStoreCommand(name, platform = process.platform) {
  const spec = PROTECTED_SECRETS[name];
  if (!spec) throw new Error(`unknown protected secret: ${name}`);
  if (platform === "darwin" && spec.darwinFile) return null;
  if (platform === "darwin") return `security add-generic-password -a "$USER" -s ${spec.keychain} -U -w`;
  if (platform === "win32") {
    if (spec.providerExec) return `$rightkitSecure = Read-Host -AsSecureString '${spec.label}'; $rightkitPointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($rightkitSecure); try { [Runtime.InteropServices.Marshal]::PtrToStringBSTR($rightkitPointer) | & rightkit-credentials.exe set --namespace '${spec.keychain}' --account ([Environment]::UserName) --replace } finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($rightkitPointer) }`;
    return `powershell -NoProfile -Command "$s = Read-Host -AsSecureString '${spec.label}'; $d = Join-Path $env:APPDATA 'RightKit'; New-Item -ItemType Directory -Force -Path $d | Out-Null; [IO.File]::WriteAllText((Join-Path $d '${spec.file}'), [Runtime.InteropServices.Marshal]::PtrToStringBSTR([Runtime.InteropServices.Marshal]::SecureStringToBSTR($s)))"`;
  }
  return null;
}

// Reads a generic password without echoing stderr; a missing item is undefined.
function readKeychainSecret({ service, account }) {
  const result = spawnSync("security", ["find-generic-password", "-a", account, "-s", service, "-w"], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "ignore"],
  });
  if (result.error || result.status !== 0) return undefined;
  return result.stdout;
}

function readProtectedFile(file) {
  try {
    return readFileSync(file, "utf8");
  } catch (error) {
    if (error.code === "ENOENT") return undefined;
    throw new Error("protected credential file is unreadable");
  }
}

export const darwinKeyFile = (env, spec) => path.join(env.HOME || os.homedir(), "Library", "Application Support", "RightKit", spec.file);

// The store is injectable so tests can use fakes without touching a real
// Keychain or the Windows profile directory.
export function createCredentialStore({
  env = process.env,
  platform = process.platform,
  readKeychain = readKeychainSecret,
  readFile = readProtectedFile,
} = {}) {
  return {
    platform,
    read(name) {
      if (!PROTECTED_SECRETS[name]) throw new Error(`unknown protected secret: ${name}`);
      const spec = PROTECTED_SECRETS[name];
      // Signing keys stay inside the native provider's child-exec boundary.
      // No JavaScript/file/environment fallback can export this credential.
      if (spec.providerExec) throw new ProtectedSecretMissingError(name, protectedSecretStoreCommand(name, platform));
      let value;
      if (platform === "darwin") {
        // Receipt MAC keys are provisioned by right-release itself as a 0600 file, never from the Keychain.
        value = spec.darwinFile ? readFile(darwinKeyFile(env, spec)) : readKeychain({ service: spec.keychain, account: env.USER || os.userInfo().username });
      } else if (platform === "win32") {
        const appData = env.APPDATA || path.join(os.homedir(), "AppData", "Roaming");
        value = readFile(path.win32.join(appData, "RightKit", spec.file));
      }
      const trimmed = typeof value === "string" ? value.trim() : "";
      if (!trimmed) throw new ProtectedSecretMissingError(name, protectedSecretStoreCommand(name, platform));
      return trimmed;
    },
  };
}

// Managed hosts refuse the named environment variable before any credential is
// read. Unmanaged hosts try the protected store first and use the variable only
// when the store has no value. A missing store entry on a managed host never
// permits the environment fallback.
export function resolveEnvBackedSecret({ name, envName, env = process.env, credentialStore, required = true }) {
  if (PROTECTED_SECRETS[name]?.providerExec) throw new Error("updater signing requires the OS credential provider execution boundary");
  refuseManagedEnvSecrets(env, [envName]);
  if (!required) return undefined;
  const store = credentialStore ?? createCredentialStore({ env });
  try {
    return store.read(name);
  } catch (error) {
    if (!(error instanceof ProtectedSecretMissingError)) throw error;
    const fallback = isManagedHost(env) ? "" : String(env[envName] ?? "").trim();
    if (fallback) return fallback;
    throw error;
  }
}

// M11 owns OS acquisition & secret lifetime. The release adapter supplies only
// non-secret identity plus an absolute native child executable and literal argv.
export function execWithUpdaterCredential(command, args, {
  env = process.env, cwd = process.cwd(), run = spawnSync,
  provider = process.platform === "win32" ? "rightkit-credentials.exe" : "rightkit-credentials",
  account = os.userInfo().username,
} = {}) {
  for (const key of Object.keys(env)) {
    if (["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"].includes(key.toUpperCase()) && env[key]) {
      throw new Error("updater signing key/password must come from the OS credential provider");
    }
  }
  if (!path.isAbsolute(command)) throw new Error("credential provider child must be an absolute native executable");
  const result = run(provider, ["exec", "--namespace", PROTECTED_SECRETS["updater-signing-key"].keychain,
    "--account", account, "--env", "TAURI_SIGNING_PRIVATE_KEY", "--", command, ...args],
  { cwd, env: { ...env, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "" }, stdio: "inherit", shell: false, windowsHide: true });
  if (result.error?.code === "ENOENT") throw new Error("OS credential provider unavailable: install rightkit-credentials >=0.2.2 on PATH before updater signing");
  return result;
}

export function resolveCloudflareApiToken({ env = process.env, credentialStore, required = true } = {}) {
  return resolveEnvBackedSecret({ name: "cloudflare-api-token", envName: "CLOUDFLARE_API_TOKEN", env, credentialStore, required });
}
