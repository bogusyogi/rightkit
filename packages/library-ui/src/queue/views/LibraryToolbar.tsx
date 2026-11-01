import { cx } from "@rightkit/react-shell/react";
import { CheckSquare, Download, Search, SlidersHorizontal, Trash2, X } from "lucide-react";
import type { ChangeEvent } from "react";
import type { Kind } from "../contract";
import { buttonClass, inputClass, mutedTextClass } from "../lib/ui";

export type SortKey = "newest" | "oldest" | "longest" | "shortest";
export type SourceFilter = "all" | "local" | "remote";
export type CategoryFilter = "all" | "media" | "social" | "document" | "web";

const KIND_FILTERS: Array<{ value: Kind | "all"; label: string }> = [
  { value: "all", label: "All types" },
  { value: "youtube", label: "YouTube" },
  { value: "reel", label: "Reel" },
  { value: "instagram_post", label: "Post" },
  { value: "carousel", label: "Carousel" },
  { value: "tiktok", label: "TikTok" },
  { value: "article", label: "Article" },
  { value: "local_file", label: "File" },
];

interface LibraryToolbarProps {
  query: string;
  loading: boolean;
  showFilters: boolean;
  selectedCount: number;
  visibleCount: number;
  kindFilter: Kind | "all";
  sourceFilter: SourceFilter;
  catFilter: CategoryFilter;
  sortKey: SortKey;
  onQueryChange: (event: ChangeEvent<HTMLInputElement>) => void;
  onToggleFilters: () => void;
  onSelectAll: () => void;
  onClearSelection: () => void;
  confirmBulkDelete: boolean;
  onBulkDelete: () => void;
  onBulkExport: () => void;
  onKindFilter: (value: Kind | "all") => void;
  onSourceFilter: (value: SourceFilter) => void;
  onCatFilter: (value: CategoryFilter) => void;
  onSortKey: (value: SortKey) => void;
}

export function LibraryToolbar({
  query,
  loading,
  showFilters,
  selectedCount,
  visibleCount,
  kindFilter,
  sourceFilter,
  catFilter,
  sortKey,
  onQueryChange,
  onToggleFilters,
  onSelectAll,
  onClearSelection,
  confirmBulkDelete,
  onBulkDelete,
  onBulkExport,
  onKindFilter,
  onSourceFilter,
  onCatFilter,
  onSortKey,
}: LibraryToolbarProps) {
  return (
    <>
      <div className="relative">
        <Search
          className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-[var(--muted)]"
          size={16}
          strokeWidth={1.9}
          aria-hidden="true"
        />
        <input
          type="search"
          className={cx(inputClass, "h-11 pl-10")}
          value={query}
          onChange={onQueryChange}
          placeholder="Search transcripts..."
          // biome-ignore lint/a11y/noAutofocus: search is the primary action on this view; focusing on mount is intentional
          autoFocus
        />
        {loading && (
          <span className={cx("absolute right-3 top-1/2 -translate-y-1/2", mutedTextClass)}>
            ...
          </span>
        )}
      </div>

      <div className="flex items-center gap-2">
        <button
          type="button"
          className={cx(
            "inline-flex h-8 items-center gap-1.5 rounded-[6px] border px-3 text-xs transition-colors",
            showFilters
              ? "border-[var(--accent)] text-[var(--accent)]"
              : "border-[var(--line)] text-[var(--muted)] hover:border-[var(--accent)] hover:text-[var(--paper)]",
          )}
          onClick={onToggleFilters}
          aria-label="Toggle filters"
        >
          <SlidersHorizontal size={13} strokeWidth={1.8} />
          <span>Filters</span>
        </button>
        {selectedCount > 0 ? (
          <>
            <span className="font-[family-name:var(--mono)] text-xs text-[var(--muted)]">
              {selectedCount} selected
            </span>
            <button
              type="button"
              className={buttonClass}
              onClick={onBulkExport}
              title="Export selected transcripts"
              aria-label="Export selected"
            >
              <Download size={13} strokeWidth={1.8} />
              <span>Export</span>
            </button>
            <button
              type="button"
              className={cx(
                "inline-flex h-8 items-center gap-1.5 rounded-[6px] border px-3 text-xs transition-colors",
                confirmBulkDelete
                  ? "border-[var(--warn)] bg-[color-mix(in_srgb,var(--warn)_18%,var(--panel-2))] text-[var(--warn)]"
                  : "border-[color-mix(in_srgb,var(--warn)_45%,var(--line))] text-[var(--warn)] hover:bg-[color-mix(in_srgb,var(--warn)_14%,var(--panel-2))]",
              )}
              onClick={onBulkDelete}
              title="Delete selected transcripts"
              aria-label="Delete selected"
              data-confirm={confirmBulkDelete || undefined}
            >
              <Trash2 size={13} strokeWidth={1.8} />
              <span>{confirmBulkDelete ? "Confirm delete?" : "Delete"}</span>
            </button>
            <button
              type="button"
              className={buttonClass}
              onClick={onClearSelection}
              title="Clear selection"
              aria-label="Clear selection"
            >
              <X size={13} strokeWidth={1.8} />
            </button>
          </>
        ) : (
          visibleCount > 0 && (
            <button
              type="button"
              className={buttonClass}
              onClick={onSelectAll}
              title="Select all visible"
              aria-label="Select all"
            >
              <CheckSquare size={13} strokeWidth={1.8} />
              <span>Select all</span>
            </button>
          )
        )}
      </div>

      {showFilters && (
        <div className="flex flex-wrap items-center gap-3 rounded-[8px] border border-[var(--line)] bg-[color-mix(in_srgb,var(--panel-2)_72%,var(--ink))] p-3">
          <label className="grid gap-1">
            <span className="text-[10px] font-semibold uppercase text-[var(--muted)]">
              Category
            </span>
            <select
              className="h-8 rounded-[6px] border border-[var(--line)] bg-[var(--panel-2)] px-2 text-sm text-[var(--paper)] outline-none focus:border-[var(--accent)]"
              value={catFilter}
              onChange={(e) => onCatFilter(e.target.value as CategoryFilter)}
            >
              <option value="all">All categories</option>
              <option value="media">Media</option>
              <option value="social">Social</option>
              <option value="document">Document</option>
              <option value="web">Web</option>
            </select>
          </label>
          <label className="grid gap-1">
            <span className="text-[10px] font-semibold uppercase text-[var(--muted)]">Type</span>
            <select
              className="h-8 rounded-[6px] border border-[var(--line)] bg-[var(--panel-2)] px-2 text-sm text-[var(--paper)] outline-none focus:border-[var(--accent)]"
              value={kindFilter}
              onChange={(e) => onKindFilter(e.target.value as Kind | "all")}
            >
              {KIND_FILTERS.map((f) => (
                <option key={f.value} value={f.value}>
                  {f.label}
                </option>
              ))}
            </select>
          </label>
          <label className="grid gap-1">
            <span className="text-[10px] font-semibold uppercase text-[var(--muted)]">Source</span>
            <select
              className="h-8 rounded-[6px] border border-[var(--line)] bg-[var(--panel-2)] px-2 text-sm text-[var(--paper)] outline-none focus:border-[var(--accent)]"
              value={sourceFilter}
              onChange={(e) => onSourceFilter(e.target.value as SourceFilter)}
            >
              <option value="all">All sources</option>
              <option value="local">Local files</option>
              <option value="remote">Remote URLs</option>
            </select>
          </label>
          <label className="grid gap-1">
            <span className="text-[10px] font-semibold uppercase text-[var(--muted)]">Sort</span>
            <select
              className="h-8 rounded-[6px] border border-[var(--line)] bg-[var(--panel-2)] px-2 text-sm text-[var(--paper)] outline-none focus:border-[var(--accent)]"
              value={sortKey}
              onChange={(e) => onSortKey(e.target.value as SortKey)}
            >
              <option value="newest">Newest first</option>
              <option value="oldest">Oldest first</option>
              <option value="longest">Longest</option>
              <option value="shortest">Shortest</option>
            </select>
          </label>
        </div>
      )}
    </>
  );
}
