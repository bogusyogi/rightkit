import { renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { useManagedToolHealth } from "./useManagedToolHealth";

describe("useManagedToolHealth", () => {
  it("checks on mount and exposes the installed version", async () => {
    const probe = vi.fn().mockResolvedValue({ installed: true, version: "1.2.3", output: "ok" });
    const { result } = renderHook(() => useManagedToolHealth({ toolId: "claude-code", probe }));
    await waitFor(() => expect(result.current.health?.version).toBe("1.2.3"));
    expect(probe).toHaveBeenCalledTimes(1);
    expect(result.current.error).toBeNull();
  });

  it("reports missing installations and invokes remediation notification once", async () => {
    const probe = vi.fn().mockResolvedValue({ installed: false, output: "No such file or directory" });
    const onNotInstalled = vi.fn();
    const { result } = renderHook(() => useManagedToolHealth({ toolId: "claude-code", probe, onNotInstalled }));
    await waitFor(() => expect(result.current.health?.installed).toBe(false));
    expect(onNotInstalled).toHaveBeenCalledWith(expect.objectContaining({ installed: false, toolId: "claude-code" }));
  });

  it("keeps probe failures separate from a missing-install result", async () => {
    const probe = vi.fn().mockRejectedValue(new Error("permission denied"));
    const { result } = renderHook(() => useManagedToolHealth({ toolId: "claude-code", probe }));
    await waitFor(() => expect(result.current.error).toBe("permission denied"));
    expect(result.current.health).toBeNull();
  });
});
