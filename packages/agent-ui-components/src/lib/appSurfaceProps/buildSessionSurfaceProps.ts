import type WorkspaceTrustGate from "@rightkit/agent-ui-core/components/app/WorkspaceTrustGate";
import type OnboardingPanel from "../../components/app/OnboardingPanel";
import type { RunTarget } from "../../hooks/useShellUiState";
import type { WorkspaceTrustLevel, WorkspaceTrustStatus } from "@rightkit/agent-ui-core/lib/types";

type OnboardingPanelProps = Parameters<typeof OnboardingPanel>[0];
type WorkspaceTrustGateProps = Parameters<typeof WorkspaceTrustGate>[0];

export type BuildOnboardingPanelPropsParams = {
  status: string;
  workspaceRoot: string;
  runTarget: RunTarget;
  showAdvancedConnect: boolean;
  urlDraft: string;
  tokenDraft: string;
  onOpenWorkspace: () => void;
  onRunTarget: (target: RunTarget) => void;
  onRetryEngine: () => void;
  onToggleAdvanced: () => void;
  onUrlDraft: (value: string) => void;
  onTokenDraft: (value: string) => void;
  onConnect: () => void;
};

export type BuildTrustGatePropsParams = {
  workspaceRoot: string;
  trust: WorkspaceTrustStatus;
  pending: WorkspaceTrustLevel | null;
  onSetWorkspaceTrust: (trust: WorkspaceTrustLevel) => void;
};

export function buildOnboardingPanelProps(
  params: BuildOnboardingPanelPropsParams,
): OnboardingPanelProps {
  return {
    status: params.status,
    workspaceRoot: params.workspaceRoot,
    runTarget: params.runTarget,
    showAdvancedConnect: params.showAdvancedConnect,
    urlDraft: params.urlDraft,
    tokenDraft: params.tokenDraft,
    onOpenWorkspace: params.onOpenWorkspace,
    onRunTarget: params.onRunTarget,
    onRetryEngine: params.onRetryEngine,
    onToggleAdvanced: params.onToggleAdvanced,
    onUrlDraft: params.onUrlDraft,
    onTokenDraft: params.onTokenDraft,
    onConnect: params.onConnect,
  };
}

export function buildTrustGateProps(params: BuildTrustGatePropsParams): WorkspaceTrustGateProps {
  return {
    workspaceRoot: params.workspaceRoot,
    trust: params.trust,
    pending: params.pending,
    onSetWorkspaceTrust: params.onSetWorkspaceTrust,
  };
}
