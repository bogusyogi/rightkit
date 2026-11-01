import type { HealthResponse } from "../../lib/types.js";
import type { ReactNode } from "react";

export interface AppShellHeaderProps {
  headerTitle: string;
  headerSubtitle: string;
  status: string;
  health: HealthResponse | null;
  selected: { id: string } | null;
  messagesLength: number;
  isWorking: boolean;
  onCompact: () => void;
  hasAgents?: boolean;
  agentCount?: number;
  runningAgentCount?: number;
  agentsOpen?: boolean;
  onToggleAgents?: () => void;
  onRetryEngine?: () => void;
  headerActions?: ReactNode;
}
