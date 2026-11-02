import type { DaemonConfig, ExecutionNetworkDefault } from "../types.js";
import { requestDaemonApi } from "../generated/daemon-openapi.js";
import { invoke } from "@tauri-apps/api/core";
import { isTauriHost } from "./messages-stream-readiness.js";
import { QA_BUILD } from "../../qaBuild.js";

export interface SaveExecutionNetworkProfileRequest {
  name?: string;
  default?: ExecutionNetworkDefault;
  allowHosts?: string[];
  allowPrivateNetworks?: boolean;
  denyPrivateNetworks?: boolean;
}

export interface ProviderKeyStatus {
  provider: string;
  configured: boolean;
  path: string;
  source: "env" | "keychain" | "file" | "missing";
}

export interface CliAccountProfileView {
  providerId: string;
  profileId: string;
  label: string;
  signedIn: boolean;
  authMode: "native_cli_account" | "byok_or_external_api" | "unknown";
  accountFingerprint: string | null;
  lastVerifiedAt: string | null;
}
export interface Attachment {
  type: "image";
  data: string;
  mimeType: string;
  /** Optional id for cross-store pairing with `ReferenceObject` (D2). The
   * desktop composer writes a UUID when attaching so the legacy image
   * thumbnail UI and the new chip-strip reference can be removed in lock-step.
   * Older in-flight attachments that pre-date D2 have no id — the chip strip
   * renders the metadata-only entry without a paired remove, which is the
   * safe degradation. */
  id?: string;
  /** Optional filename for chip-strip display; defaults to the legacy
   * "image" label. */
  filename?: string;
  /** Optional size in bytes for chip-strip display. */
  sizeBytes?: number;
}

/** Browser-host persistence key for the user-entered daemon connection (persisted by the connection hook). */
export const DAEMON_CONFIG_KEY = "coderight.daemon";

/**
 * The build-time daemon URL and token: a dev-server / QA convenience. A release bundle reads
 * neither (both conditions are compile-time constants, so the branch is dead-code-eliminated).
 */
export function daemonConfigFromEnv(): DaemonConfig | null {
  const fromEnv =
    isTauriHost() || !(import.meta.env?.DEV || QA_BUILD)
      ? undefined
      : (import.meta.env?.VITE_RIGHTKIT_DAEMON_URL as string | undefined);
  if (!fromEnv) return null;
  return {
    url: fromEnv,
    token: (import.meta.env?.VITE_RIGHTKIT_DAEMON_TOKEN as string | undefined) ?? "",
  };
}

export async function startDaemon(): Promise<DaemonConfig> {
  const info = await invoke<{
    url: string;
    token: string;
    databasePath: string;
  }>("start_daemon");
  return { url: info.url, token: info.token };
}

export async function restartDaemon(): Promise<DaemonConfig> {
  const info = await invoke<{
    url: string;
    token: string;
    databasePath: string;
  }>("restart_daemon");
  return { url: info.url, token: info.token };
}

/** @deprecated CLI providers use native login via `loginCliProvider`. */
export async function getProviderKeyStatus(
  config: DaemonConfig,
  provider = "openrouter",
): Promise<ProviderKeyStatus> {
  return requestDaemonApi<ProviderKeyStatus>(config, "getProviderKeysByProvider", {
    pathParams: { provider },
  });
}

/** @deprecated API-key persistence is rejected by the CLI-only runtime. */
export async function saveProviderKey(
  config: DaemonConfig,
  provider: string,
  apiKey: string,
): Promise<ProviderKeyStatus> {
  return requestDaemonApi<ProviderKeyStatus, { apiKey: string }>(
    config,
    "putProviderKeysByProvider",
    {
      pathParams: { provider },
      body: { apiKey },
    },
  );
}

/** @deprecated CLI logout removes a managed profile instead of an API key. */
export async function removeProviderKey(
  config: DaemonConfig,
  provider: string,
): Promise<ProviderKeyStatus> {
  return requestDaemonApi<ProviderKeyStatus>(config, "deleteProviderKeysByProvider", {
    pathParams: { provider },
  });
}

export async function listCliAccountProfiles(provider: string): Promise<CliAccountProfileView[]> {
  return invoke<CliAccountProfileView[]>("list_cli_account_profiles", { provider });
}

export async function loginCliProvider(
  provider: string,
  label = "default",
): Promise<{
  providerId: string;
  profileId: string;
  status: "started" | "signed_in" | "failed";
  observedAt: string;
}> {
  return invoke("login_cli_provider", { provider, label });
}
export async function getSystemUsername(): Promise<string> {
  return invoke<string>("get_username");
}

export async function getGitBranches(
  workspaceRoot: string,
): Promise<{ current: string; branches: string[] }> {
  return invoke("get_git_branches", { workspaceRoot });
}

export async function switchGitBranch(
  workspaceRoot: string,
  branch: string,
  allowDirty = false,
): Promise<{
  switched: boolean;
  dirty: boolean;
  current: string;
  branches: string[];
  message: string;
}> {
  return invoke("switch_git_branch", { workspaceRoot, branch, allowDirty });
}
