import { createContext, memo, useContext, useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  ArrowsCounterClockwise,
  Check,
  Copy,
  Lock,
  PencilSimple,
  Sparkle,
  Trash,
} from "@phosphor-icons/react";
import { Button, cx } from "@rightkit/react-shell/react";
import type { LibraryGroup, LibraryRow, LibrarySource, LibraryVariant } from "../contract.js";
import { OriginIcon } from "./OriginIcon.js";
import {
  badgeClass,
  rowInputClass,
  type DateFilter,
  filterControl,
  formatClip,
  friendlyAppName,
  type PendingConfirm,
  rowActionChip,
  timeLabel,
} from "./format.js";

/** Words the list uses for its rows and actions. Defaults read "item"/"items". */
export interface LibraryLabels {
  /** One row, lower case ("transcript"). */
  item: string;
  /** Several rows, lower case ("transcripts"). */
  items: string;
  /** Heading of the area ("History"). */
  area: string;
  /** Toggle between `text` and `altText`: [show alt, show text, tag on text, tag on alt]. */
  alt: [string, string, string, string];
  /** Line shown while a row is edited. */
  editHint?: string;
}

export const DEFAULT_LIBRARY_LABELS: LibraryLabels = {
  item: "item",
  items: "items",
  area: "Library",
  alt: ["Show original", "Show edited", "edited", "original"],
};

export interface LibraryContextValue {
  source: LibrarySource;
  groups: readonly LibraryGroup[];
  variants: readonly LibraryVariant[];
  labels: LibraryLabels;
}

export const LibraryContext = createContext<LibraryContextValue | null>(null);

export function useLibraryContext(): LibraryContextValue {
  const value = useContext(LibraryContext);
  if (!value) throw new Error("@rightkit/library-ui: list parts must render inside LibraryScreen");
  return value;
}

const cap = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);



// The variants (e.g. Dictations/Cancelled) as a quiet segmented control (the mockup's `.seg`
// grammar): one neutral track, the active segment raised — no ember. Keeps
// the tablist semantics and the ArrowLeft/ArrowRight roving tabindex.
function VariantTabs({
  outcomeFilter,
  setOutcomeFilter,
}: {
  outcomeFilter: string;
  setOutcomeFilter: (value: string) => void;
}) {
  const { variants: OUTCOME_TABS, labels } = useLibraryContext();
  if (OUTCOME_TABS.length < 2) return null;
  return (
    <div
      role="tablist"
      aria-label={`${labels.area} view`}
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

/** One option of the origin filter: `value` is sent as `LibraryQuery.origin`, `label` is shown. */
export interface LibraryFilterOption {
  value: string;
  label: string;
}

// The page's filter row: outcome tabs + date/app pop-ups. Source visibility
// and the search field live in the title bar. Retention, Export, Select and
// Delete All sit on the ONE compact line directly under it
// (HistoryActionsLine).
export function LibraryToolbar({
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
  /** Origin filter options; a bare string is both the value and (through `friendlyAppName`) the label. */
  appOptions: ReadonlyArray<string | LibraryFilterOption>;
  outcomeFilter: string;
  setDateFilter: (value: DateFilter) => void;
  setAppFilter: (value: string) => void;
  setOutcomeFilter: (value: string) => void;
}) {
  const { labels } = useLibraryContext();
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-[var(--space-2)]">
      <VariantTabs outcomeFilter={outcomeFilter} setOutcomeFilter={setOutcomeFilter} />
      <select
        data-setting-id="history.date-filter"
        className={cx(filterControl, "w-[110px] shrink-0 truncate")}
        aria-label={`Filter ${labels.area.toLowerCase()} by date`}
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
        className={cx(filterControl, "w-[140px] shrink-0 truncate")}
        aria-label={`Filter ${labels.area.toLowerCase()} by app`}
        value={appFilter}
        onChange={(e) => setAppFilter(e.target.value)}
      >
        <option value="all">All apps</option>
        {appOptions.map((app) => {
          const option = typeof app === "string" ? { value: app, label: friendlyAppName(app) } : app;
          return (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          );
        })}
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
export function LibraryActionsLine({
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
  retention: string;
  retentionSaving: boolean;
  setRetention: (value: string) => void;
  exportHistory: (() => void) | undefined;
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
  const { source, labels } = useLibraryContext();
  return (
    <div className="ml-auto flex min-h-11 min-w-0 flex-wrap items-center gap-[var(--space-1)] whitespace-nowrap text-[length:var(--size-sm)] text-[var(--muted)]">
      {source.retention ? (
      <span
        data-setting-id="history.keep-history-for"
        className="inline-flex items-center gap-[var(--space-2)]"
      >
        <span>Keep</span>
        <select
          className={filterControl}
          aria-label={`Keep ${labels.area.toLowerCase()} for`}
          value={retention}
          disabled={retentionSaving}
          onChange={(event) => setRetention(event.target.value)}
        >
          {source.retention.options.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
      </span>
      ) : null}
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
          {source.saveText ? (
          <button
            type="button"
            className={lineButton}
            disabled={selectedCount === 0}
            onClick={exportSelected}
          >
            Export selected
          </button>
          ) : null}
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
          {exportHistory ? (
          <button
            type="button"
            data-setting-id="history.export"
            className={lineButton}
            onClick={exportHistory}
          >
            Export…
          </button>
          ) : null}
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
            title={`Removes the ${labels.items} this device owns in the current filters. ${cap(labels.items)} from other devices aren't affected.`}
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
export function LibraryConfirmDialog({
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

  const { labels } = useLibraryContext();
  if (!pendingConfirm) return null;
  const title =
    pendingConfirm.kind === "clear"
      ? "Delete all?"
      : pendingConfirm.kind === "selected"
        ? "Delete selected?"
        : `Delete this ${labels.item}?`;
  const description =
    pendingConfirm.kind === "clear"
      ? `This removes ${pendingConfirm.count} local ${pendingConfirm.count === 1 ? labels.item : labels.items} owned by this device, in the current filters. ${cap(labels.items)} from other devices are not affected.`
      : pendingConfirm.kind === "selected"
        ? `This removes ${pendingConfirm.ids.length} selected ${pendingConfirm.ids.length === 1 ? labels.item : labels.items} from local ${labels.area}.`
        : `This removes the selected ${labels.item} from local ${labels.area}.`;
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
            aria-label={`Don't ask again before deleting ${labels.items}`}
            onChange={(e) => setDontAskAgain(e.target.checked)}
          />
          Don't ask again before deleting {labels.items}
        </label>
      ) : null}
      <div className="mt-[var(--space-5)] flex justify-end gap-[var(--space-2)]">
        <Button ref={cancelRef} variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button variant="danger" onClick={onConfirm}>
          {pendingConfirm.kind === "clear"
            ? "Delete all"
            : pendingConfirm.kind === "selected"
              ? "Delete selected"
              : "Delete"}
        </Button>
      </div>
    </dialog>
  );
}

// One transcript clip-measurement hub shared by every mounted row: a
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

// The group glyph column (right-aligned, fixed width): the row's group icon (e.g. the device it was
// recorded on), with the group name on the tooltip/label. Hosts without groups get no column.
function GroupGlyph({ row }: { row: LibraryRow }) {
  const { groups } = useLibraryContext();
  if (groups.length === 0) return null;
  const group = groups.find((g) => g.key === row.group) ?? groups.find((g) => g.local);
  if (!group) return <span aria-hidden="true" className="mt-[2px] h-[20px] w-[24px] shrink-0" />;
  return (
    <span
      role="img"
      aria-label={`From ${group.label}`}
      title={`From ${group.label}`}
      className="mt-[2px] grid h-[20px] w-[24px] shrink-0 place-items-center justify-self-end text-[var(--muted-2)] [&>svg]:h-[15px] [&>svg]:w-[15px]"
    >
      {group.icon}
    </span>
  );
}

// The trailing action-button cluster (edit/copy/AI-toggle/delete), split out
// of HistoryRow for size — this is one branch of that row's action area
// (the other being the Save/Copy-fix/Cancel buttons shown inside the
// edit-bar while editing).
function RowActions({
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
  item: LibraryRow;
  actionContext: string;
  isEditing: boolean;
  isCopied: boolean;
  showRaw: boolean;
  draft: string;
  isDeletable: boolean;
  onCancelCorrection: () => void;
  onSaveCorrection: (item: LibraryRow) => void;
  onCopyCorrection: (item: LibraryRow) => void;
  onCopyItem: (item: LibraryRow) => void;
  onToggleShowRaw: (id: string) => void;
  onRequestDelete: (item: LibraryRow) => void;
  onStartCorrection: (item: LibraryRow) => void;
}) {
  const { labels, source } = useLibraryContext();
  const hasAiEdit = !!item.altText && item.altText !== item.text;
  return isEditing ? (
    <>
      <Button variant="ghost" size="sm" onClick={onCancelCorrection}>
        Cancel
      </Button>
      <Button variant="ghost" size="sm" disabled={!draft.trim()} onClick={() => onCopyCorrection(item)}>
        Copy fix
      </Button>
      <Button size="sm" disabled={!draft.trim()} onClick={() => onSaveCorrection(item)}>
        Save fix
      </Button>
    </>
  ) : (
    <>
      {/* HR-HISTORY-006/007: a foreign-origin (iPhone/Watch) row is
          view-only here — no content edit, no delete affordance. Copy
          and the AI-polish toggle stay available since neither mutates
          the synced record. */}
      {isDeletable && source.update ? (
        <button
          type="button"
          className={rowActionChip}
          title={`Fix ${labels.item}`}
          aria-label={`Fix ${labels.item}, ${actionContext}`}
          onClick={() => onStartCorrection(item)}
        >
          <PencilSimple aria-hidden="true" className="h-[17px] w-[17px]" />
        </button>
      ) : null}
      <button
        type="button"
        className={rowActionChip}
        title={`Copy ${labels.item}`}
        aria-label={`Copy ${labels.item}, ${actionContext}`}
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
          className={rowActionChip}
          title={showRaw ? labels.alt[1] : labels.alt[0]}
          aria-label={`${showRaw ? labels.alt[1] : labels.alt[0]}, ${actionContext}`}
          onClick={() => {
            onToggleShowRaw(item.id);
          }}
        >
          {showRaw ? (
            <Sparkle aria-hidden="true" className="h-[17px] w-[17px]" />
          ) : (
            <ArrowsCounterClockwise aria-hidden="true" className="h-[17px] w-[17px]" />
          )}
        </button>
      )}
      {isDeletable && source.remove ? (
        <button
          type="button"
          className={cx(
            rowActionChip,
            "hover:bg-[color-mix(in_srgb,var(--danger),transparent_88%)] hover:text-[var(--danger)]",
          )}
          title={`Delete ${labels.item}`}
          aria-label={`Delete ${labels.item}, ${actionContext}`}
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
export const LibraryRowView = memo(function LibraryRowView({
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
  item: LibraryRow;
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
  onToggleExpand: (id: string) => void;
  onStartCorrection: (item: LibraryRow) => void;
  onCancelCorrection: () => void;
  onSaveCorrection: (item: LibraryRow) => void;
  onCopyCorrection: (item: LibraryRow) => void;
  onCopyItem: (item: LibraryRow) => void;
  onToggleShowRaw: (id: string) => void;
  onRequestDelete: (item: LibraryRow) => void;
  onToggleSelect: (id: string) => void;
}) {
  const hasAiEdit = !!item.altText && item.altText !== item.text;
  const shownText = showRaw && item.altText ? item.altText : item.text;
  // The meta line's "app · window" half: a friendly app name once, and the
  const { labels } = useLibraryContext();
  const appName = item.origin?.label ?? "";
  const windowTitle = item.origin?.detail ?? "";
  const windowSuffix = windowTitle ? ` · ${windowTitle}` : "";
  const { source } = useLibraryContext();
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
            aria-label={`Select ${labels.item}, ${actionContext}`}
            onClick={() => onToggleSelect(item.id)}
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
            title={`This ${labels.item} is read-only here`}
            aria-label={`${cap(labels.item)} is read-only, ${actionContext}`}
          >
            <Lock aria-hidden="true" className="h-3.5 w-3.5" />
          </span>
        )
      ) : null}
      <span className="mt-[2px] w-[62px] shrink-0 whitespace-nowrap [font-family:var(--font-mono)] text-[length:var(--size-xs)] text-[var(--muted-2)] [font-variant-numeric:tabular-nums]">
        {timeLabel(item.timestampMs)}
      </span>
      <OriginIcon origin={item.origin} source={source} size={28} className="mt-[1px]" />
      <div className="min-w-0 flex-1">
        {isEditing ? (
          <textarea
            className={cx(rowInputClass, "min-h-[92px] w-full resize-y")}
            aria-label={`Correct ${labels.item}`}
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
                  ? "Collapse"
                  : "Expand"
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
              if (canToggleTranscript) onToggleExpand(item.id);
            }}
          >
            {shownText}
          </button>
        )}
        {isEditing ? (
          <div className="mt-[var(--space-2)] flex flex-wrap items-center gap-[var(--space-2)]">
            <span className="min-w-[180px] flex-1 text-[length:var(--size-xs)] text-[var(--muted)]">
              {labels.editHint ?? "The fixed text replaces this entry."}
            </span>
            <RowActions
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
            {item.badges?.map((badge) => (
              <span key={badge} className={badgeClass}>
                {badge}
              </span>
            ))}
            <span className="block truncate text-[length:var(--size-xs)] text-[var(--muted)] [font-variant-numeric:tabular-nums]">
              {[appName, item.status].filter(Boolean).join(" · ")}
              {windowSuffix}
              {formatClip(item.durationMs) ? ` · ${formatClip(item.durationMs)}` : ""}
              {hasAiEdit ? (showRaw ? ` · ${labels.alt[3]}` : ` · ${labels.alt[2]}`) : ""}
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
          <RowActions
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
      <GroupGlyph row={item} />
    </div>
  );
});

export const LibraryNotice = memo(function LibraryNotice({ notice }: { notice: string }) {
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
