import { useEffect, useMemo } from "react";
import type { Command as ShellCommand } from "@rightkit/react-shell";
import { CommandPalette as ShellCommandPalette } from "@rightkit/react-shell/react";

import Icon from "./Icon.js";
import type { IconName } from "./Icon.js";
import type { CommandInfo } from "../lib/daemon.js";
import type { RightPanelTab } from "../state/rightPanelStore.js";
import {
  SCOPE_LABELS,
  usePaletteStore,
  type PaletteGroup,
  type PaletteItem,
  type PaletteScope,
} from "../state/paletteStore.js";

type Pane = "session" | "tasks" | "runtime" | "terminal" | "memory";
export type UtilityTarget =
  | RightPanelTab
  | "terminal"
  | "maximize-utilities"
  | "maximize-terminal"
  | "reset-layout";
type SessionItem = { id: string; title: string; model?: string };

const GROUP_ORDER: PaletteGroup[] = [
  "Recents",
  "Skills",
  "Plugins",
  "MCP",
  "Tools",
  "Commands",
  "Settings",
  "Sessions",
  "Files",
  "Branches",
  "Todos",
  "Views",
];

function buildPaneItems(
  onPane: (pane: Pane) => void,
  onOpenSettings: () => void,
  onOpenUtility: (tab: UtilityTarget) => void,
): PaletteItem[] {
  // type: "View" is the legacy chip label kept for parity with the previous
  // CommandPalette contract (uiRedesignContract.test.tsx).
  return [
    {
      key: "pane:session",
      label: "Go to Sessions",
      icon: "activity",
      scope: "global",
      group: "Views",
      kind: "view",
      type: "View",
      run: () => onPane("session"),
    },
    {
      key: "pane:settings",
      label: "Open Settings",
      icon: "settings",
      scope: "global",
      group: "Settings",
      kind: "setting",
      type: "View",
      run: () => onOpenSettings(),
    },
    {
      key: "pane:terminal",
      label: "Open Terminal Dock",
      icon: "terminal",
      scope: "global",
      group: "Views",
      kind: "view",
      type: "View",
      run: () => onOpenUtility("terminal"),
    },
    {
      key: "pane:runtime",
      label: "Go to Runtime",
      icon: "activity",
      scope: "global",
      group: "Views",
      kind: "view",
      type: "View",
      run: () => onPane("runtime"),
    },
    {
      key: "pane:memory",
      label: "Go to Memory",
      icon: "search",
      scope: "global",
      group: "Views",
      kind: "view",
      type: "View",
      run: () => onPane("memory"),
    },
    {
      key: "utility:files",
      label: "Open Files",
      icon: "folder",
      scope: "global",
      group: "Files",
      kind: "view",
      type: "View",
      run: () => onOpenUtility("files"),
    },
    {
      key: "utility:artifacts",
      label: "Open Artifacts",
      icon: "file",
      scope: "global",
      group: "Views",
      kind: "view",
      type: "View",
      run: () => onOpenUtility("artifacts"),
    },
    {
      key: "utility:review",
      label: "Open Review",
      icon: "diff",
      scope: "global",
      group: "Views",
      kind: "view",
      type: "View",
      run: () => onOpenUtility("review"),
    },
    {
      key: "utility:browser",
      label: "Open Browser",
      icon: "panel-right",
      scope: "global",
      group: "Views",
      kind: "view",
      type: "View",
      run: () => onOpenUtility("browser"),
    },
    {
      key: "utility:delivery",
      label: "Open Delivery",
      icon: "git",
      scope: "global",
      group: "Branches",
      kind: "view",
      type: "View",
      run: () => onOpenUtility("delivery"),
    },
    {
      key: "utility:environments",
      label: "Open Environments",
      icon: "server",
      scope: "global",
      group: "Views",
      kind: "view",
      type: "View",
      run: () => onOpenUtility("environments"),
    },
    {
      key: "utility:attention",
      label: "Open Attention",
      icon: "alert",
      scope: "global",
      group: "Views",
      kind: "view",
      type: "View",
      run: () => onOpenUtility("attention"),
    },
    {
      key: "utility:agents",
      label: "Open Agents",
      icon: "activity",
      scope: "global",
      group: "Views",
      kind: "view",
      type: "View",
      run: () => onOpenUtility("background-tasks"),
    },
    {
      key: "layout:maximize-utilities",
      label: "Maximize Workspace Utilities",
      icon: "maximize",
      scope: "global",
      group: "Views",
      kind: "view",
      type: "View",
      run: () => onOpenUtility("maximize-utilities"),
    },
    {
      key: "layout:maximize-terminal",
      label: "Maximize Terminal Dock",
      icon: "maximize",
      scope: "global",
      group: "Views",
      kind: "view",
      type: "View",
      run: () => onOpenUtility("maximize-terminal"),
    },
    {
      key: "layout:reset",
      label: "Reset Workspace Layout",
      icon: "panel",
      scope: "global",
      group: "Views",
      kind: "view",
      type: "View",
      run: () => onOpenUtility("reset-layout"),
    },
  ];
}

const PALETTE_LABELS = {
  placeholder: "Run a command or jump…",
  title: "Command palette",
  empty: "No matches",
  results: "Command palette results",
  inputAriaLabel: "Search commands, sessions, and views",
};

// The store already filters and ranks `items`, so the palette must not re-rank them.
const keepStoreOrder = (commands: readonly ShellCommand[]) => commands;

/// Quick-open palette (Ctrl/Cmd+P / Ctrl/Cmd+K): jump to a view, fire a slash
/// command, or insert a workspace mention. Single fuzzy panel that handles
/// both global ⌘K and composer `/` + `@` scoping (parity spec §4.4). The panel
/// itself is RightKit's `CommandPalette`; `usePaletteStore` stays the source of
/// scope, query, ranking and highlight, and this component maps it onto the
/// palette's controlled props.
export default function CommandPalette({
  open,
  onClose,
  commands,
  sessions,
  onPane,
  onRunCommand,
  onSelectSession,
  onOpenSettings,
  onOpenUtility = () => {},
}: {
  open: boolean;
  onClose: () => void;
  commands: CommandInfo[];
  sessions: SessionItem[];
  onPane: (pane: Pane) => void;
  onRunCommand: (input: string) => void;
  onSelectSession: (id: string) => void;
  onOpenSettings: () => void;
  onOpenUtility?: (tab: UtilityTarget) => void;
}) {
  // Subscribe to each primitive/action separately. Reading getState() here
  // leaves the rendered list stale after typing or arrow navigation because
  // Zustand cannot notify a component that never subscribed to the store.
  const scope = usePaletteStore((state) => state.scope);
  const query = usePaletteStore((state) => state.query);
  const items = usePaletteStore((state) => state.items);
  const active = usePaletteStore((state) => state.active);
  const setQuery = usePaletteStore((state) => state.setQuery);
  const setActive = usePaletteStore((state) => state.setActive);
  const setSources = usePaletteStore((state) => state.setSources);

  // React's server snapshot for Zustand is the store's initial state rather
  // than its current state. Use the live store for static rendering too so
  // SSR/pre-rendered palette markup retains the active scope and grouped rows.
  const currentStore = usePaletteStore.getState();
  const renderedScope = currentStore.scope ?? scope;
  const renderedQuery = currentStore.query || query;
  const renderedItems = currentStore.items.length > 0 ? currentStore.items : items;
  const renderedActive = currentStore.active ?? active;

  const sessionItems: PaletteItem[] = useMemo(
    () =>
      // type: "Session" is the legacy chip label kept for parity with the
      // previous CommandPalette contract (uiRedesignContract.test.tsx).
      sessions.map((s) => ({
        key: `session:${s.id}`,
        label: s.title || "Untitled session",
        sub: s.model,
        icon: "branch" as IconName,
        scope: "mention" as PaletteScope,
        group: "Sessions" as PaletteGroup,
        kind: "session" as const,
        type: "Session" as const,
        run: () => {
          onSelectSession(s.id);
          onPane("session");
        },
      })),
    [sessions, onSelectSession, onPane],
  );

  const paneItems = useMemo(
    () => buildPaneItems(onPane, onOpenSettings, onOpenUtility),
    [onPane, onOpenSettings, onOpenUtility],
  );

  useEffect(() => {
    setSources({
      commands,
      skills: [],
      panes: paneItems,
      sessions: sessionItems,
      onPane: (pane) => onPane(pane as Pane),
      onRunCommand,
      onSelectSession,
    });
  }, [commands, paneItems, sessionItems, onPane, onRunCommand, onSelectSession, setSources]);

  useEffect(() => {
    // The opener already selected the scope (global, slash, or mention).
    // Reset transient navigation without collapsing that scope back to global.
    if (open) usePaletteStore.setState({ query: "", active: 0 });
  }, [open]);

  // Rows in the order RightKit's palette shows them (groups in GROUP_ORDER, items in
  // store order within a group), so the highlight index means the same row in both.
  const visibleItems = useMemo(() => {
    const byGroup = new Map<PaletteGroup, PaletteItem[]>();
    for (const item of renderedItems) {
      const rows = byGroup.get(item.group);
      if (rows) rows.push(item);
      else byGroup.set(item.group, [item]);
    }
    return [...byGroup.entries()]
      .sort((a, b) => GROUP_ORDER.indexOf(a[0]) - GROUP_ORDER.indexOf(b[0]))
      .flatMap(([, rows]) => rows);
  }, [renderedItems]);

  const shellCommands = useMemo<ShellCommand[]>(
    () =>
      visibleItems.map((item) => ({
        id: item.key,
        label: item.label,
        group: item.group,
        kind: item.type ?? item.kind,
        sublabel: item.sub,
        icon: <Icon name={item.icon} size={14} />,
        run: item.run,
      })),
    [visibleItems],
  );

  if (!open) return null;

  const activeItem = renderedItems[renderedActive];
  const activeVisible = Math.max(0, visibleItems.indexOf(activeItem));

  return (
    <ShellCommandPalette
      commands={shellCommands}
      onClose={onClose}
      rank={keepStoreOrder}
      limit={Number.MAX_SAFE_INTEGER}
      labels={PALETTE_LABELS}
      paletteWidth={560}
      scopeLabel={SCOPE_LABELS[renderedScope]}
      groupOrder={GROUP_ORDER}
      query={renderedQuery}
      onQueryChange={setQuery}
      activeIndex={activeVisible}
      onActiveChange={(index) => {
        const next = visibleItems[index];
        if (next) setActive(Math.max(0, renderedItems.indexOf(next)));
      }}
      onRun={(command) => visibleItems.find((item) => item.key === command.id)?.run()}
    />
  );
}
