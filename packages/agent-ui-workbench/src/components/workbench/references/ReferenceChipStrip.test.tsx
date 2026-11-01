import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import ReferenceChipStrip from "./ReferenceChipStrip";
import type { ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";

function makeRef(id: string, kind: ReferenceObject["kind"] = "workspace_file"): ReferenceObject {
  return {
    id,
    kind,
    destination: `${id}.txt`,
    filename: `${id}.txt`,
    mime: "text/plain",
    sizeBytes: 1024,
    provenance: {
      source: "test",
      createdAt: "2026-07-20T00:00:00Z",
      trust: "user",
    },
    actions: ["remove"],
  };
}

describe("ReferenceChipStrip", () => {
  it("renders nothing when references is empty", () => {
    const { container } = render(<ReferenceChipStrip references={[]} onRemove={() => {}} />);
    expect(container.firstChild).toBeNull();
  });

  it("renders one chip per reference in compact mode", () => {
    const refs = [makeRef("a"), makeRef("b"), makeRef("c")];
    const { container } = render(<ReferenceChipStrip references={refs} onRemove={() => {}} />);
    expect(container.querySelectorAll('[data-reference-strip="true"]')).toHaveLength(1);
    expect(container.querySelectorAll("li")).toHaveLength(3);
    const strip = container.querySelector('[data-reference-strip="true"]');
    expect(strip?.getAttribute("data-reference-strip-mode")).toBe("composer");
  });

  it("marks transcript mode when compact is false", () => {
    const refs = [makeRef("a")];
    const { container } = render(
      <ReferenceChipStrip references={refs} onRemove={() => {}} compact={false} />,
    );
    const strip = container.querySelector('[data-reference-strip="true"]');
    expect(strip?.getAttribute("data-reference-strip-mode")).toBe("transcript");
  });

  it("wires onRemove through every chip", () => {
    const onRemove = vi.fn();
    const refs = [makeRef("a"), makeRef("b")];
    const { getAllByRole } = render(<ReferenceChipStrip references={refs} onRemove={onRemove} />);
    const removeButtons = getAllByRole("button", { name: /Remove/ });
    expect(removeButtons).toHaveLength(2);
    fireEvent.click(removeButtons[0]);
    expect(onRemove).toHaveBeenCalledWith("a");
    fireEvent.click(removeButtons[1]);
    expect(onRemove).toHaveBeenCalledWith("b");
  });

  it("uses the supplied aria-label when provided", () => {
    const refs = [makeRef("a")];
    const { container } = render(
      <ReferenceChipStrip references={refs} onRemove={() => {}} ariaLabel="Attached files" />,
    );
    const strip = container.querySelector("ul");
    expect(strip?.getAttribute("aria-label")).toBe("Attached files");
  });
});
