import { useDismiss } from "@rightkit/platform-ui/headless";
import { useRef, useState } from "react";
import type { AppShellHeaderProps } from "./types";
import Icon from "../Icon";
import { AgentIndicator, TaskIndicator } from "./RunIndicators";
import type { TaskNode } from "../../lib/types";

// Slim status strip rendered above the workbench area. The Claude Code
// reference does not include a separate header row — the project chip and
// model selector live in the chrome bar and the composer. We keep a tiny
// status pill so the engine connection state stays visible (the Engine
// badge is the only diagnostic indicator in this layout).
export default function AppShellHeader({
  headerTitle,
  headerSubtitle,
  status,
  health,
  selected,
  agentCount = 0,
  runningAgentCount = 0,
  agentsOpen = false,
  onToggleAgents,
  onRetryEngine,
  headerActions,
  tasks = [],
}: AppShellHeaderProps & { tasks?: TaskNode[] }) {
  const normalizedStatus = status.trim().toLowerCase();
  const engineConnected = health?.ok === true;
  // This affordance reports daemon connectivity, not the most recent action.
  // A stale "Load failed" message beside a green dot made the shell contradict
  // itself. Action failures remain in their own panel/toast; a healthy engine
  // always reads as connected here.
  const displayStatus = engineConnected ? "Connected" : status.trim() || "Engine unavailable";
  const statusDotClass = health?.ok
    ? "bg-accent-2"
    : normalizedStatus.includes("preview")
      ? "bg-accent"
      : "bg-danger";
  const previewMode = normalizedStatus.includes("preview");
  const engineStarting =
    normalizedStatus.includes("starting") || normalizedStatus.includes("connecting");
  const engineStale = normalizedStatus.includes("stale");
  const [statusExpanded, setStatusExpanded] = useState(false);
  const statusRootRef = useRef<HTMLDivElement | null>(null);

  useDismiss(() => setStatusExpanded(false), {
    enabled: statusExpanded,
    container: statusRootRef,
    preventDefault: false,
    stopPropagation: false,
  });

  return (
    // Rendered into the title bar's slot (TitleBar `data-cr-titlebar-slot`), so
    // this row owns no height, border or padding of its own — the host app used to
    // draw it as a second full-width bar under the window chrome.
    <div className="relative flex min-w-0 flex-1 items-center gap-2 text-[13px]">
      <span
        className="grid h-5 w-5 flex-shrink-0 place-items-center text-text-3"
        aria-hidden="true"
      >
        <Icon name="terminal" size={13} />
      </span>
      <strong className="max-w-[32vw] truncate font-medium text-text">{headerTitle}</strong>
      {headerSubtitle ? (
        <span
          className="max-w-[220px] truncate rounded-md bg-panel px-2 py-0.5 text-[11px] text-text-3"
          title={headerSubtitle}
        >
          {headerSubtitle}
        </span>
      ) : null}
      <span className="flex-1" data-tauri-drag-region />
      {headerActions}

      {/* Run indicators (Adrian, 2026-08-04). Both are always present, reading
          "0 Agents" / "0 Tasks" at rest, so the row never reflows when work
          starts. These are the only home for either count — the old bordered
          "Agents · N" button and the transcript-level todo dock were removed
          when this landed. Compact left the title bar entirely: it is a session
          action, not window chrome, and now lives in the composer behind a
          confirmation (or `/compact`). */}
      <AgentIndicator
        agentCount={agentCount}
        runningAgentCount={runningAgentCount}
        agentsOpen={agentsOpen}
        onToggleAgents={onToggleAgents}
      />
      <TaskIndicator tasks={tasks} />

      <div ref={statusRootRef} className="relative flex items-center">
        {/* Connection status lives here in the title bar (Adrian, 2026-08-04).
            Connected reads as a calm labelled chip — soft dot + "Connected" —
            that opens engine/session detail on click; the actionable states
            (starting, stale, disconnected) carry a retry. */}
        {engineConnected ? (
          <button
            type="button"
            aria-label={`Engine status: ${displayStatus}`}
            aria-expanded={statusExpanded}
            title={displayStatus}
            className="cr-control-pill inline-flex h-6 items-center gap-1.5 rounded-full border border-line bg-transparent pl-2 pr-2.5 text-[11px] font-medium text-text-3 cursor-pointer hover:bg-panel hover:text-text-2"
            onClick={() => setStatusExpanded((v) => !v)}
          >
            <span className="relative flex h-[7px] w-[7px] items-center justify-center">
              <span className="absolute inline-flex h-full w-full rounded-full bg-accent-2/40" />
              <span className={`relative h-[6px] w-[6px] rounded-full ${statusDotClass}`} />
            </span>
            <span className="max-[880px]:hidden">Connected</span>
          </button>
        ) : engineStarting ? (
          <span
            title="Engine is starting"
            className="inline-flex h-6 items-center gap-1.5 rounded-full border border-line bg-panel px-2 text-[11px] text-text-3"
          >
            <span className="h-[6px] w-[6px] rounded-full bg-warning" />
            <span>Starting engine</span>
          </span>
        ) : engineStale ? (
          <button
            type="button"
            aria-label="Stale data: reconnect engine"
            title={status}
            className="inline-flex h-6 items-center gap-1.5 rounded-full border border-warning/40 bg-[color-mix(in_srgb,var(--color-warning)_10%,transparent)] px-2 text-[11px] font-semibold text-warning cursor-pointer hover:bg-[color-mix(in_srgb,var(--color-warning)_18%,transparent)]"
            onClick={() => onRetryEngine?.()}
          >
            <span className="h-[6px] w-[6px] rounded-full bg-warning" />
            <Icon name="refresh" size={11} />
            <span>Stale data</span>
          </button>
        ) : (
          <button
            type="button"
            aria-label={previewMode ? "Reload preview" : "Retry engine"}
            title={status}
            className="cr-control-pill inline-flex h-6 items-center gap-1.5 rounded-full border border-danger/40 bg-[color-mix(in_srgb,var(--color-danger)_10%,transparent)] px-2 text-[11px] font-semibold text-danger cursor-pointer hover:bg-[color-mix(in_srgb,var(--color-danger)_18%,transparent)]"
            onClick={() => onRetryEngine?.()}
          >
            <span className={`h-[6px] w-[6px] rounded-full ${statusDotClass}`} />
            <Icon name="refresh" size={11} />
            <span className="max-[960px]:hidden">
              {previewMode ? "Reload preview" : "Retry engine"}
            </span>
          </button>
        )}

        {statusExpanded ? (
          <div className="cr-panel-reveal absolute right-0 top-8 z-30 w-[260px] rounded-lg border border-line bg-panel p-3 text-[12px] text-text-2">
            <div className="mb-2 flex items-center gap-2">
              <span className={`h-2 w-2 rounded-full ${statusDotClass}`} />
              <strong className="text-text text-[13px]">{displayStatus}</strong>
            </div>
            <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-[11px]">
              <dt className="text-text-3">Engine</dt>
              <dd>{health?.ok ? "Connected" : "Disconnected"}</dd>
              {selected ? (
                <>
                  <dt className="text-text-3">Session</dt>
                  <dd className="truncate">{selected.id}</dd>
                </>
              ) : null}
            </dl>
          </div>
        ) : null}
      </div>
    </div>
  );
}
