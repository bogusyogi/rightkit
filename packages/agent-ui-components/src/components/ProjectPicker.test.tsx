// @vitest-environment happy-dom
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { open } from "@tauri-apps/plugin-dialog";
import ProjectPicker from "./ProjectPicker";
import { useProjectRecentsStore } from "@rightkit/agent-ui-core/state/projectRecentsStore";

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));

const nativeOpen = vi.mocked(open);

beforeEach(() => {
  nativeOpen.mockReset();
  useProjectRecentsStore.getState().clear();
});

describe("ProjectPicker", () => {
  it("ports the native directory picker and records the selected project", async () => {
    nativeOpen.mockResolvedValue("/work/code-right");
    const changed: string[] = [];

    render(<ProjectPicker workspaceRoot="" onWorkspaceChange={(path) => changed.push(path)} />);
    fireEvent.click(screen.getByTestId("project-picker-open"));

    await waitFor(() => expect(changed).toEqual(["/work/code-right"]));
    expect(nativeOpen).toHaveBeenCalledWith({
      directory: true,
      multiple: false,
      title: "Choose project directory",
    });
    expect(screen.getByText("code-right")).toBeTruthy();
  });

  it("opens a typed path without invoking the native dialog", () => {
    const changed: string[] = [];

    render(<ProjectPicker workspaceRoot="" onWorkspaceChange={(path) => changed.push(path)} />);
    fireEvent.change(screen.getByTestId("project-picker-path"), {
      target: { value: "  /work/fixture  " },
    });
    fireEvent.click(screen.getByTestId("project-picker-path-open"));

    expect(changed).toEqual(["/work/fixture"]);
    expect(nativeOpen).not.toHaveBeenCalled();
  });

  it("rejects an empty typed path", () => {
    const changed = vi.fn();

    render(<ProjectPicker workspaceRoot="" onWorkspaceChange={changed} />);
    fireEvent.click(screen.getByTestId("project-picker-path-open"));

    expect(changed).not.toHaveBeenCalled();
    expect(screen.getByTestId("project-picker-error")).toBeTruthy();
  });

  it("does not change context when the native picker is cancelled", async () => {
    nativeOpen.mockResolvedValue(null);
    const changed = vi.fn();

    render(<ProjectPicker workspaceRoot="/work/current" onWorkspaceChange={changed} />);
    fireEvent.click(screen.getByTestId("project-picker-open"));

    await waitFor(() => expect(nativeOpen).toHaveBeenCalled());
    expect(changed).not.toHaveBeenCalled();
    expect(screen.getByTestId("project-picker-empty")).toBeTruthy();
  });

  it("renders grouped recents and keeps pin/remove actions separate from selection", () => {
    const store = useProjectRecentsStore.getState();
    store.addOrTouch("/work/one", "One");
    store.addOrTouch("/work/two", "Two");
    store.setGroup("/work/one", "Clients");

    const changed = vi.fn();
    render(<ProjectPicker workspaceRoot="/work/one" onWorkspaceChange={changed} />);

    expect(screen.getByRole("region", { name: "Clients" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Pin One" }));
    expect(useProjectRecentsStore.getState().projects.find((p) => p.label === "One")?.pinned).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Remove Two from recents" }));
    expect(useProjectRecentsStore.getState().projects.some((p) => p.label === "Two")).toBe(false);
    expect(changed).not.toHaveBeenCalled();
  });
});
