import { RIGHT_SUITE_API_ORIGIN } from "./suite-api.generated.mjs";

const DEFAULT_API_BASE = RIGHT_SUITE_API_ORIGIN;
const REGISTRATION_ROUTES = new Set([
  "/v1/admin/apps/patches",
  "/v1/admin/apps/releases",
  "/v1/admin/apps/runtime-artifacts",
]);

export async function registerRightAppsRelease(route, body, {
  token,
  env = process.env,
  fetchImpl = fetch,
} = {}) {
  if (!token?.trim()) throw new Error("RIGHTAPPS_RELEASE_TOKEN release token is required for RightApps registration");
  if (!REGISTRATION_ROUTES.has(route)) {
    throw new Error("RightApps registration route is not approved");
  }
  const apiBase = (env.RIGHTAPPS_API_URL || DEFAULT_API_BASE).replace(/\/$/, "");
  const response = await fetchImpl(`${apiBase}${route}`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${token.trim()}`,
      "content-type": "application/json",
      "x-store-slug": env.RIGHTAPPS_STORE_SLUG || "rightapps",
    },
    body: JSON.stringify(body),
  });
  const text = await response.text();
  if (!response.ok) throw new Error(`RightApps registration failed: ${response.status} ${text}`);
  if (!text) return null;
  try { return JSON.parse(text); } catch { return text; }
}
