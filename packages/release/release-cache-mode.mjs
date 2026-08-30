export function resolveReleaseCacheMode({ requested, platform, env = process.env } = {}) {
  const fallback = env.GITHUB_ACTIONS === "true"
    ? "legacy"
    : platform === "mac" || platform === "win" ? "shared" : "legacy";
  const mode = requested ?? fallback;
  if (mode !== "legacy" && mode !== "shared") throw new Error("RIGHT_RELEASE_CACHE_MODE must be legacy or shared");
  return mode;
}
