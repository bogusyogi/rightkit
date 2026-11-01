import type {
  BenchmarkGateKind,
  BudgetConfig,
  McpServerScope,
  McpServerType,
  OrchestrationSettings,
  PermissionRuleEffect,
  PersistentPermissionRuleScope,
  RoutingSettings,
} from "@rightkit/agent-ui-core/lib/types";
import type { InferenceSelection } from "@rightkit/agent-ui-core/lib/types/inference";
import type { IconName } from "@rightkit/agent-ui-core/components/Icon";

export type SettingsTab =
  | "appearance"
  | "memory"
  | "models"
  | "providers"
  | "repair"
  | "conductors"
  | "mcp"
  | "lsp"
  | "plugins"
  | "skills"
  | "tools"
  | "runtime"
  | "middleware"
  | "terminal"
  | "tasks"
  | "permissions"
  | "execution"
  | "remote"
  | "diagnostics"
  | "license"
  | "budgets"
  | "workspace";

export const SETTINGS_TABS: SettingsTab[] = [
  "appearance",
  "memory",
  "models",
  "providers",
  "repair",
  "conductors",
  "mcp",
  "lsp",
  "plugins",
  "skills",
  "tools",
  "runtime",
  "middleware",
  "terminal",
  "tasks",
  "permissions",
  "execution",
  "remote",
  "diagnostics",
  "license",
  "budgets",
  "workspace",
];

export interface SettingsNavItem {
  tab: SettingsTab;
  label: string;
  icon: IconName;
}

export const SETTINGS_NAV_ITEMS: SettingsNavItem[] = [
  { tab: "appearance", label: "Appearance", icon: "panel" },
  { tab: "memory", label: "Project memory", icon: "file" },
  { tab: "models", label: "Models", icon: "spark" },
  { tab: "providers", label: "Providers", icon: "cloud" },
  { tab: "repair", label: "Continuity & repair", icon: "refresh" },
  { tab: "conductors", label: "Conductors", icon: "command" },
  { tab: "mcp", label: "MCP", icon: "server" },
  { tab: "lsp", label: "LSP", icon: "activity" },
  { tab: "plugins", label: "Plugins", icon: "settings" },
  { tab: "skills", label: "Skills", icon: "spark" },
  { tab: "tools", label: "Tools", icon: "lock" },
  { tab: "runtime", label: "Runtime", icon: "activity" },
  { tab: "middleware", label: "Middleware", icon: "server" },
  { tab: "terminal", label: "Terminal", icon: "terminal" },
  { tab: "tasks", label: "Tasks", icon: "tasks" },
  { tab: "permissions", label: "Permissions", icon: "lock" },
  { tab: "execution", label: "Execution", icon: "server" },
  { tab: "remote", label: "Remote", icon: "server" },
  { tab: "diagnostics", label: "Diagnostics", icon: "activity" },
  { tab: "license", label: "License", icon: "lock" },
  { tab: "budgets", label: "Budgets", icon: "lock" },
  { tab: "workspace", label: "Workspace", icon: "folder" },
];

export type SettingsGroupId =
  | "general"
  | "modelsProviders"
  | "capabilities"
  | "execution"
  | "access"
  | "advanced";

export interface SettingsNavGroup {
  id: SettingsGroupId;
  label: string;
  tabs: SettingsTab[];
}

// Grouped nav order — the modal renders sections in this order (top → bottom),
// each section's rows in the order listed here. Single source of truth: every
// tab belongs to exactly one group; SETTINGS_NAV_GROUPS must cover every
// SettingsTab value or surface `workspace` as a fallback.
export const SETTINGS_NAV_GROUPS: SettingsNavGroup[] = [
  {
    id: "general",
    label: "General",
    tabs: ["appearance", "memory", "workspace"],
  },
  {
    id: "modelsProviders",
    label: "Models & Providers",
    tabs: ["models", "providers", "repair", "conductors"],
  },
  {
    id: "capabilities",
    label: "Capabilities",
    tabs: ["mcp", "lsp", "plugins", "skills", "tools"],
  },
  {
    id: "execution",
    label: "Execution",
    tabs: ["runtime", "middleware", "terminal", "tasks", "execution", "remote"],
  },
  {
    id: "access",
    label: "Access",
    tabs: ["permissions", "budgets"],
  },
  {
    id: "advanced",
    label: "Advanced",
    tabs: ["license", "diagnostics"],
  },
];

export function navItemForTab(tab: SettingsTab): SettingsNavItem {
  const found = SETTINGS_NAV_ITEMS.find((item) => item.tab === tab);
  // SETTINGS_NAV_ITEMS is the single source of truth — fall back to the first
  // item so we never throw in the modal mount path.
  return found ?? SETTINGS_NAV_ITEMS[0];
}

export interface McpDraft {
  name: string;
  scope: McpServerScope;
  type: McpServerType;
  trustPreset: string;
  channelAllowlist: string;
  allowMutating: boolean;
  url: string;
  command: string;
  args: string;
}

export interface PermissionRuleDraft {
  pattern: string;
  effect: PermissionRuleEffect;
}

export interface PersistentPermissionRuleDraft {
  scope: PersistentPermissionRuleScope;
  pattern: string;
  effect: PermissionRuleEffect;
}

export interface PluginInstallDraft {
  sourcePath: string;
  trustAccepted: boolean;
  overwrite: boolean;
}

export interface PluginMarketplaceSyncDraft {
  catalogUrl: string;
  expectedSha256: string;
  trustAccepted: boolean;
  requireSigned: boolean;
  allowFileUrls: boolean;
}

export interface PluginMarketplaceInstallDraft {
  expectedSha256: string;
  trustAccepted: boolean;
  overwrite: boolean;
  allowFileUrls: boolean;
}

export interface SkillInstallDraft {
  slug: string;
  name: string;
  description: string;
  trigger: string;
  sourceUrl: string;
  tags: string;
  content: string;
  enabled: boolean;
}

export interface SkillTrainingRunDraft {
  name: string;
  skillId: string;
  objective: string;
  optimizer: string;
  targetModel: string;
  gateName: string;
  gateKind: BenchmarkGateKind;
  gateThreshold: string;
}

export interface SkillTrainingEpochDraft {
  proposedBody: string;
  validationScore: string;
  operation: string;
  summary: string;
  gateMode: "" | BenchmarkGateKind;
  threshold: string;
}

export interface ToolsetDraft {
  id: string;
  name: string;
  description: string;
  allowedTools: string;
  disallowedTools: string;
  readOnly: boolean;
  defaultBehavioralSkills: string;
}

export interface SettingsDraft {
  primaryOrchestratorModelId: string;
  primarySelection?: InferenceSelection | null;
  roleSelections?: Record<string, InferenceSelection>;
  roleModelMode: OrchestrationSettings["roleModelMode"];
  roleModelSplitAvailable: boolean;
  /** Per-role model overrides keyed by agent id (e.g. `{ coder: "qwen/..." }`).
   *  Only entries the user changes are sent; empty = built-in default. */
  modelOverrides: Record<string, string>;
  budget: BudgetConfig;
  benchmarks: OrchestrationSettings["benchmarks"];
  remote: {
    telegramToken: string;
    clearTelegramToken: boolean;
    telegramAllowedUsersText: string;
    webEnabled: boolean;
    telegramTokenConfigured: boolean;
    webEnabledSource?: string | null;
  };
  /** Capability/cost router toggle + aggressiveness (R1a-L4 Settings > Models). */
  routing: RoutingSettings & { capabilityRouterSource?: string | null };
  /** Keep the engine running after the last client detaches so headless
   * schedules (Telegram delivery, cron, recipe schedules) keep firing. */
  keepDaemonResident: boolean;
}

export type { SettingsPaneProps } from "./SettingsPaneProps";

export const permissionRulePresets: {
  label: string;
  pattern: string;
  effect: PermissionRuleEffect;
}[] = [
  { label: "Ask before push", pattern: "shell(git push*)", effect: "ask" },
  { label: "Deny secrets", pattern: "read_file(**/.env*)", effect: "deny" },
  { label: "Allow source edits", pattern: "FileEdit(src/**)", effect: "allow" },
  {
    label: "Ask package changes",
    pattern: "FileEdit(package*.json)",
    effect: "ask",
  },
];
