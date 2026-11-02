// @vitest-environment happy-dom
import { render, screen } from "@testing-library/react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import SettingsPaneContent from "./SettingsPaneContent.js";
import type { SettingsPaneProps } from "./types.js";

const stubModel = {
  id: "gpt-test",
  displayName: "Test Model",
  provider: "test",
  modelSlug: "gpt-test",
  contextWindow: 8192,
  supportsVision: false,
  supportsTools: true,
  enabled: true,
};

const stubConductor = {
  id: "minimax/minimax-m3",
  label: "MiniMax M3",
  aliases: ["minimax-m3"],
  supportsThinkingLevels: false,
  supportsThinkingBudgetTokens: false,
};

const registryStub = {
  defaultBackend: "local",
  backends: [
    {
      id: "local",
      name: "local",
      kind: "local" as const,
      enabled: true,
      persistent: false,
      workspaceLayout: {
        root: "",
        uploads: "",
        workspace: "",
        outputs: "",
        logs: "",
        skills: "",
        metadata: "",
      },
      resourceLimits: {},
      supports: {
        run: true,
        spawn: true,
        poll: true,
        stdin: true,
        kill: true,
        networkIsolation: false,
        persistentWorkspace: false,
      },
    },
  ],
  networkProfiles: [],
};

const minimalProps = {
  settings: null,
  settingsDraft: null,
  settingsDirty: false,
  settingsConfigStatus: null,
  settingsConfigPending: false,
  localModelCatalog: null,
  localModelProbeBaseUrl: "",
  localModelProbeResult: null,
  localModelChatSmokeResult: null,
  localProviderSuggestion: null,
  localModelActionPending: null,
  localModelError: null,
  diagnostics: null,
  coreSandboxSettings: null,
  supportBundle: null,
  supportBundlePending: null,
  instructionStatus: null,
  diagnosticsLoading: false,
  diagnosticsError: null,
  executionBackendRegistry: registryStub,
  executionDoctorReports: {},
  executionActionPending: null,
  executionError: null,
  telegramStatus: null,
  telegramTest: null,
  telegramUsers: null,
  remoteChannels: null,
  telegramStatusLoading: false,
  telegramUsersLoading: false,
  telegramStatusError: null,
  telegramUsersError: null,
  pluginDir: "",
  plugins: [],
  pluginActionPending: null,
  pluginInstallDraft: {
    sourcePath: "",
    trustAccepted: false,
    overwrite: false,
  },
  pluginMarketplacePath: "",
  pluginMarketplaceEntries: [],
  pluginMarketplaceQuery: "",
  pluginMarketplaceSyncDraft: {
    catalogUrl: "",
    expectedSha256: "",
    trustAccepted: false,
    requireSigned: true,
    allowFileUrls: false,
  },
  pluginMarketplaceInstallDraft: {
    expectedSha256: "",
    trustAccepted: false,
    overwrite: false,
    allowFileUrls: false,
  },
  pluginMarketplaceError: null,
  mcpServers: [],
  mcpProbeResults: {},
  mcpServerModeStatus: null,
  mcpUserConfigPath: "",
  mcpWorkspaceConfigPath: null,
  mcpDraft: {
    name: "",
    scope: "user" as const,
    type: "stdio" as const,
    trustPreset: "",
    channelAllowlist: "",
    allowMutating: false,
    url: "",
    command: "",
    args: "",
  },
  mcpActionPending: null,
  mcpError: null,
  lspStatus: null,
  lspDraft: [],
  lspActionPending: null,
  lspError: null,
  selectedSession: null,
  permissionMode: "ask" as const,
  permissionRules: [],
  permissionRuleDraft: { pattern: "", effect: "ask" as const },
  permissionRuleActionPending: null,
  persistentPermissionRuleDraft: {
    scope: "user" as const,
    pattern: "",
    effect: "ask" as const,
  },
  persistentPermissionRuleActionPending: null,
  persistentPermissionRuleStorePath: "",
  persistentUserPermissionRules: [],
  persistentProjectPermissionRules: [],
  persistentPermissionRulesError: null,
  permissionRuleJsonDraft: "",
  permissionRuleJsonPending: null,
  permissionDenials: [],
  permissionDenialStorePath: "",
  permissionDenialsLoading: false,
  permissionDenialsError: null,
  skillDirs: [],
  skills: [],
  selectedSkillSlug: null,
  selectedSkillContent: "",
  skillGovernance: null,
  skillOptimizationRuns: [],
  skillDraftProvenanceReview: null,
  selectedSkillOptimizationRunId: null,
  skillTrainingRunDraft: {
    name: "",
    skillId: "",
    objective: "",
    optimizer: "",
    targetModel: "",
    gateName: "",
    gateKind: "hard" as const,
    gateThreshold: "0",
  },
  skillTrainingEpochDraft: {
    proposedBody: "",
    validationScore: "0",
    operation: "",
    summary: "",
    gateMode: "",
    threshold: "0",
  },
  selectedSkillProposalId: null,
  skillDraft: {
    slug: "",
    name: "",
    description: "",
    trigger: "",
    sourceUrl: "",
    tags: "",
    content: "",
    enabled: false,
  },
  skillActionPending: null,
  skillError: null,
  settingsToolsets: null,
  gatewaySnapshot: null,
  toolsetDraft: {
    id: "",
    name: "",
    description: "",
    allowedTools: "",
    disallowedTools: "",
    readOnly: false,
    defaultBehavioralSkills: "",
  },
  toolsetActionPending: null,
  toolsetError: null,
  middlewareSnapshot: {
    middleware: [],
    spans: [],
    observations: [],
  },
  runtimeMiddlewarePending: null,
  runtimeMiddlewareError: null,
  workspaceRoot: "",
  engineReady: true,
  config: null,
  activeTab: "conductors" as const,
  newSessionControls: {
    driverKind: "native",
    model: stubModel.id,
    effort: "",
    thinkingBudgetTokens: "",
    disableThinking: false,
    fableMode: false,
    executionBackend: "local",
    executionWorkspaceId: "",
    allowedTools: "",
    disallowedTools: "",
  },
  modelOptions: [stubConductor],
  newSessionWorkspaces: [],
  newSessionWorkspacePending: false,
  onNewSessionControls: () => {},
  onCreateExecutionWorkspace: () => {},
  onTab: () => {},
  onOpenWorkspace: () => {},
  onSettingsDraft: () => {},
  onReloadSettingsConfig: () => {},
  onSaveSettings: () => {},
  onDiscardSettings: () => {},
  onRefreshLocalModels: () => {},
  onLocalModelProbeBaseUrl: () => {},
  onProbeLocalModelEndpoint: () => {},
  onSmokeLocalModelEndpoint: () => {},
  onSuggestLocalProvider: () => {},
  onRefreshDiagnostics: () => {},
  onCreateSupportBundle: () => {},
  onDownloadSupportBundle: () => {},
  onRefreshExecutionBackends: () => {},
  onDoctorExecutionBackend: () => {},
  onSetDefaultExecutionBackend: () => {},
  onSetExecutionBackendEnabled: () => {},
  onSaveExecutionBackendResourceLimits: () => {},
  onSaveExecutionBackendNetworkProfile: () => {},
  onSaveCoreSandboxTier: () => {},
  onSaveExecutionNetworkProfileDefinition: () => {},
  onDeleteExecutionNetworkProfileDefinition: () => {},
  onRefreshTelegramStatus: () => {},
  onDiscoverTelegramUsers: () => {},
  onRefreshPlugins: () => {},
  onTogglePlugin: () => {},
  onPluginInstallDraft: () => {},
  onPickPluginSource: () => {},
  onInstallPlugin: () => {},
  onPluginMarketplaceQuery: () => {},
  onRefreshPluginMarketplace: () => {},
  onPluginMarketplaceSyncDraft: () => {},
  onSyncPluginMarketplace: () => {},
  onPluginMarketplaceInstallDraft: () => {},
  onInstallMarketplacePlugin: () => {},
  onMcpDraft: () => {},
  onSaveMcpServer: () => {},
  onRemoveMcpServer: () => {},
  onProbeMcpServer: () => {},
  onRefreshMcp: () => {},
  onRefreshLsp: () => {},
  onLspDraft: () => {},
  onSaveLsp: () => {},
  onRefreshSkills: () => {},
  onRefreshSkillGovernance: () => {},
  onRefreshSkillOptimizationRuns: () => {},
  onSelectSkill: () => {},
  onSelectSkillOptimizationRun: () => {},
  onSelectSkillProposal: () => {},
  onSelectedSkillContent: () => {},
  onSkillDraft: () => {},
  onSkillTrainingRunDraft: () => {},
  onSkillTrainingEpochDraft: () => {},
  onInstallSkill: () => {},
  onSaveSkill: () => {},
  onToggleSkill: () => {},
  onDeleteSkill: () => {},
  onQuarantineSkillProposal: () => {},
  onMarkSkillProposalBenchmark: () => {},
  onReviewSkillDraftProvenance: () => {},
  onCreateSkillTrainingRun: () => {},
  onProposeSkillTrainingCandidate: () => {},
  onExecuteSkillTrainingEpoch: () => {},
  onTransitionSkill: () => {},
  onToolsetDraft: () => {},
  onRefreshToolsets: () => {},
  onSaveToolset: () => {},
  onDeleteToolset: () => {},
  onRefreshRuntimeMiddleware: () => {},
  onToggleRuntimeMiddleware: () => {},
  licenseView: null,
  licenseActionPending: null,
  licenseLatestVersion: "",
  licenseUpdateState: "idle" as const,
  licenseError: null,
  onCheckForUpdate: () => {},
  onInstallUpdate: () => {},
  onPermissionRuleDraft: () => {},
  onAddPermissionRule: () => {},
  onRemovePermissionRule: () => {},
  onPersistentPermissionRuleDraft: () => {},
  onAddPersistentPermissionRule: () => {},
  onRemovePersistentPermissionRule: () => {},
  onPermissionRuleJsonDraft: () => {},
  onExportPermissionRules: () => {},
  onImportPermissionRules: () => {},
  onRefreshPermissions: () => {},
  onCyclePermissionMode: () => {},
} satisfies SettingsPaneProps;

describe("SettingsPaneContent — conductors tab", () => {
  it("renders a <ConductorsTab> heading with a card per external CLI", () => {
    const html = renderToStaticMarkup(
      <SettingsPaneContent {...minimalProps} activeTab="conductors" />,
    );
    expect(html).toContain("Conductors");
    expect(html).toContain("Codex");
    expect(html).toContain("Claude");
    expect(html).toContain("Installed");
    expect(html).toContain("Authenticated");
    expect(html).toContain("Governed tools");
    expect(html).not.toContain("Session defaults");
  });

  it("never renders defaults-for-future-sessions controls (no conductor/model/backend picker, no Fable toggle)", () => {
    const html = renderToStaticMarkup(
      <SettingsPaneContent {...minimalProps} activeTab="conductors" />,
    );
    expect(html).not.toContain("Fable mode");
    expect(html).not.toContain("Execution backend");
    expect(html).not.toContain("New execution workspace");
  });

  it("shows a note to select a session when none uses Codex or Claude", () => {
    render(<SettingsPaneContent {...minimalProps} activeTab="conductors" selectedSession={null} />);
    expect(screen.getByText(/Select a session that runs on Codex or Claude/)).toBeTruthy();
  });

  it("shows driver lifecycle controls for an external-driver session", () => {
    render(
      <SettingsPaneContent
        {...minimalProps}
        activeTab="conductors"
        selectedSession={{
          id: "driver-session",
          title: "Codex driver",
          model: "",
          workspaceRoot: "/tmp",
          createdAt: "2026-07-27T00:00:00Z",
          updatedAt: "2026-07-27T00:00:00Z",
          driverKind: "codex",
        }}
      />,
    );
    expect(screen.getByText(/Codex driver/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Refresh binary & auth" })).toBeTruthy();
    expect(screen.getByText("Gateway")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Interrupt" })).toBeTruthy();
  });
});

describe("SettingsPaneContent — unavailable runtime surfaces", () => {
  it.each([
    ["runtime", "Runtime"],
    ["terminal", "Terminal"],
    ["tasks", "Tasks"],
  ] as const)("renders an explanatory empty state when %s data is unavailable", (activeTab, heading) => {
    const html = renderToStaticMarkup(
      <SettingsPaneContent {...minimalProps} activeTab={activeTab} />,
    );

    expect(html).toContain(heading);
    expect(html).toContain("is unavailable for this session");
  });
});
