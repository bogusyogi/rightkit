import type { SubAgentState } from "../../hooks/useSubAgents";

// Props shared by the rendered BackgroundTasksPanel and the legacy
// AgentRail surface (the legacy surface was deleted 2026-07-16; the
// type lives here so consumers don't depend on the deleted component).
export interface BackgroundTasksSurfaceProps {
  agents: SubAgentState[];
  onCancel: (id: string) => void;
  onCancelAll?: () => void;
  onOpenAgentSession?: (id: string) => void;
  sessionTitle?: string;
  goalStatus?: string;
  costUsd?: number;
}

export type { SubAgentState };
