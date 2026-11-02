// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ErrorBoundary } from "./ErrorBoundary.js";

function Crash() {
  throw new Error("render failed");
}

describe("ErrorBoundary", () => {
  it("shows a reload action instead of unmounting the whole app surface", () => {
    const original = console.error;
    console.error = vi.fn();
    try {
      render(
        <ErrorBoundary label="Markdown viewer">
          <Crash />
        </ErrorBoundary>,
      );
    } finally {
      console.error = original;
    }

    expect(screen.getByText("Something went wrong in Markdown viewer")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reload Markdown viewer" })).toBeInTheDocument();
  });
});
