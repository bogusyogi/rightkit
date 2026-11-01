import { useMemo, useRef, type ReactNode } from "react";
import { MagnifyingGlass } from "@phosphor-icons/react/dist/icons/MagnifyingGlass";
import { rankItems as rankByScore } from "@rightkit/platform-ui/headless";
import { CommandPalette as ShellCommandPalette, type Command } from "@rightkit/react-shell/react";

export interface PaletteItem {
  id: string;
  label: string;
  hint?: string; // right-aligned tag: a shortcut, "open tab", "recent", etc.
  icon?: ReactNode; // optional leading glyph (Phosphor) — decorative; label stays the match target.
  run: () => void;
}

const isDocument = (it: PaletteItem) => it.id.startsWith("tab:") || it.id.startsWith("recent:");

// Keeps the app's ranking: subsequence score within each section, Documents (open tabs + recents)
// ahead of Commands. The shell groups by first-seen order, so documents must come first here.
function rankPaletteItems(commands: readonly Command[], query: string, limit: number): Command[] {
  const ranked = rankByScore(commands, query);
  const documents = ranked.filter((c) => c.group === "Documents");
  const rest = ranked.filter((c) => c.group !== "Documents");
  return [...documents, ...rest].slice(0, limit);
}

/** Ctrl+Shift+P quick-open + command runner. Thin adapter over the shell's CommandPalette. */
export function CommandPalette({ items, onClose }: { items: PaletteItem[]; onClose: () => void }) {
  // A command runs once per open palette: a second Enter or click during the exit animation is dropped.
  const ran = useRef(false);
  const commands = useMemo<Command[]>(
    () =>
      items.map((it) => ({
        id: it.id,
        label: it.label,
        hint: it.hint,
        icon: it.icon,
        group: isDocument(it) ? "Documents" : "Commands",
        run: () => {
          if (ran.current) return;
          ran.current = true;
          it.run();
        },
      })),
    [items],
  );

  return (
    <ShellCommandPalette
      commands={commands}
      onClose={onClose}
      rank={rankPaletteItems}
      limit={50}
      paletteWidth={680}
      enterDelayMs={80}
      labels={{
        placeholder: "Go to a file or run a command…",
        inputAriaLabel: "Search files and commands",
        title: "Command palette",
        results: "Results",
        empty: "No matches.",
      }}
      className="vr-pop vr-pop-modal vr-glass-panel max-w-[92vw] overflow-hidden rounded-[28px]"
      slots={{
        field: "h-[62px] gap-3 border-b border-line px-5",
        input: "vr-focus min-w-0 flex-1 bg-transparent font-sans text-[22px] text-paper placeholder:text-muted-2",
        list: "max-h-[min(56vh,520px)] overflow-auto px-2.5 pb-2.5 pt-1.5",
        group: "px-3 pb-1 pt-2.5 font-sans text-chrome-xs font-semibold text-muted-2",
        option:
          "vr-palette-opt vr-focus h-[42px] w-full gap-3 rounded-xl px-3 font-sans text-paper/85 hover:bg-ink/30 [&.is-active]:bg-copper/[0.16] [&.is-active]:text-paper",
        icon: "h-7 w-7 rounded-lg bg-ink/40 text-muted [.is-active_&]:bg-copper/20 [.is-active_&]:text-copper-hi",
        empty: "px-4 py-3 font-sans text-xs text-muted-2",
      }}
      searchIcon={<MagnifyingGlass size={20} weight="regular" aria-hidden="true" className="shrink-0 text-muted" />}
    />
  );
}
