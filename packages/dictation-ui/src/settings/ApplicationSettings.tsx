// Application section — OS-level app behavior: launch at login and the
// auto-update preference. (Resting-pill visibility lives in the title bar.)

import { memo, useEffect, useState } from "react";
import { getPlatform } from "@rightkit/platform-ui";
import type { SettingsContext } from "@rightkit/settings-ui/core";
import { CompactSettingRow, CompactSettingsGroup } from "../components/settings/CompactSettingsGroup.js";
import ErrorBanner from "../components/ErrorBanner.js";
import { hostAppName } from "../host.js";
import type { PatchValue, SettingsBlob } from "../types/settings.js";
import { sectionClient } from "./client.js";
import { HistoryKeyRotationRow } from "./HistoryKeyRotationRow.js";
import { SectionToggle as Toggle } from "./Toggle.js";
// HR-SYNC-001 (docs/DOGFOOD.md): the one "Sync with Apple devices" control.
// Mounted here only — AppleSyncSettings.tsx owns its own implementation and
// is macOS-gated internally (HR-SYNC-012), so it is safe to mount
// unconditionally for every platform.

function ApplicationSettingsImpl({ settings }: { settings: SettingsContext }) {
  // Launch-at-login is persisted by the OS (registry Run key / LaunchAgent)
  // through tauri-plugin-autostart, not the settings blob — so the toggle
  // self-manages: read on mount, write on change. null = still loading.
  const [launchAtLogin, setLaunchAtLogin] = useState<boolean | null>(null);
  // set_launch_at_login is a raw invoke, not a settings backend patch, so it needs
  // its own error state: a silent revert with zero feedback leaves the OS
  // state and the UI disagreeing with no explanation. useSettings() below
  // tracks its own patch failures separately, in `patchError`.
  const [launchAtLoginError, setLaunchAtLoginError] = useState("");
  const desktop = getPlatform() !== "ios";
  // macOS calls this "Open at Login" (System Settings > General > Login Items).
  const launchLabel = getPlatform() === "macos" ? "Open at Login" : "Open at startup";
  const loaded = true;
  const get = <T extends PatchValue>(key: keyof SettingsBlob, fallback: T): T =>
    settings.values[key] == null ? fallback : (settings.values[key] as T);
  const patch = (key: keyof SettingsBlob, value: PatchValue) => settings.set(key, value);
  const patchError = settings.error ?? "";
  const setPatchError = (message: string) => {
    if (!message) settings.clearError();
  };
  const autoInstall = get("auto_update_install", false);

  useEffect(() => {
    if (!desktop) return;
    let alive = true;
    sectionClient("app")
      .launchAtLoginEnabled()
      .then((enabled) => alive && setLaunchAtLogin(enabled))
      .catch(() => alive && setLaunchAtLogin(false));
    return () => {
      alive = false;
    };
  }, [desktop]);

  if (!desktop) return null;
  return (
    <CompactSettingsGroup title="App">
      <ErrorBanner
        message={launchAtLoginError || patchError}
        onDismiss={() => {
          setLaunchAtLoginError("");
          setPatchError("");
        }}
      />
      <CompactSettingRow
        label={launchLabel}
        sublabel={`Start ${hostAppName()} quietly when you sign in, so dictation is ready.`}
        settingId="settings.open-at-startup"
      >
        <Toggle
          checked={launchAtLogin === true}
          disabled={launchAtLogin === null}
          ariaLabel={launchLabel}
          onChange={(next) => {
            setLaunchAtLogin(next);
            setLaunchAtLoginError("");
            void sectionClient("app").setLaunchAtLogin(next).catch((e) => {
              // Revert on failure so the toggle never lies about OS state,
              // and say so — a silent revert leaves the user staring at a
              // toggle that flipped itself back with no explanation.
              setLaunchAtLogin(!next);
              setLaunchAtLoginError(String(e));
            });
          }}
        />
      </CompactSettingRow>
      <CompactSettingRow
        label="Install updates automatically"
        sublabel="Updates download and install in the background. Relaunch is still your choice."
        settingId="settings.install-updates-automatically"
      >
        <Toggle
          checked={autoInstall}
          disabled={!loaded}
          ariaLabel="Install updates automatically"
          onChange={(next) => void patch("auto_update_install", next)}
        />
      </CompactSettingRow>
      <HistoryKeyRotationRow />
    </CompactSettingsGroup>
  );
}

export const ApplicationSettings = memo(ApplicationSettingsImpl);
