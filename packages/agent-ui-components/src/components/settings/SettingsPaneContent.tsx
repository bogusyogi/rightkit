import { Checkbox } from "@rightkit/react-shell/react";
import AppearanceTab from "@rightkit/agent-ui-settings/components/settings/tabs/AppearanceTab";
import BudgetsTab from "./tabs/BudgetsTab.js";
import ConductorsTab from "@rightkit/agent-ui-settings/components/settings/tabs/ConductorsTab";
import DiagnosticsTab from "@rightkit/agent-ui-settings/components/settings/tabs/DiagnosticsTab";
import ExecutionBackendsTab from "@rightkit/agent-ui-settings/components/settings/tabs/ExecutionBackendsTab";
import LicenseTab from "@rightkit/agent-ui-settings/components/settings/tabs/LicenseTab";
import LspTab from "@rightkit/agent-ui-settings/components/settings/tabs/LspTab";
import McpTab from "./tabs/McpTab.js";
import ModelsTab from "./tabs/ModelsTab.js";
import PermissionsTab from "./tabs/PermissionsTab.js";
import PluginsTab from "./tabs/PluginsTab.js";
import ProvidersTab from "@rightkit/agent-ui-settings/components/settings/tabs/ProvidersTab";
import RepairTab from "@rightkit/agent-ui-settings/components/settings/tabs/RepairTab";
import RemoteTab from "./tabs/RemoteTab.js";
import RuntimeMiddlewareTab from "@rightkit/agent-ui-settings/components/settings/tabs/RuntimeMiddlewareTab";
import SkillsTab from "./tabs/SkillsTab.js";
import ToolsTab from "./tabs/ToolsTab.js";
import WorkspaceTab from "@rightkit/agent-ui-settings/components/settings/tabs/WorkspaceTab";
import MemoryTab from "@rightkit/agent-ui-settings/components/settings/tabs/MemoryTab";
import McpDiscoverPanel from "@rightkit/agent-ui-settings/components/settings/marketplace/McpDiscoverPanel";
import SkillMarketplace from "@rightkit/agent-ui-settings/components/settings/marketplace/SkillMarketplace";
import RuntimePane from "@rightkit/agent-ui-workbench/components/RuntimePane";
import TerminalPane from "@rightkit/terminal-ui/components/TerminalPane";
import TasksPane from "@rightkit/agent-ui-workbench/components/workbench/TasksPane";
import type { SettingsPaneProps } from "./types.js";

function UnavailableRuntimeSurface({ title }: { title: string }) {
  return (
    <section className="flex max-w-[720px] flex-col gap-2 p-6">
      <h3 className="text-[16px] font-bold text-text">{title}</h3>
      <p className="text-[12px] text-text-3">
        {title} is unavailable for this session. Open a connected workspace or session to use it.
      </p>
    </section>
  );
}

export default function SettingsPaneContent(props: SettingsPaneProps) {
  const { activeTab } = props;

  switch (activeTab) {
    case "appearance":
      return <AppearanceTab />;
    case "memory":
      return <MemoryTab config={props.config} workspaceRoot={props.workspaceRoot} />;
    case "models":
      return (
        <ModelsTab
          config={props.config}
          settings={props.settings}
          settingsDraft={props.settingsDraft}
          workspaceRoot={props.workspaceRoot}
          localModelCatalog={props.localModelCatalog}
          localModelProbeBaseUrl={props.localModelProbeBaseUrl}
          localModelProbeResult={props.localModelProbeResult}
          localModelChatSmokeResult={props.localModelChatSmokeResult}
          localProviderSuggestion={props.localProviderSuggestion}
          localModelActionPending={props.localModelActionPending}
          localModelError={props.localModelError}
          onSettingsDraft={props.onSettingsDraft}
          onOpenWorkspace={props.onOpenWorkspace}
          onRefreshLocalModels={props.onRefreshLocalModels}
          onLocalModelProbeBaseUrl={props.onLocalModelProbeBaseUrl}
          onProbeLocalModelEndpoint={props.onProbeLocalModelEndpoint}
          onSmokeLocalModelEndpoint={props.onSmokeLocalModelEndpoint}
          onSuggestLocalProvider={props.onSuggestLocalProvider}
        />
      );
    case "providers":
      return (
        <ProvidersTab
          settings={props.settings}
          config={props.config}
          sessionId={props.selectedSession?.id ?? null}
          onLoginCliProvider={props.onLoginCliProvider}
        />
      );
    case "repair":
      return (
        <RepairTab providerId={props.currentCliProviderId} onCompleted={props.onRepairAction} />
      );
    case "conductors":
      return <ConductorsTab config={props.config} selectedSession={props.selectedSession} />;
    case "mcp":
      return (
        <>
          <McpTab
            servers={props.mcpServers}
            probeResults={props.mcpProbeResults}
            serverModeStatus={props.mcpServerModeStatus}
            userConfigPath={props.mcpUserConfigPath}
            workspaceConfigPath={props.mcpWorkspaceConfigPath}
            draft={props.mcpDraft}
            pendingId={props.mcpActionPending}
            error={props.mcpError}
            workspaceRoot={props.workspaceRoot}
            onDraft={props.onMcpDraft}
            onSave={props.onSaveMcpServer}
            onRemove={props.onRemoveMcpServer}
            onProbe={props.onProbeMcpServer}
            onRefresh={props.onRefreshMcp}
          />
          {props.mcpMarketplace ? <McpDiscoverPanel {...props.mcpMarketplace} /> : null}
        </>
      );
    case "lsp":
      return (
        <LspTab
          status={props.lspStatus}
          draft={props.lspDraft}
          pendingId={props.lspActionPending}
          error={props.lspError}
          workspaceRoot={props.workspaceRoot}
          onDraft={props.onLspDraft}
          onRefresh={props.onRefreshLsp}
          onSave={props.onSaveLsp}
        />
      );
    case "plugins":
      return (
        <PluginsTab
          pluginDir={props.pluginDir}
          plugins={props.plugins}
          pendingId={props.pluginActionPending}
          installDraft={props.pluginInstallDraft}
          marketplacePath={props.pluginMarketplacePath}
          marketplaceEntries={props.pluginMarketplaceEntries}
          marketplaceQuery={props.pluginMarketplaceQuery}
          marketplaceSyncDraft={props.pluginMarketplaceSyncDraft}
          marketplaceInstallDraft={props.pluginMarketplaceInstallDraft}
          marketplaceError={props.pluginMarketplaceError}
          onRefresh={props.onRefreshPlugins}
          onToggle={props.onTogglePlugin}
          onInstallDraft={props.onPluginInstallDraft}
          onPickSource={props.onPickPluginSource}
          onInstall={props.onInstallPlugin}
          onMarketplaceQuery={props.onPluginMarketplaceQuery}
          onRefreshMarketplace={props.onRefreshPluginMarketplace}
          onMarketplaceSyncDraft={props.onPluginMarketplaceSyncDraft}
          onSyncMarketplace={props.onSyncPluginMarketplace}
          onMarketplaceInstallDraft={props.onPluginMarketplaceInstallDraft}
          onInstallMarketplace={props.onInstallMarketplacePlugin}
        />
      );
    case "skills":
      return (
        <>
          <SkillsTab
            skillDirs={props.skillDirs}
          skills={props.skills}
          selectedSlug={props.selectedSkillSlug}
          selectedContent={props.selectedSkillContent}
          governance={props.skillGovernance}
          trainingRuns={props.skillOptimizationRuns}
          draftProvenanceReview={props.skillDraftProvenanceReview}
          selectedTrainingRunId={props.selectedSkillOptimizationRunId}
          trainingRunDraft={props.skillTrainingRunDraft}
          trainingEpochDraft={props.skillTrainingEpochDraft}
          selectedProposalId={props.selectedSkillProposalId}
          draft={props.skillDraft}
          pendingId={props.skillActionPending}
          error={props.skillError}
          onRefresh={props.onRefreshSkills}
          onRefreshGovernance={props.onRefreshSkillGovernance}
          onRefreshTrainingRuns={props.onRefreshSkillOptimizationRuns}
          onSelect={props.onSelectSkill}
          onSelectTrainingRun={props.onSelectSkillOptimizationRun}
          onSelectProposal={props.onSelectSkillProposal}
          onContent={props.onSelectedSkillContent}
          onDraft={props.onSkillDraft}
          onTrainingRunDraft={props.onSkillTrainingRunDraft}
          onTrainingEpochDraft={props.onSkillTrainingEpochDraft}
          onInstall={props.onInstallSkill}
          onSave={props.onSaveSkill}
          onToggle={props.onToggleSkill}
          onDelete={props.onDeleteSkill}
          onQuarantineProposal={props.onQuarantineSkillProposal}
          onMarkBenchmark={props.onMarkSkillProposalBenchmark}
          onReviewDraftProvenance={props.onReviewSkillDraftProvenance}
          onCreateTrainingRun={props.onCreateSkillTrainingRun}
          onProposeTrainingCandidate={props.onProposeSkillTrainingCandidate}
          onExecuteTrainingEpoch={props.onExecuteSkillTrainingEpoch}
            onTransitionSkill={props.onTransitionSkill}
          />
          {props.skillMarketplace ? <SkillMarketplace {...props.skillMarketplace} /> : null}
        </>
      );
    case "tools":
      return (
        <ToolsTab
          snapshot={props.settingsToolsets}
          gatewaySnapshot={props.gatewaySnapshot}
          draft={props.toolsetDraft}
          pendingId={props.toolsetActionPending}
          error={props.toolsetError}
          onDraft={props.onToolsetDraft}
          onRefresh={props.onRefreshToolsets}
          onSave={props.onSaveToolset}
          onDelete={props.onDeleteToolset}
        />
      );
    case "runtime":
      return (
        <div className="flex flex-1 min-h-0">
          <section className="flex min-w-0 max-w-[720px] flex-col gap-4 px-6 py-5 border-b border-line">
            <div className="flex flex-wrap items-start gap-3">
              <div className="flex flex-col gap-1 flex-1 basis-64 min-w-0">
                <h3 className="text-[16px] font-bold text-text">Engine residency</h3>
                <p className="text-[13px] text-text-2">
                  Keep the engine running after the app closes so headless schedules (Telegram
                  delivery, cron, recipe schedules) keep firing.
                </p>
              </div>
              <Checkbox
                className="mt-1 min-w-0"
                label="Keep the engine running after the app closes"
                checked={props.settingsDraft?.keepDaemonResident ?? false}
                disabled={!props.settingsDraft}
                onChange={(checked) => {
                  const draft = props.settingsDraft;
                  if (!draft) return;
                  props.onSettingsDraft({ ...draft, keepDaemonResident: checked });
                }}
              />
            </div>
            <p className="text-[11px] text-text-3">
              Headless schedules also force the engine to stay resident even when this toggle is
              off, so cron and Telegram delivery still fire while the desktop app is closed.
            </p>
          </section>
          {props.runtimePaneProps ? (
            <RuntimePane {...props.runtimePaneProps} />
          ) : (
            <UnavailableRuntimeSurface title="Runtime" />
          )}
        </div>
      );
    case "middleware":
      return (
        <RuntimeMiddlewareTab
          middlewareSnapshot={props.middlewareSnapshot}
          pendingId={props.runtimeMiddlewarePending}
          error={props.runtimeMiddlewareError}
          onRefresh={props.onRefreshRuntimeMiddleware}
          onToggle={props.onToggleRuntimeMiddleware}
        />
      );
    case "terminal":
      return props.terminalPaneProps ? (
        <TerminalPane {...props.terminalPaneProps} />
      ) : (
        <UnavailableRuntimeSurface title="Terminal" />
      );
    case "tasks":
      return props.tasksPaneProps ? (
        <TasksPane {...props.tasksPaneProps} />
      ) : (
        <UnavailableRuntimeSurface title="Tasks" />
      );
    case "permissions":
      return (
        <PermissionsTab
          selectedSession={props.selectedSession}
          permissionMode={props.permissionMode}
          rules={props.permissionRules}
          draft={props.permissionRuleDraft}
          pendingId={props.permissionRuleActionPending}
          persistentDraft={props.persistentPermissionRuleDraft}
          persistentPendingId={props.persistentPermissionRuleActionPending}
          persistentStorePath={props.persistentPermissionRuleStorePath}
          persistentUserRules={props.persistentUserPermissionRules}
          persistentProjectRules={props.persistentProjectPermissionRules}
          persistentError={props.persistentPermissionRulesError}
          jsonDraft={props.permissionRuleJsonDraft}
          jsonPending={props.permissionRuleJsonPending}
          denials={props.permissionDenials}
          denialStorePath={props.permissionDenialStorePath}
          denialsLoading={props.permissionDenialsLoading}
          denialsError={props.permissionDenialsError}
          onDraft={props.onPermissionRuleDraft}
          onAdd={props.onAddPermissionRule}
          onRemove={props.onRemovePermissionRule}
          onPersistentDraft={props.onPersistentPermissionRuleDraft}
          onPersistentAdd={props.onAddPersistentPermissionRule}
          onPersistentRemove={props.onRemovePersistentPermissionRule}
          onJsonDraft={props.onPermissionRuleJsonDraft}
          onJsonExport={props.onExportPermissionRules}
          onJsonImport={props.onImportPermissionRules}
          onRefresh={props.onRefreshPermissions}
          onCycleMode={props.onCyclePermissionMode}
        />
      );
    case "execution":
      return (
        <ExecutionBackendsTab
          registry={props.executionBackendRegistry}
          diagnostics={props.diagnostics}
          sandboxSettings={props.coreSandboxSettings}
          doctorReports={props.executionDoctorReports}
          pendingId={props.executionActionPending}
          error={props.executionError}
          workspaceRoot={props.workspaceRoot}
          onRefresh={props.onRefreshExecutionBackends}
          onDoctor={props.onDoctorExecutionBackend}
          onSetDefault={props.onSetDefaultExecutionBackend}
          onSetEnabled={props.onSetExecutionBackendEnabled}
          onSaveResourceLimits={props.onSaveExecutionBackendResourceLimits}
          onSaveNetworkProfile={props.onSaveExecutionBackendNetworkProfile}
          onSaveNetworkProfileDefinition={props.onSaveExecutionNetworkProfileDefinition}
          onDeleteNetworkProfileDefinition={props.onDeleteExecutionNetworkProfileDefinition}
          onSaveSandboxTier={props.onSaveCoreSandboxTier}
        />
      );
    case "remote":
      return (
        <RemoteTab
          settingsDraft={props.settingsDraft}
          telegramStatus={props.telegramStatus}
          telegramTest={props.telegramTest}
          telegramUsers={props.telegramUsers}
          remoteChannels={props.remoteChannels}
          loading={props.telegramStatusLoading}
          usersLoading={props.telegramUsersLoading}
          error={props.telegramStatusError}
          usersError={props.telegramUsersError}
          onSettingsDraft={props.onSettingsDraft}
          onRefresh={props.onRefreshTelegramStatus}
          onDiscoverUsers={props.onDiscoverTelegramUsers}
        />
      );
    case "diagnostics":
      return (
        <DiagnosticsTab
          config={props.config}
          report={props.diagnostics}
          supportBundle={props.supportBundle}
          supportBundlePending={props.supportBundlePending}
          instructionStatus={props.instructionStatus}
          loading={props.diagnosticsLoading}
          error={props.diagnosticsError}
          onRefresh={props.onRefreshDiagnostics}
          onCreateBundle={props.onCreateSupportBundle}
          onDownloadBundle={props.onDownloadSupportBundle}
        />
      );
    case "license":
      return (
        <LicenseTab
          license={props.licenseView}
          actionPending={props.licenseActionPending}
          latestVersion={props.licenseLatestVersion}
          updateState={props.licenseUpdateState}
          error={props.licenseError}
          onCheckForUpdate={props.onCheckForUpdate}
          onInstallUpdate={props.onInstallUpdate}
        />
      );
    case "budgets":
      return (
        <BudgetsTab settingsDraft={props.settingsDraft} onSettingsDraft={props.onSettingsDraft} />
      );
    case "workspace":
      return (
        <WorkspaceTab
          workspaceRoot={props.workspaceRoot}
          settingsConfigStatus={props.settingsConfigStatus}
          onOpenWorkspace={props.onOpenWorkspace}
        />
      );
    default:
      return null;
  }
}
