import {
  ActionDialog,
  EmptyState,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeaderCell,
  TableMessage,
  TableRow,
} from "@rightkit/react-shell/react";
import { useCallback, useEffect, useMemo, useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import PaneLoading from "@rightkit/agent-ui-core/components/PaneLoading";
import { sanitizeError } from "@rightkit/agent-ui-hooks/components/sanitizeError";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types";
import {
  memrightActivity,
  memrightDelete,
  memrightGet,
  memrightHealth,
  memrightList,
  memrightMetrics,
  memrightPut,
  memrightRecall,
  type MemrightActivity,
  type MemrightHealth,
  type MemrightListRow,
  type MemrightMetrics,
  type MemrightRecallHit,
} from "@rightkit/agent-ui-core/lib/daemon/memright";

// Native Memory panel over the daemon's in-process memright store
// (~/.coderight/evolution.db). Browse/search/read/edit/delete + savings
// metrics. Reference layout: engine/crates/memright/src/dashboard.html —
// rebuilt on the CR design system (index.css tokens), no new deps.

const DEGRADED_EMBEDDER_DIM = 256;

type LayerStatus = "live" | "idle" | "na";
interface LayerRow {
  n: number;
  name: string;
  status: LayerStatus;
  metric: string;
}
interface Family {
  title: string;
  tag: string;
  explain: string;
  layers: LayerRow[];
}

// The context-engine taxonomy (workspace CONTEXT-ENGINEERING.md §1-2): three
// families over eight layers. This machine's engine logs L2/3/5 (transform_log),
// L7 (recall_log) and L8 (curate); L1/L4/L6 are conductor/runtime seams not
// logged to this store — shown honestly as n/a.
function buildFamilies(metrics: MemrightMetrics | null): Family[] {
  const t = (metrics?.transforms ?? {}) as Record<string, { runs?: number; chars_saved?: number }>;
  const sum = (...verbs: string[]) =>
    verbs.reduce(
      (a, v) => ({
        runs: a.runs + (t[v]?.runs ?? 0),
        saved: a.saved + (t[v]?.chars_saved ?? 0),
      }),
      { runs: 0, saved: 0 },
    );
  const runc = sum("runc");
  const prep = sum("skel", "prep:skel", "prep:skel-fallback-copy", "prep:copy", "prep:missing");
  const comp = sum("compress", "prep:compress");
  const recalls = Number(metrics?.recalls ?? 0);
  const tokens = Number(metrics?.tokens_saved_est ?? metrics?.tokens_saved ?? 0);
  const curate = metrics?.curate as { runs?: number; merged?: number; pruned?: number } | undefined;
  const fmt = (n: number) => n.toLocaleString();
  const tRow = (n: number, name: string, s: { runs: number; saved: number }): LayerRow => ({
    n,
    name,
    status: s.runs > 0 ? "live" : "idle",
    metric: s.runs > 0 ? `${fmt(s.runs)} runs · ${fmt(s.saved)} chars saved` : "no runs yet",
  });
  return [
    {
      title: "Compaction",
      tag: "PUSH",
      explain: "Shrinks what flows into context — output caps, skeletons, compression.",
      layers: [
        {
          n: 1,
          name: "brief — reply editing",
          status: "na",
          metric: "prompt-seam · not logged here",
        },
        tRow(2, "runc — command output cap", runc),
        tRow(3, "prep / skel — file → agent input", prep),
        {
          n: 4,
          name: "machine-minimal — agent prompts",
          status: "na",
          metric: "conductor-seam · not logged here",
        },
        tRow(5, "compress — doc output", comp),
        {
          n: 6,
          name: "compact — running context",
          status: "na",
          metric: "compactor-owned · not logged here",
        },
      ],
    },
    {
      title: "Retrieval",
      tag: "PULL",
      explain: "Pulls the right durable memory into context, ranked by meaning.",
      layers: [
        {
          n: 7,
          name: "recall — store → context",
          status: recalls > 0 ? "live" : "idle",
          metric:
            recalls > 0
              ? `${fmt(recalls)} recalls · ${fmt(tokens)} tokens saved`
              : "no recalls yet",
        },
      ],
    },
    {
      title: "Curation",
      tag: "PERSIST",
      explain: "Keeps the store lean — dedupe, prune, consolidate.",
      layers: [
        {
          n: 8,
          name: "curate — dream consolidation",
          status: (curate?.runs ?? 0) > 0 ? "live" : "idle",
          metric:
            (curate?.runs ?? 0) > 0
              ? `${fmt(curate?.runs ?? 0)} runs · ${fmt(curate?.merged ?? 0)} merged · ${fmt(curate?.pruned ?? 0)} pruned`
              : "wired · not scheduled",
        },
      ],
    },
  ];
}

function LayerPill({ status }: { status: LayerStatus }) {
  const cls =
    status === "live" ? "text-accent-2" : status === "idle" ? "text-accent" : "text-text-3";
  const label = status === "na" ? "n/a" : status;
  return (
    <span className={`inline-flex items-center gap-1.5 text-[10px] ${cls}`}>
      <span className="w-1.5 h-1.5 rounded-full bg-current" />
      {label}
    </span>
  );
}

export default function MemoryPane({ config }: { config: DaemonConfig }) {
  const [rows, setRows] = useState<MemrightListRow[]>([]);
  const [metrics, setMetrics] = useState<MemrightMetrics | null>(null);
  const [health, setHealth] = useState<MemrightHealth | null>(null);
  const [activity, setActivity] = useState<MemrightActivity | null>(null);
  const [updatedAt, setUpdatedAt] = useState<string>("");
  const [hits, setHits] = useState<MemrightRecallHit[] | null>(null);
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [content, setContent] = useState("");
  const [accessCount, setAccessCount] = useState<number | null>(null);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [refreshing, setRefreshing] = useState<boolean>(false);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      const [listRows, m, h, act] = await Promise.all([
        memrightList(config),
        memrightMetrics(config),
        memrightHealth(config),
        memrightActivity(config),
      ]);
      setRows(listRows);
      setMetrics(m);
      setHealth(h);
      setActivity(act);
      setUpdatedAt(new Date().toLocaleTimeString());
      setError(null);
      setRefreshing(false);
    } catch (e) {
      setError(e);
      setRefreshing(false);
    }
  }, [config]);

  useEffect(() => {
    void refresh();
    // Live feed: chips + activity auto-refresh, matching the memright
    // resident dashboard's 10s cadence.
    const timer = setInterval(() => void refresh(), 10_000);
    return () => clearInterval(timer);
  }, [refresh]);

  const openEntry = useCallback(
    async (id: string) => {
      setSelectedId(id);
      setEditing(false);
      setCreating(false);
      setConfirmDelete(false);
      try {
        // The /memright/get fetch IS the effectiveness signal — the
        // store counts this access.
        const entry = await memrightGet(config, id);
        setContent(entry.content);
        setAccessCount(entry.access_count);
        setError(null);
      } catch (e) {
        setContent("");
        setAccessCount(null);
        setError(e);
      }
    },
    [config],
  );

  const runSearch = useCallback(async () => {
    const q = query.trim();
    if (!q) {
      setHits(null);
      return;
    }
    try {
      setHits(await memrightRecall(config, q));
      setError(null);
    } catch (e) {
      setError(e);
    }
  }, [config, query]);

  const saveEdit = useCallback(async () => {
    if (!selectedId) return;
    setBusy(true);
    try {
      // Ids are `scope/name`; put() re-derives the id from (scope, name),
      // so split the prefix back out or the save would fork a new
      // `scope/scope/name` entry instead of updating in place.
      const slash = selectedId.indexOf("/");
      const scope = slash > 0 ? selectedId.slice(0, slash) : "global";
      const name = slash > 0 ? selectedId.slice(slash + 1) : selectedId;
      await memrightPut(config, { name, content: draft, scope });
      setContent(draft);
      setEditing(false);
      await refresh();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }, [config, selectedId, draft, refresh]);

  const createEntry = useCallback(async () => {
    const name = newName.trim();
    if (!name || !draft.trim()) return;
    setBusy(true);
    try {
      const res = await memrightPut(config, { name, content: draft });
      setCreating(false);
      setNewName("");
      await refresh();
      await openEntry(res.put);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }, [config, newName, draft, refresh, openEntry]);

  const deleteEntry = useCallback(async () => {
    if (!selectedId) return;
    setBusy(true);
    try {
      await memrightDelete(config, selectedId);
      setSelectedId(null);
      setContent("");
      setConfirmDelete(false);
      await refresh();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }, [config, selectedId, refresh]);

  const visibleRows = useMemo(() => {
    if (!hits) return rows;
    const byId = new Map(rows.map((r) => [r.id, r]));
    return hits.map(
      (h) =>
        byId.get(h.id) ?? {
          id: h.id,
          tier: "?",
          chars: h.skel.length,
          access: 0,
          inject: 0,
        },
    );
  }, [rows, hits]);

  const degraded = health != null && health.embedder_dim === DEGRADED_EMBEDDER_DIM;

  return (
    <section className="flex flex-col flex-1 min-h-0 overflow-hidden" data-testid="memory-pane">
      <div className="flex items-center gap-3 px-6 py-4 border-b border-line flex-shrink-0">
        <h2 className="text-[16px] font-bold text-text">Memory</h2>
        {metrics ? (
          <>
            <span className="rounded-full bg-panel px-2.5 py-0.5 text-[11px] text-text-2 border border-line">
              {String(metrics.recalls ?? 0)} recalls
            </span>
            <span className="rounded-full bg-panel px-2.5 py-0.5 text-[11px] text-text-2 border border-line">
              {String(metrics.tokens_saved ?? 0)} tokens saved
            </span>
          </>
        ) : null}
        {updatedAt ? <span className="text-[10px] text-text-3">updated {updatedAt}</span> : null}
        {health ? (
          <span
            data-testid="embedder-chip"
            className={
              degraded
                ? "rounded-full px-2.5 py-0.5 text-[11px] font-bold border border-accent text-accent"
                : "rounded-full bg-panel px-2.5 py-0.5 text-[11px] text-text-2 border border-line"
            }
            title={
              degraded
                ? "dim 256 = degraded hash embedder — the engine is misbuilt"
                : "embedder dimension"
            }
          >
            {degraded ? "⚠ " : ""}dim {health.embedder_dim}
          </span>
        ) : null}
        <button
          type="button"
          className="ml-auto h-8 rounded-lg border border-line bg-panel px-3 text-[12px] text-text-2 hover:border-line-2 hover:text-text"
          onClick={() => {
            setCreating(true);
            setEditing(false);
            setSelectedId(null);
            setDraft("");
            setNewName("");
          }}
        >
          + new
        </button>
      </div>
      {error ? <MemoryErrorBanner error={error} onDismiss={() => setError(null)} /> : null}
      <PaneLoading loading={refreshing} label="Refreshing memory store..." deferMs={150} />
      <div
        className="grid grid-cols-1 md:grid-cols-3 gap-3 px-6 py-3 border-b border-line flex-shrink-0"
        data-testid="memory-families"
      >
        {buildFamilies(metrics).map((f) => (
          <div key={f.title} className="rounded-lg border border-line bg-panel px-3 py-2">
            <div className="flex items-baseline gap-2">
              <span className="text-[13px] font-bold text-text">{f.title}</span>
              <span className="text-[9px] tracking-[0.12em] text-text-3 font-mono">{f.tag}</span>
            </div>
            <div className="text-[10px] text-text-3 pb-1.5">{f.explain}</div>
            {f.layers.map((l) => (
              <div
                key={l.n}
                className="flex items-center gap-2 py-[3px] text-[11px] border-t border-line"
              >
                <span className="font-mono text-text-3 w-5">L{l.n}</span>
                <span className="flex-1 text-text-2 truncate" title={l.name}>
                  {l.name}
                </span>
                <span
                  className="text-text-3 tabular-nums truncate max-w-[45%] text-right"
                  title={l.metric}
                >
                  {l.metric}
                </span>
                <LayerPill status={l.status} />
              </div>
            ))}
          </div>
        ))}
      </div>
      <div className="flex flex-1 min-h-0">
        <div className="w-[380px] flex flex-col border-r border-line min-h-0">
          <div className="flex gap-2 px-4 py-3 border-b border-line">
            <input
              aria-label="Search memories"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void runSearch();
              }}
              placeholder="Semantic search…"
              className="flex-1 h-8 rounded-lg border border-line bg-panel px-2.5 text-[12px] text-text outline-none focus:border-accent"
            />
            <button
              type="button"
              onClick={() => void runSearch()}
              className="h-8 rounded-lg border border-line bg-panel px-3 text-[12px] text-text-2 hover:text-text"
            >
              Search
            </button>
            {hits ? (
              <button
                type="button"
                onClick={() => setHits(null)}
                className="h-8 rounded-lg border border-line bg-panel px-2 text-[12px] text-text-3 hover:text-text"
              >
                Clear
              </button>
            ) : null}
          </div>
          <div className="flex-1 overflow-y-auto">
            <Table label="Memory entries" dense>
              <TableHead>
                <TableRow>
                  <TableHeaderCell>id</TableHeaderCell>
                  <TableHeaderCell>tier</TableHeaderCell>
                  <TableHeaderCell align="end">chars</TableHeaderCell>
                  <TableHeaderCell align="end">inject</TableHeaderCell>
                  <TableHeaderCell align="end">access</TableHeaderCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {visibleRows.map((r) => (
                  <TableRow key={r.id} selected={r.id === selectedId} onActivate={() => void openEntry(r.id)}>
                    <TableCell rowHeader className="font-mono truncate max-w-[170px]">
                      {r.id}
                    </TableCell>
                    <TableCell>{r.tier}</TableCell>
                    <TableCell numeric>{r.chars}</TableCell>
                    <TableCell numeric>{r.inject}</TableCell>
                    <TableCell numeric>{r.access}</TableCell>
                  </TableRow>
                ))}
                {visibleRows.length === 0 ? (
                  <TableMessage colSpan={5}>
                    <EmptyState icon={<Icon name="spark" size={18} />} title={hits ? "No semantic hits." : "No memory entries yet."}>{hits
                          ? "Adjust the query above to broaden the search."
                          : "Create an entry from the right panel to start recording context."}</EmptyState>
                  </TableMessage>
                ) : null}
              </TableBody>
            </Table>
          </div>
          <div
            className="border-t border-line max-h-[180px] overflow-y-auto px-4 py-2"
            data-testid="memory-activity-feed"
          >
            <div className="text-[10px] uppercase tracking-wide text-text-3 pb-1">
              Live activity
            </div>
            {activity && activity.recalls.length > 0 ? (
              activity.recalls.slice(0, 12).map((r, i) => (
                <div
                  // biome-ignore lint/suspicious/noArrayIndexKey: recall log keyed by timestamp+index; entries have no stable id.
                  key={`${r.ts}-${i}`}
                  className="flex gap-2 py-0.5 text-[11px] font-mono text-text-3"
                >
                  <span className="text-text-2">{r.ts.slice(11, 19)}</span>
                  <span className="truncate">{r.scope ?? "—"}</span>
                  <span className="ml-auto whitespace-nowrap">
                    {r.hits} hits · {r.injected_chars}ch
                  </span>
                </div>
              ))
            ) : (
              <div className="py-1 text-[11px] text-text-3">No recalls yet.</div>
            )}
          </div>
        </div>
        <div className="flex-1 flex flex-col min-h-0">
          {creating ? (
            <div className="flex flex-col flex-1 min-h-0 p-4 gap-3">
              <input
                aria-label="Entry name / id"
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                placeholder="Entry name / id"
                className="h-8 rounded-lg border border-line bg-panel px-2.5 text-[12px] font-mono text-text outline-none focus:border-accent"
              />
              <textarea
                aria-label="Content (markdown)"
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                placeholder="Content (markdown)…"
                className="flex-1 rounded-lg border border-line bg-panel p-3 text-[12px] font-mono text-text outline-none focus:border-accent resize-none"
              />
              <div className="flex gap-2">
                <button
                  type="button"
                  disabled={busy || !newName.trim() || !draft.trim()}
                  onClick={() => void createEntry()}
                  className="h-8 rounded-lg bg-accent px-4 text-[12px] font-bold text-on-accent disabled:opacity-50"
                >
                  Create
                </button>
                <button
                  type="button"
                  onClick={() => setCreating(false)}
                  className="h-8 rounded-lg border border-line px-4 text-[12px] text-text-2"
                >
                  Cancel
                </button>
              </div>
            </div>
          ) : selectedId ? (
            <div className="flex flex-col flex-1 min-h-0">
              <div className="flex items-center gap-2 px-4 py-3 border-b border-line">
                <span className="font-mono text-[12px] text-text-2 truncate">{selectedId}</span>
                {accessCount != null ? (
                  <span className="text-[11px] text-text-3">· {accessCount} reads</span>
                ) : null}
                <div className="ml-auto flex gap-2">
                  {editing ? (
                    <>
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() => void saveEdit()}
                        className="h-7 rounded-lg bg-accent px-3 text-[12px] font-bold text-on-accent disabled:opacity-50"
                      >
                        Save
                      </button>
                      <button
                        type="button"
                        onClick={() => setEditing(false)}
                        className="h-7 rounded-lg border border-line px-3 text-[12px] text-text-2"
                      >
                        Cancel
                      </button>
                    </>
                  ) : (
                    <>
                      <button
                        type="button"
                        onClick={() => {
                          setDraft(content);
                          setEditing(true);
                        }}
                        className="h-7 rounded-lg border border-line px-3 text-[12px] text-text-2 hover:text-text"
                      >
                        Edit
                      </button>
                      <button
                        type="button"
                        onClick={() => setConfirmDelete(true)}
                        className="h-7 rounded-lg border border-line px-3 text-[12px] text-text-2 hover:text-accent"
                      >
                        Delete
                      </button>
                    </>
                  )}
                </div>
              </div>
              {editing ? (
                <textarea
                  aria-label="Memory content"
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  className="flex-1 m-4 rounded-lg border border-line bg-panel p-3 text-[12px] font-mono text-text outline-none focus:border-accent resize-none"
                />
              ) : (
                <pre className="flex-1 overflow-y-auto m-4 rounded-lg border border-line bg-panel p-3 text-[12px] font-mono text-text whitespace-pre-wrap">
                  {content}
                </pre>
              )}
            </div>
          ) : (
            <div className="flex-1 flex items-center justify-center text-[13px] text-text-3">
              Select an entry, search semantically, or create one.
            </div>
          )}
        </div>
      </div>
      {confirmDelete ? (
        <ActionDialog
          title="Delete memory entry?"
          description={`Delete memory entry "${selectedId ?? ""}"? This can't be undone.`}
          role="alertdialog"
          dismissible={!busy}
          onClose={() => setConfirmDelete(false)}
          actions={[
            {
              id: "cancel",
              label: "Cancel",
              autoFocus: true,
              disabled: busy,
              onClick: () => setConfirmDelete(false),
            },
            {
              id: "confirm",
              label: "Delete",
              variant: "danger",
              disabled: busy,
              onClick: () => void deleteEntry(),
            },
          ]}
        />
      ) : null}
    </section>
  );
}

/**
 * Renders a sanitized one-line error summary plus a collapsed <details> block
 * with the full message + stack. Stack traces NEVER appear as the primary UI
 * text — the C-3 spec calls this out as a recurring leak across panes.
 */
function MemoryErrorBanner({ error, onDismiss }: { error: unknown; onDismiss: () => void }) {
  const { summary, details } = sanitizeError(error, "Memory store unavailable.");
  return (
    <div
      role="alert"
      data-testid="memory-error-banner"
      className="flex flex-col gap-1 px-6 py-2 text-[12px] text-danger border-b border-line"
    >
      <div className="flex items-center gap-2">
        <span className="flex-1 truncate">{summary}</span>
        <button
          type="button"
          aria-label="Dismiss error"
          onClick={onDismiss}
          className="cr-control-pill grid h-6 w-6 place-items-center rounded-md border-0 bg-transparent text-text-3 cursor-pointer hover:text-text"
        >
          <Icon name="close" size={11} />
        </button>
      </div>
      {details ? (
        <details className="text-text-3">
          <summary className="cursor-pointer text-[11px] hover:text-text-2">Show details</summary>
          <pre className="mt-1 max-h-32 overflow-auto whitespace-pre-wrap break-all rounded bg-panel p-2 font-mono text-[11px]">
            {details}
          </pre>
        </details>
      ) : null}
    </div>
  );
}
