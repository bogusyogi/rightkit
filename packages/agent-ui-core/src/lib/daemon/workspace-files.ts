// D1 — Live workspace files/artifacts daemon client.
//
// These functions talk to the authenticated `/workspace/*` surface. The
// request helpers deliberately stay local because the watch route is an SSE
// stream rather than a JSON operation supported by the generated request
// wrapper. The shapes here are local TS mirrors of the OpenAPI schemas the
// daemon publishes — `check:protocol-mirror` validates drift.

import type { DaemonConfig } from "../types/settings-plugins";
import type {
  WorkspaceArtifactsResponse,
  WorkspaceFileResponse,
  WorkspaceFileSaveRequest,
  WorkspaceFileSaveResponse,
  WorkspaceRecentResponse,
  WorkspaceSearchRequest,
  WorkspaceSearchResponse,
  WorkspaceTreeResponse,
  WorkspaceWatchEvent,
} from "../types/workspace-files";

export interface WorkspaceRequestError extends Error {
  status: number;
  reason: string;
}

function buildError(status: number, reason: string): WorkspaceRequestError {
  const err = new Error(`workspace request failed (${status}): ${reason}`) as WorkspaceRequestError;
  err.status = status;
  err.reason = reason;
  return err;
}

async function workspaceRequest<T>(
  config: DaemonConfig,
  method: "GET" | "POST" | "PUT",
  path: string,
  options: {
    query?: Record<string, string | number | boolean | null | undefined>;
    body?: unknown;
    signal?: AbortSignal;
  } = {},
): Promise<T> {
  const url = new URL(`${config.url.replace(/\/+$/, "")}${path}`);
  if (options.query) {
    for (const [key, value] of Object.entries(options.query)) {
      if (value === undefined || value === null) continue;
      url.searchParams.set(key, String(value));
    }
  }
  const headers = new Headers();
  if (config.token) headers.set("authorization", `Bearer ${config.token}`);
  let body: BodyInit | undefined;
  if (options.body !== undefined) {
    headers.set("content-type", "application/json");
    body = JSON.stringify(options.body);
  }
  const response = await fetch(url, {
    method,
    headers,
    body,
    signal: options.signal,
    cache: "no-store",
  });
  if (!response.ok) {
    const text = await response.text().catch(() => "");
    if (options.signal?.aborted) {
      throw buildError(0, "aborted");
    }
    throw buildError(response.status, text || response.statusText);
  }
  if (response.status === 204) return undefined as unknown as T;
  return (await response.json()) as T;
}

export async function getWorkspaceTree(
  config: DaemonConfig,
  workspaceRoot: string,
  path?: string,
  options: { signal?: AbortSignal } = {},
): Promise<WorkspaceTreeResponse> {
  return workspaceRequest<WorkspaceTreeResponse>(config, "GET", "/workspace/tree", {
    query: { workspaceRoot, path: path ?? null },
    signal: options.signal,
  });
}

export async function postWorkspaceTreeExpand(
  config: DaemonConfig,
  workspaceRoot: string,
  path?: string,
  options: { signal?: AbortSignal } = {},
): Promise<WorkspaceTreeResponse> {
  return workspaceRequest<WorkspaceTreeResponse>(config, "POST", "/workspace/tree/expand", {
    body: { workspaceRoot, path: path ?? null },
    signal: options.signal,
  });
}

export async function getWorkspaceFile(
  config: DaemonConfig,
  workspaceRoot: string,
  path: string,
  options: { signal?: AbortSignal } = {},
): Promise<WorkspaceFileResponse> {
  return workspaceRequest<WorkspaceFileResponse>(config, "GET", "/workspace/file", {
    query: { workspaceRoot, path },
    signal: options.signal,
  });
}

export async function putWorkspaceFile(
  config: DaemonConfig,
  request: WorkspaceFileSaveRequest,
  options: { signal?: AbortSignal } = {},
): Promise<WorkspaceFileSaveResponse> {
  return workspaceRequest<WorkspaceFileSaveResponse>(config, "PUT", "/workspace/file", {
    body: request,
    signal: options.signal,
  });
}

export async function postWorkspaceSearch(
  config: DaemonConfig,
  request: WorkspaceSearchRequest,
  options: { signal?: AbortSignal } = {},
): Promise<WorkspaceSearchResponse> {
  return workspaceRequest<WorkspaceSearchResponse>(config, "POST", "/workspace/search", {
    body: request,
    signal: options.signal,
  });
}

export async function getWorkspaceArtifacts(
  config: DaemonConfig,
  workspaceRoot: string,
  options: { signal?: AbortSignal } = {},
): Promise<WorkspaceArtifactsResponse> {
  return workspaceRequest<WorkspaceArtifactsResponse>(config, "GET", "/workspace/artifacts", {
    query: { workspaceRoot },
    signal: options.signal,
  });
}

export async function getWorkspaceRecent(
  config: DaemonConfig,
  workspaceRoot: string,
  options: { signal?: AbortSignal } = {},
): Promise<WorkspaceRecentResponse> {
  return workspaceRequest<WorkspaceRecentResponse>(config, "GET", "/workspace/recent", {
    query: { workspaceRoot },
    signal: options.signal,
  });
}

export interface WorkspaceWatchHandle {
  close: () => void;
}

export interface WorkspaceWatchSubscription {
  events: AsyncIterableIterator<WorkspaceWatchEvent>;
  handle: WorkspaceWatchHandle;
}

export async function openWorkspaceWatch(
  config: DaemonConfig,
  workspaceRoot: string,
  onEvent: (event: WorkspaceWatchEvent) => void,
  onError: (error: WorkspaceRequestError) => void,
  options: { signal?: AbortSignal } = {},
): Promise<WorkspaceWatchHandle> {
  const url = new URL(`${config.url.replace(/\/+$/, "")}/workspace/watch`);
  url.searchParams.set("workspaceRoot", workspaceRoot);
  const headers = new Headers({ accept: "text/event-stream" });
  if (config.token) headers.set("authorization", `Bearer ${config.token}`);
  const controller = new AbortController();
  if (options.signal) {
    if (options.signal.aborted) {
      throw buildError(0, "aborted");
    }
    options.signal.addEventListener("abort", () => controller.abort(), { once: true });
  }
  const response = await fetch(url, {
    method: "GET",
    headers,
    signal: controller.signal,
    cache: "no-store",
  });
  if (!response.ok || !response.body) {
    throw buildError(response.status, response.statusText || "watch unavailable");
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  void (async () => {
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        let boundary = buffer.search(/\r?\n\r?\n/);
        while (boundary >= 0) {
          const separator = buffer.match(/\r?\n\r?\n/)?.[0] ?? "\n\n";
          const frame = buffer.slice(0, boundary);
          buffer = buffer.slice(boundary + separator.length);
          boundary = buffer.search(/\r?\n\r?\n/);
          const payload = frame
            .split(/\r?\n/)
            .filter((line) => line.startsWith("data:"))
            .map((line) => line.slice(5).replace(/^ /, ""))
            .join("\n")
            .trim();
          if (!payload) continue;
          let parsed: WorkspaceWatchEvent;
          try {
            parsed = JSON.parse(payload) as WorkspaceWatchEvent;
          } catch (error) {
            onError(buildError(500, `malformed SSE payload: ${(error as Error).message}`));
            continue;
          }
          try {
            onEvent(parsed);
          } catch (error) {
            onError(buildError(500, `workspace watch callback failed: ${(error as Error).message}`));
          }
        }
      }
    } catch (error) {
      if ((error as { name?: string }).name === "AbortError") return;
      onError(error as WorkspaceRequestError);
    }
  })();
  return {
    close: () => controller.abort(),
  };
}
