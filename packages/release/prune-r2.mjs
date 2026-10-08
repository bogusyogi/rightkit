import { resolveCloudflareApiToken } from "./protected-credentials.mjs";

const ACCOUNT_ID = process.env.CLOUDFLARE_ACCOUNT_ID || "03ae77ccd7a07bcbb2dcfde47fa7ba3a";
const BUCKETS = { public: "rightapps-downloads", private: "rightapps-updates" };

export async function pruneReleaseObjects({ app, platform, keep, dryRun = false, env = process.env, credentialStore }) {
  if (!new Set(["mac", "windows"]).has(platform)) throw new Error(`invalid R2 prune platform: ${platform}`);
  const token = resolveCloudflareApiToken({ env, credentialStore, required: !dryRun });
  for (const [alias, bucket] of Object.entries(BUCKETS)) {
    const retained = new Set(keep[alias] ?? []);
    if (dryRun) {
      console.log(`[dry-run] PRUNE ${bucket}/${app}/${platform} keep=${[...retained].sort().join(",") || "<none>"}`);
      continue;
    }
    for (const key of await listObjects(bucket, app, token)) {
      if (!isReleaseObject(app, platform, key) || retained.has(key)) continue;
      console.log(`DELETE ${bucket}/${key}`);
      await deleteObject(bucket, key, token);
    }
  }
}

export function isReleaseObject(app, platform, key) {
  if (!key.startsWith(`${app}/`)) return false;
  const relative = key.slice(app.length + 1);
  const segments = relative.split("/");
  if (!segments.includes(platform)) return false;
  if (/^(?:installers|updates|updaters|releases)\//.test(relative)) return true;
  return /^v?\d+\.\d+\.\d+(?:[-+][^/]+)?\//.test(relative);
}

async function listObjects(bucket, app, token) {
  const objects = [];
  let cursor = "";
  do {
    const url = new URL(`https://api.cloudflare.com/client/v4/accounts/${ACCOUNT_ID}/r2/buckets/${bucket}/objects`);
    url.searchParams.set("prefix", `${app}/`);
    if (cursor) url.searchParams.set("cursor", cursor);
    const response = await fetch(url, { headers: { authorization: `Bearer ${token}` } });
    const body = await response.json().catch(() => ({}));
    if (!response.ok || body.success === false) throw new Error(`R2 list failed for ${bucket}: ${response.status}`);
    const page = Array.isArray(body.result) ? body.result : body.result?.objects ?? [];
    objects.push(...page.map((entry) => entry.key).filter(Boolean));
    cursor = body.result_info?.cursor || body.result?.cursor || "";
  } while (cursor);
  return objects;
}

async function deleteObject(bucket, key, token) {
  const encoded = key.split("/").map(encodeURIComponent).join("/");
  const response = await fetch(`https://api.cloudflare.com/client/v4/accounts/${ACCOUNT_ID}/r2/buckets/${bucket}/objects/${encoded}`, {
    method: "DELETE",
    headers: { authorization: `Bearer ${token}` },
  });
  const body = await response.json().catch(() => ({}));
  if (!response.ok || body.success === false) throw new Error(`R2 delete failed for ${bucket}/${key}: ${response.status}`);
}
