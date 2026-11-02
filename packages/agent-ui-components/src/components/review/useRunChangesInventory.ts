import { useEffect } from "react";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";
import { getRunChangesInventory, getRunChangesSummary } from "./runChangesClient.js";
import { dispatch, resetStore } from "./useRunChangesState.js";

/** Hydrate the one run-scoped review store from the daemon source of truth. */
export function useRunChangesInventory(
  config: DaemonConfig | null | undefined,
  runId: string | null | undefined,
  workspaceRoot: string | null | undefined,
  refreshKey = 0,
) {
  useEffect(() => {
    if (!config || !runId || !workspaceRoot) {
      resetStore();
      return;
    }
    const controller = new AbortController();
    const requestedRefreshKey = refreshKey;
    dispatch({ type: "capture/start", runId, workspaceRoot });
    const load = () =>
      Promise.all([
        getRunChangesInventory(config, { runId, workspaceRoot }, { signal: controller.signal }),
        getRunChangesSummary(config, { runId, workspaceRoot }, { signal: controller.signal }),
      ])
        .then(([inventory, summary]) => {
          if (!controller.signal.aborted && requestedRefreshKey === refreshKey) {
            dispatch({ type: "capture/ok", payload: inventory, summary });
          }
        })
        .catch((error: unknown) => {
          if (controller.signal.aborted) return;
          const status = (error as { status?: number }).status;
          // A new run has no change record yet. That is an empty state, not an
          // error banner. A tool result can publish its capture immediately
          // after this first request, so keep polling while this session lives.
          if (status === 404) {
            if (!controller.signal.aborted) window.setTimeout(load, 500);
            return;
          }
          dispatch({
            type: "capture/fail",
            message: error instanceof Error ? error.message : "review inventory failed",
          });
        });
    void load();
    return () => {
      controller.abort();
    };
  }, [config, runId, workspaceRoot, refreshKey]);
}
