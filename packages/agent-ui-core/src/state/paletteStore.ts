// the host app palette model (scopes, sources, recents, ranking, highlight). The panel that renders it
// is RightKit's `CommandPalette`, driven through its controlled props in components/CommandPalette.tsx.
import { create } from "zustand";

import type { IconName } from "../components/Icon";
import type { CommandInfo } from "../lib/daemon/sessions-permissions";
import type { SkillItem } from "../lib/types";
import type { WorkspaceMention } from "../lib/daemon/workspace-health";
import { rankCommands } from "@rightkit/platform-ui/headless";

export type PaletteScope = "global" | "slash" | "mention";

export type PaletteGroup =
  | "Skills"
  | "Plugins"
  | "MCP"
  | "Tools"
  | "Commands"
  | "Settings"
  | "Recents"
  | "Sessions"
  | "Files"
  | "Branches"
  | "Todos"
  | "Views";

export type PaletteItemKind =
  | "skill"
  | "plugin"
  | "mcp"
  | "tool"
  | "command"
  | "setting"
  | "recent"
  | "session"
  | "view"
  | "file"
  | "branch"
  | "todo";

export type PaletteItem = {
  key: string;
  scope: PaletteScope;
  group: PaletteGroup;
  kind: PaletteItemKind;
  /** Legacy single-word chip label (Command / View / Session) used by the
   * existing palette chip row. New callers should prefer `kind`. */
  type?: "Command" | "View" | "Session";
  label: string;
  sub?: string;
  icon: IconName;
  aliases?: string[];
  run: () => void;
};

export const SCOPE_LABELS: Record<PaletteScope, string> = {
  global: "Skills · Plugins · MCP · Tools · Settings · Recents",
  slash: "Skills · Plugins · MCP · Tools · Commands",
  mention: "Files · Branches · Todos",
};

export type PaletteSource = {
  commands: CommandInfo[];
  skills: SkillItem[];
  plugins?: PaletteItem[];
  mcpTools?: PaletteItem[];
  panes?: PaletteItem[];
  sessions?: PaletteItem[];
  mentions?: WorkspaceMention[];
  recents?: PaletteItem[];
  onPane?: (pane: string) => void;
  onRunCommand?: (input: string) => void;
  onSelectSession?: (id: string) => void;
  onInsertMention?: (mention: WorkspaceMention) => void;
};

export type PaletteStoreState = {
  scope: PaletteScope;
  query: string;
  items: PaletteItem[];
  active: number;
  isOpen: boolean;
  prevFocus: HTMLElement | null;
  sources: PaletteSource;
  open: (scope?: PaletteScope) => void;
  close: () => void;
  setScope: (scope: PaletteScope) => void;
  setQuery: (query: string) => void;
  setActive: (index: number) => void;
  moveActive: (delta: number) => void;
  activate: (index: number) => void;
  setSources: (sources: PaletteSource) => void;
};

function buildItems(scope: PaletteScope, sources: PaletteSource): PaletteItem[] {
  const items: PaletteItem[] = [];
  if (scope === "global") {
    if (sources.recents?.length) {
      for (const r of sources.recents) items.push(r);
    }
    if (sources.skills.length) {
      for (const skill of sources.skills) {
        items.push({
          key: `skill:${skill.slug}`,
          scope: "global",
          group: "Skills",
          kind: "skill",
          label: skill.name || skill.slug,
          sub: skill.description || `@${skill.slug}`,
          icon: "spark",
          aliases: [skill.slug, `@${skill.slug}`],
          run: () => {},
        });
      }
    }
    if (sources.plugins?.length) items.push(...sources.plugins);
    if (sources.mcpTools?.length) items.push(...sources.mcpTools);
    if (sources.commands.length) {
      for (const command of sources.commands) {
        items.push({
          key: `command:${command.name}`,
          scope: "global",
          group: "Tools",
          kind: "command",
          type: "Command",
          label: command.usage || `/${command.name}`,
          sub: command.description,
          icon: "command",
          aliases: [command.name, `/${command.name}`],
          run: () => sources.onRunCommand?.(command.usage || `/${command.name}`),
        });
      }
    }
    if (sources.panes?.length) {
      for (const p of sources.panes)
        items.push({ ...p, type: p.kind === "view" ? "View" : undefined });
    }
  } else if (scope === "slash") {
    if (sources.skills.length) {
      for (const skill of sources.skills) {
        items.push({
          key: `skill:${skill.slug}`,
          scope: "slash",
          group: "Skills",
          kind: "skill",
          label: skill.name || skill.slug,
          sub: `@${skill.slug}`,
          icon: "spark",
          aliases: [skill.slug, `@${skill.slug}`],
          run: () => {},
        });
      }
    }
    if (sources.plugins?.length) items.push(...sources.plugins);
    if (sources.mcpTools?.length) items.push(...sources.mcpTools);
    if (sources.commands.length) {
      for (const command of sources.commands) {
        items.push({
          key: `command:${command.name}`,
          scope: "slash",
          group: "Commands",
          kind: "command",
          type: "Command",
          label: command.usage || `/${command.name}`,
          sub: command.description,
          icon: "command",
          aliases: [command.name, `/${command.name}`],
          run: () => sources.onRunCommand?.(command.usage || `/${command.name}`),
        });
      }
    }
    if (sources.panes?.length) {
      for (const p of sources.panes) {
        if (p.group === "Settings" || p.group === "Views") items.push({ ...p, scope: "slash" });
      }
    }
  } else {
    if (sources.sessions?.length) {
      for (const session of sources.sessions)
        items.push({ ...session, scope: "mention", type: "Session" });
    }
    if (sources.mentions?.length) {
      for (const mention of sources.mentions) {
        const insertText = mention.insertText || `@${mention.label}`;
        items.push({
          key: `mention:${mention.path}:${mention.line ?? 0}`,
          scope: "mention",
          group: "Files",
          kind: "file",
          label: mention.label,
          sub: mention.detail,
          icon: mention.kind === "file" ? "folder" : "spark",
          aliases: [mention.path, insertText],
          run: () => sources.onInsertMention?.(mention),
        });
      }
    }
  }
  return items;
}

function rankAndFilter(items: PaletteItem[], query: string): PaletteItem[] {
  const needle = query.trim();
  if (!needle) return items;
  // Ranking is RightKit's `rankCommands` (@rightkit/platform-ui) with sub-label and alias tiers,
  // ties sorted by label.
  return rankCommands(items, needle, undefined, {
    subLabel: true,
    aliasTiers: true,
    tieBreak: "label",
  });
}

function capturePrevFocus(): HTMLElement | null {
  if (typeof document === "undefined") return null;
  const active = document.activeElement;
  if (!active) return null;
  if (typeof HTMLElement !== "undefined" && active instanceof HTMLElement) return active;
  // Test environments (and stale node SSR) lack HTMLElement but the active
  // element still has focus() — accept any focus-capable node.
  if (typeof (active as { focus?: unknown }).focus === "function") {
    return active as HTMLElement;
  }
  return null;
}

const EMPTY_SOURCES: PaletteSource = { commands: [], skills: [] };

export const usePaletteStore = create<PaletteStoreState>((set, get) => {
  function recomputeItems() {
    const state = get();
    const built = buildItems(state.scope, state.sources);
    const filtered = rankAndFilter(built, state.query);
    set({
      items: filtered,
      active: Math.min(state.active, Math.max(0, filtered.length - 1)),
    });
  }

  return {
    scope: "global",
    query: "",
    items: [],
    active: 0,
    isOpen: false,
    prevFocus: null,
    sources: EMPTY_SOURCES,

    open(scope = "global") {
      set({
        isOpen: true,
        scope,
        query: "",
        active: 0,
        prevFocus: capturePrevFocus(),
      });
      recomputeItems();
    },

    close() {
      const prev = get().prevFocus;
      set({ isOpen: false, query: "", active: 0 });
      if (prev && typeof prev.focus === "function") {
        prev.focus();
      }
    },

    setScope(scope) {
      set({ scope, query: "", active: 0 });
      recomputeItems();
    },

    setQuery(query) {
      set({ query, active: 0 });
      recomputeItems();
    },

    setActive(active) {
      set({ active });
    },

    moveActive(delta) {
      const items = get().items;
      if (items.length === 0) return;
      const next = Math.max(0, Math.min(items.length - 1, get().active + delta));
      set({ active: next });
    },

    activate(index) {
      const item = get().items[index];
      if (!item) return;
      item.run();
      get().close();
    },

    setSources(sources) {
      set({ sources });
      recomputeItems();
    },
  };
});

export function resetPaletteStoreForTests(sources: PaletteSource = EMPTY_SOURCES) {
  usePaletteStore.setState({
    scope: "global",
    query: "",
    items: [],
    active: 0,
    isOpen: false,
    prevFocus: null,
    sources,
  });
}
