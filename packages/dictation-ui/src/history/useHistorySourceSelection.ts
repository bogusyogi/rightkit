import { useMemo } from "react";
import { getPlatform } from "@rightkit/platform-ui";
import { useGroupSelection } from "@rightkit/library-ui";
import { deliveryGroups, type HistorySource } from "./deliveryRows.js";

/** Which devices' records History shows, by device key. */
export type SourceSelection = Record<HistorySource, boolean>;

/**
 * The History device choice (this desktop on, iPhone and Watch off until switched on), kept in the host's
 * `groupSelection` storage key and shared by every mounted hook: the History page and any control the host
 * renders elsewhere (a title bar's checkboxes, `HISTORY_SOURCES`). Windows has no Apple sync, so there the
 * choice is fixed to this PC and writes are ignored. `groupSelection` is the list's shape; `sourceSelection`
 * the same choice by device.
 */
export function useHistorySourceSelection() {
  const groups = useMemo(() => deliveryGroups(), []);
  const { groupSelection, setGroupSelection } = useGroupSelection(groups, { fixed: getPlatform() === "windows" });
  return {
    groupSelection,
    sourceSelection: groupSelection as SourceSelection,
    setSourceSelection: setGroupSelection as (next: SourceSelection) => void,
  };
}
