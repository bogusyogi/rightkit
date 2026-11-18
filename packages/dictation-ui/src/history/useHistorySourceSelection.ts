import { useGroupSelection, type LibraryGroup } from "@rightkit/library-ui";

/** Hosts choose provenance groups, defaults, storage configuration & platform policy. */
export function useHistorySourceSelection(groups: readonly LibraryGroup[], options: { fixed?: boolean } = {}) {
  const { groupSelection, setGroupSelection } = useGroupSelection(groups, options);
  return {
    groupSelection,
    sourceSelection: groupSelection,
    setSourceSelection: setGroupSelection,
  };
}
