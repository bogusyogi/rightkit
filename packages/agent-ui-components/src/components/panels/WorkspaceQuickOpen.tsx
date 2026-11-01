// D1 — WorkspaceQuickOpen palette.
//
// Fuzzy file picker that surfaces any entry the panel has loaded so far.
// The palette is intentionally client-side (the daemon already returned the
// tree; we don't re-query) and supports:
//   - ↑/↓ to move the cursor (RightKit useListNavigation)
//   - Enter to activate
//   - Esc to close (RightKit ModalShell)
//   - substring match on the workspace-relative path (case-insensitive)

import { useListNavigation } from "@rightkit/platform-ui/headless";
import { ModalShell } from "@rightkit/react-shell/react";
import { useMemo, useState } from "react";
import type { WorkspaceEntry } from "@rightkit/agent-ui-core/lib/types/workspace-files";

export interface WorkspaceQuickOpenProps {
  workspaceRoot: string;
  entries: WorkspaceEntry[];
  onClose: () => void;
  onPick: (path: string) => void;
}

const MAX_VISIBLE = 64;

function flatten(entries: WorkspaceEntry[], prefix = ""): string[] {
  // Sub-entries are not pre-loaded by the root handler; the palette only offers
  // what the panel already knows about. A refresh after expanding directories
  // adds more paths to this list.
  return entries.map((entry) => (prefix ? `${prefix}/${entry.name}` : entry.name));
}

export default function WorkspaceQuickOpen({
  workspaceRoot,
  entries,
  onClose,
  onPick,
}: WorkspaceQuickOpenProps) {
  const [query, setQuery] = useState("");

  const candidates = useMemo(() => flatten(entries), [entries]);
  const filtered = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    if (!normalized) return candidates;
    return candidates.filter((candidate) => candidate.toLowerCase().includes(normalized));
  }, [candidates, query]);
  const visible = useMemo(() => filtered.slice(0, MAX_VISIBLE), [filtered]);

  const nav = useListNavigation({
    count: visible.length,
    resetKey: query,
    homeEnd: false,
    scrollIntoView: true,
    idPrefix: "quick-open",
    onActivate: (index) => {
      const pick = visible[index];
      if (pick) onPick(pick);
    },
  });

  return (
    <ModalShell
      label="Quick open file"
      onClose={onClose}
      align="top"
      width={480}
      dismissOn="click"
      className="overflow-hidden p-0"
    >
      <div data-testid="files-quick-open-dialog">
        <input
          aria-label="Filter files"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Type a file path…"
          data-testid="files-quick-open-input"
          className="block w-full rounded-t-lg border-b border-line bg-panel-2 px-3 py-2 text-[13px] text-text placeholder:text-text-3 focus:outline-none"
          {...nav.getInputProps()}
        />
        <ul
          className="max-h-[260px] overflow-y-auto p-1 text-[12px]"
          data-testid="files-quick-open-list"
          {...nav.getListProps()}
        >
          {visible.length === 0 ? (
            <li className="px-3 py-4 text-text-3">No matches</li>
          ) : (
            visible.map((candidate, idx) => (
              // biome-ignore lint/a11y/useKeyWithClickEvents: keyboard activation is the filter input's own (aria-activedescendant, Enter in onActivate); this click is the pointer path to the same onPick.
              <li
                key={candidate}
                {...nav.getOptionProps(idx)}
                onClick={() => onPick(candidate)}
                className={`flex w-full cursor-pointer items-center gap-2 rounded px-2 py-1 text-left ${
                  idx === nav.activeIndex ? "bg-accent/20 text-text" : "text-text-2"
                }`}
              >
                <span className="font-mono truncate">{candidate}</span>
              </li>
            ))
          )}
        </ul>
        <footer className="flex items-center justify-between border-t border-line px-3 py-1.5 text-[10px] text-text-3">
          <span>{workspaceRoot ? workspaceRoot : "No workspace"}</span>
          <span>Esc to close</span>
        </footer>
      </div>
    </ModalShell>
  );
}
