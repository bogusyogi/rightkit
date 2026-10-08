#!/usr/bin/env node
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { verifySealedRelease } from "./release-state.mjs";
import { uploadVerifiedR2File } from "./r2-verified-upload.mjs";
import { resolveCloudflareApiToken } from "./protected-credentials.mjs";

export function runLargeUpload(argv, { run = spawnSync, env = process.env, credentialStore } = {}) {
  const [localFile, r2Key, bucketAlias = "public", flag, evidencePath, ...extra] = argv;
  if (!localFile || !r2Key) throw new Error("Usage: upload-large <file> <key> public|private --evidence <json> | --seal <sealed-dir>");
  if (!["public", "private"].includes(bucketAlias)) throw new Error(`invalid bucket alias ${bucketAlias}; expected public|private`);
  if (!["--evidence", "--seal"].includes(flag) || !evidencePath || extra.length) throw new Error("publication seal or hash evidence is required (--seal or --evidence)");
  let evidence;
  if (flag === "--seal") {
    const sealed = verifySealedRelease(path.resolve(evidencePath));
    if (path.dirname(path.resolve(localFile)) !== path.resolve(sealed.sealedDir)) throw new Error("artifact is outside sealed directory");
    evidence = sealed.manifest.files.find((entry) => entry.name === path.basename(localFile));
  } else {
    const value = JSON.parse(readFileSync(evidencePath, "utf8"));
    evidence = { sha256: value.artifactSha256, sizeBytes: value.sizeBytes };
  }
  // Validate before runner discovery or any remote operation, including dry runs.
  uploadVerifiedR2File(localFile, evidence, r2Key, bucketAlias, { dryRun: true });
  // The Cloudflare token is resolved only for a real upload; it is injected into the Wrangler child environment alone.
  const dryRun = env.RIGHT_RELEASE_UPLOAD_DRY_RUN === "1";
  const token = resolveCloudflareApiToken({ env, credentialStore, required: !dryRun });
  const uploadEnv = dryRun ? env : { ...env, CLOUDFLARE_API_TOKEN: token };
  const probe = run("pnpm", ["--version"], { env, stdio: "ignore", shell: process.platform === "win32", windowsHide: true });
  if (probe.error || probe.status !== 0) throw new Error("no package runner found: pnpm dlx");
  const object = `${bucketAlias === "public" ? "rightapps-downloads" : "rightapps-updates"}/${r2Key}`;
  console.log(`wrangler r2 object put ${object} --remote (mandatory hash readback)`);
  console.log("runner: pnpm dlx");
  return uploadVerifiedR2File(localFile, evidence, r2Key, bucketAlias, { run, env: uploadEnv, dryRun });
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { runLargeUpload(process.argv.slice(2)); }
  catch (error) { console.error(`upload-large: ${error.message}`); process.exitCode = 1; }
}
