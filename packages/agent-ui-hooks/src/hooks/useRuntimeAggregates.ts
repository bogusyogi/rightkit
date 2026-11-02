import { useCallback } from "react";
import { useSchedulesRuntime, type ScheduleDraft } from "@rightkit/agent-ui-core/hooks/runtime/useSchedulesRuntime";
import { useLearningRuntime } from "./runtime/useLearningRuntime.js";
import { useHarnessRadarRuntime } from "./runtime/useHarnessRadarRuntime.js";
import { useSpecsRuntime } from "./runtime/useSpecsRuntime.js";
import { useRecipesRuntime } from "./runtime/useRecipesRuntime.js";
import { useMiddlewareObservationsRuntime } from "./runtime/useMiddlewareObservationsRuntime.js";
import {
  useWebhookTriggersRuntime,
  type WebhookTriggerDraft,
} from "@rightkit/agent-ui-core/hooks/runtime/useWebhookTriggersRuntime";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types";

export type { ScheduleDraft, WebhookTriggerDraft };

interface UseRuntimeAggregatesOptions {
  config: DaemonConfig | null;
  sessionId: string | null;
  sessionWorkspaceRoot: string | null;
  activeSessionSpecId: string | null;
  workspaceRoot: string;
  formatError: (error: unknown, fallback: string) => string;
  onLoadRuntimePanels: () => Promise<unknown>;
  onLoadTeams: () => Promise<unknown>;
  onStatus: (status: string) => void;
}

export function useRuntimeAggregates({
  config,
  sessionId,
  sessionWorkspaceRoot,
  activeSessionSpecId,
  workspaceRoot,
  formatError,
  onLoadRuntimePanels,
  onLoadTeams,
  onStatus,
}: UseRuntimeAggregatesOptions) {
  const runtimeWorkspace = sessionWorkspaceRoot || workspaceRoot;

  const schedulesRuntime = useSchedulesRuntime({
    config,
    runtimeWorkspace,
    onStatus,
    formatError,
  });

  const webhookTriggersRuntime = useWebhookTriggersRuntime({
    config,
    runtimeWorkspace,
    onStatus,
    formatError,
  });

  const learningRuntime = useLearningRuntime({
    config,
    sessionId,
    onStatus,
    formatError,
  });

  const harnessRadarRuntime = useHarnessRadarRuntime({
    config,
    runtimeWorkspace,
    onStatus,
    formatError,
    refreshSchedules: schedulesRuntime.refreshSchedules,
    refreshLearning: learningRuntime.loadLearningRuntime,
  });

  const specsRuntime = useSpecsRuntime({
    config,
    runtimeWorkspace,
    activeSessionSpecId,
    sessionId,
    onStatus,
    formatError,
  });

  const recipesRuntime = useRecipesRuntime({
    config,
    runtimeWorkspace,
    onStatus,
    formatError,
    refreshSchedules: schedulesRuntime.refreshSchedules,
  });

  const middlewareRuntime = useMiddlewareObservationsRuntime({ config, sessionId });

  const loadRuntime = useCallback(async () => {
    if (!config) return;
    try {
      await Promise.all([
        learningRuntime.loadLearningRuntime(),
        middlewareRuntime.loadMiddlewareObservations(),
        harnessRadarRuntime.loadHarnessRadar(),
        specsRuntime.loadSpecsRuntime(),
        schedulesRuntime.refreshSchedules(),
        webhookTriggersRuntime.refreshWebhookTriggers(),
        recipesRuntime.loadRecipes(),
        onLoadTeams(),
        onLoadRuntimePanels(),
      ]);
    } catch (err) {
      onStatus(formatError(err, "Runtime failed"));
    }
  }, [
    config,
    formatError,
    harnessRadarRuntime.loadHarnessRadar,
    learningRuntime.loadLearningRuntime,
    middlewareRuntime.loadMiddlewareObservations,
    onLoadRuntimePanels,
    onLoadTeams,
    onStatus,
    recipesRuntime.loadRecipes,
    schedulesRuntime.refreshSchedules,
    specsRuntime.loadSpecsRuntime,
    webhookTriggersRuntime.refreshWebhookTriggers,
  ]);

  const submitLearningDecision = useCallback(
    async (proposalId: string, decision: "approve" | "reject") => {
      await learningRuntime.submitLearningDecision(proposalId, decision, async () => {
        await loadRuntime();
      });
    },
    [learningRuntime.submitLearningDecision, loadRuntime],
  );

  const openRecipeDeeplink = useCallback(
    (url: string) => recipesRuntime.openRecipeDeeplink(url),
    [recipesRuntime.openRecipeDeeplink],
  );

  return {
    ...learningRuntime,
    ...schedulesRuntime,
    ...webhookTriggersRuntime,
    ...harnessRadarRuntime,
    ...specsRuntime,
    ...recipesRuntime,
    ...middlewareRuntime,
    loadRuntime,
    openRecipeDeeplink,
    submitLearningDecision,
  };
}

export type UseRuntimeAggregatesReturn = ReturnType<typeof useRuntimeAggregates>;
