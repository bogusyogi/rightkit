import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import ImageGallery from "./ImageGallery";
import type { ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";

function makeImage(
  id: string,
  opts: { filename?: string; previewUrl?: string } = {},
): ReferenceObject {
  return {
    id,
    kind: "image",
    destination: `${id}.png`,
    filename: opts.filename ?? `${id}.png`,
    mime: "image/png",
    sizeBytes: 1024,
    provenance: {
      source: "test",
      createdAt: "2026-07-20T00:00:00Z",
      trust: "user",
    },
    actions: ["remove"],
    previewUrl: opts.previewUrl ?? `data:image/png;base64,AAA-${id}`,
  };
}

describe("ImageGallery", () => {
  it("renders nothing when no image references are present", () => {
    const { container } = render(<ImageGallery references={[]} />);
    expect(container.firstChild).toBeNull();
  });

  it("renders nothing when only non-image references are present", () => {
    const refs: ReferenceObject[] = [
      {
        id: "doc-1",
        kind: "generated_document",
        destination: "/tmp/foo.md",
        provenance: {
          source: "test",
          createdAt: "2026-07-20T00:00:00Z",
          trust: "user",
        },
        actions: [],
      },
    ];
    const { container } = render(<ImageGallery references={refs} />);
    expect(container.firstChild).toBeNull();
  });

  it("renders one thumbnail per image reference (up to GALLERY_VISIBLE_LIMIT)", () => {
    const refs = [makeImage("a"), makeImage("b"), makeImage("c")];
    const { container } = render(<ImageGallery references={refs} />);
    expect(container.querySelectorAll('button[aria-label^="Open"]')).toHaveLength(3);
    expect(container.querySelector('[data-image-gallery="true"]')).toBeTruthy();
  });

  it("renders a +N overflow chip when there are more images than the visible limit", () => {
    const refs = Array.from({ length: 9 }, (_, i) => makeImage(`img-${i}`));
    const { container } = render(<ImageGallery references={refs} />);
    const overflowChip = container.querySelector('[aria-label*="more image references"]');
    expect(overflowChip).toBeTruthy();
    expect(overflowChip?.textContent).toMatch(/^\+\d+$/);
  });

  it("opens the zoom viewer on thumbnail click and closes on close button", () => {
    const refs = [makeImage("a"), makeImage("b")];
    const { container } = render(<ImageGallery references={refs} />);
    const firstThumb = container.querySelector(
      'button[aria-label="Open a.png"]',
    ) as HTMLButtonElement;
    fireEvent.click(firstThumb);
    expect(container.querySelector('[role="dialog"]')).toBeTruthy();
    const closeBtn = container.querySelector(
      'button[aria-label="Close image viewer"]',
    ) as HTMLButtonElement;
    fireEvent.click(closeBtn);
    expect(container.querySelector('[role="dialog"]')).toBeNull();
  });

  it("fires onRemove when the remove badge is clicked", () => {
    const onRemove = vi.fn();
    const refs = [makeImage("a")];
    const { getByLabelText } = render(<ImageGallery references={refs} onRemove={onRemove} />);
    const removeBtn = getByLabelText("Remove a.png") as HTMLElement;
    fireEvent.click(removeBtn);
    expect(onRemove).toHaveBeenCalledWith("a");
  });
});
