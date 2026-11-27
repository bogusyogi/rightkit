import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { assertArtifactUnchanged, PublicationError, readVerifiedArtifact, verifyArtifactFile } from "./publication-artifacts.mjs";
import { deriveR2S3Credentials, putObject, R2_ACCOUNT_ID, R2_MULTIPART_THRESHOLD_BYTES, streamObjectDigest, uploadMultipart } from "./r2-s3-multipart.mjs";

const validRoute = (key, bucket) => ["public", "private"].includes(bucket) && /^[A-Za-z0-9][A-Za-z0-9._/-]*$/.test(key ?? "") && !key.includes("..");

// One in-process transport for every object (rightkit#1754): no child process, so no credential is carried to a broker job (BROKER-CONTRACT D4)
// and nothing is downloaded at publish time. `bytes` (already verified) go up in one PutObject; without them the file goes up as a multipart
// upload. Either way the remote bytes are read back through a sha256 and a byte counter and must equal the evidence.
async function inProcessUpload({ file, bytes, artifact, key, bucket, env, fetchImpl, sleep }) {
  const bucketName = bucket === "public" ? "rightapps-downloads" : "rightapps-updates";
  const target = `${bucketName}/${key}`;
  const cleanEnv = { ...env, CLOUDFLARE_ACCOUNT_ID: env.CLOUDFLARE_ACCOUNT_ID || R2_ACCOUNT_ID };
  for (const name of ["CLOUDFLARE_API_KEY", "CLOUDFLARE_EMAIL", "CF_API_KEY", "CF_EMAIL"]) delete cleanEnv[name];
  const credentials = await deriveR2S3Credentials(cleanEnv.CLOUDFLARE_API_TOKEN, { account: cleanEnv.CLOUDFLARE_ACCOUNT_ID, fetchImpl });
  const transport = { bucket: bucketName, key, credentials, account: cleanEnv.CLOUDFLARE_ACCOUNT_ID, fetchImpl, sleep };
  try {
    if (bytes) await putObject({ body: bytes, evidence: artifact, ...transport });
    else await uploadMultipart({ file: path.resolve(file), evidence: artifact, ...transport });
  } catch (error) {
    if (error instanceof PublicationError && error.code !== "PUBLICATION_UPLOAD_FAILED") throw error;
    throw new PublicationError("PUBLICATION_UPLOAD_FAILED", `R2 upload failed (${error.message}); inspect remote key ${target}`);
  }
  try {
    const remote = await streamObjectDigest(transport);
    if (remote.sha256 !== artifact.sha256) throw new Error("remote sha256 differs from the evidence");
    if (remote.sizeBytes !== artifact.sizeBytes) throw new Error(`remote size ${remote.sizeBytes} differs from ${artifact.sizeBytes}`);
  } catch (error) {
    throw new PublicationError("PUBLICATION_REMOTE_MISMATCH", `R2 verification failed (${error.message}); remove remote key ${target}`);
  }
}

/**
 * The publication path's upload of one verified artifact (publish-update): the bytes read and hashed at verification go up in process and are
 * read back. An artifact at or above R2_MULTIPART_THRESHOLD_BYTES goes up from its file as a multipart upload after the file is checked unchanged.
 */
export async function putVerifiedR2Artifact(artifact, key, bucket, { env = process.env, fetchImpl, sleep } = {}) {
  if (!validRoute(key, bucket)) throw new PublicationError("PUBLICATION_ROUTE_INVALID", "invalid R2 publication route");
  const bytes = assertArtifactUnchanged(artifact);
  await inProcessUpload({ file: artifact.file, bytes: bytes.length < R2_MULTIPART_THRESHOLD_BYTES ? bytes : null, artifact, key, bucket, env, fetchImpl, sleep });
  return artifact;
}

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

// Path-only large-file transport: success requires remote bytes matching captured evidence. Objects below R2_MULTIPART_THRESHOLD_BYTES go up in
// one PutObject; larger ones go up as a multipart upload (r2-s3-multipart.mjs, #902), both through the S3-compatible endpoint in this process
// (rightkit#1754: wrangler, a child process, is off this path). Either way the readback streams GetObject through a sha256 and a byte counter
// (docs/rules/cloudflare-access.md Rule 6). The S3 credentials are derived in memory from CLOUDFLARE_API_TOKEN and never printed.
// The order the callers keep is immutable object -> signed envelope -> pointer, and a readback that differs from the evidence is
// PUBLICATION_REMOTE_MISMATCH.
export async function uploadVerifiedR2File(file, evidence, key, bucket, { run = spawnSync, env = process.env, cwd, dryRun = false, fetchImpl, sleep } = {}) {
  if (!["public", "private"].includes(bucket) || !/^[A-Za-z0-9][A-Za-z0-9._/-]*$/.test(key ?? "") || key.includes("..")) throw new PublicationError("PUBLICATION_ROUTE_INVALID", "invalid R2 publication route");
  const artifact = verifyArtifactFile(file, evidence);
  if (dryRun) return artifact;
  // Below the multipart threshold the bytes are read once and verified again, then go up in one PutObject; above it the file goes up multipart.
  const bytes = artifact.sizeBytes < R2_MULTIPART_THRESHOLD_BYTES ? readVerifiedArtifact(file, evidence).bytes : null;
  await inProcessUpload({ file, bytes, artifact, key, bucket, env, fetchImpl, sleep });
  return artifact;
}
