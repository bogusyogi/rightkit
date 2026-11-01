import { fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import ImageZoomViewer from "./ImageZoomViewer.js";

describe("ImageZoomViewer", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("renders nothing when closed", () => {
    const { container } = render(
      <ImageZoomViewer open={false} src="x.png" alt="x" onClose={() => {}} />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("renders a dialog with role=dialog and aria-modal=true when open", () => {
    const { container } = render(
      <ImageZoomViewer open src="data:image/png;base64,AAAA" alt="preview" onClose={() => {}} />,
    );
    const dialog = container.querySelector('[role="dialog"]');
    expect(dialog).toBeTruthy();
    expect(dialog?.getAttribute("aria-modal")).toBe("true");
    expect(dialog?.getAttribute("aria-label")).toBe("preview");
    expect(container.querySelector('[data-image-zoom-viewer="true"]')).toBeTruthy();
  });

  it("closes on Escape (RightKit ModalShell plays a short exit, then calls onClose)", async () => {
    const onClose = vi.fn();
    render(<ImageZoomViewer open src="x.png" alt="x" onClose={onClose} />);
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it("closes when the backdrop (not the inner content) is pressed", async () => {
    const onClose = vi.fn();
    const { container } = render(<ImageZoomViewer open src="x.png" alt="x" onClose={onClose} />);
    const dialog = container.querySelector('[role="dialog"]') as HTMLElement;
    const backdrop = dialog.parentElement as HTMLElement;
    fireEvent.mouseDown(backdrop);
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it("does NOT close when the inner image is pressed", () => {
    const onClose = vi.fn();
    const { container } = render(<ImageZoomViewer open src="x.png" alt="x" onClose={onClose} />);
    const img = container.querySelector("img");
    expect(img).toBeTruthy();
    if (img) {
      fireEvent.mouseDown(img);
      fireEvent.click(img);
    }
    expect(onClose).not.toHaveBeenCalled();
  });

  it("renders the caption when provided", () => {
    const { container } = render(
      <ImageZoomViewer open src="x.png" alt="x" caption="hello.png" onClose={() => {}} />,
    );
    expect(container.textContent).toContain("hello.png");
  });
});
