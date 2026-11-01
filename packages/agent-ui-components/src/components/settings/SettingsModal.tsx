import { Button, ConfirmDialog, ModalShell } from "@rightkit/react-shell/react";
import { appearanceSection } from "@rightkit/settings-ui/appearance";
import { SettingsPanel, type SettingsSection } from "@rightkit/settings-ui/core";
import { useCallback, useEffect, useMemo, useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import PaneErrorBoundary from "@rightkit/agent-ui-core/components/PaneErrorBoundary";
import AppearanceTab from "@rightkit/agent-ui-settings/components/settings/tabs/AppearanceTab";
import { licenseTabSection } from "@rightkit/agent-ui-settings/components/settings/tabs/LicenseTab";
import SettingsPaneContent from "./SettingsPaneContent";
import {
  SETTINGS_NAV_GROUPS,
  SETTINGS_NAV_ITEMS,
  type SettingsPaneProps,
  type SettingsTab,
  navItemForTab,
} from "./types";
import { hostAppName } from "../../appName";

export type SettingsModalProps = SettingsPaneProps;

// The modal host (backdrop, Escape, focus trap, focus restore) is RightKit's `ModalShell`
// (@rightkit/react-shell); the grouped section nav, search and active-section frame are RightKit's
// `SettingsPanel` (@rightkit/settings-ui). This module supplies the host app header, dirty/config
// banners and the save / discard / reload footer around them.
export default function SettingsModal(props: SettingsModalProps) {
  const {
    open,
    onClose,
    activeTab,
    onTab,
    settingsDirty,
    settingsDraft,
    settingsConfigPending,
    settingsConfigStatus,
    workspaceRoot,
    engineReady,
    onReloadSettingsConfig,
    onDiscardSettings,
    onSaveSettings,
    ...rest
  } = props;

  const canSaveSettings = Boolean(settingsDirty && settingsDraft);
  const [discardConfirmOpen, setDiscardConfirmOpen] = useState(false);

  const attemptClose = useCallback(() => {
    if (settingsConfigPending) return;
    if (settingsDirty) {
      setDiscardConfirmOpen(true);
      return;
    }
    onClose?.();
  }, [onClose, settingsConfigPending, settingsDirty]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: `rest` is destructured from `props` above, so [props] already covers it; listing `rest` too would rebuild the sections on state-only renders.
  const sections = useMemo<SettingsSection[]>(
    () =>
      SETTINGS_NAV_GROUPS.flatMap((group, groupIndex) =>
        group.tabs.map((tab, tabIndex) => {
          const item = navItemForTab(tab);
          if (tab === "appearance") {
            // The theme row (System / Light / Dark) is RightKit's `appearanceSection`; the palette
            // identities, preview, analytics consent and typography reference are the host app and
            // render in its `extra` slot.
            return {
              ...appearanceSection({ extra: () => <AppearanceTab /> }),
              id: tab,
              label: item.label,
              group: group.label,
              order: groupIndex * 100 + tabIndex,
              icon: <Icon name={item.icon} size={14} />,
            };
          }
          if (tab === "license") {
            // The licence rows are RightKit's `licenseSection`; the host app Apple panels, update
            // controls and feature list follow them in the same section.
            return {
              ...licenseTabSection,
              id: tab,
              label: item.label,
              group: group.label,
              order: groupIndex * 100 + tabIndex,
              icon: <Icon name={item.icon} size={14} />,
              hideSectionHeading: true,
              render: (ctx) => (
                <PaneErrorBoundary name="Settings">
                  {licenseTabSection.render(ctx)}
                  <SettingsPaneContent {...props} {...rest} activeTab={tab} />
                </PaneErrorBoundary>
              ),
            };
          }
          return {
            id: tab,
            label: item.label,
            group: group.label,
            order: groupIndex * 100 + tabIndex,
            icon: <Icon name={item.icon} size={14} />,
            hideSectionHeading: true,
            render: () => (
              <PaneErrorBoundary name="Settings">
                <SettingsPaneContent {...props} {...rest} activeTab={tab} />
              </PaneErrorBoundary>
            ),
          };
        }),
      ),
    [props],
  );

  // Deep-link support: #settings=<tab> opens the modal on the matching tab.
  // Tabs are validated against SETTINGS_NAV_ITEMS, so a stale or hand-typed
  // URL cannot mount an unknown surface.
  useEffect(() => {
    if (!open) return;
    function handleHashChange() {
      const raw = window.location.hash.replace(/^#/, "");
      const match = raw.match(/settings=([\w-]+)/);
      if (!match) return;
      const candidate = match[1] as SettingsTab;
      if (SETTINGS_NAV_ITEMS.some((item) => item.tab === candidate)) {
        onTab(candidate);
      }
    }
    handleHashChange();
    window.addEventListener("hashchange", handleHashChange);
    return () => window.removeEventListener("hashchange", handleHashChange);
  }, [open, onTab]);

  if (!open) return null;

  function selectTab(tab: string) {
    onTab(tab as SettingsTab);
    if (typeof window !== "undefined") {
      const next = `${window.location.pathname}${window.location.search}#settings=${tab}`;
      window.history.replaceState(null, "", next);
    }
  }

  const activeItem = navItemForTab(activeTab);
  const engineLabel = engineReady ? "local engine ready" : "local engine starting";
  const contextLabel = workspaceRoot || engineLabel || "No workspace selected";

  const isReloading = settingsConfigPending;
  const configUnavailable = Boolean(
    workspaceRoot.trim() && !settingsDraft && !settingsConfigStatus && !settingsConfigPending,
  );
  const reloadDisabled = isReloading || !workspaceRoot.trim();
  const reloadLabel = isReloading ? "Reloading…" : "Reload settings";
  const saveTooltip = isReloading
    ? "Settings reload in progress — wait for it to finish"
    : settingsDraft
      ? settingsDirty
        ? "Save your settings changes"
        : "No unsaved changes to save"
      : "Settings draft is not loaded";
  const discardTooltip = isReloading
    ? "Settings reload in progress — wait for it to finish"
    : settingsDirty
      ? "Discard unsaved settings changes"
      : "Nothing to discard";
  const closeTooltip = isReloading
    ? "Settings reload in progress — wait for it to finish"
    : "Close settings (Esc)";

  return (
    <>
      <ModalShell
        label={`${hostAppName()} settings`}
        ariaLabel={`${hostAppName()} settings`}
        width={960}
        onClose={attemptClose}
        // Veto: Escape and backdrop route through attemptClose (discard guard or close), and the
        // shell never starts its own exit, so a dirty form cannot close behind the confirm.
        dismissible={() => {
          attemptClose();
          return false;
        }}
        className="flex h-[min(680px,88vh)] max-h-[min(780px,94vh)] flex-col overflow-hidden p-0"
      >
        <div
          className="flex min-h-0 flex-1 flex-col"
          aria-busy={isReloading}
          data-settings-pending={isReloading ? "true" : "false"}
        >
          <header className="flex min-h-14 flex-none items-center justify-between gap-3 border-b border-line px-5 py-2.5">
            <div className="min-w-0">
              <h2
                id="settings-modal-title"
                className="truncate text-[15px] font-semibold leading-5 text-text"
              >
                Settings
              </h2>
              <p className="truncate text-[11px] leading-4 text-text-3" title={contextLabel}>
                {activeItem.label}
                {contextLabel ? ` · ${contextLabel}` : ""}
              </p>
            </div>
            <button
              type="button"
              aria-label="Close settings (Esc)"
              title={closeTooltip}
              disabled={isReloading}
              data-settings-action="close"
              className="grid h-7 w-7 place-items-center rounded-md border border-line bg-transparent text-text-2 cursor-pointer hover:bg-panel-2 hover:text-text disabled:cursor-not-allowed disabled:opacity-70"
              onClick={attemptClose}
            >
              <Icon name="close" size={14} />
            </button>
          </header>

          {configUnavailable ? (
            <div
              role="alert"
              data-testid="settings-config-error"
              className="flex items-center gap-3 border-b border-danger/30 bg-danger/10 px-5 py-2 text-[12px] text-danger"
            >
              <span className="flex-1">
                Settings unavailable. Check the engine connection and try again.
              </span>
              <button
                type="button"
                disabled={isReloading}
                onClick={onReloadSettingsConfig}
                data-settings-action="try-again"
                className="cr-control-pill rounded-md border border-danger/40 px-2 py-1"
              >
                {isReloading ? "Reloading…" : "Try again"}
              </button>
            </div>
          ) : null}
          {settingsDirty && settingsDraft ? (
            <div
              role="status"
              aria-live="polite"
              data-testid="settings-dirty-banner"
              className="border-b border-line bg-warning/10 px-4 py-1.5 text-[11px] text-warning"
            >
              Unsaved settings changes. Save or discard before closing.
            </div>
          ) : null}

          <div className="min-h-0 flex-1">
            <SettingsPanel
              sections={sections}
              activeId={activeTab}
              onActiveChange={selectTab}
              navHeading={false}
              searchLabel="Search settings"
              findShortcut
              hideSectionHeading
              paint={false}
            />
          </div>

          <footer className="flex h-12 min-h-0 flex-none items-center justify-between gap-2 border-t border-line px-5">
            <Button
              type="button"
              variant="secondary"
              disabled={reloadDisabled}
              onClick={onReloadSettingsConfig}
              aria-label={reloadLabel}
              title={reloadLabel}
              data-settings-action="reload"
            >
              <Icon name="activity" />
              {reloadLabel}
            </Button>
            <div className="flex items-center gap-2">
              <Button
                type="button"
                variant="secondary"
                disabled={isReloading || !settingsDirty}
                onClick={onDiscardSettings}
                aria-label="Discard unsaved settings changes"
                title={discardTooltip}
                data-settings-action="discard"
              >
                Discard
              </Button>
              <Button
                type="button"
                variant={canSaveSettings ? "primary" : "secondary"}
                disabled={isReloading || !canSaveSettings}
                onClick={onSaveSettings}
                aria-label={
                  canSaveSettings ? "Save settings changes" : "No unsaved settings changes"
                }
                title={saveTooltip}
                data-settings-action="save"
              >
                Save
              </Button>
            </div>
          </footer>
        </div>
      </ModalShell>
      {discardConfirmOpen ? (
        <ConfirmDialog
          title="Discard unsaved settings changes?"
          description="Your changes in Settings have not been saved and will be lost."
          confirmLabel="Discard changes"
          danger={false}
          onCancel={() => setDiscardConfirmOpen(false)}
          onConfirm={() => {
            onDiscardSettings();
            setDiscardConfirmOpen(false);
            onClose?.();
          }}
        />
      ) : null}
    </>
  );
}
