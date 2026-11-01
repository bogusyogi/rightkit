import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
  type RefObject,
} from "react";
import type { TranscriptDensity, TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";
import type { TranscriptGroup } from "@rightkit/agent-ui-core/lib/transcriptGroups";
import {
  createTranscriptRenderCache,
  layoutTranscriptRows,
  LONG_TRANSCRIPT_THRESHOLD,
  projectTranscriptGroups,
  transcriptViewportWindow,
  TRANSCRIPT_ROW_ESTIMATE_PX,
} from "@rightkit/agent-ui-core/lib/performance/longTranscriptRender";
import type { PendingTranscriptQuestion } from "@rightkit/agent-ui-core/lib/performance/longTranscriptRender";

export interface LongTranscriptViewportProps {
  messages: readonly TranscriptMessage[];
  pendingQuestion: PendingTranscriptQuestion | null;
  density: TranscriptDensity;
  renderGroup: (group: TranscriptGroup, index: number) => ReactNode;
  /**
   * Pass the existing transcript scroller when this viewport is mounted inside
   * a larger transcript surface. With no ref, this component owns scrolling.
   */
  scrollContainerRef?: RefObject<HTMLElement | null>;
  className?: string;
  rowEstimate?: number;
}

type ViewportState = { height: number; scrollTop: number };

/**
 * Measured, overscanned transcript rows for long sessions. This is a renderer
 * projection only: all groups arrive as immutable props, and row measurement
 * uses browser layout rather than asking the daemon or workspace for data.
 * The short path remains ordinary document flow, preserving native selection
 * and the existing transcript layout for ordinary conversations.
 */
export function LongTranscriptViewport({
  messages,
  pendingQuestion,
  density,
  renderGroup,
  scrollContainerRef,
  className,
  rowEstimate = TRANSCRIPT_ROW_ESTIMATE_PX,
}: LongTranscriptViewportProps) {
  const rootRef = useRef<HTMLDivElement | null>(null);
  const groupsCache = useRef(createTranscriptRenderCache());
  const measuredSizes = useRef(new Map<string, number>());
  const observedRows = useRef(new Map<string, ResizeObserver>());
  const groups = projectTranscriptGroups(groupsCache.current, {
    messages,
    pendingQuestion,
    density,
  });
  const [viewport, setViewport] = useState<ViewportState>({ height: 600, scrollTop: 0 });
  const [measurementVersion, setMeasurementVersion] = useState(0);
  const isLong = groups.length > LONG_TRANSCRIPT_THRESHOLD;
  const rowKeys = useMemo(
    () => groups.map((group, index) => transcriptGroupKey(group, index)),
    [groups],
  );

  useEffect(() => {
    const validKeys = new Set(rowKeys);
    for (const key of measuredSizes.current.keys()) {
      if (!validKeys.has(key)) measuredSizes.current.delete(key);
    }
    setMeasurementVersion((version) => version + 1);
  }, [rowKeys]);

  const updateViewport = useCallback(() => {
    const root = rootRef.current;
    const external = scrollContainerRef?.current;
    const scroller = external ?? root;
    if (!scroller) return;
    setViewport({
      height: scroller.clientHeight || 600,
      scrollTop: external ? external.scrollTop : root?.scrollTop ?? 0,
    });
  }, [scrollContainerRef]);

  useEffect(() => {
    const root = rootRef.current;
    const external = scrollContainerRef?.current;
    const scroller = external ?? root;
    if (!scroller) return;
    updateViewport();
    scroller.addEventListener("scroll", updateViewport, { passive: true });
    if (typeof ResizeObserver === "undefined") {
      return () => scroller.removeEventListener("scroll", updateViewport);
    }
    const observer = new ResizeObserver(updateViewport);
    observer.observe(scroller);
    return () => {
      scroller.removeEventListener("scroll", updateViewport);
      observer.disconnect();
    };
  }, [scrollContainerRef, updateViewport]);

  useEffect(() => () => {
    for (const observer of observedRows.current.values()) observer.disconnect();
    observedRows.current.clear();
  }, []);

  const measureRow = useCallback((element: HTMLDivElement | null, key: string) => {
    if (!element) {
      observedRows.current.get(key)?.disconnect();
      observedRows.current.delete(key);
      return;
    }
    const updateMeasurement = () => {
      const height = element.getBoundingClientRect().height || element.offsetHeight;
      if (height > 0 && measuredSizes.current.get(key) !== height) {
        measuredSizes.current.set(key, height);
        setMeasurementVersion((version) => version + 1);
      }
    };
    updateMeasurement();
    if (typeof ResizeObserver === "undefined") return;
    observedRows.current.get(key)?.disconnect();
    const observer = new ResizeObserver(updateMeasurement);
    observer.observe(element);
    observedRows.current.set(key, observer);
  }, []);

  // biome-ignore lint/correctness/useExhaustiveDependencies: measurementVersion invalidates ref-backed row sizes.
  const layout = useMemo(() => {
    const sizes = new Map<number, number>();
    rowKeys.forEach((key, index) => {
      const measured = measuredSizes.current.get(key);
      if (measured !== undefined) sizes.set(index, measured);
    });
    const gap = density === "summary" ? 4 : density === "verbose" ? 16 : 8;
    return layoutTranscriptRows(groups.length, sizes, rowEstimate, gap);
  }, [density, groups.length, measurementVersion, rowEstimate, rowKeys]);

  if (!isLong) {
    return (
      <div
        ref={rootRef}
        className={`flex w-full flex-col ${densityGap(density)} ${className ?? ""}`.trim()}
        data-transcript-render-mode="static"
      >
        {groups.map((group, index) => (
          <div key={rowKeys[index]} data-index={index}>
            {renderGroup(group, index)}
          </div>
        ))}
      </div>
    );
  }

  const external = scrollContainerRef?.current;
  const localScrollTop = external && rootRef.current
    ? Math.max(
        0,
        viewport.scrollTop
          - (rootRef.current.getBoundingClientRect().top
            - external.getBoundingClientRect().top
            + viewport.scrollTop),
      )
    : viewport.scrollTop;
  const visible = transcriptViewportWindow(layout, localScrollTop, viewport.height);
  const rows = visible.rows.map((row) => {
    const group = groups[row.index];
    if (!group) return null;
    const style: CSSProperties = {
      left: 0,
      position: "absolute",
      top: 0,
      transform: `translateY(${row.start}px)`,
      width: "100%",
    };
    return (
      <div
        key={rowKeys[row.index]}
        ref={(element) => {
          const key = rowKeys[row.index];
          if (key) measureRow(element, key);
        }}
        data-index={row.index}
        style={style}
      >
        {renderGroup(group, row.index)}
      </div>
    );
  });

  return (
    <div
      ref={rootRef}
      className={`relative min-h-0 w-full ${external ? "" : "overflow-y-auto"} ${className ?? ""}`.trim()}
      data-transcript-render-mode="virtualized"
      onScroll={external ? undefined : updateViewport}
    >
      <div className="relative" style={{ height: `${visible.totalSize}px` }}>
        {rows}
      </div>
    </div>
  );
}

function transcriptGroupKey(group: TranscriptGroup, _index: number): string {
  return `${group.kind}:${group.id}`;
}

function densityGap(density: TranscriptDensity): string {
  return density === "summary" ? "gap-1" : density === "verbose" ? "gap-4" : "gap-2";
}

export default LongTranscriptViewport;
