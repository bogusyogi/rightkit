import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useDismiss } from "@rightkit/platform-ui/headless";
import { useEffect } from "react";
import { registerShortcut } from "@rightkit/agent-ui-core/lib/shortcuts/dispatcher";
import {
  getGitBranches,
  getSystemUsername,
  isTauriHost,
  type PermissionMode,
} from "@rightkit/agent-ui-core/lib/daemon";
import type { PendingApproval } from "@rightkit/agent-ui-hooks/hooks/useApprovals";
import type { SettingsTab } from "../../components/settings/types.js";
import type { RuntimeTab } from "@rightkit/agent-ui-workbench/components/RuntimePane";
import type { Pane } from "../../components/WorkbenchContent.js";
import type {
  AwaySummary,
  DaemonConfig,
  ExecutionProcessLogs,
  HealthResponse,
} from "@rightkit/agent-ui-core/lib/types";
import { usePermissionsStore } from "../../state/permissionsStore.js";

export function useAppEffects(params: {
  config: DaemonConfig | null;
  health: HealthResponse | null;
  pane: Pane;
  settingsTab: SettingsTab;
  workspaceRoot: string;
  selectedSessionId: string | null;
  messagesLength: number;
  openRecipeDeeplink: (url: string) => Promise<void>;
  enqueueRecipeDeeplinks: (urls: string[]) => void;
  recipeDeeplinkQueue: React.MutableRefObject<string[]>;
  recipeDeeplinkProcessing: React.MutableRefObject<boolean>;
  setRecipeDeeplinkSignal: React.Dispatch<React.SetStateAction<number>>;
  setPane: (pane: Pane) => void;
  setRuntimeTab: (tab: RuntimeTab) => void;
  loadSettings: () => Promise<void>;
  loadPlugins: () => Promise<void>;
  loadPluginMarketplace: () => Promise<void>;
  loadMcp: () => Promise<void>;
  loadAllPermissions: () => Promise<void>;
  loadLsp: () => Promise<void>;
  loadSkills: () => Promise<void>;
  loadDiagnostics: () => Promise<void>;
  loadLocalModels: () => Promise<void>;
  loadExecutionBackends: () => Promise<void>;
  loadTelegramStatus: () => Promise<void>;
  loadTasks: () => Promise<void>;
  loadTerminalProcesses: () => Promise<void>;
  loadRuntime: () => Promise<void>;
  setAwaySummary: React.Dispatch<React.SetStateAction<AwaySummary | null>>;
  setAwaySummaryCursor: (value: number) => void;
  setTerminalInput: (value: string) => void;
  setTerminalLogs: React.Dispatch<React.SetStateAction<ExecutionProcessLogs | null>>;
  setSelectedTerminalProcessId: React.Dispatch<React.SetStateAction<string | null>>;
  selectedTerminalProcessId: string | null;
  loadTerminalLogs: (processId: string) => Promise<void>;
  setSystemUser: (value: string | null) => void;
  setGitInfo: (value: { current: string; branches: string[] } | null) => void;
  resetSubAgents: () => void;
  clearQueuedMessages: () => void;
  resetGoal: () => void;
  setPermissionModeState: (value: PermissionMode) => void;
  setSessionCostUsd: (value: number) => void;
  setContextPercent: (value: number) => void;
  setUsagePromptTokens: (value: number) => void;
  setUsageCompletionTokens: (value: number) => void;
  setUsageContextTokens: (value: number) => void;
  setCacheReadTokens: (value: number) => void;
  setUsageCacheCreationTokens: (value: number) => void;
  isWorking: boolean;
  pendingApproval: PendingApproval | null;
  showRunMenu: boolean;
  showWorkspaceMenu: boolean;
  showBranchMenu: boolean;
  setPaletteOpen: React.Dispatch<React.SetStateAction<boolean>>;
  setShowRunMenu: (value: boolean) => void;
  setShowWorkspaceMenu: (value: boolean) => void;
  setShowBranchMenu: (value: boolean) => void;
  interrupt: () => Promise<void>;
  setStatus: (value: string) => void;
  messageOf: (err: unknown, fallback: string) => string;
}) {
  const {
    config,
    health,
    pane,
    settingsTab,
    workspaceRoot,
    selectedSessionId,
    messagesLength,
    openRecipeDeeplink,
    enqueueRecipeDeeplinks,
    recipeDeeplinkQueue,
    recipeDeeplinkProcessing,
    setRecipeDeeplinkSignal,
    setPane,
    setRuntimeTab,
    loadSettings,
    loadPlugins,
    loadPluginMarketplace,
    loadMcp,
    loadAllPermissions,
    loadLsp,
    loadSkills,
    loadDiagnostics,
    loadLocalModels,
    loadExecutionBackends,
    loadTelegramStatus,
    loadTasks,
    loadTerminalProcesses,
    loadRuntime,
    setAwaySummary,
    setAwaySummaryCursor,
    setTerminalInput,
    setTerminalLogs,
    setSelectedTerminalProcessId,
    selectedTerminalProcessId,
    loadTerminalLogs,
    setSystemUser,
    setGitInfo,
    resetSubAgents,
    clearQueuedMessages,
    resetGoal,
    setPermissionModeState,
    setSessionCostUsd,
    setContextPercent,
    setUsagePromptTokens,
    setUsageCompletionTokens,
    setUsageContextTokens,
    setCacheReadTokens,
    setUsageCacheCreationTokens,
    isWorking,
    pendingApproval,
    showRunMenu,
    showWorkspaceMenu,
    showBranchMenu,
    setPaletteOpen,
    setShowRunMenu,
    setShowWorkspaceMenu,
    setShowBranchMenu,
    interrupt,
    setStatus,
    messageOf,
  } = params;

  useEffect(() => {
    if (!isTauriHost()) return;
    let disposed = false;
    let cleanup: (() => void) | null = null;
    void invoke<string[]>("opened_urls")
      .then((urls) => {
        if (disposed) return;
        enqueueRecipeDeeplinks(Array.isArray(urls) ? urls : []);
      })
      .catch((err) => {
        if (disposed) return;
        setStatus(messageOf(err, "Recipe deeplink startup failed"));
      });
    void listen<string[]>("coderight-opened-urls", (event) => {
      if (disposed) return;
      enqueueRecipeDeeplinks(Array.isArray(event.payload) ? event.payload : []);
    }).then((unlisten) => {
      if (disposed) {
        unlisten();
        return;
      }
      cleanup = unlisten;
    });
    return () => {
      disposed = true;
      cleanup?.();
    };
  }, [enqueueRecipeDeeplinks, messageOf, setStatus]);

  useEffect(() => {
    if (!config || !health || recipeDeeplinkProcessing.current) return;
    if (recipeDeeplinkQueue.current.length === 0) return;
    let cancelled = false;
    recipeDeeplinkProcessing.current = true;
    void (async () => {
      while (!cancelled) {
        const url = recipeDeeplinkQueue.current.shift();
        if (!url) break;
        setPane("runtime");
        setRuntimeTab("recipes");
        await openRecipeDeeplink(url);
      }
    })().finally(() => {
      recipeDeeplinkProcessing.current = false;
      if (!cancelled && recipeDeeplinkQueue.current.length > 0) {
        setRecipeDeeplinkSignal((value) => value + 1);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [
    config,
    health,
    openRecipeDeeplink,
    recipeDeeplinkProcessing,
    recipeDeeplinkQueue,
    setPane,
    setRecipeDeeplinkSignal,
    setRuntimeTab,
  ]);

  useEffect(() => {
    if (!config || !health) return;
    if (pane === "tasks") void loadTasks();
    if (pane === "terminal") void loadTerminalProcesses();
    if (pane === "runtime") void loadRuntime();
  }, [config, health, loadRuntime, loadTasks, loadTerminalProcesses, pane]);

  // Settings modal data: hydrate when the active tab changes. Settings is now
  // a modal, not a pane, so we hook on `settingsTab` directly.
  useEffect(() => {
    if (!config || !health) return;
    void loadSettings();
    if (settingsTab === "plugins") {
      void loadPlugins();
      void loadPluginMarketplace();
    }
    if (settingsTab === "mcp") void loadMcp();
    if (settingsTab === "lsp") void loadLsp();
    if (settingsTab === "skills") void loadSkills();
    if (settingsTab === "permissions") void loadAllPermissions();
    if (settingsTab === "diagnostics") void loadDiagnostics();
    if (settingsTab === "models") void loadLocalModels();
    if (settingsTab === "execution") {
      void loadExecutionBackends();
      void loadDiagnostics();
    }
    if (settingsTab === "remote") {
      void loadTelegramStatus();
    }
  }, [
    config,
    health,
    settingsTab,
    loadSettings,
    loadPlugins,
    loadPluginMarketplace,
    loadMcp,
    loadLsp,
    loadSkills,
    loadAllPermissions,
    loadDiagnostics,
    loadLocalModels,
    loadExecutionBackends,
    loadTelegramStatus,
  ]);

  useEffect(() => {
    setAwaySummary(null);
    setAwaySummaryCursor(messagesLength);
  }, [messagesLength, setAwaySummary, setAwaySummaryCursor]);

  useEffect(() => {
    setTerminalInput("");
    setTerminalLogs(null);
    setSelectedTerminalProcessId(null);
  }, [setSelectedTerminalProcessId, setTerminalInput, setTerminalLogs]);

  useEffect(() => {
    if (pane !== "terminal" || !config) return;
    void loadTerminalProcesses();
    const interval = window.setInterval(() => {
      void loadTerminalProcesses();
    }, 1500);
    return () => window.clearInterval(interval);
  }, [config?.url, loadTerminalProcesses, pane, config]);

  useEffect(() => {
    if (pane !== "terminal" || !config || !selectedTerminalProcessId) return;
    void loadTerminalLogs(selectedTerminalProcessId);
    const interval = window.setInterval(() => {
      void loadTerminalLogs(selectedTerminalProcessId);
    }, 1000);
    return () => window.clearInterval(interval);
  }, [config?.url, loadTerminalLogs, pane, selectedTerminalProcessId, config]);

  useEffect(() => {
    if (config && health && workspaceRoot.trim()) {
      void loadSettings();
      void usePermissionsStore.getState().loadWorkspaceTrust(config, workspaceRoot, setStatus);
    } else {
      usePermissionsStore.getState().resetWorkspaceTrust();
    }
  }, [config?.url, health?.ok, loadSettings, setStatus, workspaceRoot, config, health]);

  useEffect(() => {
    if (isTauriHost()) {
      void getSystemUsername()
        .then(setSystemUser)
        .catch(() => {});
    }
  }, [setSystemUser]);

  useEffect(() => {
    if (!workspaceRoot.trim() || !isTauriHost()) {
      setGitInfo(null);
      return;
    }
    void getGitBranches(workspaceRoot)
      .then((info) => setGitInfo(info.branches.length ? info : null))
      .catch(() => setGitInfo(null));
  }, [setGitInfo, workspaceRoot]);

  useEffect(() => {
    // Reset transient state whenever the selected session changes.
    void selectedSessionId;
    setSessionCostUsd(0);
    setContextPercent(0);
    setUsagePromptTokens(0);
    setUsageCompletionTokens(0);
    setUsageContextTokens(0);
    setCacheReadTokens(0);
    setUsageCacheCreationTokens(0);
    resetSubAgents();
    clearQueuedMessages();
    resetGoal();
    setPermissionModeState("ask");
  }, [
    clearQueuedMessages,
    resetGoal,
    resetSubAgents,
    selectedSessionId,
    setCacheReadTokens,
    setContextPercent,
    setPermissionModeState,
    setSessionCostUsd,
    setUsageCacheCreationTokens,
    setUsageCompletionTokens,
    setUsageContextTokens,
    setUsagePromptTokens,
  ]);

  const anyMenuOpen = showRunMenu || showWorkspaceMenu || showBranchMenu;
  const closeMenus = () => {
    setShowRunMenu(false);
    setShowWorkspaceMenu(false);
    setShowBranchMenu(false);
  };

  // Escape closes the run / workspace / branch menus (RightKit useDismiss; it
  // stops the key so the interrupt below does not also fire). A press inside any
  // `[data-composer-menu]` zone is not outside.
  useDismiss(closeMenus, {
    enabled: anyMenuOpen,
    outside: "mousedown",
    isInside: (target) => (target instanceof Element ? target : target.parentElement)?.closest("[data-composer-menu]") != null,
  });

  useEffect(
    () =>
      registerShortcut({
        id: "app.toggle_palette_alt",
        // Old matching: Ctrl or Meta (either, or both), no Shift, no Alt; no composing or defaultPrevented
        // guard. The run guard restores the Shift/Alt exclusion (reported unhandled, no preventDefault).
        chord: "ctrlormeta+p",
        label: "Toggle command palette",
        hidden: true,
        ignoreExtraModifiers: true,
        allowComposing: true,
        allowDefaultPrevented: true,
        run: (event) => {
          if (event.shiftKey || event.altKey) return false;
          setPaletteOpen((value) => !value);
          return true;
        },
      }),
    [setPaletteOpen],
  );

  // Escape interrupts a running turn. It stays a window listener on purpose: Escape has several owners
  // (menus via `useDismiss` above, approval and question cards, the tool rail, find) that all react to
  // the same press, and the shortcut registry runs only the first matching binding.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape" && isWorking && !pendingApproval) {
        e.preventDefault();
        void interrupt();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [interrupt, isWorking, pendingApproval]);
}
