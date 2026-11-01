import { useSyncExternalStore } from "react";
import type { AutoUpdateStatus } from "@rightkit/app-update";
import { getUpdateStatus, subscribeUpdateStatus } from "@rightkit/agent-ui-core/state/updates";

/** Latest status from the published update client; re-renders the overlay and settings label on each change. */
export function useUpdateStatus(): AutoUpdateStatus {
  return useSyncExternalStore(subscribeUpdateStatus, getUpdateStatus, getUpdateStatus);
}
