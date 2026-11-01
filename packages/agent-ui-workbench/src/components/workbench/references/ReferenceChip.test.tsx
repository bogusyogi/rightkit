import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import ReferenceChip from "./ReferenceChip.js";
import type { ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";

function makeRef(overrides: Partial<ReferenceObject> = {}): ReferenceObject {
  return {
    id: "ref-1",
    kind: "workspace_file",
    destination: "/tmp/foo.txt",
    mime: "text/plain",
    filename: "foo.txt",
    sizeBytes: 2048,
    provenance: {
      source: "workspace_resolver",
      createdAt: "2026-07-20T00:00:00Z",
      trust: "workspaceTrusted",
    },
    actions: ["open", "preview"],
    ...overrides,
  };
}

describe("ReferenceChip", () => {
  it("renders filename, kind, and human-readable size in transcript mode", () => {
    const ref = makeRef({ sizeBytes: 4096 });
    const { container } = render(
      <ReferenceChip reference={ref} onRemove={() => {}} compact={false} />,
    );
    expect(container.textContent).toContain("foo.txt");
    expect(container.textContent).toContain("workspace_file");
    expect(container.textContent).toMatch(/4\.0\s*KB/);
    expect(container.textContent).toContain("text/plain");
  });

  it("renders error state with the danger flag", () => {
    const ref = makeRef({ error: "image bytes lost on restart — re-attach to resend" });
    const { container } = render(
      <ReferenceChip reference={ref} onRemove={() => {}} compact={false} />,
    );
    const errorRow = container.querySelector('[data-reference-error="true"]');
    expect(errorRow).toBeTruthy();
    expect(container.textContent).toContain("image bytes lost");
  });

  it("does not render the error row when reference has no error", () => {
    const ref = makeRef();
    const { container } = render(
      <ReferenceChip reference={ref} onRemove={() => {}} compact={false} />,
    );
    expect(container.querySelector('[data-reference-error="true"]')).toBeNull();
  });

  it("exposes remove button that calls onRemove with the id", () => {
    const onRemove = vi.fn();
    const ref = makeRef({ id: "remove-me" });
    const { getByLabelText } = render(
      <ReferenceChip reference={ref} onRemove={onRemove} compact={false} />,
    );
    fireEvent.click(getByLabelText("Remove foo.txt"));
    expect(onRemove).toHaveBeenCalledWith("remove-me");
  });

  it("Backspace and Delete keys on the focused chip call onRemove", () => {
    const onRemove = vi.fn();
    const ref = makeRef({ id: "kbd-remove" });
    const { getByLabelText } = render(
      <ReferenceChip reference={ref} onRemove={onRemove} compact={false} />,
    );
    const chip = getByLabelText("Reference foo.txt");
    fireEvent.keyDown(chip, { key: "Backspace" });
    expect(onRemove).toHaveBeenCalledWith("kbd-remove");
    onRemove.mockClear();
    fireEvent.keyDown(chip, { key: "Delete" });
    expect(onRemove).toHaveBeenCalledWith("kbd-remove");
  });

  it("Enter on the focused chip triggers open when onAction is provided", () => {
    const onAction = vi.fn();
    const ref = makeRef({ actions: ["open"] });
    const { getByLabelText } = render(
      <ReferenceChip reference={ref} onRemove={() => {}} onAction={onAction} compact={false} />,
    );
    const chip = getByLabelText("Reference foo.txt");
    fireEvent.keyDown(chip, { key: "Enter" });
    expect(onAction).toHaveBeenCalledWith("ref-1", "open");
  });

  it("renders Open + Preview buttons only when actions allow them", () => {
    const ref = makeRef({ actions: ["preview"] });
    const onAction = vi.fn();
    const { getByLabelText, queryByLabelText } = render(
      <ReferenceChip reference={ref} onRemove={() => {}} onAction={onAction} compact={false} />,
    );
    expect(getByLabelText("Preview foo.txt")).toBeTruthy();
    expect(queryByLabelText("Open foo.txt")).toBeNull();
  });

  it("compact mode renders only the thumbnail + close button", () => {
    const ref = makeRef();
    const { container, queryByText } = render(
      <ReferenceChip reference={ref} onRemove={() => {}} compact />,
    );
    // Transcript-mode labels should NOT appear in compact.
    expect(queryByText("workspace_file")).toBeNull();
    expect(container.querySelector('[aria-label="Remove foo.txt"]')).toBeTruthy();
  });

  it("uses the destination as the label when filename is absent", () => {
    const ref = makeRef({ filename: undefined, destination: "https://example.com/foo" });
    const { container } = render(
      <ReferenceChip reference={ref} onRemove={() => {}} compact={false} />,
    );
    expect(container.textContent).toContain("https://example.com/foo");
  });

  it.each([
    ["repository", "stablyai/orca"],
    ["skill", "Blueprint"],
  ] as const)("renders %s references as compact semantic chips", (kind, label) => {
    const ref = makeRef({
      kind,
      destination: kind === "repository" ? "https://github.com/stablyai/orca" : "@blueprint",
      filename: label,
      sizeBytes: undefined,
      mime: undefined,
      actions: [],
    });
    const { container, getByLabelText } = render(
      <ReferenceChip reference={ref} onRemove={() => {}} compact={false} />,
    );
    const chip = getByLabelText(`Reference ${label}`);
    expect(chip.getAttribute("data-reference-kind")).toBe(kind);
    expect(container.textContent).toContain(label);
    expect(container.textContent).not.toContain("workspace_file");
  });
});
