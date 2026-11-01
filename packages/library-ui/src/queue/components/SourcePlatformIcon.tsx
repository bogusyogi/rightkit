import type { Kind } from "../contract";

const platforms = [
  { id: "youtube", label: "YouTube", color: "#d92c2c", domains: ["youtube.com", "youtu.be"] },
  {
    id: "instagram",
    label: "Instagram",
    color: "linear-gradient(135deg, #d66e29, #b52c82, #704bc0)",
    domains: ["instagram.com"],
  },
  { id: "threads", label: "Threads", color: "#292929", domains: ["threads.net", "threads.com"] },
  { id: "tiktok", label: "TikTok", color: "#292929", domains: ["tiktok.com"] },
  { id: "facebook", label: "Facebook", color: "#1877f2", domains: ["facebook.com", "fb.watch"] },
  { id: "soundcloud", label: "SoundCloud", color: "#dc6500", domains: ["soundcloud.com"] },
  { id: "x", label: "X", color: "#292929", domains: ["x.com", "twitter.com"] },
  { id: "vimeo", label: "Vimeo", color: "#1481ab", domains: ["vimeo.com"] },
  { id: "twitch", label: "Twitch", color: "#7442b5", domains: ["twitch.tv"] },
  { id: "reddit", label: "Reddit", color: "#d84712", domains: ["reddit.com", "redd.it"] },
  { id: "bandcamp", label: "Bandcamp", color: "#38788b", domains: ["bandcamp.com"] },
  { id: "mixcloud", label: "Mixcloud", color: "#454b77", domains: ["mixcloud.com"] },
] as const;

type Platform = (typeof platforms)[number];

export function sourcePlatform(kind: Kind, input: string): Platform | undefined {
  try {
    const url = new URL(input);
    if (/^https?:$/.test(url.protocol)) {
      const host = url.hostname.toLowerCase();
      const platform = platforms.find(({ domains }) =>
        domains.some((domain) => host === domain || host.endsWith(`.${domain}`)),
      );
      if (platform) return platform;
    }
  } catch {
    // Local paths and unresolved inputs retain their detected source kind.
  }
  const id = ["reel", "carousel", "instagram_post"].includes(kind) ? "instagram" : kind;
  return platforms.find((platform) => platform.id === id);
}

export function SourcePlatformIcon({ platform, size = 16 }: { platform: Platform; size?: number }) {
  return (
    <img
      className="source-platform-icon"
      src={`/source-icons/${platform.id}.svg`}
      width={size}
      height={size}
      alt=""
      aria-hidden="true"
      title={platform.label}
      data-source-platform={platform.id}
    />
  );
}
