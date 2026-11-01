// D1 — WorkspaceRecentMenu.
//
// Renders the recents list maintained by `useWorkspaceStore`. When the
// store has no entries (the daemon hasn't started or `recents` is empty)
// the menu surfaces a "no recent files yet" honest state. Recents are
// stored both client-side (recently opened files in this session) and
// server-side (server keeps a per-workspace list); the menu shows both.
// Dialog semantics, Escape, backdrop and focus restore are RightKit's ModalShell; the
// row cursor is RightKit's useListNavigation.

import { useListNavigation } from "@rightkit/platform-ui/headless";
import { ModalShell } from "@rightkit/react-shell/react";
import { useMemo } from "react";
import { useWorkspaceStore } from "@rightkit/agent-ui-core/state/workspaceStore";
import type { WorkspaceRecentEntry } from "@rightkit/agent-ui-core/lib/types/workspace-files";

export interface WorkspaceRecentMenuProps {
  workspaceRoot: string;
  onClose: () => void;
  onPick: (path: string) => void;
}

export default function WorkspaceRecentMenu({
  workspaceRoot,
  onClose,
  onPick,
}: WorkspaceRecentMenuProps) {
  const recents = useWorkspaceStore((state) => state.recents);

  const list: WorkspaceRecentEntry[] = useMemo(() => recents ?? [], [recents]);

  const nav = useListNavigation({
    count: list.length,
    focusOwner: "list",
    role: "menu",
    activateKeys: ["Enter"],
    idPrefix: "recent-files",
    scrollIntoView: true,
    onActivate: (index) => {
      const pick = list[index];
      if (pick) onPick(pick.path);
    },
  });

  return (
    <ModalShell
      label="Recent files"
      onClose={onClose}
      align="top"
      width={420}
      dismissOn="click"
      className="overflow-hidden p-0"
    >
      <div data-testid="files-recent-dialog">
        <header className="flex items-center justify-between border-b border-line px-3 py-2">
          <span className="text-[13px] font-medium text-text">Recent files</span>
          <span className="text-[10px] text-text-3">{workspaceRoot || "No workspace"}</span>
        </header>
        <ul
          className="max-h-[260px] overflow-y-auto p-1 text-[12px] focus:outline-none"
          // biome-ignore lint/a11y/noNoninteractiveTabindex: getListProps() sets role="menu" (focusOwner: "list"), so this ul is the focusable composite widget; Biome cannot see a role that arrives through the spread.
          tabIndex={0}
          {...nav.getListProps()}
        >
          {list.length === 0 ? (
            <li className="px-3 py-4 text-text-3" data-testid="files-recent-empty">
              No recent files yet.
            </li>
          ) : (
            list.map((entry, idx) => (
              // biome-ignore lint/a11y/useKeyWithClickEvents: keyboard activation is the list's own (aria-activedescendant on the ul, Enter in onActivate); this click is the pointer path to the same onPick.
              <li
                key={entry.path}
                {...nav.getOptionProps(idx)}
                onClick={() => onPick(entry.path)}
                className={`flex w-full cursor-pointer items-center justify-between gap-2 rounded px-2 py-1 text-left ${
                  idx === nav.activeIndex ? "bg-accent/20 text-text" : "text-text-2"
                }`}
              >
                <span className="truncate font-mono">{entry.name}</span>
                <span className="text-[10px] text-text-3">{entry.openedAt}</span>
              </li>
            ))
          )}
        </ul>
        <footer className="flex items-center justify-between border-t border-line px-3 py-1.5 text-[10px] text-text-3">
          <span>{list.length} entries</span>
          <span>Esc to close</span>
        </footer>
      </div>
    </ModalShell>
  );
}
