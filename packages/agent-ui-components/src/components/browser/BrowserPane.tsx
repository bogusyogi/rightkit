// BrowserPane — owned by the D15 shared-browser lease surface.
// Renders URL bar, navigation controls, screenshot preview, console/errors,
// pause/take-control, viewport + zoom, dev servers, and the structured
// annotation flow. The pane composes `useBrowserWorkspace` + the store.
// The pane is intentionally self-contained: the desktop shell mounts it
// inside the right-side pane rail, no central/AppBody/TitleBar edits.

import { useEffect, useMemo, useState } from "react";
import { ActionDialog, Select } from "@rightkit/react-shell/react";
import {
  useBrowserWorkspace,
  type UseBrowserWorkspaceOptions,
} from "@rightkit/agent-ui-hooks/hooks/browser/useBrowserWorkspace";
import { useBrowserWorkspaceStore } from "@rightkit/agent-ui-core/state/browserStore";

export interface BrowserPaneProps extends UseBrowserWorkspaceOptions {
  className?: string;
}

export function BrowserPane(props: BrowserPaneProps) {
  const ws = useBrowserWorkspace(props);
  const summary = useBrowserWorkspaceStore((s) => s.summary);
  const screenshot = useBrowserWorkspaceStore((s) => s.screenshot);
  const errors = useBrowserWorkspaceStore((s) => s.errors);
  const consoleLines = useBrowserWorkspaceStore((s) => s.console);
  const devServers = useBrowserWorkspaceStore((s) => s.devServers);
  const networkEntries = useBrowserWorkspaceStore((s) => s.networkEntries);
  const actionLogLen = useBrowserWorkspaceStore((s) => s.actionLogLen);
  const annotations = useBrowserWorkspaceStore((s) => s.annotations);
  const loading = useBrowserWorkspaceStore((s) => s.loading);
  const lastError = useBrowserWorkspaceStore((s) => s.lastError);
  const resetBrowserStore = useBrowserWorkspaceStore((s) => s.reset);
  const clearScreenshot = useBrowserWorkspaceStore((s) => s.setScreenshot);
  const consoleRows = useMemo(() => {
    const occurrences = new Map<string, number>();
    return consoleLines.map((line) => {
      const base = `${line.level}:${line.text}`;
      const occurrence = (occurrences.get(base) ?? 0) + 1;
      occurrences.set(base, occurrence);
      return { line, key: `${base}:${occurrence}` };
    });
  }, [consoleLines]);

  const [url, setUrl] = useState("");
  const [selectedSession, setSelectedSession] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const [annotationLocator, setAnnotationLocator] = useState<string | undefined>();
  const [viewportWidth, setViewportWidth] = useState("1280");
  const [viewportHeight, setViewportHeight] = useState("800");
  const [zoom, setZoom] = useState("1");
  const [profileKind, setProfileKind] = useState<"ephemeral" | "named_shared">("ephemeral");
  const [profileLabel, setProfileLabel] = useState("");
  const [profileShareConfirmOpen, setProfileShareConfirmOpen] = useState(false);
  const [profileSharePending, setProfileSharePending] = useState(false);
  const [profileShareError, setProfileShareError] = useState<string | null>(null);
  const [host, setHost] = useState("");
  const [hostDecision, setHostDecision] = useState<"allow" | "ask" | "block">("ask");
  const codeRightSessionId = props.sessionId;
  const liveSession = summary?.sessions.find((session) => session.sessionId === selectedSession);
  const humanLease = liveSession?.controlLease?.owner === "workspace-user";

  useEffect(() => {
    // The session id is the reset boundary even when this pane is rendered
    // outside AppBody (which also keys the production mount by session).
    void codeRightSessionId;
    resetBrowserStore();
    setSelectedSession(null);
    setUrl("");
    setAnnotationLocator(undefined);
  }, [codeRightSessionId, resetBrowserStore]);

  useEffect(() => {
    void ws.refresh();
    // Annotation hydration is auxiliary to the workspace summary; a daemon
    // disconnect must remain visible through the hook without producing an
    // unhandled background rejection during pane mount.
    void ws.refreshAnnotations().catch(() => undefined);
  }, [ws.refresh, ws.refreshAnnotations]);

  useEffect(() => {
    if (summary) {
      const selectionStillExists = summary.sessions.some(
        (session) => session.sessionId === selectedSession,
      );
      if (!selectionStillExists) {
        setSelectedSession(summary.sessions[0]?.sessionId ?? null);
      }
    }
    if (summary?.viewport) {
      setViewportWidth(String(summary.viewport.width));
      setViewportHeight(String(summary.viewport.height));
    }
    if (summary) {
      setProfileKind(summary.profileKind);
      setProfileLabel(summary.profileLabel ?? "");
    }
  }, [selectedSession, summary]);

  useEffect(() => {
    if (!selectedSession) return;
    clearScreenshot(null);
    void Promise.all([ws.screenshot(selectedSession), ws.refreshSignals(selectedSession)]).catch(
      () => undefined,
    );
  }, [clearScreenshot, selectedSession, ws.refreshSignals, ws.screenshot]);

  useEffect(() => {
    if (!selectedSession || !humanLease) return;
    // A human lease turns captured daemon frames into a live desktop surface.
    // No browser state lives in React; every frame is read from BrowserSession.
    void ws.refreshLive(selectedSession);
    const timer = window.setInterval(() => void ws.refreshLive(selectedSession), 750);
    return () => window.clearInterval(timer);
  }, [humanLease, selectedSession, ws.refreshLive]);

  useEffect(() => {
    if (!selectedSession) return;
    const active = summary?.sessions.find((session) => session.sessionId === selectedSession);
    if (active) {
      setUrl(active.url);
      if (active.zoom != null) setZoom(String(active.zoom));
    }
  }, [selectedSession, summary?.sessions]);

  const onOpen = async () => {
    if (!url.trim()) return;
    if (selectedSession) {
      await ws.navigate(selectedSession, url.trim());
      await ws.refresh();
      await ws.screenshot(selectedSession);
      await ws.refreshSignals(selectedSession);
    } else {
      const out = await ws.open(url.trim());
      setSelectedSession(out.sessionId);
      await ws.screenshot(out.sessionId);
      await ws.refreshSignals(out.sessionId);
    }
  };

  const onReload = async () => {
    if (!selectedSession) return;
    await ws.reload(selectedSession);
    await ws.screenshot(selectedSession);
    await ws.refreshSignals(selectedSession);
  };

  const moveHistory = async (direction: "back" | "forward") => {
    if (!selectedSession) return;
    await ws[direction](selectedSession);
    await ws.refresh();
    await ws.screenshot(selectedSession);
    await ws.refreshSignals(selectedSession);
  };

  const onScreenshot = async () => {
    if (!selectedSession) return;
    await ws.screenshot(selectedSession);
  };

  const onAnnotate = async () => {
    if (!selectedSession) return;
    await ws.createAnnotation({
      atMs: Date.now(),
      locator: annotationLocator,
      viewport: summary?.viewport ?? undefined,
      screenshotBase64: screenshot?.dataBase64 ?? undefined,
      note,
    });
    setNote("");
    setAnnotationLocator(undefined);
  };

  const applyViewport = async () => {
    const width = Number(viewportWidth);
    const height = Number(viewportHeight);
    if (!Number.isFinite(width) || !Number.isFinite(height) || width < 320 || height < 240) return;
    await ws.setViewport({
      width,
      height,
      deviceScaleFactor: summary?.viewport?.deviceScaleFactor ?? 1,
    });
  };

  const applyZoom = async () => {
    const next = Number(zoom);
    if (!Number.isFinite(next)) return;
    await ws.setZoom(next);
    await ws.refresh();
  };

  const applyProfile = async () => {
    if (profileKind === "named_shared") {
      if (!profileLabel.trim()) return;
      setProfileShareError(null);
      setProfileShareConfirmOpen(true);
      return;
    }
    await ws.setProfile(profileKind, profileLabel.trim() || undefined);
  };

  const confirmProfileShare = async () => {
    if (profileSharePending) return;
    setProfileSharePending(true);
    setProfileShareError(null);
    try {
      await ws.setProfile("named_shared", profileLabel.trim());
      setProfileShareConfirmOpen(false);
    } catch (error) {
      setProfileShareError(
        error instanceof Error ? error.message : "Browser profile could not be shared",
      );
    } finally {
      setProfileSharePending(false);
    }
  };

  return (
    <div
      className={`flex min-h-0 flex-col overflow-hidden rounded-xl border border-token-border bg-token-bg ${props.className ?? ""}`}
      data-testid="browser-pane"
    >
      <header className="flex flex-none items-center gap-1.5 border-b border-token-border p-2">
        <button
          type="button"
          aria-label="Back"
          className="rounded-md px-2 py-1 text-token-muted hover:bg-token-hover disabled:opacity-40"
          disabled={!selectedSession || loading}
          onClick={() => void moveHistory("back")}
        >
          ←
        </button>
        <button
          type="button"
          aria-label="Forward"
          className="rounded-md px-2 py-1 text-token-muted hover:bg-token-hover disabled:opacity-40"
          disabled={!selectedSession || loading}
          onClick={() => void moveHistory("forward")}
        >
          →
        </button>
        <button
          type="button"
          aria-label="Reload"
          className="rounded-md px-2 py-1 text-token-muted hover:bg-token-hover disabled:opacity-40"
          disabled={!selectedSession || loading}
          onClick={() => void onReload()}
        >
          ↻
        </button>
        <div className="flex min-w-0 flex-1 items-center rounded-lg border border-token-border bg-token-surface px-2">
          <input
            aria-label="URL"
            className="min-w-0 flex-1 bg-transparent px-1 py-1.5 text-sm outline-none"
            placeholder="Enter a URL"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void onOpen();
            }}
          />
        </div>
        <button
          type="button"
          className="rounded-md bg-token-accent px-3 py-1.5 text-sm font-semibold text-token-bg disabled:opacity-50"
          disabled={loading || !url.trim()}
          onClick={() => void onOpen()}
        >
          {selectedSession ? "Go" : "Open"}
        </button>
        {summary && summary.sessions.length > 1 ? (
          <Select
            label="Browser session"
            value={selectedSession ?? ""}
            onChange={(value) => setSelectedSession(value || null)}
            options={[
              { value: "", label: "Choose session" },
              ...summary.sessions.map((s) => ({
                value: s.sessionId,
                label: s.title || s.url || s.sessionId,
              })),
            ]}
          />
        ) : null}
      </header>

      {lastError ? (
        <div
          className="mx-2 mt-2 flex-none rounded-lg border border-token-border bg-token-bg p-3 text-sm"
          role="alert"
        >
          <p className="font-semibold">Browser needs attention</p>
          <p className="mt-0.5 text-xs text-token-muted">{lastError}</p>
          <button
            type="button"
            className="mt-2 rounded-md border border-token-border px-2 py-1 text-xs"
            disabled={!url.trim() || loading}
            onClick={() => void onOpen()}
          >
            Retry
          </button>
        </div>
      ) : null}

      <section className="min-h-0 flex-1 overflow-auto" aria-label="Preview">
        {liveSession ? (
          <div
            className="flex flex-wrap items-center gap-x-2 gap-y-1 border-b border-token-border px-3 py-1.5 text-xs text-token-muted"
            aria-live="polite"
            aria-atomic="true"
            data-testid="browser-lease-strip"
          >
            <span>
              {liveSession.mode === "visible"
                ? "Visible BrowserSession"
                : "Headless BrowserSession"}
            </span>
            <span>·</span>
            <span>{humanLease ? "You control this session" : "Runtime controls this session"}</span>
            {liveSession.controlLease ? (
              <span>· lease {liveSession.controlLease.generation}</span>
            ) : null}
          </div>
        ) : null}
        {screenshot ? (
          <button
            type="button"
            aria-label="Select a screenshot point to annotate"
            className="block w-full cursor-crosshair bg-black/10 p-0"
            onClick={(event) => {
              const bounds = event.currentTarget.getBoundingClientRect();
              const x =
                event.detail === 0
                  ? Math.round(screenshot.width / 2)
                  : Math.round(((event.clientX - bounds.left) / bounds.width) * screenshot.width);
              const y =
                event.detail === 0
                  ? Math.round(screenshot.height / 2)
                  : Math.round(((event.clientY - bounds.top) / bounds.height) * screenshot.height);
              setAnnotationLocator(`viewport:${x},${y}`);
            }}
          >
            <img
              alt={`browser screenshot of ${screenshot.url || (selectedSession ?? "")}`}
              src={`data:${screenshot.mime};base64,${screenshot.dataBase64}`}
              className="block max-h-[480px] w-full object-contain"
            />
          </button>
        ) : (
          <div className="grid h-full min-h-56 place-items-center p-6 text-center">
            <div>
              <p className="text-sm font-semibold">Start browsing</p>
              <p className="mt-1 text-xs text-token-muted">Enter a URL to open a page.</p>
              {devServers.length > 0 ? (
                <div className="mt-3 flex flex-wrap justify-center gap-2">
                  {devServers.map((server) => (
                    <button
                      key={`${server.port}:${server.url}`}
                      type="button"
                      className="rounded-md border border-token-border px-2 py-1 text-xs"
                      onClick={() => setUrl(server.url)}
                    >
                      Open :{server.port}
                    </button>
                  ))}
                </div>
              ) : null}
            </div>
          </div>
        )}
      </section>

      <details
        className="mx-2 mb-2 flex-none rounded-lg border border-token-border bg-token-bg"
        data-testid="browser-advanced-controls"
      >
        <summary className="cursor-pointer px-3 py-2 text-sm font-medium">
          Advanced controls
        </summary>
        <div className="grid gap-3 border-t border-token-border p-3 text-xs">
          <div className="flex flex-wrap gap-2">
            <button
              type="button"
              className="rounded-md border border-token-border px-2 py-1"
              disabled={!selectedSession || loading}
              onClick={() => void onScreenshot()}
            >
              Capture visual
            </button>
            <button
              type="button"
              className="rounded-md border border-token-border px-2 py-1"
              disabled={loading}
              onClick={summary?.paused ? ws.resume : ws.pause}
            >
              {summary?.paused ? "Resume automation" : "Pause automation"}
            </button>
            <button
              type="button"
              className="rounded-md border border-token-border px-2 py-1"
              disabled={!selectedSession || loading}
              onClick={() => selectedSession && void ws.close(selectedSession)}
            >
              Close BrowserSession
            </button>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-semibold">Viewport</span>
            <input
              aria-label="Viewport width"
              className="w-16 rounded-md border border-token-border bg-token-bg px-2 py-1"
              value={viewportWidth}
              onChange={(e) => setViewportWidth(e.target.value)}
              inputMode="numeric"
            />
            <span>×</span>
            <input
              aria-label="Viewport height"
              className="w-16 rounded-md border border-token-border bg-token-bg px-2 py-1"
              value={viewportHeight}
              onChange={(e) => setViewportHeight(e.target.value)}
              inputMode="numeric"
            />
            <button
              type="button"
              className="rounded-md border border-token-border px-2 py-1"
              disabled={loading}
              onClick={() => void applyViewport()}
            >
              Apply
            </button>
            <label className="ml-auto flex items-center gap-1">
              Zoom{" "}
              <input
                aria-label="Zoom"
                className="w-12 rounded-md border border-token-border bg-token-bg px-1 py-1"
                value={zoom}
                onChange={(e) => setZoom(e.target.value)}
                inputMode="decimal"
              />
            </label>
            <button
              type="button"
              className="rounded-md border border-token-border px-2 py-1"
              disabled={loading}
              onClick={() => void applyZoom()}
            >
              Set
            </button>
          </div>
          <div className="grid gap-2 rounded-lg border border-token-border p-2">
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-semibold">Profile</span>
              <Select value={profileKind} options={[{ value: "ephemeral", label: "Ephemeral" }, { value: "named_shared", label: "Named shared" }]} onChange={(value) => setProfileKind(value as "ephemeral" | "named_shared")} label="Browser profile" className="rounded-md border border-token-border bg-token-bg px-2 py-1" />
              <input
                aria-label="Profile label"
                className="min-w-0 flex-1 rounded-md border border-token-border bg-token-bg px-2 py-1"
                placeholder="Profile label"
                value={profileLabel}
                onChange={(e) => setProfileLabel(e.target.value)}
              />
              <button
                type="button"
                className="rounded-md border border-token-border px-2 py-1"
                disabled={loading || (profileKind === "named_shared" && !profileLabel.trim())}
                onClick={() => void applyProfile()}
              >
                Save profile
              </button>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-semibold">Host access</span>
              <input
                aria-label="Host"
                className="min-w-0 flex-1 rounded-md border border-token-border bg-token-bg px-2 py-1"
                placeholder="example.com"
                value={host}
                onChange={(e) => setHost(e.target.value)}
              />
              <Select value={hostDecision} options={[{ value: "ask", label: "Ask" }, { value: "allow", label: "Allow" }, { value: "block", label: "Block" }]} onChange={(value) => setHostDecision(value as "allow" | "ask" | "block")} label="Host decision" className="rounded-md border border-token-border bg-token-bg px-2 py-1" />
              <button
                type="button"
                className="rounded-md border border-token-border px-2 py-1"
                disabled={!host.trim() || loading}
                onClick={() => void ws.setHostPolicy(host.trim(), hostDecision)}
              >
                Save rule
              </button>
            </div>
          </div>
          <div className="flex flex-wrap gap-2">
            <button
              type="button"
              className="rounded-md border border-token-border px-2 py-1"
              disabled={loading}
              onClick={ws.takeControl}
            >
              Take control
            </button>
            <button
              type="button"
              className="rounded-md border border-token-border px-2 py-1"
              disabled={loading}
              onClick={ws.releaseControl}
            >
              Release control
            </button>
          </div>
        </div>
      </details>

      <details
        className="mx-2 mb-2 flex-none rounded-lg border border-token-border bg-token-bg"
        data-testid="browser-diagnostics"
      >
        <summary className="cursor-pointer px-3 py-2 text-sm font-medium">
          Signals &amp; annotations{" "}
          <span className="ml-1 text-xs font-normal text-token-muted">
            {errors.length} errors · {consoleLines.length} console · {annotations.length} notes
          </span>
        </summary>
        <div className="grid gap-3 border-t border-token-border p-3 text-xs">
          <div>
            <div className="mb-1 flex items-center gap-2">
              <h3 className="font-semibold">Console</h3>
              <button
                type="button"
                className="ml-auto rounded-md border border-token-border px-2 py-0.5"
                disabled={!selectedSession || loading}
                onClick={() => selectedSession && void ws.refreshSignals(selectedSession)}
              >
                Refresh
              </button>
            </div>
            <ul className="max-h-32 space-y-1 overflow-auto font-mono text-[11px]">
              {consoleRows.map(({ line, key }) => (
                <li key={key}>
                  <span className="text-token-muted">[{line.level}]</span> {line.text}
                </li>
              ))}
              {consoleLines.length === 0 ? (
                <li className="text-token-muted">No console output captured.</li>
              ) : null}
            </ul>
          </div>
          <div>
            <h3 className="mb-1 font-semibold">Errors</h3>
            <ul className="max-h-32 space-y-1 overflow-auto">
              {errors.map((e) => (
                <li key={`${e.level}:${e.text}`}>
                  <span className="text-token-accent">[{e.level}]</span> {e.text}
                </li>
              ))}
              {errors.length === 0 ? (
                <li className="text-token-muted">No errors captured.</li>
              ) : null}
            </ul>
          </div>
          <div className="grid gap-2">
            <h3 className="font-semibold">Send an annotation</h3>
            <textarea
              aria-label="Annotation note"
              className="w-full rounded-md border border-token-border bg-token-bg p-2"
              rows={2}
              placeholder="Describe what the agent should inspect"
              value={note}
              onChange={(e) => setNote(e.target.value)}
            />
            {annotationLocator ? (
              <p className="font-mono text-[10px] text-token-muted">target {annotationLocator}</p>
            ) : null}
            <div className="flex items-center gap-2">
              <button
                type="button"
                className="rounded-md bg-token-accent px-3 py-1 text-token-bg disabled:opacity-50"
                disabled={!selectedSession || !note.trim()}
                onClick={() => void onAnnotate()}
              >
                Send note
              </button>
              <span className="text-token-muted">
                {networkEntries.length} network · {actionLogLen} actions
              </span>
            </div>
            <ul className="space-y-1">
              {annotations.map((a) => (
                <li key={a.id} className="flex items-center gap-2">
                  <span className="truncate flex-1">{a.note}</span>
                  <button
                    type="button"
                    className="rounded border border-token-border px-2 py-0.5"
                    onClick={() => ws.sendAnnotation(a.id)}
                  >
                    Send
                  </button>
                </li>
              ))}
            </ul>
          </div>
        </div>
      </details>
      {profileShareConfirmOpen ? (
        <ActionDialog
          title="Share browser profile with this workspace?"
          description={`The “${profileLabel.trim()}” profile may include signed-in site state. Browser automation in this workspace will use it.`}
          role="alertdialog"
          dismissible={!profileSharePending}
          onClose={() => {
            setProfileShareError(null);
            setProfileShareConfirmOpen(false);
          }}
          actions={[
            {
              id: "cancel",
              label: "Cancel",
              autoFocus: true,
              disabled: profileSharePending,
              onClick: () => {
                setProfileShareError(null);
                setProfileShareConfirmOpen(false);
              },
            },
            {
              id: "confirm",
              label: "Share profile",
              variant: "primary",
              disabled: profileSharePending,
              onClick: () => void confirmProfileShare(),
            },
          ]}
        >
          {profileShareError ? (
            <p role="alert" className="rk-modal__error">
              {profileShareError}
            </p>
          ) : null}
        </ActionDialog>
      ) : null}
    </div>
  );
}
