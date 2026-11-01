import { useEffect, useMemo, useState } from "react";
import { searchSessions } from "@rightkit/agent-ui-core/lib/daemon";
import type { SessionSearchHit } from "@rightkit/agent-ui-core/lib/daemon";
import type { DaemonConfig, SessionRow } from "@rightkit/agent-ui-core/lib/types";
import { registerShortcut } from "@rightkit/agent-ui-core/lib/shortcuts/dispatcher";
import { visibleWorkspacePaths } from "@rightkit/agent-ui-core/lib/utils";

interface UseSessionListOptions {
  config: DaemonConfig | null;
  initialSelectedId: string | null;
  formatError: (error: unknown, fallback: string) => string;
  onStatus: (status: string) => void;
}

export function useSessionList({
  config,
  initialSelectedId,
  formatError,
  onStatus,
}: UseSessionListOptions) {
  const [sessions, setSessions] = useState<SessionRow[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(initialSelectedId);
  const [sessionSearch, setSessionSearch] = useState("");
  const [sessionSearchHits, setSessionSearchHits] = useState<SessionSearchHit[]>([]);
  const [sessionSearchResultsFor, setSessionSearchResultsFor] = useState("");
  const [sessionSearchPending, setSessionSearchPending] = useState(false);

  // Never repaint the list with a response for an older query. SessionRow
  // objects remain authoritative so search cannot replace the selected
  // session with a synthetic result.
  const currentSearchHits = useMemo(
    () =>
      sessionSearch.trim() && sessionSearchResultsFor === sessionSearch.trim()
        ? sessionSearchHits
        : [],
    [sessionSearch, sessionSearchHits, sessionSearchResultsFor],
  );

  const uniqueWorkspaces = useMemo(
    () => visibleWorkspacePaths(sessions.map((session) => session.workspaceRoot)),
    [sessions],
  );

  const sidebarSessions = useMemo(
    () =>
      visibleSidebarSessions(
        sessionSearch.trim()
          ? sessionRowsForSearch(currentSearchHits, sessions, sessionSearch)
          : sessions,
      ),
    [currentSearchHits, sessionSearch, sessions],
  );
  const sessionSearchSnippets = useMemo(
    () => sessionSearchSnippetsBySession(currentSearchHits),
    [currentSearchHits],
  );

  const selected = useMemo(
    () =>
      sessions.find((session) => session.id === selectedId) ??
      sidebarSessions.find((session) => session.id === selectedId) ??
      (sessionSearch.trim() ? sidebarSessions[0] : sessions[0]) ??
      null,
    [selectedId, sessionSearch, sessions, sidebarSessions],
  );

  useEffect(() => {
    if (!config) {
      setSessionSearchHits([]);
      setSessionSearchResultsFor("");
      setSessionSearchPending(false);
      return;
    }
    const q = sessionSearch.trim();
    if (!q) {
      setSessionSearchHits([]);
      setSessionSearchResultsFor("");
      setSessionSearchPending(false);
      return;
    }
    let cancelled = false;
    setSessionSearchPending(true);
    const timer = window.setTimeout(() => {
      void searchSessions(config, q, 30)
        .then((hits) => {
          if (!cancelled) {
            setSessionSearchHits(hits);
            setSessionSearchResultsFor(q);
          }
        })
        .catch((err) => {
          if (!cancelled) onStatus(formatError(err, "Session search failed"));
        })
        .finally(() => {
          if (!cancelled) setSessionSearchPending(false);
        });
    }, 250);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [config, formatError, onStatus, sessionSearch]);

  useEffect(() => {
    if (selected && selected.id !== selectedId) setSelectedId(selected.id);
  }, [selected, selectedId]);

  // Ctrl+1..9 selects the nth visible sidebar session. Another modifier alongside Ctrl still counts, as
  // before; with no such session the key is reported unhandled so the browser default still runs.
  useEffect(
    () =>
      registerShortcut({
        id: "sessions.select_nth",
        chord: "ctrl+digit",
        label: "Select visible session by number",
        hidden: true,
        ignoreExtraModifiers: true,
        allowComposing: true,
        allowDefaultPrevented: true,
        run: (event) => {
          const n = Number(event.key);
          const visibleSession = n >= 1 && n <= 9 ? sidebarSessions[n - 1] : undefined;
          if (!visibleSession) return false;
          setSelectedId(visibleSession.id);
          return true;
        },
      }),
    [sidebarSessions],
  );

  return {
    sessions,
    setSessions,
    selected,
    selectedId,
    setSelectedId,
    sessionSearch,
    setSessionSearch,
    sessionSearchPending,
    sessionSearchSnippets,
    sidebarSessions,
    uniqueWorkspaces,
  };
}

export function sessionRowsForSearch(
  hits: SessionSearchHit[],
  sessions: SessionRow[],
  query: string,
) {
  const byId = new Map(sessions.map((session) => [session.id, session]));
  const rows: SessionRow[] = [];
  const seen = new Set<string>();

  for (const hit of hits) {
    const existing = byId.get(hit.sessionId);
    const row =
      existing ??
      ({
        id: hit.sessionId,
        title: hit.title || hit.sessionId,
        model: "history",
        workspaceRoot: "Search results",
        createdAt: "",
        updatedAt: "",
        status: "dormant",
        hasHistory: true,
      } satisfies SessionRow);
    if (!seen.has(row.id)) {
      rows.push(row);
      seen.add(row.id);
    }
  }

  const localQuery = query.trim().toLowerCase();
  for (const session of sessions) {
    if (seen.has(session.id)) continue;
    if (
      session.title.toLowerCase().includes(localQuery) ||
      session.workspaceRoot.toLowerCase().includes(localQuery)
    ) {
      rows.push(session);
      seen.add(session.id);
    }
  }

  return rows;
}

export function sessionSearchSnippetsBySession(hits: SessionSearchHit[]) {
  const snippets = new Map<string, string[]>();
  for (const hit of hits) {
    // The engine wraps matches in rightkit-search's SOH/STX markers; the sidebar shows plain text.
    const snippet = hit.snippet.replaceAll("\u0001", "").replaceAll("\u0002", "").trim();
    if (!snippet) continue;
    const existing = snippets.get(hit.sessionId) ?? [];
    existing.push(snippet);
    snippets.set(hit.sessionId, existing);
  }
  return snippets;
}

export function visibleSidebarSessions(rows: SessionRow[]) {
  return rows.filter(
    (row) =>
      row.id !== "default" &&
      !row.parentSessionId &&
      (row.hasHistory !== false || (row.driverKind ?? "native") !== "native") &&
      !isPlaceholderSessionTitle(row.title),
  );
}

export function isPlaceholderSessionTitle(title: string | null | undefined) {
  const normalized = (title ?? "").trim().toLowerCase();
  return normalized === "" || normalized === "new session";
}
