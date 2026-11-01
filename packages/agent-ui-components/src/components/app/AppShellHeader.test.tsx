// @vitest-environment happy-dom
import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { cleanup } from "@testing-library/react";
import AppShellHeader from "@rightkit/agent-ui-core/components/app/AppShellHeader";

const baseProps = {
  headerTitle: "Session",
  headerSubtitle: "D:/work",
  status: "Connected",
  health: { ok: true, service: "coderight", port: 1, databasePath: "D:/db", utcNow: "now" },
  selected: { id: "s1" },
  messagesLength: 1,
  isWorking: false,
  onCompact: () => {},
};

describe("AppShellHeader", () => {
  afterEach(cleanup);
  it("renders the supplied chat utility menu in the top-right action row", () => {
    render(
      <AppShellHeader {...baseProps} headerActions={<button type="button">Chat actions</button>} />,
    );
    expect(screen.getByRole("button", { name: "Chat actions" })).toBeTruthy();
  });

  it("dismisses the engine status popover when clicking outside", () => {
    render(<AppShellHeader {...baseProps} />);
    const trigger = screen.getByRole("button", { name: "Engine status: Connected" });
    fireEvent.click(trigger);
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    fireEvent.mouseDown(document.body);
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
  });

  it("reports a healthy engine truthfully when a stale operation failure remains in app status", () => {
    render(<AppShellHeader {...baseProps} status="Load failed" />);

    expect(screen.getByRole("button", { name: "Engine status: Connected" })).toBeTruthy();
    expect(screen.queryByText("Load failed")).toBeNull();
  });
});
