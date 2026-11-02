// @vitest-environment happy-dom
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import EditedFilesSummary, { type EditedFileEntry } from "./EditedFilesSummary.js";

const files: EditedFileEntry[] = [
  { filePath: "src/a.ts", additions: 4, deletions: 1, diff: "@@ -1,1 +1,4 @@\n+a" },
  { filePath: "src/b.ts", additions: 2, deletions: 0, diff: "@@ -1,0 +1,2 @@\n+b" },
];

describe("EditedFilesSummary", () => {
  it("renders nothing when there are no files", () => {
    const { container } = render(<EditedFilesSummary files={[]} />);
    expect(container.firstChild).toBeNull();
  });

  it("is collapsed by default and shows aggregate file count and +/- totals", () => {
    render(
      <EditedFilesSummary files={files}>
        <div data-testid="per-file-detail">detail</div>
      </EditedFilesSummary>,
    );

    expect(screen.getByText("2 files edited")).toBeTruthy();
    expect(screen.getByText("+6")).toBeTruthy();
    expect(screen.getByText("-1")).toBeTruthy();
    expect(screen.queryByTestId("per-file-detail")).toBeNull();
    expect(
      screen.getByRole("button", { name: "Expand 2 files edited" }).getAttribute("aria-expanded"),
    ).toBe("false");
  });

  it("expands on demand to reveal the per-file DiffCard children", () => {
    render(
      <EditedFilesSummary files={files}>
        <div data-testid="per-file-detail">detail</div>
      </EditedFilesSummary>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Expand 2 files edited" }));

    expect(screen.getByTestId("per-file-detail")).toBeTruthy();
    expect(
      screen.getByRole("button", { name: "Collapse 2 files edited" }).getAttribute("aria-expanded"),
    ).toBe("true");
  });

  it("invokes the Review handler with the full file set", () => {
    const onReview = vi.fn();
    render(<EditedFilesSummary files={files} onReview={onReview} />);

    fireEvent.click(screen.getByRole("button", { name: "Review edited files" }));
    expect(onReview).toHaveBeenCalledWith(files);
  });

  it("invokes the Undo handler with the full file set", () => {
    const onUndo = vi.fn();
    render(<EditedFilesSummary files={files} onUndo={onUndo} />);

    fireEvent.click(screen.getByRole("button", { name: "Undo edited files" }));
    expect(onUndo).toHaveBeenCalledWith(files);
  });

  it("disables Review and Undo when no handler is supplied", () => {
    render(<EditedFilesSummary files={files} />);

    expect(screen.getByRole("button", { name: "Review edited files" })).toHaveProperty(
      "disabled",
      true,
    );
    expect(screen.getByRole("button", { name: "Undo edited files" })).toHaveProperty(
      "disabled",
      true,
    );
  });

  it("renders a single row for one file using singular wording", () => {
    render(<EditedFilesSummary files={[files[0]]} />);
    expect(screen.getByText("1 file edited")).toBeTruthy();
  });
});
