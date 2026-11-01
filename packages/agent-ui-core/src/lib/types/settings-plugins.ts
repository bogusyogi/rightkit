import type { MiddlewareEffect, MiddlewarePhase } from "./middleware-runtime";
import type { InferenceSelection } from "./inference";

export interface DaemonConfig {
  url: string;
  token: string;
}

export interface BudgetConfig {
  perTaskMaxUsd: number;
  perGoalMaxUsd: number;
  perSessionMaxUsd: number;
  perAgentCallMaxUsd: number;
  globalMonthlyMaxUsd: number;
  warningAt: number;
  maxJuryCallsPerRun: number;
  maxEscalationDepth: number;
  maxRetriesPerRole: number;
  maxWallClockMinutes: number;
  maxIterations: number;
  maxWorkers: number;
}

export interface BenchmarkConfig {
  dailyHealthChecksEnabled: boolean;
  promotionMinQualityDelta: number;
}

export interface RemoteSettings {
  telegramTokenConfigured: boolean;
  telegramAllowedUsers: number[];
  webEnabled: boolean;
  webEnabledConfigured?: boolean | null;
  webEnabledSource?: string | null;
}

/** Aggressiveness preset name for the capability/cost router (ADR §D3). */
export type RoutingAggressiveness = "conservative" | "balanced" | "aggressive";

/**
 * Capability/cost router settings (Lane R1a-L3/L4, ADR
 * `docs/canon/daemon-master-canon-atoms.md` §D5). Mirrors
 * `engine/crates/config::types::RoutingConfig`. Backend-only until this
 * lane; the Settings > Models toggle is R1a-L4's.
 */
export interface RoutingSettings {
  capabilityRouter: boolean;
  aggressiveness: RoutingAggressiveness;
  capabilityRouterConfigured?: boolean | null;
  capabilityRouterSource?: string | null;
}

export interface SettingsConfigFile {
  scope: string;
  path: string;
  exists: boolean;
  modifiedMs?: number | null;
  bytes?: number | null;
  sha256?: string | null;
}

export interface SettingsConfigStatus {
  workspaceRoot: string;
  fingerprint: string;
  loadedAt: string;
  files: SettingsConfigFile[];
}

export interface ModelCatalogEntry {
  id: string;
  displayName: string;
  provider: string;
  modelSlug: string;
  baseUrl?: string;
  contextWindow: number;
  inputCostPer1M?: number;
  outputCostPer1M?: number;
  supportsVision: boolean;
  supportsTools: boolean;
  enabled: boolean;
}

export interface AgentDefinition {
  id: string;
  name: string;
  role: string;
  modelId: string;
  enabled: boolean;
}

export interface OrchestrationSettings {
  workspaceRoot: string;
  defaultModel: string;
  primaryOrchestratorModelId: string;
  /** V6 authority-bound replacement for the legacy scalar model id. */
  primarySelection?: InferenceSelection | null;
  /** Per-role authority-bound overrides keyed by agent id. */
  roleSelections?: Record<string, InferenceSelection>;
  roleModelMode: "single" | "split";
  roleModelSplitAvailable: boolean;
  models: ModelCatalogEntry[];
  agents: AgentDefinition[];
  budget: BudgetConfig;
  benchmarks: BenchmarkConfig;
  remote: RemoteSettings;
  routing: RoutingSettings;
  orchestration?: {
    conductorRouting?: boolean;
    conductorRoutingConfigured?: boolean | null;
    conductorRoutingSource?: string | null;
  } | null;
  /** Keep the engine running after the desktop app closes so headless
   * schedules (Telegram delivery, cron, recipe schedules) keep firing. */
  keepDaemonResident: boolean;
}

export interface SettingsReloadResponse {
  reloaded: boolean;
  reloadedAt: string;
  configStatus: SettingsConfigStatus;
  settings: OrchestrationSettings;
}

export interface PluginManifest {
  id: string;
  name: string;
  version: string;
  description: string;
  entry: string;
  providesTools: string[];
  providesCommands: string[];
  tools?: Record<
    string,
    {
      description?: string;
      inputSchema?: unknown;
      readOnly?: boolean;
    }
  >;
  commands?: Record<
    string,
    {
      description?: string;
      usage?: string;
    }
  >;
  prompts?: {
    id: string;
    title?: string;
    body?: string;
    appliesToRoles?: string[];
  }[];
  middleware?: {
    id: string;
    title?: string;
    phase: MiddlewarePhase | string;
    effect: MiddlewareEffect | string;
    tool?: string;
    requiresToolset?: string;
    condition?: {
      argument?: string;
      contains?: string;
      equals?: unknown;
    };
    mutateArgs?: Record<string, unknown>;
    reason?: string;
    enabled?: boolean;
    order?: number;
    description?: string;
  }[];
  sandbox?: {
    isolation?: "trustedSubprocess" | "restrictedSubprocess" | "metadataOnly" | string;
    allowAbsoluteEntry?: boolean;
    timeoutMs?: number;
    inheritEnv?: boolean;
    allowedEnv?: string[];
    deniedEnv?: string[];
    stripNetworkEnv?: boolean;
  };
  enabled: boolean;
  rollbackVersions?: string[];
  hasSessionOverrides?: boolean;
  activeForSession?: boolean;
  signatureStatus?: string;
  signatureSignedBy?: string;
}

export interface PluginListResponse {
  pluginDir: string;
  plugins: PluginManifest[];
}

export interface PluginInstallResponse {
  installed: boolean;
  overwritten: boolean;
  pluginDir: string;
  installedPath: string;
  marketplacePath?: string;
  sourceUrl?: string;
  checksumSha256?: string;
  packageSha256?: string;
  signedBy?: string;
  plugin: PluginManifest;
}

export interface PluginMarketplaceEntry extends PluginManifest {
  downloads: number;
  rating: number;
  sourceUrl: string;
  sourcePath: string;
  checksumSha256: string;
  packageSha256?: string;
  signedBy: string;
  signingPublicKey?: string;
  signature?: string;
  category?: string;
  tags?: string[];
}

export interface PluginMarketplaceListResponse {
  marketplacePath: string;
  plugins: PluginMarketplaceEntry[];
  categories?: { category: string; count: number }[];
}

export interface PluginMarketplaceCategoriesResponse {
  marketplacePath: string;
  categories: { category: string; count: number }[];
  tags: { tag: string; count: number }[];
}

export interface PluginRollbackResponse {
  rolledBack: boolean;
  id: string;
  targetVersion: string;
  middlewareCount: number;
  plugin: PluginManifest;
}

export interface PluginVersionsResponse {
  id: string;
  currentVersion?: string | null;
  rollbackVersions: string[];
  marketplaceVersions: string[];
  allVersions: string[];
}

export interface SessionPluginListResponse {
  sessionId: string;
  plugins: PluginManifest[];
  enabledOverrides: string[];
  disabledOverrides: string[];
}

export interface PluginMarketplaceSyncResponse {
  synced: boolean;
  marketplacePath: string;
  catalogUrl: string;
  checksumSha256: string;
  pluginCount: number;
  requireSigned: boolean;
}
