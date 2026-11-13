// Sync provider choice (issue #17): one provider per account. Apple-only users
// use iCloud; anyone with a non-Apple device uses Google (their own Drive),
// with an explicit one-time migration when switching. Gated by
// the host's `enabled` flag (off by default); renders nothing until enabled.

import { memo, useCallback, useEffect, useState } from "react";
import { getPlatform } from "@rightkit/platform-ui";
import { CompactSettingRow, CompactSettingsGroup } from "../components/settings/CompactSettingsGroup.js";
import ErrorBanner from "../components/ErrorBanner.js";
import { sectionClient, type SyncProviderChoice } from "./client.js";

type SyncProvider = SyncProviderChoice;

const LABELS: Record<SyncProvider, string> = { icloud: "iCloud", google: "Google Drive", off: "Off" };

// `enabled` is the host's feature flag for the provider-choice UI (off by default).
function SyncProviderSettingsImpl({ enabled = false }: { enabled?: boolean }) {
  const isMac = getPlatform() === "macos";
  const [provider, setProvider] = useState<SyncProvider | null>(null);
  const [switching, setSwitching] = useState<SyncProvider | null>(null);
  const [error, setError] = useState("");

  const refresh = useCallback(async () => {
    const stored = await sectionClient("sync").syncProvider();
    setProvider(stored ?? "off");
  }, []);

  useEffect(() => {
    if (!enabled) return;
    void refresh().catch((e) => setError(String(e)));
  }, [enabled, refresh]);

  if (!enabled) return null;

  const options: SyncProvider[] = isMac ? ["icloud", "google", "off"] : ["google", "off"];

  async function confirmSwitch() {
    if (!switching) return;
    const target = switching;
    try {
      if (target === "google") {
        // TODO(rightkit#162): launch "Sign in with Google" and run the one-time
        // migration into the user's own Drive before persisting the choice.
        throw new Error("Google sign-in is not available yet.");
      }
      await sectionClient("sync").setSyncProvider(target);
      setSwitching(null);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <CompactSettingsGroup title="Sync provider" id="settings-sync-provider">
      <ErrorBanner message={error} onDismiss={() => setError("")} />
      <CompactSettingRow
        label="Sync provider"
        sublabel="One provider per account. Apple-only users use iCloud; any non-Apple device needs Google Drive."
        settingId="settings.sync-provider"
      >
        <span className="text-[length:var(--size-sm)] text-[var(--muted)]">
          {provider ? LABELS[provider] : "Checking…"}
        </span>
        <select
          aria-label="Switch provider"
          value=""
          onChange={(e) => e.target.value && setSwitching(e.target.value as SyncProvider)}
        >
          <option value="">Switch provider…</option>
          {options.filter((o) => o !== provider).map((o) => (
            <option key={o} value={o}>{LABELS[o]}</option>
          ))}
        </select>
      </CompactSettingRow>
      {switching && (
        <div className="px-[var(--space-4)] py-[var(--space-3)]" role="alertdialog">
          <span className="block max-w-[560px] text-[length:var(--size-xs)] leading-[1.45] text-[var(--warn)]">
            Switching to {LABELS[switching]} runs a one-time migration of your synced data. Only one
            provider is active at a time, and devices on the old provider stop syncing.
          </span>
          <button type="button" onClick={() => void confirmSwitch()}>Switch to {LABELS[switching]}</button>
          <button type="button" onClick={() => setSwitching(null)}>Cancel</button>
        </div>
      )}
    </CompactSettingsGroup>
  );
}

export const SyncProviderSettings = memo(SyncProviderSettingsImpl);
