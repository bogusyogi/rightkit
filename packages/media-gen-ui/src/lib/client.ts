import { convertFileSrc, invoke, isTauri } from '@tauri-apps/api/core';

/**
 * The transport every panel uses to reach the media-generation engine. The host supplies it with
 * setMediaGenClient() once at startup, before rendering; the package ships one implementation, the
 * Tauri client below, and no mock. A browser preview, a test harness or a host with another
 * transport (an HTTP sidecar, a worker) passes its own.
 *
 * Wire: `request('command', {name, input})` runs one engine command (`generate`, `job.status`,
 * `studio.recipes.preview`, ...); other methods are the engine's own (`models`, `doctor`, ...).
 */
export interface MediaGenClient {
  /** One engine request; rejects with the engine's `{code, message, details?}` error. */
  request<T>(method: string, params: Record<string, unknown>): Promise<T>;
  /**
   * Approve a preflight draft as the user. The host mints the engine's approval token with a secret
   * only it holds, so no engine command approves on its own.
   */
  approveDraft<T>(request: {
    kind: string;
    draftId: string;
    displayedHash: string;
    params?: Record<string, unknown>;
  }): Promise<T>;
  /** A URL the webview can load for an on-disk asset path. */
  assetUrl(path: string): string;
  /** True when the engine runs on this machine (desktop features such as native drag and drop). */
  isDesktop(): boolean;
}

/** The Tauri host's client: `engine_request` and `approve_draft` commands, `convertFileSrc` for files. */
export const tauriClient: MediaGenClient = {
  request: <T>(method: string, params: Record<string, unknown>) =>
    invoke<T>('engine_request', { command: { method, params } }),
  approveDraft: <T>(request: { kind: string; draftId: string; displayedHash: string; params?: Record<string, unknown> }) =>
    invoke<T>('approve_draft', { request }),
  assetUrl: (path: string) => convertFileSrc(path),
  isDesktop: () => isTauri()
};

let current: MediaGenClient = tauriClient;

export function setMediaGenClient(client: MediaGenClient): void {
  current = client;
}

export function mediaGenClient(): MediaGenClient {
  return current;
}
