import { useEffect, useState } from "react";
import { AppWindow, FileAudio } from "@phosphor-icons/react";
import { cx } from "@rightkit/react-shell/react";
import type { LibraryOrigin, LibrarySource } from "../contract.js";

// Real application icons. The source resolves an origin (by its `iconKey`) to an image URL, or null
// when no icon is available. Results are cached for the life of the page (module-level) and
// concurrent requests for one key share a single call, so a list of 200 rows from the same few
// apps costs a few calls.
const iconCache = new Map<string, string | null>();
const iconInFlight = new Map<string, Promise<string | null>>();
const iconListeners = new Map<string, Set<(url: string | null) => void>>();

function requestIcon(key: string, origin: LibraryOrigin, source: LibrarySource): Promise<string | null> {
  const pending = iconInFlight.get(key);
  if (pending) return pending;
  const request = (source.originIcon ? source.originIcon(origin) : Promise.resolve(null))
    .then((url) => (typeof url === "string" && url.length > 0 ? url : null))
    .catch(() => null)
    .then((url) => {
      iconCache.set(key, url);
      iconInFlight.delete(key);
      const listeners = iconListeners.get(key);
      if (listeners) {
        for (const notify of listeners) notify(url);
        iconListeners.delete(key);
      }
      return url;
    });
  iconInFlight.set(key, request);
  return request;
}

/** Cached icon for an origin (image URL) or null while loading/unavailable. */
export function useOriginIcon(origin: LibraryOrigin | undefined, source: LibrarySource): string | null {
  const key = origin?.kind === "app" || origin?.kind === undefined ? (origin?.iconKey?.trim().toLowerCase() ?? "") : "";
  const [url, setUrl] = useState<string | null>(() => (key ? (iconCache.get(key) ?? null) : null));
  useEffect(() => {
    if (!key) {
      setUrl(null);
      return;
    }
    if (iconCache.has(key)) {
      setUrl(iconCache.get(key) ?? null);
      return;
    }
    let live = true;
    const notify = (next: string | null) => {
      if (live) setUrl(next);
    };
    const listeners = iconListeners.get(key) ?? new Set();
    listeners.add(notify);
    iconListeners.set(key, listeners);
    if (origin) void requestIcon(key, origin, source);
    return () => {
      live = false;
      listeners.delete(notify);
    };
    // The key identifies the origin; a new object for the same key must not refetch.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, source]);
  return url;
}

/**
 * Leading tile for a row: the real app icon when the source resolves one. File and synced origins
 * keep a type glyph. An app origin whose icon cannot be resolved renders an EMPTY fixed-width slot
 * (no placeholder glyph, which would look like a real icon) so the columns stay aligned. A letter
 * monogram is never shown: two apps starting with the same letter read as the same app.
 */
export function OriginIcon({
  origin,
  source,
  size = 28,
  className,
}: {
  origin: LibraryOrigin | undefined;
  source: LibrarySource;
  size?: number;
  className?: string;
}) {
  const kind = origin?.kind ?? "app";
  const label = origin?.label ?? "";
  const url = useOriginIcon(origin, source);
  const box = { width: size, height: size };

  if (url) {
    return (
      <img
        src={url}
        alt=""
        aria-hidden="true"
        title={label}
        draggable={false}
        style={box}
        className={cx("shrink-0 select-none rounded-[22%] object-contain", className)}
      />
    );
  }
  if (kind === "app") {
    return <span aria-hidden="true" style={box} className={cx("shrink-0", className)} />;
  }
  const Glyph = kind === "file" ? FileAudio : AppWindow;
  return (
    <span
      aria-hidden="true"
      title={label}
      style={box}
      className={cx(
        "grid shrink-0 place-items-center rounded-[22%] bg-[var(--surface-3)] text-[var(--muted)]",
        className,
      )}
    >
      <Glyph style={{ width: size * 0.54, height: size * 0.54 }} />
    </span>
  );
}
