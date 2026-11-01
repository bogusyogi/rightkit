import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { useWorkspaceSearch } from "@rightkit/agent-ui-hooks/hooks/useWorkspaceSearch";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";
import type { WorkspaceSearchHit } from "@rightkit/agent-ui-core/lib/types/workspace-files";

const SEARCH_DELAY_MS = 180;
const SEARCH_LIMIT = 500;

export interface SearchPanelProps {
  config: DaemonConfig | null;
  workspaceRoot: string;
  onClose: () => void;
  onMaximize?: () => void;
  /** Opens the hit in the host editor/viewer, preserving line metadata. */
  onOpenFile?: (hit: WorkspaceSearchHit) => void;
}

export interface SearchFileGroup {
  path: string;
  name: string;
  hits: WorkspaceSearchHit[];
  hasPathMatch: boolean;
}

// Ported from Hermes IDE's SearchPanel pure helper. Offsets are character
// offsets, matching the renderer's string slicing semantics.
export function highlightMatch(
  line: string,
  startChar: number,
  endChar: number,
): { before: string; match: string; after: string } {
  return {
    before: line.slice(0, startChar),
    match: line.slice(startChar, endChar),
    after: line.slice(endChar),
  };
}

// Ported from Hermes IDE's SearchPanel result-summary helper.
export function formatResultCount(total: number, fileCount: number): string {
  const resultWord = total === 1 ? "result" : "results";
  const fileWord = fileCount === 1 ? "file" : "files";
  return `${total} ${resultWord} in ${fileCount} ${fileWord}`;
}

// Ported from Hermes IDE's SearchPanel testable debounce helper.
export function debounce<T extends (...args: never[]) => void>(fn: T, ms: number): T {
  let timer: ReturnType<typeof setTimeout> | null = null;
  const debounced = (...args: Parameters<T>) => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => fn(...args), ms);
  };
  return debounced as T;
}

export function groupSearchHits(hits: WorkspaceSearchHit[]): SearchFileGroup[] {
  const groups = new Map<string, SearchFileGroup>();
  for (const hit of hits) {
    const existing = groups.get(hit.path);
    if (existing) {
      existing.hits.push(hit);
      existing.hasPathMatch ||= hit.kind === "file";
      continue;
    }
    groups.set(hit.path, {
      path: hit.path,
      name: hit.name,
      hits: [hit],
      hasPathMatch: hit.kind === "file",
    });
  }
  return [...groups.values()];
}

export default function SearchPanel({
  config,
  workspaceRoot,
  onClose,
  onMaximize,
  onOpenFile,
}: SearchPanelProps) {
  const [query, setQuery] = useState("");
  const [debouncedQuery, setDebouncedQuery] = useState("");
  const [collapsedFiles, setCollapsedFiles] = useState<Set<string>>(new Set());
  const [requestNonce, setRequestNonce] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    const timer = window.setTimeout(() => setDebouncedQuery(query.trim()), SEARCH_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [query]);

  const requestId = debouncedQuery
    ? `search-panel:${requestNonce}:${workspaceRoot}:${debouncedQuery}`
    : undefined;
  const search = useWorkspaceSearch({
    config,
    workspaceRoot,
    query: debouncedQuery,
    kind: "all",
    limit: SEARCH_LIMIT,
    requestId,
  });
  const groups = useMemo(() => groupSearchHits(search.hits), [search.hits]);
  const hasQuery = query.trim().length > 0;
  const isDebouncing = query.trim() !== debouncedQuery;
  const isSearching = hasQuery && (isDebouncing || search.status === "loading");

  const toggleCollapse = useCallback((path: string) => {
    setCollapsedFiles((previous) => {
      const next = new Set(previous);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }, []);

  const retry = useCallback(() => {
    setRequestNonce((nonce) => nonce + 1);
  }, []);

  const statusMessage = !config
    ? "Connect to a daemon to search the workspace."
    : !workspaceRoot.trim()
      ? "Open a workspace to search its files."
      : !hasQuery
        ? "Search in files"
        : isSearching
          ? "Searching…"
          : search.status === "error"
            ? (search.reason ?? "Search failed")
            : search.status === "cancelled"
              ? "Search cancelled"
              : formatResultCount(search.hits.length, groups.length);

  return (
    <aside
      aria-label="Search panel"
      data-testid="search-panel"
      className="flex h-full w-full min-w-0 shrink-0 flex-col overflow-hidden bg-panel/85"
    >
      <header className="flex h-9 flex-none items-center gap-2 border-b border-line px-3 text-[13px] font-medium text-text">
        <span className="flex-1 truncate">Search</span>
        {onMaximize ? (
          <button
            type="button"
            aria-label="Maximize search panel"
            title="Maximize"
            onClick={onMaximize}
            className="cr-control-pill grid h-6 w-6 place-items-center rounded-md border border-transparent text-text-3 hover:bg-panel-2 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
          >
            <Icon name="maximize" size={13} />
          </button>
        ) : null}
        <button
          type="button"
          aria-label="Close search panel"
          title="Close"
          onClick={onClose}
          className="cr-control-pill grid h-6 w-6 place-items-center rounded-md border border-transparent text-text-3 hover:bg-panel-2 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          <Icon name="close" size={13} />
        </button>
      </header>

      <div className="flex-none border-b border-line px-3 py-2">
        <label className="relative block">
          <span className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-text-3">
            <Icon name="search" size={12} />
          </span>
          <input
            ref={inputRef}
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                setQuery("");
              }
            }}
            placeholder="Search files…"
            aria-label="Search workspace files"
            data-testid="search-input"
            spellCheck={false}
            className="block w-full rounded-md border border-line bg-panel-2 py-1.5 pl-7 pr-8 text-[12px] text-text placeholder:text-text-3 focus:border-accent/60 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
          />
          {query ? (
            <button
              type="button"
              aria-label="Clear search"
              title="Clear search"
              onClick={() => setQuery("")}
              className="absolute right-1 top-1/2 grid h-6 w-6 -translate-y-1/2 place-items-center rounded text-text-3 hover:bg-panel hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
            >
              <Icon name="close" size={12} />
            </button>
          ) : null}
        </label>
        <p className="mt-1 truncate text-[10px] text-text-3">
          {workspaceRoot.trim() || "No workspace selected"}
        </p>
      </div>

      <div
        data-testid="search-status"
        role={search.status === "error" ? "alert" : "status"}
        aria-live="polite"
        className="flex flex-none items-center gap-2 border-b border-line/70 px-3 py-1.5 text-[11px] text-text-3"
      >
        <span className="min-w-0 flex-1 truncate">{statusMessage}</span>
        {isSearching ? (
          <button
            type="button"
            onClick={search.cancel}
            className="shrink-0 text-text-2 underline decoration-line underline-offset-2 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
          >
            Cancel
          </button>
        ) : null}
        {hasQuery && search.status === "error" ? (
          <button
            type="button"
            onClick={retry}
            className="shrink-0 text-text-2 underline decoration-line underline-offset-2 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
          >
            Retry
          </button>
        ) : null}
      </div>

      {hasQuery && search.status === "cancelled" ? (
        <div className="mx-3 mt-2 rounded-md border border-line/70 bg-panel-2/60 p-2 text-[11px] text-text-2">
          Search cancelled. Run it again to scan the workspace.
        </div>
      ) : null}
      {hasQuery && search.status === "ready" && search.truncated ? (
        <div className="mx-3 mt-2 rounded-md border border-line/70 bg-panel-2/60 p-2 text-[11px] text-text-2">
          Results capped at {SEARCH_LIMIT}. Narrow your search.
        </div>
      ) : null}

      <div className="min-h-0 flex-1 overflow-y-auto px-1 py-1" data-testid="search-results">
        {hasQuery && !isSearching && search.status === "ready" && groups.length === 0 ? (
          <div data-testid="search-empty" role="status" className="mx-1 mt-2 rounded-md border border-line/70 bg-panel-2/60 p-3 text-[12px] text-text-2">
            No results found
          </div>
        ) : null}
        {hasQuery
          ? groups.map((group) => (
          <section key={group.path} className="search-file-group">
            <button
              type="button"
              aria-expanded={!collapsedFiles.has(group.path)}
              aria-label={`${collapsedFiles.has(group.path) ? "Expand" : "Collapse"} ${group.path}`}
              data-testid={`search-file-${encodeURIComponent(group.path)}`}
              onClick={() => toggleCollapse(group.path)}
              className="flex w-full items-center gap-1.5 rounded px-2 py-1 text-left text-[12px] text-text-2 hover:bg-panel-2/70 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
            >
              <span aria-hidden="true" className="text-text-3">
                {collapsedFiles.has(group.path) ? "▸" : "▾"}
              </span>
              <span className="min-w-0 flex-1 truncate font-mono" title={group.path}>
                {group.path}
              </span>
              <span className="shrink-0 tabular-nums text-[10px] text-text-3">
                {group.hits.filter((hit) => hit.kind === "content").length || (group.hasPathMatch ? "path" : 0)}
              </span>
            </button>
            {!collapsedFiles.has(group.path) ? (
              <div className="pb-1">
                {group.hits.map((hit, index) => (
                  <button
                    // biome-ignore lint/suspicious/noArrayIndexKey: search hits can share every source coordinate.
                    key={`${hit.path}:${hit.line}:${hit.column}:${index}`}
                    type="button"
                    data-testid={`search-hit-${encodeURIComponent(hit.path)}-${hit.line}-${index}`}
                    onClick={() => onOpenFile?.(hit)}
                    disabled={!onOpenFile}
                    className="flex w-full items-start gap-2 rounded px-3 py-1 text-left text-[11px] text-text-2 hover:bg-panel-2/70 disabled:cursor-default focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
                  >
                    <span className="w-8 shrink-0 text-right font-mono text-[10px] text-text-3">
                      {hit.kind === "content" ? hit.line : "path"}
                    </span>
                    <span className="min-w-0 flex-1 truncate font-mono" title={hit.snippet}>
                      <HighlightedSnippet snippet={hit.kind === "file" ? hit.path : hit.snippet} query={debouncedQuery} />
                    </span>
                  </button>
                ))}
              </div>
            ) : null}
          </section>
            ))
          : null}
      </div>
    </aside>
  );
}

function HighlightedSnippet({ snippet, query }: { snippet: string; query: string }) {
  const start = snippet.toLocaleLowerCase().indexOf(query.toLocaleLowerCase());
  if (!query || start < 0) return <>{snippet}</>;
  const parts = highlightMatch(snippet, start, start + query.length);
  return (
    <>
      {parts.before}
      <mark className="rounded bg-accent/20 text-text">{parts.match}</mark>
      {parts.after}
    </>
  );
}
