import { invoke } from "@tauri-apps/api/core";

/** Live remote gateway status returned by the native pairing command. */
export interface RemoteStatus {
  running: boolean;
  port: number | null;
  tlsActive: boolean;
  fingerprint: string | null;
  pairingActive: boolean;
  devices: number;
}

/** A paired device, deliberately excluding its token or token hash. */
export interface RemoteDevice {
  id: string;
  name: string;
  platform: string;
  createdAt: number;
  lastSeenAt: number;
  revoked: boolean;
  connected?: boolean;
}

/** A desktop host this instance may connect to. */
export interface RemoteHost {
  id: string;
  hostId: string;
  hostname: string;
  displayName: string;
  fingerprint: string;
  candidates: string[];
  createdAt: number;
  lastSeenAt: number;
  alias?: string;
}

/** One-time pairing material. The code is displayed once by the UI. */
export interface PairingBegin {
  code: string;
  expiresAt: number;
  qrPayload: string;
}

export interface RemoteHostPairRequest {
  host: string;
  fingerprint: string;
  code: string;
  candidates?: string[];
}

export interface RemoteProjectSnapshot {
  hostId: string;
  projects: unknown;
  icons?: unknown;
}

/**
 * Thin native-command boundary, ported from Aethon's remote service. The
 * native/daemon implementation owns pairing state and tokens; this module
 * only carries typed payloads across the Tauri boundary.
 */
export interface RemoteClient {
  remoteStatus(): Promise<RemoteStatus>;
  remotePairingBegin(): Promise<PairingBegin>;
  remotePairingCancel(): Promise<void>;
  remoteDevicesList(): Promise<RemoteDevice[]>;
  remoteDeviceRevoke(id: string): Promise<void>;
  remoteDeviceRename(id: string, name: string): Promise<void>;
  remoteHostsList(): Promise<RemoteHost[]>;
  remoteHostPair(request: RemoteHostPairRequest): Promise<RemoteHost>;
  remoteHostProjectSnapshot(id: string): Promise<RemoteProjectSnapshot>;
  remoteHostForget(id: string): Promise<void>;
  remoteHostRename(id: string, name: string): Promise<void>;
  remoteHostReconnect(id: string): Promise<void>;
  remoteHostInvoke<T = unknown>(
    id: string,
    cmd: string,
    args?: Record<string, unknown>,
  ): Promise<T>;
}

function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return args === undefined ? invoke<T>(command) : invoke<T>(command, args);
}

export function remoteStatus(): Promise<RemoteStatus> {
  return call<RemoteStatus>("remote_status");
}

export function remotePairingBegin(): Promise<PairingBegin> {
  return call<PairingBegin>("remote_pairing_begin");
}

export function remotePairingCancel(): Promise<void> {
  return call<void>("remote_pairing_cancel");
}

export function remoteDevicesList(): Promise<RemoteDevice[]> {
  return call<RemoteDevice[]>("remote_devices_list");
}

export function remoteDeviceRevoke(id: string): Promise<void> {
  return call<void>("remote_device_revoke", { id });
}

export function remoteDeviceRename(id: string, name: string): Promise<void> {
  return call<void>("remote_device_rename", { id, name });
}

export function remoteHostsList(): Promise<RemoteHost[]> {
  return call<RemoteHost[]>("remote_hosts_list");
}

export function remoteHostPair(request: RemoteHostPairRequest): Promise<RemoteHost> {
  return call<RemoteHost>("remote_host_pair", {
    host: request.host,
    fingerprint: request.fingerprint,
    code: request.code,
    ...(request.candidates === undefined ? {} : { candidates: request.candidates }),
  });
}

export function remoteHostProjectSnapshot(id: string): Promise<RemoteProjectSnapshot> {
  return call<RemoteProjectSnapshot>("remote_host_project_snapshot", { id });
}

export function remoteHostForget(id: string): Promise<void> {
  return call<void>("remote_host_forget", { id });
}

export function remoteHostRename(id: string, name: string): Promise<void> {
  return call<void>("remote_host_rename", { id, name });
}

export function remoteHostReconnect(id: string): Promise<void> {
  return call<void>("remote_host_reconnect", { id });
}

export function remoteHostInvoke<T = unknown>(
  id: string,
  cmd: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  return call<T>("remote_host_invoke", { id, cmd, args });
}

export const remoteClient: RemoteClient = {
  remoteStatus,
  remotePairingBegin,
  remotePairingCancel,
  remoteDevicesList,
  remoteDeviceRevoke,
  remoteDeviceRename,
  remoteHostsList,
  remoteHostPair,
  remoteHostProjectSnapshot,
  remoteHostForget,
  remoteHostRename,
  remoteHostReconnect,
  remoteHostInvoke,
};

/** Compact identity display that retains both ends of a fingerprint. */
export function shortRemoteFingerprint(fingerprint: string): string {
  return fingerprint.length > 16
    ? `${fingerprint.slice(0, 8)}…${fingerprint.slice(-8)}`
    : fingerprint;
}

/** Humanized age for the reachability/device projections. */
export function remoteRelativeTime(epochMs: number, now = Date.now()): string {
  const delta = Math.max(0, now - epochMs);
  if (delta < 60_000) return "just now";
  const minutes = Math.floor(delta / 60_000);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

/** Parse the multiline/comma-separated candidate field without duplicates. */
export function parseRemoteCandidates(value: string): string[] {
  return [...new Set(value.split(/[\n,]/).map((candidate) => candidate.trim()).filter(Boolean))];
}
