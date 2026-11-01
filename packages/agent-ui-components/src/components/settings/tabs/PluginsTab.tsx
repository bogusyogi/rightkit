import { Checkbox } from "@rightkit/react-shell/react";
import { useState, type ReactNode } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { hasPluginUpdate } from "../../../hooks/usePluginsRuntime";
import type { PluginManifest, PluginMarketplaceEntry } from "@rightkit/agent-ui-core/lib/types";
import type {
  PluginInstallDraft,
  PluginMarketplaceInstallDraft,
  PluginMarketplaceSyncDraft,
} from "../types";
import InstalledPluginsList from "@rightkit/agent-ui-settings/components/settings/tabs/plugins/InstalledPluginsList";
import PluginContributionHost from "@rightkit/agent-ui-settings/components/PluginContributionHost";
import { projectDaemonPluginManifest } from "@rightkit/agent-ui-core/lib/pluginContributionProjection";

const EMPTY_PLUGIN_INSTALL_DRAFT: PluginInstallDraft = {
  sourcePath: "",
  trustAccepted: false,
  overwrite: false,
};
const EMPTY_MARKETPLACE_SYNC_DRAFT: PluginMarketplaceSyncDraft = {
  catalogUrl: "",
  expectedSha256: "",
  trustAccepted: false,
  requireSigned: true,
  allowFileUrls: false,
};
const EMPTY_MARKETPLACE_INSTALL_DRAFT: PluginMarketplaceInstallDraft = {
  expectedSha256: "",
  trustAccepted: false,
  overwrite: false,
  allowFileUrls: false,
};

function shortHash(value: string | undefined): string {
  if (!value) return "missing";
  return value.length > 12 ? `${value.slice(0, 12)}...` : value;
}

function pluginsSummary(
  plugins: PluginManifest[],
  marketplaceEntries: PluginMarketplaceEntry[],
  requireSigned: boolean,
): string {
  if (plugins.length === 0) return "No plugins installed";
  const enabledCount = plugins.filter((p) => p.enabled).length;
  const updatesAvailable = marketplaceEntries.filter((entry) => {
    const installed = plugins.find((p) => p.id === entry.id);
    return installed !== undefined && hasPluginUpdate(installed.version, entry.version);
  }).length;
  const parts = [`${plugins.length} installed`, `${enabledCount} enabled`];
  if (updatesAvailable > 0) {
    parts.push(`${updatesAvailable} update${updatesAvailable === 1 ? "" : "s"} available`);
  }
  if (requireSigned) parts.push("signing required");
  return parts.join(" · ");
}

function Disclosure({ label, children }: { label: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="rounded-xl border border-line">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center justify-between gap-2 px-4 py-2.5 text-[13px] font-semibold text-text-2 cursor-pointer border-0 bg-transparent hover:text-text focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
      >
        <span>{label}</span>
        <span
          aria-hidden="true"
          className={`transition-transform duration-150 ${open ? "rotate-180" : ""}`}
        >
          <Icon name="chevron" size={12} />
        </span>
      </button>
      {open ? <div className="flex flex-col gap-3 border-t border-line p-4">{children}</div> : null}
    </div>
  );
}
export default function PluginsTab({
  pluginDir = "",
  plugins = [],
  pendingId = null,
  installDraft = EMPTY_PLUGIN_INSTALL_DRAFT,
  marketplacePath = "",
  marketplaceEntries = [],
  marketplaceQuery = "",
  marketplaceSyncDraft = EMPTY_MARKETPLACE_SYNC_DRAFT,
  marketplaceInstallDraft = EMPTY_MARKETPLACE_INSTALL_DRAFT,
  marketplaceError = null,
  onRefresh,
  onToggle,
  onInstallDraft,
  onPickSource,
  onInstall,
  onMarketplaceQuery,
  onRefreshMarketplace,
  onMarketplaceSyncDraft,
  onSyncMarketplace,
  onMarketplaceInstallDraft,
  onInstallMarketplace,
}: {
  pluginDir: string;
  plugins: PluginManifest[];
  pendingId: string | null;
  installDraft: PluginInstallDraft;
  marketplacePath: string;
  marketplaceEntries: PluginMarketplaceEntry[];
  marketplaceQuery: string;
  marketplaceSyncDraft: PluginMarketplaceSyncDraft;
  marketplaceInstallDraft: PluginMarketplaceInstallDraft;
  marketplaceError: string | null;
  onRefresh: () => void;
  onToggle: (plugin: PluginManifest) => void;
  onInstallDraft: (draft: PluginInstallDraft) => void;
  onPickSource: () => void;
  onInstall: () => void;
  onMarketplaceQuery: (query: string) => void;
  onRefreshMarketplace: () => void;
  onMarketplaceSyncDraft: (draft: PluginMarketplaceSyncDraft) => void;
  onSyncMarketplace: () => void;
  onMarketplaceInstallDraft: (draft: PluginMarketplaceInstallDraft) => void;
  onInstallMarketplace: (entry: PluginMarketplaceEntry) => void;
}) {
  const installDisabled =
    pendingId !== null || !installDraft.sourcePath.trim() || !installDraft.trustAccepted;
  const marketplaceSyncDisabled =
    pendingId !== null ||
    !marketplaceSyncDraft.catalogUrl.trim() ||
    !marketplaceSyncDraft.expectedSha256.trim() ||
    !marketplaceSyncDraft.trustAccepted;
  const marketplaceInstallDisabled = !marketplaceInstallDraft.trustAccepted || pendingId !== null;

  const summary = pluginsSummary(plugins, marketplaceEntries, marketplaceSyncDraft.requireSigned);

  return (
    <section
      className="flex flex-col gap-5 p-6"
      aria-busy={pendingId !== null}
      aria-label="Plugin management"
    >
      <div className="flex items-start gap-3">
        <div className="flex flex-col gap-1 flex-1 min-w-0">
          <h3 className="text-[16px] font-bold text-text">Plugins</h3>
          <p className="text-[13px] font-semibold text-text">{summary}</p>
          <p className="text-[12px] text-text-3 truncate">
            {pluginDir || "Plugin directory will appear after the engine connects."}
          </p>
        </div>
        <button
          type="button"
          className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text"
          onClick={onRefresh}
          disabled={pendingId !== null}
          aria-label="Reload installed plugins"
        >
          <Icon name="refresh" /> Reload
        </button>
      </div>

      <div className="flex flex-col gap-3 border border-line rounded-xl p-4">
        <div className="grid grid-cols-[minmax(0,1fr)_auto_auto] gap-3 items-end">
          <label className="flex flex-col gap-1 text-[13px] text-text-2 min-w-0">
            Local plugin folder
            <input
              className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
              value={installDraft.sourcePath}
              onChange={(e) =>
                onInstallDraft({
                  ...installDraft,
                  sourcePath: e.target.value,
                })
              }
              placeholder="Folder containing plugin.json"
            />
          </label>
          <button
            type="button"
            className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text"
            onClick={onPickSource}
          >
            <Icon name="folder" /> Choose
          </button>
          <button
            type="button"
            className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[13px] cursor-pointer hover:opacity-90 disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3"
            disabled={installDisabled}
            onClick={onInstall}
          >
            <Icon name="plus" /> {installDraft.overwrite ? "Update" : "Install"}
          </button>
        </div>
        <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-3 items-center">
          <Checkbox checked={installDraft.trustAccepted} onChange={(checked) => onInstallDraft({
                  ...installDraft,
                  trustAccepted: checked,
                })} label="I trust this local plugin source and understand it can run subprocess code." className="inline-flex items-center gap-2 text-[13px] text-text-2 min-w-0" />
          <Checkbox checked={installDraft.overwrite} onChange={(checked) => onInstallDraft({
                  ...installDraft,
                  overwrite: checked,
                })} label="Overwrite existing" className="inline-flex items-center gap-2 text-[13px] text-text-2" />
        </div>
      </div>

      <div className="flex flex-col gap-4">
        <div className="flex flex-col gap-3 border border-line rounded-xl p-4 min-w-0">
          <div className="flex items-center justify-between gap-3">
            <div className="flex flex-col gap-1 min-w-0">
              <h4 className="text-[13px] font-semibold text-text">Marketplace</h4>
              <p className="text-[12px] text-text-3 truncate">
                {marketplacePath || "No catalog loaded"}
              </p>
            </div>
            <button
              type="button"
              className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel hover:text-text"
              onClick={onRefreshMarketplace}
              disabled={pendingId !== null}
              aria-label="Refresh plugin marketplace"
            >
              <Icon name="refresh" /> Refresh
            </button>
          </div>
          <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-3">
            <input
              aria-label="Search marketplace"
              className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
              value={marketplaceQuery}
              onChange={(e) => onMarketplaceQuery(e.target.value)}
              placeholder="Search marketplace"
            />
            <button
              type="button"
              className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text"
              onClick={onRefreshMarketplace}
              disabled={pendingId !== null}
              aria-label="Search plugin marketplace"
            >
              <Icon name="search" /> Search
            </button>
          </div>
          {marketplaceError ? (
            <div className="rounded-lg border border-danger/40 bg-danger/10 px-3 py-2 text-[12px] text-danger">
              {marketplaceError}
            </div>
          ) : null}
          <div className="flex flex-col divide-y divide-line border border-line rounded-lg overflow-hidden">
            {marketplaceEntries.length > 0 ? (
              marketplaceEntries.map((entry) => {
                const pending = pendingId === `marketplace:${entry.id}`;
                const defaultHash = entry.packageSha256 || entry.checksumSha256;
                // The catalog used to render every row as a bare "Install",
                // even for plugins already installed, so a user could not tell
                // what they had or when a newer version existed. Cross-reference
                // the installed manifests to answer both.
                const installed = plugins.find((p) => p.id === entry.id);
                const updateAvailable =
                  installed !== undefined && hasPluginUpdate(installed.version, entry.version);
                const actionLabel = pending
                  ? updateAvailable
                    ? "Updating"
                    : "Installing"
                  : updateAvailable
                    ? "Update"
                    : installed
                      ? "Reinstall"
                      : "Install";
                return (
                  <article
                    key={`${entry.id}:${entry.version}:${entry.checksumSha256}`}
                    className="grid grid-cols-[minmax(0,1fr)_auto] gap-3 px-3 py-3"
                  >
                    <div className="flex flex-col gap-1 min-w-0">
                      <div className="flex items-center gap-2 min-w-0">
                        <strong className="text-[13px] font-semibold text-text truncate">
                          {entry.name || entry.id}
                        </strong>
                        <code className="text-[11px] font-mono text-text-3">{entry.version}</code>
                        {installed ? (
                          <span
                            className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold ${
                              updateAvailable
                                ? "bg-accent/15 text-accent"
                                : "bg-panel-2 text-text-3"
                            }`}
                            title={
                              updateAvailable
                                ? `Installed ${installed.version}; marketplace has ${entry.version}`
                                : `Installed ${installed.version}`
                            }
                          >
                            {updateAvailable ? `update · has ${installed.version}` : "installed"}
                          </span>
                        ) : null}
                        {/* Trust at a glance. A raw sha proves integrity but says
                            nothing a person can read; whether an entry is signed
                            is the decision they actually make before installing. */}
                        <span
                          className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold ${
                            entry.signedBy
                              ? "bg-accent-2/15 text-accent-2"
                              : "bg-warning/15 text-warning"
                          }`}
                          title={
                            entry.signedBy
                              ? `Signed by ${entry.signedBy}`
                              : "No signature — integrity is checksum-only"
                          }
                        >
                          {entry.signedBy ? "signed" : "unsigned"}
                        </span>
                      </div>
                      <span className="text-[12px] text-text-3 truncate">
                        {entry.description || entry.sourceUrl || entry.sourcePath}
                      </span>
                      <span className="text-[11px] text-text-3 truncate">
                        {entry.providesTools.join(", ") || "no tools"} /{" "}
                        {entry.providesCommands.join(", ") || "no commands"}
                      </span>
                      <span className="text-[11px] text-text-3 truncate">
                        sha {shortHash(entry.checksumSha256)}
                        {entry.packageSha256 ? ` · pkg ${shortHash(entry.packageSha256)}` : ""}
                        {entry.signedBy ? ` · ${entry.signedBy}` : ""}
                      </span>
                    </div>
                    <button
                      type="button"
                      className="inline-flex items-center justify-center gap-2 h-8 px-3 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[12px] cursor-pointer hover:opacity-90 disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3"
                      disabled={marketplaceInstallDisabled || pending}
                      onClick={() =>
                        onInstallMarketplace({
                          ...entry,
                          checksumSha256: entry.checksumSha256 || defaultHash,
                        })
                      }
                    >
                      <Icon name="download" /> {actionLabel}
                    </button>
                  </article>
                );
              })
            ) : (
              <div className="px-3 py-4 text-[12px] text-text-3">No marketplace entries.</div>
            )}
          </div>
        </div>

        <Disclosure label="Advanced: marketplace catalog sync">
          <label className="flex flex-col gap-1 text-[12px] text-text-2">
            Catalog URL
            <input
              className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
              value={marketplaceSyncDraft.catalogUrl}
              onChange={(e) =>
                onMarketplaceSyncDraft({
                  ...marketplaceSyncDraft,
                  catalogUrl: e.target.value,
                })
              }
              placeholder="https://..."
            />
          </label>
          <label className="flex flex-col gap-1 text-[12px] text-text-2">
            Catalog SHA-256
            <input
              className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
              value={marketplaceSyncDraft.expectedSha256}
              onChange={(e) =>
                onMarketplaceSyncDraft({
                  ...marketplaceSyncDraft,
                  expectedSha256: e.target.value,
                })
              }
              placeholder="sha256"
            />
          </label>
          <label className="flex flex-col gap-1 text-[12px] text-text-2">
            Install SHA-256
            <input
              className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
              value={marketplaceInstallDraft.expectedSha256}
              onChange={(e) =>
                onMarketplaceInstallDraft({
                  ...marketplaceInstallDraft,
                  expectedSha256: e.target.value,
                })
              }
              placeholder="optional override"
            />
          </label>
          <div className="grid grid-cols-2 gap-2">
            <Checkbox checked={marketplaceSyncDraft.trustAccepted} onChange={(checked) => onMarketplaceSyncDraft({
                    ...marketplaceSyncDraft,
                    trustAccepted: checked,
                  })} label="Trust catalog" className="inline-flex items-center gap-2 text-[12px] text-text-2" />
            <Checkbox checked={marketplaceSyncDraft.requireSigned} onChange={(checked) => onMarketplaceSyncDraft({
                    ...marketplaceSyncDraft,
                    requireSigned: checked,
                  })} label="Require signed" className="inline-flex items-center gap-2 text-[12px] text-text-2" />
            <Checkbox checked={marketplaceInstallDraft.trustAccepted} onChange={(checked) => onMarketplaceInstallDraft({
                    ...marketplaceInstallDraft,
                    trustAccepted: checked,
                  })} label="Trust install" className="inline-flex items-center gap-2 text-[12px] text-text-2" />
            <Checkbox checked={marketplaceInstallDraft.overwrite} onChange={(checked) => onMarketplaceInstallDraft({
                    ...marketplaceInstallDraft,
                    overwrite: checked,
                  })} label="Overwrite" className="inline-flex items-center gap-2 text-[12px] text-text-2" />
            <Checkbox checked={marketplaceSyncDraft.allowFileUrls || marketplaceInstallDraft.allowFileUrls} onChange={(checked) => {
                  onMarketplaceSyncDraft({
                    ...marketplaceSyncDraft,
                    allowFileUrls: checked,
                  });
                  onMarketplaceInstallDraft({
                    ...marketplaceInstallDraft,
                    allowFileUrls: checked,
                  });
                }} label="Allow file URLs" className="inline-flex items-center gap-2 text-[12px] text-text-2 col-span-2" />
          </div>
          <button
            type="button"
            className="inline-flex items-center justify-center gap-2 h-9 px-4 rounded-lg border border-line bg-transparent text-text-2 font-semibold text-[13px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40"
            disabled={marketplaceSyncDisabled}
            onClick={onSyncMarketplace}
          >
            <Icon name="download" /> {pendingId === "marketplace-sync" ? "Syncing" : "Sync"}
          </button>
        </Disclosure>
      </div>

      <InstalledPluginsList plugins={plugins} pendingId={pendingId} onToggle={onToggle} />
      {plugins.map((plugin) => (
        <PluginContributionHost
          key={`contributions-${plugin.id}`}
          projection={projectDaemonPluginManifest(plugin)}
          panelFallback={() => <p className="text-[12px] text-text-3">No active panel.</p>}
        />
      ))}
    </section>
  );
}
