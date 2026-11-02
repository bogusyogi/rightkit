import { useCallback, useEffect, useRef, useState } from "react";
import {
  getOrchestrationSettings,
  getSettingsConfigStatus,
  reloadSettingsConfig,
  saveOrchestrationSettings,
} from "@rightkit/agent-ui-core/lib/daemon";
import type { DaemonConfig, OrchestrationSettings, SettingsConfigStatus } from "@rightkit/agent-ui-core/lib/types";
import type { SettingsDraft } from "../components/settings/types.js";
import type { InferenceSelection } from "@rightkit/agent-ui-core/lib/types/inference";

interface UseOrchestrationSettingsOptions {
  config: DaemonConfig | null;
  workspaceRoot: string;
  formatError: (error: unknown, fallback: string) => string;
  onStatus: (status: string) => void;
}

function toDraft(settings: OrchestrationSettings): SettingsDraft {
  return {
    primaryOrchestratorModelId: settings.primaryOrchestratorModelId,
    primarySelection: settings.primarySelection ?? null,
    roleSelections: { ...(settings.roleSelections ?? {}) },
    roleModelMode: settings.roleModelMode,
    roleModelSplitAvailable: settings.roleModelSplitAvailable,
    modelOverrides: {},
    budget: { ...settings.budget },
    benchmarks: { ...settings.benchmarks },
    remote: {
      telegramToken: "",
      clearTelegramToken: false,
      telegramAllowedUsersText: settings.remote.telegramAllowedUsers.join(", "),
      webEnabled: settings.remote.webEnabled,
      telegramTokenConfigured: settings.remote.telegramTokenConfigured,
      webEnabledSource: settings.remote.webEnabledSource ?? null,
    },
    routing: {
      ...settings.routing,
      capabilityRouterSource: settings.routing.capabilityRouterSource ?? null,
    },
    keepDaemonResident: settings.keepDaemonResident,
  };
}

function parseTelegramAllowedUsers(raw: string): number[] {
  const parts = raw
    .split(/[\s,]+/)
    .map((part) => part.trim())
    .filter(Boolean);
  const values = parts.map((part) => {
    if (!/^\d+$/.test(part)) {
      throw new Error(`Telegram user id must be numeric: ${part}`);
    }
    const value = Number(part);
    if (!Number.isSafeInteger(value)) {
      throw new Error(`Telegram user id is too large: ${part}`);
    }
    return value;
  });
  return Array.from(new Set(values));
}

export function useOrchestrationSettings({
  config,
  workspaceRoot,
  formatError,
  onStatus,
}: UseOrchestrationSettingsOptions) {
  const [settings, setSettings] = useState<OrchestrationSettings | null>(null);
  const [settingsDraft, setSettingsDraftState] = useState<SettingsDraft | null>(null);
  const [settingsDirty, setSettingsDirty] = useState(false);
  const [settingsConfigStatus, setSettingsConfigStatus] = useState<SettingsConfigStatus | null>(
    null,
  );
  const [settingsConfigPending, setSettingsConfigPending] = useState(false);
  const settingsConfigFingerprintRef = useRef<string | null>(null);

  const loadSettings = useCallback(async () => {
    if (!config || !workspaceRoot.trim()) return;
    try {
      const next = await getOrchestrationSettings(config, workspaceRoot.trim());
      const status = await getSettingsConfigStatus(config, workspaceRoot.trim());
      setSettings(next);
      setSettingsDraftState(toDraft(next));
      setSettingsDirty(false);
      setSettingsConfigStatus(status);
      settingsConfigFingerprintRef.current = status.fingerprint;
    } catch (err) {
      onStatus(formatError(err, "Settings failed"));
    }
  }, [config, formatError, onStatus, workspaceRoot]);

  const setSettingsDraft = useCallback((draft: SettingsDraft) => {
    setSettingsDraftState(draft);
    setSettingsDirty(true);
  }, []);

  const discardSettings = useCallback(() => {
    if (!settings) return;
    setSettingsDraftState(toDraft(settings));
    setSettingsDirty(false);
  }, [settings]);

  const saveSettings = useCallback(async () => {
    if (!config || !settings || !settingsDraft) return;
    try {
      const telegramAllowedUsers = parseTelegramAllowedUsers(
        settingsDraft.remote.telegramAllowedUsersText,
      );
      const telegramToken = settingsDraft.remote.telegramToken.trim();
      const next = {
        workspaceRoot: settings.workspaceRoot,
        primarySelection: settingsDraft.primarySelection,
        roleSelections: settingsDraft.roleSelections ?? {},
        roleModelMode: settingsDraft.roleModelMode,
        budget: settingsDraft.budget,
        benchmarks: settingsDraft.benchmarks,
        remote: {
          telegramToken: telegramToken || undefined,
          clearTelegramToken: settingsDraft.remote.clearTelegramToken,
          telegramAllowedUsers,
          webEnabled: settingsDraft.remote.webEnabled,
        },
        routing: settingsDraft.routing,
        keepDaemonResident: settingsDraft.keepDaemonResident,
      };
      await saveOrchestrationSettings(config, next);
      const tokenConfigured = settingsDraft.remote.clearTelegramToken
        ? false
        : Boolean(telegramToken) || settings.remote.telegramTokenConfigured;
      const savedRemote = {
        telegramTokenConfigured: tokenConfigured,
        telegramAllowedUsers,
        webEnabled: settingsDraft.remote.webEnabled,
      };
      setSettings((current) => (current ? { ...current, ...next, remote: savedRemote } : current));
      setSettingsDraftState((current) =>
        current
          ? {
              ...current,
              remote: {
                telegramToken: "",
                clearTelegramToken: false,
                telegramAllowedUsersText: telegramAllowedUsers.join(", "),
                webEnabled: savedRemote.webEnabled,
                telegramTokenConfigured: savedRemote.telegramTokenConfigured,
              },
            }
          : current,
      );
      setSettingsDirty(false);
      // Only the Telegram bot token/allow-list needs a daemon restart to take
      // effect — don't claim it for unrelated saves (budgets, models, etc.).
      const telegramChanged = Boolean(telegramToken) || settingsDraft.remote.clearTelegramToken;
      onStatus(
        telegramChanged
          ? "Settings saved. Restart the daemon to apply Telegram changes."
          : "Settings saved.",
      );
      void getSettingsConfigStatus(config, settings.workspaceRoot)
        .then((status) => {
          setSettingsConfigStatus(status);
          settingsConfigFingerprintRef.current = status.fingerprint;
        })
        .catch(() => {});
    } catch (err) {
      onStatus(formatError(err, "Settings save failed"));
    }
  }, [config, formatError, onStatus, settings, settingsDraft]);

  const saveRoleSelection = useCallback(async (roleId: string, selection: InferenceSelection) => {
    if (!config || !settings || !settingsDraft || !settings.agents.some((agent) => agent.id === roleId)) {
      throw new Error("Unknown role");
    }
    const roleSelections = { ...(settingsDraft.roleSelections ?? {}), [roleId]: selection };
    await saveOrchestrationSettings(config, {
      workspaceRoot: settings.workspaceRoot,
      primarySelection: settingsDraft.primarySelection,
      roleSelections,
      roleModelMode: settingsDraft.roleModelMode,
      budget: settingsDraft.budget,
      benchmarks: settingsDraft.benchmarks,
      remote: { telegramAllowedUsers: parseTelegramAllowedUsers(settingsDraft.remote.telegramAllowedUsersText), webEnabled: settingsDraft.remote.webEnabled },
      routing: settingsDraft.routing,
      keepDaemonResident: settingsDraft.keepDaemonResident,
    });
    await loadSettings();
  }, [config, loadSettings, settings, settingsDraft]);

  const reloadSettingsConfigNow = useCallback(async () => {
    if (!config || !workspaceRoot.trim()) return;
    setSettingsConfigPending(true);
    try {
      const reloaded = await reloadSettingsConfig(config, workspaceRoot.trim());
      setSettings(reloaded.settings);
      setSettingsDraftState(toDraft(reloaded.settings));
      setSettingsDirty(false);
      setSettingsConfigStatus(reloaded.configStatus);
      settingsConfigFingerprintRef.current = reloaded.configStatus.fingerprint;
      onStatus("Settings reloaded from config files.");
    } catch (err) {
      onStatus(formatError(err, "Settings reload failed"));
    } finally {
      setSettingsConfigPending(false);
    }
  }, [config, formatError, onStatus, workspaceRoot]);

  useEffect(() => {
    if (!config || !workspaceRoot.trim()) {
      setSettingsConfigStatus(null);
      settingsConfigFingerprintRef.current = null;
      return;
    }
    let cancelled = false;
    const poll = async () => {
      try {
        const status = await getSettingsConfigStatus(config, workspaceRoot.trim());
        if (cancelled) return;
        const previous = settingsConfigFingerprintRef.current;
        setSettingsConfigStatus(status);
        if (!previous) {
          settingsConfigFingerprintRef.current = status.fingerprint;
          return;
        }
        if (previous !== status.fingerprint) {
          if (settingsDirty) {
            settingsConfigFingerprintRef.current = status.fingerprint;
            onStatus("Config changed on disk. Save or discard local edits before reload.");
            return;
          }
          const reloaded = await reloadSettingsConfig(config, workspaceRoot.trim());
          if (cancelled) return;
          setSettings(reloaded.settings);
          setSettingsDraftState(toDraft(reloaded.settings));
          setSettingsDirty(false);
          setSettingsConfigStatus(reloaded.configStatus);
          settingsConfigFingerprintRef.current = reloaded.configStatus.fingerprint;
          onStatus("Settings reloaded from changed config files.");
        }
      } catch {
        // Polling is best-effort; manual refresh surfaces errors.
      }
    };
    const timer = window.setInterval(() => {
      void poll();
    }, 5000);
    void poll();
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [config, onStatus, settingsDirty, workspaceRoot]);

  return {
    settings,
    settingsDraft,
    settingsDirty,
    settingsConfigStatus,
    settingsConfigPending,
    setSettingsDraft,
    loadSettings,
    reloadSettingsConfigNow,
    saveSettings,
    saveRoleSelection,
    discardSettings,
  };
}

export type UseOrchestrationSettingsReturn = ReturnType<typeof useOrchestrationSettings>;
