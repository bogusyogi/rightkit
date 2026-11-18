import { useCallback, useEffect, useRef, useState } from "react";
import { pillClient } from "./pillHost.js";

export interface MiniClipboardItem {
  delivery_id: string;
  transcript: string;
  raw_transcript?: string | null;
}

export type MiniClipboardMode = "single" | "recent";

export interface MiniClipboardOptions {
  items: MiniClipboardItem[];
  loaded?: boolean;
  mode: MiniClipboardMode;
  onModeChange?: (mode: MiniClipboardMode, items: MiniClipboardItem[]) => void;
  onClose?: () => void;
  onAfterOpenHub?: () => void;
  onEditInteraction?: () => void;
  closeAfterCopy?: boolean;
}

function withTimeout<T>(promise: Promise<T>, ms = 1500): Promise<T> {
  let timer: number | undefined;
  return Promise.race([
    promise,
    new Promise<never>((_, reject) => {
      timer = window.setTimeout(() => reject(new Error("timeout")), ms);
    }),
  ]).finally(() => {
    if (timer !== undefined) window.clearTimeout(timer);
  });
}

async function openHub(tab: "home" | "history") {
  await pillClient().action(tab === "history" ? "openHubHistory" : "openHubHome").catch(() => undefined);
}

export function useMiniClipboard({
  items,
  loaded = true,
  mode,
  onModeChange,
  onClose,
  onAfterOpenHub,
  onEditInteraction,
  closeAfterCopy = false,
}: MiniClipboardOptions) {
  const [copiedId, setCopiedId] = useState("");
  // The current (single) row is editable so a misheard word can be fixed before it
  // lands in the clipboard. `null` = untouched → use the item's own transcript.
  const current = mode === "single" ? items[0] : undefined;
  // Draft is tagged with the item id it belongs to, so it auto-resets when the
  // current transcript changes - no effect, no stale sync.
  const [draft, setDraft] = useState<{ id: string; text: string; dirty: boolean } | null>(null);
  const fieldRef = useRef<HTMLTextAreaElement | null>(null);
  // Copied-flash / close-after-copy timers: tracked so an unmount (pill
  // dismiss) clears them instead of writing state on a dead component.
  const pendingTimers = useRef<Set<number>>(new Set());
  const schedule = useCallback((fn: () => void, ms: number) => {
    const id = window.setTimeout(() => {
      pendingTimers.current.delete(id);
      fn();
    }, ms);
    pendingTimers.current.add(id);
  }, []);
  useEffect(
    () => () => {
      for (const id of pendingTimers.current) window.clearTimeout(id);
      pendingTimers.current.clear();
    },
    [],
  );
  const editValue =
    current && draft && draft.id === current.delivery_id ? draft.text : (current?.transcript ?? "");
  const rawTranscript =
    current?.raw_transcript && current.raw_transcript.trim() !== current.transcript.trim()
      ? current.raw_transcript
      : "";
  const polishedTranscript = current?.transcript ?? "";
  const showingRaw =
    Boolean(rawTranscript) && editValue.trim() === rawTranscript.trim() && draft?.dirty !== true;
  const finishCopy = useCallback(
    (id: string) => {
      setCopiedId(id);
      if (closeAfterCopy) {
        schedule(() => onClose?.(), 260);
      } else {
        schedule(() => {
          setCopiedId((c) => (c === id ? "" : c));
        }, 1200);
      }
    },
    [closeAfterCopy, onClose, schedule],
  );

  async function copyEdited() {
    if (!current) return;
    // Read the live textarea value, not the controlled state — a webview reload /
    // item refetch can reset the React draft, which would copy the stale original.
    const value = fieldRef.current?.value ?? editValue;
    try {
      await withTimeout(pillClient().copyText(value));
      // Persist only user edits. AI undo/redo is a local view toggle so redo is
      // not lost by overwriting the persisted polished transcript.
      if (draft?.dirty && value !== current.transcript) {
        onModeChange?.(
          mode,
          items.map((item) =>
            item.delivery_id === current.delivery_id ? { ...item, transcript: value } : item,
          ),
        );
        setDraft({ id: current.delivery_id, text: value, dirty: true });
        const update = withTimeout(
          pillClient().updateHistoryItem(current.delivery_id, value),
          600,
        ).catch(() => undefined);
        if (current.delivery_id === "current") {
          await update;
        } else {
          void update;
        }
      }
      finishCopy(current.delivery_id);
    } catch {
      setCopiedId("");
    }
  }

  const copy = useCallback(
    async (item: MiniClipboardItem) => {
      try {
        await withTimeout(pillClient().copyText(item.transcript));
        finishCopy(item.delivery_id);
      } catch {
        setCopiedId("");
      }
    },
    [finishCopy],
  );

  function toggleAiPolish() {
    if (!current || !rawTranscript) return;
    const next = showingRaw ? polishedTranscript : rawTranscript;
    setDraft({ id: current.delivery_id, text: next, dirty: false });
  }

  return {
    items, loaded, mode, current, copiedId, editValue, rawTranscript,
    polishedTranscript, showingRaw, fieldRef,
    copy, copyEdited, toggleAiPolish, close: onClose,
    markEditing: () => onEditInteraction?.(),
    setEditValue: (text: string) => {
      if (!current) return;
      setDraft({ id: current.delivery_id, text, dirty: true });
    },
    openHub: (tab: "home" | "history") => openHub(tab).finally(() => onAfterOpenHub?.()),
  };
}

export type MiniClipboardController = ReturnType<typeof useMiniClipboard>;
export interface MiniClipboardProps extends MiniClipboardOptions {
  children: (controller: MiniClipboardController) => import("react").ReactNode;
}

/** Host owns layout, labels, controls & styling. */
export default function MiniClipboard({ children, ...options }: MiniClipboardProps) {
  return children(useMiniClipboard(options));
}
