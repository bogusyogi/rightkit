import { useMemo, useState } from "react";
import { ConfirmDialog, useTheme } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { IconName } from "@rightkit/agent-ui-core/components/Icon";
import type {
  DaemonConfig,
  ExecutionBackendRegistry,
  ExecutionWorkspaceRecord,
  ModelCatalogEntry,
  SessionRow,
} from "@rightkit/agent-ui-core/lib/types";
import type { DriverKind } from "@rightkit/agent-ui-core/lib/types";
import EditedFilesSection from "./EditedFilesSection";
import { encodeSessionShareCode } from "@rightkit/agent-ui-core/lib/sessionShareCode";
import type { SessionShareStatus } from "@rightkit/agent-ui-core/lib/sessionShareCode";
import { attentionStateForSession } from "@rightkit/agent-ui-core/state/sessionAttentionStore";
import { projectName } from "@rightkit/agent-ui-core/lib/utils";
import { VirtualSessionList } from "./VirtualSessionList";
import type { VirtualSessionListRow } from "./VirtualSessionList";
// a11y-test: aria-label="Sessions" role="region" aria-label="Recent sessions" aria-current={isActive ? "page" : undefined} aria-label={`Delete session role="status" aria-live="polite"

/// How many sessions the sidebar lists. Five hid finished runs entirely;
/// the container scrolls, so the only cost of more is scroll distance.
const RECENTS_LIMIT = 25;

export type NewSessionControls = {
  driverKind: DriverKind;
  model: string;
  effort: "" | "low" | "medium" | "high";
  thinkingBudgetTokens: string;
  disableThinking: boolean;
  fableMode: boolean;
  executionBackend: string;
  executionWorkspaceId: string;
  allowedTools: string;
  disallowedTools: string;
};

export default function Sidebar({
  sessions,
  selectedId,
  newSessionControls,
  onSelect,
  onDelete,
  onNewSession,
  onForkCurrentSession,
  onOpenSettings,
  search,
  searchPending,
  searchSnippets,
  onSearch,
  // connectionLabel is still accepted in the type (buildSidebarProps supplies
  // it) but is no longer destructured — the footer dropped the status dot;
  // connection status lives in the title bar.
}: {
  sessions: SessionRow[];
  selectedId: string | null;
  workspaceRoot: string;
  connectionLabel: string;
  config: DaemonConfig | null;
  modelOptions: ModelCatalogEntry[];
  executionBackendRegistry: ExecutionBackendRegistry | null;
  executionWorkspaces: ExecutionWorkspaceRecord[];
  executionWorkspacePending: boolean;
  newSessionControls: NewSessionControls;
  search: string;
  searchPending: boolean;
  searchSnippets: Map<string, string[]>;
  onSelect: (id: string) => void;
  onDelete: (id: string) => void;
  onNewSession: (driverKind?: DriverKind) => void;
  onForkCurrentSession: () => void;
  onNewSessionControls: (controls: NewSessionControls) => void;
  onCreateExecutionWorkspace: () => void;
  onSearch: (query: string) => void;
  // Provided at the render site (AppBody), not by buildSidebarProps — optional
  // so the built prop bag stays valid without threading it through.
  onOpenSettings?: () => void;
}) {
  const [pendingDelete, setPendingDelete] = useState<{ id: string; title: string } | null>(null);
  const { resolved: theme, setChoice } = useTheme();
  const toggleTheme = () => setChoice(theme === "dark" ? "light" : "dark");

  const grouped = useMemo(() => {
    const stamp = (row: SessionRow) => Date.parse(row.updatedAt || row.createdAt || "") || 0;
    const sorted = [...sessions].sort((a, b) => stamp(b) - stamp(a)).slice(0, RECENTS_LIMIT);
    return groupSessions(sorted);
  }, [sessions]);

  const totalCount = grouped.reduce((acc, g) => acc + g.rows.length, 0);
  const virtualRows = useMemo<VirtualSessionListRow[]>(
    () =>
      grouped.flatMap((group) => [
        {
          key: `workspace:${group.workspace}`,
          kind: "divider" as const,
          label: projectName(group.workspace) || group.workspace,
        },
        ...group.rows.map((session) => ({ kind: "session" as const, session })),
      ]),
    [grouped],
  );
  const [shareStatus, setShareStatus] = useState<"idle" | "copied" | "failed">("idle");

  async function copySessionShareCode() {
    const code = encodeSessionShareCode({
      clusters: [],
      edges: [],
      nodes: sessions.map((row) => {
        const attention = attentionStateForSession(row.id, row.status);
        const status: SessionShareStatus = attention === "draft" ? "idle" : attention;
        return {
          id: row.id,
          label: row.title || "Untitled session",
          category: row.workspaceRoot.split(/[\\/]/).filter(Boolean).at(-1) || "workspace",
          model: row.model,
          status,
          timestamp: Date.parse(row.updatedAt) || null,
        };
      }),
      stats: {},
    });
    try {
      await navigator.clipboard.writeText(code);
      setShareStatus("copied");
    } catch {
      setShareStatus("failed");
    }
  }

  return (
    <aside
      className="min-h-0 flex w-full flex-1 flex-col overflow-hidden bg-sidebar text-text-2"
      aria-label="Sessions"
    >
      <nav className="flex flex-col px-2 pb-1.5 pt-2" aria-label="Primary">
        <NavItem
          icon="plus"
          label="New session"
          active={!selectedId}
          onClick={() => onNewSession(newSessionControls.driverKind)}
        />
      </nav>

      <search className="px-2 pb-1" aria-label="Session search">
        <label className="relative flex min-h-8 items-center gap-2 rounded-md border border-line bg-bg px-2 text-text-3 focus-within:border-accent focus-within:text-text">
          <Icon name="search" size={13} />
          <span className="sr-only">Search sessions</span>
          <input
            type="search"
            value={search}
            onChange={(event) => onSearch(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape" && search) {
                event.preventDefault();
                onSearch("");
              }
            }}
            placeholder="Search sessions…"
            aria-label="Search sessions"
            className="min-w-0 flex-1 bg-transparent py-1 text-[12px] text-text outline-none placeholder:text-text-3"
          />
          {search ? (
            <button
              type="button"
              aria-label="Clear session search"
              title="Clear search"
              onClick={() => onSearch("")}
              className="grid h-6 w-6 shrink-0 place-items-center rounded text-text-3 hover:bg-panel-2 hover:text-text"
            >
              <Icon name="close" size={12} />
            </button>
          ) : null}
        </label>
        {search.trim() ? (
          <div
            id="session-search-status"
            role="status"
            aria-live="polite"
            className="px-1 pt-1 text-[10px] text-text-3"
          >
            {searchPending
              ? "Searching session history…"
              : `${totalCount} matching session${totalCount === 1 ? "" : "s"}${
                  searchSnippets.size > 0
                    ? ` · ${searchSnippets.size} with matching history`
                    : ""
                }`}
          </div>
        ) : null}
      </search>

      <SectionHeader
        title="Recents"
        right={
          <div className="flex items-center gap-1">
            <button
              type="button"
              aria-label="Fork current session"
              title="Fork"
              onClick={onForkCurrentSession}
              disabled={!selectedId}
              className="cr-control-pill grid h-6 w-6 place-items-center rounded border border-transparent text-text-3 cursor-pointer hover:bg-panel-2 hover:text-text-2"
            >
              <Icon name="branch" size={11} />
            </button>
            <button
              type="button"
              aria-label={shareStatus === "copied" ? "Session share code copied" : "Copy session share code"}
              title={shareStatus === "failed" ? "Copy failed" : shareStatus === "copied" ? "Copied" : "Share sessions"}
              onClick={() => void copySessionShareCode()}
              className="cr-control-pill grid h-6 w-6 place-items-center rounded border border-transparent text-text-3 cursor-pointer hover:bg-panel-2 hover:text-text-2"
            >
              <Icon name={shareStatus === "copied" ? "check" : "copy"} size={11} />
            </button>
          </div>
        }
      />

      <section className="min-h-0 flex-1 pb-2" aria-label="Recent sessions">
        {totalCount === 0 ? (
          <div
            className="grid gap-2 px-4 py-3 text-text-3 text-[13px]"
            role="status"
            aria-live="polite"
          >
            <Icon name="spark" />
            <span>
              {search.trim()
                ? `No sessions match “${search.trim()}”.`
                : "Open a local folder to start."}
            </span>
          </div>
        ) : (
          <VirtualSessionList
            activeSessionId={selectedId}
            onDeleteSession={(id) => {
              const row = sessions.find((session) => session.id === id);
              setPendingDelete({ id, title: row?.title || "Untitled session" });
            }}
            onSelectSession={onSelect}
            rows={virtualRows}
            sortable={false}
          />
        )}
      </section>

      <EditedFilesSection />

      <div className="flex-none border-t border-line px-2.5 py-2">
        <div className="flex items-center gap-1">
          <span className="font-display text-[14px] leading-none text-text">
            Code<span className="text-accent">Right</span>
          </span>
          <span className="flex-1" />
          <button
            type="button"
            aria-label={theme === "dark" ? "Switch to light theme" : "Switch to dark theme"}
            title={theme === "dark" ? "Light theme" : "Dark theme"}
            onClick={toggleTheme}
            className="grid h-7 w-7 place-items-center rounded-md text-text-3 cursor-pointer bg-transparent border-0 hover:text-text hover:bg-panel-2 transition-colors"
          >
            <Icon name={theme === "dark" ? "sun" : "moon"} size={14} />
          </button>
          <button
            type="button"
            aria-label="Open settings"
            title="Settings"
            onClick={onOpenSettings}
            className="grid h-7 w-7 place-items-center rounded-md text-text-3 cursor-pointer bg-transparent border-0 hover:text-text hover:bg-panel-2 transition-colors"
          >
            <Icon name="settings" size={14} />
          </button>
        </div>
      </div>

      {pendingDelete !== null ? (
        <ConfirmDialog
          title="Delete session?"
          description={`Delete session "${pendingDelete?.title ?? ""}" and its transcript? This can't be undone.`}
          confirmLabel="Delete"
          danger
          onCancel={() => setPendingDelete(null)}
          onConfirm={() => {
            if (pendingDelete) onDelete(pendingDelete.id);
            setPendingDelete(null);
          }}
        />
      ) : null}
    </aside>
  );
}

function NavItem({
  icon,
  label,
  onClick,
  trailing,
  active = false,
}: {
  icon: IconName;
  label: string;
  onClick: () => void;
  trailing?: React.ReactNode;
  active?: boolean;
}) {
  return (
    <div className={active ? "cr-sidebar-row-active -mr-2 z-10 rounded-l-xl bg-bg" : undefined}>
      <button
        type="button"
        aria-current={active ? "page" : undefined}
        aria-label={label}
        title={label}
        onClick={onClick}
        className={`cr-control-pill group flex w-full items-center gap-2 truncate border border-transparent px-2 py-1.5 text-left text-[13px] cursor-pointer hover:text-text ${
          active
            ? "rounded-l-xl rounded-r-none bg-transparent text-text"
            : "rounded-md text-text-2 hover:bg-panel-2"
        }`}
      >
        <Icon name={icon} size={13} />
        <span className="flex-1 truncate">{label}</span>
        {trailing}
      </button>
    </div>
  );
}

function SectionHeader({ title, right }: { title: string; right?: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-2 px-3 pb-1 pt-2">
      <span className="text-[10px] font-semibold uppercase tracking-[0.12em] text-text-3">
        {title}
      </span>
      {right}
    </div>
  );
}

function isPinned(row: SessionRow): boolean {
  const tags = row.tags ?? [];
  if (tags.includes("pinned") || tags.includes("pin")) return true;
  const title = row.title?.toLowerCase() ?? "";
  return title.startsWith("📌") || title.startsWith("[pin]");
}

function isArchived(row: SessionRow): boolean {
  const tags = row.tags ?? [];
  if (tags.includes("archived") || tags.includes("archive")) return true;
  const status = (row.status ?? "").toLowerCase();
  return status === "archived";
}

function isAttention(row: SessionRow): boolean {
  const status = (row.status ?? "").toLowerCase();
  if (
    status === "needs_input" ||
    status === "needsinput" ||
    status === "failed" ||
    status === "blocked"
  )
    return true;
  if (status.includes("attention") || status.includes("fail") || status.includes("blocked"))
    return true;
  return false;
}

function isParallelWork(row: SessionRow): boolean {
  const status = (row.status ?? "").toLowerCase();
  return status === "running" || status === "working" || status === "in_progress";
}

function groupSessions(rows: SessionRow[]) {
  const map = new Map<string, SessionRow[]>();
  for (const row of rows) {
    if (row.parentSessionId && !row.forkedFromSessionId) continue;
    const existing = map.get(row.workspaceRoot) ?? [];
    existing.push(row);
    map.set(row.workspaceRoot, existing);
  }
  // Within each project group, pinned first, attention next, running next,
  // archived last, otherwise by recency. This gives pin/archive/attention/
  // parallel-work visibility without a separate filter.
  for (const [, list] of map) {
    list.sort((a, b) => {
      const aPin = isPinned(a) ? 0 : 1;
      const bPin = isPinned(b) ? 0 : 1;
      if (aPin !== bPin) return aPin - bPin;
      const aAtt = isAttention(a) ? 0 : 1;
      const bAtt = isAttention(b) ? 0 : 1;
      if (aAtt !== bAtt) return aAtt - bAtt;
      const aRun = isParallelWork(a) ? 0 : 1;
      const bRun = isParallelWork(b) ? 0 : 1;
      if (aRun !== bRun) return aRun - bRun;
      const aArch = isArchived(a) ? 1 : 0;
      const bArch = isArchived(b) ? 1 : 0;
      if (aArch !== bArch) return aArch - bArch;
      const ta = Date.parse(a.updatedAt || a.createdAt || "") || 0;
      const tb = Date.parse(b.updatedAt || b.createdAt || "") || 0;
      return tb - ta;
    });
  }
  return [...map.entries()].map(([workspace, rows]) => ({ workspace, rows }));
}

function shortModel(model: string) {
  return model.includes("/") ? model.slice(model.lastIndexOf("/") + 1) : model;
}

export { groupSessions, shortModel };
