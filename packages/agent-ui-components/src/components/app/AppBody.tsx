import {
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { BackgroundTasksSurfaceProps as AgentRailProps } from "@rightkit/agent-ui-core/lib/appSurfaceProps/backgroundTasksTypes";
import BackgroundTasksPanel from "../BackgroundTasksPanel.js";
import ConnectionSwitcher from "../ConnectionSwitcher.js";
import ProjectPicker from "../ProjectPicker.js";
import PersistentTerminal, { TerminalSlot } from "@rightkit/terminal-ui/components/PersistentTerminal";
import FilesPanel from "../panels/FilesPanel.js";
import KebabMenu, { type KebabAction } from "../panels/KebabMenu.js";
import ArtifactRowsPanel from "@rightkit/agent-ui-workbench/components/runtime/ArtifactRowsPanel";
import WorkspaceFileViewer from "@rightkit/agent-ui-workbench/components/runtime/WorkspaceFileViewer";
import ReviewPane from "../review/ReviewPane.js";
import { BrowserPane } from "../browser/BrowserPane.js";
import { BotsPane } from "../bots/BotsPane.js";
import DeliveryPanel from "../delivery/DeliveryPanel.js";
import EnvironmentsPanel from "../environments/EnvironmentsPanel.js";
import { ExecutionGraphPane } from "../execution/ExecutionGraphPane.js";
import AttentionPanel from "../attention/AttentionPanel.js";
import Icon, { type IconName } from "@rightkit/agent-ui-core/components/Icon";
import PaneErrorBoundary from "@rightkit/agent-ui-core/components/PaneErrorBoundary";
import Sidebar from "../Sidebar.js";
import {
  ConfirmDialog,
  ContextMenu,
  type MenuItem,
  PromptDialog,
  ShortcutHelp,
} from "@rightkit/react-shell/react";
import WorkbenchContent from "../WorkbenchContent.js";
import DriverLifecycleStrip from "@rightkit/agent-ui-workbench/components/workbench/DriverLifecycleStrip";
import ComposerIsland from "./ComposerIsland.js";
import { WorkspaceToolRail } from "./WorkspaceToolRail.js";
import TitleBarSlotPortal from "./TitleBarSlotPortal.js";
import OnboardingPanel from "./OnboardingPanel.js";
import WorkspaceTrustGate from "@rightkit/agent-ui-core/components/app/WorkspaceTrustGate";
import AppShellHeader from "@rightkit/agent-ui-core/components/app/AppShellHeader";
import ExtensionSlot from "@rightkit/agent-ui-core/extensions/ExtensionSlot";
import SearchPanel from "../panels/SearchPanel.js";
import type { AppShellHeaderProps } from "@rightkit/agent-ui-core/components/app/types";
import type { ProviderKeyStatus } from "@rightkit/agent-ui-core/lib/daemon";
import type { HealthResponse } from "@rightkit/agent-ui-core/lib/types";
import type { KeyFormatCheck } from "@rightkit/agent-ui-core/lib/providerPresets";
import { useSidePanelStore } from "@rightkit/agent-ui-core/state/sidePanelStore";
import { useRightPanelStore, type RightPanelTab } from "@rightkit/agent-ui-core/state/rightPanelStore";
import { projectLayoutOrDefault, useLayoutStore, type DockSurface } from "@rightkit/agent-ui-core/state/layoutStore";
import { useWorkspaceStore } from "@rightkit/agent-ui-core/state/workspaceStore";
import {
  cancelPendingShortcut,
  registerShortcut,
  rightKitShortcuts,
  syncShortcutBindings,
} from "@rightkit/agent-ui-core/lib/shortcuts/dispatcher";
import {
  readShortcutOverrides,
  SHORTCUT_OVERRIDES_CHANGED_EVENT,
  SHORTCUT_OVERRIDES_STORAGE_KEY,
} from "@rightkit/agent-ui-settings/components/settings/keyboard/KeyboardShortcutsSettings";
import { useAccounts } from "@rightkit/agent-ui-hooks/hooks/useAccounts";
import { useRateLimitStore } from "@rightkit/agent-ui-core/state/rateLimitStore";
import RateLimitBanner from "./RateLimitBanner.js";
import {
  AccountSwitchNotice,
  SessionAuthMethodLine,
  SubscriptionUsageNotice,
} from "./AccountNotices.js";
import { hostAppName } from "../../appName.js";

const SIDEBAR_DEFAULT_WIDTH = 280;
const SIDEBAR_MIN_WIDTH = 220;
const SIDEBAR_MAX_WIDTH = 420;
const SIDEBAR_COLLAPSED_WIDTH = 0;
const RIGHT_DOCK_MIN_WIDTH = 320;
const RIGHT_DOCK_MAX_WIDTH = 760;
const TERMINAL_DOCK_MIN_HEIGHT = 180;
const TERMINAL_DOCK_MAX_HEIGHT = 520;

function clampSidebarWidth(width: number) {
  return Math.min(SIDEBAR_MAX_WIDTH, Math.max(SIDEBAR_MIN_WIDTH, width));
}

interface AppBodyProps {
  status: string;
  providerKeyStatuses: ProviderKeyStatus[];
  providerKeyDrafts: Record<string, string>;
  providerKeySaving: string | null;
  providerKeyValidation: Record<string, KeyFormatCheck>;
  validateProviderKeyDraft: (provider: string, value: string) => void;
  setProviderKeyDrafts: (
    updater: (drafts: Record<string, string>) => Record<string, string>,
  ) => void;
  headerProps: AppShellHeaderProps;
  health: HealthResponse | null;
  sidebarProps: Parameters<typeof Sidebar>[0];
  trustGateProps: Parameters<typeof WorkspaceTrustGate>[0] | null;
  showOnboardingPanel: boolean;
  onboardingPanelProps: Parameters<typeof OnboardingPanel>[0];
  workbenchProps: Parameters<typeof WorkbenchContent>[0];
  sessionActions: {
    renameSession: (id: string, title: string) => void;
    archiveSession: (id: string, title: string) => void;
    forkSelectedSession: () => void;
    deleteSelectedSession: (id: string) => void;
    continueOnAccount: (accountId: string, cappedLabel: string) => void;
  };
  railProps: AgentRailProps | null;
  composerProps: Parameters<typeof ComposerIsland>[0];
  sidebarCollapsed: boolean;
  onToggleSidebar: () => void;
  onOpenSettings: () => void;
  onOpenPane: (pane: Parameters<typeof WorkbenchContent>[0]["pane"]) => void;
  onRetryEngine: () => void;
  onCancelSubAgent: (id: string) => void;
}

export default function AppBody({
  status,
  headerProps,
  health,
  sidebarProps,
  trustGateProps,
  showOnboardingPanel,
  onboardingPanelProps,
  workbenchProps,
  sessionActions,
  railProps,
  composerProps,
  sidebarCollapsed,
  onToggleSidebar,
  onOpenSettings,
  onOpenPane,
  onRetryEngine,
  onCancelSubAgent,
}: AppBodyProps) {
  // Agents is a stable title-bar destination, including before the first
  // sub-agent starts. Gating it on railProps made "0 Agents" a dead button.
  const hasAgentRail = true;
  const closeSidePanel = useSidePanelStore((s) => s.setOpen);
  const rightPanelTab = useRightPanelStore((s) => s.tab);
  const setRightPanelTab = useRightPanelStore((s) => s.setTab);
  const currentProjectId = workbenchProps.workspaceRoot.trim() || "default";
  const accountProject = workbenchProps.workspaceRoot.trim() || null;
  const accountState = useAccounts(
    workbenchProps.config,
    workbenchProps.selected?.id ?? null,
    accountProject,
  );
  const sessionRateLimit = useRateLimitStore((s) =>
    workbenchProps.selected?.id ? (s.bySession[workbenchProps.selected.id] ?? null) : null,
  );
  const projectLayout = useLayoutStore((s) =>
    projectLayoutOrDefault(s.byProject, currentProjectId),
  );
  const visibleSurfaces = projectLayout.visible;
  // Layout visibility is per-project. The legacy side-panel store is global,
  // so it must not resurrect the previous project's dock when this project has
  // no visible utility surface.
  const rightPanelSurfaces = visibleSurfaces.filter(
    (surface) => surface !== "terminal" && surface !== "diagnostics",
  );
  const preferredRightSurface = rightPanelSurface(projectLayout.rightTab ?? rightPanelTab);
  const activeRightSurface = rightPanelSurfaces.includes(preferredRightSurface)
    ? preferredRightSurface
    : rightPanelSurfaces.at(-1);
  const layoutSetVisible = useLayoutStore((s) => s.setVisible);
  const toggleRightPanel = useLayoutStore((s) => s.toggleRightPanel);
  const layoutSetRightTab = useLayoutStore((s) => s.setRightTab);
  const layoutSetSize = useLayoutStore((s) => s.setSize);
  const layoutSetMaximized = useLayoutStore((s) => s.setMaximized);
  const maximizedPane = projectLayout.maximized;
  // The rendered tab must always correspond to a visible surface. This keeps
  // a restored layout with a stale tab usable instead of showing the wrong
  // panel (or a blank dock).
  const effectiveRightPanelTab = activeRightSurface
    ? rightPanelTabForSurface(activeRightSurface)
    : projectLayout.rightTab ?? rightPanelTab;
  const showRightPanel = rightPanelSurfaces.length > 0 && maximizedPane !== "terminal";
  const terminalDockOpen = visibleSurfaces.includes("terminal") && maximizedPane !== "right";
  const sidebarWidth = clampSidebarWidth(projectLayout.sizes.sidebar ?? SIDEBAR_DEFAULT_WIDTH);
  const rightDockWidth = Math.min(
    RIGHT_DOCK_MAX_WIDTH,
    Math.max(RIGHT_DOCK_MIN_WIDTH, projectLayout.sizes.right ?? 380),
  );
  const terminalDockHeight = Math.min(
    TERMINAL_DOCK_MAX_HEIGHT,
    Math.max(TERMINAL_DOCK_MIN_HEIGHT, projectLayout.sizes.terminal ?? 240),
  );
  const workspaceFilePath = useWorkspaceStore((state) => state.openedFilePath);
  const setWorkspaceFilePath = useWorkspaceStore((state) => state.openFile);
  const openSurface = useCallback(
    (surface: DockSurface, tab: RightPanelTab) => {
      const state = useLayoutStore.getState();
      const current = projectLayoutOrDefault(state.byProject, currentProjectId);
      const visible = current.visible;
      layoutSetVisible(
        currentProjectId,
        visible.includes(surface) ? visible : [...visible, surface],
      );
      layoutSetRightTab(currentProjectId, tab);
      setRightPanelTab(tab);
      closeSidePanel(true);
    },
    [closeSidePanel, currentProjectId, layoutSetRightTab, layoutSetVisible, setRightPanelTab],
  );
  const openReview = useCallback(() => openSurface("review", "review"), [openSurface]);
  const openTerminalDock = useCallback(() => {
    const state = useLayoutStore.getState();
    const current = projectLayoutOrDefault(state.byProject, currentProjectId);
    const visible = current.visible;
    layoutSetVisible(
      currentProjectId,
      visible.includes("terminal") ? visible : [...visible, "terminal"],
    );
  }, [currentProjectId, layoutSetVisible]);
  const closeTerminalDock = useCallback(() => {
    const visible = useLayoutStore.getState().byProject[currentProjectId]?.visible ?? [];
    layoutSetVisible(
      currentProjectId,
      visible.filter((surface) => surface !== "terminal"),
    );
    layoutSetMaximized(currentProjectId, null);
  }, [currentProjectId, layoutSetMaximized, layoutSetVisible]);
  const toggleTerminalDock = useCallback(() => {
    if (terminalDockOpen) closeTerminalDock();
    else openTerminalDock();
  }, [closeTerminalDock, openTerminalDock, terminalDockOpen]);
  const closeRightPanel = useCallback(() => {
    const surface = rightPanelSurface(effectiveRightPanelTab);
    const state = useLayoutStore.getState();
    const visible = projectLayoutOrDefault(state.byProject, currentProjectId).visible;
    const remaining = visible.filter((candidate) => candidate !== surface);
    layoutSetVisible(currentProjectId, remaining);
    layoutSetMaximized(currentProjectId, null);
    const remainingRight = [...remaining]
      .reverse()
      .find((candidate) => candidate !== "terminal" && candidate !== "diagnostics");
    if (remainingRight) {
      const tab = rightPanelTabForSurface(remainingRight);
      layoutSetRightTab(currentProjectId, tab);
      setRightPanelTab(tab);
      closeSidePanel(true);
    } else {
      closeSidePanel(false);
    }
  }, [
    closeSidePanel,
    currentProjectId,
    effectiveRightPanelTab,
    layoutSetMaximized,
    layoutSetRightTab,
    layoutSetVisible,
    setRightPanelTab,
  ]);
  const toggleUtilityPanel = useCallback(() => {
    if (showRightPanel) {
      closeRightPanel();
      return;
    }
    openSurface(rightPanelSurface(effectiveRightPanelTab), effectiveRightPanelTab);
  }, [closeRightPanel, effectiveRightPanelTab, openSurface, showRightPanel]);
  const openDiagnostics = useCallback(() => {
    onOpenPane("runtime");
    workbenchProps.onRuntimeTab("observability");
  }, [onOpenPane, workbenchProps]);
  const runningAgentCount = useMemo(
    () => railProps?.agents.filter((a) => a.status === "running").length ?? 0,
    [railProps],
  );
  const agentsOpen = showRightPanel && effectiveRightPanelTab === "background-tasks";
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  const [searchOpen, setSearchOpen] = useState(
    () => new URLSearchParams(window.location.search).get("search") === "1",
  );
  const [renameTarget, setRenameTarget] = useState<{ id: string; title: string } | null>(null);
  const [sessionConfirm, setSessionConfirm] = useState<{
    action: "archive" | "delete";
    id: string;
    title: string;
  } | null>(null);
  const composerRef = useRef<HTMLTextAreaElement | null>(null);
  // The quiet new-chat surface belongs to the conversation pane only. Without
  // the pane check, opening Terminal/Tasks/Runtime/Memory on a session that has
  // no messages yet renders the empty greeting instead of the pane the header
  // already names — the surface reads as permanently blank.
  const showNewChatState =
    workbenchProps.pane === "session" &&
    workbenchProps.messages.length === 0 &&
    !headerProps.isWorking;

  const toggleAgents = useCallback(() => {
    if (showRightPanel && effectiveRightPanelTab === "background-tasks") {
      closeRightPanel();
      return;
    }
    openSurface("agents", "background-tasks");
  }, [closeRightPanel, effectiveRightPanelTab, openSurface, showRightPanel]);

  const focusComposer = useCallback(() => {
    composerRef.current?.focus();
  }, []);

  const handleKebabAction = useCallback(
    (action: KebabAction) => {
      switch (action) {
        case "open-files":
          openSurface("files", "files");
          break;
        case "open-background-tasks":
          openSurface("agents", "background-tasks");
          break;
        case "open-artifacts":
          openSurface("artifacts", "artifacts");
          break;
        case "open-review":
          openReview();
          break;
        case "open-browser":
          openSurface("browser", "browser");
          break;
        case "open-delivery":
          openSurface("delivery", "delivery");
          break;
        case "open-environments":
          openSurface("environments", "environments");
          break;
        case "open-attention":
          openSurface("attention", "attention");
          break;
        case "open-diagnostics":
          openDiagnostics();
          break;
        case "open-in-terminal":
          workbenchProps.onCreateTerminalProcess();
          openTerminalDock();
          break;
        case "open-settings":
          onOpenSettings();
          break;
        case "rename":
          if (workbenchProps.selected) {
            setRenameTarget({
              id: workbenchProps.selected.id,
              title: workbenchProps.selected.title,
            });
          }
          break;
        case "fork":
          sessionActions.forkSelectedSession();
          break;
        case "archive":
          if (workbenchProps.selected) {
            setSessionConfirm({
              action: "archive",
              id: workbenchProps.selected.id,
              title: workbenchProps.selected.title,
            });
          }
          break;
        case "delete":
          if (workbenchProps.selected) {
            setSessionConfirm({
              action: "delete",
              id: workbenchProps.selected.id,
              title: workbenchProps.selected.title,
            });
          }
          break;
        default:
          break;
      }
    },
    [
      onOpenSettings,
      openDiagnostics,
      openReview,
      openSurface,
      openTerminalDock,
      sessionActions,
      workbenchProps,
    ],
  );

  useEffect(() => {
    const unregister = [
      registerShortcut({ id: "toggle_sidebar", label: "Toggle sidebar", run: () => { onToggleSidebar(); return true; } }),
      registerShortcut({ id: "focus_input", label: "Focus composer", run: () => { focusComposer(); return true; } }),
      registerShortcut({ id: "shell.utility_panel", chord: "mod+.", label: "Toggle utility panel", run: () => { toggleUtilityPanel(); return true; } }),
      registerShortcut({ id: "shell.open_review", chord: "mod+shift+r", label: "Open Review", run: () => { openReview(); return true; } }),
      registerShortcut({ id: "shell.open_diagnostics", chord: "mod+shift+d", label: "Open diagnostics", run: () => { openDiagnostics(); return true; } }),
      registerShortcut({ id: "toggle_terminal", label: "Open terminal dock", run: () => { openTerminalDock(); return true; } }),
      registerShortcut({ id: "shell.shortcuts_help", chord: "?", label: "Keyboard shortcuts", run: () => { setShortcutsOpen(true); return true; } }),
      registerShortcut({ id: "open_settings", label: "Open settings", hidden: true, run: () => { onOpenSettings(); return true; } }),
      registerShortcut({ id: "open_help", label: "Open help", hidden: true, run: () => { setShortcutsOpen(true); return true; } }),
    ];

    // The registry owns the keydown listener; repeats are decided per binding (see `registerShortcut`).
    const detach = rightKitShortcuts.attach();
    const syncOverrides = () => syncShortcutBindings(readShortcutOverrides());
    syncOverrides();
    const onStorage = (event: StorageEvent) => {
      if (event.key === null || event.key === SHORTCUT_OVERRIDES_STORAGE_KEY) syncOverrides();
    };
    const onBlur = () => cancelPendingShortcut();
    window.addEventListener(SHORTCUT_OVERRIDES_CHANGED_EVENT, syncOverrides);
    window.addEventListener("storage", onStorage);
    window.addEventListener("blur", onBlur);
    return () => {
      detach();
      window.removeEventListener(SHORTCUT_OVERRIDES_CHANGED_EVENT, syncOverrides);
      window.removeEventListener("storage", onStorage);
      window.removeEventListener("blur", onBlur);
      cancelPendingShortcut();
      unregister.forEach((remove) => {
        remove();
      });
    };
  }, [
    focusComposer,
    onOpenSettings,
    onToggleSidebar,
    openDiagnostics,
    openReview,
    openTerminalDock,
    toggleUtilityPanel,
  ]);

  const setAndPersistSidebarWidth = (width: number) => {
    layoutSetSize(currentProjectId, "sidebar", clampSidebarWidth(width));
  };

  const resizeSidebarBy = (delta: number) => {
    setAndPersistSidebarWidth(sidebarWidth + delta);
  };

  const resizeRightDockBy = (delta: number) => {
    layoutSetSize(
      currentProjectId,
      "right",
      Math.min(RIGHT_DOCK_MAX_WIDTH, Math.max(RIGHT_DOCK_MIN_WIDTH, rightDockWidth + delta)),
    );
  };

  const resizeTerminalDockBy = (delta: number) => {
    layoutSetSize(
      currentProjectId,
      "terminal",
      Math.min(
        TERMINAL_DOCK_MAX_HEIGHT,
        Math.max(TERMINAL_DOCK_MIN_HEIGHT, terminalDockHeight + delta),
      ),
    );
  };

  const startSidebarResize = (event: ReactPointerEvent<HTMLHRElement>) => {
    event.preventDefault();
    const startX = event.clientX;
    const startWidth = sidebarWidth;
    let nextWidth = startWidth;
    const onMove = (move: PointerEvent) => {
      nextWidth = clampSidebarWidth(startWidth + move.clientX - startX);
      layoutSetSize(currentProjectId, "sidebar", nextWidth);
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp, { once: true });
  };

  const startRightDockResize = (event: ReactPointerEvent<HTMLHRElement>) => {
    event.preventDefault();
    const startX = event.clientX;
    const startWidth = rightDockWidth;
    const onMove = (move: PointerEvent) => {
      const next = Math.min(
        RIGHT_DOCK_MAX_WIDTH,
        Math.max(RIGHT_DOCK_MIN_WIDTH, startWidth + startX - move.clientX),
      );
      layoutSetSize(currentProjectId, "right", next);
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp, { once: true });
  };

  const startTerminalDockResize = (event: ReactPointerEvent<HTMLHRElement>) => {
    event.preventDefault();
    const startY = event.clientY;
    const startHeight = terminalDockHeight;
    const onMove = (move: PointerEvent) => {
      const next = Math.min(
        TERMINAL_DOCK_MAX_HEIGHT,
        Math.max(TERMINAL_DOCK_MIN_HEIGHT, startHeight + startY - move.clientY),
      );
      layoutSetSize(currentProjectId, "terminal", next);
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp, { once: true });
  };

  return (
    <main className="relative flex min-h-0 flex-1 overflow-hidden">
      <div
        className={`relative min-h-0 shrink-0 overflow-hidden ${
          showRightPanel ? "flex max-[1100px]:hidden" : "flex"
        }`}
        style={{ width: sidebarCollapsed ? SIDEBAR_COLLAPSED_WIDTH : sidebarWidth }}
      >
        {!sidebarCollapsed ? (
          <>
            <div className="flex min-h-0 min-w-0 flex-1 flex-col">
              <PaneErrorBoundary name="Sidebar">
                <Sidebar {...sidebarProps} onOpenSettings={onOpenSettings} />
              </PaneErrorBoundary>
              <div className="border-t border-line bg-sidebar p-2">
                <ProjectPicker
                  workspaceRoot={workbenchProps.workspaceRoot}
                  onWorkspaceChange={composerProps.onSetWorkspace}
                  disabled={!health}
                />
              </div>
            </div>
            <hr
              aria-label="Resize sidebar"
              aria-orientation="vertical"
              aria-valuemin={SIDEBAR_MIN_WIDTH}
              aria-valuemax={SIDEBAR_MAX_WIDTH}
              aria-valuenow={sidebarWidth}
              aria-valuetext={`${sidebarWidth} pixels wide`}
              tabIndex={0}
              className="absolute -right-1 top-0 z-20 m-0 h-full w-2 cursor-col-resize border-0 bg-transparent p-0 focus-visible:outline focus-visible:outline-1 focus-visible:outline-focus"
              onPointerDown={startSidebarResize}
              onKeyDown={(event) => {
                if (event.key === "ArrowLeft") {
                  event.preventDefault();
                  resizeSidebarBy(-16);
                } else if (event.key === "ArrowRight") {
                  event.preventDefault();
                  resizeSidebarBy(16);
                } else if (event.key === "Home") {
                  event.preventDefault();
                  setAndPersistSidebarWidth(SIDEBAR_MIN_WIDTH);
                } else if (event.key === "End") {
                  event.preventDefault();
                  setAndPersistSidebarWidth(SIDEBAR_MAX_WIDTH);
                }
              }}
            />
          </>
        ) : null}
      </div>

      <section
        className="cr-content-plane relative flex min-w-0 flex-1 flex-col overflow-hidden bg-bg"
        data-cr-content-plane
        data-layout-project={currentProjectId}
        data-maximized-pane={maximizedPane ?? undefined}
      >
        <TitleBarSlotPortal>
          <AppShellHeader
            headerTitle={headerProps.headerTitle}
            headerSubtitle={headerProps.headerSubtitle}
            status={status}
            health={health}
            selected={headerProps.selected}
            messagesLength={headerProps.messagesLength}
            isWorking={headerProps.isWorking}
            onCompact={headerProps.onCompact}
            hasAgents={hasAgentRail}
            agentCount={railProps?.agents.length ?? 0}
            runningAgentCount={runningAgentCount}
            agentsOpen={agentsOpen}
            onToggleAgents={toggleAgents}
            onRetryEngine={onRetryEngine}
            tasks={workbenchProps.taskGraph ?? []}
            headerActions={
              <>
                <ConnectionSwitcher
                  config={workbenchProps.config}
                  activeSelection={composerProps.activeSelection}
                  disabled={!health}
                  onSelectionChange={composerProps.onSelectionChange}
                  onOpenConnectionSettings={onOpenSettings}
                />
                {workbenchProps.selected && workbenchProps.messages.length > 0 ? (
                  <KebabMenu
                    activeTab={showRightPanel ? effectiveRightPanelTab : null}
                    onSelect={handleKebabAction}
                  />
                ) : null}
              </>
            }
          />
        </TitleBarSlotPortal>

        {trustGateProps ? <WorkspaceTrustGate {...trustGateProps} /> : null}
        <ExtensionSlot name="header" />
        {searchOpen ? (
          <SearchPanel
            config={workbenchProps.config}
            workspaceRoot={workbenchProps.workspaceRoot}
            onClose={() => setSearchOpen(false)}
            onOpenFile={(hit) => setWorkspaceFilePath(hit.path)}
          />
        ) : null}

        {workbenchProps.selected?.driverKind && workbenchProps.selected.driverKind !== "native" ? (
          <div className="mx-auto w-full max-w-[calc(var(--cr-measure)+8rem)] px-3 pt-3">
            <DriverLifecycleStrip
              config={workbenchProps.config}
              sessionId={workbenchProps.selected.id}
              driverKind={workbenchProps.selected.driverKind}
              onManage={workbenchProps.onOpenDriverSettings}
            />
            {workbenchProps.selected.driverKind === "claude" ||
            workbenchProps.selected.driverKind === "codex" ? (
              <SessionAuthMethodLine
                config={workbenchProps.config}
                sessionId={workbenchProps.selected.id}
              />
            ) : null}
          </div>
        ) : null}

        {showOnboardingPanel ? (
          <PaneErrorBoundary name="Onboarding">
            <OnboardingPanel {...onboardingPanelProps} />
          </PaneErrorBoundary>
        ) : showNewChatState ? (
          /* Keep fresh-session canvas quiet while anchoring composer to same
             bottom command position used after transcript begins. */
          <div className="min-h-6 flex-1" />
        ) : (
          <div className="relative flex min-h-0 flex-1 flex-col overflow-hidden">
            <PaneErrorBoundary name="Workbench">
              <WorkbenchContent {...workbenchProps} />
            </PaneErrorBoundary>
          </div>
        )}

        {showNewChatState && !showOnboardingPanel ? <EmptyState /> : null}

        {/* The checklist moved to the title-bar task indicator (Adrian,
            2026-08-04). It lived here for one build; two places showing one
            truth is how they drift, so this slot is intentionally empty. */}
        {showOnboardingPanel ? null : (
          <>
          {sessionRateLimit ? (
            <div className="mx-auto w-full max-w-[calc(var(--cr-measure)+8rem)] px-3 pb-2">
              <RateLimitBanner
                event={sessionRateLimit}
                accounts={accountState.accounts}
                onContinueOnAccount={(accountId) =>
                  sessionActions.continueOnAccount(accountId, sessionRateLimit.account)
                }
              />
            </div>
          ) : null}
          <div className="mx-auto flex w-full max-w-[calc(var(--cr-measure)+8rem)] flex-col gap-2 px-3 pb-2 empty:hidden">
            <SubscriptionUsageNotice accounts={accountState.accounts} />
            <AccountSwitchNotice
              conflict={accountState.homeConflict}
              accounts={accountState.accounts}
              onStartNewSession={(accountId) => {
                // Manual only: a new session on the chosen account, seeded with a handoff.
                sessionActions.continueOnAccount(
                  accountId,
                  accountState.accounts.find((a) => a.id === accountState.selectedAccountId)
                    ?.label ?? "the current account",
                );
                accountState.dismissHomeConflict();
              }}
              onDismiss={accountState.dismissHomeConflict}
            />
          </div>
          <ComposerIsland
            {...composerProps}
            onCompact={headerProps.onCompact}
            canCompact={Boolean(
              headerProps.selected && headerProps.messagesLength > 0 && !headerProps.isWorking,
            )}
            composerRef={composerRef}
            agents={railProps?.agents ?? []}
            onOpenAgentsPanel={() => openSurface("agents", "background-tasks")}
            onOpenEnvironmentsPanel={() => openSurface("environments", "environments")}
            accountPicker={
              accountState.accounts.length > 0
                ? {
                    accounts: accountState.accounts,
                    selectedAccountId: accountState.selectedAccountId,
                    projectDefaultAccountId: accountState.projectDefaultAccountId,
                    onSelectAccount: (accountId: string) =>
                      accountState.selectAccount(accountId, {
                        // A chat with no messages has not run yet: the same-home rule is lifted.
                        threadStarted: headerProps.messagesLength > 0,
                      }),
                    onSetProjectDefault: accountState.setProjectDefault,
                  }
                : undefined
            }
          />
          </>
        )}

        {!showOnboardingPanel && terminalDockOpen ? (
          <TerminalDock
            height={terminalDockHeight}
            maximized={maximizedPane === "terminal"}
            onToggleMaximize={() =>
              layoutSetMaximized(currentProjectId, maximizedPane === "terminal" ? null : "terminal")
            }
            onResize={startTerminalDockResize}
            onResizeBy={resizeTerminalDockBy}
            onClose={closeTerminalDock}
          >
            <TerminalSlot className="flex min-h-0 min-w-0 flex-1 flex-col" />
          </TerminalDock>
        ) : null}
        <PersistentTerminal
          config={workbenchProps.config}
          sessionId={workbenchProps.selected?.id ?? null}
          workspaceRoot={workbenchProps.selected?.workspaceRoot || workbenchProps.workspaceRoot}
        />
      </section>

      {showRightPanel ? (
        <aside
          aria-label="Workspace utilities"
          className={`z-30 flex min-h-0 shrink-0 flex-col overflow-hidden rounded-xl border border-line bg-panel/95 shadow-xl shadow-black/20 ${
            maximizedPane === "right"
              ? "absolute bottom-3 left-3 right-16 top-3 z-40"
              : "relative my-3 mr-16"
          }`}
          style={{ width: maximizedPane === "right" ? undefined : rightDockWidth }}
        >
          <hr
            aria-label="Resize workspace utilities"
            aria-orientation="vertical"
            aria-valuemin={RIGHT_DOCK_MIN_WIDTH}
            aria-valuemax={RIGHT_DOCK_MAX_WIDTH}
            aria-valuenow={rightDockWidth}
            aria-valuetext={`${rightDockWidth} pixels wide`}
            tabIndex={0}
            className="absolute -left-1 top-0 z-30 m-0 h-full w-2 cursor-col-resize border-0 bg-transparent p-0 focus-visible:outline focus-visible:outline-1 focus-visible:outline-focus"
            onPointerDown={startRightDockResize}
            onKeyDown={(event) => {
              if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
                event.preventDefault();
                resizeRightDockBy(event.key === "ArrowLeft" ? 16 : -16);
              } else if (event.key === "PageUp") {
                event.preventDefault();
                resizeRightDockBy(64);
              } else if (event.key === "PageDown") {
                event.preventDefault();
                resizeRightDockBy(-64);
              } else if (event.key === "Home") {
                event.preventDefault();
                resizeRightDockBy(RIGHT_DOCK_MIN_WIDTH - rightDockWidth);
              } else if (event.key === "End") {
                event.preventDefault();
                resizeRightDockBy(RIGHT_DOCK_MAX_WIDTH - rightDockWidth);
              }
            }}
          />
          <div className="min-h-0 flex-1 overflow-hidden">
            <PaneErrorBoundary name={effectiveRightPanelTab}>
              {effectiveRightPanelTab === "files" ? (
                workspaceFilePath ? (
                  <WorkspaceFilePanel
                    config={workbenchProps.config}
                    workspaceRoot={workbenchProps.workspaceRoot}
                    path={workspaceFilePath}
                    onBack={() => setWorkspaceFilePath(null)}
                  />
                ) : (
                  <FilesPanel
                    config={workbenchProps.config}
                    workspaceRoot={workbenchProps.workspaceRoot}
                    onClose={closeRightPanel}
                    onMaximize={() =>
                      layoutSetMaximized(
                        currentProjectId,
                        maximizedPane === "right" ? null : "right",
                      )
                    }
                    onActivate={({ path }) => setWorkspaceFilePath(path)}
                    onReveal={(path) =>
                      setWorkspaceFilePath(
                        relativeWorkspacePath(path, workbenchProps.workspaceRoot),
                      )
                    }
                    onCopyPath={(path) => void copyPath(path)}
                  />
                )
              ) : effectiveRightPanelTab === "artifacts" ? (
                <ArtifactsPanel
                  artifacts={workbenchProps.artifacts}
                  selectedTitle={workbenchProps.selected?.title ?? null}
                  onClose={closeRightPanel}
                  onReveal={(path) => {
                    setWorkspaceFilePath(relativeWorkspacePath(path, workbenchProps.workspaceRoot));
                    openSurface("files", "files");
                  }}
                />
              ) : effectiveRightPanelTab === "review" ? (
                <ReviewPane
                  config={workbenchProps.config}
                  workspaceRoot={workbenchProps.workspaceRoot}
                  onRequestClose={closeRightPanel}
                  onDraftRevisionRequest={workbenchProps.onPrefillComposer}
                />
              ) : effectiveRightPanelTab === "execution" ? (
                <ExecutionGraphPane
                  tasks={workbenchProps.tasks}
                  agents={railProps?.agents ?? []}
                  costUsd={railProps?.costUsd}
                  onClose={closeRightPanel}
                  onStopAgent={onCancelSubAgent}
                  onOpenAgentSession={(id) => railProps?.onOpenAgentSession?.(id)}
                />
              ) : effectiveRightPanelTab === "bots" ? (
                <DockPanel title="Bots" onClose={closeRightPanel} flush>
                  {workbenchProps.config ? (
                    <BotsPane
                      config={workbenchProps.config}
                      workspaceRoot={workbenchProps.workspaceRoot}
                    />
                  ) : (
                    <UnavailableDock label={`Connect ${hostAppName()} to manage bots.`} />
                  )}
                </DockPanel>
              ) : effectiveRightPanelTab === "browser" ? (
                <DockPanel title="Browser" onClose={closeRightPanel} flush>
                  {workbenchProps.config && workbenchProps.selected ? (
                    <BrowserPane
                      key={workbenchProps.selected.id}
                      baseUrl={workbenchProps.config.url}
                      token={workbenchProps.config.token}
                      sessionId={workbenchProps.selected.id}
                      className="h-full min-h-0"
                    />
                  ) : (
                    <UnavailableDock label="Select a connected session to use the browser." />
                  )}
                </DockPanel>
              ) : effectiveRightPanelTab === "delivery" ? (
                <DockPanel title="Delivery" onClose={closeRightPanel}>
                  <DeliveryPanel
                    config={workbenchProps.config}
                    workspaceRoot={workbenchProps.workspaceRoot}
                  />
                </DockPanel>
              ) : effectiveRightPanelTab === "environments" ? (
                <DockPanel title="Environments" onClose={closeRightPanel}>
                  <EnvironmentsPanel
                    config={workbenchProps.config}
                    workspaceRoot={workbenchProps.workspaceRoot}
                  />
                </DockPanel>
              ) : effectiveRightPanelTab === "attention" ? (
                <DockPanel title="Attention" onClose={closeRightPanel}>
                  <AttentionPanel
                    config={workbenchProps.config}
                    workspaceRoot={workbenchProps.workspaceRoot}
                  />
                </DockPanel>
              ) : (
                <BackgroundTasksPanel
                  agents={railProps?.agents ?? []}
                  sessionTitle={railProps?.sessionTitle}
                  goalStatus={railProps?.goalStatus}
                  costUsd={railProps?.costUsd}
                  maximized={maximizedPane === "right"}
                  onClose={closeRightPanel}
                  onMaximize={() =>
                    layoutSetMaximized(currentProjectId, maximizedPane === "right" ? null : "right")
                  }
                  onStop={onCancelSubAgent}
                  onCancelAll={
                    railProps?.onCancelAll
                      ? () => {
                          void railProps?.onCancelAll?.();
                        }
                      : undefined
                  }
                  onOpenAgentSession={(id) => railProps?.onOpenAgentSession?.(id)}
                />
              )}
            </PaneErrorBoundary>
          </div>
        </aside>
      ) : null}
      {!showOnboardingPanel && maximizedPane === null ? (
        <aside className="absolute right-3 top-3 z-30" aria-label="Workspace tool rail">
          <WorkspaceToolRail
            activePanel={
              showRightPanel && effectiveRightPanelTab !== "background-tasks"
                ? effectiveRightPanelTab
                : null
            }
            terminalOpen={terminalDockOpen}
            onTogglePanel={(tab) => toggleRightPanel(currentProjectId, tab)}
            onClosePanel={closeRightPanel}
            onToggleTerminal={toggleTerminalDock}
          />
        </aside>
      ) : null}

      {shortcutsOpen ? (
        <ShortcutHelp
          registry={rightKitShortcuts}
          onClose={() => setShortcutsOpen(false)}
          title="Keyboard shortcuts"
        />
      ) : null}
      {renameTarget ? (
        <PromptDialog
          title="Rename session"
          label="Session title"
          initialValue={renameTarget.title}
          confirmLabel="Rename"
          validate={(value) => (value.trim() ? null : "Enter a session title.")}
          onCancel={() => setRenameTarget(null)}
          onSubmit={(value) => {
            sessionActions.renameSession(renameTarget.id, value);
            setRenameTarget(null);
          }}
        />
      ) : null}
      {sessionConfirm ? (
      <ConfirmDialog
        title={sessionConfirm?.action === "delete" ? "Delete session?" : "Archive session?"}
        description={
          sessionConfirm
            ? sessionConfirm.action === "delete"
              ? `Delete “${sessionConfirm.title}” and its transcript? This cannot be undone.`
              : `Archive “${sessionConfirm.title}”? It will be removed from your active sessions.`
            : ""
        }
        confirmLabel={sessionConfirm?.action === "delete" ? "Delete" : "Archive"}
        danger={sessionConfirm?.action === "delete"}
        onCancel={() => setSessionConfirm(null)}
        onConfirm={() => {
          if (sessionConfirm?.action === "delete") {
            sessionActions.deleteSelectedSession(sessionConfirm.id);
          } else if (sessionConfirm?.action === "archive") {
            sessionActions.archiveSession(sessionConfirm.id, sessionConfirm.title);
          }
          setSessionConfirm(null);
        }}
      />
      ) : null}
    </main>
  );
}

function rightPanelSurface(tab: RightPanelTab): Exclude<DockSurface, "terminal" | "diagnostics"> {
  return tab === "background-tasks" ? "agents" : tab;
}

function rightPanelTabForSurface(
  surface: Exclude<DockSurface, "terminal" | "diagnostics">,
): RightPanelTab {
  return surface === "agents" ? "background-tasks" : surface;
}

const OVERFLOW_RIGHT_UTILITY_TABS: Array<{
  surface: Exclude<DockSurface, "terminal" | "diagnostics">;
  tab: RightPanelTab;
  label: string;
  icon: IconName;
}> = [
  { surface: "artifacts", tab: "artifacts", label: "Artifacts", icon: "package" },
  { surface: "browser", tab: "browser", label: "Browser", icon: "panel-right" },
  { surface: "delivery", tab: "delivery", label: "Git", icon: "git" },
  { surface: "environments", tab: "environments", label: "Envs", icon: "server" },
  { surface: "attention", tab: "attention", label: "Inbox", icon: "alert" },
];

export function UtilityOverflowMenu({
  active,
  onSelect,
}: {
  active: RightPanelTab;
  onSelect: (surface: Exclude<DockSurface, "terminal" | "diagnostics">, tab: RightPanelTab) => void;
}) {
  const overflowActive = OVERFLOW_RIGHT_UTILITY_TABS.some((item) => item.tab === active);
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  // The anchored menu (focus, arrows, Home/End, Escape, outside press) is RightKit's `ContextMenu`.
  const items = useMemo<MenuItem[]>(
    () =>
      OVERFLOW_RIGHT_UTILITY_TABS.map((item) => ({
        id: item.tab,
        label: item.label,
        run: () => onSelect(item.surface, item.tab),
      })),
    [onSelect],
  );
  return (
    <div className="relative shrink-0">
      <button
        ref={triggerRef}
        aria-label="More workspace tools"
        aria-haspopup="menu"
        aria-expanded={open}
        title="More workspace tools"
        data-testid="utility-overflow-trigger"
        onClick={() => setOpen((current) => !current)}
        className={`cr-control-pill grid h-7 w-7 place-items-center rounded-md text-text-3 hover:bg-panel-2 hover:text-text ${overflowActive ? "bg-panel-2 text-text" : ""}`}
        type="button"
      >
        <Icon name="more" size={13} />
      </button>
      {open && triggerRef.current ? (
        <ContextMenu
          anchor={triggerRef.current}
          placement="bottom-end"
          label="More workspace tools"
          items={items}
          itemAs="button"
          width={180}
          onClose={() => setOpen(false)}
          renderItem={(item) => {
            const meta = OVERFLOW_RIGHT_UTILITY_TABS.find((entry) => entry.tab === item.id);
            return (
              <span
                data-testid={`utility-overflow-item-${item.id}`}
                aria-current={active === item.id ? "true" : undefined}
                className="flex w-full items-center gap-2"
              >
                {meta ? <Icon name={meta.icon} size={12} /> : null}
                <span>{item.label}</span>
              </span>
            );
          }}
        />
      ) : null}
    </div>
  );
}

function TerminalDock({
  height,
  maximized,
  onToggleMaximize,
  onResize,
  onResizeBy,
  onClose,
  children,
}: {
  height: number;
  maximized: boolean;
  onToggleMaximize: () => void;
  onResize: (event: ReactPointerEvent<HTMLHRElement>) => void;
  onResizeBy: (delta: number) => void;
  onClose: () => void;
  children: ReactNode;
}) {
  return (
    <section
      aria-label="Terminal dock"
      className={`flex min-h-0 flex-none flex-col overflow-hidden border-t border-line bg-bg ${maximized ? "absolute inset-0 z-40" : "relative"}`}
      style={{ height: maximized ? "100%" : height }}
    >
      <hr
        aria-label="Resize terminal dock"
        aria-orientation="horizontal"
        aria-valuemin={TERMINAL_DOCK_MIN_HEIGHT}
        aria-valuemax={TERMINAL_DOCK_MAX_HEIGHT}
        aria-valuenow={height}
        aria-valuetext={`${height} pixels tall`}
        tabIndex={0}
        className="absolute -top-1 left-0 z-30 m-0 h-2 w-full cursor-row-resize border-0 bg-transparent p-0 focus-visible:outline focus-visible:outline-1 focus-visible:outline-focus"
        onPointerDown={onResize}
        onKeyDown={(event) => {
          if (event.key === "ArrowUp" || event.key === "ArrowDown") {
            event.preventDefault();
            onResizeBy(event.key === "ArrowUp" ? 16 : -16);
          } else if (event.key === "PageUp") {
            event.preventDefault();
            onResizeBy(64);
          } else if (event.key === "PageDown") {
            event.preventDefault();
            onResizeBy(-64);
          } else if (event.key === "Home") {
            event.preventDefault();
            onResizeBy(TERMINAL_DOCK_MIN_HEIGHT - height);
          } else if (event.key === "End") {
            event.preventDefault();
            onResizeBy(TERMINAL_DOCK_MAX_HEIGHT - height);
          }
        }}
      />
      <header className="flex h-8 flex-none items-center gap-2 border-b border-line bg-panel px-3 text-[11px] font-medium text-text-2">
        <Icon name="terminal" size={12} />
        <span>Terminal</span>
        <span className="text-text-3">⌘J</span>
        <button
          type="button"
          aria-label={maximized ? "Restore terminal dock" : "Maximize terminal dock"}
          title={maximized ? "Restore" : "Maximize"}
          onClick={onToggleMaximize}
          className="cr-control-pill ml-auto grid h-6 w-6 place-items-center rounded-md text-text-3 hover:bg-panel-2 hover:text-text"
        >
          <Icon name="maximize" size={11} />
        </button>
        <button
          type="button"
          aria-label="Close terminal dock"
          title="Close terminal"
          onClick={onClose}
          className="cr-control-pill grid h-6 w-6 place-items-center rounded-md text-text-3 hover:bg-panel-2 hover:text-text"
        >
          <Icon name="close" size={11} />
        </button>
      </header>
      <div className="flex min-h-0 flex-1 overflow-hidden">{children}</div>
    </section>
  );
}

function EmptyState() {
  // The composer is the primary action. A fresh session needs room to think,
  // not a dashboard heading competing for attention.
  return (
    <p className="sr-only" aria-live="polite">
      New session ready.
    </p>
  );
}

function ArtifactsPanel({
  artifacts,
  selectedTitle,
  onClose,
  onReveal,
}: {
  artifacts: Parameters<typeof ArtifactRowsPanel>[0]["artifacts"];
  selectedTitle: string | null;
  onClose: () => void;
  onReveal: (path: string) => void;
}) {
  return (
    <aside
      aria-label="Artifacts panel"
      data-testid="artifacts-panel"
      className="flex h-full w-full min-w-0 shrink-0 flex-col overflow-hidden bg-panel/85"
    >
      <header className="flex h-9 flex-none items-center gap-2 border-b border-line px-3 text-[13px] font-medium text-text">
        <span className="flex-1 truncate">Artifacts</span>
        <span className="max-w-[160px] truncate text-[11px] font-normal text-text-3">
          {artifacts.length} {artifacts.length === 1 ? "item" : "items"}
          {selectedTitle ? ` · ${selectedTitle}` : ""}
        </span>
        <button
          type="button"
          aria-label="Close artifacts"
          title="Close"
          onClick={onClose}
          className="cr-control-pill grid h-6 w-6 place-items-center rounded-md border border-transparent text-text-3 cursor-pointer hover:bg-panel-2 hover:text-text"
        >
          <Icon name="close" size={13} />
        </button>
      </header>
      <div className="min-h-0 flex-1 divide-y divide-line overflow-y-auto">
        <ArtifactRowsPanel artifacts={artifacts} onReveal={onReveal} />
      </div>
    </aside>
  );
}

function DockPanel({
  title,
  onClose,
  flush = false,
  children,
}: {
  title: string;
  onClose: () => void;
  flush?: boolean;
  children: ReactNode;
}) {
  return (
    <aside
      aria-label={`${title} panel`}
      className="flex h-full w-full min-w-0 shrink-0 flex-col overflow-hidden bg-panel/85"
    >
      <header className="flex h-9 flex-none items-center gap-2 border-b border-line px-3 text-[13px] font-medium text-text">
        <span className="flex-1 truncate">{title}</span>
        <button
          type="button"
          aria-label={`Close ${title}`}
          title="Close"
          onClick={onClose}
          className="cr-control-pill grid h-6 w-6 place-items-center rounded-md border border-transparent text-text-3 cursor-pointer hover:bg-panel-2 hover:text-text"
        >
          <Icon name="close" size={13} />
        </button>
      </header>
      <div className={`min-h-0 flex-1 overflow-y-auto ${flush ? "" : "p-3"}`}>{children}</div>
    </aside>
  );
}

function WorkspaceFilePanel({
  config,
  workspaceRoot,
  path,
  onBack,
}: {
  config: Parameters<typeof WorkspaceFileViewer>[0]["config"];
  workspaceRoot: string;
  path: string;
  onBack: () => void;
}) {
  return (
    <aside
      aria-label="Workspace file preview"
      className="flex h-full w-full min-w-0 shrink-0 flex-col overflow-hidden bg-panel/85"
    >
      <header className="flex h-9 flex-none items-center gap-2 border-b border-line px-3 text-[13px] font-medium text-text">
        <button
          type="button"
          aria-label="Back to files"
          title="Back to files"
          onClick={onBack}
          className="cr-control-pill grid h-6 w-6 place-items-center rounded-md border border-transparent text-text-3 cursor-pointer hover:bg-panel-2 hover:text-text"
        >
          <span className="rotate-90">
            <Icon name="chevron" size={13} />
          </span>
        </button>
        <span className="min-w-0 flex-1 truncate font-mono text-[11px]">{path}</span>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto p-3">
        <WorkspaceFileViewer config={config} workspaceRoot={workspaceRoot} path={path} />
      </div>
    </aside>
  );
}

function UnavailableDock({ label }: { label: string }) {
  return (
    <p className="rounded-md border border-line bg-panel-2 p-3 text-[12px] text-text-2">{label}</p>
  );
}

function relativeWorkspacePath(path: string, workspaceRoot: string) {
  const normalizedPath = path.replaceAll("\\", "/");
  const normalizedRoot = workspaceRoot.replaceAll("\\", "/").replace(/\/+$/, "");
  return normalizedPath.startsWith(`${normalizedRoot}/`)
    ? normalizedPath.slice(normalizedRoot.length + 1)
    : normalizedPath.replace(/^\/+/, "");
}

async function copyPath(path: string) {
  try {
    await navigator.clipboard?.writeText(path);
  } catch {
    // A clipboard denial must not make a file row non-interactive.
  }
}
