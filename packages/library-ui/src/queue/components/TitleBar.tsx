import { Titlebar, cx } from "@rightkit/react-shell/react";
import { createWindowControlLabels } from "@rightkit/platform-ui";
import { Settings as SettingsIcon } from "lucide-react";
import { getShell } from "../lib/ipc/index.js";
import { isMac } from "../lib/platform.js";
import { NavTabs, type Tab } from "./NavTabs.js";

type QueueCounts = { total: number; active: number; ready: number };

type TitleBarProps = {
  activeTab: Tab;
  onTabChange: (tab: Tab) => void;
  counts: QueueCounts;
};

// Window chrome comes from @rightkit/react-shell's Titlebar. Caption buttons use the window actions of
// the RightKit shell (CaptionControls draws the built-in glyphs). macOS keeps the native traffic lights
// (titleBarStyle Overlay in tauri.macos.conf.json) and Titlebar reserves their space (#259).
export const CAPTION_LABELS = createWindowControlLabels({
  minimize: "Minimize",
  maximize: "Maximize",
  close: "Close",
});

export function TitleBar({ activeTab, onTabChange, counts }: TitleBarProps) {
  return (
    <Titlebar
      className="desktop-titlebar relative select-none"
      platform={isMac ? "macos" : "windows"}
      startWidth="auto"
      windowControls={getShell().bridge().window}
      windowControlLabels={CAPTION_LABELS}
      start={
        <>
          <span className="pointer-events-none flex shrink-0 items-center font-[family-name:var(--wordmark)] text-lg tracking-wide text-[var(--paper)]">
            Scrape<span className="text-[var(--accent)]">Right</span>
          </span>
          <NavTabs activeTab={activeTab} onChange={onTabChange} />
          {counts.active > 0 && (
            <span className="hidden shrink-0 items-center gap-2 font-[family-name:var(--mono)] text-xs text-[var(--muted)] md:flex">
              <span role="status">{counts.active} in progress</span>
            </span>
          )}
        </>
      }
      end={
        <button
          type="button"
          className={cx(
            "tb-btn h-full w-11 text-[var(--muted)] hover:text-[var(--paper)]",
            activeTab === "settings" && "bg-[var(--panel-2)] text-[var(--accent)]",
          )}
          onClick={() => onTabChange("settings")}
          title="Settings"
          aria-label="Settings"
        >
          <SettingsIcon size={17} strokeWidth={1.8} aria-hidden="true" />
        </button>
      }
    />
  );
}
