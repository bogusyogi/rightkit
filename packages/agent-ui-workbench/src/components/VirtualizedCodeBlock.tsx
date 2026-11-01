/**
 * Large code output renderer, ported from OpenChamber's
 * `VirtualizedCodeBlock` atom.
 *
 * The donor uses @tanstack/react-virtual and a worker highlighter. the host app
 * renderer deliberately has neither dependency in its current package, so
 * this seam keeps the donor's important contract (one row per line, overscan,
 * small-block fallback, stable text selection) with a dependency-free window.
 * Text is rendered as text rather than injected HTML; a future highlighter can
 * be added at the row boundary without changing virtualization ownership.
 */

import { useLayoutEffect, useMemo, useRef, useState, type CSSProperties } from "react";

export const VIRTUALIZE_THRESHOLD = 80;
export const CODE_ROW_HEIGHT = 20;
const OVERSCAN = 20;

export interface CodeLine {
  text: string;
  lineNumber?: number | null;
  isInfo?: boolean;
  type?: "context" | "added" | "removed";
}

export interface VirtualizedCodeBlockProps {
  lines: readonly CodeLine[];
  language: string;
  maxHeight?: string;
  showLineNumbers?: boolean;
  lineStyles?: (line: CodeLine) => CSSProperties | undefined;
}

function CodeRow({ line, showLineNumbers, style }: {
  line: CodeLine;
  showLineNumbers: boolean;
  style?: CSSProperties;
}) {
  return (
    <div className="typography-code font-mono flex w-full min-w-0" style={style}>
      {showLineNumbers ? (
        <span
          className="w-10 flex-shrink-0 text-right pr-3 select-none border-r mr-3 -my-0.5 py-0.5"
          style={{ color: "var(--tools-edit-line-number)", borderColor: "var(--tools-border)" }}
        >
          {!line.isInfo && line.lineNumber != null ? line.lineNumber : ""}
        </span>
      ) : null}
      <div className="min-w-0 flex-1">
        <div className={line.isInfo ? "whitespace-pre-wrap break-words italic text-text-3" : "whitespace-pre"}>
          {line.text}
        </div>
      </div>
    </div>
  );
}

function SmallCodeBlock({ lines, language, maxHeight, showLineNumbers = true, lineStyles }: VirtualizedCodeBlockProps) {
  return (
    <div
      className="typography-code font-mono w-full min-w-0"
      data-code-block-mode="static"
      data-language={languageLabel(lines, language)}
      style={{ maxHeight, overflow: "auto" }}
    >
      {lines.map((line, index) => (
        <CodeRow
          // biome-ignore lint/suspicious/noArrayIndexKey: rendered code lines have no domain identity.
          key={index}
          line={line}
          showLineNumbers={showLineNumbers}
          style={lineStyles?.(line)}
        />
      ))}
    </div>
  );
}

function languageLabel(lines: readonly CodeLine[], language: string): string {
  // Keep this helper pure so the static/virtualized roots expose a stable
  // diagnostic attribute even when a caller supplies an empty language.
  void lines;
  return language;
}

function VirtualizedRows({ lines, language, maxHeight, showLineNumbers = true, lineStyles }: VirtualizedCodeBlockProps) {
  const parentRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(0);

  useLayoutEffect(() => {
    const node = parentRef.current;
    if (!node) return;
    const updateHeight = () => setViewportHeight(node.clientHeight);
    updateHeight();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(updateHeight);
    observer.observe(node);
    return () => observer.disconnect();
  }, []);

  const firstVisible = Math.max(0, Math.floor(scrollTop / CODE_ROW_HEIGHT) - OVERSCAN);
  const visibleCount = Math.ceil(viewportHeight / CODE_ROW_HEIGHT) || Math.ceil(480 / CODE_ROW_HEIGHT);
  const lastVisible = Math.min(lines.length, firstVisible + visibleCount + OVERSCAN * 2);
  const visibleLines = lines.slice(firstVisible, lastVisible);
  const totalHeight = lines.length * CODE_ROW_HEIGHT;

  return (
    <div
      ref={parentRef}
      className="typography-code font-mono w-full min-w-0"
      data-code-block-mode="virtualized"
      data-language={languageLabel(lines, language)}
      style={{ height: `min(${totalHeight}px, ${maxHeight})`, maxHeight, overflow: "auto" }}
      onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}
    >
      <div style={{ height: totalHeight, position: "relative" }}>
        <div style={{ height: firstVisible * CODE_ROW_HEIGHT }} aria-hidden="true" />
        {visibleLines.map((line, offset) => {
          const index = firstVisible + offset;
          return (
            <div key={index} data-index={index} style={{ height: CODE_ROW_HEIGHT }}>
              <CodeRow line={line} showLineNumbers={showLineNumbers} style={lineStyles?.(line)} />
            </div>
          );
        })}
      </div>
    </div>
  );
}

export function VirtualizedCodeBlock({
  lines,
  language,
  maxHeight = "60vh",
  showLineNumbers = true,
  lineStyles,
}: VirtualizedCodeBlockProps) {
  // Keep the whole input stable for callers that construct a new array during
  // streaming; rendering still uses the latest line objects.
  const normalizedLines = useMemo(() => [...lines], [lines]);
  const props = { lines: normalizedLines, language, maxHeight, showLineNumbers, lineStyles };
  if (normalizedLines.length <= VIRTUALIZE_THRESHOLD) {
    return <SmallCodeBlock {...props} />;
  }
  return <VirtualizedRows {...props} />;
}

export default VirtualizedCodeBlock;
