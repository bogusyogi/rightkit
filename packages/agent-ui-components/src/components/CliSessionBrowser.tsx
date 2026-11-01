import { ModalShell } from "@rightkit/react-shell/react";
import { useCallback, useEffect, useMemo, useState } from "react";

import { formatAgeLabel } from "@rightkit/agent-ui-core/lib/cliProviders";
import { workspaceMenuLabel } from "@rightkit/agent-ui-core/lib/utils";

export type CliAgent = "claude" | "codex";

export interface CliSessionSummary {
  /** Claude session UUID or Codex thread id. */
  sessionId: string;
  agent: CliAgent;
  cwd: string;
  firstPrompt: string;
  startedAt: string;
  lastActivityAt: string;
  /** Claude message count; Codex completed-turn count. */
  messageCount: number;
  model?: string;
  cliVersion?: string;
  /** Claude JSONL size; Codex total rollout size. */
  fileSize: number;
  filePath: string;
  rolloutPaths?: string[];
  hasSubagents: boolean;
  alreadyImported: boolean;
  existingRunId?: string;
}

export interface DiscoverResult {
  sessions: CliSessionSummary[];
  total: number;
  truncated: boolean;
}

export interface ImportResult {
  runId: string;
  sessionId: string;
  eventsImported: number;
  eventsSkipped: number;
  usageIncomplete: boolean;
  skippedSubtypes: Record<string, number>;
}

export interface SyncResult {
  newEvents: number;
  newWatermark?: { offset: number; mtimeNs: number; fileSize: number; lastUuid?: string };
  newRollouts?: string[];
  usageIncomplete: boolean;
}

/**
 * Renderer seam for the donor's `invoke` calls. The daemon remains the owner
 * of discovery, import, and synchronization; this component only owns the
 * modal projection and the interaction state around those calls.
 */
export interface CliSessionBrowserClient {
  discoverSessions: (cwd: string, agent: CliAgent) => Promise<DiscoverResult>;
  importSession: (sessionId: string, cwd: string, agent: CliAgent) => Promise<ImportResult>;
  syncSession: (runId: string) => Promise<SyncResult>;
}

export interface CliSessionBrowserProps {
  client: CliSessionBrowserClient;
  cwd: string;
  onClose: () => void;
  onImported: (runId: string) => void;
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function formatRelativeTime(value: string): string {
  const timestamp = Date.parse(value);
  return formatAgeLabel(Number.isFinite(timestamp) ? Math.max(0, Date.now() - timestamp) : 0);
}

function isStoredAgent(value: string | null): value is CliAgent {
  return value === "claude" || value === "codex";
}

/** Effective cwd for import — use the session's own cwd. */
function importCwd(session: CliSessionSummary, fallbackCwd: string): string {
  return session.cwd || fallbackCwd;
}

export function CliSessionBrowser({ client, cwd, onClose, onImported }: CliSessionBrowserProps) {
  const [sessions, setSessions] = useState<CliSessionSummary[]>([]);
  const [totalSessions, setTotalSessions] = useState(0);
  const [truncated, setTruncated] = useState(false);
  const [loading, setLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState("");
  const [importingId, setImportingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [warning, setWarning] = useState<string | null>(null);
  const [importingAll, setImportingAll] = useState(false);
  // Agent toggle: localStorage-backed, defaults to "claude" for new users.
  const [agent, setAgentState] = useState<CliAgent>(() => {
    if (typeof window === "undefined") return "claude";
    const stored = window.localStorage.getItem("cliImport_agent");
    return isStoredAgent(stored) ? stored : "claude";
  });
  const [selectedProject, setSelectedProject] = useState<string | null>(null);

  const isShowAll = !cwd || cwd === "/";

  const setAgent = useCallback(
    (next: CliAgent) => {
      if (next === agent) return;
      setAgentState(next);
      if (typeof window !== "undefined") {
        window.localStorage.setItem("cliImport_agent", next);
      }
    },
    [agent],
  );

  const discoverSessions = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await client.discoverSessions(cwd, agent);
      setSessions(result.sessions);
      setTotalSessions(result.total);
      setTruncated(result.truncated);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setLoading(false);
    }
  }, [agent, client, cwd]);

  useEffect(() => {
    void discoverSessions();
  }, [discoverSessions]);

  const projects = useMemo(() => {
    const cwdMap = new Map<string, number>();
    for (const session of sessions) {
      if (session.cwd) cwdMap.set(session.cwd, (cwdMap.get(session.cwd) ?? 0) + 1);
    }
    return Array.from(cwdMap.entries())
      .sort((a, b) => b[1] - a[1])
      .map(([path, count]) => ({ path, label: workspaceMenuLabel(path), count }));
  }, [sessions]);

  const filtered = useMemo(() => {
    let list = sessions;
    if (selectedProject) list = list.filter((session) => session.cwd === selectedProject);
    const query = searchQuery.trim().toLowerCase();
    if (query) {
      list = list.filter(
        (session) =>
          session.firstPrompt.toLowerCase().includes(query) ||
          (session.model ?? "").toLowerCase().includes(query) ||
          (session.cwd ?? "").toLowerCase().includes(query),
      );
    }
    return list;
  }, [searchQuery, selectedProject, sessions]);

  const newCount = filtered.filter((session) => !session.alreadyImported).length;

  const importSession = useCallback(
    async (session: CliSessionSummary) => {
      if (importingId) return;
      setImportingId(session.sessionId);
      setError(null);
      setWarning(null);
      try {
        const result = await client.importSession(session.sessionId, importCwd(session, cwd), agent);
        if (result.usageIncomplete) setWarning("Usage data is incomplete for this import.");
        await discoverSessions();
        onImported(result.runId);
      } catch (reason) {
        setError(String(reason));
      } finally {
        setImportingId(null);
      }
    },
    [agent, client, cwd, discoverSessions, importingId, onImported],
  );

  const syncSession = useCallback(
    async (runId: string) => {
      setImportingId(runId);
      setError(null);
      setWarning(null);
      try {
        const result = await client.syncSession(runId);
        if (result.usageIncomplete) {
          setWarning("Usage data is incomplete for this sync.");
        } else if (result.newRollouts && result.newRollouts.length > 0) {
          setWarning(`${result.newRollouts.length} new rollout${result.newRollouts.length === 1 ? "" : "s"} imported.`);
        }
        await discoverSessions();
      } catch (reason) {
        setError(String(reason));
      } finally {
        setImportingId(null);
      }
    },
    [client, discoverSessions],
  );

  const importAllNew = useCallback(async () => {
    const newSessions = filtered.filter((session) => !session.alreadyImported);
    if (newSessions.length === 0) return;
    setImportingAll(true);
    setError(null);
    setWarning(null);
    let lastRunId: string | null = null;
    try {
      for (const session of newSessions) {
        setImportingId(session.sessionId);
        const result = await client.importSession(session.sessionId, importCwd(session, cwd), agent);
        lastRunId = result.runId;
        if (result.usageIncomplete) setWarning("Usage data is incomplete for this import.");
      }
      await discoverSessions();
      if (lastRunId) onImported(lastRunId);
    } catch (reason) {
      setError(String(reason));
      await discoverSessions().catch(() => undefined);
    } finally {
      setImportingId(null);
      setImportingAll(false);
    }
  }, [agent, client, cwd, discoverSessions, filtered, onImported]);

  // Dialog semantics, Escape, backdrop dismissal, focus trap and focus restore are RightKit's ModalShell.
  return (
    <ModalShell
      label="Import CLI sessions"
      onClose={onClose}
      width={672}
      className="relative flex max-h-[80vh] w-full max-w-2xl flex-col overflow-hidden rounded-xl p-0"
      overlayClassName="bg-black/55 p-6 backdrop-blur-sm"
    >
      <div className="relative flex min-h-0 flex-1 flex-col" data-testid="cli-session-browser">
        <div className="border-b border-line px-6 py-4">
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <h2 className="text-base font-semibold text-text">
                {agent === "codex" ? "Import Codex sessions" : "Import Claude sessions"}
              </h2>
              <p className="mt-0.5 text-xs text-text-3">
                {isShowAll ? "All projects" : cwd} · {truncated
                  ? agent === "codex"
                    ? `${sessions.length} threads found across ${totalSessions} files`
                    : `${sessions.length} shown of ${totalSessions}`
                  : `${sessions.length} found`}
              </p>
            </div>
            <button
              type="button"
              className="rounded-md p-1.5 text-text-3 transition-colors hover:bg-panel-2 hover:text-text"
              onClick={onClose}
              aria-label="Close"
            >
              <span aria-hidden="true" className="text-xl leading-none">×</span>
            </button>
          </div>

          <fieldset className="m-0 mt-3 inline-flex rounded-md border border-line p-0.5" aria-label="CLI agent">
            {(["claude", "codex"] as const).map((option) => (
              <button
                key={option}
                type="button"
                className={`rounded px-3 py-1 text-xs font-medium transition-colors ${
                  agent === option ? "bg-accent text-on-accent" : "text-text-3 hover:text-text"
                }`}
                aria-pressed={agent === option}
                onClick={() => setAgent(option)}
              >
                {option === "claude" ? "Claude" : "Codex"}
              </button>
            ))}
          </fieldset>

          {isShowAll && projects.length > 1 ? (
            <nav className="mt-3 flex items-center gap-1.5 overflow-x-auto" aria-label="Project filter">
              <button
                type="button"
                className={`shrink-0 rounded-md px-2.5 py-1 text-xs font-medium ${selectedProject === null ? "bg-accent text-on-accent" : "text-text-3 hover:bg-panel-2 hover:text-text"}`}
                aria-pressed={selectedProject === null}
                onClick={() => setSelectedProject(null)}
              >
                All ({sessions.length})
              </button>
              {projects.map((project) => (
                <button
                  key={project.path}
                  type="button"
                  className={`shrink-0 rounded-md px-2.5 py-1 text-xs font-medium ${selectedProject === project.path ? "bg-accent text-on-accent" : "text-text-3 hover:bg-panel-2 hover:text-text"}`}
                  aria-pressed={selectedProject === project.path}
                  title={project.path}
                  onClick={() => setSelectedProject(project.path)}
                >
                  {project.label} ({project.count})
                </button>
              ))}
            </nav>
          ) : null}
        </div>

        <div className="border-b border-line px-6 py-3">
          <label className="relative block">
            <span className="sr-only">Search CLI sessions</span>
            <span aria-hidden="true" className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-text-3">⌕</span>
            <input
              type="search"
              value={searchQuery}
              onChange={(event) => setSearchQuery(event.target.value)}
              placeholder="Search sessions…"
              className="w-full rounded-lg border border-line bg-bg py-2 pl-10 pr-3 text-sm text-text placeholder:text-text-3 focus:border-accent focus:outline-none focus:ring-1 focus:ring-accent"
            />
          </label>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-6 py-3">
          {loading ? (
            <div className="flex items-center justify-center py-12" role="status" aria-label="Loading sessions">
              <span className="h-5 w-5 animate-spin rounded-full border-2 border-accent/30 border-t-accent" />
            </div>
          ) : error && sessions.length === 0 ? (
            <div className="flex flex-col items-center gap-2 py-12 text-center">
              <span aria-hidden="true" className="text-2xl text-danger">!</span>
              <p className="text-sm text-danger">{error}</p>
            </div>
          ) : filtered.length === 0 ? (
            <div className="flex flex-col items-center gap-2 py-12 text-center">
              <span aria-hidden="true" className="text-2xl text-text-3">□</span>
              <p className="text-sm text-text-3">
                {agent === "codex" ? "No Codex sessions found." : "No Claude sessions found."}
              </p>
            </div>
          ) : (
            <div className="space-y-2">
              {filtered.map((session) => {
                const isImporting = importingId === session.sessionId;
                const existingRunId = session.existingRunId;
                const isImported = session.alreadyImported;
                return (
                  <div key={session.sessionId} className="group rounded-lg border border-line p-3 transition-colors hover:bg-panel-2" data-testid={`cli-session-${session.sessionId}`}>
                    <div className="flex items-start justify-between gap-3">
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-2">
                          <span
                            className={`inline-block h-2 w-2 shrink-0 rounded-full ${isImported ? "bg-accent-2" : "bg-accent"}`}
                            role="img"
                            aria-label={isImported ? "Imported" : "Not imported"}
                          />
                          <span className="shrink-0 text-xs text-text-3">{formatRelativeTime(session.lastActivityAt)}</span>
                          {isShowAll && !selectedProject && session.cwd ? (
                            <span className="max-w-[140px] shrink-0 truncate rounded bg-panel-2 px-1.5 py-0.5 text-[10px] font-medium text-text-3" title={session.cwd}>
                              {workspaceMenuLabel(session.cwd)}
                            </span>
                          ) : null}
                          {session.model ? <span className="ml-auto shrink-0 rounded bg-panel-2 px-1.5 py-0.5 text-[10px] font-medium text-text-3">{session.model}</span> : null}
                        </div>
                        <p className="mt-1 truncate text-sm font-medium text-text">{session.firstPrompt || "—"}</p>
                        <div className="mt-1 flex items-center gap-2 text-xs text-text-3">
                          <span>{session.agent === "codex" ? `${session.messageCount} turn${session.messageCount === 1 ? "" : "s"}` : `${session.messageCount} message${session.messageCount === 1 ? "" : "s"}`}</span>
                          {session.agent === "codex" && session.rolloutPaths && session.rolloutPaths.length > 1 ? <><span>·</span><span>{session.rolloutPaths.length} rollouts</span></> : null}
                          <span>·</span>
                          <span>{formatSize(session.fileSize)}</span>
                          {session.hasSubagents ? <><span>·</span><span>Subagents</span></> : null}
                          {isImported && existingRunId ? <><span>·</span><span className="text-accent-2">Already imported</span></> : null}
                        </div>
                      </div>

                      <div className="flex shrink-0 items-center gap-1.5 pt-0.5">
                        {isImported && existingRunId ? (
                          <>
                            <button
                              type="button"
                              className="rounded-md border border-line px-2.5 py-1 text-xs font-medium text-text transition-colors hover:bg-panel-2 disabled:opacity-50"
                              onClick={() => void syncSession(existingRunId)}
                              disabled={Boolean(importingId)}
                            >
                              {importingId === existingRunId ? <span role="status" aria-label="Syncing" className="inline-block h-3 w-3 animate-spin rounded-full border-2 border-accent/30 border-t-accent" /> : "Sync"}
                            </button>
                            <button type="button" className="rounded-md border border-line px-2.5 py-1 text-xs font-medium text-text transition-colors hover:bg-panel-2" onClick={() => onImported(existingRunId)}>
                              Open
                            </button>
                          </>
                        ) : (
                          <button
                            type="button"
                            className="rounded-md bg-accent px-2.5 py-1 text-xs font-medium text-on-accent transition-colors hover:bg-accent/90 disabled:opacity-50"
                            onClick={() => void importSession(session)}
                            disabled={Boolean(importingId)}
                          >
                            {isImporting ? <span role="status" aria-label="Importing" className="inline-block h-3 w-3 animate-spin rounded-full border-2 border-on-accent/30 border-t-on-accent" /> : "Import"}
                          </button>
                        )}
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>

        {!loading && filtered.length > 0 ? (
          <div className="flex items-center justify-between gap-3 border-t border-line px-6 py-3">
            {error ? <p className="max-w-[60%] truncate text-xs text-danger">{error}</p> : warning ? <p className="max-w-[60%] truncate text-xs text-warning">{warning}</p> : <span />}
            {newCount > 0 ? (
              <button
                type="button"
                className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-on-accent transition-colors hover:bg-accent/90 disabled:opacity-50"
                onClick={() => void importAllNew()}
                disabled={Boolean(importingId) || importingAll}
              >
                {importingAll ? "Importing…" : `Import all new (${newCount})`}
              </button>
            ) : null}
          </div>
        ) : null}
      </div>
    </ModalShell>
  );
}

export default CliSessionBrowser;
