// The distribution an upload uses for one platform. An app that ships desktop and mobile has a different store per
// platform, so `targets.<platform>.distribution` wins; the top-level `distribution` is the single-store fallback.
export function targetDistribution(config, platform) {
  return config?.targets?.[platform]?.distribution ?? config?.distribution;
}
