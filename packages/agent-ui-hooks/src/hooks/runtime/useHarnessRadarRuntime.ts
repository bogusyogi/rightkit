import { useCallback, useState } from "react";
import {
  getHarnessRadar,
  promoteHarnessRadarProposal,
  scheduleHarnessRadarRefresh,
} from "@rightkit/agent-ui-core/lib/daemon";
import type { DaemonConfig, HarnessRadarSnapshot } from "@rightkit/agent-ui-core/lib/types";

const EMPTY_HARNESS_RADAR: HarnessRadarSnapshot = {
  sources: [],
  entries: [],
  capabilities: [],
  capabilityMap: {},
  proposals: [],
  changes: [],
  validations: [],
  notes: [],
};

interface UseHarnessRadarRuntimeParams {
  config: DaemonConfig | null;
  runtimeWorkspace: string;
  onStatus: (status: string) => void;
  formatError: (error: unknown, fallback: string) => string;
  /** Cross-domain: refresh schedules after scheduling a radar refresh. */
  refreshSchedules: () => Promise<unknown>;
  /** Cross-domain: refresh learning after promoting a radar proposal. */
  refreshLearning: () => Promise<unknown>;
}

export function useHarnessRadarRuntime({
  config,
  runtimeWorkspace,
  onStatus,
  formatError,
  refreshSchedules,
  refreshLearning,
}: UseHarnessRadarRuntimeParams) {
  const [harnessRadar, setHarnessRadar] = useState<HarnessRadarSnapshot>(EMPTY_HARNESS_RADAR);
  const [harnessRadarPromotionPending, setHarnessRadarPromotionPending] = useState<string | null>(
    null,
  );
  const [harnessRadarSchedulePending, setHarnessRadarSchedulePending] = useState(false);

  const loadHarnessRadar = useCallback(async () => {
    if (!config) return;
    setHarnessRadar(await getHarnessRadar(config));
  }, [config]);

  const promoteRadarProposal = useCallback(
    async (proposalId: string) => {
      if (!config) return;
      setHarnessRadarPromotionPending(proposalId);
      try {
        const proposal = await promoteHarnessRadarProposal(config, proposalId);
        await refreshLearning();
        setHarnessRadar(await getHarnessRadar(config));
        onStatus(`Harness Radar proposal queued: ${proposal.title || proposal.id}`);
      } catch (err) {
        onStatus(formatError(err, "Harness Radar promotion failed"));
      } finally {
        setHarnessRadarPromotionPending(null);
      }
    },
    [config, formatError, onStatus, refreshLearning],
  );

  const scheduleRadarRefresh = useCallback(async () => {
    if (!config || !runtimeWorkspace.trim()) return;
    setHarnessRadarSchedulePending(true);
    try {
      await scheduleHarnessRadarRefresh(config, {
        workspaceRoot: runtimeWorkspace.trim(),
        cronExpr: "0 9 * * 1",
        timeZone: "UTC",
        paused: false,
      });
      await refreshSchedules();
      onStatus("Harness Radar weekly refresh scheduled");
    } catch (err) {
      onStatus(formatError(err, "Harness Radar schedule failed"));
    } finally {
      setHarnessRadarSchedulePending(false);
    }
  }, [config, formatError, onStatus, refreshSchedules, runtimeWorkspace]);

  return {
    harnessRadar,
    harnessRadarPromotionPending,
    harnessRadarSchedulePending,
    loadHarnessRadar,
    promoteRadarProposal,
    scheduleRadarRefresh,
  };
}
