import { isManagedHost } from "./managed-host-secrets.mjs";

export function resolveReleaseCacheMode({ requested, platform, env = process.env } = {}) {
  const fallback = env.GITHUB_ACTIONS === "true"
    ? "legacy"
    : platform === "mac" || platform === "win" ? "shared" : "legacy";
  const mode = requested ?? fallback;
  if (mode !== "legacy" && mode !== "shared") throw new Error("RIGHT_RELEASE_CACHE_MODE must be legacy or shared");
  return mode;
}

/**
 * Legacy (non-shared, non-broker) mode links the checkout's target directory to the release cache.
 * A target that already exists and points elsewhere fails closed on developer hosts, where it may hold
 * work. Hosted CI runners are ephemeral: an earlier workflow step (a build or a cache restore) may have
 * created a real target directory, so it is renamed aside and the link created instead of failing.
 */
export function ensureLegacyTargetLink({ targetLink, cacheTarget, platform = process.platform, env = process.env, fs, now = () => Date.now() }) {
  const linkType = platform === "win32" ? "junction" : "dir";
  if (!fs.existsSync(targetLink)) { fs.symlinkSync(cacheTarget, targetLink, linkType); return { action: "linked" }; }
  if (fs.realpathSync(targetLink) === fs.realpathSync(cacheTarget)) return { action: "current" };
  if (env.GITHUB_ACTIONS !== "true" || isManagedHost(env)) {
    return { action: "refused", reason: `primary checkout target exists and is not the release cache link: ${targetLink}` };
  }
  const aside = `${targetLink}.pre-right-release-${now()}`;
  fs.renameSync(targetLink, aside);
  fs.symlinkSync(cacheTarget, targetLink, linkType);
  return { action: "moved-aside", aside };
}
