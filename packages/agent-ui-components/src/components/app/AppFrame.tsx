import { useCallback, useEffect, useState } from "react";
import CommandPalette, { type UtilityTarget } from "@rightkit/agent-ui-core/components/CommandPalette";
import TitleBar, { type TitleBarPane } from "../TitleBar";
import {
  AppFrame as RightKitAppFrame,
  ShellProvider,
  usePersistedState,
  useShortcuts,
} from "@rightkit/react-shell/react";
import { shellThemeStore } from "@rightkit/agent-ui-core/state/themeStore";
import { defaultToastBus } from "@rightkit/react-shell";
import AppBody from "./AppBody";
import SettingsModal from "../settings/SettingsModal";
import { listCliAccountProfiles, loginCliProvider } from "@rightkit/agent-ui-core/lib/daemon/config-host";
import { CLI_PROVIDER_IDS } from "@rightkit/agent-ui-core/lib/cliProviders";
import { usePaletteStore } from "@rightkit/agent-ui-core/state/paletteStore";
import { useSidePanelStore } from "@rightkit/agent-ui-core/state/sidePanelStore";
import { useRightPanelStore, type RightPanelTab } from "@rightkit/agent-ui-core/state/rightPanelStore";
import { projectLayoutOrDefault, useLayoutStore, type DockSurface } from "@rightkit/agent-ui-core/state/layoutStore";
import type { Pane as WorkbenchPane } from "../WorkbenchContent";
import { deriveUpdateOverlay } from "@rightkit/app-update";
import { UpdateOverlay } from "@rightkit/app-update/react";
import { useUpdateStatus } from "@rightkit/agent-ui-hooks/hooks/useUpdateStatus";
import { dismissUpdateStatus } from "@rightkit/agent-ui-core/state/updates";

type AppBodyProps = Parameters<typeof AppBody>[0];
type CommandPaletteProps = Parameters<typeof CommandPalette>[0];
// AppFrame's setPane accepts the broader workbench Pane union. TitleBar's
// legacy onPane is the same string union, but TypeScript can't see that
// without the explicit intersection.
type Pane = WorkbenchPane & TitleBarPane;

/**
 * The palette chord is a binding on the shell's registry, not a private window listener. the host app own
 * registry holds the `mod+k` sequences; both see every keydown, and its bindings accept an event this one
 * already claimed (`allowDefaultPrevented`), so `mod+k` opens the palette and still starts a sequence.
 */
function PaletteShortcut() {
  useShortcuts([
    {
      id: "app.palette",
      chord: "mod+k",
      label: "Command palette",
      group: "General",
      allowInInput: true,
      run: () => {
        const palette = usePaletteStore.getState();
        if (palette.isOpen) palette.close();
        else palette.open("global");
        return true;
      },
    },
  ]);
  return null;
}

export default function AppFrame(props: {
  paletteOpen: boolean;
  setPaletteOpen: (open: boolean) => void;
  settingsOpen: boolean;
  setSettingsOpen: (open: boolean) => void;
  sidebarSessions: CommandPaletteProps["sessions"];
  setPane: (pane: Pane) => void;
  setSelectedId: (id: string | null) => void;
  pane: Pane;
  updateActions: { onCheck: () => void; onDownload: () => void; onInstall: () => void };
  status: string;
  providerKeyStatuses: AppBodyProps["providerKeyStatuses"];
  providerKeyDrafts: AppBodyProps["providerKeyDrafts"];
  providerKeySaving: AppBodyProps["providerKeySaving"];
  providerKeyValidation: AppBodyProps["providerKeyValidation"];
  validateProviderKeyDraft: AppBodyProps["validateProviderKeyDraft"];
  setProviderKeyDrafts: AppBodyProps["setProviderKeyDrafts"];
  onSaveProviderKeyDraft: (provider: string) => void;
  onRemoveProviderKey: (provider: string) => void;
  surfaces: ReturnType<typeof import("../../hooks/app/useAppSurfaces").useAppSurfaces>;
  health: AppBodyProps["health"];
  commands: CommandPaletteProps["commands"];
  runSlashCommand: (input: string) => void;
  onRetryEngine: () => void;
  onCancelSubAgent: (id: string) => void;
  projectLabel: string | null;
  projectBrand: string | null;
}) {
  const {
    settingsOpen,
    setSettingsOpen,
    sidebarSessions,
    setPane,
    setSelectedId,
    pane,
    updateActions,
    status,
    providerKeyStatuses,
    providerKeyDrafts,
    providerKeySaving,
    providerKeyValidation,
    validateProviderKeyDraft,
    setProviderKeyDrafts,
    surfaces,
    health,
    commands,
    runSlashCommand,
    onRetryEngine,
    onCancelSubAgent,
    projectLabel,
    projectBrand,
  } = props;

  // Persisted under the historical key; the stored values stay "1" / "0".
  const [sidebarCollapsed, setSidebarCollapsed] = usePersistedState<boolean>(
    "cr.sidebarCollapsed.v1",
    false,
    {
      parse: (raw) => raw === "1",
      serialize: (value) => (value ? "1" : "0"),
    },
  );
  const toggleSidebar = useCallback(() => setSidebarCollapsed((collapsed) => !collapsed), [setSidebarCollapsed]);
  const updateStatus = useUpdateStatus();

  const paletteIsOpen = usePaletteStore((s) => s.isOpen);
  const paletteOpenStore = usePaletteStore((s) => s.open);
  const paletteCloseStore = usePaletteStore((s) => s.close);

  const setSidePanelOpen = useSidePanelStore((s) => s.setOpen);
  const setRightPanelTab = useRightPanelStore((s) => s.setTab);
  const projectId = surfaces.workbenchProps.workspaceRoot.trim() || "default";

  const openUtility = (target: UtilityTarget) => {
    const layout = useLayoutStore.getState();
    if (target === "reset-layout") {
      layout.resetToDefault(projectId);
      setRightPanelTab("files");
      setSidePanelOpen(true);
      return;
    }
    if (target === "maximize-utilities") {
      layout.setMaximized(projectId, "right");
      setSidePanelOpen(true);
      return;
    }
    if (target === "maximize-terminal") {
      const visible = projectLayoutOrDefault(layout.byProject, projectId).visible;
      layout.setVisible(
        projectId,
        visible.includes("terminal") ? visible : [...visible, "terminal"],
      );
      layout.setMaximized(projectId, "terminal");
      return;
    }
    const visible = projectLayoutOrDefault(layout.byProject, projectId).visible;
    const surface: DockSurface = target === "background-tasks" ? "agents" : target;
    layout.setVisible(projectId, visible.includes(surface) ? visible : [...visible, surface]);
    if (target === "terminal") return;
    const tab: RightPanelTab = target;
    layout.setRightTab(projectId, tab);
    setRightPanelTab(tab);
    setSidePanelOpen(true);
  };

  const [cliAccountProfiles, setCliAccountProfiles] = useState<
    import("@rightkit/agent-ui-core/lib/daemon/config-host").CliAccountProfileView[]
  >([]);

  const refreshCliAccountProfiles = useCallback(async () => {
    try {
      const profiles = await Promise.all(
        CLI_PROVIDER_IDS.map((provider) => listCliAccountProfiles(provider).catch(() => [])),
      );
      setCliAccountProfiles(profiles.flat());
    } catch (error) {
      console.warn("Unable to load CLI account profiles", error);
    }
  }, []);

  useEffect(() => {
    if (settingsOpen) void refreshCliAccountProfiles();
  }, [settingsOpen, refreshCliAccountProfiles]);

  const onLoginCliProvider = async (provider: string) => {
    try {
      await loginCliProvider(provider);
      await refreshCliAccountProfiles();
    } catch (error) {
      console.warn(`Unable to sign in with ${provider}`, error);
    }
  };

  const openSettingsModalProps = {
    ...surfaces.settingsPaneProps,
    cliAccountProfiles,
    onLoginCliProvider,
    open: settingsOpen,
    onClose: () => setSettingsOpen(false),
  };

  // ShellProvider mounts RightKit's toast viewport on the default bus that toast() writes to.
  // Its palette, help and sidebar chords stay disabled: the palette chord is owned by the
  // Ctrl/Cmd+K listener below (B11), and help is AppBody's "?" registration.
  return (
    <ShellProvider
      themeStore={shellThemeStore}
      toastBus={defaultToastBus}
      toastPlacement="top-center"
      toastOptions={{ dedupe: "newest", maxVisible: 3 }}
      keybindings={{ palette: false, help: false, sidebar: false }}
    >
      <RightKitAppFrame>
        <PaletteShortcut />
        <CommandPalette
          open={paletteIsOpen}
          onClose={paletteCloseStore}
          commands={commands}
          sessions={sidebarSessions}
          onPane={setPane}
          onRunCommand={(input) => void runSlashCommand(input)}
          onSelectSession={(id) => setSelectedId(id)}
          onOpenSettings={() => setSettingsOpen(true)}
          onOpenUtility={openUtility}
        />
        <TitleBar
          activePane={pane}
          onPane={setPane}
          onOpenPalette={() => paletteOpenStore("global")}
          sidebarCollapsed={sidebarCollapsed}
          onToggleSidebar={toggleSidebar}
          settingsOpen={settingsOpen}
          onOpenSettings={() => setSettingsOpen(true)}
          projectLabel={surfaces.headerProps.headerTitle ?? projectLabel}
          projectBrand={surfaces.headerProps.headerSubtitle ?? projectBrand}
        />
        <div className="px-4 pt-2 text-[13px] text-text-2">
          <UpdateOverlay
            model={deriveUpdateOverlay(updateStatus)}
            onCheck={updateActions.onCheck}
            onDownload={updateActions.onDownload}
            onInstall={updateActions.onInstall}
            onDismiss={dismissUpdateStatus}
          />
        </div>
        <AppBody
          status={status}
          providerKeyStatuses={providerKeyStatuses}
          providerKeyDrafts={providerKeyDrafts}
          providerKeySaving={providerKeySaving}
          providerKeyValidation={providerKeyValidation}
          validateProviderKeyDraft={validateProviderKeyDraft}
          setProviderKeyDrafts={setProviderKeyDrafts}
          headerProps={surfaces.headerProps}
          health={health}
          sidebarProps={surfaces.sidebarProps}
          trustGateProps={surfaces.trustGateProps}
          showOnboardingPanel={surfaces.showOnboardingPanel}
          onboardingPanelProps={surfaces.onboardingPanelProps}
          workbenchProps={surfaces.workbenchProps}
          sessionActions={surfaces.sessionActions}
          railProps={surfaces.railProps}
          composerProps={surfaces.composerProps}
          sidebarCollapsed={sidebarCollapsed}
          onToggleSidebar={toggleSidebar}
          onOpenSettings={() => setSettingsOpen(true)}
          onOpenPane={setPane}
          onRetryEngine={onRetryEngine}
          onCancelSubAgent={onCancelSubAgent}
        />
        <SettingsModal {...openSettingsModalProps} />
      </RightKitAppFrame>
    </ShellProvider>
  );
}
