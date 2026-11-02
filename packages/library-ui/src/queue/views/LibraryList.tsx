import { cx } from "@rightkit/react-shell/react";
import { memo } from "react";
import { CheckSquare, Images, Square, Trash2 } from "lucide-react";
import { categoryIcon } from "../components/CategoryIcon.js";
import type { HistoryRow } from "../contract.js";
import { labelForSource, categoryForKind } from "../lib/jobDisplay.js";
import {
  IMAGE_KIND_LABEL,
  imageCountForRow,
  imageCountLabel,
  isImageRow,
} from "../lib/mediaSelection.js";
import { isQaMode, qaState } from "../lib/ipc/index.js";
import { fmtDate, fmtDuration } from "../lib/format.js";
import { categoryToneClass } from "../lib/categoryDisplay.js";
import { panelClass } from "../lib/ui.js";

type GroupedRows = Array<[string, HistoryRow[]]>;

interface LibraryListProps {
  rows: HistoryRow[];
  grouped: GroupedRows | null;
  selectedIds: Set<string>;
  confirmDeleteId: string | null;
  deletingId: string | null;
  onOpen: (id: string) => void;
  onToggleSelect: (id: string) => void;
  onDelete: (row: HistoryRow) => void;
}

export const LibraryList = memo(function LibraryList({
  rows,
  grouped,
  selectedIds,
  confirmDeleteId,
  deletingId,
  onOpen,
  onToggleSelect,
  onDelete,
}: LibraryListProps) {
  return (
    <ul className="m-0 flex list-none flex-col gap-2 p-0">
      {grouped
        ? grouped.map(([cat, catRows]) => (
            <CategoryGroup
              key={cat}
              category={cat}
              rows={catRows}
              selectedIds={selectedIds}
              confirmDeleteId={confirmDeleteId}
              deletingId={deletingId}
              onOpen={onOpen}
              onToggleSelect={onToggleSelect}
              onDelete={onDelete}
            />
          ))
        : rows.map((row, index) => (
            <li key={row.id}>
              <LibraryRow
                row={row}
                index={index}
                selected={selectedIds.has(row.id)}
                confirmDelete={confirmDeleteId === row.id}
                deleting={deletingId === row.id}
                onOpen={onOpen}
                onToggleSelect={onToggleSelect}
                onDelete={onDelete}
              />
            </li>
          ))}
    </ul>
  );
});

const CategoryGroup = memo(function CategoryGroup({
  category,
  rows,
  selectedIds,
  confirmDeleteId,
  deletingId,
  onOpen,
  onToggleSelect,
  onDelete,
}: {
  category: string;
  rows: HistoryRow[];
  selectedIds: Set<string>;
  confirmDeleteId: string | null;
  deletingId: string | null;
  onOpen: (id: string) => void;
  onToggleSelect: (id: string) => void;
  onDelete: (row: HistoryRow) => void;
}) {
  const CatIcon = categoryIcon(category);
  return (
    <li className="mb-1">
      <div className={categoryToneClass(category)}>
        {CatIcon && <CatIcon size={13} />}
        <span>{category}</span>
        <span className="font-mono text-[10px] opacity-70">({rows.length})</span>
      </div>
      <ul className="m-0 flex list-none flex-col gap-2 p-0">
        {rows.map((row, index) => (
          <li key={row.id}>
            <LibraryRow
              row={row}
              index={index}
              selected={selectedIds.has(row.id)}
              confirmDelete={confirmDeleteId === row.id}
              deleting={deletingId === row.id}
              onOpen={onOpen}
              onToggleSelect={onToggleSelect}
              onDelete={onDelete}
            />
          </li>
        ))}
      </ul>
    </li>
  );
});

const LibraryRow = memo(function LibraryRow({
  row,
  index,
  selected,
  confirmDelete,
  deleting,
  onOpen,
  onToggleSelect,
  onDelete,
}: {
  row: HistoryRow;
  index: number;
  selected: boolean;
  confirmDelete: boolean;
  deleting: boolean;
  onOpen: (id: string) => void;
  onToggleSelect: (id: string) => void;
  onDelete: (row: HistoryRow) => void;
}) {
  const imageJob = isImageRow(row);
  const CatIcon = imageJob ? Images : categoryIcon(categoryForKind(row.kind));
  return (
    <div
      className={cx(
        "group grid w-full grid-cols-[auto_minmax(0,1fr)_auto] items-stretch overflow-hidden text-left transition-[background-color,border-color,box-shadow,transform] duration-150 ease-out hover:-translate-y-px hover:border-[var(--accent)] hover:bg-[color-mix(in_srgb,var(--panel-2)_88%,var(--accent))]",
        panelClass,
        isQaMode &&
          qaState === "library-selected" &&
          index === 0 &&
          "shadow-[0_0_0_2px_color-mix(in_srgb,var(--accent)_65%,transparent)]",
      )}
      title={row.url}
    >
      <button
        type="button"
        className="ml-3 inline-flex h-8 w-8 self-center items-center justify-center rounded-[6px] text-[var(--muted)] transition-colors hover:text-[var(--accent)]"
        onClick={() => onToggleSelect(row.id)}
        aria-label={selected ? "Deselect" : "Select"}
      >
        {selected ? (
          <CheckSquare size={16} strokeWidth={1.8} />
        ) : (
          <Square size={16} strokeWidth={1.8} />
        )}
      </button>
      <button
        type="button"
        className="grid min-w-0 grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-3 px-4 py-3 text-left focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-[color-mix(in_srgb,var(--accent)_45%,transparent)]"
        onClick={() => onOpen(row.id)}
        title={row.url}
        aria-label={`Open transcript ${row.title ?? row.url}`}
      >
        <span className="inline-flex min-w-[82px] items-center justify-center gap-1 rounded-[5px] border border-[var(--line)] px-2 py-[3px] font-[family-name:var(--mono)] text-[11px] text-[var(--accent)]">
          {CatIcon && <CatIcon size={12} />}
          {imageJob ? IMAGE_KIND_LABEL : labelForSource(row.kind, row.url)}
        </span>
        <span className="truncate text-base font-medium text-[var(--paper)]">
          {row.title ?? row.url}
        </span>
        <span className="flex shrink-0 items-center gap-2 font-[family-name:var(--mono)] text-xs text-[var(--muted)]">
          {imageJob && <span>{imageCountLabel(imageCountForRow(row))}</span>}
          {row.duration_s != null && <span>{fmtDuration(row.duration_s)}</span>}
          {row.lang && <span>{row.lang.toUpperCase()}</span>}
          <span>{fmtDate(row.created_at)}</span>
        </span>
        {row.snippet && (
          <span className="col-start-2 col-end-4 line-clamp-2 text-sm text-[var(--muted)]">
            {row.snippet}
          </span>
        )}
      </button>
      <button
        type="button"
        className={cx(
          "mr-3 inline-flex h-8 w-8 self-center items-center justify-center rounded-[6px] transition-[opacity,background-color,color,transform] duration-150 focus:outline-none focus:ring-2 active:scale-95 disabled:cursor-not-allowed disabled:opacity-35 group-hover:opacity-100",
          confirmDelete
            ? "bg-[var(--warn)] text-white opacity-100 hover:bg-[color-mix(in_srgb,var(--warn)_80%,#000)]"
            : "text-[var(--muted)] opacity-70 hover:bg-[color-mix(in_srgb,var(--warn)_14%,var(--panel-2))] hover:text-[var(--warn)] focus:ring-[var(--warn)]/35",
        )}
        onClick={() => onDelete(row)}
        disabled={deleting}
        title={confirmDelete ? "Click again to confirm deletion" : "Delete transcript"}
        aria-label={`Delete transcript ${row.title ?? row.url}`}
      >
        <Trash2 size={15} strokeWidth={1.9} aria-hidden="true" />
      </button>
    </div>
  );
});

