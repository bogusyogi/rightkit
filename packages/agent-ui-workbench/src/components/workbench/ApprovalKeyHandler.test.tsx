// @vitest-environment happy-dom
import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ApprovalKeyHandler from "./ApprovalKeyHandler";
import { rightKitShortcuts } from "@rightkit/agent-ui-core/lib/shortcuts/dispatcher";
import type { PendingApproval } from "@rightkit/agent-ui-hooks/hooks/useApprovals";

// The keys are registry bindings; AppBody attaches the registry to the window in the app.
let detach: () => void = () => {};
beforeEach(() => {
  detach = rightKitShortcuts.attach();
});
afterEach(() => {
  detach();
  cleanup();
});

const pending = { id: "a1" } as unknown as PendingApproval;

describe("ApprovalKeyHandler", () => {
  it("y approves, n and Escape deny, in either case", () => {
    const onApprove = vi.fn();
    const onDeny = vi.fn();
    render(<ApprovalKeyHandler pendingApproval={pending} onApprove={onApprove} onDeny={onDeny} />);
    fireEvent.keyDown(window, { key: "y" });
    fireEvent.keyDown(window, { key: "Y", shiftKey: true });
    expect(onApprove).toHaveBeenCalledTimes(2);
    fireEvent.keyDown(window, { key: "n" });
    fireEvent.keyDown(window, { key: "N", shiftKey: true });
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onDeny).toHaveBeenCalledTimes(3);
  });

  it("does nothing without a pending approval and unregisters on unmount", () => {
    const onApprove = vi.fn();
    const onDeny = vi.fn();
    const { rerender, unmount } = render(
      <ApprovalKeyHandler pendingApproval={null} onApprove={onApprove} onDeny={onDeny} />,
    );
    fireEvent.keyDown(window, { key: "y" });
    expect(onApprove).not.toHaveBeenCalled();
    rerender(<ApprovalKeyHandler pendingApproval={pending} onApprove={onApprove} onDeny={onDeny} />);
    unmount();
    fireEvent.keyDown(window, { key: "y" });
    expect(onApprove).not.toHaveBeenCalled();
  });

  it("a held key does not approve repeatedly", () => {
    const onApprove = vi.fn();
    render(<ApprovalKeyHandler pendingApproval={pending} onApprove={onApprove} onDeny={vi.fn()} />);
    fireEvent.keyDown(window, { key: "y" });
    fireEvent.keyDown(window, { key: "y", repeat: true });
    expect(onApprove).toHaveBeenCalledTimes(1);
  });
});
