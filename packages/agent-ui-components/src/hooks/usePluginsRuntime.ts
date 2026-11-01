import { useCallback, useEffect, useRef, useState } from "react";
import type {
  PluginInstallDraft,
  PluginMarketplaceInstallDraft,
  PluginMarketplaceSyncDraft,
} from "../components/settings/types";
import {
  installMarketplacePlugin,
  installPlugin,
  listPluginMarketplace,
  listPlugins,
  reloadPlugins,
  setPluginEnabled,
  syncPluginMarketplace,
} from "@rightkit/agent-ui-core/lib/daemon";
import type { DaemonConfig, PluginMarketplaceEntry, PluginManifest } from "@rightkit/agent-ui-core/lib/types";

interface UsePluginsRuntimeOptions {
  config: DaemonConfig | null;
  formatError: (error: unknown, fallback: string) => string;
  onPickDirectory: () => Promise<string | null>;
  onStatus: (status: string) => void;
}

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

/**
 * Compare the three numeric semver components used by the daemon marketplace.
 * Invalid components fail closed as equal, so a malformed catalog entry never
 * gets presented as an available update.
 *
 * This is the small, dependency-free comparison used by Hermes IDE's plugin
 * update checker; keeping it here lets the renderer and its state summary use
 * the same update decision without moving marketplace policy into the daemon.
 */
export function comparePluginVersions(a: string, b: string): -1 | 0 | 1 {
  const parsedA = a.split(".").map(Number);
  const parsedB = b.split(".").map(Number);
  for (let index = 0; index < 3; index += 1) {
    const left = parsedA[index] ?? 0;
    const right = parsedB[index] ?? 0;
    if (!Number.isFinite(left) || !Number.isFinite(right)) return 0;
    if (left > right) return 1;
    if (left < right) return -1;
  }
  return 0;
}

/** Return true only when the catalog contains a newer plugin version. */
export function hasPluginUpdate(current: string, catalog: string): boolean {
  return comparePluginVersions(current, catalog) < 0;
}

export function usePluginsRuntime({
  config,
  formatError,
  onPickDirectory,
  onStatus,
}: UsePluginsRuntimeOptions) {
  const [pluginDir, setPluginDir] = useState("");
  const [plugins, setPlugins] = useState<PluginManifest[]>([]);
  const [pluginActionPending, setPluginActionPending] = useState<string | null>(null);
  const [pluginInstallDraft, setPluginInstallDraft] = useState<PluginInstallDraft>(
    EMPTY_PLUGIN_INSTALL_DRAFT,
  );
  const [pluginMarketplacePath, setPluginMarketplacePath] = useState("");
  const [pluginMarketplaceEntries, setPluginMarketplaceEntries] = useState<
    PluginMarketplaceEntry[]
  >([]);
  const [pluginMarketplaceQuery, setPluginMarketplaceQuery] = useState("");
  const [pluginMarketplaceError, setPluginMarketplaceError] = useState<string | null>(null);
  const [pluginMarketplaceSyncDraft, setPluginMarketplaceSyncDraft] =
    useState<PluginMarketplaceSyncDraft>(EMPTY_MARKETPLACE_SYNC_DRAFT);
  const [pluginMarketplaceInstallDraft, setPluginMarketplaceInstallDraft] =
    useState<PluginMarketplaceInstallDraft>(EMPTY_MARKETPLACE_INSTALL_DRAFT);
  // The settings effect can start a new read while an older daemon request is
  // still in flight (notably after a search or daemon reconnect). Only the
  // newest response may replace the renderer projection.
  const pluginLoadEpoch = useRef(0);
  const marketplaceLoadEpoch = useRef(0);
  const actionLock = useRef<string | null>(null);
  const configKey = config ? `${config.url}\u0000${config.token}` : "";
  const configKeyRef = useRef(configKey);
  const previousConfigKey = useRef(configKey);
  configKeyRef.current = configKey;

  useEffect(() => {
    pluginLoadEpoch.current += 1;
    marketplaceLoadEpoch.current += 1;
    if (previousConfigKey.current !== configKey) {
      setPluginDir("");
      setPlugins([]);
      setPluginMarketplacePath("");
      setPluginMarketplaceEntries([]);
      setPluginMarketplaceError(null);
      previousConfigKey.current = configKey;
    }
  }, [configKey]);

  const beginAction = useCallback((id: string): boolean => {
    if (actionLock.current !== null) return false;
    actionLock.current = id;
    setPluginActionPending(id);
    return true;
  }, []);

  const endAction = useCallback((id: string) => {
    if (actionLock.current === id) actionLock.current = null;
    setPluginActionPending((pending) => (pending === id ? null : pending));
  }, []);

  const loadPlugins = useCallback(async () => {
    if (!config) return;
    const epoch = ++pluginLoadEpoch.current;
    const requestedConfigKey = configKey;
    try {
      const next = await listPlugins(config);
      if (epoch !== pluginLoadEpoch.current || configKeyRef.current !== requestedConfigKey) return;
      setPluginDir(next.pluginDir);
      setPlugins(next.plugins);
    } catch (err) {
      if (epoch === pluginLoadEpoch.current && configKeyRef.current === requestedConfigKey) {
        onStatus(formatError(err, "Plugins failed"));
      }
    }
  }, [config, configKey, formatError, onStatus]);

  const loadPluginMarketplace = useCallback(
    async (query = pluginMarketplaceQuery) => {
      if (!config) return;
      const epoch = ++marketplaceLoadEpoch.current;
      const requestedConfigKey = configKey;
      try {
        setPluginMarketplaceError(null);
        const next = await listPluginMarketplace(config, query);
        if (
          epoch !== marketplaceLoadEpoch.current ||
          configKeyRef.current !== requestedConfigKey
        ) {
          return;
        }
        setPluginMarketplacePath(next.marketplacePath);
        setPluginMarketplaceEntries(next.plugins);
      } catch (err) {
        if (
          epoch !== marketplaceLoadEpoch.current ||
          configKeyRef.current !== requestedConfigKey
        ) {
          return;
        }
        const message = formatError(err, "Plugin marketplace failed");
        setPluginMarketplaceError(message);
        onStatus(message);
      }
    },
    [config, configKey, formatError, onStatus, pluginMarketplaceQuery],
  );

  const refreshPlugins = useCallback(async () => {
    if (!config || !beginAction("refresh")) return;
    try {
      const next = await reloadPlugins(config);
      setPluginDir(next.pluginDir);
      setPlugins(next.plugins);
      await loadPluginMarketplace();
      onStatus(`Reloaded ${next.count} plugins`);
    } catch (err) {
      onStatus(formatError(err, "Plugin reload failed"));
    } finally {
      endAction("refresh");
    }
  }, [beginAction, config, endAction, formatError, loadPluginMarketplace, onStatus]);

  const pickPluginSource = useCallback(async () => {
    const picked = await onPickDirectory();
    if (picked) {
      setPluginInstallDraft((draft) => ({ ...draft, sourcePath: picked }));
    }
  }, [onPickDirectory]);

  const submitPluginInstall = useCallback(async () => {
    if (!config) return;
    const sourcePath = pluginInstallDraft.sourcePath.trim();
    if (!sourcePath) {
      onStatus("Choose a plugin folder first");
      return;
    }
    if (!pluginInstallDraft.trustAccepted) {
      onStatus("Confirm plugin trust before installing");
      return;
    }
    if (!beginAction("install")) return;
    try {
      const installed = await installPlugin(
        config,
        sourcePath,
        pluginInstallDraft.trustAccepted,
        pluginInstallDraft.overwrite,
      );
      await loadPlugins();
      onStatus(
        `${installed.overwritten ? "Updated" : "Installed"} ${installed.plugin.name || installed.plugin.id} ${installed.plugin.version}`,
      );
      setPluginInstallDraft(EMPTY_PLUGIN_INSTALL_DRAFT);
    } catch (err) {
      onStatus(formatError(err, "Plugin install failed"));
    } finally {
      endAction("install");
    }
  }, [beginAction, config, endAction, formatError, loadPlugins, onStatus, pluginInstallDraft]);

  const submitPluginMarketplaceSync = useCallback(async () => {
    if (!config) return;
    const catalogUrl = pluginMarketplaceSyncDraft.catalogUrl.trim();
    const expectedSha256 = pluginMarketplaceSyncDraft.expectedSha256.trim();
    if (!catalogUrl || !expectedSha256) {
      onStatus("Marketplace sync needs a catalog URL and SHA-256");
      return;
    }
    if (!pluginMarketplaceSyncDraft.trustAccepted) {
      onStatus("Confirm marketplace catalog trust before syncing");
      return;
    }
    if (!beginAction("marketplace-sync")) return;
    try {
      // The same explicit checkbox is the daemon's required second factor for
      // file:// catalogs. Keep the wire field here even though older generated
      // client types do not declare it yet; requestDaemonApi forwards it as-is.
      const syncRequest = {
        ...pluginMarketplaceSyncDraft,
        catalogUrl,
        expectedSha256,
        fileUrlTrustAccepted: pluginMarketplaceSyncDraft.allowFileUrls,
      };
      const synced = await syncPluginMarketplace(config, syncRequest);
      await loadPluginMarketplace();
      onStatus(`Synced ${synced.pluginCount} marketplace plugins`);
      setPluginMarketplaceSyncDraft(EMPTY_MARKETPLACE_SYNC_DRAFT);
    } catch (err) {
      onStatus(formatError(err, "Marketplace sync failed"));
    } finally {
      endAction("marketplace-sync");
    }
  }, [
    beginAction,
    config,
    endAction,
    formatError,
    loadPluginMarketplace,
    onStatus,
    pluginMarketplaceSyncDraft,
  ]);

  const submitMarketplacePluginInstall = useCallback(
    async (entry: PluginMarketplaceEntry) => {
      if (!config) return;
      const expectedSha256 =
        pluginMarketplaceInstallDraft.expectedSha256.trim() ||
        entry.packageSha256 ||
        entry.checksumSha256;
      if (!expectedSha256.trim()) {
        onStatus("Marketplace entry is missing a package checksum");
        return;
      }
      if (!pluginMarketplaceInstallDraft.trustAccepted) {
        onStatus("Confirm marketplace plugin trust before installing");
        return;
      }
      const actionId = `marketplace:${entry.id}`;
      if (!beginAction(actionId)) return;
      try {
        // File URLs require a separately named acknowledgement in the daemon
        // request. The checkbox is the user's explicit acknowledgement.
        const installRequest = {
          ...pluginMarketplaceInstallDraft,
          expectedSha256,
          fileUrlTrustAccepted: pluginMarketplaceInstallDraft.allowFileUrls,
        };
        const installed = await installMarketplacePlugin(config, entry.id, installRequest);
        await loadPlugins();
        await loadPluginMarketplace();
        onStatus(
          `${installed.overwritten ? "Updated" : "Installed"} ${installed.plugin.name || installed.plugin.id} from marketplace`,
        );
        setPluginMarketplaceInstallDraft(EMPTY_MARKETPLACE_INSTALL_DRAFT);
      } catch (err) {
        onStatus(formatError(err, "Marketplace install failed"));
      } finally {
        endAction(actionId);
      }
    },
    [
      beginAction,
      config,
      endAction,
      formatError,
      loadPluginMarketplace,
      loadPlugins,
      onStatus,
      pluginMarketplaceInstallDraft,
    ],
  );

  const togglePlugin = useCallback(
    async (plugin: PluginManifest) => {
      if (!config || !beginAction(plugin.id)) return;
      try {
        await setPluginEnabled(config, plugin.id, !plugin.enabled);
        await loadPlugins();
        onStatus(plugin.enabled ? "Plugin disabled" : "Plugin enabled");
      } catch (err) {
        onStatus(formatError(err, "Plugin update failed"));
      } finally {
        endAction(plugin.id);
      }
    },
    [beginAction, config, endAction, formatError, loadPlugins, onStatus],
  );

  return {
    pluginDir,
    plugins,
    pluginActionPending,
    pluginInstallDraft,
    setPluginInstallDraft,
    pluginMarketplacePath,
    pluginMarketplaceEntries,
    pluginMarketplaceQuery,
    setPluginMarketplaceQuery,
    pluginMarketplaceError,
    pluginMarketplaceSyncDraft,
    setPluginMarketplaceSyncDraft,
    pluginMarketplaceInstallDraft,
    setPluginMarketplaceInstallDraft,
    loadPlugins,
    loadPluginMarketplace,
    refreshPlugins,
    pickPluginSource,
    submitPluginInstall,
    submitPluginMarketplaceSync,
    submitMarketplacePluginInstall,
    togglePlugin,
  };
}

export type UsePluginsRuntimeReturn = ReturnType<typeof usePluginsRuntime>;
