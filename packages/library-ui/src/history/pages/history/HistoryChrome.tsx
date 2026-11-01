import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  ArrowsCounterClockwise,
  Check,
  Copy,
  Desktop,
  DeviceMobile,
  Laptop,
  Lock,
  PencilSimple,
  Sparkle,
  Trash,
  Watch,
} from "@phosphor-icons/react";
import type { HistoryItem } from "@rightkit/dictation-ui/lib/homeMetrics";
import { isMac } from "@rightkit/dictation-ui/hooks/usePlatform";
import { cx, ui } from "@rightkit/dictation-ui/lib/ui";
import { AppIcon } from "@rightkit/dictation-ui/pages/history/AppIcon";
import {
  cancelledBadge,
  type DateFilter,
  deviceBadgeLabel,
  formatClip,
  historyActionChip,
  historyDevice,
  historyFilterControl,
  historyOutcomeLabel,
  friendlyAppName,
  type OutcomeFilter,
  type PendingConfirm,
  targetDisplay,
  timeLabel,
} from "@rightkit/dictation-ui/pages/history/historyUtils";
import { hostAppName } from "../../../appName";

export type HistoryRetention = "forever" | "7_days" | "30_days" | "90_days" | "1_year";

const OUTCOME_TABS: { key: OutcomeFilter; label: string }[] = [
  { key: "dictations", label: "Dictations" },
  { key: "cancelled", label: "Cancelled" },
];

// Dictations/Cancelled as a quiet segmented control (the mockup's `.seg`
// grammar): one neutral track, the active segment raised — no ember. Keeps
// the tablist semantics and the ArrowLeft/ArrowRight roving tabindex.
function HistoryOutcomeTabs({
  outcomeFilter,
  setOutcomeFilter,
}: {
  outcomeFilter: OutcomeFilter;
  setOutcomeFilter: (value: OutcomeFilter) => void;
}) {
  return (
    <div
      role="tablist"
      aria-label="History view"
      className="inline-flex shrink-0 items-center gap-[2px] rounded-[var(--radius-sm)] border border-[color-mix(in_srgb,var(--line),transparent_30%)] bg-[var(--surface-2)] p-[2px]"
    >
      {OUTCOME_TABS.map((tab, index) => {
        const isActive = outcomeFilter === tab.key;
        return (
          <button
            key={tab.key}
            type="button"
            role="tab"
            id={`history-tab-${tab.key}`}
            aria-selected={isActive}
            aria-controls="history-list"
            tabIndex={isActive ? 0 : -1}
            className={cx(
              "cursor-pointer rounded-[4px] border-0 px-[10px] py-[4px] [font:inherit] text-[length:var(--size-sm)] transition-colors duration-150 ease-[var(--ease-out-strong)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]",
              isActive
                ? "bg-[var(--surface-3)] font-semibold text-[var(--paper)] [box-shadow:inset_0_0_0_1px_color-mix(in_srgb,var(--line),transparent_24%)]"
                : "bg-transparent text-[var(--muted)] hover:text-[var(--paper)]",
            )}
            onClick={() => setOutcomeFilter(tab.key)}
            onKeyDown={(event) => {
              if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
              event.preventDefault();
              const next =
                OUTCOME_TABS[
                  (index + (event.key === "ArrowRight" ? 1 : -1) + OUTCOME_TABS.length) %
                    OUTCOME_TABS.length
                ];
              setOutcomeFilter(next.key);
              window.requestAnimationFrame(() =>
                document.getElementById(`history-tab-${next.key}`)?.focus(),
              );
            }}
          >
            {tab.label}
          </button>
        );
      })}
    </div>
  );
}

// The page's filter row: outcome tabs + date/app pop-ups. Source visibility
// and the search field live in the title bar. Retention, Export, Select and
// Delete All sit on the ONE compact line directly under it
// (HistoryActionsLine).
export function HistoryToolbar({
  dateFilter,
  appFilter,
  appOptions,
  outcomeFilter,
  setDateFilter,
  setAppFilter,
  setOutcomeFilter,
}: {
  dateFilter: DateFilter;
  appFilter: string;
  appOptions: string[];
  outcomeFilter: OutcomeFilter;
  setDateFilter: (value: DateFilter) => void;
  setAppFilter: (value: string) => void;
  setOutcomeFilter: (value: OutcomeFilter) => void;
}) {
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-[var(--space-2)]">
      <HistoryOutcomeTabs outcomeFilter={outcomeFilter} setOutcomeFilter={setOutcomeFilter} />
      <select
        data-setting-id="history.date-filter"
        className={cx(historyFilterControl, "w-[110px] shrink-0 truncate")}
        aria-label="Filter history by date"
        value={dateFilter}
        onChange={(e) => setDateFilter(e.target.value as DateFilter)}
      >
        <option value="all">All dates</option>
        <option value="today">Today</option>
        <option value="yesterday">Yesterday</option>
        <option value="week">Last 7 days</option>
      </select>
      <select
        data-setting-id="history.app-filter"
        className={cx(historyFilterControl, "w-[140px] shrink-0 truncate")}
        aria-label="Filter history by app"
        value={appFilter}
        onChange={(e) => setAppFilter(e.target.value)}
      >
        <option value="all">All apps</option>
        {appOptions.map((app) => (
          <option key={app} value={app}>
            {friendlyAppName(app)}
          </option>
        ))}
      </select>
    </div>
  );
}

// Text-button grammar for the one-line actions strip: no border, a real hit
// area, neutral by default. The destructive variant is danger-tinted.
const lineButton =
  "inline-flex h-7 items-center rounded-[var(--radius-sm)] border-0 bg-transparent px-[var(--space-2)] [font:inherit] text-[length:var(--size-sm)] text-[var(--muted-2)] transition-colors duration-150 ease-[var(--ease-out-strong)] hover:bg-[var(--surface-hover)] hover:text-[var(--paper)] disabled:cursor-not-allowed disabled:opacity-45 disabled:hover:bg-transparent disabled:hover:text-[var(--muted-2)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]";
const lineButtonDanger =
  "!text-[var(--danger)] !bg-[color-mix(in_srgb,var(--danger),transparent_90%)] hover:!bg-[color-mix(in_srgb,var(--danger),transparent_80%)] disabled:!bg-transparent";

// The prod-style single line under the filter row: "Keep history for [select]
// · Export… · Select · Delete All…". Select mode swaps the right-hand part
// for the selection cluster. The one Delete-all trigger (HR-HISTORY-011) keeps
// its confirmation dialog as the required second step.
export function HistoryActionsLine({
  retention,
  retentionSaving,
  setRetention,
  exportHistory,
  requestDeleteAll,
  deletableCount,
  isSelecting,
  selectedCount,
  allVisibleSelected,
  toggleSelectAll,
  exportSelected,
  requestDeleteSelected,
  toggleSelecting,
}: {
  retention: HistoryRetention;
  retentionSaving: boolean;
  setRetention: (value: HistoryRetention) => void;
  exportHistory: () => void;
  requestDeleteAll: () => void;
  /** Count of locally-owned (deletable) rows in the current filter/source
   *  scope - what "Delete all" actually removes (HR-HISTORY-011). */
  deletableCount: number;
  isSelecting: boolean;
  selectedCount: number;
  allVisibleSelected: boolean;
  toggleSelectAll: () => void;
  exportSelected: () => void;
  requestDeleteSelected: () => void;
  toggleSelecting: () => void;
}) {
  return (
    <div className="ml-auto flex min-h-11 min-w-0 flex-wrap items-center gap-[var(--space-1)] whitespace-nowrap text-[length:var(--size-sm)] text-[var(--muted)]">
      <span
        data-setting-id="history.keep-history-for"
        className="inline-flex items-center gap-[var(--space-2)]"
      >
        <span>Keep</span>
        <select
          className={historyFilterControl}
          aria-label="Keep history for"
          value={retention}
          disabled={retentionSaving}
          onChange={(event) => setRetention(event.target.value as HistoryRetention)}
        >
          <option value="forever">Forever</option>
          <option value="7_days">7 days</option>
          <option value="30_days">30 days</option>
          <option value="90_days">90 days</option>
          <option value="1_year">1 year</option>
        </select>
      </span>
      {isSelecting ? (
        <span
          data-setting-id="history.select-mode"
          className="inline-flex flex-nowrap items-center gap-x-[var(--space-1)]"
        >
          <span className="px-[var(--space-2)] text-[length:var(--size-xs)]">
            {selectedCount} selected
          </span>
          <button type="button" className={lineButton} onClick={toggleSelectAll}>
            {allVisibleSelected ? "Deselect all" : "Select all"}
          </button>
          <button
            type="button"
            className={lineButton}
            disabled={selectedCount === 0}
            onClick={exportSelected}
          >
            Export selected
          </button>
          <button
            type="button"
            className={cx(lineButton, lineButtonDanger)}
            disabled={selectedCount === 0}
            onClick={requestDeleteSelected}
          >
            Delete selected
          </button>
          <button type="button" className={lineButton} onClick={toggleSelecting}>
            Cancel
          </button>
        </span>
      ) : (
        <>
          <button
            type="button"
            data-setting-id="history.export"
            className={lineButton}
            onClick={exportHistory}
          >
            Export…
          </button>
          <button
            type="button"
            data-setting-id="history.select-mode"
            className={lineButton}
            onClick={toggleSelecting}
          >
            Select
          </button>
          <button
            type="button"
            data-setting-id="history.delete-all"
            className={cx(lineButton, lineButtonDanger)}
            title="Removes this device's transcripts in the current filters. Transcripts from other devices aren't affected."
            disabled={deletableCount === 0}
            onClick={requestDeleteAll}
          >
            Delete All…
          </button>
        </>
      )}
    </div>
  );
}

// Native `<dialog>` focus trap: `showModal()` (not the `open` attribute) is
// what makes the browser own focus containment, Escape-to-cancel, and the
// `::backdrop` layer — a `<dialog open>` alone (the previous implementation)
// stays a plain non-modal block with background history rows still reachable
// by mouse/keyboard while the dialog is up. Mirrors the pattern already used
// for App.tsx's LicenseLifecycleDialog/TranscribeModal. This is conditionally
// rendered by its parent (HistoryImpl), so mount == "should be open".
export function HistoryConfirmDialog({
  pendingConfirm,
  dontAskAgain,
  cancelRef,
  setDontAskAgain,
  onCancel,
  onConfirm,
}: {
  pendingConfirm: PendingConfirm;
  dontAskAgain: boolean;
  cancelRef: React.RefObject<HTMLButtonElement | null>;
  setDontAskAgain: (value: boolean) => void;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    if (!pendingConfirm) return;
    const el = dialogRef.current;
    el?.showModal();
    return () => el?.close();
  }, [pendingConfirm]);

  if (!pendingConfirm) return null;
  const title =
    pendingConfirm.kind === "clear"
      ? "Delete all?"
      : pendingConfirm.kind === "selected"
        ? "Delete selected?"
        : "Delete this transcript?";
  const description =
    pendingConfirm.kind === "clear"
      ? `This removes ${pendingConfirm.count} local transcript${pendingConfirm.count === 1 ? "" : "s"} owned by this device, in the current filters. Transcripts from other devices are not affected.`
      : pendingConfirm.kind === "selected"
        ? `This removes ${pendingConfirm.ids.length} selected transcript${pendingConfirm.ids.length === 1 ? "" : "s"} from local History.`
        : "This removes the selected transcript from local History.";
  return (
    <dialog
      ref={dialogRef}
      aria-labelledby="history-confirm-title"
      className="m-auto w-[calc(100%-32px)] max-w-[420px] rounded-[var(--radius-md)] border border-[var(--glass-edge)] bg-[var(--surface-1)] p-[var(--space-5)] text-[var(--paper)] [box-shadow:0_20px_70px_rgba(0,0,0,0.42),inset_0_1px_0_var(--glass-highlight)] backdrop:bg-[rgba(0,0,0,0.38)] backdrop:[backdrop-filter:blur(10px)]"
      onCancel={(e) => {
        // Escape fires the native `cancel` event — route it through the same
        // React state update the Cancel button uses instead of letting the
        // dialog close with pendingConfirm still set.
        e.preventDefault();
        onCancel();
      }}
      onClick={(e) => {
        // A click landing on the dialog element itself (not a descendant) is
        // a click on the native `::backdrop` — treat it as Cancel, same as
        // the old manual backdrop div's implicit outside-click dismiss.
        if (e.target === dialogRef.current) onCancel();
      }}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          onCancel();
        }
      }}
    >
      <h2
        id="history-confirm-title"
        className="m-0 [font-family:var(--font-display)] text-[length:var(--size-xl)] text-[var(--paper)]"
      >
        {title}
      </h2>
      <p className="m-0 mt-[var(--space-2)] text-[length:var(--size-sm)] text-[var(--muted)]">
        {description}
      </p>
      {pendingConfirm.kind === "delete" ? (
        <label className="mt-[var(--space-4)] flex min-h-10 cursor-pointer items-center gap-[var(--space-3)] rounded-[var(--radius-sm)] text-[length:var(--size-sm)] text-[var(--paper)]">
          <input
            type="checkbox"
            className="h-5 w-5 accent-[var(--accent)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]"
            checked={dontAskAgain}
            aria-label="Don't ask again before deleting transcripts"
            onChange={(e) => setDontAskAgain(e.target.checked)}
          />
          Don't ask again before deleting transcripts
        </label>
      ) : null}
      <div className="mt-[var(--space-5)] flex justify-end gap-[var(--space-2)]">
        <button
          ref={cancelRef}
          type="button"
          className={cx(ui.button, ui.buttonGhost)}
          onClick={onCancel}
        >
          Cancel
        </button>
        <button type="button" className={cx(ui.button, ui.buttonDanger)} onClick={onConfirm}>
          {pendingConfirm.kind === "clear"
            ? "Delete all"
            : pendingConfirm.kind === "selected"
              ? "Delete selected"
              : "Delete"}
        </button>
      </div>
    </dialog>
  );
}

// One transcript clip-measurement hub shared by every mounted HistoryRow: a
// single ResizeObserver plus a single window "resize" listener replace the
// per-row observer + listener pair each row used to mount (N rows meant N
// window listeners and N observers). A row registers its transcript element
// and measure callback; the hub fans element resizes out to just that row's
// callback and broadcasts window resizes to all of them. The first
// subscription creates both resources and the last unsubscribe tears them
// down, so leaving the History page drops the listener entirely.
const transcriptMeasures = new Map<Element, () => void>();
let transcriptObserver: ResizeObserver | null = null;

function measureAllTranscripts() {
  transcriptMeasures.forEach((measure) => { measure(); });
}

function subscribeTranscriptMeasure(element: Element, measure: () => void) {
  transcriptMeasures.set(element, measure);
  if (transcriptMeasures.size === 1) {
    if (typeof ResizeObserver !== "undefined") {
      transcriptObserver = new ResizeObserver((entries) => {
        for (const entry of entries) {
          transcriptMeasures.get(entry.target)?.();
        }
      });
    }
    window.addEventListener("resize", measureAllTranscripts);
  }
  transcriptObserver?.observe(element);
  return () => {
    transcriptMeasures.delete(element);
    transcriptObserver?.unobserve(element);
    if (transcriptMeasures.size === 0) {
      transcriptObserver?.disconnect();
      transcriptObserver = null;
      window.removeEventListener("resize", measureAllTranscripts);
    }
  };
}

// The source-device glyph column (right-aligned, fixed width) — laptop/desktop for this machine, phone for
// iPhone-origin rows, watch for Watch-origin rows (provenance the old
// deviceBadge pill carried as text; the glyph keeps it readable at a glance,
// with the full device name on the tooltip/label).
function HistoryDeviceGlyph({ item }: { item: HistoryItem }) {
  const device = historyDevice(item);
  const deviceName = deviceBadgeLabel(item) ?? (isMac() ? "this Mac" : "this PC");
  const Icon =
    device === "iphone" ? DeviceMobile : device === "watch" ? Watch : isMac() ? Laptop : Desktop;
  return (
    <span
      role="img"
      aria-label={`Recorded on ${deviceName}`}
      title={`Recorded on ${deviceName}`}
      className="mt-[2px] grid h-[20px] w-[24px] shrink-0 place-items-center justify-self-end text-[var(--muted-2)]"
    >
      <Icon aria-hidden="true" className="h-[15px] w-[15px]" />
    </span>
  );
}

// The trailing action-button cluster (edit/copy/AI-toggle/delete), split out
// of HistoryRow for size — this is one branch of that row's action area
// (the other being the Save/Copy-fix/Cancel buttons shown inside the
// edit-bar while editing).
function HistoryRowActions({
  item,
  actionContext,
  isEditing,
  isCopied,
  showRaw,
  draft,
  isDeletable,
  onCancelCorrection,
  onSaveCorrection,
  onCopyCorrection,
  onCopyItem,
  onToggleShowRaw,
  onRequestDelete,
  onStartCorrection,
}: {
  item: HistoryItem;
  actionContext: string;
  isEditing: boolean;
  isCopied: boolean;
  showRaw: boolean;
  draft: string;
  isDeletable: boolean;
  onCancelCorrection: () => void;
  onSaveCorrection: (item: HistoryItem) => void;
  onCopyCorrection: (item: HistoryItem) => void;
  onCopyItem: (item: HistoryItem) => void;
  onToggleShowRaw: (deliveryId: string) => void;
  onRequestDelete: (item: HistoryItem) => void;
  onStartCorrection: (item: HistoryItem) => void;
}) {
  const hasAiEdit = !!item.raw_transcript && item.raw_transcript !== item.transcript;
  return isEditing ? (
    <>
      <button
        type="button"
        className={cx(ui.button, ui.buttonGhost, ui.buttonCompact)}
        onClick={onCancelCorrection}
      >
        Cancel
      </button>
      <button
        type="button"
        className={cx(ui.button, ui.buttonGhost, ui.buttonCompact)}
        disabled={!draft.trim()}
        onClick={() => onCopyCorrection(item)}
      >
        Copy fix
      </button>
      <button
        type="button"
        className={cx(ui.button, ui.buttonCompact)}
        disabled={!draft.trim()}
        onClick={() => onSaveCorrection(item)}
      >
        Save fix
      </button>
    </>
  ) : (
    <>
      {/* HR-HISTORY-006/007: a foreign-origin (iPhone/Watch) row is
          view-only here — no content edit, no delete affordance. Copy
          and the AI-polish toggle stay available since neither mutates
          the synced record. */}
      {isDeletable ? (
        <button
          type="button"
          className={historyActionChip}
          title="Fix transcript"
          aria-label={`Fix transcript, ${actionContext}`}
          onClick={() => onStartCorrection(item)}
        >
          <PencilSimple aria-hidden="true" className="h-[17px] w-[17px]" />
        </button>
      ) : null}
      <button
        type="button"
        className={historyActionChip}
        title="Copy transcript"
        aria-label={`Copy transcript, ${actionContext}`}
        onClick={() => onCopyItem(item)}
      >
        {isCopied ? (
          <Check aria-hidden="true" className="h-[17px] w-[17px]" weight="bold" />
        ) : (
          <Copy aria-hidden="true" className="h-[17px] w-[17px]" />
        )}
      </button>
      {/* Only rendered when an AI edit exists — which requires Pro, so
          Free users never see the chip at all. */}
      {hasAiEdit && (
        <button
          type="button"
          className={historyActionChip}
          title={showRaw ? "Redo AI polish" : "Undo AI polish"}
          aria-label={`${showRaw ? "Redo AI polish" : "Undo AI polish"}, ${actionContext}`}
          onClick={() => {
            onToggleShowRaw(item.delivery_id);
          }}
        >
          {showRaw ? (
            <Sparkle aria-hidden="true" className="h-[17px] w-[17px]" />
          ) : (
            <ArrowsCounterClockwise aria-hidden="true" className="h-[17px] w-[17px]" />
          )}
        </button>
      )}
      {isDeletable ? (
        <button
          type="button"
          className={cx(
            historyActionChip,
            "hover:bg-[color-mix(in_srgb,var(--danger),transparent_88%)] hover:text-[var(--danger)]",
          )}
          title="Delete transcript"
          aria-label={`Delete transcript, ${actionContext}`}
          onClick={() => onRequestDelete(item)}
        >
          <Trash aria-hidden="true" className="h-[17px] w-[17px]" />
        </button>
      ) : null}
    </>
  );
}

// Memoized per-row component so a state tick elsewhere in HistoryPage (search,
// notice, editingId on a DIFFERENT row, etc.) doesn't re-render every row in the
// list. Cost of that re-render grows linearly with history size otherwise.
export const HistoryRow = memo(function HistoryRow({
  item,
  actionContext,
  isEditing,
  isExpanded,
  isCopied,
  showRaw,
  draft,
  isSelecting,
  isSelected,
  isDeletable,
  onSetDraft,
  onToggleExpand,
  onStartCorrection,
  onCancelCorrection,
  onSaveCorrection,
  onCopyCorrection,
  onCopyItem,
  onToggleShowRaw,
  onRequestDelete,
  onToggleSelect,
}: {
  item: HistoryItem;
  actionContext: string;
  isEditing: boolean;
  isExpanded: boolean;
  isCopied: boolean;
  showRaw: boolean;
  draft: string;
  /** Selection mode is active (HR-HISTORY-009). */
  isSelecting: boolean;
  isSelected: boolean;
  /** HR-HISTORY-006/007: false for a foreign-origin (iPhone/Watch) row —
   *  view-only here, no delete affordance and not selectable for deletion. */
  isDeletable: boolean;
  onSetDraft: (value: string) => void;
  onToggleExpand: (deliveryId: string) => void;
  onStartCorrection: (item: HistoryItem) => void;
  onCancelCorrection: () => void;
  onSaveCorrection: (item: HistoryItem) => void;
  onCopyCorrection: (item: HistoryItem) => void;
  onCopyItem: (item: HistoryItem) => void;
  onToggleShowRaw: (deliveryId: string) => void;
  onRequestDelete: (item: HistoryItem) => void;
  onToggleSelect: (deliveryId: string) => void;
}) {
  const hasAiEdit = !!item.raw_transcript && item.raw_transcript !== item.transcript;
  const shownText = showRaw && item.raw_transcript ? item.raw_transcript : item.transcript;
  const isCancelled = item.outcome.kind === "cancelled";
  // The meta line's "app · window" half: a friendly app name once, and the
  // window title only when it adds information (see targetDisplay).
  const { app: appName, windowTitle } = targetDisplay(item);
  const windowSuffix = windowTitle ? ` · ${windowTitle}` : "";
  const transcriptRef = useRef<HTMLButtonElement>(null);
  const [isTranscriptClipped, setIsTranscriptClipped] = useState(false);

  useLayoutEffect(() => {
    if (isExpanded || isEditing) return;
    const transcript = transcriptRef.current;
    if (!transcript) return;
    const measure = () => {
      if (shownText.length === 0) {
        setIsTranscriptClipped(false);
        return;
      }
      setIsTranscriptClipped(transcript.scrollHeight > transcript.clientHeight + 1);
    };
    measure();
    // Shared surface above: one observer + one window listener for all rows.
    return subscribeTranscriptMeasure(transcript, measure);
  }, [isEditing, isExpanded, shownText]);

  const canToggleTranscript = isExpanded || isTranscriptClipped;

  return (
    <div
      className={cx(
        // The mockup's `.hrow`: time | app tile | transcript (+meta) | source
        // device glyph | hover actions — one row inside the day's inset group,
        // separated by a hairline that the last row drops.
        "group flex min-w-0 items-start gap-[var(--space-3)] border-b border-[color-mix(in_srgb,var(--line),transparent_72%)] px-[var(--space-3)] py-[10px] transition-colors duration-150 ease-[var(--ease-out-strong)] last:border-b-0 hover:bg-[var(--surface-hover)]",
        isEditing && "bg-[var(--surface-hover)]",
      )}
    >
      {isSelecting ? (
        isDeletable ? (
          <button
            type="button"
            className="mt-[2px] grid h-5 w-5 shrink-0 place-items-center rounded-[4px] border-0 bg-transparent p-0"
            aria-pressed={isSelected}
            aria-label={`Select transcript, ${actionContext}`}
            onClick={() => onToggleSelect(item.delivery_id)}
          >
            <span
              className={cx(
                "grid h-4 w-4 place-items-center rounded-[4px] border",
                isSelected
                  ? "border-[var(--accent)] bg-[var(--accent)]"
                  : "border-[color-mix(in_srgb,var(--line),transparent_20%)]",
              )}
            >
              {isSelected ? (
                <Check aria-hidden="true" className="h-3 w-3 text-[var(--hr-ink)]" weight="bold" />
              ) : null}
            </span>
          </button>
        ) : (
          <span
            role="img"
            className="mt-[2px] grid h-5 w-5 shrink-0 place-items-center text-[var(--muted)]"
            title="Foreign-device transcript is read-only here"
            aria-label={`Transcript from another device is read-only, ${actionContext}`}
          >
            <Lock aria-hidden="true" className="h-3.5 w-3.5" />
          </span>
        )
      ) : null}
      <span className="mt-[2px] w-[62px] shrink-0 whitespace-nowrap [font-family:var(--font-mono)] text-[length:var(--size-xs)] text-[var(--muted-2)] [font-variant-numeric:tabular-nums]">
        {timeLabel(item.delivered_at_ms)}
      </span>
      <AppIcon item={item} size={28} className="mt-[1px]" />
      <div className="min-w-0 flex-1">
        {isEditing ? (
          <textarea
            className={cx(ui.input, "min-h-[92px] w-full resize-y")}
            aria-label="Correct transcript"
            value={draft}
            onChange={(e) => onSetDraft(e.target.value)}
          />
        ) : (
          <button
            ref={transcriptRef}
            type="button"
            title={
              canToggleTranscript
                ? isExpanded
                  ? "Collapse transcript"
                  : "Expand transcript"
                : undefined
            }
            aria-expanded={canToggleTranscript ? isExpanded : undefined}
            disabled={!canToggleTranscript}
            className={cx(
              "m-0 w-full whitespace-pre-wrap break-words rounded-[var(--radius-sm)] border-0 bg-transparent p-0 text-left [font:inherit] text-[length:var(--size-sm)] leading-[1.42] text-[var(--paper)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)] disabled:cursor-text disabled:opacity-100",
              canToggleTranscript && "cursor-pointer",
              isExpanded ? "block" : "line-clamp-2",
            )}
            onClick={() => {
              if (canToggleTranscript) onToggleExpand(item.delivery_id);
            }}
          >
            {shownText}
          </button>
        )}
        {isEditing ? (
          <div className="mt-[var(--space-2)] flex flex-wrap items-center gap-[var(--space-2)]">
            <span className="min-w-[180px] flex-1 text-[length:var(--size-xs)] text-[var(--muted)]">
              Correcting teaches {hostAppName()}. The fixed text replaces this entry.
            </span>
            <HistoryRowActions
              item={item}
              actionContext={actionContext}
              isEditing={isEditing}
              isCopied={isCopied}
              showRaw={showRaw}
              draft={draft}
              isDeletable={isDeletable}
              onCancelCorrection={onCancelCorrection}
              onSaveCorrection={onSaveCorrection}
              onCopyCorrection={onCopyCorrection}
              onCopyItem={onCopyItem}
              onToggleShowRaw={onToggleShowRaw}
              onRequestDelete={onRequestDelete}
              onStartCorrection={onStartCorrection}
            />
          </div>
        ) : (
          <div className="mt-[5px] flex min-w-0 items-center gap-[6px]">
            {isCancelled ? <span className={cancelledBadge}>Cancelled</span> : null}
            <span className="block truncate text-[length:var(--size-xs)] text-[var(--muted)] [font-variant-numeric:tabular-nums]">
              {appName} · {historyOutcomeLabel(item)}
              {windowSuffix}
              {formatClip(item.recording_ms) ? ` · ${formatClip(item.recording_ms)}` : ""}
              {hasAiEdit ? (showRaw ? " · original" : " · AI") : ""}
            </span>
          </div>
        )}
      </div>
      {/* Fixed-width action track: it holds its width whether the row has one
          action or four, so the device glyph and the time/icon/text columns
          sit in the same place on every row. Actions reveal on hover or
          keyboard focus for every row alike. */}
      <div
        className={cx(
          "flex w-[140px] shrink-0 flex-nowrap items-center justify-end gap-[4px] transition-opacity duration-150 ease-[var(--ease-out-strong)]",
          isSelecting || isEditing
            ? "pointer-events-none opacity-0"
            : "opacity-0 group-hover:opacity-100 group-focus-within:opacity-100",
        )}
      >
        {/* Editing renders its Save/Copy-fix/Cancel buttons inside the
            edit-bar above; selection mode has the row checkbox. */}
        {isSelecting || isEditing ? null : (
          <HistoryRowActions
            item={item}
            actionContext={actionContext}
            isEditing={isEditing}
            isCopied={isCopied}
            showRaw={showRaw}
            draft={draft}
            isDeletable={isDeletable}
            onCancelCorrection={onCancelCorrection}
            onSaveCorrection={onSaveCorrection}
            onCopyCorrection={onCopyCorrection}
            onCopyItem={onCopyItem}
            onToggleShowRaw={onToggleShowRaw}
            onRequestDelete={onRequestDelete}
            onStartCorrection={onStartCorrection}
          />
        )}
      </div>
      {/* Device glyph: its own fixed column at the row's right edge, so it sits
          in the same place on every row instead of floating beside the
          hover-only action track. */}
      <HistoryDeviceGlyph item={item} />
    </div>
  );
});

export const HistoryNotice = memo(function HistoryNotice({ notice }: { notice: string }) {
  return (
    <>
      <div aria-live="polite" className="sr-only">
        {notice}
      </div>
      {notice ? (
        <output className="fixed right-[var(--space-5)] top-[calc(var(--titlebar-height)+var(--space-4))] z-40 rounded-[var(--radius-sm)] border border-[var(--glass-edge)] bg-[var(--surface-2)] px-[var(--space-3)] py-[var(--space-2)] text-[length:var(--size-sm)] text-[var(--paper)] [box-shadow:var(--surface-shadow-hover)]">
          {notice}
        </output>
      ) : null}
    </>
  );
});
