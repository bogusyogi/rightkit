import { usePlatform } from "@rightkit/platform-ui/react";
import { CaptionControls, IconButton, Titlebar } from "@rightkit/react-shell/react";
import { shellWindowAction } from "@rightkit/agent-ui-core/lib/shellWindow";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { hostAppName } from "../appName";

type WindowAction = "minimize" | "toggle-maximize" | "close";

async function windowCommand(action: WindowAction): Promise<void> {
  try {
    await shellWindowAction(action);
  } catch {
    // Browser previews do not expose native window commands.
  }
}

// Uses the RightKit shell client's window commands (`createShell().window`).
const captionAdapter = {
  minimize: () => windowCommand("minimize"),
  toggleMaximize: () => windowCommand("toggle-maximize"),
  close: () => windowCommand("close"),
};

// Keep the host app existing accessible names rather than RightKit's defaults.
const captionLabels = {
  minimize: "Minimize window",
  maximize: "Maximize window",
  close: "Close window",
  enterFullscreen: "Enter fullscreen",
  exitFullscreen: "Exit fullscreen",
};

// TitleBarPane is the legacy string union the old TitleBar accepted (used by
// the AppFrame type bridge). New code that needs the workbench pane type
// should import `Pane` from `./WorkbenchContent` directly.
export type TitleBarPane = "session" | "tasks" | "runtime" | "terminal" | "memory";

interface TitleBarProps {
  // Legacy props kept for caller compatibility — the v5 chrome bar has no tabs.
  activePane?: TitleBarPane;
  onPane?: (pane: TitleBarPane) => void;

  sidebarCollapsed?: boolean;
  onToggleSidebar?: () => void;

  // Search / palette trigger.
  onOpenPalette?: () => void;

  // Settings modal.
  settingsOpen?: boolean;
  onOpenSettings?: () => void;

  // Project labels are shown in the composer/workspace strip, not in the
  // Windows title bar.
  projectLabel?: string | null;
  projectBrand?: string | null;
}

export default function TitleBar(props: TitleBarProps) {
  const {
    sidebarCollapsed = false,
    onToggleSidebar,
    onOpenPalette,
  } = props;
  // macOS draws its own traffic lights in the overlay title bar; the app must
  // not paint a second set of caption buttons, and must leave room on the left
  // so the native ones don't sit on top of the sidebar toggle. RightKit reserves
  // that room via --rk-lights-width (78px, see styles/rightkit-shell-tokens.css).
  // The platform comes from the host-injected value (lib.rs, __RIGHTKIT_PLATFORM__).
  const platform = usePlatform();
  const isMac = platform === "macos";
  return (
    // biome-ignore lint/a11y/useSemanticElements: a <fieldset> is the semantic twin, but this is a display: contents wrapper and fieldset's form-control rendering is not guaranteed to honour that in every webview (WebView2, WKWebView); the group role and name are what assistive tech reads.
    <div role="group" aria-label=`${hostAppName()} window controls` className="contents">
      <Titlebar
        platform={platform}
        startWidth="auto"
        className="select-none text-text-2"
        slots={{ title: "flex flex-1 h-full min-w-0", drag: "hidden" }}
        start={
          <div className={`flex items-center gap-0.5 ${isMac ? "" : "pl-2"}`}>
            <IconButton
              label="Toggle sessions sidebar"
              title="Toggle sidebar"
              className="cr-control-pill inline-flex h-6 w-6 items-center justify-center rounded-md border border-transparent bg-transparent text-text-3 hover:bg-panel-2 hover:text-text"
              // `pressed` mirrors the toggled surface: the sidebar is visible when
              // NOT collapsed, so its pressed state is the negation of collapsed.
              pressed={!sidebarCollapsed}
              onClick={onToggleSidebar}
            >
              <Icon name="panel" size={14} />
            </IconButton>
            <IconButton
              label="Search sessions"
              title="Search"
              className="cr-control-pill inline-flex h-6 w-6 items-center justify-center rounded-md border border-transparent bg-transparent text-text-3 hover:bg-panel-2 hover:text-text"
              onClick={onOpenPalette}
            >
              <Icon name="search" size={14} />
            </IconButton>
          </div>
        }
        // Session identity + per-session controls are portalled into this slot by
        // AppBody (Adrian, 2026-08-04). Empty space in the slot still drags the window.
        title={
          <div
            data-cr-titlebar-slot
            className="flex h-full min-w-0 flex-1 items-center gap-2 pl-2"
            data-tauri-drag-region
          />
        }
        end={
          isMac ? null : (
            <CaptionControls adapter={captionAdapter} labels={captionLabels} />
          )
        }
      />
    </div>
  );
}
