// Home: shortcut strip, file drop row & recent transcripts.
// Recording hints reflect current trigger settings; file actions open Transcribe.

import { useCallback, useEffect, useRef, useState } from "react";
import {
  ArrowsCounterClockwise,
  Check,
  Copy,
  FileAudio,
  Microphone,
  MouseMiddleClick,
  PencilSimple,
  Sparkle,
} from "@phosphor-icons/react";
import ErrorBanner from "../components/ErrorBanner";
import Kbd, { DICTATION_CHORD, pushToTalkChord } from "../components/Kbd";
import PageShell from "../components/PageShell";
import { useHomeDashboard } from "../hooks/useHomeDashboard";
import { usePlatform } from "../hooks/usePlatform";
import { useSettings } from "../hooks/useSettings";
import type { HistoryItem } from "../lib/homeMetrics";
import { invoke, listen } from "../lib/tauri";
import { cx, ui } from "../lib/ui";
import { AppIcon } from "./history/AppIcon";
import { historyActionChip, historyDevice, isLocallyDeletable, targetDisplay } from "./history/historyUtils";

// Same payload shape TranscribeSurface consumes - the backend emits
// `native-drag-drop` for every file drag over the hub window, with the
// pointer position in PHYSICAL pixels.
interface NativeDragDropPosition {
  x: number;
  y: number;
}

type NativeDragDropPayload =
  | { type: "enter"; paths: string[]; position: NativeDragDropPosition }
  | { type: "over"; position: NativeDragDropPosition }
  | { type: "drop"; paths: string[]; position: NativeDragDropPosition }
  | { type: "leave" };

function timeAgo(ms: number): string {
  const delta = Math.max(0, Date.now() - ms);
  if (delta < 60_000) return "Just now";
  if (delta < 3_600_000) return `${Math.round(delta / 60_000)}m ago`;
  if (delta < 86_400_000) return `${Math.round(delta / 3_600_000)}h ago`;
  return `${Math.round(delta / 86_400_000)}d ago`;
}

// "App · Window title" for the Recent row meta: a friendly app name once, and
// the window title only when it adds information (see targetDisplay).
function targetMeta(item: HistoryItem): string {
  const { app, windowTitle } = targetDisplay(item);
  return windowTitle ? `${app} · ${windowTitle}` : app;
}

// True when the native drag pointer sits inside `el`'s bounds. The payload's
// position is in physical pixels; getBoundingClientRect is in CSS px.
function positionInside(el: HTMLElement | null, position: NativeDragDropPosition): boolean {
  if (!el) return false;
  const scale = window.devicePixelRatio || 1;
  const x = position.x / scale;
  const y = position.y / scale;
  const rect = el.getBoundingClientRect();
  return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
}

export default function HomePage({ onOpenTranscribe }: { onOpenTranscribe?: () => void }) {
  const { history, error, clearError, refreshHistory } = useHomeDashboard("home");
  const platform = usePlatform();
  const { get } = useSettings();

  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [editingId, setEditingId] = useState("");
  const [draft, setDraft] = useState("");
  const [showRaw, setShowRaw] = useState<Record<string, boolean>>({});
  const [actionError, setActionError] = useState("");
  const [fileDragOver, setFileDragOver] = useState(false);
  const copiedTimer = useRef<number | undefined>(undefined);
  const dropZoneRef = useRef<HTMLElement | null>(null);
  // One modal open per file-drag session - `over` events stream at OS rate
  // and must not re-run openTranscribe (each call re-captures focus state).
  const dragOpenedRef = useRef(false);

  const openTranscribe = useCallback(() => {
    if (onOpenTranscribe) onOpenTranscribe();
    else window.location.hash = "#transcribe";
  }, [onOpenTranscribe]);

  // Clear the pending copied-flash timer on unmount so it can't write state
  // on an unmounted component.
  useEffect(
    () => () => {
      if (copiedTimer.current !== undefined) window.clearTimeout(copiedTimer.current);
    },
    [],
  );

  // The drop zone hands real drops to the existing transcribe flow: when a
  // file drag reaches the zone, open the Transcribe modal and let ITS
  // `native-drag-drop` listener receive the follow-on over/drop events (the
  // payload's paths are only delivered to a mounted listener, so the file is
  // transcribed inside the modal, same as today). A drop that lands before
  // the modal mounts still opens it, so retrying is one motion.
  useEffect(() => {
    const unlistenPromise = listen<NativeDragDropPayload>("native-drag-drop", (event) => {
      const p = event.payload;
      if (p.type === "leave") {
        setFileDragOver(false);
        dragOpenedRef.current = false;
        return;
      }
      const inside = positionInside(dropZoneRef.current, p.position);
      setFileDragOver(inside);
      if (inside && !dragOpenedRef.current) {
        dragOpenedRef.current = true;
        openTranscribe();
      }
      if (p.type === "drop") {
        dragOpenedRef.current = false;
        setFileDragOver(false);
      }
    });
    return () => {
      void unlistenPromise.then((stop) => stop()).catch(() => undefined);
    };
  }, [openTranscribe]);

  function copyTranscript(id: string, text: string) {
    setActionError("");
    void invoke<void>("copy_text_to_clipboard", { text })
      .then(() => {
        setCopiedId(id);
        if (copiedTimer.current !== undefined) window.clearTimeout(copiedTimer.current);
        copiedTimer.current = window.setTimeout(
          () => setCopiedId((prev) => (prev === id ? null : prev)),
          1500,
        );
      })
      .catch((nextError) => {
        setActionError(`Clipboard copy failed: ${String(nextError)}`);
      });
  }

  async function saveCorrection(deliveryId: string) {
    const transcript = draft.trim();
    if (!transcript) return;
    try {
      const saved = await invoke<boolean>("update_history_item", { deliveryId, transcript });
      if (!saved) throw new Error("Transcript was not found.");
      setEditingId("");
      setDraft("");
      await refreshHistory();
    } catch (nextError) {
      setActionError(String(nextError));
    }
  }

  // Match History's default scope: this device's source only (synced iPhone/Watch rows stay in History), and cancelled & draft
  // rows belong outside Home's recent delivered dictations. historyDevice maps rows with no provenance (every local
  // Windows row) to "mac", so this filter changes nothing there.
  const recent = history
    .filter(
      (item) =>
        historyDevice(item) === "mac" &&
        item.outcome.kind !== "cancelled" &&
        item.outcome.kind !== "draft",
    )
    .slice(0, 5);
  // Both default true (see useSettings/Controls.tsx) — a raw settings.x read
  // without that fallback always reported "off" until a user touched the
  // toggle once, so this page kept telling people to "enable" triggers that
  // were already on.
  const pttOn = get("push_to_talk", true);
  const pttKey =
    platform === "macos"
      ? (get("mac_push_to_talk_key", "ctrl") as string)
      : (get("push_to_talk_key", "ralt") as string);
  const middleMouseOn = get("middle_mouse_recording", true);

  return (
    <PageShell tab="home">
      <div className="flex w-full flex-col gap-[var(--space-4)]">
        <ErrorBanner
          message={actionError || error}
          onDismiss={() => {
            setActionError("");
            clearError();
          }}
        />

        <section
          data-setting-id="home.dictate-into-any-app"
          className="grid grid-cols-[minmax(210px,1.25fr)_minmax(0,3fr)] items-center gap-4 rounded-[var(--radius-md)] border border-[var(--line)] bg-[var(--surface-2)] p-4 max-[1000px]:grid-cols-1"
        >
          <div className="flex items-start gap-3">
            <Microphone className="h-6 w-6 shrink-0 text-[var(--accent)]" weight="fill" aria-hidden="true" />
            <div>
              <h2 className={ui.sectionHeader}>Dictate into any app</h2>
              <p className="m-0 mt-1 text-[length:var(--size-xs)] text-[var(--muted)]">Text goes to the app that has focus.</p>
              <button type="button" className="mt-2 rounded text-[length:var(--size-xs)] text-[var(--accent-cool)] hover:underline focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]" onClick={() => { window.location.hash = "#controls"; }}>
                Controls →
              </button>
            </div>
          </div>
          <div className="grid grid-cols-3 items-stretch gap-2">
            <div data-home-trigger="shortcut" className="grid min-w-0 grid-rows-[28px_20px_18px] content-center gap-1 rounded-[var(--radius-sm)] border border-[var(--line)] bg-[var(--surface-1)] p-3">
              <div className="flex items-center"><Kbd chord={DICTATION_CHORD} /></div>
              <span className="text-[length:var(--size-sm)] font-semibold">Tap to start / stop</span>
              <span className="text-[length:var(--size-xs)] text-[var(--muted)]">Keyboard shortcut</span>
            </div>
            <div data-home-trigger="push-to-talk" className="grid min-w-0 grid-rows-[28px_20px_18px] content-center gap-1 rounded-[var(--radius-sm)] border border-[var(--line)] bg-[var(--surface-1)] p-3">
              <div className="flex items-center"><Kbd chord={pushToTalkChord(platform, pttKey)} /></div>
              <span className="text-[length:var(--size-sm)] font-semibold">Push-to-talk</span>
              <span className="text-[length:var(--size-xs)] text-[var(--muted)]">{pttOn ? "Release to paste" : "Disabled in Controls"}</span>
            </div>
            <div data-home-trigger="middle-click" className="grid min-w-0 grid-rows-[28px_20px_18px] content-center gap-1 rounded-[var(--radius-sm)] border border-[var(--line)] bg-[var(--surface-1)] p-3">
              <div className="flex items-center"><MouseMiddleClick className="h-5 w-5" aria-hidden="true" /></div>
              <span className="text-[length:var(--size-sm)] font-semibold">Middle-click</span>
              <span className="text-[length:var(--size-xs)] text-[var(--muted)]">{middleMouseOn ? "Tap to start / stop" : "Disabled in Controls"}</span>
            </div>
          </div>
        </section>

        <section
          ref={dropZoneRef}
          data-setting-id="home.transcribe-a-file"
          className={cx("flex flex-wrap items-center gap-3 rounded-[var(--radius-md)] border border-dashed px-4 py-2 text-[length:var(--size-sm)] transition-colors", fileDragOver ? "border-[var(--accent)] bg-[var(--surface-2)]" : "border-[var(--line)] bg-[var(--surface-2)]")}
        >
          <FileAudio className="h-5 w-5 shrink-0 text-[var(--muted)]" aria-hidden="true" />
          <span className="font-medium">Transcribe a file</span>
          <span className="text-[var(--muted)]">Drop audio or video here</span>
          <button type="button" onClick={openTranscribe} aria-label="Choose an audio or video file to transcribe" className={cx(ui.button, ui.buttonGhost, "ml-auto")}>
            Choose file…
          </button>
        </section>

        {/* Recent - inset list, full transcripts (wrap, never truncated). */}
        {recent.length > 0 && (
          <div className="hr-stagger" style={{ animationDelay: "80ms" }}>
            <div className="mb-[var(--space-2)] flex items-center justify-between">
              <h2 className={ui.sectionHeader}>Recent</h2>
              <button
                type="button"
                className="rounded-[var(--radius-sm)] text-[length:var(--size-sm)] text-[var(--muted)] transition-colors duration-150 ease-[var(--ease-out-strong)] hover:text-[var(--accent-cool)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]"
                onClick={() => {
                  window.location.hash = "#history";
                }}
              >
                Show All →
              </button>
            </div>
            <div className={cx(ui.surface, "overflow-hidden")}>
              {recent.map((item, index) => {
                // UX-6: use the stable delivery_id as the React key and the
                // copiedId state. delivered_at_ms can collide (two deliveries
                // in the same millisecond) AND the key as state confuses the
                // 'is this the row I just copied?' check after a refetch.
                const copied = copiedId === item.delivery_id;
                const isEditing = editingId === item.delivery_id;
                const isEditable = isLocallyDeletable(item);
                const hasAiEdit =
                  !!item.raw_transcript && item.raw_transcript !== item.transcript;
                const isShowingRaw = !!showRaw[item.delivery_id];
                const shownText =
                  isShowingRaw && item.raw_transcript ? item.raw_transcript : item.transcript;
                const actionContext = `recent item ${index + 1}, ${timeAgo(item.delivered_at_ms)}`;
                return (
                  <div
                    key={item.delivery_id}
                    data-home-history-row={item.delivery_id}
                    className="group flex min-w-0 items-start gap-[var(--space-3)] border-b border-[color-mix(in_srgb,var(--line),transparent_60%)] px-[var(--space-3)] py-[8px] text-left transition-colors duration-150 ease-[var(--ease-out-strong)] last:border-b-0 hover:bg-[var(--surface-hover)]"
                  >
                    <span className="mt-[1px] w-[56px] shrink-0 whitespace-nowrap [font-family:var(--font-mono)] text-[length:var(--size-xs)] text-[var(--muted-2)] [font-variant-numeric:tabular-nums]">
                      {timeAgo(item.delivered_at_ms)}
                    </span>
                    <AppIcon item={item} size={28} className="mt-[1px]" />
                    <div className="min-w-0 flex-1">
                      {isEditing ? (
                        <textarea
                          className={cx(ui.input, "min-h-[80px] w-full resize-y")}
                          aria-label="Correct transcript"
                          value={draft}
                          onChange={(event) => setDraft(event.target.value)}
                        />
                      ) : (
                        <span className="block whitespace-pre-wrap break-words text-[length:var(--size-sm)] leading-[1.45] text-[var(--paper)]">
                          {shownText}
                        </span>
                      )}
                      {isEditing ? (
                        <div className="mt-[var(--space-2)] flex flex-wrap items-center gap-[var(--space-1)]">
                          <button
                            type="button"
                            className={cx(ui.button, ui.buttonCompact)}
                            disabled={!draft.trim()}
                            onClick={() => void saveCorrection(item.delivery_id)}
                          >
                            Save fix
                          </button>
                          <button
                            type="button"
                            className={cx(ui.button, ui.buttonGhost, ui.buttonCompact)}
                            disabled={!draft.trim()}
                            onClick={() => copyTranscript(item.delivery_id, draft.trim())}
                          >
                            Copy fix
                          </button>
                          <button
                            type="button"
                            className={cx(ui.button, ui.buttonGhost, ui.buttonCompact)}
                            onClick={() => {
                              setEditingId("");
                              setDraft("");
                            }}
                          >
                            Cancel
                          </button>
                        </div>
                      ) : (
                        <span className="mt-[3px] block truncate text-[length:var(--size-xs)] text-[var(--muted)]">
                          {targetMeta(item)}
                          {hasAiEdit ? (isShowingRaw ? " · original" : " · AI") : ""}
                        </span>
                      )}
                    </div>
                    {/* Fixed-width action track (edit / copy / AI toggle): Copy
                      and the AI chip stay visible at rest; Fix transcript
                      appears on hover or keyboard focus. Rows that cannot be
                      edited (file transcriptions, synced rows) have no Fix
                      slot; the remaining buttons stay right-aligned. */}
                    <span
                      className={cx(
                        "flex w-[108px] shrink-0 items-center justify-end gap-[4px]",
                        isEditing && "pointer-events-none opacity-0",
                      )}
                    >
                      {isEditing ? null : (
                        <>
                          {isEditable ? (
                            <span className="opacity-0 transition-opacity duration-150 ease-[var(--ease-out-strong)] group-hover:opacity-100 group-focus-within:opacity-100">
                              <button
                                type="button"
                                className={historyActionChip}
                                title="Fix transcript"
                                aria-label={`Fix transcript, ${actionContext}`}
                                onClick={() => {
                                  setEditingId(item.delivery_id);
                                  setDraft(item.transcript);
                                }}
                              >
                                <PencilSimple className="h-[17px] w-[17px]" aria-hidden="true" />
                              </button>
                            </span>
                          ) : null}
                          <button
                            type="button"
                            className={historyActionChip}
                            title="Copy transcript"
                            aria-label={`Copy transcript, ${actionContext}`}
                            onClick={() => copyTranscript(item.delivery_id, shownText)}
                          >
                            {copied ? (
                              <Check
                                className="h-[17px] w-[17px]"
                                weight="bold"
                                aria-hidden="true"
                              />
                            ) : (
                              <Copy className="h-[17px] w-[17px]" aria-hidden="true" />
                            )}
                          </button>
                          {hasAiEdit && (
                            <button
                              type="button"
                              className={historyActionChip}
                              title={isShowingRaw ? "Redo AI polish" : "Undo AI polish"}
                              aria-label={`${isShowingRaw ? "Redo AI polish" : "Undo AI polish"}, ${actionContext}`}
                              onClick={() =>
                                setShowRaw((current) => ({
                                  ...current,
                                  [item.delivery_id]: !current[item.delivery_id],
                                }))
                              }
                            >
                              {isShowingRaw ? (
                                <Sparkle className="h-[17px] w-[17px]" aria-hidden="true" />
                              ) : (
                                <ArrowsCounterClockwise
                                  className="h-[17px] w-[17px]"
                                  aria-hidden="true"
                                />
                              )}
                            </button>
                          )}
                        </>
                      )}
                    </span>
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </div>
    </PageShell>
  );
}
