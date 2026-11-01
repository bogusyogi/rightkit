import type {
  DaemonConfig,
  MemoryListResponse,
  MessageListResponse,
  NotificationDismissResponse,
  NotificationListResponse,
  OrchestrationSettings,
  PluginInstallResponse,
  PluginListResponse,
  PluginMarketplaceListResponse,
  PluginMarketplaceSyncResponse,
  RemoteChannelFanoutRequest,
  RemoteChannelFanoutResponse,
  RemoteChannelOutboundRequest,
  RemoteChannelOutboundResponse,
  RemoteChannelProfileRequest,
  RemoteChannelProfileResponse,
  RemoteChannelsSnapshot,
  SaveToolsetRequest,
  SettingsConfigStatus,
  SettingsReloadResponse,
  SettingsToolsetsResponse,
  TeamListResponse,
  TelegramRemoteStatus,
  TelegramRemoteTest,
  TelegramRemoteUsers,
} from "../types";
import type { InferenceSelection } from "../types/inference";
import { requestDaemonApi } from "../generated/daemon-openapi";

export async function getOrchestrationSettings(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<OrchestrationSettings> {
  return requestDaemonApi<OrchestrationSettings>(config, "getSettingsOrchestration", {
    query: { workspaceRoot },
  });
}

export async function getSettingsConfigStatus(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<SettingsConfigStatus> {
  return requestDaemonApi<SettingsConfigStatus>(config, "getSettingsConfigStatus", {
    query: { workspaceRoot },
  });
}

export async function reloadSettingsConfig(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<SettingsReloadResponse> {
  return requestDaemonApi<SettingsReloadResponse>(config, "postSettingsReload", {
    query: { workspaceRoot },
  });
}

export async function getSettingsToolsets(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<SettingsToolsetsResponse> {
  return requestDaemonApi<SettingsToolsetsResponse>(config, "getSettingsToolsets", {
    query: { workspaceRoot },
  });
}

export async function saveSettingsToolset(
  config: DaemonConfig,
  workspaceRoot: string,
  id: string,
  toolset: SaveToolsetRequest,
): Promise<SettingsToolsetsResponse> {
  await requestDaemonApi(config, "putSettingsToolsetsById", {
    pathParams: { id },
    query: { workspaceRoot },
    body: toolset,
  });
  return getSettingsToolsets(config, workspaceRoot);
}

export async function deleteSettingsToolset(
  config: DaemonConfig,
  workspaceRoot: string,
  id: string,
): Promise<SettingsToolsetsResponse> {
  await requestDaemonApi(config, "deleteSettingsToolsetsById", {
    pathParams: { id },
    query: { workspaceRoot },
  });
  return getSettingsToolsets(config, workspaceRoot);
}

export async function saveOrchestrationSettings(
  config: DaemonConfig,
  settings: Pick<
    OrchestrationSettings,
    "workspaceRoot" | "primarySelection" | "budget" | "benchmarks"
  > & {
    roleSelections?: Record<string, InferenceSelection>;
    roleModelMode?: OrchestrationSettings["roleModelMode"];
    remote?: {
      telegramToken?: string;
      clearTelegramToken?: boolean;
      telegramAllowedUsers?: number[];
      webEnabled?: boolean;
    };
    routing?: OrchestrationSettings["routing"];
    keepDaemonResident?: boolean;
  },
): Promise<void> {
  await requestDaemonApi(config, "putSettingsOrchestration", {
    body: settings,
  });
}

export async function listPlugins(config: DaemonConfig): Promise<PluginListResponse> {
  return requestDaemonApi<PluginListResponse>(config, "getPlugins");
}

export async function reloadPlugins(
  config: DaemonConfig,
): Promise<PluginListResponse & { reloaded: boolean; count: number }> {
  return requestDaemonApi<PluginListResponse & { reloaded: boolean; count: number }>(
    config,
    "postPluginsReload",
  );
}

export async function installPlugin(
  config: DaemonConfig,
  sourcePath: string,
  trustAccepted: boolean,
  overwrite: boolean,
): Promise<PluginInstallResponse> {
  return requestDaemonApi<
    PluginInstallResponse,
    { sourcePath: string; trustAccepted: boolean; overwrite: boolean }
  >(config, "postPluginsInstall", {
    body: {
      sourcePath,
      trustAccepted,
      overwrite,
    },
  });
}

export async function listPluginMarketplace(
  config: DaemonConfig,
  query = "",
): Promise<PluginMarketplaceListResponse> {
  return requestDaemonApi<PluginMarketplaceListResponse>(config, "getPluginsMarketplace", {
    query: { query: query.trim() || undefined },
  });
}

export async function syncPluginMarketplace(
  config: DaemonConfig,
  body: {
    catalogUrl: string;
    expectedSha256: string;
    trustAccepted: boolean;
    requireSigned: boolean;
    allowFileUrls: boolean;
  },
): Promise<PluginMarketplaceSyncResponse> {
  return requestDaemonApi<PluginMarketplaceSyncResponse, typeof body>(
    config,
    "postPluginsMarketplaceSync",
    { body },
  );
}

export async function installMarketplacePlugin(
  config: DaemonConfig,
  pluginId: string,
  body: {
    trustAccepted: boolean;
    expectedSha256?: string;
    overwrite: boolean;
    allowFileUrls: boolean;
  },
): Promise<PluginInstallResponse> {
  return requestDaemonApi<PluginInstallResponse, typeof body>(
    config,
    "postPluginsMarketplaceByIdInstall",
    { pathParams: { id: pluginId }, body },
  );
}

export async function setPluginEnabled(
  config: DaemonConfig,
  pluginId: string,
  enabled: boolean,
): Promise<void> {
  await requestDaemonApi(config, enabled ? "postPluginsByIdEnable" : "postPluginsByIdDisable", {
    pathParams: { id: pluginId },
  });
}

export async function listTeams(config: DaemonConfig): Promise<TeamListResponse> {
  return requestDaemonApi<TeamListResponse>(config, "getTeams");
}

export async function listMessages(config: DaemonConfig): Promise<MessageListResponse> {
  return requestDaemonApi<MessageListResponse>(config, "getMessages");
}

export async function listMemory(
  config: DaemonConfig,
  query = "",
  limit = 20,
): Promise<MemoryListResponse> {
  return requestDaemonApi<MemoryListResponse>(config, "getMemory", {
    query: { query: query.trim() || undefined, limit },
  });
}

export async function addMemory(
  config: DaemonConfig,
  content: string,
  keywords: string[] = [],
): Promise<{ added: boolean; entry: MemoryListResponse["entries"][number] }> {
  return requestDaemonApi<
    { added: boolean; entry: MemoryListResponse["entries"][number] },
    { content: string; keywords: string[] }
  >(config, "postMemory", { body: { content, keywords } });
}

export async function getTelegramStatus(config: DaemonConfig): Promise<TelegramRemoteStatus> {
  return requestDaemonApi<TelegramRemoteStatus>(config, "getRemoteTelegramStatus");
}

export async function getRemoteChannels(config: DaemonConfig): Promise<RemoteChannelsSnapshot> {
  return requestDaemonApi<RemoteChannelsSnapshot>(config, "getRemoteChannels");
}

export async function saveRemoteChannelProfile(
  config: DaemonConfig,
  channelId: string,
  body: RemoteChannelProfileRequest,
): Promise<RemoteChannelProfileResponse> {
  return requestDaemonApi<RemoteChannelProfileResponse, RemoteChannelProfileRequest>(
    config,
    "postRemoteChannelsByIdProfile",
    { pathParams: { id: channelId }, body },
  );
}

export async function fanoutRemoteChannelMessage(
  config: DaemonConfig,
  channelId: string,
  body: RemoteChannelFanoutRequest,
): Promise<RemoteChannelFanoutResponse> {
  return requestDaemonApi<RemoteChannelFanoutResponse, RemoteChannelFanoutRequest>(
    config,
    "postRemoteChannelsByIdFanout",
    { pathParams: { id: channelId }, body },
  );
}

export async function sendRemoteChannelOutbound(
  config: DaemonConfig,
  channelId: string,
  body: RemoteChannelOutboundRequest,
): Promise<RemoteChannelOutboundResponse> {
  return requestDaemonApi<RemoteChannelOutboundResponse, RemoteChannelOutboundRequest>(
    config,
    "postRemoteChannelsByIdOutbound",
    { pathParams: { id: channelId }, body },
  );
}

export async function getTelegramTest(config: DaemonConfig): Promise<TelegramRemoteTest> {
  return requestDaemonApi<TelegramRemoteTest>(config, "getRemoteTelegramTest");
}

export async function getTelegramUsers(
  config: DaemonConfig,
  limit = 20,
): Promise<TelegramRemoteUsers> {
  return requestDaemonApi<TelegramRemoteUsers>(config, "getRemoteTelegramUsers", {
    query: { limit },
  });
}

export async function listNotifications(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<NotificationListResponse> {
  return requestDaemonApi<NotificationListResponse>(config, "getNotifications", {
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
  });
}

export async function dismissNotification(
  config: DaemonConfig,
  workspaceRoot: string,
  id: string,
): Promise<NotificationDismissResponse> {
  return requestDaemonApi<NotificationDismissResponse>(config, "postNotificationsByIdDismiss", {
    pathParams: { id },
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
  });
}
