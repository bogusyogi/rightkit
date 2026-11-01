import { hostAppName } from "../../../appName";
export type RecoveryVariant =
  | "local-unavailable"
  | "remote-unreachable"
  | "remote-incompatible"
  | "remote-wrong-service"
  | "remote-missing"
  | "missing-default-host";

export type RecoveryPrimaryAction = "use-local" | "use-remote";

export type RecoveryNextStep =
  | { kind: "local-setup" }
  | { kind: "switch-default-to-local" }
  | { kind: "remote-form" };

export type RecoveryIcon = "local" | "remote";

export interface RecoveryConfig {
  title: string;
  description: string;
  iconKey: RecoveryIcon;
  showRetry: boolean;
  retryLabel: string;
  showUseLocal: boolean;
  showUseRemote: boolean;
  useLocalLabel: string;
  useRemoteLabel: string;
}

function redactHost(value: string): string {
  const trimmed = value.trim();
  if (!trimmed) return "";

  try {
    const url = new URL(trimmed);
    const credentialsRemoved = `${url.protocol}//${url.hostname}${url.port ? `:${url.port}` : ""}${url.pathname}`;
    return url.search || url.hash ? `${credentialsRemoved}?REDACTED` : credentialsRemoved;
  } catch {
    return trimmed;
  }
}

export function formatRecoveryHost(hostLabel?: string, hostUrl?: string): string | undefined {
  if (hostLabel?.trim()) return redactHost(hostLabel);
  if (hostUrl) return redactHost(hostUrl);
  return undefined;
}

export function getRecoveryConfig(
  variant: RecoveryVariant,
  hostLabel?: string,
  hostUrl?: string,
): RecoveryConfig {
  const host = formatRecoveryHost(hostLabel, hostUrl);
  const remoteName = host || "the remote server";
  const unknownRemote = host || "unknown";

  switch (variant) {
    case "local-unavailable":
      return {
        title: `Local ${hostAppName()} Engine Unavailable`,
        description: `The local ${hostAppName()} daemon could not be started. Retry the local engine or connect to a remote server instead.`,
        iconKey: "local",
        showRetry: true,
        retryLabel: "Retry Local",
        showUseLocal: true,
        showUseRemote: true,
        useLocalLabel: "Set Up Local",
        useRemoteLabel: "Use Remote",
      };
    case "remote-missing":
    case "missing-default-host":
      return {
        title: "No Default Connection",
        description: "Your saved default connection could not be found. Choose how you want to connect.",
        iconKey: "local",
        showRetry: false,
        retryLabel: "Retry Connection",
        showUseLocal: true,
        showUseRemote: true,
        useLocalLabel: "Use Local",
        useRemoteLabel: "Use Remote",
      };
    case "remote-unreachable":
      return {
        title: "Remote Server Unreachable",
        description: `Could not connect to \"${remoteName}\". Check your network connection and verify the server address.`,
        iconKey: "remote",
        showRetry: true,
        retryLabel: "Retry Connection",
        showUseLocal: true,
        showUseRemote: true,
        useLocalLabel: "Use Local",
        useRemoteLabel: "Use Remote",
      };
    case "remote-wrong-service":
      return {
        title: "Incompatible Server",
        description: `The server at \"${unknownRemote}\" is not running a compatible ${hostAppName()} daemon. Verify the address points to a ${hostAppName()} server.`,
        iconKey: "remote",
        showRetry: false,
        retryLabel: "Retry Connection",
        showUseLocal: true,
        showUseRemote: true,
        useLocalLabel: "Use Local",
        useRemoteLabel: "Use Remote",
      };
    case "remote-incompatible":
      return {
        title: "Server Update Required",
        description: `The ${hostAppName()} server at \"${unknownRemote}\" is not compatible with this app version. Update the server, then try again.`,
        iconKey: "remote",
        showRetry: true,
        retryLabel: "Retry Connection",
        showUseLocal: true,
        showUseRemote: true,
        useLocalLabel: "Use Local",
        useRemoteLabel: "Use Remote",
      };
    default: {
      const exhaustive: never = variant;
      throw new Error(`Unhandled recovery variant: ${exhaustive}`);
    }
  }
}

export function resolveRecoveryNextStep(
  variant: RecoveryVariant,
  action: RecoveryPrimaryAction,
): RecoveryNextStep {
  if (action === "use-remote") return { kind: "remote-form" };

  switch (variant) {
    case "local-unavailable":
      return { kind: "local-setup" };
    case "remote-unreachable":
    case "remote-incompatible":
    case "remote-wrong-service":
    case "remote-missing":
    case "missing-default-host":
      return { kind: "switch-default-to-local" };
    default: {
      const exhaustive: never = variant;
      throw new Error(`Unhandled recovery variant: ${exhaustive}`);
    }
  }
}

/** Donor-compatible name used by OpenChamber's recovery screen. */
export const getDesktopRecoveryConfig = getRecoveryConfig;
