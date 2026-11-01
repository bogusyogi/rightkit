// BrowserWorkspace daemon client — owned by the D15 shared-browser lease
// surface. Thin HTTP wrapper over `/sessions/:id/browser/workspace/*`. Lives next to
// `lib/daemon/*.ts` so the desktop shell can call it from BrowserPane the
// same way it calls `memright.ts` etc. The router
// already authenticates; this client does not re-handshake.

import type {
  AnnotationSent,
  BrowserAnnotation,
  BrowserWorkspaceSummary,
  BrowserWorkspaceError,
  ConsoleLine,
  DevServer,
  HostPolicyDecision,
  BrowserProfileKind,
  ScreenshotEnvelope,
  StoredAnnotation,
  ViewportInfo,
} from "../../types/browserWorkspace";

export interface BrowserWorkspaceClient {
  getWorkspace(sessionId: string): Promise<BrowserWorkspaceSummary>;
  openSession(
    sessionId: string,
    url: string,
    name?: string,
  ): Promise<{ sessionId: string; url: string }>;
  closeSession(sessionId: string, browserSessionId: string): Promise<{ closed: string }>;
  navigate(
    sessionId: string,
    browserSessionId: string,
    url: string,
  ): Promise<{ navigated: string }>;
  reload(sessionId: string, browserSessionId: string): Promise<{ reloaded: string }>;
  back(
    sessionId: string,
    browserSessionId: string,
  ): Promise<{ direction: "back"; browserSessionId: string }>;
  forward(
    sessionId: string,
    browserSessionId: string,
  ): Promise<{ direction: "forward"; browserSessionId: string }>;
  setViewport(sessionId: string, viewport: ViewportInfo): Promise<{ viewport: ViewportInfo }>;
  getViewport(sessionId: string): Promise<{ viewport: ViewportInfo | null }>;
  setZoom(sessionId: string, zoom: number): Promise<{ zoom: number }>;
  screenshot(sessionId: string, browserSessionId: string): Promise<ScreenshotEnvelope>;
  console(
    sessionId: string,
    browserSessionId: string,
  ): Promise<{ sessionId: string; lines: string[] }>;
  errors(
    sessionId: string,
    browserSessionId: string,
  ): Promise<{ sessionId: string; errors: ConsoleLine[] }>;
  network(
    sessionId: string,
  ): Promise<{ sessionId: string; available: boolean; entries: unknown[]; note: string }>;
  devServers(sessionId: string): Promise<{ sessionId: string; servers: DevServer[] }>;
  actionLog(sessionId: string): Promise<{ sessionId: string; actionLogLen: number }>;
  pause(sessionId: string): Promise<{ paused: boolean }>;
  resume(sessionId: string): Promise<{ paused: boolean }>;
  takeControl(sessionId: string): Promise<{ paused: boolean; control: "human" | "agent" }>;
  releaseControl(sessionId: string): Promise<{ paused: boolean; control: "human" | "agent" }>;
  listAnnotations(
    sessionId: string,
  ): Promise<{ sessionId: string; annotations: StoredAnnotation[] }>;
  createAnnotation(sessionId: string, ann: BrowserAnnotation): Promise<StoredAnnotation>;
  sendAnnotation(sessionId: string, annId: string): Promise<AnnotationSent>;
  getHostPolicy(
    sessionId: string,
  ): Promise<{ sessionId: string; policy: Array<{ host: string; decision: HostPolicyDecision }> }>;
  setHostPolicy(
    sessionId: string,
    host: string,
    decision: HostPolicyDecision,
  ): Promise<{ host: string; decision: HostPolicyDecision }>;
  getProfile(sessionId: string): Promise<{ kind: BrowserProfileKind; label: string | null }>;
  setProfile(
    sessionId: string,
    kind: BrowserProfileKind,
    label?: string,
  ): Promise<{ kind: BrowserProfileKind; label: string | null }>;
}

export interface BrowserWorkspaceClientOptions {
  baseUrl: string;
  token: string;
}

export function createBrowserWorkspaceClient(
  opts: BrowserWorkspaceClientOptions,
): BrowserWorkspaceClient {
  const headers = (): HeadersInit => ({
    "content-type": "application/json",
    authorization: `Bearer ${opts.token}`,
  });
  // Session and browser ids are daemon-issued opaque path segments. Encode
  // each segment so a malformed or future id cannot escape the workspace
  // route; ordinary ids retain the canonical URL shape used by the daemon.
  const url = (path: string): string =>
    `${opts.baseUrl.replace(/\/$/, "")}/sessions/${path
      .split("/")
      .map((segment) => encodeURIComponent(segment))
      .join("/")}`;

  async function call<T>(path: string, init?: RequestInit): Promise<T> {
    const res = await fetch(url(path), {
      ...init,
      headers: { ...headers(), ...(init?.headers ?? {}) },
    });
    if (!res.ok) {
      const body = (await res.json().catch(() => ({}))) as Partial<BrowserWorkspaceError>;
      throw new Error(body.message ?? `browser-workspace ${res.status}`);
    }
    return (await res.json()) as T;
  }

  return {
    async getWorkspace(sessionId) {
      return call<BrowserWorkspaceSummary>(`${sessionId}/browser/workspace`);
    },
    async openSession(sessionId, pageUrl, name) {
      return call<{ sessionId: string; url: string }>(`${sessionId}/browser/workspace/open`, {
        method: "POST",
        body: JSON.stringify({ url: pageUrl, name }),
      });
    },
    async closeSession(sessionId, browserSessionId) {
      return call<{ closed: string }>(`${sessionId}/browser/workspace/close`, {
        method: "POST",
        body: JSON.stringify({ sessionId: browserSessionId }),
      });
    },
    async navigate(sessionId, browserSessionId, pageUrl) {
      return call<{ navigated: string }>(`${sessionId}/browser/workspace/navigate`, {
        method: "POST",
        body: JSON.stringify({ url: pageUrl, browserSessionId }),
      });
    },
    async reload(sessionId, browserSessionId) {
      return call<{ reloaded: string }>(`${sessionId}/browser/workspace/reload`, {
        method: "POST",
        body: JSON.stringify({ sessionId: browserSessionId }),
      });
    },
    async back(sessionId, browserSessionId) {
      return call<{ direction: "back"; browserSessionId: string }>(
        `${sessionId}/browser/workspace/back`,
        {
          method: "POST",
          body: JSON.stringify({ sessionId: browserSessionId }),
        },
      );
    },
    async forward(sessionId, browserSessionId) {
      return call<{ direction: "forward"; browserSessionId: string }>(
        `${sessionId}/browser/workspace/forward`,
        {
          method: "POST",
          body: JSON.stringify({ sessionId: browserSessionId }),
        },
      );
    },
    async setViewport(sessionId, viewport) {
      return call<{ viewport: ViewportInfo }>(`${sessionId}/browser/workspace/viewport`, {
        method: "POST",
        body: JSON.stringify(viewport),
      });
    },
    async getViewport(sessionId) {
      return call<{ viewport: ViewportInfo | null }>(`${sessionId}/browser/workspace/viewport`);
    },
    async setZoom(sessionId, zoom) {
      return call<{ zoom: number }>(`${sessionId}/browser/workspace/zoom`, {
        method: "POST",
        body: JSON.stringify({ zoom }),
      });
    },
    async screenshot(sessionId, browserSessionId) {
      return call<ScreenshotEnvelope>(`${sessionId}/browser/workspace/screenshot`, {
        method: "POST",
        body: JSON.stringify({ sessionId: browserSessionId }),
      });
    },
    async console(sessionId, browserSessionId) {
      return call<{ sessionId: string; lines: string[] }>(
        `${sessionId}/browser/workspace/console`,
        { method: "POST", body: JSON.stringify({ sessionId: browserSessionId }) },
      );
    },
    async errors(sessionId, browserSessionId) {
      return call<{ sessionId: string; errors: ConsoleLine[] }>(
        `${sessionId}/browser/workspace/errors`,
        { method: "POST", body: JSON.stringify({ sessionId: browserSessionId }) },
      );
    },
    async network(sessionId) {
      return call<{
        sessionId: string;
        available: boolean;
        entries: unknown[];
        note: string;
      }>(`${sessionId}/browser/workspace/network`);
    },
    async devServers(sessionId) {
      return call<{ sessionId: string; servers: DevServer[] }>(
        `${sessionId}/browser/workspace/dev-servers`,
      );
    },
    async actionLog(sessionId) {
      return call<{ sessionId: string; actionLogLen: number }>(
        `${sessionId}/browser/workspace/action-log`,
      );
    },
    async pause(sessionId) {
      return call<{ paused: boolean }>(`${sessionId}/browser/workspace/pause`, {
        method: "POST",
      });
    },
    async resume(sessionId) {
      return call<{ paused: boolean }>(`${sessionId}/browser/workspace/resume`, {
        method: "POST",
      });
    },
    async takeControl(sessionId) {
      return call<{ paused: boolean; control: "human" | "agent" }>(
        `${sessionId}/browser/workspace/take-control`,
        { method: "POST" },
      );
    },
    async releaseControl(sessionId) {
      return call<{ paused: boolean; control: "human" | "agent" }>(
        `${sessionId}/browser/workspace/release-control`,
        { method: "POST" },
      );
    },
    async listAnnotations(sessionId) {
      return call<{ sessionId: string; annotations: StoredAnnotation[] }>(
        `${sessionId}/browser/workspace/annotations`,
      );
    },
    async createAnnotation(sessionId, ann) {
      return call<StoredAnnotation>(`${sessionId}/browser/workspace/annotations`, {
        method: "POST",
        body: JSON.stringify(ann),
      });
    },
    async sendAnnotation(sessionId, annId) {
      return call<AnnotationSent>(`${sessionId}/browser/workspace/annotations/${annId}/send`, {
        method: "POST",
      });
    },
    async getHostPolicy(sessionId) {
      return call<{
        sessionId: string;
        policy: Array<{ host: string; decision: HostPolicyDecision }>;
      }>(`${sessionId}/browser/workspace/host-policy`);
    },
    async setHostPolicy(sessionId, host, decision) {
      return call<{ host: string; decision: HostPolicyDecision }>(
        `${sessionId}/browser/workspace/host-policy`,
        { method: "POST", body: JSON.stringify({ host, decision }) },
      );
    },
    async getProfile(sessionId) {
      return call<{ kind: BrowserProfileKind; label: string | null }>(
        `${sessionId}/browser/workspace/profile`,
      );
    },
    async setProfile(sessionId, kind, label) {
      return call<{ kind: BrowserProfileKind; label: string | null }>(
        `${sessionId}/browser/workspace/profile`,
        { method: "POST", body: JSON.stringify({ kind, label }) },
      );
    },
  };
}
