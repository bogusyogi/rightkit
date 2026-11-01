import type { DiagnosticsCheck, DiagnosticsStatus } from "./health-support";
import type { ContentTrust } from "./protocol-core";

export interface TeamDefinition {
  name: string;
  description?: string | null;
  members: string[];
  maxWorkers?: number | null;
  budgetUsd?: number | null;
}

export interface TeamListResponse {
  teamStorePath: string;
  teams: TeamDefinition[];
}

export interface MessageHistoryRow {
  id: string;
  createdAt: string;
  fromSessionId: string;
  targetSessionId?: string | null;
  channel: string;
  message: string;
  deliveryStatus: string;
  deliveredSessionIds: string[];
  error?: string | null;
}

export interface MessageListResponse {
  messageStorePath: string;
  messages: MessageHistoryRow[];
}

export interface MemoryEntryRow {
  id: string;
  tier: "Working" | "Episodic" | "Semantic";
  content: string;
  keywords: string[];
  score: number;
  createdAt: string;
  accessCount: number;
}

export interface MemoryListResponse {
  query: string;
  limit: number;
  count: number;
  entries: MemoryEntryRow[];
}

export interface TelegramRemoteStatus {
  state: string;
  tokenConfigured: boolean;
  allowedUserCount: number;
  longPollRequested: boolean;
  bindingStorePath: string;
  bindingStoreReadable: boolean;
  bindingCount: number;
  bindingStoreError?: string | null;
  runtime?: {
    startedAt?: string | null;
    lastPollAt?: string | null;
    lastSuccessAt?: string | null;
    lastErrorAt?: string | null;
    lastError?: string | null;
    updateOffset: number;
    activeChatCount: number;
    chatCursors: {
      chat: string;
      sessionId: string;
      lastPushedSeq: number;
    }[];
  } | null;
  notes: string[];
}

export interface RemoteGatewayProfile {
  id: string;
  channels: string[];
  toolset: string;
  deliveryPolicy: string;
}

export interface RemoteChannelProfile {
  profileId: string;
  toolset: string;
  deliveryPolicy: string;
  trust: ContentTrust;
}

export interface RemoteChannelStatus {
  state: string;
  configured: boolean;
  bindingCount: number;
  activeTargetCount: number;
  lastSuccessAt?: string | null;
  lastErrorAt?: string | null;
  lastError?: string | null;
}

export interface RemoteChannelDeliverySummary {
  id: string;
  bindingId?: string | null;
  targetRedacted: string;
  status: string;
  deliveryKind: string;
  error?: string | null;
  createdAt: string;
}

export interface RemoteChannelDescriptor {
  id: string;
  kind: string;
  enabled: boolean;
  configured: boolean;
  profile: RemoteChannelProfile;
  status: RemoteChannelStatus;
  recentDeliveries: RemoteChannelDeliverySummary[];
  notes: string[];
}

export interface RemoteChannelsSnapshot {
  profiles: RemoteGatewayProfile[];
  channels: RemoteChannelDescriptor[];
}

export interface RemoteChannelProfileRequest {
  profileId?: string | null;
  toolset?: string | null;
  deliveryPolicy?: string | null;
  trust?: string | null;
  enabled?: boolean | null;
}

export interface RemoteChannelProfileResponse {
  ok: boolean;
  channel?: RemoteChannelDescriptor | null;
  error?: string | null;
}

export interface RemoteChannelFanoutRequest {
  text: string;
  source?: string | null;
  targetKey?: string | null;
  sessionId?: string | null;
}

export interface RemoteChannelFanoutResult {
  bindingId: string;
  sessionId: string;
  targetRedacted: string;
  status: string;
  seq?: number | null;
  error?: string | null;
}

export interface RemoteChannelFanoutResponse {
  ok: boolean;
  channelId: string;
  attempted: number;
  delivered: number;
  failed: number;
  results: RemoteChannelFanoutResult[];
  error?: string | null;
}

export interface RemoteChannelOutboundTarget {
  target: string;
  allowPrivateNetwork?: boolean;
}

export interface RemoteChannelOutboundRequest {
  text: string;
  source?: string | null;
  deliveryKind?: string | null;
  sessionId?: string | null;
  targets: RemoteChannelOutboundTarget[];
}

export interface RemoteChannelOutboundResult {
  id: string;
  targetRedacted: string;
  status: string;
  deliveryKind: string;
  error?: string | null;
  createdAt: string;
}

export interface RemoteChannelOutboundResponse {
  ok: boolean;
  channelId: string;
  attempted: number;
  delivered: number;
  failed: number;
  results: RemoteChannelOutboundResult[];
  error?: string | null;
}

export interface TelegramRemoteTest {
  verdict: DiagnosticsStatus;
  state: string;
  tokenConfigured: boolean;
  allowedUserCount: number;
  bindingStoreReadable: boolean;
  bindingCount: number;
  runtime?: {
    startedAt?: string | null;
    lastPollAt?: string | null;
    lastSuccessAt?: string | null;
    lastErrorAt?: string | null;
    lastError?: string | null;
    updateOffset: number;
    activeChatCount: number;
  } | null;
  checks: DiagnosticsCheck[];
  notes: string[];
}

export interface TelegramRemoteUserRow {
  userId: number;
  username?: string | null;
  firstName?: string | null;
  lastName?: string | null;
  chatId?: number | null;
  chatType?: string | null;
  lastUpdateId: number;
}

export interface TelegramRemoteUsers {
  verdict: DiagnosticsStatus;
  telegramTokenConfigured: boolean;
  detail?: string | null;
  error?: string | null;
  userCount: number;
  users: TelegramRemoteUserRow[];
  notes?: string[];
}

export interface NotificationRow {
  id: string;
  kind: string;
  title: string;
  body: string;
  source: string;
  scheduleId?: string | null;
  sessionId?: string | null;
  createdAt: string;
  read: boolean;
}

export interface NotificationListResponse {
  workspaceRoot: string;
  notificationStorePath: string;
  unreadCount: number;
  notifications: NotificationRow[];
}

export interface NotificationDismissResponse {
  dismissed: boolean;
  notification: NotificationRow;
}
