import { EmptyState } from "@rightkit/react-shell/react";
import type { CSSProperties, ReactNode } from "react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { ActionDialog } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import usePersistentTerminal from "../hooks/usePersistentTerminal.js";
import type { DaemonConfig, ExecutionProcessRow } from "@rightkit/agent-ui-core/lib/types";
import {
  createHistoryProvider,
  suggest,
  type ProjectContext,
} from "@rightkit/agent-ui-core/lib/terminal/suggest/suggestionEngine";
import {
  clearGhostOverlay,
  renderGhostText,
  type GhostTextEntry,
} from "@rightkit/agent-ui-core/lib/terminal/suggest/ghostText";

interface Rect {
  hidden: boolean;
  top: number;
  left: number;
  width: number;
  height: number;
}

const DEFAULT_SLOT_CLASS = "relative flex min-h-0 min-w-0 flex-1 flex-col";
let activeSlot: HTMLElement | null = null;
const slotListeners = new Set<() => void>();

function publishSlot(slot: HTMLElement | null): void {
  activeSlot = slot;
  for (const listener of slotListeners) listener();
}

function subscribeSlot(listener: () => void): () => void {
  slotListeners.add(listener);
  return () => slotListeners.delete(listener);
}

/** Layout anchor. The persistent terminal never moves the daemon-owned surface. */
export function TerminalSlot({ className = DEFAULT_SLOT_CLASS }: { className?: string }) {
  const ref = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    publishSlot(element);
    return () => {
      if (activeSlot === element) publishSlot(null);
    };
  }, []);
  return <div className={className} data-terminal-slot="" ref={ref} />;
}

function useTerminalSlot(): HTMLElement | null {
  const [slot, setSlot] = useState(activeSlot);
  useEffect(() => {
    const unsubscribe = subscribeSlot(() => setSlot(activeSlot));
    setSlot(activeSlot);
    return unsubscribe;
  }, []);
  return slot;
}

function isHiddenInLayout(element: HTMLElement): boolean {
  for (let node: HTMLElement | null = element; node; node = node.parentElement) {
    if (
      node.hidden ||
      node.getAttribute("aria-hidden") === "true" ||
      node.hasAttribute("data-terminal-hidden") ||
      node.classList.contains("hidden")
    ) {
      return true;
    }
  }
  return false;
}

function sameRect(a: Rect | null, b: Rect): boolean {
  return Boolean(
    a &&
      a.hidden === b.hidden &&
      a.top === b.top &&
      a.left === b.left &&
      a.width === b.width &&
      a.height === b.height,
  );
}

function useSlotRect(slot: HTMLElement | null): Rect | null {
  const [rect, setRect] = useState<Rect | null>(null);

  useLayoutEffect(() => {
    if (!slot) {
      setRect(null);
      return;
    }
    const currentSlot = slot;

    let previous: Rect | null = null;
    let frame = 0;
    let stopped = false;
    let paused = document.visibilityState === "hidden";

    function measure(): void {
      if (stopped || paused) return;
      const measured = currentSlot.getBoundingClientRect();
      const top = Math.floor(measured.top);
      const left = Math.floor(measured.left);
      const next: Rect = {
        hidden: isHiddenInLayout(currentSlot),
        top,
        left,
        width: Math.max(0, Math.ceil(measured.right) - left),
        height: Math.max(0, Math.ceil(measured.bottom) - top),
      };
      if (!sameRect(previous, next)) {
        previous = next;
        setRect(next);
        if (next.width > 0 && next.height > 0) schedule("settle");
      }
    }

    function schedule(_reason: string): void {
      if (stopped || paused || frame !== 0) return;
      frame = window.requestAnimationFrame(() => {
        frame = 0;
        measure();
      });
    }

    const onVisibility = () => {
      paused = document.visibilityState === "hidden";
      if (paused) {
        if (frame) window.cancelAnimationFrame(frame);
        frame = 0;
      } else {
        schedule("visibility");
      }
    };
    const onFocus = () => {
      paused = false;
      schedule("focus");
    };
    const onBlur = () => {
      paused = true;
      if (frame) window.cancelAnimationFrame(frame);
      frame = 0;
    };
    const onScroll = () => schedule("scroll");
    const resizeObserver = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => schedule("resize"));
    const mutationObserver = typeof MutationObserver === "undefined" ? null : new MutationObserver(() => schedule("layout"));

    measure();
    resizeObserver?.observe(currentSlot);
    for (let node: HTMLElement | null = currentSlot; node; node = node.parentElement) {
      mutationObserver?.observe(node, {
        attributes: true,
        attributeFilter: ["class", "style", "hidden", "aria-hidden", "data-terminal-hidden"],
        childList: true,
        subtree: false,
      });
      node.addEventListener("scroll", onScroll);
    }
    window.addEventListener("resize", onScroll);
    window.addEventListener("scroll", onScroll);
    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("focus", onFocus);
    window.addEventListener("blur", onBlur);

    return () => {
      stopped = true;
      if (frame) window.cancelAnimationFrame(frame);
      resizeObserver?.disconnect();
      mutationObserver?.disconnect();
      for (let node: HTMLElement | null = currentSlot; node; node = node.parentElement) {
        node.removeEventListener("scroll", onScroll);
      }
      window.removeEventListener("resize", onScroll);
      window.removeEventListener("scroll", onScroll);
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("blur", onBlur);
    };
  }, [slot]);

  return rect;
}

export interface PersistentTerminalProps {
  config: DaemonConfig | null;
  sessionId: string | null;
  workspaceRoot: string;
  projectContext?: ProjectContext | null;
  history?: string[];
  onAddSelectionToChat?: (text: string, label?: string) => void;
  className?: string;
  emptyState?: ReactNode;
}

/**
 * Keep-alive terminal projection. The overlay follows its slot while hidden,
 * preserving the mounted renderer and the daemon-owned process/scrollback.
 */
export function PersistentTerminal({
  config,
  sessionId,
  workspaceRoot,
  projectContext = null,
  history = [],
  onAddSelectionToChat,
  className = "",
  emptyState,
}: PersistentTerminalProps) {
  const slot = useTerminalSlot();
  const rect = useSlotRect(slot);
  const [mounted, setMounted] = useState(false);
  const [stopCandidate, setStopCandidate] = useState<ExecutionProcessRow | null>(null);
  const ghostHostRef = useRef<HTMLDivElement | null>(null);
  const ghostEntryRef = useRef<GhostTextEntry | null>(null);
  const terminal = usePersistentTerminal({ config, sessionId, workspaceRoot });
  const selectedProcess =
    terminal.processes.find((process) => process.id === terminal.selectedProcessId) ??
    terminal.processes[0] ??
    null;

  const historyProvider = useMemo(
    () => createHistoryProvider([...history, ...terminal.history]),
    [history, terminal.history],
  );
  const suggestions = useMemo(
    () => suggest(terminal.input, projectContext, historyProvider),
    [historyProvider, projectContext, terminal.input],
  );
  const ghostSuggestion =
    suggestions.find((suggestion) => suggestion.text.startsWith(terminal.input) && suggestion.text !== terminal.input)
      ?.text.slice(terminal.input.length) ?? "";

  useEffect(() => {
    if (rect && rect.width > 0 && rect.height > 0 && config && sessionId) setMounted(true);
  }, [config, rect, sessionId]);

  useEffect(() => {
    const host = ghostHostRef.current;
    if (!host) return;
    if (ghostEntryRef.current) clearGhostOverlay(ghostEntryRef.current);
    const entry: GhostTextEntry = {
      container: host,
      ghostText: null,
      ghostOverlay: null,
      attached: true,
      opened: true,
      getCursorPosition: () => ({ x: terminal.input.length * 7.2, y: 8 }),
    };
    ghostEntryRef.current = entry;
    if (ghostSuggestion) renderGhostText(entry, ghostSuggestion);
    return () => {
      clearGhostOverlay(entry);
      if (ghostEntryRef.current === entry) ghostEntryRef.current = null;
    };
  }, [ghostSuggestion, terminal.input.length]);

  const visible = Boolean(rect && !rect.hidden && rect.width > 0 && rect.height > 0);
  const style: CSSProperties = {
    position: "fixed",
    top: rect?.top ?? 0,
    left: rect?.left ?? 0,
    width: rect?.width ?? 0,
    height: rect?.height ?? 0,
    display: "flex",
    flexDirection: "column",
    visibility: visible ? "visible" : "hidden",
    opacity: visible ? 1 : 0,
    pointerEvents: visible ? "auto" : "none",
    zIndex: 4,
    backgroundColor: "var(--color-bg, var(--bg))",
    contain: "layout size paint",
  };

  const send = () => void terminal.sendInput();
  const addSelection = () => {
    const selected = window.getSelection()?.toString().trim();
    if (selected && onAddSelectionToChat) onAddSelectionToChat(selected, "Terminal output");
  };

  return (
    <div
      aria-hidden={!visible}
      className={className}
      data-persistent-terminal=""
      style={style}
    >
      {mounted ? (
        <section data-testid="persistent-terminal-surface" className="flex min-h-0 min-w-0 flex-1 flex-col" aria-label="Persistent terminal">
          <header className="flex items-center justify-between gap-3 border-b border-line px-4 py-3">
            <div className="min-w-0">
              <div className="flex items-center gap-2 text-[13px] font-semibold text-text">
                <Icon name="terminal" size={14} />
                <span>Terminal</span>
              </div>
              <div className="mt-1 truncate text-[11px] text-text-3" title={workspaceRoot}>
                {workspaceRoot || "Choose a workspace"}
              </div>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              {onAddSelectionToChat ? (
                <button type="button" className="rounded-lg border border-line px-2 py-1 text-[11px] text-text-2" onClick={addSelection}>
                  Add selection
                </button>
              ) : null}
              <button type="button" className="inline-flex h-8 items-center gap-1 rounded-lg border border-accent/40 bg-accent/10 px-2 text-[11px] font-semibold text-accent disabled:opacity-45" disabled={!sessionId || terminal.actionPending === "create"} onClick={() => void terminal.createTerminal()}>
                <Icon name="plus" size={12} />
                {terminal.actionPending === "create" ? "Opening" : "Open terminal"}
              </button>
              <button type="button" aria-label="Refresh terminal processes" className="grid h-8 w-8 place-items-center rounded-lg border border-line text-text-2" onClick={() => void terminal.refresh()}>
                <Icon name="refresh" size={13} />
              </button>
            </div>
          </header>
          {terminal.error ? <div role="alert" className="border-b border-danger/30 bg-danger/10 px-4 py-2 text-[12px] text-danger">Terminal service: {terminal.error}</div> : null}
          {terminal.processes.length > 0 ? (
            <nav className="flex gap-2 overflow-x-auto border-b border-line px-4 py-2" aria-label="Terminal processes">
              {terminal.processes.map((process) => (
                <button key={process.id} type="button" className={`max-w-56 shrink-0 truncate rounded border px-2 py-1 text-left text-[11px] ${process.id === selectedProcess?.id ? "border-accent/50 bg-accent/10 text-text" : "border-line text-text-2"}`} title={process.command} onClick={() => terminal.selectProcess(process.id)}>
                  {process.command} · {process.statusLabel}
                </button>
              ))}
            </nav>
          ) : null}
          <div className="min-h-0 flex-1 overflow-auto bg-bg px-4 py-3">
            {selectedProcess ? (
              <pre className="m-0 whitespace-pre-wrap font-mono text-[12px] leading-6 text-text-2">
                {terminal.logs?.output || (terminal.loading ? "Loading terminal output..." : "(no output yet)")}
              </pre>
            ) : emptyState ?? (
              <EmptyState icon={<Icon name="terminal" size={18} />} title="No terminal session yet">{"Open a terminal to keep its process and scrollback available while this surface is hidden."}</EmptyState>
            )}
          </div>
          <footer className="border-t border-line bg-panel px-4 py-3">
            <nav className="mb-2 flex gap-2 overflow-x-auto" aria-label="Terminal suggestions">
              {suggestions.slice(0, 5).map((suggestion) => (
                <button key={suggestion.text} type="button" className="shrink-0 rounded border border-line bg-bg px-2 py-1 text-left text-[11px] text-text-2" onClick={() => terminal.setInput(suggestion.text)}>
                  <span className="font-mono">{suggestion.text}</span>
                  {suggestion.badge ? <span className="ml-2 text-text-3">{suggestion.badge}</span> : null}
                </button>
              ))}
            </nav>
            <div ref={ghostHostRef} data-terminal-ghost-host="" className="relative flex items-end gap-2">
              <label className="min-w-0 flex-1 text-[11px] text-text-3">
                Send input
                <input
                  className="mt-1 h-10 w-full rounded-lg border border-line bg-bg px-3 font-mono text-[12px] text-text outline-none"
                  value={terminal.input}
                  placeholder={selectedProcess ? "Type a shell command and press Enter" : "Open a process first"}
                  disabled={!selectedProcess || terminal.actionPending === `stdin:${selectedProcess.id}`}
                  onChange={(event) => terminal.setInput(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Tab" && ghostSuggestion) {
                      event.preventDefault();
                      terminal.setInput(terminal.input + ghostSuggestion);
                    } else if (event.key === "Enter") {
                      event.preventDefault();
                      send();
                    }
                  }}
                />
              </label>
              <button type="button" aria-label="Send terminal input" className="h-10 shrink-0 rounded-lg border border-accent/40 bg-accent/10 px-3 text-[12px] font-semibold text-accent disabled:opacity-45" disabled={!selectedProcess || !terminal.input.length} onClick={send}>
                <Icon name="send" size={13} />
              </button>
            </div>
          </footer>
          {selectedProcess ? (
            <button type="button" className="absolute right-4 top-16 rounded border border-line bg-panel px-2 py-1 text-[11px] text-text-2" disabled={Boolean(terminal.actionPending)} onClick={() => setStopCandidate(selectedProcess)}>
              Stop
            </button>
          ) : null}
        </section>
      ) : null}
      {stopCandidate ? (
        <ActionDialog
          title="Stop terminal process?"
          description={`Stop “${stopCandidate.command}”. Running work may be interrupted.`}
          role="alertdialog"
          dismissible={terminal.actionPending !== `kill:${stopCandidate.id}`}
          onClose={() => setStopCandidate(null)}
          actions={[
            {
              id: "cancel",
              label: "Cancel",
              autoFocus: true,
              disabled: terminal.actionPending === `kill:${stopCandidate.id}`,
              onClick: () => setStopCandidate(null),
            },
            {
              id: "confirm",
              label: "Stop process",
              variant: "danger",
              disabled: terminal.actionPending === `kill:${stopCandidate.id}`,
              onClick: () => {
                void terminal.stopProcess(stopCandidate.id);
                setStopCandidate(null);
              },
            },
          ]}
        />
      ) : null}
    </div>
  );
}

export default PersistentTerminal;
