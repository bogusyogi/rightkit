// @vitest-environment happy-dom
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { BrowserPane } from "./BrowserPane";
import { useBrowserWorkspaceStore } from "@rightkit/agent-ui-core/state/browserStore";

const browserActions = vi.hoisted(() => {
  const noop = vi.fn().mockResolvedValue(undefined);
  return {
    refresh: noop,
    refreshAnnotations: noop,
    refreshSignals: noop,
    refreshLive: noop,
    back: vi.fn().mockResolvedValue(undefined),
    forward: vi.fn().mockResolvedValue(undefined),
    screenshot: noop,
    reload: noop,
    navigate: noop,
    open: vi.fn().mockResolvedValue({ sessionId: "browser-1" }),
    close: noop,
    setViewport: noop,
    setZoom: noop,
    pause: noop,
    resume: noop,
    takeControl: noop,
    releaseControl: noop,
    setHostPolicy: noop,
    setProfile: vi.fn().mockResolvedValue(undefined),
    createAnnotation: noop,
    sendAnnotation: noop,
  };
});

vi.mock("@rightkit/agent-ui-hooks/hooks/browser/useBrowserWorkspace", () => ({
  useBrowserWorkspace: () => ({ client: {}, ...browserActions }),
}));

const summary = {
  paused: false,
  profileKind: "ephemeral" as const,
  profileLabel: null,
  viewport: null,
  actionLogLen: 0,
  sessions: [
    {
      sessionId: "browser-1",
      ownerSessionId: "code-session-1",
      mode: "headless" as const,
      profile: { kind: "ephemeral", label: null, ephemeral: true },
      controlLease: { owner: null, generation: 1 },
      url: "https://example.com",
      title: "Example",
      consoleErrorCount: 0,
      zoom: 1,
    },
  ],
};

describe("BrowserPane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useBrowserWorkspaceStore.getState().reset();
  });

  it("exposes working Back and Forward controls for the selected browser session", async () => {
    render(
      <BrowserPane baseUrl="http://127.0.0.1:9999" token="token" sessionId="code-session-1" />,
    );
    act(() => useBrowserWorkspaceStore.getState().setSummary(summary));

    const back = (await screen.findByRole("button", { name: "Back" })) as HTMLButtonElement;
    await waitFor(() => expect(back.disabled).toBe(false));
    fireEvent.click(back);
    fireEvent.click(screen.getByRole("button", { name: "Forward" }));

    await waitFor(() => expect(browserActions.back).toHaveBeenCalledWith("browser-1"));
    await waitFor(() => expect(browserActions.forward).toHaveBeenCalledWith("browser-1"));
  });

  it("keeps policy and diagnostics out of the default preview workspace", () => {
    render(
      <BrowserPane baseUrl="http://127.0.0.1:9999" token="token" sessionId="code-session-1" />,
    );

    expect(screen.getByText("Start browsing")).toBeTruthy();
    expect((screen.getByTestId("browser-advanced-controls") as HTMLDetailsElement).open).toBe(
      false,
    );
    expect((screen.getByTestId("browser-diagnostics") as HTMLDetailsElement).open).toBe(false);
  });

  it("restores a visual for an existing browser session", async () => {
    render(
      <BrowserPane baseUrl="http://127.0.0.1:9999" token="token" sessionId="code-session-1" />,
    );
    act(() => useBrowserWorkspaceStore.getState().setSummary(summary));

    await waitFor(() => expect(browserActions.screenshot).toHaveBeenCalledWith("browser-1"));
    expect(browserActions.refreshSignals).toHaveBeenCalledWith("browser-1");
  });

  it("renders daemon lease truth & refreshes live frames only during human control", async () => {
    render(
      <BrowserPane baseUrl="http://127.0.0.1:9999" token="token" sessionId="code-session-1" />,
    );
    act(() =>
      useBrowserWorkspaceStore.getState().setSummary({
        ...summary,
        sessions: [
          {
            ...summary.sessions[0],
            mode: "visible",
            controlLease: { owner: "workspace-user", generation: 7 },
          },
        ],
      }),
    );

    expect(await screen.findByText("Visible BrowserSession")).toBeTruthy();
    expect(screen.getByText("You control this session")).toBeTruthy();
    expect(screen.getByText("· lease 7")).toBeTruthy();
    await waitFor(() => expect(browserActions.refreshLive).toHaveBeenCalledWith("browser-1"));
  });

  it("clears browser-owned state when the App session changes", async () => {
    const { rerender } = render(
      <BrowserPane baseUrl="http://127.0.0.1:9999" token="token" sessionId="code-session-1" />,
    );
    act(() => {
      useBrowserWorkspaceStore.getState().setSummary(summary);
      useBrowserWorkspaceStore.getState().setError("old session error");
      useBrowserWorkspaceStore.getState().setConsole([{ level: "error", text: "old" }]);
    });

    rerender(
      <BrowserPane baseUrl="http://127.0.0.1:9999" token="token" sessionId="code-session-2" />,
    );
    await waitFor(() => {
      const state = useBrowserWorkspaceStore.getState();
      expect(state.summary).toBeNull();
      expect(state.lastError).toBeNull();
      expect(state.console).toEqual([]);
    });
  });

  it("requires an in-app confirmation before sharing a named browser profile", async () => {
    render(
      <BrowserPane baseUrl="http://127.0.0.1:9999" token="token" sessionId="code-session-1" />,
    );
    fireEvent.click(screen.getByText("Advanced controls"));
    fireEvent.change(screen.getByRole("combobox", { name: "Browser profile" }), {
      target: { value: "named_shared" },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "Profile label" }), {
      target: { value: "Team staging" },
    });
    const save = screen.getByRole("button", { name: "Save profile" });
    const profileCallsBeforeConfirm = browserActions.setProfile.mock.calls.length;
    save.focus();
    fireEvent.click(save);

    expect(await screen.findByRole("alertdialog")).toBeTruthy();
    expect(browserActions.setProfile).toHaveBeenCalledTimes(profileCallsBeforeConfirm);
    fireEvent.keyDown(screen.getByRole("alertdialog"), { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(document.activeElement).toBe(save);
  });

  it("prevents duplicate profile sharing and keeps a failed confirmation recoverable", async () => {
    let rejectProfile: ((error: Error) => void) | null = null;
    browserActions.setProfile.mockImplementationOnce(
      () =>
        new Promise<void>((_resolve, reject) => {
          rejectProfile = reject;
        }),
    );
    render(
      <BrowserPane baseUrl="http://127.0.0.1:9999" token="token" sessionId="code-session-1" />,
    );
    fireEvent.click(screen.getByText("Advanced controls"));
    fireEvent.change(screen.getByRole("combobox", { name: "Browser profile" }), {
      target: { value: "named_shared" },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "Profile label" }), {
      target: { value: "Team staging" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save profile" }));
    const share = await screen.findByRole("button", { name: "Share profile" });
    fireEvent.click(share);
    expect((share as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(share);
    expect(browserActions.setProfile).toHaveBeenCalledTimes(1);

    await act(async () => rejectProfile?.(new Error("Profile service unavailable")));
    expect(await screen.findByRole("alert")).toHaveTextContent("Profile service unavailable");
    expect(
      (screen.getByRole("button", { name: "Share profile" }) as HTMLButtonElement).disabled,
    ).toBe(false);
  });
});
