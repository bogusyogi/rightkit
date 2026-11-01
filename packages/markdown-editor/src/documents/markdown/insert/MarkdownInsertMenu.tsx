/**
 * Markdown insertion menu.
 *
 * Accessible from the source toolbar's Insert surface. Selecting a
 * template inserts the snippet at the cursor and routes the edit
 * through `core.applyCommand`.
 */

import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { useDismiss } from "@rightkit/platform-ui/headless";
import { useOptionalBlockDrafts } from "@rightkit/document-core/app/BlockDraftContext";
import { Code } from "@phosphor-icons/react/dist/icons/Code";
import { FlowArrow } from "@phosphor-icons/react/dist/icons/FlowArrow";
import { Function as FunctionIcon } from "@phosphor-icons/react/dist/icons/Function";
import { Image } from "@phosphor-icons/react/dist/icons/Image";
import { ListChecks } from "@phosphor-icons/react/dist/icons/ListChecks";
import { ListBullets } from "@phosphor-icons/react/dist/icons/ListBullets";
import { ListNumbers } from "@phosphor-icons/react/dist/icons/ListNumbers";
import { Minus } from "@phosphor-icons/react/dist/icons/Minus";
import { NotePencil } from "@phosphor-icons/react/dist/icons/NotePencil";
import { Quotes } from "@phosphor-icons/react/dist/icons/Quotes";
import { Sigma } from "@phosphor-icons/react/dist/icons/Sigma";
import { Table } from "@phosphor-icons/react/dist/icons/Table";
import { TextHOne } from "@phosphor-icons/react/dist/icons/TextHOne";
import type { Icon } from "@phosphor-icons/react";
import { MARKDOWN_INSERT_TEMPLATES, type MarkdownInsertTemplate } from "./markdownInsertTemplates";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore";
import type { MarkdownTextEdit } from "../core/types";
import { insertMarkdownTable } from "../tables/markdownTableCommands";

const INSERT_GROUPS = [
  { label: "Text", kinds: ["heading", "bulleted-list", "numbered-list", "quote", "divider"] },
  { label: "Content", kinds: ["table", "task-list", "image"] },
  { label: "Code & diagrams", kinds: ["code", "mermaid"] },
  { label: "Math & notes", kinds: ["math-block", "math-inline", "footnote"] },
] as const;

const INSERT_META: Record<MarkdownInsertTemplate["kind"], { icon: Icon; description: string }> = {
  heading: { icon: TextHOne, description: "Section heading" },
  "bulleted-list": { icon: ListBullets, description: "Unordered list of items" },
  "numbered-list": { icon: ListNumbers, description: "Ordered list of items" },
  quote: { icon: Quotes, description: "Block quotation" },
  divider: { icon: Minus, description: "Horizontal rule" },
  table: { icon: Table, description: "Columns with a header row" },
  "task-list": { icon: ListChecks, description: "Track checkable items" },
  image: { icon: Image, description: "Path, URL, or paste directly" },
  code: { icon: Code, description: "Fenced TypeScript example" },
  mermaid: { icon: FlowArrow, description: "Editable flow diagram" },
  "math-block": { icon: Sigma, description: "Display equation" },
  "math-inline": { icon: FunctionIcon, description: "Equation inside prose" },
  footnote: { icon: NotePencil, description: "Reference with note body" },
};

export interface MarkdownInsertMenuProps {
  readonly core: MarkdownDocumentCore;
  readonly insertAt: number;
  readonly onAfterInsert?: (newCaret: number) => void;
  readonly onClose?: () => void;
  readonly onInsertImage?: () => void;
  /** Slash-menu filter text; narrows entries by label/kind (case-insensitive). */
  readonly filter?: string;
  readonly getEditorView?: () => import("prosemirror-view").EditorView | null;
}

/**
 * Render the insert menu. Each entry is a button; selecting one
 * applies the snippet via a single edit and closes the menu.
 */
export function MarkdownInsertMenu({
  core,
  insertAt,
  onAfterInsert,
  onClose,
  onInsertImage,
  filter,
  getEditorView,
}: MarkdownInsertMenuProps): ReactNode {
  const needle = (filter ?? "").trim().toLowerCase();
  const matches = (kind: MarkdownInsertTemplate["kind"]): boolean => {
    if (!needle) return true;
    const label = MARKDOWN_INSERT_TEMPLATES.find((item) => item.kind === kind)?.label ?? "";
    return `${kind} ${label}`.toLowerCase().includes(needle);
  };
  const [tablePickerOpen, setTablePickerOpen] = useState(false);
  const blockDrafts = useOptionalBlockDrafts();
  const [draftTick, setDraftTick] = useState(0);
  useEffect(() => {
    if (!blockDrafts) return;
    return blockDrafts.subscribe(() => setDraftTick((t) => t + 1));
  }, [blockDrafts]);
  void draftTick;
  // biome-ignore lint/correctness/useExhaustiveDependencies: draftTick forces invalidation on registry changes
  const activeDraft = useMemo(() => {
    if (!blockDrafts) return null;
    const docId = core.state.documentId;
    return blockDrafts.get({ documentPath: docId, pagePath: docId });
  }, [blockDrafts, core, draftTick]);
  const getDraftIdentity = () => {
    const docId = core.state.documentId;
    return { documentPath: docId, pagePath: docId } as const;
  };
  const getInsertDisabledReason = (): string | null => {
    if (!activeDraft) return null;
    if (activeDraft.status === "stale")
      return activeDraft.reason ?? "Block changed outside this editor.";
    if (activeDraft.status === "committing") return "Committing…";
    const base = activeDraft.sourceRange?.start ?? 0;
    const end = activeDraft.sourceRange?.end ?? base + activeDraft.originalText.length;
    if (insertAt < base || insertAt > end) return "No valid target in edited block";
    return null;
  };
  const disabledReason = getInsertDisabledReason();

  const insertTable = (rows: number, columns: number): void => {
    if (disabledReason) return;
    if (activeDraft && blockDrafts) {
      const base = activeDraft.sourceRange?.start ?? 0;
      const current = activeDraft.currentText;
      const localAt = Math.max(0, Math.min(insertAt - base, current.length));
      // For draft, insert a simple table snippet (reuse template but adapt dimensions)
      const header = `| ${Array.from({ length: columns }, (_, i) => `Column ${String.fromCharCode(65 + i)}`).join(" | ")} |`;
      const separator = `| ${Array.from({ length: columns }, () => "--------").join(" | ")} |`;
      const rowsText = Array.from(
        { length: rows - 1 },
        () => `| ${Array.from({ length: columns }, () => "        ").join(" | ")} |`,
      ).join("\n");
      const snippet = `${header}\n${separator}\n${rowsText}\n`;
      const next = current.slice(0, localAt) + snippet + current.slice(localAt);
      blockDrafts.update(getDraftIdentity(), next, activeDraft.selectionBookmark);
      onAfterInsert?.(insertAt + snippet.length);
      setTablePickerOpen(false);
      onClose?.();
      return;
    }
    const view = getEditorView?.();
    if (
      view &&
      insertMarkdownTable(rows, columns)(view.state, (transaction) => view.dispatch(transaction))
    ) {
      setTablePickerOpen(false);
      onClose?.();
      return;
    }
    // Keep the menu useful in source-only hosts that have not supplied an EditorView yet.
    const template = MARKDOWN_INSERT_TEMPLATES.find((item) => item.kind === "table");
    if (!template) return;
    core.applyCommand({
      edits: [{ from: insertAt, to: insertAt, insert: template.snippet, inheritMode: "preserve" }],
      expectedRevision: core.snapshot().revision,
      origin: "block-editor",
      label: "Format",
      isolateHistory: true,
      timestampMs: Math.round(performance.now()),
    });
    onAfterInsert?.(insertAt + template.cursorOffset);
    setTablePickerOpen(false);
    onClose?.();
  };

  const onSelect = (template: MarkdownInsertTemplate): void => {
    if (disabledReason) return;
    if (activeDraft && blockDrafts) {
      if (template.kind === "image" && onInsertImage) {
        onInsertImage();
        onClose?.();
        return;
      }
      if (template.kind === "table") {
        setTablePickerOpen((open) => !open);
        return;
      }
      const base = activeDraft.sourceRange?.start ?? 0;
      const current = activeDraft.currentText;
      const localAt = Math.max(0, Math.min(insertAt - base, current.length));
      const next = current.slice(0, localAt) + template.snippet + current.slice(localAt);
      blockDrafts.update(getDraftIdentity(), next, activeDraft.selectionBookmark);
      onAfterInsert?.(insertAt + template.cursorOffset);
      onClose?.();
      return;
    }
    if (template.kind === "image" && onInsertImage) {
      onInsertImage();
      onClose?.();
      return;
    }
    if (template.kind === "table") {
      setTablePickerOpen((open) => !open);
      return;
    }
    const edit: MarkdownTextEdit = {
      from: insertAt,
      to: insertAt,
      insert: template.snippet,
      inheritMode: "preserve",
    };
    core.applyCommand({
      edits: [edit],
      expectedRevision: core.snapshot().revision,
      origin: "block-editor",
      label: "Format",
      isolateHistory: true,
      timestampMs: Math.round(performance.now()),
    });
    onAfterInsert?.(insertAt + template.cursorOffset);
    onClose?.();
  };

  return (
    <div data-testid="markdown-insert-menu" role="menu" className="p-3">
      <p className="mb-4 font-sans text-xs leading-5 text-muted-2">
        Add reusable Markdown blocks at your cursor.
      </p>
      {INSERT_GROUPS.filter((group) => group.kinds.some(matches)).map((group) => (
        <div key={group.label} role="group" aria-label={group.label} className="mb-4 last:mb-0">
          <div className="mb-1.5 px-1 font-sans text-chrome-xs uppercase tracking-[0.18em] text-muted-2">
            {group.label}
          </div>
          <div className="grid gap-1">
            {group.kinds.filter(matches).map((kind) => {
              const template = MARKDOWN_INSERT_TEMPLATES.find((item) => item.kind === kind);
              if (!template) return null;
              const meta = INSERT_META[kind];
              const MetaIcon = meta.icon;
              return (
                <div key={template.kind}>
                  <button
                    type="button"
                    role="menuitem"
                    aria-expanded={template.kind === "table" ? tablePickerOpen : undefined}
                    aria-disabled={!!disabledReason}
                    disabled={!!disabledReason}
                    title={disabledReason ?? undefined}
                    onClick={() => onSelect(template)}
                    data-testid={`md-insert-${template.kind}`}
                    className={`vr-press vr-focus group flex w-full items-center gap-3 rounded-md border border-transparent px-2 py-2 text-left hover:border-line hover:bg-ink/60 ${disabledReason ? "opacity-40" : ""}`}
                  >
                    <span className="grid h-8 w-9 shrink-0 place-items-center rounded border border-line bg-panel-2 text-muted group-hover:border-copper/60 group-hover:text-paper">
                      <MetaIcon size={16} weight="regular" aria-hidden />
                    </span>
                    <span className="min-w-0">
                      <span className="block font-sans text-xs font-semibold text-paper">
                        {template.label}
                      </span>
                      <span className="mt-0.5 block font-sans text-chrome-xs text-muted-2">
                        {meta.description}
                      </span>
                    </span>
                  </button>
                  {template.kind === "table" && tablePickerOpen && (
                    <MarkdownTablePicker
                      onSelect={insertTable}
                      onEscape={() => setTablePickerOpen(false)}
                    />
                  )}
                </div>
              );
            })}
          </div>
        </div>
      ))}
    </div>
  );
}

const TABLE_PICKER_ROWS = 8;
const TABLE_PICKER_COLUMNS = 8;

interface MarkdownTablePickerProps {
  readonly onSelect: (rows: number, columns: number) => void;
  readonly onEscape: () => void;
}

function MarkdownTablePicker({ onSelect, onEscape }: MarkdownTablePickerProps) {
  const [rows, setRows] = useState(2);
  const [columns, setColumns] = useState(2);

  const setDimensions = (nextRows: number, nextColumns: number) => {
    setRows(Math.max(2, Math.min(TABLE_PICKER_ROWS, nextRows)));
    setColumns(Math.max(1, Math.min(TABLE_PICKER_COLUMNS, nextColumns)));
  };

  // Escape inside the grid closes the picker (bubble phase on the grid, as the handler it replaces).
  const gridRef = useRef<HTMLDivElement>(null);
  useDismiss(onEscape, { escapeTarget: gridRef, outside: false, stopPropagation: false, imeGuard: false });

  const onGridKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    let nextRows = rows;
    let nextColumns = columns;
    if (event.key === "ArrowRight") nextColumns++;
    if (event.key === "ArrowLeft") nextColumns--;
    if (event.key === "ArrowDown") nextRows++;
    if (event.key === "ArrowUp") nextRows--;
    if (event.key === "Home") {
      nextRows = 2;
      nextColumns = 1;
    }
    if (event.key === "End") {
      nextRows = TABLE_PICKER_ROWS;
      nextColumns = TABLE_PICKER_COLUMNS;
    }
    if (nextRows !== rows || nextColumns !== columns) {
      event.preventDefault();
      setDimensions(nextRows, nextColumns);
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      onSelect(rows, columns);
    }
  };

  return (
    <div
      data-testid="md-table-picker"
      role="group"
      aria-label="Choose table dimensions"
      className="ml-11 mt-1 rounded border border-line bg-panel-2 p-2"
    >
      <div className="mb-1 flex items-center justify-between gap-3 font-sans text-chrome-xs text-muted-2">
        <span aria-live="polite">
          {columns} columns × {rows} rows
        </span>
        <span>Arrow keys · Enter</span>
      </div>
      <div
        ref={gridRef}
        role="grid"
        aria-label="Table dimensions"
        aria-rowcount={TABLE_PICKER_ROWS}
        aria-colcount={TABLE_PICKER_COLUMNS}
        onKeyDown={onGridKeyDown}
        className="grid w-fit gap-0.5"
      >
        {Array.from({ length: TABLE_PICKER_ROWS }, (_, rowIndex) => {
          const row = rowIndex + 2;
          return (
            <div
              key={`row-${row}`}
              role="row"
              tabIndex={-1}
              aria-rowindex={rowIndex + 1}
              className="grid gap-0.5"
              style={{
                gridTemplateColumns: `repeat(${TABLE_PICKER_COLUMNS}, minmax(1.15rem, 1fr))`,
              }}
            >
              {Array.from({ length: TABLE_PICKER_COLUMNS }, (_, columnIndex) => {
                const column = columnIndex + 1;
                const active = row <= rows && column <= columns;
                const selected = row === rows && column === columns;
                return (
                  <button
                    type="button"
                    key={`${row}-${column}`}
                    role="gridcell"
                    aria-selected={selected}
                    aria-colindex={column}
                    tabIndex={selected ? 0 : -1}
                    aria-label={`${row} rows by ${column} columns`}
                    data-testid={`md-table-cell-${row}-${column}`}
                    onMouseEnter={() => setDimensions(row, column)}
                    onFocus={() => setDimensions(row, column)}
                    onClick={() => onSelect(row, column)}
                    className={`h-5 w-5 rounded-sm border ${active ? "border-copper/70 bg-copper/40" : "border-line bg-ink/50"} vr-focus`}
                  />
                );
              })}
            </div>
          );
        })}
      </div>
    </div>
  );
}
