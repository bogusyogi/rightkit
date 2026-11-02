import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import ManagedToolHealthPanel from "./ManagedToolHealthPanel.js";

describe("ManagedToolHealthPanel", () => {
  it("shows the installed version after the real probe resolves", async () => {
    const probe = vi.fn().mockResolvedValue({ installed: true, version: "1.2.3", output: "Claude Code 1.2.3" });
    render(<ManagedToolHealthPanel toolId="claude-code" label="Claude Code" probe={probe} />);
    await waitFor(() => expect(screen.getByText("v1.2.3")).toBeTruthy());
    expect(screen.getByText("Installed")).toBeTruthy();
  });

  it("preserves missing-install output and exposes remediation actions", async () => {
    const probe = vi.fn().mockResolvedValue({ installed: false, output: "No such file or directory" });
    const onSelectInstallation = vi.fn();
    const onRemediate = vi.fn();
    render(<ManagedToolHealthPanel toolId="claude-code" label="Claude Code" probe={probe} onSelectInstallation={onSelectInstallation} onRemediate={onRemediate} installUrl="https://example.test/install" />);
    await waitFor(() => expect(screen.getByText(/was not found/)).toBeTruthy());
    expect(screen.getByText("No such file or directory")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Select installation" }));
    fireEvent.click(screen.getByRole("button", { name: "Install or repair" }));
    expect(onSelectInstallation).toHaveBeenCalledTimes(1);
    expect(onRemediate).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("link", { name: /Installation guide/ })).toHaveAttribute("href", "https://example.test/install");
  });

  it("renders probe failures without claiming that the tool is missing", async () => {
    const probe = vi.fn().mockRejectedValue(new Error("probe failed"));
    render(<ManagedToolHealthPanel toolId="claude-code" probe={probe} />);
    await waitFor(() => expect(screen.getByText("probe failed")).toBeTruthy());
    expect(screen.getByText("Unknown")).toBeTruthy();
    expect(screen.queryByText(/was not found/)).toBeNull();
  });
});
