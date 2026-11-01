/** #251: playlist and channel links. Mirrors `scraperight_detect::classify_playlist`
 * (the engine re-checks; this only decides which command the paste bar calls). */

/** Result of the `submit_playlist` command. */
export interface PlaylistResponse {
  playlist: string;
  limit: number;
  found: number;
  accepted: number;
  job_ids: string[];
  rejected: string[];
}

const CHANNEL_TABS = new Set(["videos", "shorts", "streams"]);

function parse(input: string): URL | null {
  const trimmed = input.trim();
  if (!/^https?:\/\//i.test(trimmed)) return null;
  try {
    return new URL(trimmed);
  } catch {
    return null;
  }
}

/** True for a playlist or channel link: `youtube.com/playlist?list=`,
 * `youtube.com/@name|channel/id|c/name|user/name[/videos|shorts|streams]`, and
 * Vimeo showcases, SoundCloud sets and Dailymotion playlists. A watch link that
 * merely carries `list=` is a single video and stays false. */
export function isPlaylistUrl(input: string): boolean {
  const url = parse(input);
  if (!url) return false;
  const host = url.hostname.toLowerCase().replace(/^(www|m)\./, "");
  const segs = url.pathname.toLowerCase().split("/").filter(Boolean);
  const tabOk = (rest: string[]) => rest.length === 0 || (rest.length === 1 && CHANNEL_TABS.has(rest[0]));
  switch (host) {
    case "youtube.com": {
      if (segs[0] === "playlist" && segs.length === 1) {
        return (url.searchParams.get("list") ?? "").trim().length > 0;
      }
      if (segs[0]?.startsWith("@") && segs[0].length > 1) return tabOk(segs.slice(1));
      if (["channel", "c", "user"].includes(segs[0]) && segs.length >= 2) return tabOk(segs.slice(2));
      return false;
    }
    case "vimeo.com":
      return segs.length === 2 && ["showcase", "album", "channels"].includes(segs[0]);
    case "soundcloud.com":
      return segs.length === 3 && segs[1] === "sets";
    case "dailymotion.com":
      return segs.length === 2 && segs[0] === "playlist";
    default:
      return false;
  }
}

/** The notice shown once a playlist is queued. */
export function playlistQueuedNotice(response: Pick<PlaylistResponse, "accepted" | "rejected">): string {
  const n = response.accepted;
  const base = `Queued ${n} ${n === 1 ? "video" : "videos"}`;
  const skipped = response.rejected.length;
  return skipped > 0 ? `${base} (${skipped} skipped)` : base;
}
