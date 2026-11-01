import type { ExecutionBackendConfig, ExecutionNetworkDefault } from "@rightkit/agent-ui-core/lib/types";

export type { ExecutionBackendConfig };

export interface NetworkProfileDraft {
  id: string;
  name: string;
  defaultMode: ExecutionNetworkDefault;
  allowHosts: string;
  allowPrivateNetworks: boolean;
  denyPrivateNetworks: boolean;
}

export interface ResourceLimitsDraft {
  cpu: string;
  memoryMb: string;
  diskMb: string;
}

export type ResourceLimitsChange = (limits: {
  cpu?: number | null;
  memoryMb?: number | null;
  diskMb?: number | null;
}) => void;
export type NetworkProfileChange = (networkProfile: string | null) => void;

export interface RemoteStatusCardProps {
  title: string;
  value: string;
  status: "pass" | "warn" | "block";
  detail?: string;
}
