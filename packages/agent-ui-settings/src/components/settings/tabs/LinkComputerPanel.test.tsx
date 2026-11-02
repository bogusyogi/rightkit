// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import LinkComputerPanel from "./LinkComputerPanel.js";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

function nativePlatform(value: string | undefined) {
  (window as { __RIGHTKIT_PLATFORM__?: string }).__RIGHTKIT_PLATFORM__ = value;
}

describe("LinkComputerPanel", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });
  afterEach(() => {
    cleanup();
    nativePlatform(undefined);
  });

  it("is hidden on macOS and once a license is active", () => {
    nativePlatform("darwin");
    const mac = render(<LinkComputerPanel active={false} />);
    expect(mac.container.querySelector("[data-testid='link-computer-panel']")).toBeNull();
    cleanup();

    nativePlatform("windows");
    const activeWindows = render(<LinkComputerPanel active />);
    expect(activeWindows.container.querySelector("[data-testid='link-computer-panel']")).toBeNull();
    // The platform comes from the injected global; no native command is asked for it.
    expect(invoke).not.toHaveBeenCalled();
  });

  it("redeems the uppercased code and shows the link", async () => {
    nativePlatform("windows");
    vi.mocked(invoke).mockResolvedValue({ status: "active", tier: "pro", features: [] });
    render(<LinkComputerPanel active={false} />);
    const input = (await screen.findByLabelText("Link code")) as HTMLInputElement;
    const button = screen.getByRole("button", { name: "Link this computer" });
    expect((button as HTMLButtonElement).disabled).toBe(true);

    fireEvent.change(input, { target: { value: " ab12-cd34 " } });
    expect(input.value).toBe(" AB12-CD34 ");
    fireEvent.click(button);

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("license_windows_link_redeem", {
        code: "AB12-CD34",
        deviceLabel: null,
      }),
    );
    expect(await screen.findByText("This computer is linked.")).toBeTruthy();
  });

  it("shows the owner's message for a code it refused", async () => {
    nativePlatform("windows");
    vi.mocked(invoke).mockRejectedValue({
      code: "rejected",
      reason: "deviceLimit",
      message: "deviceLimit",
    });
    render(<LinkComputerPanel active={false} />);
    const input = await screen.findByLabelText("Link code");
    fireEvent.change(input, { target: { value: "zzzz" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(
      await screen.findByText(
        "This licence is already active on the maximum number of devices.",
      ),
    ).toBeTruthy();
    expect(screen.queryByText("This computer is linked.")).toBeNull();
  });
});
