import type { PermissionMode } from "./daemon.js";
import type { CoreSandboxSettings } from "./types.js";
import { hostAppName } from "../appName.js";

export function projectName(path: string) {
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;
}

export function workspaceMenuLabel(path: string) {
  const parts = path.split(/[\\/]/).filter(Boolean);
  const name = parts.at(-1) ?? path;
  const parent = parts.at(-2);
  return parent ? `${parent}/${name}` : name;
}

export function visibleWorkspacePaths(paths: string[]) {
  const seen = new Set<string>();
  return paths.filter((path) => {
    const normalized = path.trim().replaceAll("\\", "/");
    const key = normalized.toLowerCase();
    if (!normalized || seen.has(key) || isScratchWorkspace(key)) return false;
    seen.add(key);
    return true;
  });
}

function isScratchWorkspace(path: string) {
  const name = path.split("/").filter(Boolean).at(-1);
  return (
    path.includes("/.codex-tmp/") || path.includes("/appdata/local/temp/") || name === ".test-ws"
  );
}

/** Setter for the app-shell status line, threaded into store actions. */
export type SetStatus = (status: string) => void;

/** Setter for the shared core-sandbox settings, threaded into store actions. */
export type SetCoreSandboxSettings = (settings: CoreSandboxSettings) => void;

/**
 * Turn an unknown thrown value into a human-readable message. Recognizes the
 * generated daemon client's `"<operationId> failed: <status>"` shape and maps
 * gated/server statuses to plain language instead of leaking the operation name
 * and HTTP code into the UI.
 */
export function messageOf(
  error: unknown,
  fallback = "The requested action could not be completed.",
): string {
  if (!(error instanceof Error)) return fallback;
  const message = error.message;
  const statusMatch = message.match(/failed:\s*(\d{3})\b/);
  if (statusMatch) {
    const status = Number(statusMatch[1]);
    if (status === 402) return `This feature needs ${hostAppName()} Pro.`;
    if (status === 401 || status === 403) return `Activate ${hostAppName()} to use this.`;
    if (status === 404) return fallback;
    if (status >= 500) {
      // The client appends the daemon's own explanation after an em dash when
      // it sent one. Say it: "the local engine returned an error" alone is the
      // same sentence for a refused keychain write and a panicked handler, and
      // it hid a keychain refusal that had an exact, actionable cause.
      const detail = message
        .split(/\s+—\s+/)
        .slice(1)
        .join(" — ")
        .trim();
      return detail
        ? `${fallback} — ${detail}`
        : `${fallback} — the local engine returned an error.`;
    }
  }
  return message;
}

const PERMISSION_LABELS: Record<string, string> = {
  plan: "Plan only",
  "accept-edits": "Accept edits",
  "auto-safe": "Auto safe",
  bypass: "Full access",
};

export function permissionLabel(mode: PermissionMode): string {
  return PERMISSION_LABELS[mode] ?? "Ask first";
}

/** Split a comma-separated string into trimmed, non-empty parts. With
 * `dedupe`, repeated values are dropped (first occurrence wins). */
export function parseCommaList(value: string, options: { dedupe?: boolean } = {}): string[] {
  const parts = value
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean);
  if (!options.dedupe) return parts;
  const seen = new Set<string>();
  return parts.filter((part) => {
    if (seen.has(part)) return false;
    seen.add(part);
    return true;
  });
}
