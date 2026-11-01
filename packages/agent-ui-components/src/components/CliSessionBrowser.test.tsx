import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  CliSessionBrowserClient,
  CliSessionSummary,
  DiscoverResult,
  ImportResult,
  SyncResult,
} from "./CliSessionBrowser";
import CliSessionBrowser from "./CliSessionBrowser";

const session = (overrides: Partial<CliSessionSummary> = {}): CliSessionSummary => ({
  sessionId: "claude-1",
  agent: "claude",
  cwd: "D:/work/alpha",
  firstPrompt: "Fix the alpha parser",
  startedAt: "2026-08-29T09:00:00Z",
  lastActivityAt: "2026-08-29T09:20:00Z",
  messageCount: 4,
  model: "claude-sonnet",
  fileSize: 2048,
  filePath: "D:/home/.claude/session.jsonl",
  hasSubagents: false,
  alreadyImported: false,
  ...overrides,
});

const discovery = (sessions: CliSessionSummary[]): DiscoverResult => ({
  sessions,
  total: sessions.length,
  truncated: false,
});

const imported: ImportResult = {
  runId: "run-1",
  sessionId: "claude-1",
  eventsImported: 4,
  eventsSkipped: 0,
  usageIncomplete: false,
  skippedSubtypes: {},
};

const synced: SyncResult = { newEvents: 2, usageIncomplete: false };

function makeClient(overrides: Partial<CliSessionBrowserClient> = {}): CliSessionBrowserClient {
  return {
    discoverSessions: vi.fn(async () => discovery([session()])),
    importSession: vi.fn(async () => imported),
    syncSession: vi.fn(async () => synced),
    ...overrides,
  };
}

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

describe("CliSessionBrowser", () => {
  it("discovers sessions for the selected agent and renders the donor summary fields", async () => {
    const client = makeClient();

    render(
      <CliSessionBrowser client={client} cwd="/" onClose={vi.fn()} onImported={vi.fn()} />,
    );

    await waitFor(() => expect(screen.getByTestId("cli-session-claude-1")).toBeTruthy());
    expect(client.discoverSessions).toHaveBeenCalledWith("/", "claude");
    expect(screen.getByText("Fix the alpha parser")).toBeTruthy();
    expect(screen.getByText("4 messages")).toBeTruthy();
    expect(screen.getByText("2.0 KB")).toBeTruthy();
  });

  it("filters by prompt, model, and project and persists the agent toggle", async () => {
    const sessions = [
      session(),
      session({ sessionId: "codex-2", cwd: "D:/work/beta", firstPrompt: "Review the beta API", model: "gpt-5" }),
    ];
    const client = makeClient({ discoverSessions: vi.fn(async () => discovery(sessions)) });

    render(
      <CliSessionBrowser client={client} cwd="/" onClose={vi.fn()} onImported={vi.fn()} />,
    );
    await waitFor(() => expect(screen.getByTestId("cli-session-claude-1")).toBeTruthy());
    expect(screen.getByTestId("cli-session-codex-2")).toBeTruthy();

    fireEvent.change(screen.getByRole("searchbox", { name: "Search CLI sessions" }), {
      target: { value: "gpt-5" },
    });
    expect(screen.queryByTestId("cli-session-claude-1")).toBeNull();
    expect(screen.getByTestId("cli-session-codex-2")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "work/beta (1)" }));
    expect(screen.getByTestId("cli-session-codex-2")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Codex" }));
    expect(window.localStorage.getItem("cliImport_agent")).toBe("codex");
  });

  it("imports a session using its own cwd, refreshes, and navigates to the imported run", async () => {
    const onImported = vi.fn();
    const client = makeClient();
    render(<CliSessionBrowser client={client} cwd="D:/fallback" onClose={vi.fn()} onImported={onImported} />);

    await waitFor(() => screen.getByTestId("cli-session-claude-1"));
    fireEvent.click(screen.getByRole("button", { name: "Import" }));

    await waitFor(() => expect(onImported).toHaveBeenCalledWith("run-1"));
    expect(client.importSession).toHaveBeenCalledWith("claude-1", "D:/work/alpha", "claude");
    expect(client.discoverSessions).toHaveBeenCalledTimes(2);
  });

  it("syncs an imported session and opens the existing run without importing again", async () => {
    const importedSession = session({ alreadyImported: true, existingRunId: "run-existing" });
    const onImported = vi.fn();
    const client = makeClient({ discoverSessions: vi.fn(async () => discovery([importedSession])) });
    render(<CliSessionBrowser client={client} cwd="/" onClose={vi.fn()} onImported={onImported} />);

    await waitFor(() => screen.getByTestId("cli-session-claude-1"));
    fireEvent.click(screen.getByRole("button", { name: "Sync" }));
    await waitFor(() => expect(client.syncSession).toHaveBeenCalledWith("run-existing"));
    expect(client.importSession).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    expect(onImported).toHaveBeenCalledWith("run-existing");
  });

  it("imports every filtered new session and navigates to the last run", async () => {
    const sessions = [session(), session({ sessionId: "claude-2", firstPrompt: "Second prompt" })];
    const onImported = vi.fn();
    const client = makeClient({
      discoverSessions: vi.fn(async () => discovery(sessions)),
      importSession: vi
        .fn()
        .mockResolvedValueOnce(imported)
        .mockResolvedValueOnce({ ...imported, runId: "run-2", sessionId: "claude-2" }),
    });
    render(<CliSessionBrowser client={client} cwd="/" onClose={vi.fn()} onImported={onImported} />);

    await waitFor(() => screen.getByTestId("cli-session-claude-1"));
    fireEvent.click(screen.getByRole("button", { name: "Import all new (2)" }));

    await waitFor(() => expect(onImported).toHaveBeenCalledWith("run-2"));
    expect(client.importSession).toHaveBeenNthCalledWith(1, "claude-1", "D:/work/alpha", "claude");
    expect(client.importSession).toHaveBeenNthCalledWith(2, "claude-2", "D:/work/alpha", "claude");
  });

  it("closes from Escape, the close button, and the backdrop and surfaces discovery errors", async () => {
    const onClose = vi.fn();
    const client = makeClient({ discoverSessions: vi.fn(async () => { throw new Error("CLI unavailable"); }) });
    render(<CliSessionBrowser client={client} cwd="/" onClose={onClose} onImported={vi.fn()} />);

    await waitFor(() => expect(screen.getByText("Error: CLI unavailable")).toBeTruthy());
    // ModalShell plays its exit transition before it calls onClose; each dismissal runs once.
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    cleanup();

    const onCloseButton = vi.fn();
    render(
      <CliSessionBrowser client={client} cwd="/" onClose={onCloseButton} onImported={vi.fn()} />,
    );
    await waitFor(() => expect(screen.getByText("Error: CLI unavailable")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await waitFor(() => expect(onCloseButton).toHaveBeenCalledTimes(1));
    cleanup();

    const onBackdrop = vi.fn();
    render(<CliSessionBrowser client={client} cwd="/" onClose={onBackdrop} onImported={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("Error: CLI unavailable")).toBeTruthy());
    fireEvent.mouseDown(screen.getByRole("dialog").parentElement as HTMLElement);
    await waitFor(() => expect(onBackdrop).toHaveBeenCalledTimes(1));
  });
});
