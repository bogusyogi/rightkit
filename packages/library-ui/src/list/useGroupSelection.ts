import { useCallback, useEffect, useMemo, useState } from "react";
import type { LibraryGroup } from "../contract.js";
import { libraryStorageKey, readStored, writeStored } from "../host.js";

/** Which groups are visible, by group key. */
export type GroupSelection = Record<string, boolean>;

export function defaultGroupSelection(groups: readonly LibraryGroup[]): GroupSelection {
  return Object.fromEntries(groups.map((group) => [group.key, group.defaultOn]));
}

/** The stored choice, falling back per group to its default on first run or a corrupt value. Never throws. */
export function loadGroupSelection(groups: readonly LibraryGroup[]): GroupSelection {
  const selection = defaultGroupSelection(groups);
  try {
    const raw = readStored("groupSelection");
    if (!raw) return selection;
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    for (const group of groups) {
      if (typeof parsed[group.key] === "boolean") selection[group.key] = parsed[group.key] as boolean;
    }
  } catch {
    // Corrupt value: the defaults stand.
  }
  return selection;
}

const changedEvent = () => `${libraryStorageKey("groupSelection")}:changed`;

/**
 * Inclusive group visibility (switching a group off hides its rows), shared by the list and any
 * control the host renders elsewhere (a title bar): every mounted hook follows the same choice.
 * `fixed` pins the selection to the defaults and ignores writes (e.g. a host with one group).
 */
export function useGroupSelection(groups: readonly LibraryGroup[], options: { fixed?: boolean } = {}) {
  const fixed = options.fixed ?? false;
  const defaults = useMemo(() => defaultGroupSelection(groups), [groups]);
  const [stored, setStored] = useState<GroupSelection>(() => loadGroupSelection(groups));
  const groupSelection = fixed ? defaults : stored;

  useEffect(() => {
    const sync = (event: Event) => {
      const next = (event as CustomEvent<GroupSelection>).detail;
      setStored(next ?? loadGroupSelection(groups));
    };
    const name = changedEvent();
    window.addEventListener(name, sync);
    return () => window.removeEventListener(name, sync);
  }, [groups]);

  const setGroupSelection = useCallback(
    (next: GroupSelection) => {
      if (fixed) return;
      writeStored("groupSelection", JSON.stringify(next));
      setStored(next);
      window.dispatchEvent(new CustomEvent(changedEvent(), { detail: next }));
    },
    [fixed],
  );

  return { groupSelection, setGroupSelection };
}
