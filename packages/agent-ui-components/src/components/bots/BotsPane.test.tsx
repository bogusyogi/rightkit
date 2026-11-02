// @vitest-environment happy-dom
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { BotsPane } from "./BotsPane.js";

const daemon = vi.hoisted(() => ({
  listBots: vi.fn(),
  getBot: vi.fn(),
  createBot: vi.fn(),
  deleteBot: vi.fn(),
  runBot: vi.fn(),
  updateBot: vi.fn(),
  exportBot: vi.fn(),
  importBot: vi.fn(),
}));

vi.mock("@rightkit/agent-ui-core/lib/daemon", () => daemon);

describe("BotsPane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    daemon.listBots.mockResolvedValue([{ id: "reviewer", name: "Reviewer" }]);
    daemon.getBot.mockResolvedValue({
      id: "reviewer",
      name: "Reviewer",
      version: 3,
      rolePack: "reviewer",
      model: "provider/model",
      description: "Independent review",
      instructions: "Review result",
      enabled: true,
      concurrency: "skip",
      concurrencyCap: 1,
      browser: { mode: "disabled" },
      tools: ["read_file"],
      triggers: [{ kind: "cron", expression: "0 9 * * *" }],
    });
  });

  it("renders versioned daemon profile, trigger count, controls, & authority-free import", async () => {
    render(<BotsPane config={{} as never} workspaceRoot="D:/work" />);

    expect(await screen.findByText("Reviewer")).toBeTruthy();
    await waitFor(() => expect(screen.getByText(/provider\/model · v3/)).toBeTruthy());
    expect(screen.getByText("1", { selector: "dd" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Run now" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Pause" })).toBeTruthy();
    expect(screen.getByText("Import authority-free package")).toBeTruthy();
  });

  it("opens creation editor & keeps trigger edits in one profile save", async () => {
    render(<BotsPane config={{} as never} workspaceRoot="D:/work" />);
    await screen.findByText("Reviewer");
    fireEvent.click(screen.getByRole("button", { name: "Create bot" }));
    expect(screen.getByText("Create bot profile")).toBeTruthy();
    fireEvent.change(screen.getByRole("textbox", { name: "Trigger expression" }), {
      target: { value: "0 9 * * *" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add trigger" }));
    expect(screen.getByText("cron: 0 9 * * *")).toBeTruthy();
  });
});
