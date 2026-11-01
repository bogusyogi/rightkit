import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent, ReactNode } from "react";
import type { SessionRow } from "@rightkit/agent-ui-core/lib/types";
import SessionStatusDot from "./SessionStatusDot";

export interface VirtualSessionDividerRow {
  key: string;
  kind: "divider";
  label: string;
}

export interface VirtualSessionEntryRow {
  key?: string;
  kind: "session";
  branchStem?: string;
  session: SessionRow;
}

export type VirtualSessionListRow = VirtualSessionDividerRow | VirtualSessionEntryRow;

export interface VirtualSessionListProps {
  activeSessionId: string | null;
  /** Render every session row as a three-line inbox card. */
  card?: boolean;
  className?: string;
  /** Hover-revealed control for date dividers (the group-level "+"). */
  dividerAction?: ReactNode;
  rows: VirtualSessionListRow[];
  onArchiveSession?: (sessionId: string) => void;
  onBranchSession?: (sessionId: string, profile?: string) => void;
  onDeleteSession: (sessionId: string) => void;
  onResumeSession?: (sessionId: string) => void;
  onSelectSession: (sessionId: string) => void;
  onTogglePin?: (sessionId: string) => void;
  onToggleUnread?: (sessionId: string) => void;
  /** Treat every row as pinned; individual `pinned`/`pin` tags are also honored. */
  pinned?: boolean;
  showProfileTags?: boolean;
  /** Enables the native drag seam used by the sidebar's reorder owner. */
  sortable?: boolean;
  /** Called when a sortable row starts dragging. */
  onStartReorder?: (sessionId: string) => void;
  density?: "comfortable" | "compact";
}

// These estimates match the Hermes donor's measured-list contract: dividers are
// short, normal rows are compact, and inbox cards reserve space for a preview.
const CARD_ROW_ESTIMATE_PX = 66;
const COMPACT_ROW_ESTIMATE_PX = 34;
const COMFORTABLE_ROW_ESTIMATE_PX = 42;
const DIVIDER_ESTIMATE_PX = 28;
const OVERSCAN_ROWS = 12;

interface VirtualItem {
  index: number;
  start: number;
  size: number;
}

/**
 * A dependency-free port of Hermes' measured absolute-row virtual list.
 * TanStack's adapter is not a the host app dependency, so the browser-facing
 * mechanics live here while the daemon remains the source of session state.
 */
export function VirtualSessionList({
  activeSessionId,
  card = false,
  className,
  density = "comfortable",
  dividerAction,
  onArchiveSession,
  onBranchSession,
  onDeleteSession,
  onResumeSession,
  onSelectSession,
  onStartReorder,
  onTogglePin,
  onToggleUnread,
  pinned = false,
  rows: listRows,
  showProfileTags = false,
  sortable = false,
}: VirtualSessionListProps) {
  const scrollerRef = useRef<HTMLDivElement | null>(null);
  const measuredSizes = useRef(new Map<string, number>());
  const observers = useRef(new Map<string, ResizeObserver>());
  const listRowsRef = useRef(listRows);
  listRowsRef.current = listRows;
  const [viewport, setViewport] = useState({ height: 600, scrollTop: 0 });
  const [measurementVersion, setMeasurementVersion] = useState(0);

  const rowKeys = useMemo(() => listRows.map(rowKey), [listRows]);
  const estimateSize = useCallback(
    (row: VirtualSessionListRow) => {
      if (row.kind === "divider") return DIVIDER_ESTIMATE_PX;
      if (card) return CARD_ROW_ESTIMATE_PX;
      return density === "compact" ? COMPACT_ROW_ESTIMATE_PX : COMFORTABLE_ROW_ESTIMATE_PX;
    },
    [card, density],
  );

  useEffect(() => {
    const validKeys = new Set(rowKeys);
    for (const key of measuredSizes.current.keys()) {
      if (!validKeys.has(key)) measuredSizes.current.delete(key);
    }
    setMeasurementVersion((version) => version + 1);
  }, [rowKeys]);

  useEffect(() => {
    const element = scrollerRef.current;
    if (!element) return;

    const updateViewport = () => {
      setViewport({
        height: element.clientHeight || 600,
        scrollTop: element.scrollTop,
      });
    };

    updateViewport();
    element.addEventListener("scroll", updateViewport, { passive: true });
    let resizeObserver: ResizeObserver | undefined;
    if (typeof ResizeObserver !== "undefined") {
      resizeObserver = new ResizeObserver(updateViewport);
      resizeObserver.observe(element);
    }

    return () => {
      element.removeEventListener("scroll", updateViewport);
      resizeObserver?.disconnect();
    };
  }, []);

  useEffect(() => {
    return () => {
      for (const observer of observers.current.values()) observer.disconnect();
      observers.current.clear();
    };
  }, []);

  const measureElement = useCallback((element: HTMLDivElement | null, key: string) => {
    if (!element) {
      observers.current.get(key)?.disconnect();
      observers.current.delete(key);
      return;
    }

    const updateMeasurement = () => {
      const index = Number(element.dataset.index);
      const row = listRowsRef.current[index];
      if (!row || !Number.isInteger(index)) return;
      const measured = element.getBoundingClientRect().height || element.offsetHeight;
      if (measured > 0 && measuredSizes.current.get(key) !== measured) {
        measuredSizes.current.set(key, measured);
        setMeasurementVersion((version) => version + 1);
      }
    };

    updateMeasurement();
    if (typeof ResizeObserver === "undefined") return;
    const existing = observers.current.get(key);
    if (existing) existing.disconnect();
    const observer = new ResizeObserver(updateMeasurement);
    observer.observe(element);
    observers.current.set(key, observer);
  }, []);

  // biome-ignore lint/correctness/useExhaustiveDependencies: measurementVersion invalidates ref-backed row measurements.
  const { totalSize, virtualItems } = useMemo(() => {
    const starts: number[] = [];
    let total = 0;
    for (const row of listRows) {
      starts.push(total);
      total += measuredSizes.current.get(rowKey(row)) ?? estimateSize(row);
    }

    if (listRows.length === 0) {
      return { totalSize: 0, virtualItems: [] as VirtualItem[] };
    }

    const firstVisible = Math.max(0, upperBound(starts, viewport.scrollTop) - 1);
    const lastVisible = Math.min(
      listRows.length,
      upperBound(starts, viewport.scrollTop + viewport.height) + 1,
    );
    const first = Math.max(0, firstVisible - OVERSCAN_ROWS);
    const last = Math.min(listRows.length, lastVisible + OVERSCAN_ROWS);
    const items = listRows.slice(first, last).map((row, offset) => {
      const index = first + offset;
      return {
        index,
        size: measuredSizes.current.get(rowKey(row)) ?? estimateSize(row),
        start: starts[index],
      };
    });

    return { totalSize: total, virtualItems: items };
  }, [estimateSize, listRows, measurementVersion, viewport]);

  const renderSession = (row: VirtualSessionEntryRow, index: number) => {
    const session = row.session;
    const isSelected = session.id === activeSessionId;
    const isPinned = pinned || hasPinTag(session);
    const isUnread = session.tags?.includes("unread") ?? false;
    const reorderable = sortable && !row.branchStem;
    const itemStyle: CSSProperties = {
      left: 0,
      position: "absolute",
      top: 0,
      transform: `translateY(${virtualItems.find((item) => item.index === index)?.start ?? 0}px)`,
      width: "100%",
    };

    return (
      <div
        className="px-2 py-0.5"
        data-index={index}
        key={row.key ?? session.id}
        ref={(element) => measureElement(element, rowKey(row))}
        style={itemStyle}
      >
        {/* biome-ignore lint/a11y/noStaticElementInteractions: virtualized row owns native drag initiation. */}
        <div
          className={`group/row flex min-w-0 items-center gap-0 rounded-md border px-2 py-1.5 ${
            isSelected
              ? "cr-sidebar-row-active border-transparent bg-bg text-text"
              : "border-transparent text-text-2 hover:bg-panel-2 hover:text-text"
          }`}
          draggable={reorderable}
          onDragStart={() => onStartReorder?.(session.id)}
        >
          <button
            aria-current={isSelected ? "page" : undefined}
            aria-label={session.title || "Untitled session"}
            className="flex min-w-0 flex-1 items-start gap-1.5 border-0 bg-transparent p-0 text-left text-[13px] text-inherit focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent/60"
            onClick={() => onSelectSession(session.id)}
            onKeyDown={(event) => handleRowKeyDown(event, () => onSelectSession(session.id))}
            title={session.title || "Untitled session"}
            type="button"
          >
            <SessionStatusDot
              storedSessionId={session.id}
              session={session}
              branchStem={row.branchStem}
            />
            <span className="min-w-0 flex-1">
              <span className="block truncate">{session.title || "Untitled session"}</span>
              {card ? (
                <span className="mt-0.5 block truncate text-[11px] text-text-3">
                  {session.workspaceRoot}
                </span>
              ) : null}
              {showProfileTags || card ? (
                <span className="mt-0.5 block truncate text-[10px] text-text-3">
                  {session.driverKind ? `${session.driverKind} · ` : ""}
                  {shortModel(session.model)}
                  {row.branchStem ? ` · ${row.branchStem}` : ""}
                </span>
              ) : null}
            </span>
            {isPinned ? <span className="text-[10px] text-accent" title="Pinned">●</span> : null}
            {isUnread ? <span className="h-1.5 w-1.5 rounded-full bg-accent" title="Unread" /> : null}
          </button>
          <span className="ml-1 flex flex-none items-center gap-0.5 opacity-0 transition-opacity group-hover/row:opacity-100 group-focus-within/row:opacity-100">
            {onResumeSession ? (
              <SessionAction label="Resume session" onClick={() => onResumeSession(session.id)} />
            ) : null}
            {onToggleUnread ? (
              <SessionAction
                label={isUnread ? "Mark session read" : "Mark session unread"}
                onClick={() => onToggleUnread(session.id)}
              />
            ) : null}
            {onTogglePin ? (
              <SessionAction
                label={isPinned ? "Unpin session" : "Pin session"}
                onClick={() => onTogglePin(session.id)}
              />
            ) : null}
            {onBranchSession ? (
              <SessionAction
                label="Branch session"
                onClick={() => onBranchSession(session.id, session.driverKind)}
              />
            ) : null}
            {onArchiveSession ? (
              <SessionAction label="Archive session" onClick={() => onArchiveSession(session.id)} />
            ) : null}
            <SessionAction label={`Delete session ${session.title || session.id}`} danger onClick={() => onDeleteSession(session.id)} />
          </span>
        </div>
      </div>
    );
  };

  const rows = virtualItems.map((virtualItem) => {
    const row = listRows[virtualItem.index];
    if (!row) return null;
    if (row.kind === "session") return renderSession(row, virtualItem.index);

    const itemStyle: CSSProperties = {
      left: 0,
      position: "absolute",
      top: 0,
      transform: `translateY(${virtualItem.start}px)`,
      width: "100%",
    };
    return (
      <div
        className="flex items-center justify-between gap-2 px-3 pb-1 pt-2"
        data-index={virtualItem.index}
        data-testid={`divider-${row.label}`}
        key={row.key}
        ref={(element) => measureElement(element, rowKey(row))}
        style={itemStyle}
      >
        <span className="truncate text-[10px] font-semibold uppercase tracking-[0.12em] text-text-3">
          {row.label}
        </span>
        {dividerAction}
      </div>
    );
  });

  return (
    <div
      className={`scrollbar-fade relative min-h-0 flex-1 overflow-x-hidden overflow-y-auto ${className ?? ""}`.trim()}
      ref={scrollerRef}
    >
      <div className="relative" style={{ height: `${totalSize}px` }}>
        {rows}
      </div>
    </div>
  );
}

function SessionAction({
  danger = false,
  label,
  onClick,
}: {
  danger?: boolean;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      aria-label={label}
      className={`grid h-6 w-6 place-items-center rounded border-0 bg-transparent text-[12px] leading-none cursor-pointer hover:bg-panel-2 ${danger ? "text-danger" : "text-text-3 hover:text-text"}`}
      onClick={(event) => {
        event.stopPropagation();
        onClick();
      }}
      title={label}
      type="button"
    >
      {danger ? "×" : "·"}
    </button>
  );
}

function handleRowKeyDown(event: KeyboardEvent, select: () => void) {
  if (event.key !== "Enter" && event.key !== " ") return;
  event.preventDefault();
  select();
}

function rowKey(row: VirtualSessionListRow) {
  return row.kind === "divider" ? `divider:${row.key}` : `session:${row.key ?? row.session.id}`;
}

function upperBound(values: number[], target: number) {
  let low = 0;
  let high = values.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (values[middle] <= target) low = middle + 1;
    else high = middle;
  }
  return low;
}

function hasPinTag(session: SessionRow) {
  const tags = session.tags ?? [];
  const title = session.title.toLowerCase();
  return tags.includes("pinned") || tags.includes("pin") || title.startsWith("📌") || title.startsWith("[pin]");
}

function shortModel(model: string) {
  return model.includes("/") ? model.slice(model.lastIndexOf("/") + 1) : model;
}

export { hasPinTag, rowKey, shortModel };
