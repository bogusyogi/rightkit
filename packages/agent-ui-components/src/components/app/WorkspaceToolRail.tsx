import { useEffect, useRef, useState, type KeyboardEvent } from "react";

import Icon, { type IconName } from "@rightkit/agent-ui-core/components/Icon";
import type { WorkspaceRightPanelTab } from "@rightkit/agent-ui-core/state/layoutStore";

export type WorkspaceToolRailPanel = WorkspaceRightPanelTab;
export type WorkspaceToolRailItem = WorkspaceToolRailPanel | "terminal";

type RailItem = {
  id: WorkspaceToolRailItem;
  label: string;
  icon: IconName;
};

const ITEMS: ReadonlyArray<RailItem> = [
  { id: "files", label: "Files", icon: "folder" },
  { id: "artifacts", label: "Artifacts", icon: "package" },
  { id: "review", label: "Review / Changes", icon: "diff" },
  { id: "execution", label: "Execution graph", icon: "tasks" },
  { id: "bots", label: "Bots", icon: "user" },
  { id: "browser", label: "Browser", icon: "browser" },
  { id: "delivery", label: "Delivery", icon: "upload" },
  { id: "environments", label: "Environments", icon: "server" },
  { id: "attention", label: "Attention", icon: "alert" },
  { id: "terminal", label: "Terminal", icon: "terminal" },
];

export interface WorkspaceToolRailProps {
  /** Current panel owned by the existing layout/right-panel stores. */
  activePanel: WorkspaceToolRailPanel | null;
  /** Existing terminal dock state; the rail never models Terminal as a panel. */
  terminalOpen: boolean;
  /** Toggle or switch an existing right-panel surface. */
  onTogglePanel: (tab: WorkspaceToolRailPanel) => void;
  /** Close any existing workspace panel before restoring trigger focus. */
  onClosePanel: () => void;
  /** Toggle the existing terminal dock. */
  onToggleTerminal: () => void;
}

/** Control-only rail. AppBody owns its placement plus drawer geometry. */
export function WorkspaceToolRail({
  activePanel,
  terminalOpen,
  onTogglePanel,
  onClosePanel,
  onToggleTerminal,
}: WorkspaceToolRailProps) {
  const buttons = useRef<Array<HTMLButtonElement | null>>([]);
  const [rovingIndex, setRovingIndex] = useState(0);

  useEffect(() => {
    const activeId = activePanel ?? (terminalOpen ? "terminal" : null);
    if (!activeId) return;
    const activeIndex = ITEMS.findIndex((item) => item.id === activeId);
    const closeOnEscape = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      event.preventDefault();
      if (activePanel) onClosePanel();
      else onToggleTerminal();
      buttons.current[activeIndex]?.focus();
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [activePanel, terminalOpen, onClosePanel, onToggleTerminal]);

  const activate = (item: WorkspaceToolRailItem) => {
    if (item === "terminal") onToggleTerminal();
    else onTogglePanel(item);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number, isActive: boolean) => {
    if (event.key === "Escape") {
      event.preventDefault();
      if (activePanel) onClosePanel();
      else if (isActive && ITEMS[index].id === "terminal") onToggleTerminal();
      buttons.current[index]?.focus();
      return;
    }
    const direction =
      event.key === "ArrowDown" || event.key === "ArrowRight"
        ? 1
        : event.key === "ArrowUp" || event.key === "ArrowLeft"
          ? -1
          : 0;
    const next =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? ITEMS.length - 1
          : direction
            ? (index + direction + ITEMS.length) % ITEMS.length
            : null;
    if (next === null) return;
    event.preventDefault();
    setRovingIndex(next);
    buttons.current[next]?.focus();
  };

  return (
    <nav
      aria-label="Workspace tools"
      className={`flex w-10 flex-col gap-0 rounded-xl border border-line bg-panel-2/90 p-1 shadow-lg shadow-black/20 backdrop-blur transition-opacity hover:opacity-100 focus-within:opacity-100 ${activePanel || terminalOpen ? "opacity-100" : "opacity-70"}`}
      data-testid="workspace-tool-rail"
    >
      {ITEMS.map((item, index) => {
        const pressed = item.id === "terminal" ? terminalOpen : activePanel === item.id;
        return (
          <button
            aria-describedby={`workspace-tool-tooltip-${item.id}`}
            aria-label={item.label}
            aria-pressed={pressed}
            className={`group relative grid h-8 w-8 place-items-center rounded-md border transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus ${pressed ? "border-accent/60 bg-accent/15 text-text" : "border-transparent text-text-3 hover:bg-panel hover:text-text"}`}
            key={item.id}
            onClick={() => activate(item.id)}
            onFocus={() => setRovingIndex(index)}
            onKeyDown={(event) => onKeyDown(event, index, pressed)}
            ref={(node) => {
              buttons.current[index] = node;
            }}
            title={item.label}
            tabIndex={index === rovingIndex ? 0 : -1}
            type="button"
          >
            <Icon name={item.icon} size={16} />
            <span
              className="pointer-events-none absolute right-full mr-2 hidden whitespace-nowrap rounded bg-panel-2 px-2 py-1 text-xs text-text shadow group-focus-visible:block group-hover:block"
              id={`workspace-tool-tooltip-${item.id}`}
              role="tooltip"
            >
              {item.label}
            </span>
          </button>
        );
      })}
    </nav>
  );
}
