// BrowserWorkspace hook — owned by the D15 shared-browser lease surface.
// Loads the workspace summary, drives the annotation flow, and exposes
// the pause/take-control actions. Intentionally narrow: it composes the
// daemon client + the store; it does not own the daemon URL.

import { useCallback, useMemo, useRef } from "react";
import {
  createBrowserWorkspaceClient,
  type BrowserWorkspaceClient,
} from "@rightkit/agent-ui-core/lib/daemon/browserWorkspace";
import { useBrowserWorkspaceStore } from "@rightkit/agent-ui-core/state/browserStore";
import type {
  BrowserAnnotation,
  BrowserProfileKind,
  HostPolicyDecision,
  ViewportInfo,
} from "@rightkit/agent-ui-core/types/browserWorkspace";

export interface UseBrowserWorkspaceOptions {
  baseUrl: string;
  token: string;
  sessionId: string;
}

export interface UseBrowserWorkspace {
  client: BrowserWorkspaceClient;
  refresh: () => Promise<void>;
  open: (url: string, name?: string) => Promise<{ sessionId: string }>;
  close: (browserSessionId: string) => Promise<void>;
  navigate: (browserSessionId: string, url: string) => Promise<void>;
  reload: (browserSessionId: string) => Promise<void>;
  back: (browserSessionId: string) => Promise<void>;
  forward: (browserSessionId: string) => Promise<void>;
  setViewport: (viewport: ViewportInfo) => Promise<void>;
  setZoom: (zoom: number) => Promise<void>;
  screenshot: (browserSessionId: string) => Promise<void>;
  pause: () => Promise<void>;
  resume: () => Promise<void>;
  takeControl: () => Promise<void>;
  releaseControl: () => Promise<void>;
  setHostPolicy: (host: string, decision: HostPolicyDecision) => Promise<void>;
  setProfile: (kind: BrowserProfileKind, label?: string) => Promise<void>;
  createAnnotation: (ann: BrowserAnnotation) => Promise<void>;
  sendAnnotation: (annId: string) => Promise<void>;
  refreshAnnotations: () => Promise<void>;
  refreshSignals: (browserSessionId: string) => Promise<void>;
  /** Refreshes daemon-owned visual frame plus current daemon signals. */
  refreshLive: (browserSessionId: string) => Promise<void>;
}

export function useBrowserWorkspace(opts: UseBrowserWorkspaceOptions): UseBrowserWorkspace {
  const activeSessionId = useRef(opts.sessionId);
  const summaryRequest = useRef(0);
  const screenshotRequest = useRef(0);
  const signalsRequest = useRef(0);
  const annotationsRequest = useRef(0);
  const liveRefreshInFlight = useRef<{ key: string; promise: Promise<void> } | null>(null);
  activeSessionId.current = opts.sessionId;
  const client = useMemo(
    () => createBrowserWorkspaceClient({ baseUrl: opts.baseUrl, token: opts.token }),
    [opts.baseUrl, opts.token],
  );

  const setSummary = useBrowserWorkspaceStore((s) => s.setSummary);
  const setLoading = useBrowserWorkspaceStore((s) => s.setLoading);
  const setError = useBrowserWorkspaceStore((s) => s.setError);
  const setScreenshot = useBrowserWorkspaceStore((s) => s.setScreenshot);
  const setAnnotations = useBrowserWorkspaceStore((s) => s.setAnnotations);
  const upsertAnnotation = useBrowserWorkspaceStore((s) => s.upsertAnnotation);
  const setConsole = useBrowserWorkspaceStore((s) => s.setConsole);
  const setErrors = useBrowserWorkspaceStore((s) => s.setErrors);
  const setDevServers = useBrowserWorkspaceStore((s) => s.setDevServers);
  const setNetworkEntries = useBrowserWorkspaceStore((s) => s.setNetworkEntries);
  const setActionLogLen = useBrowserWorkspaceStore((s) => s.setActionLogLen);

  const refresh = useCallback(async () => {
    const requestedSessionId = opts.sessionId;
    const requestId = ++summaryRequest.current;
    setLoading(true);
    setError(null);
    try {
      const s = await client.getWorkspace(requestedSessionId);
      if (
        activeSessionId.current !== requestedSessionId ||
        summaryRequest.current !== requestId
      ) {
        return;
      }
      setSummary(s);
    } catch (e) {
      if (
        activeSessionId.current !== requestedSessionId ||
        summaryRequest.current !== requestId
      ) {
        return;
      }
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (
        activeSessionId.current === requestedSessionId &&
        summaryRequest.current === requestId
      ) {
        setLoading(false);
      }
    }
  }, [client, opts.sessionId, setSummary, setLoading, setError]);

  const open = useCallback(
    async (url: string, name?: string) => {
      setError(null);
      const out = await client.openSession(opts.sessionId, url, name);
      await refresh();
      return out;
    },
    [client, opts.sessionId, refresh, setError],
  );

  const close = useCallback(
    async (browserSessionId: string) => {
      setError(null);
      await client.closeSession(opts.sessionId, browserSessionId);
      await refresh();
    },
    [client, opts.sessionId, refresh, setError],
  );

  const navigate = useCallback(
    async (browserSessionId: string, url: string) => {
      setError(null);
      await client.navigate(opts.sessionId, browserSessionId, url);
    },
    [client, opts.sessionId, setError],
  );

  const reload = useCallback(
    async (browserSessionId: string) => {
      setError(null);
      await client.reload(opts.sessionId, browserSessionId);
    },
    [client, opts.sessionId, setError],
  );

  const back = useCallback(
    async (browserSessionId: string) => {
      setError(null);
      await client.back(opts.sessionId, browserSessionId);
    },
    [client, opts.sessionId, setError],
  );

  const forward = useCallback(
    async (browserSessionId: string) => {
      setError(null);
      await client.forward(opts.sessionId, browserSessionId);
    },
    [client, opts.sessionId, setError],
  );

  const setViewport = useCallback(
    async (viewport: ViewportInfo) => {
      setError(null);
      await client.setViewport(opts.sessionId, viewport);
      await refresh();
    },
    [client, opts.sessionId, refresh, setError],
  );

  const setZoom = useCallback(
    async (zoom: number) => {
      setError(null);
      await client.setZoom(opts.sessionId, zoom);
      await refresh();
    },
    [client, opts.sessionId, refresh, setError],
  );

  const screenshot = useCallback(
    async (browserSessionId: string) => {
      const requestedSessionId = opts.sessionId;
      const requestId = ++screenshotRequest.current;
      setError(null);
      try {
        const env = await client.screenshot(requestedSessionId, browserSessionId);
        // A late frame from a previously selected browser session must not
        // replace the frame for the current selection.
        if (
          activeSessionId.current !== requestedSessionId ||
          screenshotRequest.current !== requestId
        ) {
          return;
        }
        setScreenshot(env);
      } catch (e) {
        if (
          activeSessionId.current === requestedSessionId &&
          screenshotRequest.current === requestId
        ) {
          setError(e instanceof Error ? e.message : String(e));
        }
        throw e;
      }
    },
    [client, opts.sessionId, setError, setScreenshot],
  );

  const pause = useCallback(async () => {
    setError(null);
    await client.pause(opts.sessionId);
    await refresh();
  }, [client, opts.sessionId, refresh, setError]);

  const resume = useCallback(async () => {
    setError(null);
    await client.resume(opts.sessionId);
    await refresh();
  }, [client, opts.sessionId, refresh, setError]);

  const takeControl = useCallback(async () => {
    setError(null);
    await client.takeControl(opts.sessionId);
    await refresh();
  }, [client, opts.sessionId, refresh, setError]);

  const releaseControl = useCallback(async () => {
    setError(null);
    await client.releaseControl(opts.sessionId);
    await refresh();
  }, [client, opts.sessionId, refresh, setError]);

  const setHostPolicy = useCallback(
    async (host: string, decision: HostPolicyDecision) => {
      setError(null);
      await client.setHostPolicy(opts.sessionId, host, decision);
    },
    [client, opts.sessionId, setError],
  );

  const setProfile = useCallback(
    async (kind: BrowserProfileKind, label?: string) => {
      setError(null);
      await client.setProfile(opts.sessionId, kind, label);
      await refresh();
    },
    [client, opts.sessionId, refresh, setError],
  );

  const createAnnotation = useCallback(
    async (ann: BrowserAnnotation) => {
      const requestedSessionId = opts.sessionId;
      setError(null);
      const stored = await client.createAnnotation(requestedSessionId, ann);
      if (activeSessionId.current !== requestedSessionId) return;
      upsertAnnotation(stored);
    },
    [client, opts.sessionId, setError, upsertAnnotation],
  );

  const refreshAnnotations = useCallback(async () => {
    const requestedSessionId = opts.sessionId;
    const requestId = ++annotationsRequest.current;
    setError(null);
    try {
      const out = await client.listAnnotations(requestedSessionId);
      if (
        activeSessionId.current !== requestedSessionId ||
        annotationsRequest.current !== requestId
      ) {
        return;
      }
      setAnnotations(out.annotations);
    } catch (e) {
      if (
        activeSessionId.current === requestedSessionId &&
        annotationsRequest.current === requestId
      ) {
        setError(e instanceof Error ? e.message : String(e));
      }
      throw e;
    }
  }, [client, opts.sessionId, setError, setAnnotations]);

  const refreshSignals = useCallback(
    async (browserSessionId: string) => {
      const requestedSessionId = opts.sessionId;
      const requestId = ++signalsRequest.current;
      setError(null);
      try {
        const [consoleResult, errorsResult, networkResult, devServersResult, actionLogResult] =
          await Promise.all([
            client.console(requestedSessionId, browserSessionId),
            client.errors(requestedSessionId, browserSessionId),
            client.network(requestedSessionId),
            client.devServers(requestedSessionId),
            client.actionLog(requestedSessionId),
          ]);
        // Signals are selected-session state too; discard a late poll after a
        // session switch just as we discard late screenshot frames.
        if (
          activeSessionId.current !== requestedSessionId ||
          signalsRequest.current !== requestId
        ) {
          return;
        }
        setConsole(
          consoleResult.lines.map((line) => {
            const match = /^\[([^\]]+)\]\s*(.*)$/.exec(line);
            return { level: match?.[1] ?? "log", text: match?.[2] ?? line };
          }),
        );
        setErrors(errorsResult.errors);
        setNetworkEntries(networkResult.entries);
        setDevServers(devServersResult.servers);
        setActionLogLen(actionLogResult.actionLogLen);
      } catch (e) {
        if (
          activeSessionId.current === requestedSessionId &&
          signalsRequest.current === requestId
        ) {
          setError(e instanceof Error ? e.message : String(e));
        }
        throw e;
      }
    },
    [
      client,
      opts.sessionId,
      setActionLogLen,
      setConsole,
      setDevServers,
      setError,
      setErrors,
      setNetworkEntries,
    ],
  );

  const refreshLive = useCallback(
    async (browserSessionId: string) => {
      // BrowserSession remains daemon-owned. This only refreshes its rendered
      // frame plus current diagnostics; it never constructs browser state.
      const key = `${opts.sessionId}:${browserSessionId}`;
      const previous = liveRefreshInFlight.current;
      if (previous?.key === key) {
        await previous.promise;
        return;
      }

      const promise = (async () => {
        try {
          await Promise.all([
            screenshot(browserSessionId),
            refreshSignals(browserSessionId),
            refresh(),
          ]);
        } catch (e) {
          // Live polling is background work. Surface the daemon error in the
          // pane, but do not create an unhandled rejection every 750 ms.
          if (activeSessionId.current === opts.sessionId) {
            setError(e instanceof Error ? e.message : String(e));
          }
        }
      })();
      liveRefreshInFlight.current = { key, promise };
      try {
        await promise;
      } finally {
        if (liveRefreshInFlight.current?.promise === promise) {
          liveRefreshInFlight.current = null;
        }
      }
    },
    [opts.sessionId, refresh, refreshSignals, screenshot, setError],
  );

  const sendAnnotation = useCallback(
    async (annId: string) => {
      setError(null);
      await client.sendAnnotation(opts.sessionId, annId);
      await refreshAnnotations();
    },
    [client, opts.sessionId, refreshAnnotations, setError],
  );

  return {
    client,
    refresh,
    open,
    close,
    navigate,
    reload,
    back,
    forward,
    setViewport,
    setZoom,
    screenshot,
    pause,
    resume,
    takeControl,
    releaseControl,
    setHostPolicy,
    setProfile,
    createAnnotation,
    sendAnnotation,
    refreshAnnotations,
    refreshSignals,
    refreshLive,
  };
}
