import { useCallback, useRef } from "react";
import type { MutableRefObject } from "react";
import {
  normalizeProjectPath,
  useProjectRecentsStore,
} from "@rightkit/agent-ui-core/state/projectRecentsStore";
import { useWorkspaceStore } from "@rightkit/agent-ui-core/state/workspaceStore";

export interface WorkspaceRuntimeInfo {
  workspaceRoot?: string | null;
  branch?: string | null;
}

export type WorkspaceContextSwitchResult =
  | { kind: "staged"; workspaceRoot: string }
  | { kind: "switched"; workspaceRoot: string; sessionId: string }
  | { kind: "ignored"; reason: "stale-request" | "session-changed" }
  | { kind: "unavailable"; reason: string };

export interface UseWorkspaceContextSwitchOptions {
  /** Ref to the actually focused session, not a closure-captured row. */
  activeSessionIdRef: MutableRefObject<string | null>;
  workspaceRoot: string;
  onWorkspaceChange: (workspaceRoot: string) => void;
  onWorkspaceRuntimeInfo?: (info: { branch: string; workspaceRoot: string }) => void;
  onUnavailable?: (reason: string) => void;
  /** Optional daemon-backed normalization for a new-session draft. */
  resolveDraftWorkspace?: (workspaceRoot: string) => Promise<WorkspaceRuntimeInfo>;
  /** Optional daemon-backed mutation. No endpoint is invented when absent. */
  setSessionWorkspace?: (
    sessionId: string,
    workspaceRoot: string,
  ) => Promise<WorkspaceRuntimeInfo>;
}

/**
 * Keeps workspace changes scoped to the draft or focused session. This ports
 * Hermes' `useCwdActions` policy: read the active-session ref at action time,
 * apply backend normalization only if the request is still current, and never
 * let a late response steal focus from a newer session.
 */
export function useWorkspaceContextSwitch({
  activeSessionIdRef,
  workspaceRoot,
  onWorkspaceChange,
  onWorkspaceRuntimeInfo,
  onUnavailable,
  resolveDraftWorkspace,
  setSessionWorkspace,
}: UseWorkspaceContextSwitchOptions) {
  const workspaceGeneration = useRef(0);
  const addOrTouch = useProjectRecentsStore((state) => state.addOrTouch);

  const publishWorkspace = useCallback(
    (target: string) => {
      const normalized = normalizeProjectPath(target);
      if (!normalized) return null;
      onWorkspaceChange(normalized);
      useWorkspaceStore.getState().resetForWorkspace(normalized);
      addOrTouch(normalized);
      return normalized;
    },
    [addOrTouch, onWorkspaceChange],
  );

  const changeWorkspace = useCallback(
    async (workspace: string): Promise<WorkspaceContextSwitchResult> => {
      const target = normalizeProjectPath(workspace);
      if (!target) return { kind: "unavailable", reason: "Workspace is empty" };

      const sessionId = activeSessionIdRef.current;
      const generation = ++workspaceGeneration.current;

      if (!sessionId) {
        // Draft changes are synchronous so a new session cannot be created
        // against the old folder while normalization is in flight.
        publishWorkspace(target);
        if (!resolveDraftWorkspace) return { kind: "staged", workspaceRoot: target };

        try {
          const info = await resolveDraftWorkspace(target);
          if (generation !== workspaceGeneration.current || activeSessionIdRef.current) {
            return { kind: "ignored", reason: "stale-request" };
          }
          const resolved = publishWorkspace(info.workspaceRoot || target) ?? target;
          onWorkspaceRuntimeInfo?.({
            branch: info.branch || "",
            workspaceRoot: resolved,
          });
          return { kind: "staged", workspaceRoot: resolved };
        } catch {
          // Match Hermes' fail-safe behavior: keep the user's selected draft
          // cwd and leave branch metadata empty when normalization is absent.
          if (generation === workspaceGeneration.current && !activeSessionIdRef.current) {
            onWorkspaceRuntimeInfo?.({ branch: "", workspaceRoot: target });
          }
          return { kind: "staged", workspaceRoot: target };
        }
      }

      if (!setSessionWorkspace) {
        const reason = "The daemon does not expose session workspace switching yet";
        onUnavailable?.(reason);
        return { kind: "unavailable", reason };
      }

      try {
        const info = await setSessionWorkspace(sessionId, target);
        if (generation !== workspaceGeneration.current) {
          return { kind: "ignored", reason: "stale-request" };
        }
        if (activeSessionIdRef.current !== sessionId) {
          return { kind: "ignored", reason: "session-changed" };
        }
        const resolved = publishWorkspace(info.workspaceRoot || target) ?? target;
        onWorkspaceRuntimeInfo?.({ branch: info.branch || "", workspaceRoot: resolved });
        return { kind: "switched", workspaceRoot: resolved, sessionId };
      } catch (error) {
        const reason = error instanceof Error ? error.message : String(error);
        onUnavailable?.(reason);
        return { kind: "unavailable", reason };
      }
    },
    [
      activeSessionIdRef,
      onUnavailable,
      onWorkspaceRuntimeInfo,
      publishWorkspace,
      resolveDraftWorkspace,
      setSessionWorkspace,
    ],
  );

  const refreshWorkspace = useCallback(
    async (workspace = workspaceRoot): Promise<WorkspaceRuntimeInfo | null> => {
      const target = normalizeProjectPath(workspace);
      if (!target || activeSessionIdRef.current || !resolveDraftWorkspace) return null;
      const generation = ++workspaceGeneration.current;
      try {
        const info = await resolveDraftWorkspace(target);
        if (generation !== workspaceGeneration.current || activeSessionIdRef.current) return null;
        const resolved = publishWorkspace(info.workspaceRoot || target) ?? target;
        const result = { branch: info.branch || "", workspaceRoot: resolved };
        onWorkspaceRuntimeInfo?.(result);
        return result;
      } catch {
        return null;
      }
    },
    [activeSessionIdRef, onWorkspaceRuntimeInfo, publishWorkspace, resolveDraftWorkspace, workspaceRoot],
  );

  return { changeWorkspace, refreshWorkspace };
}
