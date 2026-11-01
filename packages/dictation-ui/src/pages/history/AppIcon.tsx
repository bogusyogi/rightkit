import { useEffect, useState } from "react";
import { AppWindow, FileAudio } from "@phosphor-icons/react";
import type { HistoryItem } from "../../lib/homeMetrics.js";
import { invoke } from "../../lib/tauri.js";
import { cx } from "../../lib/ui.js";
import { targetLabel } from "./historyUtils.js";

// Real application icons. The backend command `app_icon_png` resolves a
// process (by exe path when known, else by name) to a `data:image/png;base64`
// URL, or null when no icon is available. Results are cached for the life of
// the page (module-level) and concurrent requests for one app share a single
// invoke, so a list of 200 rows from the same few apps costs a few calls.
const iconCache = new Map<string, string | null>();
const iconInFlight = new Map<string, Promise<string | null>>();
const iconListeners = new Map<string, Set<(url: string | null) => void>>();

function iconKey(processName: string | undefined, exePath: string | undefined): string {
  return (exePath?.trim() || processName?.trim() || "").toLowerCase();
}

function requestIcon(
  key: string,
  processName: string | undefined,
  exePath: string | undefined,
): Promise<string | null> {
  const pending = iconInFlight.get(key);
  if (pending) return pending;
  const request = invoke<string | null>("app_icon_png", {
    processName: processName?.trim() ?? "",
    exePath: exePath?.trim() || null,
  })
    .then((url) => (typeof url === "string" && url.startsWith("data:image/") ? url : null))
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

/** Cached real icon for an app (data URL) or null while loading/unavailable. */
export function useAppIcon(
  processName: string | undefined,
  exePath: string | undefined,
): string | null {
  const key = iconKey(processName, exePath);
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
    void requestIcon(key, processName, exePath);
    return () => {
      live = false;
      listeners.delete(notify);
    };
  }, [key, processName, exePath]);
  return url;
}

type TargetWithExe = NonNullable<HistoryItem["target"]>;

/**
 * Leading app tile for a history/recent row: the real app icon when the
 * backend can resolve one. File transcriptions and synced rows keep a type
 * glyph (file / device window). A dictation row whose app icon cannot be
 * resolved renders an EMPTY fixed-width slot - no placeholder glyph, which
 * would look like a real icon - so the columns stay aligned. A letter
 * monogram is never shown - two apps starting with the same letter read as
 * the same app.
 */
export function AppIcon({
  item,
  size = 28,
  className,
}: {
  item: HistoryItem;
  size?: number;
  className?: string;
}) {
  const kind = item.source?.kind;
  const target: TargetWithExe | undefined = item.target;
  const label = target?.process_name?.trim() || targetLabel(item);
  const hasTargetApp = kind !== "file_transcription" && kind !== "cloud_sync";
  const url = useAppIcon(
    hasTargetApp ? target?.process_name : undefined,
    hasTargetApp ? target?.exe_path : undefined,
  );
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
  if (hasTargetApp) {
    return <span aria-hidden="true" style={box} className={cx("shrink-0", className)} />;
  }
  const Glyph = kind === "file_transcription" ? FileAudio : AppWindow;
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
