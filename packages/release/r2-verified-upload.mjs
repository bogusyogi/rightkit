import { spawnSync } from "./background-process.mjs";
import { assertArtifactUnchanged, PublicationError } from "./publication-artifacts.mjs";

// Wrangler's pipe input transports the verified buffer, with no path lookup by its child.
export function uploadVerifiedR2Artifact(artifact, key, bucket, { run = spawnSync, env = process.env, cwd } = {}) {
  if (!["public", "private"].includes(bucket) || !/^[A-Za-z0-9][A-Za-z0-9._/-]*$/.test(key ?? "") || key.includes("..")) throw new PublicationError("PUBLICATION_ROUTE_INVALID", "invalid R2 publication route");
  const input = assertArtifactUnchanged(artifact);
  const cleanEnv = { ...env, CLOUDFLARE_ACCOUNT_ID: env.CLOUDFLARE_ACCOUNT_ID || "03ae77ccd7a07bcbb2dcfde47fa7ba3a" };
  for (const name of ["CLOUDFLARE_API_KEY", "CLOUDFLARE_EMAIL", "CF_API_KEY", "CF_EMAIL"]) delete cleanEnv[name];
  const result = run("pnpm", ["dlx", "wrangler@4", "r2", "object", "put", `${bucket === "public" ? "rightapps-downloads" : "rightapps-updates"}/${key}`, "--pipe", "--remote"], {
    cwd, env: cleanEnv, input, stdio: ["pipe", "inherit", "inherit"], windowsHide: true, shell: process.platform === "win32",
  });
  if (result.error || result.status !== 0) throw new PublicationError("PUBLICATION_UPLOAD_FAILED", `R2 upload failed: ${key}`);
}
