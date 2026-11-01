import { useCallback, useState } from "react";
import { deleteTeam, listTeams, orchestrate, setGoal } from "@rightkit/agent-ui-core/lib/daemon";
import type { DaemonConfig, TeamDefinition, TeamListResponse } from "@rightkit/agent-ui-core/lib/types";

interface UseTeamsRuntimeOptions {
  config: DaemonConfig | null;
  sessionId: string | null;
  formatError: (error: unknown, fallback: string) => string;
  onStatus: (status: string) => void;
  onSessionPane: () => void;
  onGoalCommitted: () => Promise<void>;
}

export function useTeamsRuntime({
  config,
  sessionId,
  formatError,
  onStatus,
  onSessionPane,
  onGoalCommitted,
}: UseTeamsRuntimeOptions) {
  const [teams, setTeams] = useState<TeamDefinition[]>([]);
  const [teamStorePath, setTeamStorePath] = useState("");
  const [teamActionPending, setTeamActionPending] = useState<string | null>(null);
  const [teamRunPendingName, setTeamRunPendingName] = useState<string | null>(null);

  const applyTeamList = useCallback((next: TeamListResponse) => {
    setTeams(next.teams);
    setTeamStorePath(next.teamStorePath);
  }, []);

  const loadTeams = useCallback(async () => {
    if (!config) {
      setTeams([]);
      setTeamStorePath("");
      return null;
    }
    const next = await listTeams(config);
    applyTeamList(next);
    return next;
  }, [applyTeamList, config]);

  const removeTeam = useCallback(
    async (name: string) => {
      if (!config) return;
      setTeamActionPending(name);
      try {
        await deleteTeam(config, name);
        await loadTeams();
        onStatus("Team deleted");
      } catch (err) {
        onStatus(formatError(err, "Delete team failed"));
      } finally {
        setTeamActionPending(null);
      }
    },
    [config, formatError, loadTeams, onStatus],
  );

  const runTeamGoal = useCallback(
    async (name: string, goal: string) => {
      if (!config || !sessionId || !goal.trim()) return;
      const scopedGoal = `Use ${name} team for this goal:\n${goal.trim()}`;
      setTeamRunPendingName(name);
      try {
        await setGoal(config, sessionId, scopedGoal);
        await orchestrate(config, sessionId, scopedGoal);
        await onGoalCommitted();
        onSessionPane();
        onStatus(`Running ${name}`);
      } catch (err) {
        onStatus(formatError(err, "Run team failed"));
      } finally {
        setTeamRunPendingName(null);
      }
    },
    [config, formatError, onGoalCommitted, onSessionPane, onStatus, sessionId],
  );

  return {
    teams,
    teamStorePath,
    teamActionPending,
    teamRunPendingName,
    loadTeams,
    removeTeam,
    runTeamGoal,
  };
}
