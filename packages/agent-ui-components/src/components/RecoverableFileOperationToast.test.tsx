import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { RecoverableFileOperationToast } from "./RecoverableFileOperationToast.js";

describe("RecoverableFileOperationToast", () => {
  it("keeps undo visible for a successful trash operation", () => {
    render(
      <RecoverableFileOperationToast
        error={null}
        notice="1 item moved to trash"
        canUndo
        onUndo={vi.fn()}
        onDismiss={vi.fn()}
      />,
    );
    expect(screen.getByRole("status")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Undo" })).toBeTruthy();
    expect(screen.getByText("1 item moved to trash")).toBeTruthy();
  });

  it("uses an assertive alert for an operation failure", () => {
    render(
      <RecoverableFileOperationToast
        error="restore failed"
        notice={null}
        canUndo
        onUndo={vi.fn()}
        onDismiss={vi.fn()}
      />,
    );
    expect(screen.getByRole("alert")).toBeTruthy();
    expect(screen.getByText("restore failed")).toBeTruthy();
  });

  it("routes undo and dismiss without hiding either action", () => {
    const onUndo = vi.fn();
    const onDismiss = vi.fn();
    render(
      <RecoverableFileOperationToast
        error={null}
        notice="Created notes.md"
        canUndo
        onUndo={onUndo}
        onDismiss={onDismiss}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    fireEvent.click(screen.getByRole("button", { name: /dismiss file operation/i }));
    expect(onUndo).toHaveBeenCalledOnce();
    expect(onDismiss).toHaveBeenCalledOnce();
  });

  it("renders nothing when there is no message or recovery action", () => {
    const { container } = render(
      <RecoverableFileOperationToast
        error={null}
        notice={null}
        canUndo={false}
        onUndo={vi.fn()}
        onDismiss={vi.fn()}
      />,
    );
    expect(container.firstChild).toBeNull();
  });
});
