import type { ReactNode } from "react";

/** A palette entry. Register from anywhere (any layout) with `useCommands`. */
export interface Command {
  id: string;
  label: string;
  /** Section heading in the palette (first-seen order). Default "Commands". */
  group?: string;
  /** Right-aligned tag when no shortcut applies ("open tab", "recent", "Settings › Audio"). */
  hint?: string;
  icon?: ReactNode;
  /** Extra match terms; never displayed. */
  keywords?: readonly string[];
  /** Id of a registered shortcut; its live (rebound) chord is shown. */
  shortcut?: string;
  /** Literal chord to show when there is no registered shortcut. */
  chord?: string;
  /** Hide without unregistering. */
  disabled?: boolean;
  /** Re-focus the launcher after an action (not a navigation). */
  restoreFocusAfterRun?: boolean;
  run: () => void;
}

export function createCommandRegistry() {
  const sets = new Set<readonly Command[]>();
  const listeners = new Set<() => void>();
  let cache: Command[] = [];
  const rebuild = () => {
    const seen = new Map<string, Command>();
    for (const set of sets) for (const c of set) seen.set(c.id, c);
    cache = [...seen.values()].filter((c) => !c.disabled);
    for (const l of listeners) l();
  };
  return {
    /** Later registrations win on id collisions. Returns the unregister function. */
    register(commands: readonly Command[]): () => void {
      sets.add(commands);
      rebuild();
      return () => {
        sets.delete(commands);
        rebuild();
      };
    },
    list: (): readonly Command[] => cache,
    run(id: string): boolean {
      const c = cache.find((x) => x.id === id);
      if (!c) return false;
      c.run();
      return true;
    },
    subscribe(listener: () => void): () => void {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}

export type CommandRegistry = ReturnType<typeof createCommandRegistry>;
