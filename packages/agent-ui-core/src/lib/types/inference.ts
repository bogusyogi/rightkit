export type InferenceAuthority = "direct";
export type InferenceProtocol = "openai_compatible" | "anthropic_compatible";
export type CatalogState = "ready" | "needs_secret" | "unreachable" | "unqualified" | "invalid";

export interface InferenceSelection {
  schemaVersion: 1;
  authority: InferenceAuthority;
  connectionId: string;
  protocol: InferenceProtocol;
  modelId: string;
  routePolicyId: string | null;
}

export interface CatalogModel {
  id: string;
  object: string | null;
  created: number | null;
  ownedBy: string | null;
  root: string | null;
  parent: string | null;
  permission: unknown;
  capabilities: string[];
  contextLength: number | null;
  maxInputTokens: number | null;
  maxOutputTokens: number | null;
  selectable: boolean;
  unavailableReason: string | null;
}

export interface CatalogGroup {
  authority: InferenceAuthority;
  connectionId: string;
  name: string;
  protocol: InferenceProtocol;
  state: CatalogState;
  models: CatalogModel[];
}

export interface DirectConnection {
  id: string;
  name: string;
  protocol: InferenceProtocol;
  normalizedBaseUrl: string;
  authKind: "none" | "bearer" | "x_api_key" | "api_key_header";
  authHeaderName: string | null;
  publicHeaders: Record<string, string>;
  manualModelIds: string[];
  trustPrivateNetwork: boolean;
  credentialConfigured: boolean;
  credentialRevision: number;
  configurationRevision: number;
  createdAt: string;
  updatedAt: string;
}

export interface LoadSnapshot {
  queuedAgents: number;
  activeAgents: number;
  activeLimit: number;
  perConnectionActive: Record<string, number>;
  perRouteActive: Record<string, number>;
  bufferedEvents: number;
}

export function inferenceSelectionKey(value: InferenceSelection | null | undefined): string {
  if (!value) return "";
  return JSON.stringify([
    value.schemaVersion,
    value.authority,
    value.connectionId,
    value.protocol,
    value.modelId,
    value.routePolicyId,
  ]);
}

export function selectionForCatalogModel(group: CatalogGroup, modelId: string): InferenceSelection {
  return {
    schemaVersion: 1,
    authority: group.authority,
    connectionId: group.connectionId,
    protocol: group.protocol,
    modelId,
    routePolicyId: null,
  };
}
