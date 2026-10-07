// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";
import { ImageViewerShell } from "./ImageViewerShell.js";
import { initialImageEdit } from "./imageEdit.js";
import { initialImageTransform } from "./imageViewerState.js";
import { TBtn } from "./toolbar.js";

afterEach(cleanup);

const noop = () => {};

function shellProps(over: Record<string, unknown> = {}) {
  return {
    displayName: "a.png",
    displaySrc: "/a.png",
    frameRef: { current: null },
    siblingIndex: 0,
    siblingTotal: 1,
    canPrev: false,
    canNext: false,
    dragging: false,
    panning: false,
    ready: true,
    edited: false,
    canUndoEdit: false,
    canRedoEdit: false,
    view: initialImageTransform(),
    stageSize: null,
    imageSize: null,
    tool: "view" as const,
    edit: initialImageEdit(),
    cropDraft: null,
    cropRatio: null,
    selectedText: null,
    selectedLayer: null,
    adapters: {},
    onPageSibling: noop,
    onWheel: noop,
    onPointerDown: noop,
    onPointerMove: noop,
    onPointerStop: noop,
    onKeyPan: noop,
    onImageLoad: noop,
    onCropChange: noop,
    onTextSelect: noop,
    onTextChange: noop,
    onTool: noop,
    onFit: noop,
    onActual: noop,
    onZoomIn: noop,
    onZoomOut: noop,
    onZoomPreset: noop,
    onRotateLeft: noop,
    onRotateRight: noop,
    onFlipX: noop,
    onFlipY: noop,
    onReset: noop,
    onUndo: noop,
    onRedo: noop,
    onCropRatio: noop,
    onCropApply: noop,
    onCropCancel: noop,
    onCropClear: noop,
    onAddText: noop,
    onTextPatch: noop,
    onTextDelete: noop,
    onAdjustment: noop,
    onResize: noop,
    ...over,
  } as unknown as Parameters<typeof ImageViewerShell>[0];
}

describe("portal split", () => {
  it("keeps the capsule and panels in the viewer by default", () => {
    const { container } = render(<ImageViewerShell {...shellProps({ tool: "crop" })} />);
    const viewer = container.querySelector(".rk-iv")!;
    expect(viewer.querySelector(".rk-iv-bar")).not.toBeNull();
    expect(viewer.querySelector(".rk-iv-panel")).not.toBeNull();
  });

  it("portals ONLY the capsule row to toolbarContainer; panels stay in the viewer", () => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    const { container } = render(
      <ImageViewerShell {...shellProps({ tool: "crop", toolbarContainer: host })} />,
    );
    const viewer = container.querySelector(".rk-iv")!;
    expect(host.querySelector(".rk-iv-bar")).not.toBeNull();
    expect(host.querySelector('[role="toolbar"]')).not.toBeNull();
    expect(host.querySelector(".rk-iv-panel")).toBeNull();
    expect(host.querySelector(".rk-iv-toolbar")).toBeNull();
    expect(viewer.querySelector(".rk-iv-bar")).toBeNull();
    expect(viewer.querySelector(".rk-iv-panel")).not.toBeNull();
    host.remove();
  });

  it("panelsContainer portals the panels separately", () => {
    const bar = document.createElement("div");
    const panels = document.createElement("div");
    document.body.append(bar, panels);
    const { container } = render(
      <ImageViewerShell
        {...shellProps({ tool: "adjust", toolbarContainer: bar, panelsContainer: panels })}
      />,
    );
    expect(bar.querySelector(".rk-iv-bar")).not.toBeNull();
    expect(bar.querySelector(".rk-iv-panel")).toBeNull();
    expect(panels.querySelector(".rk-iv-panel")).not.toBeNull();
    expect(container.querySelector(".rk-iv-panel")).toBeNull();
    expect(container.querySelector(".rk-iv-bar")).toBeNull();
    bar.remove();
    panels.remove();
  });

  it("panelsContainer alone leaves the capsule in the viewer", () => {
    const panels = document.createElement("div");
    document.body.appendChild(panels);
    const { container } = render(
      <ImageViewerShell {...shellProps({ tool: "crop", panelsContainer: panels })} />,
    );
    expect(container.querySelector(".rk-iv-bar")).not.toBeNull();
    expect(panels.querySelector(".rk-iv-panel")).not.toBeNull();
    panels.remove();
  });
});

describe("capsule toolbar semantics", () => {
  it("is role=toolbar with default and custom labels", () => {
    const a = render(<ImageViewerShell {...shellProps()} />);
    expect(a.getByRole("toolbar", { name: "Image tools" })).toBeTruthy();
    a.unmount();
    const b = render(<ImageViewerShell {...shellProps({ toolbarLabel: "Bild" })} />);
    expect(b.getByRole("toolbar", { name: "Bild" })).toBeTruthy();
  });

  it("opt-in rovingFocus: one tab stop, arrows/Home/End move focus, disabled skipped", () => {
    const { getByRole } = render(<ImageViewerShell {...shellProps({ rovingFocus: true })} />);
    const bar = getByRole("toolbar");
    const items = () =>
      Array.from(bar.querySelectorAll<HTMLElement>("button, select")).filter(
        (el) => !(el as HTMLButtonElement).disabled,
      );
    const stops = () => items().filter((el) => el.tabIndex === 0);
    expect(stops()).toHaveLength(1);
    expect(stops()[0]).toBe(items()[0]);
    // undo/redo are disabled and must be skipped, not roving stops.
    expect(bar.querySelectorAll("button:disabled").length).toBeGreaterThan(0);
    for (const el of bar.querySelectorAll<HTMLButtonElement>("button:disabled")) {
      expect(el.tabIndex).toBe(-1);
    }

    items()[0]!.focus();
    fireEvent.keyDown(document.activeElement!, { key: "ArrowRight" });
    expect(document.activeElement).toBe(items()[1]);
    expect(stops()).toEqual([items()[1]]);
    fireEvent.keyDown(document.activeElement!, { key: "ArrowLeft" });
    expect(document.activeElement).toBe(items()[0]);
    fireEvent.keyDown(document.activeElement!, { key: "End" });
    expect(document.activeElement).toBe(items().at(-1));
    fireEvent.keyDown(document.activeElement!, { key: "ArrowRight" });
    expect(document.activeElement).toBe(items()[0]);
    fireEvent.keyDown(document.activeElement!, { key: "ArrowLeft" });
    expect(document.activeElement).toBe(items().at(-1));
    fireEvent.keyDown(document.activeElement!, { key: "Home" });
    expect(document.activeElement).toBe(items()[0]);
  });

  it("default: every enabled control is a Tab stop and arrow keys do nothing", () => {
    const { getByRole } = render(<ImageViewerShell {...shellProps()} />);
    const bar = getByRole("toolbar", { name: "Image tools" });
    const controls = Array.from(bar.querySelectorAll<HTMLElement>("button, select"));
    const enabled = controls.filter((el) => !(el as HTMLButtonElement).disabled);
    expect(enabled.length).toBeGreaterThan(2);
    for (const el of enabled) expect(el.tabIndex).toBe(0);
    enabled[0]!.focus();
    const notPrevented = fireEvent.keyDown(enabled[0]!, { key: "ArrowRight" });
    expect(notPrevented).toBe(true);
    expect(document.activeElement).toBe(enabled[0]);
    fireEvent.keyDown(enabled[0]!, { key: "End" });
    expect(document.activeElement).toBe(enabled[0]);
  });

  it("arrow keys leave the zoom select without changing its value (rovingFocus)", () => {
    const { getByLabelText } = render(<ImageViewerShell {...shellProps({ rovingFocus: true })} />);
    const select = getByLabelText("Zoom percent") as HTMLSelectElement;
    select.focus();
    const ev = fireEvent.keyDown(select, { key: "ArrowRight" });
    expect(ev).toBe(false); // default prevented
    expect(document.activeElement).not.toBe(select);
  });
});

describe("button activation", () => {
  it("runs on mousedown with preventDefault; the following pointer click does not re-run", () => {
    const run = vi.fn();
    const { getByRole } = render(<TBtn onRun={run} title="Go" label="go" />);
    const btn = getByRole("button", { name: "Go" });
    const notPrevented = fireEvent.mouseDown(btn, { button: 0 });
    expect(notPrevented).toBe(false); // preventDefault called -> focus stays put
    expect(run).toHaveBeenCalledTimes(1);
    fireEvent.click(btn, { detail: 1 });
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("does not steal focus on mousedown", () => {
    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();
    const { getByRole } = render(<TBtn onRun={() => {}} title="Go" label="go" />);
    fireEvent.mouseDown(getByRole("button"), { button: 0 });
    expect(document.activeElement).toBe(input);
    input.remove();
  });

  it("runs on click only when detail === 0 (keyboard / assistive tech)", () => {
    const run = vi.fn();
    const { getByRole } = render(<TBtn onRun={run} title="Go" label="go" />);
    fireEvent.click(getByRole("button"), { detail: 0 });
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("any mouse button activates on mousedown, like ViewRight's TBtn", () => {
    const run = vi.fn();
    const { getByRole } = render(<TBtn onRun={run} title="Go" label="go" />);
    fireEvent.mouseDown(getByRole("button"), { button: 2 });
    expect(run).toHaveBeenCalledTimes(1);
    fireEvent.mouseDown(getByRole("button"), { button: 1 });
    expect(run).toHaveBeenCalledTimes(2);
  });

  it("ignores disabled buttons", () => {
    const run = vi.fn();
    const { getByRole, rerender } = render(<TBtn onRun={run} title="Go" label="go" />);
    rerender(<TBtn disabled onRun={run} title="Go" label="go" />);
    fireEvent.mouseDown(getByRole("button"), { button: 0 });
    fireEvent.click(getByRole("button"), { detail: 0 });
    expect(run).not.toHaveBeenCalled();
  });
});
