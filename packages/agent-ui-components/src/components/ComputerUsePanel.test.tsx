import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import ComputerUsePanel, { type ComputerUseStatus } from "./ComputerUsePanel.js";

const status: ComputerUseStatus = {
  platform: "darwin",
  platformSupported: true,
  installed: true,
  version: "computer-use 0.5.1",
  ready: false,
  canGrant: true,
  checks: [{ label: "Driver", status: "error", message: "Screen Recording is missing" }],
  accessibility: true,
  screenRecording: false,
  source: { attribution: "computer-use driver" },
  error: null,
};

describe("ComputerUsePanel", () => {
  it("projects the macOS permission rows and failing checks", () => {
    render(<ComputerUsePanel status={status} loading={false} />);
    expect(screen.getByText("Computer Use readiness")).toBeTruthy();
    expect(screen.getByText("Accessibility")).toBeTruthy();
    expect(screen.getByText("Granted")).toBeTruthy();
    expect(screen.getByText("Not granted")).toBeTruthy();
    expect(screen.getByText(/Screen Recording is missing/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Grant permissions" })).toBeTruthy();
  });

  it("polls an injected grant action, then refreshes and reports the change", async () => {
    const getStatus = vi.fn()
      .mockResolvedValueOnce(status)
      .mockResolvedValueOnce({ ...status, ready: true, screenRecording: true });
    const getActionStatus = vi.fn().mockResolvedValue({ running: false });
    const onConfiguredChange = vi.fn();
    render(<ComputerUsePanel adapter={{ getStatus, grantPermissions: vi.fn().mockResolvedValue({ ok: true, actionId: "grant-1" }), getActionStatus }} onConfiguredChange={onConfiguredChange} />);

    await waitFor(() => expect(screen.getByRole("button", { name: "Grant permissions" })).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Grant permissions" }));
    await waitFor(() => expect(onConfiguredChange).toHaveBeenCalledTimes(1), { timeout: 3000 });
    expect(getActionStatus).toHaveBeenCalledWith("grant-1", 200);
    expect(getStatus).toHaveBeenCalledTimes(2);
  });

  it("does not pretend unsupported or missing hosts are ready", () => {
    const { rerender } = render(<ComputerUsePanel status={{ ...status, installed: false }} loading={false} />);
    expect(screen.getByText(/Install the computer-use backend/)).toBeTruthy();
    rerender(<ComputerUsePanel status={{ ...status, platformSupported: false }} loading={false} />);
    expect(screen.getByText(/not supported on this platform/)).toBeTruthy();
  });
});
