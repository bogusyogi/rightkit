// @vitest-environment happy-dom
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import CliUpdatePanel from "./CliUpdatePanel.js";

const updates = [
  {
    providerId: "codex",
    displayName: "Codex CLI",
    installedVersion: "1.2.3",
    latestVersion: null,
    state: "unavailable" as const,
    canInstall: false as const,
    reason: "The daemon does not currently expose a latest-version check for this CLI.",
  },
];

describe("CliUpdatePanel", () => {
  it("shows truthful unavailable status and keeps install out of the panel", () => {
    render(<CliUpdatePanel updates={updates} />);
    expect(screen.getByRole("region", { name: "CLI updates" })).toBeTruthy();
    expect(screen.getByText("Status unavailable")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /install/i })).toBeNull();
  });

  it("delegates refresh to the parent connector", () => {
    const onRefresh = vi.fn();
    render(<CliUpdatePanel updates={[]} onRefresh={onRefresh} />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(onRefresh).toHaveBeenCalledOnce();
  });
});
