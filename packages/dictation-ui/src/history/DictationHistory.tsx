import { useMemo } from "react";
import { getPlatform } from "@rightkit/platform-ui";
import { LibraryScreen, useGroupSelection } from "@rightkit/library-ui";
import { dictationClient } from "../host.js";
import { DELIVERY_VARIANTS, deliveryGroups, deliveryLabels } from "./deliveryRows.js";

/**
 * The dictation History page: `@rightkit/library-ui`'s list over the host's history source
 * (`DictationClient.history`, rows mapped with `deliveryRow`), with the Dictations/Cancelled
 * variants, the device groups and the transcript wording. `search` comes from wherever the host
 * renders the field.
 */
export function DictationHistory({ search = "" }: { search?: string }) {
  const groups = useMemo(() => deliveryGroups(), []);
  const labels = useMemo(() => deliveryLabels(), []);
  // Windows has no Apple sync: every row is this PC's and no stored choice may hide them.
  const { groupSelection } = useGroupSelection(groups, { fixed: getPlatform() === "windows" });
  return (
    <LibraryScreen
      source={dictationClient().history}
      search={search}
      variants={DELIVERY_VARIANTS}
      groups={groups}
      groupSelection={groupSelection}
      labels={labels}
    />
  );
}
