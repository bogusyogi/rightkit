import type { RecoveryVariant } from "./recovery/recoveryPolicy";
import { hostAppName } from "../../appName";

export type ConnectionTarget = "local" | "remote";
export type ConnectionErrorKind =
  | "unreachable"
  | "unauthorized"
  | "wrong-service"
  | "incompatible"
  | "missing"
  | "superseded"
  | "unknown";

export interface ConnectionErrorProjection {
  kind: ConnectionErrorKind;
  message: string;
  detail: string;
  retryable: boolean;
  stale: boolean;
  recoveryVariant: RecoveryVariant | null;
}

function rawMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error ?? "Unknown connection error");
}

function safeMessage(error: unknown): string {
  return rawMessage(error)
    .replace(/([?&](?:token|access_token|api[_-]?key|secret|password)=)[^&#\s]*/gi, "$1REDACTED")
    .replace(/(https?:\/\/)([^\s/@:]+):([^\s/@]+)@/gi, "$1REDACTED:REDACTED@");
}

export function isAuthRejectionError(error: unknown): boolean {
  return /^40[13]:/.test(rawMessage(error));
}

export function isMissingHealthEndpointError(error: unknown): boolean {
  const message = rawMessage(error);
  return /^404:/.test(message) || message.includes("endpoint is likely missing");
}

export function isGatedMissingHealthError(error: unknown): boolean {
  return isAuthRejectionError(error) && rawMessage(error).includes("no_cookie");
}

export function makeReauthRequiredError(detail?: string): Error {
  const error = new Error(
    "Your remote gateway session has expired. Open the connection settings and sign in again.",
  ) as Error & { isReauthRequired: boolean; needsOauthLogin: boolean; detail?: string };
  error.isReauthRequired = true;
  error.needsOauthLogin = true;
  if (detail) error.detail = detail;
  return error;
}

export function isReauthRequiredError(error: unknown): boolean {
  return Boolean((error as { isReauthRequired?: unknown } | null)?.isReauthRequired);
}

export function isStaleConnectionError(error: unknown): boolean {
  const candidate = error as { kind?: unknown } | null;
  return candidate?.kind === "superseded" || /superseded|stale connection attempt/i.test(rawMessage(error));
}

function statusOf(message: string): number | null {
  const match = message.match(/^(\d{3})(?::|\s)/);
  return match ? Number(match[1]) : null;
}

export function projectConnectionError(
  error: unknown,
  target: ConnectionTarget = "local",
): ConnectionErrorProjection {
  const detail = safeMessage(error);
  const lower = detail.toLowerCase();
  const stale = isStaleConnectionError(error);
  const status = statusOf(detail);
  const local = target === "local";

  if (stale) {
    return {
      kind: "superseded",
      message: "Connection attempt superseded by a newer configuration.",
      detail,
      retryable: false,
      stale: true,
      recoveryVariant: null,
    };
  }

  const kind: ConnectionErrorKind =
    status === 401 || status === 403 || lower.includes("unauthorized") || lower.includes("forbidden")
      ? "unauthorized"
      : lower.includes("wrong service") || lower.includes("not running a compatible")
        ? "wrong-service"
        : isMissingHealthEndpointError(error)
          ? "missing"
          : status === 426 || status === 409 || lower.includes("incompatible") || lower.includes("version")
            ? "incompatible"
            : lower.includes("econnrefused") || lower.includes("timed out") || lower.includes("timeout") || lower.includes("fetch failed") || lower.includes("network")
              ? "unreachable"
              : "unknown";

  const recoveryVariant: RecoveryVariant = local
    ? "local-unavailable"
    : kind === "missing"
      ? "remote-missing"
      : kind === "wrong-service"
        ? "remote-wrong-service"
        : kind === "incompatible"
          ? "remote-incompatible"
          : "remote-unreachable";

  return {
    kind,
    message:
      kind === "unauthorized"
        ? "The connection was rejected. Sign in again or choose another connection."
        : kind === "wrong-service"
          ? `The selected host is not a compatible ${hostAppName()} daemon.`
          : kind === "incompatible"
            ? "The selected daemon needs an update before this app can connect."
            : kind === "unreachable"
              ? "The daemon could not be reached. Check that it is running and try again."
              : "The daemon connection failed. Try again or choose another connection.",
    detail,
    retryable: kind !== "unauthorized" && kind !== "wrong-service" && kind !== "missing",
    stale: false,
    recoveryVariant,
  };
}

/** Short alias for connector call sites that treat the result as a UI projection. */
export const classifyConnectionError = projectConnectionError;
