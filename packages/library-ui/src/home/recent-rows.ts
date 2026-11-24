// The logic of a home page's "recent" strip and file drop row, without its markup: a compact "5m ago" time,
// the "App · Window" meta text, a native drag hit-test and a drop-zone hook, and the per-row state (copy with a
// short flash, fix a transcript, show the original). The commands, the clipboard and the drag event are the
// host's. Imports only `react`.
import { useCallback, useEffect, useRef, useState } from "react";
import type { LibraryOrigin } from "../contract.js";

/** Words for {@link compactTimeAgo}. Every field is optional; the default is the English shown. */
export interface CompactTimeAgoLabels {
  /** Under a minute. Default `Just now`. */
  justNow?: string;
  /** Under an hour. Default `${n}m ago`. */
  minutes?: (n: number) => string;
  /** Under a day. Default `${n}h ago`. */
  hours?: (n: number) => string;
  /** A day or more. Default `${n}d ago`. */
  days?: (n: number) => string;
}

/**
 * A short relative time for an epoch-millisecond timestamp: `Just now`, `5m ago`, `3h ago`, `2d ago`.
 * A future timestamp reads as `Just now`. The host passes `labels` to change the wording and `now` for tests.
 */
export function compactTimeAgo(ms: number, labels: CompactTimeAgoLabels = {}, now: number = Date.now()): string {
  const delta = Math.max(0, now - ms);
  if (delta < 60_000) return labels.justNow ?? "Just now";
  if (delta < 3_600_000) {
    const n = Math.round(delta / 60_000);
    return labels.minutes ? labels.minutes(n) : `${n}m ago`;
  }
  if (delta < 86_400_000) {
    const n = Math.round(delta / 3_600_000);
    return labels.hours ? labels.hours(n) : `${n}h ago`;
  }
  const n = Math.round(delta / 86_400_000);
  return labels.days ? labels.days(n) : `${n}d ago`;
}

/**
 * The meta text of a row's origin: `Label · detail` when the origin has a detail (a window title), the label
 * alone otherwise, and an empty string with no origin. `separator` defaults to ` · `.
 */
export function originMetaText(origin: LibraryOrigin | undefined | null, separator = " · "): string {
  return origin?.detail ? `${origin.label}${separator}${origin.detail}` : (origin?.label ?? "");
}

/** A native drag pointer position in physical pixels, as the webview reports it. */
export interface NativeDragPosition {
  x: number;
  y: number;
}

/**
 * Whether a native drag pointer sits inside `el`. The position is in physical pixels and
 * `getBoundingClientRect` in CSS pixels, so the position is divided by `scale` (default
 * `window.devicePixelRatio`, or 1). A missing element is never inside.
 */
export function isDragPositionInside(
  el: HTMLElement | null | undefined,
  position: NativeDragPosition,
  scale: number = (typeof window === "undefined" ? 1 : window.devicePixelRatio) || 1,
): boolean {
  if (!el) return false;
  const x = position.x / scale;
  const y = position.y / scale;
  const rect = el.getBoundingClientRect();
  return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
}

/** One native drag-drop event. `position` is missing on `leave`. */
export type DropZoneEvent =
  | { type: "enter"; position: NativeDragPosition }
  | { type: "over"; position: NativeDragPosition }
  | { type: "drop"; position: NativeDragPosition }
  | { type: "leave" };

/** Options of {@link useFileDropZone}. */
export interface UseFileDropZoneOptions {
  /** Subscribes to the native drag-drop event; resolves to the unsubscribe. The host adapts its event binding. */
  subscribe(cb: (event: DropZoneEvent) => void): Promise<() => void>;
  /**
   * Called once per drag, when the pointer first enters the zone (the host opens its transcribe surface).
   * `over` events stream at the OS rate and do not call it again until the drag leaves or drops.
   */
  onEnterZone(): void;
}

/** What {@link useFileDropZone} returns. */
export interface FileDropZone<E extends HTMLElement = HTMLElement> {
  /** Attach to the drop zone element. */
  ref: { current: E | null };
  /** Whether a file drag currently has the pointer inside the zone: the host styles the highlight from it. */
  over: boolean;
}

/**
 * A drop zone fed by a native drag-drop event: tracks whether the drag pointer is over the zone, and calls
 * `onEnterZone` once per drag so a streaming event does not reopen the surface each time. Does not read
 * the dropped paths: the surface `onEnterZone` opens receives the follow-on events itself.
 */
export function useFileDropZone<E extends HTMLElement = HTMLElement>(options: UseFileDropZoneOptions): FileDropZone<E> {
  const ref = useRef<E | null>(null);
  const [over, setOver] = useState(false);
  const opened = useRef(false);
  const optionsRef = useRef(options);
  optionsRef.current = options;

  useEffect(() => {
    const unlisten = optionsRef.current.subscribe((event) => {
      if (event.type === "leave") {
        setOver(false);
        opened.current = false;
        return;
      }
      const inside = isDragPositionInside(ref.current, event.position);
      setOver(inside);
      if (inside && !opened.current) {
        opened.current = true;
        optionsRef.current.onEnterZone();
      }
      if (event.type === "drop") {
        opened.current = false;
        setOver(false);
      }
    });
    return () => {
      void unlisten.then((stop) => stop()).catch(() => undefined);
    };
  }, []);

  return { ref, over };
}

/** Options of {@link useRecentRowActions}. */
export interface UseRecentRowActionsOptions {
  /** Copies text to the clipboard. */
  copyText(text: string): Promise<void>;
  /** The error shown when a copy fails; receives the failure text. */
  copyFailedMessage(detail: string): string;
  /**
   * Stores a corrected transcript for a row. Resolves true when saved, false when the row no longer exists
   * (`notFoundMessage` is shown); a rejection is shown as its text.
   */
  saveCorrection(id: string, transcript: string): Promise<boolean>;
  /** The error when `saveCorrection` resolves false. Default `Transcript was not found.` */
  notFoundMessage?: string;
  /** Called after a correction is saved, to refetch the rows. */
  onSaved?(): Promise<unknown> | void;
  /** How long a "copied" flash lasts. Default 1500 ms. */
  copiedMs?: number;
}

/** What {@link useRecentRowActions} returns. */
export interface RecentRowActions {
  /** The id of the row just copied, until the flash ends. */
  copiedId: string | null;
  /** The id of the row being fixed, or an empty string. */
  editingId: string;
  /** The text being fixed. */
  draft: string;
  setDraft(text: string): void;
  /** Whether a row shows its original (not polished) text. */
  isShowingRaw(id: string): boolean;
  /** The last copy or save failure, or an empty string. */
  actionError: string;
  clearError(): void;
  /** Copies `text` and flashes `copiedId` for the row. */
  copy(id: string, text: string): void;
  /** Starts fixing a row, with `text` as the draft. */
  startEdit(id: string, text: string): void;
  /** Stops fixing and drops the draft. */
  cancelEdit(): void;
  /** Saves the trimmed draft for the row; an empty draft does nothing. */
  saveEdit(id: string): Promise<void>;
  /** Flips a row between its polished and original text. */
  toggleRaw(id: string): void;
}

/**
 * The per-row state of a recent list: copy with a short flash (cleared on unmount), fix a transcript
 * (draft, save, cancel) and the polished/original toggle, with the failure text in one place. The host passes
 * the clipboard and storage calls and draws the rows.
 */
export function useRecentRowActions(options: UseRecentRowActionsOptions): RecentRowActions {
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [editingId, setEditingId] = useState("");
  const [draft, setDraft] = useState("");
  const [showRaw, setShowRaw] = useState<Record<string, boolean>>({});
  const [actionError, setActionError] = useState("");
  const timer = useRef<number | undefined>(undefined);
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const draftRef = useRef(draft);
  draftRef.current = draft;

  // Clear the pending flash timer on unmount so it cannot write state on an unmounted component.
  useEffect(
    () => () => {
      if (timer.current !== undefined) window.clearTimeout(timer.current);
    },
    [],
  );

  const copy = useCallback((id: string, text: string) => {
    setActionError("");
    optionsRef.current
      .copyText(text)
      .then(() => {
        setCopiedId(id);
        if (timer.current !== undefined) window.clearTimeout(timer.current);
        timer.current = window.setTimeout(
          () => setCopiedId((prev) => (prev === id ? null : prev)),
          optionsRef.current.copiedMs ?? 1500,
        );
      })
      .catch((e: unknown) => setActionError(optionsRef.current.copyFailedMessage(String(e))));
  }, []);

  const startEdit = useCallback((id: string, text: string) => {
    setEditingId(id);
    setDraft(text);
  }, []);

  const cancelEdit = useCallback(() => {
    setEditingId("");
    setDraft("");
  }, []);

  const saveEdit = useCallback(async (id: string) => {
    const transcript = draftRef.current.trim();
    if (!transcript) return;
    try {
      const saved = await optionsRef.current.saveCorrection(id, transcript);
      if (!saved) throw new Error(optionsRef.current.notFoundMessage ?? "Transcript was not found.");
      setEditingId("");
      setDraft("");
      await optionsRef.current.onSaved?.();
    } catch (e) {
      setActionError(String(e));
    }
  }, []);

  const toggleRaw = useCallback((id: string) => {
    setShowRaw((current) => ({ ...current, [id]: !current[id] }));
  }, []);

  return {
    copiedId,
    editingId,
    draft,
    setDraft,
    isShowingRaw: (id) => !!showRaw[id],
    actionError,
    clearError: () => setActionError(""),
    copy,
    startEdit,
    cancelEdit,
    saveEdit,
    toggleRaw,
  };
}
