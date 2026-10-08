import { mkdtempSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { assertArtifactUnchanged, PublicationError, verifyArtifactFile } from "./publication-artifacts.mjs";

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

// Path-only large-file transport: success requires remote bytes matching captured evidence.
export function uploadVerifiedR2File(file, evidence, key, bucket, { run = spawnSync, env = process.env, cwd, dryRun = false } = {}) {
  if (!["public", "private"].includes(bucket) || !/^[A-Za-z0-9][A-Za-z0-9._/-]*$/.test(key ?? "") || key.includes("..")) throw new PublicationError("PUBLICATION_ROUTE_INVALID", "invalid R2 publication route");
  const artifact = verifyArtifactFile(file, evidence);
  if (dryRun) return artifact;
  const target = `${bucket === "public" ? "rightapps-downloads" : "rightapps-updates"}/${key}`;
  const cleanEnv = { ...env, CLOUDFLARE_ACCOUNT_ID: env.CLOUDFLARE_ACCOUNT_ID || "03ae77ccd7a07bcbb2dcfde47fa7ba3a" };
  for (const name of ["CLOUDFLARE_API_KEY", "CLOUDFLARE_EMAIL", "CF_API_KEY", "CF_EMAIL"]) delete cleanEnv[name];
  const options = { cwd, env: cleanEnv, windowsHide: true, shell: process.platform === "win32", encoding: "utf8" };
  const prefix = ["dlx", "wrangler@4", "r2", "object"];
  const result = run("pnpm", [...prefix, "put", target, "--file", path.resolve(file), "--remote"], { ...options, stdio: "inherit" });
  if (result.error || result.status !== 0) throw new PublicationError("PUBLICATION_UPLOAD_FAILED", `R2 upload failed; inspect remote key ${target}`);
  const temporary = mkdtempSync(path.join(os.tmpdir(), "rightkit-r2-readback-"));
  try {
    const downloaded = path.join(temporary, "object");
    const readback = run("pnpm", [...prefix, "get", target, "--file", downloaded, "--remote"], options);
    if (readback.error || readback.status !== 0) throw new Error("remote readback failed");
    verifyArtifactFile(downloaded, artifact);
  } catch (error) {
    throw new PublicationError("PUBLICATION_REMOTE_MISMATCH", `R2 verification failed (${error.message}); remove remote key ${target}`);
  } finally { rmSync(temporary, { recursive: true, force: true }); }
  return artifact;
}
