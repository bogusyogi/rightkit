import { useCallback, useEffect, useRef, useState } from "react";
import {
  clearGoal,
  editGoal as editGoalApi,
  getGoalSnapshot,
  orchestrate,
  pauseGoal as pauseGoalApi,
  resumeGoal as resumeGoalApi,
  setGoal,
} from "../lib/daemon.js";
import type { DaemonConfig, GoalSnapshotV1, ProtocolEvent } from "../lib/types.js";

interface UseGoalManagementOptions {
  config: DaemonConfig | null;
  sessionId: string | null;
  formatError: (error: unknown, fallback: string) => string;
  onStatus: (status: string) => void;
}

export function useGoalManagement({
  config,
  sessionId,
  formatError,
  onStatus,
}: UseGoalManagementOptions) {
  const [activeGoal, setActiveGoal] = useState<GoalSnapshotV1 | null>(null);
  const [goalComposer, setGoalComposer] = useState("");
  const [showGoalComposer, setShowGoalComposer] = useState(false);
  const sessionGeneration = useRef(0);
  const currentSessionId = useRef(sessionId);

  const resetGoal = useCallback(() => {
    setActiveGoal(null);
    setGoalComposer("");
    setShowGoalComposer(false);
  }, []);

  const toggleGoalComposer = useCallback(() => {
    setShowGoalComposer((visible) => !visible);
  }, []);

  const hydrateGoal = useCallback(async () => {
    if (!config || !sessionId) return;
    const capturedSessionId = sessionId;
    const capturedGeneration = sessionGeneration.current;
    try {
      const snapshot = await getGoalSnapshot(config, capturedSessionId);
      if (sessionGeneration.current === capturedGeneration) setActiveGoal(snapshot);
    } catch (error) {
      if (sessionGeneration.current === capturedGeneration)
        onStatus(formatError(error, "Load goal failed"));
    }
  }, [config, formatError, onStatus, sessionId]);

  useEffect(() => {
    if (currentSessionId.current === sessionId) return;
    currentSessionId.current = sessionId;
    sessionGeneration.current += 1;
    resetGoal();
  }, [resetGoal, sessionId]);

  useEffect(() => {
    void hydrateGoal();
  }, [hydrateGoal]);

  // Apply a live GoalProgressUpdated event from the daemon (continuation /
  // close). The daemon is authoritative for status + budget meter.
  const applyGoalEvent = useCallback(
    (event: ProtocolEvent) => {
      if (event.$kind !== "goalProgressUpdated" || event.sessionId !== sessionId) return;
      const {
        $kind: _kind,
        sessionId: _sessionId,
        seq: _seq,
        createdAt: _createdAt,
        ...snapshot
      } = event;
      setActiveGoal(snapshot);
    },
    [sessionId],
  );

  const createGoal = useCallback(
    async (textOverride?: string) => {
      const text = (textOverride ?? goalComposer).trim();
      if (!text || !config || !sessionId) return;
      setGoalComposer("");
      setShowGoalComposer(false);
      try {
        await setGoal(config, sessionId, text);
        await orchestrate(config, sessionId, text);
        await hydrateGoal();
      } catch (err) {
        onStatus(formatError(err, "Set goal failed"));
      }
    },
    [config, formatError, goalComposer, hydrateGoal, onStatus, sessionId],
  );

  const clearActiveGoal = useCallback(async () => {
    if (!config || !sessionId) return;
    try {
      await clearGoal(config, sessionId);
      setActiveGoal(null);
    } catch {
      // Best effort; local UI state is already cleared.
    }
  }, [config, sessionId]);

  const pauseGoal = useCallback(async () => {
    if (!config || !sessionId) return;
    try {
      await pauseGoalApi(config, sessionId);
      await hydrateGoal();
    } catch {
      // Best effort; local UI already reflects the pause.
    }
  }, [config, hydrateGoal, sessionId]);

  const resumeGoal = useCallback(async () => {
    if (!config || !sessionId) return;
    try {
      await resumeGoalApi(config, sessionId);
      await hydrateGoal();
    } catch {
      // Best effort.
    }
  }, [config, hydrateGoal, sessionId]);

  const editGoal = useCallback(
    async (text: string) => {
      if (!config || !sessionId) return;
      try {
        await editGoalApi(config, sessionId, { text });
        await hydrateGoal();
      } catch {
        // Best effort; keep the edited local card text visible.
      }
    },
    [config, hydrateGoal, sessionId],
  );

  return {
    activeGoal,
    goalComposer,
    showGoalComposer,
    setGoalComposer,
    toggleGoalComposer,
    createGoal,
    clearActiveGoal,
    pauseGoal,
    resumeGoal,
    editGoal,
    applyGoalEvent,
    hydrateGoal,
    resetGoal,
  };
}

export type UseGoalManagementReturn = ReturnType<typeof useGoalManagement>;
