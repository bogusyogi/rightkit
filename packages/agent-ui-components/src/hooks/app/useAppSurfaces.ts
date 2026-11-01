// useAppSurfaces — pure surface assembly for the app shell.
//
// Hard contract (plan §"Lane C"): NO async work, NO effects, NO domain
// mutations, NO daemon/session side effects, NO canonical state ownership.
// It receives the values App.tsx already holds and returns the eight surface
// prop bundles AppBody renders, delegating to the section helpers under
// ./surfaces so this coordinator stays a thin composition root.

import { useMemo } from "react";
import { QA_BUILD } from "@rightkit/agent-ui-core/qaBuild";
import { buildShellSurfaces } from "./surfaces/shellSurfaces";
import { buildSettingsSurface } from "./surfaces/settingsSurface";
import { buildSessionSurfaces } from "./surfaces/sessionSurfaces";
import type { AppSurfaceInputs } from "./surfaces/types";
import type { ShellSurfaces } from "./surfaces/shellSurfaces";
import type { SessionSurfaces } from "./surfaces/sessionSurfaces";
import type { SubAgentState } from "@rightkit/agent-ui-core/hooks/useSubAgents";
import type { GoalSnapshotV1, SessionRow, TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";
import { hostAppName } from "../../appName";

export type { AppSurfaceInputs } from "./surfaces/types";

export interface UseAppSurfaces extends ShellSurfaces, SessionSurfaces {
  settingsPaneProps: ReturnType<typeof buildSettingsSurface>;
}

// Lane C-2 memo: App.tsx passes a fresh args object every render. We compute
// a shallow key over the fields the surfaces read and re-build only when one
// of those fields changes by identity. The shallow-stable object refs come
// from the per-slice Zustand selectors in src/state/storeSelectors.
function surfaceInputsKey(args: AppSurfaceInputs): unknown[] {
  return [
    args.goalState,
    args.runtimePanels.tasks,
    args.runtimePanels.artifacts,
    args.runtimePanels.memoryEntries,
    args.teamsRuntime.teams,
    args.runtime,
    args.skillsRuntime.skills,
    args.pluginsRuntime.plugins,
    args.pluginsRuntime.pluginMarketplaceEntries,
    args.lsp.lspStatus,
    args.composerState.composer,
    args.composerState.attachments,
    args.composerState.promptHistoryIndex,
    args.diagnosticsState,
    args.telegram.status,
    args.telegram.users,
    args.localModel,
    args.execution.backendRegistry,
    args.execution.workspaces,
    args.execution.workspacePending,
    args.mcp,
    args.permissions.rules,
    args.permissions.workspaceTrust,
    args.permissions.workspaceTrustPending,
    args.permissions.denials,
    args.config,
    args.health,
    args.status,
    args.engineReady,
    args.workspaceRoot,
    args.isWorking,
    args.pane,
    args.openSettings,
    args.runTarget,
    args.settingsTab,
    args.showWorkspaceMenu,
    args.showRunMenu,
    args.showBranchMenu,
    args.urlDraft,
    args.tokenDraft,
    args.showAdvancedConnect,
    args.licenseActionPending,
    args.licenseLatestVersion,
    args.licenseUpdateState,
    args.licenseError,
    args.selected,
    args.sessionSearch,
    args.sessionSearchPending,
    args.sessionSearchSnippets,
    args.sidebarSessions,
    args.uniqueWorkspaces,
    args.newSessionControls,
    args.settings,
    args.settingsDraft,
    args.settingsDirty,
    args.settingsConfigStatus,
    args.settingsConfigPending,
    args.enabledModelOptions,
    args.primaryModelLabel,
    args.modelUnavailable,
    args.conductorRuntimeModels,
    args.conductorModelPending,
    args.subAgents,
    args.messages,
    args.dispatchDecision,
    args.pendingQuestion,
    args.pendingPlan,
    args.streamingToolName,
    args.queuedMessages,
    args.pendingApproval,
    args.permissionMode,
    args.commands,
    args.runtimeTab,
    args.systemUser,
    args.gitInfo,
    args.branchSwitchPending,
    args.sessionCostUsd,
    args.sessionExportPending,
    args.contextPercent,
    args.usagePromptTokens,
    args.usageCompletionTokens,
    args.usageContextTokens,
    args.cacheReadTokens,
    args.usageCacheCreationTokens,
    args.awaySummary,
    args.awaySummaryPending,
    args.awaySummaryCursor,
    args.coreSandboxSettings,
    args.settingsToolsets,
    args.toolGatewaySnapshot,
    args.toolsetDraft,
    args.toolsetActionPending,
    args.toolsetError,
    args.runtimeMiddlewarePending,
    args.runtimeMiddlewareError,
    args.terminalProcesses,
    args.selectedTerminalProcessId,
    args.terminalLogs,
    args.terminalInput,
    args.terminalLoading,
    args.terminalError,
    args.terminalActionPending,
  ];
}

export function useAppSurfaces(args: AppSurfaceInputs): UseAppSurfaces {
  // useMemo over the per-field key list keeps the surfaces object identity
  // stable across renders that don't change any surface-reading field.
  return useMemo(() => {
    const shell = buildShellSurfaces(args);
    const settingsPaneProps = buildSettingsSurface(args);
    const session = buildSessionSurfaces(args);
    const surfaces = { ...shell, settingsPaneProps, ...session };
    const qaScenario = getQaScenario();
    if (qaScenario === "agents") return withQaAgentScenario(surfaces, args);
    if (qaScenario === "new-chat") return withQaNewChatScenario(surfaces, args);
    return surfaces;
    // biome-ignore lint/correctness/useExhaustiveDependencies: deps are the explicit per-field surfaceInputsKey(args); a full-args dep would defeat the identity-stability memo.
  }, surfaceInputsKey(args));
}

function getQaScenario(): string | null {
  if (typeof window === "undefined") return null;
  if (!import.meta.env.DEV && !QA_BUILD) return null;
  const params = new URLSearchParams(window.location.search);
  return params.get("qa") === "1" ? params.get("scenario") : null;
}

function withQaAgentScenario(surfaces: UseAppSurfaces, args: AppSurfaceInputs): UseAppSurfaces {
  const now = Date.now();
  const createdAt = new Date(now - 1000 * 60 * 22).toISOString();
  const qaSession: SessionRow = {
    id: "qa-agent-session",
    title: "iOS app Dynamic Island implementation",
    model: "minimax/minimax-m3",
    workspaceRoot: "/workspace/heardright",
    createdAt,
    updatedAt: new Date(now).toISOString(),
    status: "running",
    role: "ui-lane",
    toolset: "ui-lane",
    activeSkills: ["audit-visual", "qa", "blueprint"],
    executionBackend: "local",
    workspaceTrust: "trusted",
    effort: "high",
  };
  const qaGoal: GoalSnapshotV1 = {
    goalId: "qa-goal-ui",
    objective:
      `Keep improving ${hostAppName()} UI/UX until the default workflow is better than Claude Code and Codex.`,
    startedAt: new Date(now - 1000 * 60 * 18).toISOString(),
    status: "active",
    closedAt: null,
    dismissedAt: null,
    decompositionState: "usable",
    planRevision: 2,
    completionPercent: 40,
    now: "Improve default workflow",
    next: "Verify UI",
    steps: [
      {
        id: "qa-discover",
        label: "Map current interaction gaps",
        weight: 2,
        state: "completed",
        verificationState: "verified",
      },
      {
        id: "qa-implement",
        label: "Implement native workspace controls",
        weight: 2,
        state: "in_progress",
        verificationState: "pending",
      },
      {
        id: "qa-verify",
        label: "Verify installed app behavior",
        weight: 1,
        state: "pending",
        verificationState: "unverified",
      },
    ],
    verificationState: "pending",
    budgetTokens: 120000,
    tokensUsed: 48320,
    continuationCount: 2,
    noProgressCount: 0,
  };
  const qaAgents: SubAgentState[] = [
    {
      subSessionId: "qa-agent-visual",
      role: "Audit iOS Live Activity/Dynamic Island",
      taskTitle: "Audit iOS Live Activity/Dynamic Island",
      status: "running",
      startedAt: now - 1000 * 49,
      tokensIn: 114200,
      tokensOut: 6000,
      cacheRead: 9200,
      toolUses: 14,
      elapsedMs: 49000,
      lastTools: ["qa-shot", "view_image", "audit-visual"],
      summary:
        "Visual auditor: checking the live default screen against Claude Code and Codex references.",
    },
    {
      subSessionId: "qa-agent-flow",
      role: "Audit iOS background start/stop triggers",
      taskTitle: "Audit iOS background start/stop triggers",
      status: "running",
      startedAt: now - 1000 * 38,
      tokensIn: 122000,
      tokensOut: 6800,
      toolUses: 11,
      elapsedMs: 38000,
      lastTools: ["rg", "tsx", "qa-functional"],
      summary:
        "Inline progress, agent rail, Stop controls, Stop all, and full transcript links are present.",
    },
    {
      subSessionId: "qa-agent-copy",
      role: "Audit iOS keyboard extension",
      taskTitle: "Audit iOS keyboard extension",
      status: "running",
      startedAt: now - 1000 * 27,
      tokensIn: 96200,
      tokensOut: 6000,
      toolUses: 7,
      elapsedMs: 27000,
      lastTools: ["read_file", "apply_patch"],
      summary:
        "Stopped after confirming the current labels are direct enough for the default lane.",
    },
  ];
  const qaMessages: TranscriptMessage[] = [
    {
      $kind: "user",
      id: "qa-msg-user",
      createdAt,
      content:
        "can you see if my ios app is in the proper shape? the dynamic island, being started and stopped in bg with action or backtap or keyboard? it's possible and setup right? and the dynamic island properly shows it's status? recording like in screen i.e. whatsapp but our mac pill has same time, a nice blue rim like mac pll would be perfect. keyboard has basics like delete etc and some symbols, single record or paste button, record should recording? start the recording, and show the time, etcetera, whatever. Or... sorry. Scratch that. Yeah. Time, it should show the time and the movement pill inside the keyboard. When the keyboard is open, I think the dynamic island should not be visible. You tell me what the best UX is. But in any other case, if action button or back tab... we need proper dynamic island representation. Even our competitor, WhisperFlo, can't get it right. But if WhatsApp can, when I'm on a call and I go outside and the WhatsApp call is",
      pinned: false,
    },
    {
      $kind: "assistant",
      id: "qa-msg-assistant",
      createdAt: new Date(now - 1000 * 50).toISOString(),
      content:
        "I'll first scope the iOS app in the repo, then fan out review agents.\n\nAuditing iOS Live Activity/Dynamic Island >\n\nAll three review agents are running in parallel (Dynamic Island, background triggers, keyboard). I'll compile the verdict when they report back.\n\nUsing TaskOutput >",
      reasoningContent: undefined,
      toolCalls: [],
      pinned: false,
    },
  ];

  const openAgentSession = (id: string) => {
    args.setSelectedId(id);
    args.setPane("session");
    args.setStatus(`Opened full transcript: ${id}`);
  };

  return {
    ...surfaces,
    headerProps: {
      ...surfaces.headerProps,
      headerTitle: qaSession.title,
      headerSubtitle: "heardright",
      selected: { id: qaSession.id },
      messagesLength: qaMessages.length,
      isWorking: true,
    },
    sidebarProps: {
      ...surfaces.sidebarProps,
      sessions: [
        {
          ...qaSession,
          title: "iOS app Dynamic Island implementati",
          updatedAt: new Date(now).toISOString(),
        },
        ...surfaces.sidebarProps.sessions,
      ],
      selectedId: qaSession.id,
      workspaceRoot: qaSession.workspaceRoot,
      connectionLabel: "Preview mode",
    },
    workbenchProps: {
      ...surfaces.workbenchProps,
      pane: "session",
      selected: qaSession,
      messages: qaMessages,
      // QA scenario primes the streaming store directly so the transcript
      // subtree picks it up via its narrow selectors.
      activeGoal: qaGoal,
      queuedMessages: [],
      workspaceRoot: qaSession.workspaceRoot,
    },
    showOnboardingPanel: false,
    railProps: {
      agents: qaAgents,
      onCancel: (id) => args.setStatus(`Stop requested for ${id}`),
      onCancelAll: () => args.setStatus("Stop requested for 1 background agent"),
      onOpenAgentSession: openAgentSession,
      sessionTitle: qaSession.title,
      goalStatus: qaGoal.status,
      costUsd: 1.2847,
    },
    composerProps: {
      ...surfaces.composerProps,
      disabled: false,
      isWorking: true,
      queueLength: 1,
      activeGoal: qaGoal,
      commands: [
        {
          name: "goal",
          usage: "/goal [objective]",
          description: "Start or update a persistent goal",
        },
        { name: "skills", usage: "/skills", description: "Browse available skills" },
        { name: "mcp", usage: "/mcp", description: "Inspect connected tools" },
        { name: "status", usage: "/status", description: "Show session and agent status" },
      ],
      workspaceRoot: qaSession.workspaceRoot,
      workspaces: [qaSession.workspaceRoot, ...surfaces.composerProps.workspaces],
      primaryModelLabel: "MiniMax M3",
      modelOptions: [
        {
          id: "minimax/minimax-m3",
          label: "MiniMax M3",
          aliases: ["minimax/minimax-m3", "minimax-m3"],
          supportsThinkingLevels: false,
          supportsThinkingBudgetTokens: false,
        },
      ],
      gitBranch: "main",
      gitBranches: ["main"],
    },
  };
}

function withQaNewChatScenario(surfaces: UseAppSurfaces, _args: AppSurfaceInputs): UseAppSurfaces {
  const now = Date.now();
  const workspaceRoot = "/workspace/coderight";
  const qaSession: SessionRow = {
    id: "qa-new-chat",
    title: "New session",
    model: "minimax/minimax-m3",
    workspaceRoot,
    createdAt: new Date(now - 1000 * 60 * 5).toISOString(),
    updatedAt: new Date(now).toISOString(),
    status: "idle",
    role: "session",
    toolset: null,
    activeSkills: [],
    executionBackend: "local",
    workspaceTrust: "trusted",
    effort: "high",
  };

  return {
    ...surfaces,
    headerProps: {
      ...surfaces.headerProps,
      headerTitle: hostAppName(),
      headerSubtitle: "coderight",
      selected: { id: qaSession.id },
      messagesLength: 0,
      isWorking: false,
    },
    sidebarProps: {
      ...surfaces.sidebarProps,
      sessions: [
        { ...qaSession, title: "Config.toml validation and setup" },
        { ...qaSession, id: "qa-recent-2", title: "Code/docs audit and parity plan" },
        { ...qaSession, id: "qa-recent-3", title: "vast" },
        { ...qaSession, id: "qa-recent-4", title: "US LLC Founder Setup Kit" },
        { ...qaSession, id: "qa-recent-5", title: "Law" },
      ],
      selectedId: null,
      workspaceRoot,
      connectionLabel: "Preview mode",
    },
    workbenchProps: {
      ...surfaces.workbenchProps,
      pane: "session",
      selected: qaSession,
      messages: [],
      activeGoal: null,
      queuedMessages: [],
      workspaceRoot,
    },
    showOnboardingPanel: false,
    railProps: null,
    composerProps: {
      ...surfaces.composerProps,
      disabled: false,
      isWorking: false,
      queueLength: 0,
      workspaceRoot,
      workspaces: [workspaceRoot],
      primaryModelLabel: "MiniMax M3",
      modelOptions: [
        {
          id: "minimax/minimax-m3",
          label: "MiniMax M3",
          aliases: ["minimax/minimax-m3", "minimax-m3"],
          supportsThinkingLevels: false,
          supportsThinkingBudgetTokens: false,
        },
        {
          id: "z-ai/glm-5.2",
          label: "GLM 5.2",
          aliases: ["z-ai/glm-5.2", "glm-5.2"],
          supportsThinkingLevels: false,
          supportsThinkingBudgetTokens: false,
        },
      ],
      gitBranch: "main",
      gitBranches: ["main"],
    },
  };
}
