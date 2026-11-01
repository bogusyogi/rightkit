import { EmptyState } from "@rightkit/react-shell/react";
import { useCallback, useEffect, useRef, useState } from "react";
import { ActionDialog } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { ExecutionProcessLogs, ExecutionProcessRow, SessionRow } from "@rightkit/agent-ui-core/lib/types";

const TERMINAL_PANE_WIDTH_KEY = "cr.terminalPane.width.v1";
const MIN_PANE_WIDTH = 260;
const MAX_PANE_WIDTH = 560;
const MAX_PANE_WIDTH_FRACTION = 0.45;
const DEFAULT_PANE_WIDTH = 320;
const WIDTH_STEP = 16;

function safeViewportWidth() {
  return typeof window !== "undefined" ? window.innerWidth : DEFAULT_PANE_WIDTH;
}

function clampPaneWidth(width: number, viewportWidth: number) {
  const safeMax = Math.max(
    MIN_PANE_WIDTH,
    Math.min(MAX_PANE_WIDTH, viewportWidth * MAX_PANE_WIDTH_FRACTION),
  );
  return Math.min(Math.max(width, MIN_PANE_WIDTH), safeMax);
}

function readPersistedPaneWidth() {
  try {
    const raw = localStorage.getItem(TERMINAL_PANE_WIDTH_KEY);
    const parsed = raw ? Number(raw) : Number.NaN;
    if (Number.isFinite(parsed)) return parsed;
  } catch {
    /* ignore; in-memory default still works for this session */
  }
  return DEFAULT_PANE_WIDTH;
}

function persistPaneWidth(width: number) {
  try {
    localStorage.setItem(TERMINAL_PANE_WIDTH_KEY, String(Math.round(width)));
  } catch {
    /* ignore; in-memory width still works for this session */
  }
}

export default function TerminalPane({
  selectedSession,
  workspaceRoot,
  processes,
  selectedProcessId,
  logs,
  input,
  actionPending,
  loading,
  error,
  onSelectProcess,
  onInput,
  onCreateTerminal,
  onRefresh,
  onSendInput,
  onStopProcess,
}: {
  selectedSession: SessionRow | null;
  workspaceRoot: string;
  processes: ExecutionProcessRow[];
  selectedProcessId: string | null;
  logs: ExecutionProcessLogs | null;
  input: string;
  actionPending: string | null;
  loading: boolean;
  error: string | null;
  onSelectProcess: (id: string) => void;
  onInput: (value: string) => void;
  onCreateTerminal: () => void;
  onRefresh: () => void;
  onSendInput: () => void;
  onStopProcess: (id: string) => void;
}) {
  const selectedProcess =
    processes.find((process) => process.id === selectedProcessId) ?? processes[0] ?? null;
  const outputRef = useRef<HTMLPreElement>(null);
  const [stopCandidate, setStopCandidate] = useState<ExecutionProcessRow | null>(null);

  const [paneWidth, setPaneWidth] = useState(() =>
    clampPaneWidth(readPersistedPaneWidth(), safeViewportWidth()),
  );
  const dragStateRef = useRef<{ startX: number; startWidth: number } | null>(null);

  const applyPaneWidth = useCallback((next: number) => {
    const clamped = clampPaneWidth(next, safeViewportWidth());
    setPaneWidth(clamped);
    persistPaneWidth(clamped);
  }, []);

  useEffect(() => {
    function handlePointerMove(e: PointerEvent) {
      if (!dragStateRef.current) return;
      applyPaneWidth(dragStateRef.current.startWidth + (e.clientX - dragStateRef.current.startX));
    }
    function handlePointerUp() {
      dragStateRef.current = null;
    }
    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
    };
  }, [applyPaneWidth]);

  useEffect(() => {
    function handleWindowResize() {
      setPaneWidth((current) => clampPaneWidth(current, safeViewportWidth()));
    }
    window.addEventListener("resize", handleWindowResize);
    return () => window.removeEventListener("resize", handleWindowResize);
  }, []);

  const handleSeparatorPointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      dragStateRef.current = { startX: e.clientX, startWidth: paneWidth };
    },
    [paneWidth],
  );

  const handleSeparatorKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      if (e.key === "ArrowLeft") {
        e.preventDefault();
        applyPaneWidth(paneWidth - WIDTH_STEP);
      } else if (e.key === "ArrowRight") {
        e.preventDefault();
        applyPaneWidth(paneWidth + WIDTH_STEP);
      }
    },
    [applyPaneWidth, paneWidth],
  );

  const maxPaneWidth = Math.max(
    MIN_PANE_WIDTH,
    Math.min(MAX_PANE_WIDTH, safeViewportWidth() * MAX_PANE_WIDTH_FRACTION),
  );

  useEffect(() => {
    const node = outputRef.current;
    if (!node) return;
    node.scrollTop = node.scrollHeight;
  }, []);

  if (!workspaceRoot.trim()) {
    return (
      <section className="flex min-h-0 flex-1">
        <EmptyState icon={<Icon name="terminal" size={18} />} title="Choose a workspace first">{"The integrated terminal follows the current project root so commands run in the same workspace as the session."}</EmptyState>
      </section>
    );
  }

  return (
    <section className="flex min-h-0 min-w-0 flex-1">
      <aside
        className="flex min-w-[260px] flex-col border-r border-line bg-panel/60"
        style={{ width: paneWidth }}
      >
        <div className="flex items-center gap-2 border-b border-line px-4 py-3">
          <button
            type="button"
            className="inline-flex h-8 items-center gap-2 rounded-lg border border-accent/40 bg-accent/10 px-3 text-[12px] font-semibold text-accent hover:bg-accent/15 disabled:cursor-not-allowed disabled:opacity-45"
            disabled={!selectedSession || actionPending === "create"}
            onClick={onCreateTerminal}
          >
            <Icon name="plus" size={13} />
            {actionPending === "create" ? "Opening" : "Open terminal"}
          </button>
          <button
            type="button"
            className="grid h-8 w-8 place-items-center rounded-lg border border-line bg-transparent text-text-2 hover:bg-panel-2 hover:text-text disabled:opacity-45"
            aria-label="Refresh processes"
            title="Refresh processes"
            onClick={onRefresh}
          >
            <Icon name="refresh" size={13} />
          </button>
        </div>
        <div className="border-b border-line px-4 py-3 text-[12px] text-text-3">
          <div className="truncate">{workspaceRoot}</div>
          <div className="mt-1 truncate text-[11px]">
            {selectedSession
              ? `Session: ${selectedSession.title || selectedSession.id}`
              : "Select a session to open a terminal"}
          </div>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto p-2">
          {processes.length === 0 ? (
            <EmptyState icon={<Icon name="terminal" size={18} />} title="No processes yet">{"Open a terminal here or inspect background shells started by the agent. Live process logs and stdin stay attached in this panel."}</EmptyState>
          ) : (
            <div className="flex flex-col gap-2">
              {processes.map((process) => {
                const active = process.id === selectedProcess?.id;
                return (
                  <button
                    key={process.id}
                    type="button"
                    className={`flex flex-col gap-1 rounded-xl border px-3 py-2 text-left ${
                      active
                        ? "border-accent/50 bg-accent/10 text-text"
                        : "border-line bg-transparent text-text-2 hover:bg-panel-2 hover:text-text"
                    }`}
                    onClick={() => onSelectProcess(process.id)}
                    title={process.command}
                  >
                    <div className="flex items-center gap-2">
                      <Icon name="terminal" size={13} />
                      <span className="min-w-0 flex-1 truncate text-[12px] font-semibold">
                        {process.command}
                      </span>
                    </div>
                    <div className="flex items-center gap-2 text-[10px] uppercase tracking-wide text-text-3">
                      <span>{process.kind}</span>
                      <span>{process.backendId}</span>
                      <span>{process.statusLabel}</span>
                    </div>
                  </button>
                );
              })}
            </div>
          )}
        </div>
      </aside>
      <hr
        aria-orientation="vertical"
        aria-label="Resize terminal panel"
        aria-valuenow={Math.round(paneWidth)}
        aria-valuemin={MIN_PANE_WIDTH}
        aria-valuemax={Math.round(maxPaneWidth)}
        aria-valuetext={`${Math.round(paneWidth)} pixels wide`}
        tabIndex={0}
        className="m-0 h-auto w-1 shrink-0 cursor-col-resize touch-none self-stretch border-0 bg-line/60 outline-none hover:bg-accent/40 focus-visible:bg-accent focus-visible:ring-2 focus-visible:ring-accent/60"
        onPointerDown={handleSeparatorPointerDown}
        onKeyDown={handleSeparatorKeyDown}
      />
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <div className="flex items-center justify-between gap-3 border-b border-line px-5 py-3">
          <div className="min-w-0">
            <div
              className="truncate text-[13px] font-semibold text-text"
              title={selectedProcess ? selectedProcess.command : undefined}
            >
              {selectedProcess ? selectedProcess.command : "Terminal output"}
            </div>
            <div className="mt-1 truncate text-[11px] text-text-3">
              {selectedProcess
                ? `${selectedProcess.id} · ${selectedProcess.statusLabel} · ${selectedProcess.controlMode}`
                : "Select a process to inspect logs and send input."}
            </div>
          </div>
          {selectedProcess ? (
            <button
              type="button"
              className="inline-flex h-8 shrink-0 items-center gap-2 rounded-lg border border-line bg-transparent px-3 text-[12px] text-text-2 hover:bg-panel hover:text-text disabled:opacity-45"
              disabled={actionPending === `kill:${selectedProcess.id}`}
              onClick={() => setStopCandidate(selectedProcess)}
            >
              <Icon name="close" size={13} />
              {actionPending === `kill:${selectedProcess.id}` ? "Stopping" : "Stop"}
            </button>
          ) : null}
        </div>
        {error ? (
          <div
            className="flex items-center justify-between gap-3 border-b border-danger/30 bg-danger/10 px-5 py-2 text-[12px] text-danger"
            role="alert"
          >
            <span className="min-w-0 truncate" title={`Terminal service: ${error}`}>
              Terminal service: {error}
            </span>
            <button
              type="button"
              className="shrink-0 rounded border border-danger/35 px-2 py-1 text-[11px] font-semibold hover:bg-danger/10"
              onClick={onRefresh}
            >
              Retry
            </button>
          </div>
        ) : null}
        <div className="min-h-0 flex-1 bg-bg">
          {selectedProcess ? (
            <pre
              ref={outputRef}
              className="h-full overflow-auto px-5 py-4 font-mono text-[12px] leading-6 text-text-2 whitespace-pre-wrap"
            >
              {logs?.output || (loading ? "Loading terminal output..." : "(no output yet)")}
            </pre>
          ) : (
            <div className="grid h-full place-items-center p-6 text-center">
              <div data-testid="terminal-pane-empty"><EmptyState icon={<Icon name="terminal" size={18} />} title="No terminal session yet" action={<button
                    type="button"
                    data-testid="terminal-empty-open"
                    className="inline-flex h-8 items-center gap-2 rounded-lg border border-accent/40 bg-accent/10 px-3 text-[12px] font-semibold text-accent hover:bg-accent/15 disabled:cursor-not-allowed disabled:opacity-45"
                    disabled={!selectedSession || actionPending === "create"}
                    onClick={onCreateTerminal}
                  >
                    <Icon name="plus" size={13} />
                    {actionPending === "create" ? "Opening" : "Open terminal"}
                  </button>}>{"This pane shows live output for terminals you open here and background shells the agent starts in this workspace. Nothing has run yet — open one to see its output and send input."}</EmptyState></div>
            </div>
          )}
        </div>
        <div className="border-t border-line bg-panel px-4 py-3">
          <div className="flex items-end gap-3">
            <label className="flex min-w-0 flex-1 flex-col gap-1 text-[11px] text-text-3">
              Send input
              <input
                className="h-10 min-w-0 rounded-lg border border-line bg-bg px-3 font-mono text-[12px] text-text outline-none placeholder:text-text-3"
                placeholder={
                  selectedProcess
                    ? "Type a shell command and press Enter"
                    : "Select a process first"
                }
                value={input}
                disabled={
                  !selectedProcess || actionPending === `stdin:${selectedProcess?.id ?? ""}`
                }
                onChange={(e) => onInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    onSendInput();
                  }
                }}
              />
            </label>
            <button
              type="button"
              className="inline-flex h-10 shrink-0 items-center gap-2 rounded-lg border border-accent/40 bg-accent/10 px-4 text-[12px] font-semibold text-accent hover:bg-accent/15 disabled:cursor-not-allowed disabled:opacity-45"
              disabled={
                !selectedProcess ||
                !input.length ||
                actionPending === `stdin:${selectedProcess?.id ?? ""}`
              }
              onClick={onSendInput}
            >
              <Icon name="send" size={13} />
              {actionPending === `stdin:${selectedProcess?.id ?? ""}` ? "Sending" : "Send"}
            </button>
          </div>
        </div>
      </div>
      {stopCandidate ? (
        <ActionDialog
          title="Stop terminal process?"
          description={`Stop “${stopCandidate.command}”. Running work in this process may be interrupted.`}
          role="alertdialog"
          dismissible={actionPending !== `kill:${stopCandidate.id}`}
          onClose={() => setStopCandidate(null)}
          actions={[
            {
              id: "cancel",
              label: "Cancel",
              autoFocus: true,
              disabled: actionPending === `kill:${stopCandidate.id}`,
              onClick: () => setStopCandidate(null),
            },
            {
              id: "confirm",
              label: "Stop process",
              variant: "danger",
              disabled: actionPending === `kill:${stopCandidate.id}`,
              onClick: () => {
                onStopProcess(stopCandidate.id);
                setStopCandidate(null);
              },
            },
          ]}
        />
      ) : null}
    </section>
  );
}
