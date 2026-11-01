import { useMemo, useReducer } from "react";
import { Button, Toggle } from "@rightkit/react-shell/react";
import {
  SettingRow,
  SettingsGroup,
  UpdatesControls,
  aboutSection,
  legalSection,
  type UpdatesLabels,
  type SettingsContext,
  type SettingsSection,
} from "@rightkit/settings-ui";
import {
  APP_VERSION,
  createHostUpdatesAdapter,
  hasPendingExternalUpdate,
  openPendingExternalUpdate,
} from "../state/updateLicense.js";
import { useSettings } from "@rightkit/document-core/settings/settings";
import { hostAppName } from "../appName.js";

/** the host app keeps its own settings store; this exposes the one key the published sections read. */
function useRkContext(): SettingsContext {
  const { settings, update } = useSettings();
  return useMemo(() => {
    const apply = (patch: Record<string, unknown>) => {
      if ("updates.autoInstall" in patch) {
        update({ autoCheckUpdates: patch["updates.autoInstall"] === true });
      }
    };
    return {
      values: { "updates.autoInstall": settings.autoCheckUpdates },
      set: async (key, value) => apply({ [key]: value }),
      patch: async (patch) => apply(patch),
      saving: false,
      error: null,
      clearError: () => {},
    };
  }, [settings.autoCheckUpdates, update]);
}

/** Renders a published section body under the the host app token map. */
function RkSection({ section, ctx, children }: { section: SettingsSection; ctx: SettingsContext; children?: React.ReactNode }) {
  return (
    <div className="vr-rk">
      {section.render(ctx)}
      {children}
    </div>
  );
}

const UPDATE_LABELS: Partial<UpdatesLabels> = {
  version: (v) => `${hostAppName()} ${v}`,
  checkNow: "Check for updates",
  checking: "Checking...",
  installUpdate: "Install & relaunch",
  installing: "Installing...",
  status: (s, fallback) => {
    switch (s.kind) {
      case "idle":
        return "Look for a newer release.";
      case "up_to_date":
        return "You’re up to date.";
      case "available":
        return s.eligible
          ? `${hostAppName()} ${s.tier === "patch" ? "patch" : "update"} ${s.version} is ready.`
          : "An update license is required for this channel.";
      case "deferred":
        return `${hostAppName()} ${s.version} is available as a manual download.`;
      case "ready":
        return `Restarting ${hostAppName()}...`;
      default:
        return fallback;
    }
  },
};

/** Published UpdatesControls over the host app update client: status sentence, no badge (rightkit item 10a). */
export function UpdatesPanel() {
  const { settings, update } = useSettings();
  const [, refresh] = useReducer((n: number) => n + 1, 0);
  const client = useMemo(() => createHostUpdatesAdapter(refresh), []);
  return (
    <div className="vr-rk">
      <SettingsGroup>
        <SettingRow id="updates.status" label={`${hostAppName()} ${APP_VERSION}`}>
          <UpdatesControls
            client={client}
            labels={UPDATE_LABELS}
            upgradeHint="Activate an update license below."
            showStatus
            showBadge={false}
          />
        </SettingRow>
        <SettingRow
          id="updates.auto"
          label="Automatic update checks"
          description={`Look for new releases while ${hostAppName()} is running. Installs always wait for you.`}
        >
          <Toggle
            label="Automatic update checks"
            checked={settings.autoCheckUpdates}
            onChange={(v: boolean) => update({ autoCheckUpdates: v })}
          />
        </SettingRow>
        {hasPendingExternalUpdate() && (
          <SettingRow label="Manual download" description="Install the signed public build yourself.">
            <Button size="sm" variant="secondary" onClick={() => void openPendingExternalUpdate()}>
              Download installer
            </Button>
          </SettingRow>
        )}
      </SettingsGroup>
    </div>
  );
}

/** Published aboutSection: the host app brand card replaces the default card; panels hold the rest. */
export function AboutFrame({ card, panels }: { card: React.ReactNode; panels: React.ReactNode }) {
  const ctx = useRkContext();
  const section = useMemo(
    () => aboutSection({ appName: hostAppName(), version: APP_VERSION, card, panels }),
    [card, panels],
  );
  return <RkSection section={section} ctx={ctx} />;
}

/** Renders the published licenseSection body (status, verify, deactivate, activate, purchase rows) under the the host app token map. */
export function PublishedLicense({ section }: { section: SettingsSection }) {
  const ctx = useRkContext();
  return <RkSection section={section} ctx={ctx} />;
}

/** Published legalSection: one row per legal document with the acceptance receipt as its description. */
export function LegalPanel({
  documents,
  acceptedAt,
  acceptedVersion,
  openExternal,
}: {
  documents: ReadonlyArray<{ title: string; url: string }>;
  acceptedAt: string | null;
  acceptedVersion: string | null;
  openExternal: (url: string) => void | Promise<void>;
}) {
  const ctx = useRkContext();
  const section = useMemo(
    () => legalSection({ documents, acceptedAt, acceptedVersion, openExternal }),
    [documents, acceptedAt, acceptedVersion, openExternal],
  );
  return <RkSection section={section} ctx={ctx} />;
}
