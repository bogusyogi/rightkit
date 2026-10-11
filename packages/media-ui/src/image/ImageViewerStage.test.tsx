// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { createRef } from "react";
import { ImageViewerStage } from "./ImageViewerStage.js";

afterEach(cleanup);

function mount() {
  const handlers = {
    onKeyPan: vi.fn(),
    onZoomIn: vi.fn(),
    onZoomOut: vi.fn(),
    onActual: vi.fn(),
    onFit: vi.fn(),
  };
  render(
    <div>
      <ImageViewerStage
        displayName="cat.png"
        displaySrc="/cat.png"
        frameRef={createRef<HTMLDivElement>()}
        siblingIndex={0}
        siblingTotal={1}
        canPrev={false}
        canNext={false}
        onPageSibling={() => {}}
        dragging={false}
        panning={false}
        view={{ scale: 1, offsetX: 0, offsetY: 0 } as never}
        stageSize={null}
        stageTransform=""
        imageSize={null}
        tool="none"
        edit={{ text: [], crop: null } as never}
        cropDraft={null}
        cropRatio={null}
        filter=""
        selectedText={null}
        adapters={{} as never}
        onWheel={() => {}}
        onPointerDown={() => {}}
        onPointerMove={() => {}}
        onPointerStop={() => {}}
        onDoubleClick={() => {}}
        onImageLoad={() => {}}
        onCropChange={() => {}}
        onTextSelect={() => {}}
        onTextChange={() => {}}
        {...handlers}
      />
      <input aria-label="outside" />
    </div>,
  );
  return { stage: screen.getByRole("group", { name: "cat.png" }), handlers };
}

describe("ImageViewerStage keyboard access", () => {
  it("is a focusable, labelled group", () => {
    const { stage } = mount();
    expect(stage.getAttribute("tabindex")).toBe("0");
    expect(stage.getAttribute("aria-roledescription")).toBe("image viewer");
    stage.focus();
    expect(document.activeElement).toBe(stage);
  });

  it.each([
    ["+", "onZoomIn"],
    ["=", "onZoomIn"],
    ["-", "onZoomOut"],
    ["_", "onZoomOut"],
    ["0", "onActual"],
    ["f", "onFit"],
    ["F", "onFit"],
  ] as const)("key %s calls %s", (key, handler) => {
    const { stage, handlers } = mount();
    stage.focus();
    const proceeded = fireEvent.keyDown(stage, { key });
    expect(handlers[handler]).toHaveBeenCalledTimes(1);
    expect(proceeded).toBe(false); // preventDefault was called
  });

  it.each([
    ["ArrowLeft", 44, 0],
    ["ArrowRight", -44, 0],
    ["ArrowUp", 0, 44],
    ["ArrowDown", 0, -44],
  ] as const)("%s pans by (%i, %i)", (key, dx, dy) => {
    const { stage, handlers } = mount();
    stage.focus();
    fireEvent.keyDown(stage, { key });
    expect(handlers.onKeyPan).toHaveBeenCalledWith(dx, dy);
  });

  it("ignores other keys, shortcuts with modifiers, and typing in a field", () => {
    const { stage, handlers } = mount();
    expect(fireEvent.keyDown(stage, { key: "a" })).toBe(true);
    fireEvent.keyDown(stage, { key: "=", ctrlKey: true });
    fireEvent.keyDown(stage, { key: "-", metaKey: true });
    const field = document.createElement("textarea");
    stage.appendChild(field);
    fireEvent.keyDown(field, { key: "f" });
    for (const fn of Object.values(handlers)) expect(fn).not.toHaveBeenCalled();
  });
});
