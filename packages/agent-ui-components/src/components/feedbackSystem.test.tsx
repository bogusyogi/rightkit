import { render, screen } from "@testing-library/react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";

import { ActionDialog, ConfirmDialog } from "@rightkit/react-shell/react";
import PaneErrorBoundary from "@rightkit/agent-ui-core/components/PaneErrorBoundary";

describe("ConfirmDialog (RightKit)", () => {
  it("renders title, description, and danger-styled confirm button", () => {
    const html = renderToStaticMarkup(
      <ConfirmDialog
        title="Delete session?"
        description={`Delete session "My session" and its transcript? This can't be undone.`}
        confirmLabel="Delete"
        danger
        onConfirm={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(html).toContain("Delete session?");
    expect(html).toContain("Delete session &quot;My session&quot; and its transcript");
    expect(html).toContain('role="alertdialog"');
    expect(html).toContain('aria-modal="true"');
    expect(html).toContain("rk-btn--danger");
    expect(html).toContain(">Delete<");
    expect(html).toContain(">Cancel<");
  });

  it("supports a non-danger confirm style for non-destructive confirmations", () => {
    const html = renderToStaticMarkup(
      <ConfirmDialog
        danger={false}
        title="Discard changes?"
        description="You have unsaved edits."
        confirmLabel="Discard"
        onConfirm={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(html).toContain("rk-btn--primary");
    expect(html).not.toContain("rk-btn--danger");
  });

  it("keeps the scope line for pending or error confirmations via ActionDialog", () => {
    const html = renderToStaticMarkup(
      <ActionDialog
        title="Delete session?"
        description="Delete it?"
        role="alertdialog"
        onClose={() => {}}
        actions={[
          { id: "cancel", label: "Cancel", onClick: () => {}, autoFocus: true },
          { id: "confirm", label: "Delete", variant: "danger", onClick: () => {} },
        ]}
      >
        <p className="rk-modal__desc">
          Applies to: <strong>Session: My session</strong>
        </p>
      </ActionDialog>,
    );
    expect(html).toContain("Applies to:");
    expect(html).toContain("Session: My session");
  });
});

describe("PaneErrorBoundary", () => {
  const originalError = console.error;

  beforeEach(() => {
    console.error = vi.fn();
  });

  afterEach(() => {
    console.error = originalError;
  });

  function Boom(): never {
    throw new Error("kaboom");
  }

  it("renders the fallback UI (pane name, message, reload + copy actions) and logs the pane name", () => {
    render(
      <PaneErrorBoundary name="Sidebar">
        <Boom />
      </PaneErrorBoundary>,
    );
    expect(screen.getByText("Sidebar hit an error")).toBeTruthy();
    expect(screen.getByText("kaboom")).toBeTruthy();
    expect(screen.getByText("Reload pane")).toBeTruthy();
    expect(screen.getByText("Copy details")).toBeTruthy();
    expect(screen.getByRole("alert")).toBeTruthy();
    expect(screen.getByLabelText("Reload Sidebar")).toBeTruthy();
    expect(console.error).toHaveBeenCalledWith(
      'App pane "Sidebar" crashed:',
      expect.any(Error),
      expect.any(String),
    );
  });

  it("renders children normally when no error has been caught", () => {
    const html = renderToStaticMarkup(
      <PaneErrorBoundary name="Sidebar">
        <div data-testid="ok">fine</div>
      </PaneErrorBoundary>,
    );
    expect(html).toContain("fine");
    expect(html).not.toContain("Reload pane");
  });
});
