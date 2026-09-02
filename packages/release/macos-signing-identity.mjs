import { spawnSync } from "node:child_process";

export function resolveMacosDeveloperIdIdentity({ env = process.env, run = spawnSync } = {}) {
  const configured = env.APPLE_DEVELOPER_ID?.trim();
  if (configured) return configured;
  const result = run("security", ["find-identity", "-v", "-p", "codesigning"], { encoding: "utf8" });
  if (result.status !== 0) throw new Error("could not inspect macOS signing identities in Keychain");
  const identities = [...String(result.stdout ?? "").matchAll(/"([^"\n]*Developer ID Application:[^"]+)"/g)]
    .map((match) => match[1]);
  if (identities.length === 1) return identities[0];
  if (!identities.length) throw new Error("no Developer ID Application signing identity found in Keychain");
  throw new Error(`multiple Developer ID Application signing identities found in Keychain; set APPLE_DEVELOPER_ID explicitly (${identities.join(", ")})`);
}
